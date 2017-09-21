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
    first_sample_changed: bool,

    current_sample: usize,
    first_sample: usize,
    last_sample: usize,
    buffer_sample_window: usize,
    first_sample_lock: Option<i64>,
    sample_seeked: Option<usize>,

    pub was_exposed: bool,
    requested_sample_window: usize,
    half_requested_sample_window: usize,
    requested_step_duration: u64,
    sample_step: usize,

    width: i32,
    height: i32,
    pub exposed_image: Option<cairo::ImageSurface>,
    working_image: Option<cairo::ImageSurface>,
}

impl WaveformBuffer {
    pub fn new() -> Self {
        WaveformBuffer {
            state: SamplesExtractionState::new(),
            first_sample_changed: false,

            current_sample: 0,
            first_sample: 0,
            last_sample: 0,
            buffer_sample_window: 0,
            first_sample_lock: None,
            sample_seeked: None,

            was_exposed: false,
            requested_sample_window: 0,
            half_requested_sample_window: 0,
            requested_step_duration: 0,
            sample_step: 0,

            width: 0,
            height: 0,
            exposed_image: None,
            working_image: None,
        }
    }

    pub fn cleanup(&mut self) {
        // clear for reuse
        self.cleanup_state();
        self.first_sample_changed = false;

        self.current_sample = 0;
        self.first_sample = 0;
        self.last_sample = 0;
        self.buffer_sample_window = 0;
        self.first_sample_lock = None;
        self.sample_seeked = None;

        self.was_exposed = false;
        self.requested_sample_window = 0;
        self.half_requested_sample_window = 0;
        self.requested_step_duration = 0;
        self.sample_step = 0;

        self.width = 0;
        self.height = 0;
        self.exposed_image = None;
        self.working_image = None;
    }

    pub fn clear_exposed_status(&mut self) {
        self.was_exposed = false;
    }

    // mark seek in window and return position if applicable
    pub fn seek_in_window(&mut self, x: f64) -> Option<u64> {
        match self.get_first_visible_sample() {
            Some(first_visible_sample) => {
                self.first_sample_lock = Some(self.first_sample as i64);
                let sample_seeked = first_visible_sample + (x as usize) * self.sample_step;
                self.sample_seeked = Some(sample_seeked);
                println!("first_sample_lock: {}, sample_seeked {}", self.first_sample, sample_seeked);
                Some(sample_seeked as u64 * self.state.sample_duration_u)
            },
            None => None,
        }
    }

