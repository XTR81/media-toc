extern crate cairo;

use std::any::Any;

use std::boxed::Box;

use std::sync::{Arc, Mutex};

use media::{AudioBuffer, AudioChannel, DoubleAudioBuffer, SampleExtractor};

use media::sample_extractor::SampleExtractionState;

use super::WaveformImage;

const CONFORTABLE_WIDTH_FACTOR: f64 = 1.1f64;

pub struct DoubleWaveformBuffer {}

impl DoubleWaveformBuffer {
    pub fn new_mutex(buffer_duration: u64) -> Arc<Mutex<DoubleAudioBuffer>> {
        Arc::new(Mutex::new(DoubleAudioBuffer::new(
            buffer_duration,
            Box::new(WaveformBuffer::new(1)),
            Box::new(WaveformBuffer::new(2)),
        )))
    }
}

pub struct SamplePosition {
    pub x: f64,
    pub timestamp: u64,
}

pub struct ImagePositions {
    pub first: SamplePosition,
    pub last: Option<SamplePosition>,
    pub current: Option<SamplePosition>,
}

// A WaveformBuffer hosts one of the two buffers of the double buffering
// mechanism based on the SampleExtractor trait.
// It is responsible for preparing an up to date Waveform image which will be
// diplayed upon UI request. Up to date signifies that the Waveform image
// contains all the samples that can fit in the target window at the specified
// resolution for current playback position.
// Whenever possible, the WaveformBuffer attempts to have the Waveform scroll
// between frames with current playback position in the middle so that the
// user can seek forward or backward around current position.
pub struct WaveformBuffer {
    state: SampleExtractionState,
    conditions_changed: bool,

    image: WaveformImage,

    previous_sample: usize,
    current_sample: usize,
    cursor_sample: usize,   // The sample nb currently under the cursor (might be different from
                            // current sample during seeks)
    cursor_position: u64,   // The timestamp at the cursor's position
    first_visible_sample: Option<usize>,
    first_visible_sample_lock: Option<(i64, bool)>, // (1st position, keep lock)
    sought_sample: Option<usize>,
    pub playback_needs_refresh: bool,

    req_duration_per_1000px: f64,
    width: i32,
    width_f: f64,
    confortable_width_f: f64,
    sample_step_f: f64,
    req_sample_window: usize,
    half_req_sample_window: usize,
    quarter_req_sample_window: usize,

    is_confortable: bool,
}

impl WaveformBuffer {
    pub fn new(id: usize) -> Self {
        WaveformBuffer {
            state: SampleExtractionState::new(),
            conditions_changed: false,

            image: WaveformImage::new(id),

            previous_sample: 0,
            current_sample: 0,
            cursor_sample: 0,
            cursor_position: 0,
            first_visible_sample: None,
            first_visible_sample_lock: None,
            sought_sample: None,
            playback_needs_refresh: false,

            req_duration_per_1000px: 0f64,
            width: 0,
            width_f: 0f64,
            confortable_width_f: 0f64,
            sample_step_f: 0f64,
            req_sample_window: 0,
            half_req_sample_window: 0,
            quarter_req_sample_window: 0,

            is_confortable: false,
        }
    }

    fn refresh_position(&mut self) {
        let (position, mut sample) = self.query_current_sample();
        if self.previous_sample != sample {
            if self.image.contains_eos && sample >= self.image.upper {
                sample = self.image.upper - 1;
            }
            match self.sought_sample {
                None => {
                    self.previous_sample = self.current_sample;
                }
                Some(_) => {
                    // stream has sync after a seek
                    self.sought_sample = None;
                    // reset previous_sample because of the discontinuity
                    self.previous_sample = sample;
                }
            }

            self.current_sample = sample;
            self.cursor_sample = sample;
            self.cursor_position = position;
        } // else don't override self.previous_sample
    }

