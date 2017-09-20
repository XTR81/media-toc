extern crate gstreamer as gst;
use gstreamer::{ElementExtManual, QueryView};

use std::any::Any;

use std::mem;

use std::sync::{Arc, Mutex};

use super::AudioBuffer;

// DoubleSampleExtractor hosts two SampleExtractors
// that can be swapped to implement a double buffer mechanism
// which selects a subset of samples depending on external
// conditions
pub struct DoubleSampleExtractor {
    pub exposed_buffer_mtx: Arc<Mutex<Box<SamplesExtractor>>>,
    pub samples_offset: usize,
    working_buffer: Option<Box<SamplesExtractor>>,
}

impl DoubleSampleExtractor {
    // need 2 arguments for new as we can't clone buffers as they are known
    // as trait SamplesExtractor
    pub fn new(
        exposed_buffer: Arc<Mutex<Box<SamplesExtractor>>>,
        working_buffer: Box<SamplesExtractor>
    ) -> DoubleSampleExtractor {
        DoubleSampleExtractor {
            exposed_buffer_mtx: exposed_buffer,
            samples_offset: 0,
            working_buffer: Some(working_buffer),
        }
    }

    pub fn set_audio_sink(&mut self, audio_sink: &gst::Element) {
        {
            let exposed_buffer = &mut self.exposed_buffer_mtx.lock()
                .expect("Couldn't lock exposed_buffer_mtx while setting audio sink");
            exposed_buffer.set_audio_sink(audio_sink.clone());
        }
        self.working_buffer.as_mut()
            .expect("Couldn't get working_buffer while setting audio sink")
            .set_audio_sink(audio_sink.clone());
    }

    pub fn extract_samples(&mut self, audio_buffer: &AudioBuffer) {
        let mut working_buffer = self.working_buffer.take()
            .expect("DoubleSampleExtractor: failed to take working buffer while updating");
        working_buffer.extract_samples(audio_buffer);

        // swap buffers
        {
            let exposed_buffer_box = &mut *self.exposed_buffer_mtx.lock()
                .expect("DoubleSampleExtractor: failed to lock the exposed buffer for swap");
            mem::swap(exposed_buffer_box, &mut working_buffer);
        }

        self.samples_offset = working_buffer.get_sample_offset();
        self.working_buffer = Some(working_buffer);
        // self.working_buffer is now the buffer previously in
        // self.exposed_buffer_mtx
    }
}

unsafe impl Sync for DoubleSampleExtractor {}

pub struct SamplesExtractionState {
    pub sample_duration: f64,
    pub sample_duration_u: u64,

    pub current_sample: usize,
    pub samples_offset: usize,
    pub last_sample: usize,

    pub requested_sample_window: usize,
    pub half_requested_sample_window: usize,
    pub requested_step_duration: u64,

    pub sample_step: usize,

    audio_sink: Option<gst::Element>,
    position_query: gst::Query,
}

impl SamplesExtractionState {
    pub fn new() -> Self {
        SamplesExtractionState {
            sample_duration: 0f64,
            sample_duration_u: 0,

            current_sample: 0,
            samples_offset: 0,
            last_sample: 0,

            requested_sample_window: 0,
            half_requested_sample_window: 0,
            requested_step_duration: 0,

            sample_step: 0,

            audio_sink: None,
            position_query: gst::Query::new_position(gst::Format::Time),
        }
    }

    pub fn set_audio_sink(&mut self, audio_sink: gst::Element) {
        self.audio_sink = Some(audio_sink);
    }

    pub fn query_current_sample(&mut self) {
        self.audio_sink.as_ref()
            .expect("DoubleSampleExtractor: no audio ref while querying position")
            .query(self.position_query.get_mut().unwrap());
        self.current_sample = (
            match self.position_query.view() {
                QueryView::Position(ref position) => position.get().1 as f64,
                _ => unreachable!(),
            } / self.sample_duration
        ).round() as usize;
    }
}