    fn get_first_visible_sample(&mut self) -> Option<usize> {
        if self.exposed_image.is_some() {
            self.was_exposed = true;
            let previous_cursor = self.current_sample;
            self.current_sample = self.query_current_sample();

            if self.current_sample >= self.first_sample {
                // current sample appears after first buffer sample
                if let Some(first_sample_lock) = self.first_sample_lock {
                    // adapt according to the evolution of the position
                    let center_offset = self.current_sample as i64
                        - self.half_requested_sample_window as i64
                        - first_sample_lock;
                    if center_offset < -(self.sample_step as i64) {
                        // cursor in first half of the window
                        // keep origin on the first sample upon seek
                        println!("1st half offset: {}, first_sample_lock {}, lock first sample {}, current_sample: {}", center_offset, first_sample_lock, first_sample_lock, self.current_sample);
                        // this is in case we move to the 2d half
                        self.sample_seeked = Some(self.current_sample);
                        Some(
                            if first_sample_lock >= 0 { // TODO: this should not be necessary
                                (first_sample_lock as usize).max(self.first_sample)
                            } else {
                                self.first_sample
                            }
                        )
                    } else if (center_offset as usize) < self.sample_step {
                        // reached the center => keep cursor there
                        self.first_sample_lock = None;
                        println!("center offset: {} back to center", center_offset);
                        Some(
                            (
                                self.current_sample
                                - self.half_requested_sample_window
                            ).max(self.first_sample)
                        )
                    } else {
                        // cursor in second half of the window
                        // progressively get it back to center
                        let previous_cursor = match self.sample_seeked {
                            Some(sample_seeked) => {
                                self.sample_seeked = None;
                                sample_seeked
                            },
                            None => previous_cursor,
                        };
                        let previous_offset =
                            previous_cursor as i64 - first_sample_lock;
                        let delta_cursor =
                            if self.current_sample >= previous_cursor {
                                self.current_sample - previous_cursor
                            } else {
                                previous_cursor - self.current_sample
                            };
                        let next_first_sample =
                            self.current_sample as i64
                            - previous_offset
                            + delta_cursor as i64 / 2;
                        self.first_sample_lock = Some(next_first_sample);

                        Some(self.first_sample.max(next_first_sample as usize))
                    }
                }
                else if self.current_sample + self.half_requested_sample_window <= self.last_sample {
                    // current sample fits in the first half of the window with last sample further
                    if self.current_sample > self.first_sample + self.half_requested_sample_window {
                        // current sample can be centered (scrolling)
                        Some(self.current_sample - self.half_requested_sample_window)
                    } else {
                        // current sample before half of displayable window
                        // set origin to the first sample in the buffer
                        // current sample will be displayed between the origin
                        // and the center
                        // and discard any previous seek adjustment contraint
                        Some(self.first_sample)
                    }
                } else if self.current_sample <= self.last_sample + 2 * self.sample_step {
                    // current sample can fit in the second half of the window
                    // (take a margin due to rounding to sample_step)
                    // TODO: check if the 2* is still necessary in the above condition
                    if self.buffer_sample_window >= self.requested_sample_window {
                        // buffer window is larger than requested_sample_window
                        // set last buffer to the right
                        // and discard any previous seek adjustment contraint
                        Some(self.last_sample - self.requested_sample_window)
                    } else {
                        // buffer window is smaller than requested_sample_window
                        // set first sample to the left
                        // and discard any previous seek adjustment contraint
                        Some(self.first_sample)
                    }
                } else {
                    // current sample appears further than last sample
                    None
                }
            }
            else {
                // current sample appears before buffer first sample
                None
            }
        } else {
            // no image available yet
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
            self.width = width;
            self.height = height;

            let width = width as u64;
            // resolution
            self.requested_step_duration =
                if duration > width {
                    duration / width
                } else {
                    1
                };

            self.requested_sample_window = (
                duration as f64 / self.state.sample_duration
            ).round() as usize;
            self.half_requested_sample_window = self.requested_sample_window / 2;
        }

        match self.get_first_visible_sample() {
            Some(first_visible_sample) => {
                Some((
                    (first_visible_sample - self.first_sample) / self.sample_step, // x_offset
                    if self.current_sample > first_visible_sample { // current_x
                        (self.current_sample - first_visible_sample) / self.sample_step
                    } else {
                        0
                    },
                ))
            },
            None => None,
        }
    }

