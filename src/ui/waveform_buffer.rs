extern crate cairo;

#[cfg(feature = "profiling-waveform-buffer")]
use chrono::Utc;

use std::any::Any;

use std::sync::{Arc, Mutex};

use ::media::{AudioBuffer, SAMPLES_NORM};

use ::media::{DoubleSampleExtractor, SamplesExtractor};
use ::media::samples_extractor::SamplesExtractionState;

pub const BACKGROUND_COLOR: (f64, f64, f64) = (0.2f64, 0.2235f64, 0.2314f64);

pub struct DoubleWaveformBuffer {}
impl DoubleWaveformBuffer {
    pub fn new(
        exposed_mtx: &Arc<Mutex<Box<SamplesExtractor>>>
    ) -> DoubleSampleExtractor {
        DoubleSampleExtractor::new(
            Arc::clone(exposed_mtx),
            Box::new(WaveformBuffer::new())
        )
    }
}

pub struct WaveformBuffer {
    state: SamplesExtractionState,
    buffer_sample_window: usize,

    width: i32,
    height: i32,
    pub exposed_image: Option<cairo::ImageSurface>,
    working_image: Option<cairo::ImageSurface>,
}

impl WaveformBuffer {
    pub fn new() -> Self {
        WaveformBuffer {
            state: SamplesExtractionState::new(),
            buffer_sample_window: 0,

            width: 0,
            height: 0,
            exposed_image: None,
            working_image: None,
        }
    }

    pub fn cleanup(&mut self) {
        // clear for reuse
        self.cleanup_state();
        self.buffer_sample_window = 0;
        self.width = 0;
        self.height = 0;
        self.exposed_image = None;
        self.working_image = None;
    }

    pub fn get_position_from_x(&mut self, x: f64) -> Option<u64> {
        match self.get_first_visible_sample() {
            Some(first_visible_sample) =>
                Some(
                    (
                        first_visible_sample as u64
                        + (x as u64) * (self.state.sample_step as u64)
                    ) * (self.state.sample_duration as u64)
                ),
            None => None,
        }
    }

    fn get_first_visible_sample(&mut self) -> Option<usize> {
        if self.exposed_image.is_some() {
            let state = &mut self.state;
            state.query_current_sample();

            if state.eos
            && state.current_sample + state.half_requested_sample_window > state.last_sample {
                if self.buffer_sample_window > state.requested_sample_window {
                    Some(state.last_sample - state.requested_sample_window)
                } else {
                    Some(state.samples_offset)
                }
            } else if state.current_sample > state.half_requested_sample_window
                    + state.samples_offset {
                Some(state.current_sample - state.half_requested_sample_window)
            } else {
                Some(state.samples_offset)
            }
        } else {
            None
        }
    }

    pub fn update_conditions(&mut self,
        duration: u64,
        width: i32,
        height: i32,
    ) -> Option<(usize, usize)> // (x_offset, current_x)
    {
        {
            let state = &mut self.state;

            self.width = width;
            self.height = height;

            let width = width as u64;
            // resolution
            state.requested_step_duration =
                if duration > width {
                    duration / width
                } else {
                    1
                };

            state.requested_sample_window = (
                duration as f64 / state.sample_duration
            ).round() as usize;
            state.half_requested_sample_window = state.requested_sample_window / 2;
        }

        match self.get_first_visible_sample() {
            Some(first_visible_sample) => {
                if self.state.current_sample > first_visible_sample {
                    let state = &self.state;
                    Some((
                        (first_visible_sample - state.samples_offset) / state.sample_step, // x_offset
                        (state.current_sample - first_visible_sample) / state.sample_step, // current_x
                    ))
                } else {
                    None
                }
            },
            None => None,
        }
    }
}

impl SamplesExtractor for WaveformBuffer {
    fn as_mut_any(&mut self) -> &mut Any {
        self
    }

    fn get_extraction_state(&self) -> &SamplesExtractionState {
        &self.state
    }

    fn get_extraction_state_mut(&mut self) -> &mut SamplesExtractionState {
        &mut self.state
    }

    fn can_extract(&self) -> bool {
        self.state.requested_sample_window > 0
    }