    pub fn seek(&mut self, position: u64, is_playing: bool) {
        if self.image.sample_step == 0 {
            return;
        }
        // discard any pending lock
        self.first_visible_sample_lock = None;

        let sought_sample = (position / self.state.sample_duration) as usize /
            self.image.sample_step * self.image.sample_step;

        #[cfg(feature = "trace-waveform-buffer")]
        println!(
            concat!(
                "\nWaveformBuffer{}::seek cursor_sample {}, sought sample {} ({}), ",
                "image [{}, {}], contains_eos: {}",
            ),
            self.image.id,
            self.cursor_sample,
            sought_sample,
            position,
            self.image.lower,
            self.image.upper,
            self.image.contains_eos,
        );

        if is_playing {
            // stream is playing => let the cursor jump from current position
            // to the sought position without shifting the waveform if possible
            if let Some(first_visible_sample) = self.first_visible_sample {
                if sought_sample >= first_visible_sample &&
                    sought_sample <= first_visible_sample + self.req_sample_window &&
                    self.image.upper - self.image.lower >= self.req_sample_window
                {
                    // sought sample is in current window
                    // and the window is large enough for a constraint
                    // => lock the first sample so that the cursor appears
                    // at the sought position without abrutely scrolling
                    // the waveform.
                    self.first_visible_sample_lock = Some((
                        first_visible_sample as i64,
                        false, // don't keep lock when scrolling is possible
                    ));
                }
            }
        } else {
            // not playing => jump directly to the sought position
            self.first_visible_sample = None;
        }

        self.sought_sample = Some(sought_sample);
        self.cursor_sample = sought_sample;
    }

    pub fn start_play_range(&mut self) {
        if self.image.sample_step == 0 {
            return;
        }

        if let Some(first_visible_sample) = self.first_visible_sample {
            self.first_visible_sample_lock = Some((
                first_visible_sample as i64,
                true, // keep lock on full playback range
            ));
        }
    }

    pub fn get_position_at(&self, x: f64) -> Option<u64> {
        match self.first_visible_sample {
            Some(ref first_visible_sample) => {
                let sample_at_x = first_visible_sample +
                    (x as usize) / self.image.x_step * self.image.sample_step;

                if !self.image.contains_eos || sample_at_x < self.image.upper {
                    Some(sample_at_x as u64 * self.state.sample_duration)
                } else {
                    // not a valid position
                    None
                }
            }
            None => None,
        }
    }