    // This function is called on a working buffer
    // which means that self.exposed_image image is the image
    // that was previously exposed to the UI
    // this also means that we can safely deal with both
    // images since none of them is exposed at this very moment
    fn update_extraction(&mut self,
        audio_buffer: &AudioBuffer,
        first_sample: usize,
        last_sample: usize,
        sample_step: usize,
    ) {
        #[cfg(feature = "profiling-waveform-buffer")]
        let start = Utc::now();

        let extraction_samples_window = (last_sample - first_sample) / sample_step;

        let mut must_redraw = self.exposed_image.is_none() || self.sample_step != sample_step;
        if !must_redraw && first_sample >= self.first_sample
        && last_sample <= self.last_sample
        {   // traget extraction fits in previous extraction
            return;
        } else if first_sample + extraction_samples_window < self.first_sample
            || first_sample > self.last_sample
        {   // current samples extraction doesn't overlap with samples in previous image
            must_redraw = true;
        }

        let working_image = {
            let mut can_reuse = false;
            let target_width = (extraction_samples_window as i32).max(self.width);

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

                println!("redraw");

                // clear the image
                cr.set_source_rgb(
                    BACKGROUND_COLOR.0,
                    BACKGROUND_COLOR.1,
                    BACKGROUND_COLOR.2
                );
                cr.paint();

                self.sample_step = sample_step;
                self.first_sample = first_sample;
                self.last_sample = last_sample;

                (
                    audio_buffer.iter(first_sample, last_sample, sample_step),
                    0f64,
                    0f64,
                )
            } else {
                // can reuse previous context
                let previous_image = self.exposed_image.take()
                    .expect("WaveformBuffer: no exposed_image while updating");

                let (image_offset, sample_iter, x, clear_limit) = {
                    // Note: condition first_sample >= self.self.first_sample
                    //                 && last_sample <= self.self.last_sample
                    // (traget extraction fits in previous extraction)
                    // already checked

                    if first_sample < self.first_sample {
                        // append samples before previous first sample
                        println!("appending samples before previous first sample");

                        let image_width_as_samples =
                            working_image.get_width() as usize * sample_step;

                        let previous_first_sample = self.first_sample;
                        self.first_sample = first_sample;
                        self.last_sample = self.last_sample.min(
                            first_sample + image_width_as_samples
                        );

                        // shift previous image to the right
                        let image_offset = (
                            (previous_first_sample - first_sample) / sample_step
                        ) as f64;

                        (
                            image_offset,
                            audio_buffer.iter(first_sample, previous_first_sample, sample_step), // sample_iter
                            0f64, // first x to draw
                            image_offset, // clear_limit
                        )
                    } else {
                        // first_sample >= self.first_sample
                        // Note: due to previous conditions tested before,
                        // this also implies:
                        assert!(last_sample > self.last_sample);

                        let previous_first_sample = self.first_sample;
                        let previous_last_sample = self.last_sample;
                        // Note: image width is such a way that samples in
                        // (first_sample, last_sample) can all be rendered
                        self.first_sample = first_sample;
                        self.last_sample = last_sample;

                        // shift previous image to the left (if necessary)
                        let image_offset = -((
                            (first_sample - previous_first_sample) / sample_step
                        ) as f64);

                        // append samples after previous last sample
                        let first_sample_to_draw = previous_last_sample.max(first_sample);

                        // prepare to add remaining samples
                        (
                            image_offset,
                            audio_buffer.iter(first_sample_to_draw, last_sample, sample_step), // sample_iter
                            (
                                (first_sample_to_draw - previous_first_sample) / sample_step
                            ) as f64 + image_offset, // first x to draw
                            f64::from(working_image.get_width()), // clear_limit
                        )
                    }
                };

                cr.set_source_surface(&previous_image, image_offset, 0f64);
                cr.paint();

                // set image back, will be swapped later
                self.exposed_image = Some(previous_image);

                (sample_iter, x, clear_limit)
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

        self.buffer_sample_window = self.last_sample - self.first_sample;

        #[cfg(feature = "profiling-waveform-buffer")]
        let end = Utc::now();

        #[cfg(feature = "profiling-waveform-buffer")]
        println!("waveform-buffer,{},{}",
            start.time().format("%H:%M:%S%.6f"),
            end.time().format("%H:%M:%S%.6f"),
        );
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

    fn get_first_sample(&self) -> usize {
        self.first_sample
    }

    fn set_first_sample_changed(&mut self) {
        self.first_sample_changed = true;
    }

    fn update_concrete_state(&mut self, other: &mut Box<SamplesExtractor>) {
        let other = other.as_mut_any().downcast_mut::<WaveformBuffer>()
            .expect("WaveformBuffer.update_concrete_state: unable to downcast other ");
        if other.was_exposed {
            self.first_sample_lock = other.first_sample_lock;
            self.sample_seeked = other.sample_seeked;
            self.current_sample = other.current_sample;
            self.requested_sample_window = other.requested_sample_window;
            self.half_requested_sample_window = other.half_requested_sample_window;
            self.requested_step_duration = other.requested_step_duration;
            self.sample_step = other.sample_step;
            self.width = other.width;
            self.height = other.height;

            other.clear_exposed_status();
        } // else: other has nothing new
    }

    fn extract_samples(&mut self, audio_buffer: &AudioBuffer) {
        let (first_visible_sample, last_sample, sample_step) = {
            if self.state.sample_duration_u == 0 {
                self.state.sample_duration = audio_buffer.sample_duration;
                self.state.sample_duration_u = audio_buffer.sample_duration_u;
            }

            if self.requested_sample_window == 0 {
                // not enough info to extract yet
                return;
            }

            // use an integer number of samples per step
            let sample_step = (
                self.requested_step_duration / self.state.sample_duration_u
            ) as usize;

            if audio_buffer.samples.len() < sample_step {
                // buffer too small to render
                return;
            }

            if self.first_sample_changed {
                // upstream buffer's first sample has changed
                //  => force current sample query
                self.current_sample = self.query_current_sample();
            }

            if self.first_sample_lock.is_some()
            && (
                self.current_sample < self.first_sample
                || self.current_sample > self.last_sample
            )
            {   // seeking out of previous window
                // clear previous seeking constraint in current window
                println!("clearing first_sample_lock");
                self.first_sample_lock = None;
                self.sample_seeked = None;
            } // else still in current window => don't worry

            // see how buffers can merge
            let (first_sample, last_sample) =
                if !self.first_sample_changed {
                    // samples appended at the end of the buffer
                    // might use them for current waveform
                    (
                        audio_buffer.first_sample,
                        audio_buffer.last_sample
                    )
                } else {
                    // buffer origin has changed

                    if audio_buffer.first_sample >= self.first_sample
                    && audio_buffer.first_sample < self.last_sample
                    {   // new origin further than current
                        // but buffer can be merged with current waveform
                        // or is contained in current waveform
                        (
                            self.first_sample,
                            audio_buffer.last_sample.max(self.last_sample)
                        )
                    } else if audio_buffer.first_sample < self.first_sample
                    && audio_buffer.last_sample >= self.first_sample
                    {   // samples appended at the begining of the buffer
                        // and can be merge with current waveform
                        (
                            audio_buffer.first_sample,
                            audio_buffer.last_sample.max(self.last_sample)
                        )
                    } else {
                        // not able to merge buffer with current waveform
                        println!("not able to merge");
                        (
                            audio_buffer.first_sample,
                            audio_buffer.last_sample
                        )
                    }
                };

            if audio_buffer.eos {
                // reached the end of the stream
                // draw the end of the buffer to fit in the requested width
                // and adjust current position

                self.first_sample_lock = None;

                if self.current_sample >= first_sample
                && self.current_sample < last_sample
                && self.current_sample
                    >= first_sample + self.half_requested_sample_window
                {   // can set last sample to the right
                    (
                        if let Some(first_sample_lock) = self.first_sample_lock {
                            // an in-window seek constraint is pending
                            first_sample_lock as usize
                        } else {
                            self.current_sample - self.half_requested_sample_window
                        },
                        last_sample,
                        sample_step
                    )
                } else { // set first sample to the left
                    self.first_sample_lock = None;
                    self.sample_seeked = None;

                    (
                        first_sample,
                        last_sample,
                        sample_step
                    )
                }
            } else {
                if self.current_sample
                    >= first_sample + self.half_requested_sample_window
                && self.current_sample + self.half_requested_sample_window
                    < last_sample
                {
                    // regular case where the position can be centered on screen
                    // attempt to get a larger buffer in order to compensate
                    // for the delay when it will actually be drawn
                    // and for potentiel seek backward
                    let first_visible_sample =
                        if let Some(first_sample_lock) = self.first_sample_lock {
                            // an in-window seek constraint is pending
                            first_sample_lock as usize
                        } else {
                            self.current_sample - self.half_requested_sample_window
                        };
                    (
                        first_visible_sample.max(first_sample),
                        last_sample.min(
                            first_visible_sample
                            + self.requested_sample_window + self.half_requested_sample_window
                        ),
                        sample_step
                    )
                } else {
                    // not enough samples for the requested window
                    // around current position
                    self.first_sample_lock = None;
                    self.sample_seeked = None;

                    (
                        first_sample,
                        last_sample.min(
                            first_sample
                            + self.requested_sample_window + self.half_requested_sample_window
                        ),
                        sample_step
                    )
                }
            }
        };

        // align requested first sample in order to keep a steady
        // offset between redraws. This allows using the same samples
        // for a given requested_step_duration and avoiding flickering
        // between redraws
        let mut first_sample =
            first_visible_sample / sample_step * sample_step;
        if first_sample < audio_buffer.first_sample {
            // first sample might be smaller than audio_buffer.first_sample
            // due to alignement on sample_step

            first_sample += sample_step;
        }

        self.update_extraction(
            audio_buffer,
            first_sample,
            last_sample / sample_step * sample_step,
            sample_step
        );

        self.first_sample_changed = false;
    }
}