pub trait SamplesExtractor: Send {
    fn as_mut_any(&mut self) -> &mut Any;
    fn get_extraction_state(&self) -> &SamplesExtractionState;
    fn get_extraction_state_mut(&mut self) -> &mut SamplesExtractionState;

    fn cleanup_state(&mut self) {
        // clear for reuse
        let state = self.get_extraction_state_mut();
        state.sample_duration = 0f64;
        state.sample_duration_u = 0;
        state.current_sample = 0;
        state.samples_offset = 0;
        state.last_sample = 0;
        state.requested_sample_window = 0;
        state.half_requested_sample_window = 0;
        state.requested_step_duration = 0;
        state.sample_step = 0;
        state.audio_sink = None;
    }

    fn set_audio_sink(&mut self, audio_sink: gst::Element) {
        self.get_extraction_state_mut().set_audio_sink(audio_sink);
    }

    fn can_extract(&self) -> bool;

    fn update_extraction(&mut self,
        audio_buffer: &AudioBuffer,
        first_sample: usize,
        last_sample: usize,
        sample_step: usize,
    );

    fn get_sample_offset(&self) -> usize {
        let state = self.get_extraction_state();
        state.samples_offset
    }

    fn extract_samples(&mut self, audio_buffer: &AudioBuffer) {
        let (first_visible_sample, last_sample, sample_step) = {
            let can_extract = self.can_extract();

            let state = self.get_extraction_state_mut();
            state.sample_duration = audio_buffer.sample_duration;
            state.sample_duration_u = audio_buffer.sample_duration_u;

            if !can_extract {
                return;
            }

            // use an integer number of samples per step
            let sample_step = (
                state.requested_step_duration / state.sample_duration_u
            ) as usize;

            if audio_buffer.samples.len() < sample_step {
                // buffer too small to render
                return;
            }

            if state.current_sample < audio_buffer.samples_offset
            || state.current_sample > audio_buffer.last_sample {
                // probably seeking => force current sample query
                state.query_current_sample();
            }

            if audio_buffer.eos {
                // reached the end of stream
                // draw the end of the buffer to fit in the requested width
                // and adjust current position

                if state.current_sample >= audio_buffer.samples_offset
                && state.current_sample < audio_buffer.last_sample
                && state.current_sample
                    >= audio_buffer.samples_offset + state.half_requested_sample_window
                {
                    (
                        state.current_sample - state.half_requested_sample_window,
                        audio_buffer.last_sample,
                        sample_step
                    )
                } else {
                    (
                        audio_buffer.samples_offset,
                        audio_buffer.last_sample,
                        sample_step
                    )
                }
            } else {
                if state.current_sample
                    >= audio_buffer.samples_offset + state.half_requested_sample_window
                && state.current_sample + state.half_requested_sample_window
                    < audio_buffer.last_sample
                {
                    // regular case where the position can be centered on screen
                    // attempt to get a larger buffer in order to compensate
                    // for the delay when it will actually be drawn
                    let first_visible_sample =
                        state.current_sample - state.half_requested_sample_window;
                    (
                        first_visible_sample,
                        audio_buffer.last_sample.min(
                            first_visible_sample
                            + state.requested_sample_window + state.half_requested_sample_window
                        ),
                        sample_step
                    )
                } else {
                    // not enough samples for the requested window
                    // around current position
                    (
                        audio_buffer.samples_offset,
                        audio_buffer.last_sample.min(
                            audio_buffer.samples_offset
                            + state.requested_sample_window + state.half_requested_sample_window
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
        if first_sample < audio_buffer.samples_offset {
            // first sample might be smaller than audio_buffer.samples_offset
            // due to alignement on sample_step

            first_sample += sample_step;
        }

        self.update_extraction(
            audio_buffer,
            first_sample,
            last_sample / sample_step * sample_step,
            sample_step
        );
    }
}