    // Update to current position and compute the first sample to display.
    fn update_first_visible_sample(&mut self) {
        self.first_visible_sample = if self.image.is_ready() {
            self.refresh_position();

            if self.cursor_sample >= self.image.lower {
                // current sample appears after first buffer sample
                if let Some((first_visible_sample_lock, keep_lock)) =
                        self.first_visible_sample_lock
                {
                    // There is a position lock constraint
                    let offset_to_center = self.cursor_sample as i64 -
                        self.half_req_sample_window as i64 -
                        first_visible_sample_lock;
                    if keep_lock || offset_to_center < -(2 * self.image.sample_step as i64) {
                        // cursor in first half of the window
                        // keep origin on the first sample upon seek
                        Some(first_visible_sample_lock as usize)
                    } else if (offset_to_center as usize) <= 2 * self.image.sample_step {
                        // reached the center => keep cursor there
                        self.first_visible_sample_lock = None;
                        self.sought_sample = None;
                        Some(self.image.lower.max(
                            self.cursor_sample -
                                self.half_req_sample_window,
                        ))
                    } else if first_visible_sample_lock as usize + self.req_sample_window <
                               self.image.upper
                    {
                        // Cursor is on the right half of the window
                        // and the target sample window doesn't exceed the end
                        // of the rendered waveform yet
                        // => progressively get cursor back to center
                        let previous_offset = self.previous_sample as i64 -
                            first_visible_sample_lock;
                        let delta_cursor = self.cursor_sample as i64 - self.previous_sample as i64;
                        let next_lower = (self.image.lower as i64).max(
                            self.cursor_sample as i64 - previous_offset +
                                delta_cursor,
                        );

                        self.first_visible_sample_lock = Some((next_lower, false));
                        Some(next_lower as usize)
                    } else {
                        // Not enough overhead to get cursor back to center
                        // Follow toward the last sample
                        let next_lower =
                            if self.image.lower + self.req_sample_window < self.image.upper {
                                // buffer window is larger than req_sample_window
                                // set last buffer to the right
                                let next_lower = self.image.upper - self.req_sample_window;
                                if !self.image.contains_eos {
                                    // but keep the constraint in case more samples
                                    // are added afterward
                                    self.first_visible_sample_lock =
                                        Some((next_lower as i64, false));
                                } else {
                                    // reached EOS => don't expect returning to center
                                    self.first_visible_sample_lock = None;
                                }

                                next_lower
                            } else {
                                // buffer window is smaller than req_sample_window
                                // set first sample to the left
                                self.first_visible_sample_lock = None;

                                self.image.lower
                            };

                        Some(next_lower)
                    }
                } else if self.cursor_sample + self.half_req_sample_window <= self.image.upper {
                    // cursor_sample fits in the first half of the window with last sample further
                    if self.cursor_sample > self.image.lower + self.half_req_sample_window {
                        // cursor_sample can be centered
                        Some(self.cursor_sample - self.half_req_sample_window)
                    } else {
                        // cursor_sample before half of displayable window
                        // set origin to the first sample in the buffer
                        // current sample will be displayed between the origin
                        // and the center
                        Some(self.image.lower)
                    }
                } else if self.cursor_sample >= self.image.upper {
                    // cursor_sample appears after image last sample
                    #[cfg(feature = "trace-waveform-buffer")]
                    println!(
                        concat!(
                            "WaveformBuffer{}::update_first_visible_sample ",
                            "cursor_sample {} appears above image upper bound {}",
                        ),
                        self.image.id,
                        self.cursor_sample,
                        self.image.upper,
                    );
                    if self.image.upper + self.req_sample_window >= self.cursor_sample {
                        // rebase image attempting to keep in range
                        // even if samples are not rendered yet
                        if self.cursor_sample > self.image.lower + self.req_sample_window {
                            Some(self.cursor_sample - self.req_sample_window)
                        } else {
                            Some(self.image.lower)
                        }
                    } else {
                        // cursor no longer in range, 2 cases:
                        // - seeking forward
                        // - zoomed-in too much to keep up with the audio stream
                        None
                    }
                } else if self.image.lower + self.req_sample_window < self.image.upper {
                    // buffer window is larger than req_sample_window
                    // set last buffer to the right
                    Some(self.image.upper - self.req_sample_window)
                } else {
                    // buffer window is smaller than req_sample_window
                    // set first sample to the left
                    Some(self.image.lower)
                }
            } else if self.cursor_sample + self.req_sample_window > self.image.lower {
                // cursor is close enough to the image
                // => render what can be rendered
                Some(self.image.lower)
            } else {
                // cursor_sample appears before image first sample
                // => wait until situation clarifies
                #[cfg(feature = "trace-waveform-buffer")]
                println!(
                    concat!(
                        "WaveformBuffer{}::update_first_visible_sample cursor_sample {} ",
                        "appears before image first sample {}",
                    ),
                    self.image.id,
                    self.cursor_sample,
                    self.image.lower
                );
                None
            }
        } else {
            // no image available yet
            None
        };
    }

    // Update rendering conditions
    pub fn update_conditions(&mut self, duration_per_1000px: f64, width: i32, height: i32) {
        let (duration_changed, mut scale_factor) =
            if (duration_per_1000px - self.req_duration_per_1000px).abs() < 1f64 {
                (false, 0f64)
            } else {
                let prev_duration = self.req_duration_per_1000px;
                self.req_duration_per_1000px = duration_per_1000px;
                self.update_sample_step();
                (true, duration_per_1000px / prev_duration)
            };

        let width_changed = if width == self.width {
            false
        } else {
            let width_f = f64::from(width);
            let prev_width_f = self.width_f;

            if prev_width_f != 0f64 {
                scale_factor = width_f / prev_width_f;
            }

            self.width = width;
            self.width_f = width_f;
            self.confortable_width_f = CONFORTABLE_WIDTH_FACTOR * width_f;
            true
        };

        self.image.update_dimensions(
            self.sample_step_f,
            width,
            height,
        );

        if duration_changed || width_changed {
            self.update_sample_window();

            // update first sample in order to match new conditions
            if scale_factor != 0f64 {
                self.first_visible_sample = match self.first_visible_sample {
                    Some(first_visible_sample) => {
                        let new_first_visible_sample = first_visible_sample as i64 +
                            ((self.cursor_sample as i64 - first_visible_sample as i64) as
                                 f64 * (1f64 - scale_factor)) as i64;

                        if new_first_visible_sample > self.image.sample_step as i64 {
                            #[cfg(feature = "trace-waveform-buffer")]
                            println!(
                                concat!(
                                    "WaveformBuffer{}::rebase range [{}, {}], window {}, ",
                                    "first {} -> {}, sample_step {}, cursor_sample {}",
                                ),
                                self.image.id,
                                self.image.lower,
                                self.image.upper,
                                self.req_sample_window,
                                first_visible_sample,
                                new_first_visible_sample,
                                self.image.sample_step,
                                self.cursor_sample,
                            );

                            if let Some((_first_visible_sample_lock, keep_lock)) =
                                self.first_visible_sample_lock
                            {
                                // There is a first visible sample constraint
                                // => adapt it to match the new zoom
                                self.first_visible_sample_lock = Some((
                                    new_first_visible_sample,
                                    keep_lock,
                                ));
                            }

                            Some(new_first_visible_sample as usize)
                        } else {
                            // first_visible_sample can be snapped to the beginning
                            // => have refresh and update_first_visible_sample
                            // compute the best range for this situation
                            None
                        }
                    }
                    None => None,
                };
            }
        }
    }

