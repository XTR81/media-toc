extern crate gstreamer as gst;

use byte_slice_cast::AsSliceOf;

use std::i16;

use std::collections::vec_deque::VecDeque;

use super::{DoubleSampleExtractor};

pub const SAMPLES_NORM: f64 = 200f64;
const SAMPLES_OFFSET: f64 = SAMPLES_NORM / 2f64;

pub struct AudioBuffer {
    capacity: usize,
    pub sample_duration: f64,
    channels: usize,
    drain_size: usize,

    pub eos: bool,

    pub samples_offset: usize,
    pub samples: VecDeque<f64>,

    samples_extractor_opt: Option<DoubleSampleExtractor>,
}

impl AudioBuffer {
    pub fn new(
        caps: &gst::Caps,
        size_duration: u64,
        samples_extractor: DoubleSampleExtractor,
    ) -> Self
    {
        let structure = caps.get_structure(0)
            .expect("Couldn't get structure from audio caps");
        let rate = structure.get::<i32>("rate")
            .expect("Couldn't get rate from audio caps");

        // assert_eq!(format, S16);
        // assert_eq!(layout, Interleaved);

        let sample_duration = 1_000_000_000f64 / (rate as f64);
        let capacity = (size_duration as f64 / sample_duration) as usize;

        let drain_size = capacity / 5;

        AudioBuffer {
            capacity: capacity,
            sample_duration: sample_duration,
            channels: structure.get::<i32>("channels")
                .expect("Couldn't get channels from audio sample")
                as usize,
            drain_size: drain_size,

            eos: false,

            samples_offset: 0,
            samples: VecDeque::with_capacity(capacity),

            samples_extractor_opt: Some(samples_extractor),
        }
    }

    pub fn push_gst_sample(&mut self, sample: gst::Sample) {
        let buffer = sample.get_buffer()
            .expect("Couldn't get buffer from audio sample");

        let map = buffer.map_readable().unwrap();
        let incoming_samples = map.as_slice().as_slice_of::<i16>()
            .expect("Couldn't get audio samples as i16");

        if self.samples.len() + incoming_samples.len() > self.capacity
        {   // buffer will reach capacity => drain a chunk of samples
            // only if we have samples in history
            if self.samples_extractor_opt.as_ref().unwrap().samples_offset
                > self.samples_offset + self.drain_size
            {
                self.samples.drain(..self.drain_size);
                self.samples_offset += self.drain_size;
            }
        }

        // normalize samples in range 0f64..1f64 ready to render

        // FIXME: use gstreamer downmix
        // FIXME: select the channels using the position info
        // if more than 2 channels,
        // Use 75% for first 2 channels (assumeing front left and front right)
        // Use 25% for the others
        let (front_norm_factor, others_norm_factor, front_channels) =
            if self.channels > 2 {
                (
                    0.75f64 / 2f64 / (i16::MAX as f64) * SAMPLES_NORM / 2f64,
                    0.25f64 / ((self.channels - 2) as f64) / (i16::MAX as f64) * SAMPLES_NORM / 2f64,
                    2
                )
            } else {
                (
                    1f64 / (self.channels as f64) / (i16::MAX as f64) * SAMPLES_OFFSET,
                    0f64,
                    self.channels
                )
            };

        let mut norm_sample;
        let mut index = 0;
        while index < incoming_samples.len() {
            norm_sample = 0f64;

            for _ in 0..front_channels {
                norm_sample += incoming_samples[index] as f64 * front_norm_factor;
                index += 1;
            }
            for _ in front_channels..self.channels {
                norm_sample += incoming_samples[index] as f64 * others_norm_factor;
                index += 1;
            }
            self.samples.push_back(SAMPLES_OFFSET - norm_sample);
        };

        if !self.samples.is_empty() {
            let mut samples_extractor = self.samples_extractor_opt.take().unwrap();
            samples_extractor.extract_samples(&self);
            self.samples_extractor_opt = Some(samples_extractor);
        }
    }

    pub fn iter(&self, first: usize, last: usize, step: usize) -> Iter {
        assert!(first >= self.samples_offset);
        let last = if last > first { last } else { first };
        Iter::new(self, first, last, step)
    }

    pub fn handle_eos(&mut self) {
        if !self.samples.is_empty() {
            let mut samples_extractor = self.samples_extractor_opt.take().unwrap();
            samples_extractor.extract_samples(&self);
            self.samples_extractor_opt = Some(samples_extractor);
        }
    }
}

pub struct Iter<'a> {
    buffer: &'a AudioBuffer,
    idx: usize,
    last: usize,
    step: usize,
}

impl<'a> Iter<'a> {
    fn new(buffer: &'a AudioBuffer, first: usize, last: usize, step: usize) -> Iter<'a> {
        Iter {
            buffer: buffer,
            idx: first - buffer.samples_offset,
            last: buffer.samples.len().min(last - buffer.samples_offset),
            step: step,
        }
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a f64;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.last {
            return None;
        }

        let item = self.buffer.samples.get(self.idx);
        self.idx += self.step;

        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.idx == self.last {
            return (0, Some(0));
        }

        let remaining = (self.last - self.idx) / self.step;

        (remaining, Some(remaining))
    }
}