    fn update_extraction(&mut self,
        audio_buffer: &AudioBuffer,
        first_sample: usize,
        last_sample: usize,
        sample_step: usize,
    ) {
        #[cfg(feature = "profiling-waveform-buffer")]
        let start = Utc::now();

        let buffer_sample_window = last_sample - first_sample;
        let extracted_samples_window = buffer_sample_window / sample_step;

        let mut must_redraw = self.state.sample_step != sample_step
            || first_sample + extracted_samples_window < self.state.samples_offset
            || first_sample >= self.state.last_sample;

        let extracted_samples_window = extracted_samples_window as i32;

        let working_image = {
            let mut can_reuse = false;
            let target_width = extracted_samples_window.max(self.width);

            if let Some(ref working_image) = self.working_image {
                if self.height != working_image.get_height() {
                    // height has changed => scale samples amplitude accordingly
                    must_redraw = true;
                }

                if target_width <= working_image.get_width()
                && self.height <= working_image.get_height() {
                    // expected dimensions fit in current working image => reuse it
                    can_reuse = true;
                }
            }

            if can_reuse {
                self.working_image.take().unwrap()
            } else {
                cairo::ImageSurface::create(
                    cairo::Format::Rgb24,
                    target_width,
                    self.height
                ).expect("WaveformBuffer: couldn't create image surface in update_extraction")
            }
        };

        let cr = cairo::Context::new(&working_image);
        let (mut sample_iter, mut x, clear_limit) =
            if must_redraw {
                // Initialization or resolution has changed or seek requested
                // redraw the whole range
                self.state.sample_step = sample_step;

                // clear the image
                cr.set_source_rgb(
                    BACKGROUND_COLOR.0,
                    BACKGROUND_COLOR.1,
                    BACKGROUND_COLOR.2
                );
                cr.paint();

                (
                    audio_buffer.iter(first_sample, last_sample, sample_step),
                    0f64,
                    0f64,
                )
            } else {
                // can shift previous context
                let previous_image = self.exposed_image.take()
                    .expect("WaveformBuffer: no exposed_image while updating");

                let tuple =
                    if first_sample < self.state.samples_offset {
                        // append samples before previous first sample
                        let sample_step_offset =
                            (self.state.samples_offset - first_sample) / sample_step;
                        cr.set_source_surface(
                            &previous_image,
                            sample_step_offset as f64,
                            0f64
                        );

                        // prepare to add samples to prepend
                        (
                            audio_buffer.iter(first_sample, self.state.samples_offset, sample_step),
                            0f64,
                            sample_step_offset as f64,
                        )
                    } else {
                        // append samples after previous last sample
                        let sample_step_offset =
                            (first_sample - self.state.samples_offset) / sample_step;
                        cr.set_source_surface(
                            &previous_image,
                            -(sample_step_offset as f64),
                            0f64
                        );

                        // prepare to add remaining samples
                        (
                            audio_buffer.iter(self.state.last_sample, last_sample, sample_step),
                            (
                                (self.state.last_sample - self.state.samples_offset) / sample_step
                                - sample_step_offset
                            ) as f64,
                            f64::from(working_image.get_width()),
                        )
                    };

                cr.paint();
                self.exposed_image = Some(previous_image);

                tuple
            };

        cr.scale(1f64, f64::from(self.height) / SAMPLES_NORM);

        if !must_redraw {
            // fill the rest of the image with background color
            cr.set_source_rgb(
                BACKGROUND_COLOR.0,
                BACKGROUND_COLOR.1,
                BACKGROUND_COLOR.2
            );
            cr.rectangle(x, 0f64, clear_limit - x, SAMPLES_NORM);
            cr.fill();
        } // else brackgroung already set while clearing the image

        if sample_iter.size_hint().0 > 0 {
            // Stroke selected samples
            cr.set_line_width(0.5f64);
            cr.set_source_rgb(0.8f64, 0.8f64, 0.8f64);

            let mut sample_value = *sample_iter.next().unwrap();
            for sample in sample_iter {
                cr.move_to(x, sample_value);
                x += 1f64;
                sample_value = *sample;
                cr.line_to(x, sample_value);
                cr.stroke();
            }
        }

        if let Some(previous_image) = self.exposed_image.take() {
            self.working_image = Some(previous_image);
        }
        self.exposed_image = Some(working_image);

        self.state.samples_offset = first_sample;
        self.buffer_sample_window = buffer_sample_window;
        self.state.last_sample = last_sample;

        #[cfg(feature = "profiling-waveform-buffer")]
        let end = Utc::now();

        #[cfg(feature = "profiling-waveform-buffer")]
        println!("waveform-buffer,{},{}",
            start.time().format("%H:%M:%S%.6f"),
            end.time().format("%H:%M:%S%.6f"),
        );
    }
}