    fn update_sample_step(&mut self) {
        // compute a sample step which will produce an integer number of
        // samples per pixel or an integer number of pixels per samples

        self.sample_step_f =
            if self.req_duration_per_1000px >= self.state.duration_per_1000_samples {
                (self.req_duration_per_1000px / self.state.duration_per_1000_samples).floor()
            } else {
                1f64 / (self.state.duration_per_1000_samples / self.req_duration_per_1000px).ceil()
            };
        self.conditions_changed = true;
    }

    fn update_sample_window(&mut self) {
        // force sample window to an even number of samples
        // so that the cursor can be centered
        // and make sure to cover at least the width requested
        let half_req_sample_window = (self.sample_step_f * self.width_f / 2f64) as usize;
        let req_sample_window = half_req_sample_window * 2;

        #[cfg(feature = "trace-waveform-buffer")]
        {
            if req_sample_window != self.req_sample_window {
                println!(
                    "\nWaveformBuffer{}::update_sample_window smpl.window prev. {}, new {}",
                    self.image.id,
                    self.req_sample_window,
                    req_sample_window
                );
            }
        }

        self.req_sample_window = req_sample_window;
        self.quarter_req_sample_window = half_req_sample_window / 2;
        self.half_req_sample_window = half_req_sample_window;
        self.conditions_changed = true;
    }

    // Get the waveform as an image in current conditions.
    // This function is to be called as close as possible to
    // the actual presentation of the waveform.
    pub fn get_image(&mut self) -> Option<(&cairo::ImageSurface, ImagePositions)> {
        #[cfg(feature = "trace-waveform-buffer")]
        {
            if let Some(sought_sample) = self.sought_sample {
                if self.first_visible_sample_lock.is_none() {
                    println!(
                        "WaveformBuffer{}::get_image without lock, seeking to {}",
                        self.image.id,
                        sought_sample,
                    );
                } else {
                    println!(
                        "WaveformBuffer{}::get_image with lock, seeking to {}",
                        self.image.id,
                        sought_sample,
                    );
                }
            }
        }

        self.update_first_visible_sample();
        match self.first_visible_sample {
            Some(first_visible_sample) => {
                let cursor_opt = if self.cursor_sample >= first_visible_sample &&
                    self.cursor_sample <= first_visible_sample + self.req_sample_window
                {
                    Some(SamplePosition {
                        x: ((self.cursor_sample as f64 - first_visible_sample as f64) /
                            self.image.sample_step_f),
                        timestamp: self.cursor_position,
                    })
                } else {
                    None
                };

                let x_offset = ((first_visible_sample - self.image.lower) * self.image.x_step /
                                    self.image.sample_step) as f64;

                let last_opt = match self.image.last {
                    Some(ref last) => {
                        let delta_x = last.x - x_offset;

                        // rendering is confortable if the displayable window is larger than
                        // the confortable_width_f and cursor is in the first half of the window
                        self.is_confortable = delta_x > self.confortable_width_f &&
                            self.cursor_sample + self.half_req_sample_window <= self.image.upper;

                        let last_x = delta_x.min(self.width_f);
                        if last_x.is_sign_positive() {
                            Some(SamplePosition {
                                x: last_x,
                                timestamp: (first_visible_sample +
                                                (last_x * self.image.sample_step_f) as usize
                                            ) as u64 * self.state.sample_duration,
                            })
                        } else {
                            None
                        }
                    }
                    None => {
                        self.is_confortable = false;
                        None
                    }
                };

                Some((
                    self.image.get_image(),
                    ImagePositions {
                        first: SamplePosition {
                            x: x_offset,
                            timestamp: first_visible_sample as u64 * self.state.sample_duration,
                        },
                        last: last_opt,
                        current: cursor_opt,
                    },
                ))
            }
            None => {
                self.is_confortable = false;
                None
            }
        }
    }

