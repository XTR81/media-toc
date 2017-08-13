extern crate byteorder;
use byteorder::{LittleEndian, ReadBytesExt};

extern crate gstreamer as gst;
use gstreamer::PadExt;

use std::io::Cursor;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SampleFormat {
    F32LE,
    F64LE,
    I16LE,
    I32LE,
    I64LE,
    U8,
    Unknown,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SampleLayout {
    Interleaved,
    Unknown,
}

#[derive(Copy, Clone, Debug)]
pub struct AudioCaps {
    pub sample_format: SampleFormat,
    pub layout: SampleLayout,
    pub rate: usize,
    pub sample_duration: f64,
    pub channels: usize,
}

impl AudioCaps {
    pub fn new() -> Self {
        AudioCaps {
            sample_format: SampleFormat::Unknown,
            layout: SampleLayout::Unknown,
            rate: 0,
            sample_duration: 0f64,
            channels: 0,
        }
    }

    pub fn from_sink_pad(sink_pad: &gst::Pad) -> Self {
        let caps = sink_pad.get_current_caps()
            .expect("Couldn't get caps for audio stream");
        let structure = caps.iter().next()
            .expect("AudioCaps::from_gst_caps: No caps found");

        println!("\nAudio sink caps:\n\t{:?}", structure);

        let mut this = AudioCaps::new();

        let format = structure.get::<String>("format")
            .expect("AudioCaps::from_gst_caps: Couldn't get sample format for audio stream");
        this.sample_format = if format == "F32LE" {
            SampleFormat::F32LE
        } else if format == "F64LE" {
            SampleFormat::F64LE
        } else if format == "S16LE" {
            SampleFormat::I16LE
        } else if format == "S32LE" {
            SampleFormat::I32LE
        } else if format == "S64LE" {
            SampleFormat::I64LE
        } else if format == "U8" {
            SampleFormat::U8
        } else {
            panic!("AudioCaps::from_gst_caps: Unknown sample format: {}", format);
        };

        let layout = structure.get::<String>("layout")
            .expect("AudioCaps::from_gst_caps: Couldn't get sample layout for audio stream");
        this.layout = if layout == "interleaved" {
            SampleLayout::Interleaved
        } else {
            panic!("AudioCaps::from_gst_caps: Unknown sample layout: {}", layout);
        };

        this.rate = structure.get::<i32>("rate")
            .expect("AudioCaps::from_gst_caps: Couldn't get sample rate for audio stream")
            as usize;
        this.sample_duration = 1_000_000_000f64 / this.rate as f64;

        this.channels = structure.get::<i32>("channels")
            .expect("AudioCaps::from_gst_caps: Couldn't get sample channels for audio stream")
            as usize;

        this
    }
}

pub struct AudioBuffer {
    pub caps: AudioCaps,
    pub pts: f64,
    pub duration: usize,
    pub sample_offset: usize,
    pub samples_nb: usize,
    pub samples: Vec<f64>,
}

impl AudioBuffer {
    pub fn from_gst_buffer(caps: &AudioCaps, buffer: &gst::Buffer) -> Self {
        let samples_nb = (buffer.get_duration() as f64 / caps.sample_duration) as usize;
        let mut this = AudioBuffer {
            caps: *caps,
            pts: buffer.get_pts() as f64,
            duration: buffer.get_duration() as usize,
            sample_offset: (buffer.get_pts() as f64 / caps.sample_duration) as usize,
            samples_nb: samples_nb,
            samples: Vec::with_capacity(caps.channels * samples_nb),
        };

        assert_eq!(this.caps.layout, SampleLayout::Interleaved);

        let map = buffer.map_readable().unwrap();
        let data = map.as_slice();

        let mut data_reader = Cursor::new(data);
        loop {
            let norm_sample = match this.caps.sample_format {
                SampleFormat::F32LE => {
                    data_reader.read_f32::<LittleEndian>().map(|v| v as f64)
                },
                SampleFormat::F64LE => {
                    data_reader.read_f64::<LittleEndian>()
                },
                SampleFormat::I16LE => {
                    data_reader.read_i16::<LittleEndian>().map(|v|
                        v as f64 / ::std::i16::MAX as f64
                    )
                },
                SampleFormat::I32LE => {
                    data_reader.read_i32::<LittleEndian>().map(|v|
                        v as f64 / ::std::i32::MAX as f64
                    )
                },
                SampleFormat::I64LE => {
                    data_reader.read_i64::<LittleEndian>().map(|v|
                        v as f64 / ::std::i64::MAX as f64
                    )
                },
                SampleFormat::U8 => {
                    data_reader.read_u8().map(|v|
                        (v as f64 - ::std::i8::MAX as f64) / ::std::i8::MAX as f64
                    )
                },
                _ => panic!("never happens"), // FIXME: use proper assert
            };

            match norm_sample {
                Ok(norm_sample) => this.samples.push(1f64 - norm_sample),
                Err(_) => break,
            }
        }

        this
    }
}