    #[cfg_attr(feature = "cargo-clippy", allow(collapsible_if))]
    fn get_sample_range(&mut self, audio_buffer: &AudioBuffer) -> (usize, usize) {
        // First step: see how current waveform and the audio_buffer can merge
        let (lower, upper) = if audio_buffer.lower <= self.image.lower &&
            audio_buffer.upper >= self.image.upper
        {
            // waveform contained in buffer => regular case
            (audio_buffer.lower, audio_buffer.upper)
        } else if audio_buffer.lower >= self.image.lower && audio_buffer.lower < self.image.upper {
            // last segment further than current image origin
            // but buffer can be merged with current waveform
            // or is contained in current waveform
            (self.image.lower, audio_buffer.upper.max(self.image.upper))
        } else if audio_buffer.lower < self.image.lower && audio_buffer.upper >= self.image.lower {
            // current waveform overlaps with buffer on its left
            // or is contained in buffer
            (audio_buffer.lower, audio_buffer.upper.max(self.image.upper))
        } else {
            // not able to merge buffer with current waveform
            // synchronize on latest segment received
            #[cfg(feature = "trace-waveform-buffer")]
            println!(
                concat!(
                    "WaveformBuffer{}::get_sample_range not able to merge: ",
                    "cursor {}, image [{}, {}], buffer [{}, {}], segment: {}",
                ),
                self.image.id,
                self.cursor_sample,
                self.image.lower,
                self.image.upper,
                audio_buffer.lower,
                audio_buffer.upper,
                audio_buffer.segment_lower,
            );

            self.first_visible_sample = None;
            self.first_visible_sample_lock = None;

            (audio_buffer.segment_lower, audio_buffer.upper)
        };

        // Second step: find the range to display
        let extraction_range = if upper - lower <= self.req_sample_window {
            // image can use the full window
            #[cfg(feature = "trace-waveform-buffer")]
            println!("WaveformBuffer{}::get_sample_range using full window, range [{}, {}]",
                self.image.id,
                lower,
                upper,
            );

            self.first_visible_sample = None;
            self.first_visible_sample_lock = None;

            Some((lower, upper))
        } else {
            match self.first_visible_sample {
                Some(first_visible_sample) => {
                    if self.cursor_sample >= first_visible_sample &&
                        self.cursor_sample < first_visible_sample + self.req_sample_window
                    {
                        // cursor is still in the window => keep it
                        Some((
                            first_visible_sample as usize,
                            upper.min(
                                first_visible_sample + self.req_sample_window +
                                    self.half_req_sample_window,
                            ),
                        ))
                    } else {
                        #[cfg(feature = "trace-waveform-buffer")]
                        println!(
                            concat!(
                                "WaveformBuffer{}::get_sample_range first_visible_sample ",
                                "{} is below lower, range [{}, {}], self.cursor: {}",
                            ),
                            self.image.id,
                            first_visible_sample,
                            lower,
                            upper,
                            self.cursor_sample,
                        );

                        match self.first_visible_sample_lock {
                            Some((first_visible_sample, true)) => {
                                // don't remove lock
                                let first_visible_sample = first_visible_sample as usize;
                                Some((
                                    first_visible_sample,
                                    upper.min(
                                        first_visible_sample + self.req_sample_window +
                                            self.half_req_sample_window,
                                    ),
                                ))
                            }
                            _ => {
                                self.first_visible_sample = None;
                                self.first_visible_sample_lock = None;

                                None
                            }
                        }
                    }
                }
                None => {
                    if self.cursor_sample > lower + self.half_req_sample_window &&
                        self.cursor_sample < upper
                    {
                        // cursor can be centered or is in second half of the window
                        if self.cursor_sample + self.half_req_sample_window < upper {
                            // cursor can be centered
                            #[cfg(feature = "trace-waveform-buffer")]
                            println!(
                                "WaveformBuffer{}::get_sample_range centering cursor: {}",
                                self.image.id,
                                self.cursor_sample,
                            );

                            Some((
                                self.cursor_sample - self.half_req_sample_window,
                                upper.min(self.cursor_sample + self.req_sample_window),
                            ))
                        } else {
                            // cursor in second half
                            #[cfg(feature = "trace-waveform-buffer")]
                            println!(
                                "WaveformBuffer{}::get_sample_range cursor: {} in second half",
                                self.image.id,
                                self.cursor_sample,
                            );

                            // attempt to get an optimal range
                            if upper >
                                lower + self.req_sample_window + self.quarter_req_sample_window
                            {
                                Some((
                                    upper - self.req_sample_window -
                                        self.quarter_req_sample_window,
                                    upper,
                                ))
                            } else if upper > lower + self.req_sample_window {
                                Some((upper - self.req_sample_window, upper))
                            } else {
                                // use defaults
                                None
                            }
                        }
                    } else {
                        #[cfg(feature = "trace-waveform-buffer")]
                        println!(
                            concat!(
                                "WaveformBuffer{}::get_sample_range cursor ",
                                "{} in first half, range [{}, {}]",
                            ),
                            self.image.id,
                            self.cursor_sample,
                            lower,
                            upper,
                        );

                        None
                    }
                }
            }
        };

        // Third step: fallback to defaults if previous step failed
        extraction_range.unwrap_or((
            audio_buffer.segment_lower,
            audio_buffer.upper.min(
                audio_buffer.segment_lower + self.req_sample_window +
                    self.half_req_sample_window,
            ),
        ))
    }
}

// This is a container to pass conditions via the refresh
// function of the SampleExtractor trait
#[derive(Clone)]
pub struct WaveformConditions {
    pub duration_per_1000px: f64,
    pub width: i32,
    pub height: i32,
}

impl WaveformConditions {
    pub fn new(duration_per_1000px: f64, width: i32, height: i32) -> Self {
        WaveformConditions {
            duration_per_1000px: duration_per_1000px,
            width: width,
            height: height,
        }
    }
}

impl SampleExtractor for WaveformBuffer {
    fn as_mut_any(&mut self) -> &mut Any {
        self
    }

    fn as_any(&self) -> &Any {
        self
    }

    fn get_extraction_state(&self) -> &SampleExtractionState {
        &self.state
    }

    fn get_extraction_state_mut(&mut self) -> &mut SampleExtractionState {
        &mut self.state
    }

    fn get_lower(&self) -> usize {
        self.image.lower
    }

    fn cleanup(&mut self) {
        // clear for reuse
        self.state.cleanup();
        self.conditions_changed = false;

        self.image.cleanup();

        self.previous_sample = 0;
        self.current_sample = 0;
        self.cursor_sample = 0;
        self.cursor_position = 0;
        self.first_visible_sample = None;
        self.first_visible_sample_lock = None;
        self.sought_sample = None;
        self.playback_needs_refresh = false;

        self.req_duration_per_1000px = 0f64;
        self.width = 0;
        self.width_f = 0f64;
        self.confortable_width_f = 0f64;
        self.sample_step_f = 0f64;
        self.req_sample_window = 0;
        self.half_req_sample_window = 0;
        self.quarter_req_sample_window = 0;

        self.is_confortable = false;
    }

    fn set_sample_duration(&mut self, per_sample: u64, per_1000_samples: f64) {
        #[cfg(feature = "trace-waveform-buffer")]
        println!(
            "WaveformBuffer{}::set_sample_duration per_sample {}",
            self.image.id,
            per_sample
        );
        self.state.sample_duration = per_sample;
        self.state.duration_per_1000_samples = per_1000_samples;
        self.update_sample_step();
        self.update_sample_window();
    }

    fn set_channels(&mut self, channels: &[AudioChannel]) {
        self.image.set_channels(channels);
    }

    fn set_conditions(&mut self, conditions: Box<Any>) {
        let cndt = conditions.downcast::<WaveformConditions>().expect(
            "WaveformBuffer::set_conditions conditions is not a WaveformConditions",
        );
        self.update_conditions(cndt.duration_per_1000px, cndt.width, cndt.height);
    }

    fn drop_continuity(&mut self) {
        self.first_visible_sample = None;
        self.first_visible_sample_lock = None;
    }

    fn update_concrete_state(&mut self, other: &mut SampleExtractor) {
        let other = other.as_mut_any().downcast_mut::<WaveformBuffer>().expect(
            "WaveformBuffer.update_concrete_state: unable to downcast other ",
        );

        self.previous_sample = other.previous_sample;
        self.current_sample = other.current_sample;
        self.cursor_sample = other.cursor_sample;
        self.cursor_position = other.cursor_position;
        self.first_visible_sample = other.first_visible_sample;
        self.first_visible_sample_lock = other.first_visible_sample_lock;
        self.sought_sample = other.sought_sample;

        // playback_needs_refresh is set during extract_samples
        // so other must be updated with self status
        #[cfg(feature = "trace-waveform-buffer")]
        {
            if self.playback_needs_refresh && !other.playback_needs_refresh {
                println!("WaveformBuffer{}::update_concrete_state setting playback_needs_refresh",
                    other.image.id,
                );
            }
        }
        other.playback_needs_refresh = self.playback_needs_refresh;

        if other.conditions_changed {
            self.req_duration_per_1000px = other.req_duration_per_1000px;
            self.width = other.width;
            self.width_f = other.width_f;
            self.confortable_width_f = other.confortable_width_f;
            self.sample_step_f = other.sample_step_f;
            self.req_sample_window = other.req_sample_window;
            self.half_req_sample_window = other.half_req_sample_window;
            self.quarter_req_sample_window = other.quarter_req_sample_window;

            other.conditions_changed = false;
        } // else: other has nothing new

        self.image.update_from_other(&mut other.image);
    }

    // This is the entry point for the update of the waveform.
    // This function tries to merge the samples added to the AudioBuffer
    // since last extraction and adapts to the evolving conditions of
    // the playback position and target rendering dimensions and
    // resolution.
    #[cfg_attr(feature = "cargo-clippy", allow(needless_bool))]
    fn extract_samples(&mut self, audio_buffer: &AudioBuffer) {
        if self.req_sample_window == 0 {
            // conditions not defined yet
            return;
        }

        // Get the available sample range considering both
        // the waveform image and the AudioBuffer
        let (lower, upper) = self.get_sample_range(audio_buffer);
        self.image.render(
            audio_buffer,
            lower,
            upper,
            self.is_confortable,
        );

        self.playback_needs_refresh = if audio_buffer.eos && !self.image.contains_eos {
            // there won't be any refresh on behalf of audio_buffer
            // and image will still need more sample if playback continues
            #[cfg(feature = "trace-waveform-buffer")]
            println!("WaveformBuffer{}::extract_samples setting playback_needs_refresh",
                self.image.id,
            );

            true
        } else {
            #[cfg(feature = "trace-waveform-buffer")]
            {
                if self.playback_needs_refresh {
                    println!("WaveformBuffer{}::extract_samples resetting playback_needs_refresh",
                        self.image.id,
                    );
                }
            }
            false
        };
    }

    fn refresh(&mut self, audio_buffer: &AudioBuffer) {
        if self.image.is_ready {
            // Note: current state is up to date (updated from DoubleAudioBuffer)

            let (lower, upper) = self.get_sample_range(audio_buffer);
            self.image.render(audio_buffer, lower, upper, false);

            self.playback_needs_refresh = {
                #[cfg(feature = "trace-waveform-buffer")]
                {
                    if self.playback_needs_refresh && self.image.contains_eos {
                        println!("WaveformBuffer{}::refresh resetting playback_needs_refresh",
                            self.image.id,
                        );
                    }
                }

                !self.image.contains_eos
            };
        } // else: no need to refresh
    }

    // Refresh the waveform in its current sample range and position
    fn refresh_with_conditions(&mut self, audio_buffer: &AudioBuffer, conditions: Box<Any>) {
        self.set_conditions(conditions);
        self.refresh(audio_buffer);
    }
}
