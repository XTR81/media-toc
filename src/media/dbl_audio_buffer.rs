use gstreamer as gst;
use gstreamer_audio as gst_audio;

use std::any::Any;

use std::mem;

use std::sync::{Arc, Mutex};

use super::{AudioBuffer, AudioChannel, QUEUE_SIZE_NS, SampleExtractor};

const EXTRACTION_THRESHOLD: usize = 1024;

// The DoubleBuffer is reponsible for ensuring a thread safe double buffering
// mechanism that receives samples from GStreamer, prepares an extraction of
// these samples and presents the most recent extraction to an external
// mechanism (e.g. UI).
// The DoubleBuffer hosts two SampleExtractors and an AudioBuffer:
//   - The SampleExtractor trait is designed to allow the extraction of samples
//   depending on certains conditions as defined in its concrete implementation.
//   - The AudioBuffer manages the container for all the samples received.
// The DoubleBuffer prepares the extraction in the working_buffer and exposes
// the exposed_buffer during that time. When the extration is done, the buffers
// are swapped.
pub struct DoubleAudioBuffer {
    audio_buffer: AudioBuffer,
    samples_since_last_extract: usize,
    exposed_buffer_mtx: Arc<Mutex<Box<SampleExtractor>>>,
    working_buffer: Option<Box<SampleExtractor>>,
    lower_to_keep: usize,
    sample_gauge: Option<usize>,
    sample_window: usize,
    max_sample_window: usize,
    can_handle_eos: bool, // accept / ignore eos (required for range seeks handling)
}

impl DoubleAudioBuffer {
    // need 2 arguments for new as we can't clone buffers as they are known
    // as trait SampleExtractor
    pub fn new(
        buffer_duration: u64,
        exposed_buffer: Box<SampleExtractor>,
        working_buffer: Box<SampleExtractor>,
    ) -> DoubleAudioBuffer {
        DoubleAudioBuffer {
            audio_buffer: AudioBuffer::new(buffer_duration),
            samples_since_last_extract: 0,
            exposed_buffer_mtx: Arc::new(Mutex::new(exposed_buffer)),
            working_buffer: Some(working_buffer),
            lower_to_keep: 0,
            sample_gauge: None,
            sample_window: 0,
            max_sample_window: 0,
            can_handle_eos: true,
        }
    }

    // Get a reference on the exposed buffer mutex.
    pub fn get_exposed_buffer_mtx(&self) -> Arc<Mutex<Box<SampleExtractor>>> {
        Arc::clone(&self.exposed_buffer_mtx)
    }

    pub fn cleanup(&mut self) {
        self.reset();
        self.audio_buffer.cleanup();

        {
            let exposed_buffer = &mut self.exposed_buffer_mtx.lock().expect(
                "DoubleAudioBuffer: couldn't lock exposed_buffer_mtx while setting audio sink",
            );
            exposed_buffer.cleanup();
        }

        self.working_buffer
            .as_mut()
            .expect("DoubleAudioBuffer: couldn't get working_buffer while setting audio sink")
            .cleanup();
    }

    fn reset(&mut self) {
        self.samples_since_last_extract = 0;
        self.lower_to_keep = 0;
        self.sample_gauge = None;
        self.sample_window = 0;
        self.max_sample_window = 0;
        self.can_handle_eos = true;
    }

    pub fn set_caps(&mut self, caps: &gst::CapsRef) {
        let audio_info = gst_audio::AudioInfo::from_caps(caps)
            .expect("DoubleAudioBuffer::set_caps unable to get AudioInfo");

        self.reset();

        let rate = u64::from(audio_info.rate());
        let channels = audio_info.channels() as usize;

        let sample_duration = 1_000_000_000 / rate;
        self.max_sample_window = (QUEUE_SIZE_NS / sample_duration) as usize;
        let duration_for_1000_samples = 1_000_000_000_000f64 / (rate as f64);

        let mut channels: Vec<AudioChannel> = Vec::with_capacity(channels);
        if let Some(positions) = audio_info.positions() {
            for position in positions {
                channels.push(AudioChannel::new(position));
            }
        };

        self.audio_buffer.init(audio_info);

        {
            let exposed_buffer = &mut self.exposed_buffer_mtx.lock().expect(
                "DoubleAudioBuffer: couldn't lock exposed_buffer_mtx while setting audio sink",
            );
            exposed_buffer.set_sample_duration(sample_duration, duration_for_1000_samples);
            exposed_buffer.set_channels(&channels);
        }

        {
            let working_buffer = self.working_buffer
                .as_mut()
                .expect("DoubleAudioBuffer: couldn't get working_buffer while setting audio sink");
            working_buffer.set_sample_duration(sample_duration, duration_for_1000_samples);
            working_buffer.set_channels(&channels);
        }
    }

    pub fn set_ref(&mut self, audio_ref: &gst::Element) {
        {
            let exposed_buffer = &mut self.exposed_buffer_mtx.lock().expect(
                "DoubleAudioBuffer: couldn't lock exposed_buffer_mtx while setting audio sink",
            );
            exposed_buffer.set_audio_sink(audio_ref.clone());
        }

        let working_buffer = self.working_buffer
            .as_mut()
            .expect("DoubleAudioBuffer: couldn't get working_buffer while setting audio sink");
        working_buffer.set_audio_sink(audio_ref.clone());
    }

    // Init the buffers with the provided conditions.
    // Conditions concrete type must conform to a struct expected
    // by the concrete implementation of the SampleExtractor.
    pub fn set_conditions<T: Any + Clone>(&mut self, conditions: Box<T>) {
        {
            self.working_buffer
                .as_mut()
                .expect("DoubleAudioBuffer::set_conditions failed to take working buffer")
                .set_conditions(conditions.clone());
        }

        {
            let exposed_buffer_box = &mut *self.exposed_buffer_mtx
                .lock()
                .expect("DoubleAudioBuffer:::set_conditions failed to lock the exposed buffer");
            exposed_buffer_box.set_conditions(conditions);
        }
    }

    pub fn ignore_eos(&mut self) {
        self.can_handle_eos = false;
    }

    pub fn accept_eos(&mut self) {
        self.can_handle_eos = true;
    }

    pub fn handle_eos(&mut self) {
        if self.can_handle_eos {
            self.audio_buffer.handle_eos();
            // extract last samples and swap
            self.extract_samples();
            // do it again to update second extractor too
            // this is required in case of a subsequent seek
            // in the extractors' range
            self.extract_samples();
        }
        self.sample_gauge = None
    }

    pub fn have_gst_segment(&mut self, segment: &gst::Segment) {
        self.audio_buffer.have_gst_segment(segment);
        self.sample_gauge = Some(0);
    }

    pub fn push_gst_buffer(&mut self, buffer: &gst::Buffer) -> bool {
        // store incoming samples
        let sample_nb = self.audio_buffer
            .push_gst_buffer(buffer, self.lower_to_keep);
        self.samples_since_last_extract += sample_nb;

        let sample_window = self.sample_window;
        let must_notify = self.sample_gauge.as_mut().map_or(false, |gauge| {
            *gauge += sample_nb;
            *gauge > sample_window
        });

        if must_notify || self.samples_since_last_extract >= EXTRACTION_THRESHOLD {
            // extract new samples and swap
            self.extract_samples();
            self.samples_since_last_extract = 0;

            if must_notify {
                self.sample_gauge = None;
            }
        }

        must_notify
    }

    // Update the working extractor with new samples and swap.
    pub fn extract_samples(&mut self) {
        let mut working_buffer = self.working_buffer
            .take()
            .expect("DoubleAudioBuffer::extract_samples: failed to take working buffer");
        working_buffer.extract_samples(&self.audio_buffer);

        // swap buffers
        {
            let exposed_buffer_box = &mut *self.exposed_buffer_mtx
                .lock()
                .expect("DoubleAudioBuffer::extract_samples: failed to lock the exposed buffer");
            // get latest state from the previously exposed buffer
            // in order to smoothen rendering between frames
            working_buffer.update_concrete_state(exposed_buffer_box.as_mut());
            mem::swap(exposed_buffer_box, &mut working_buffer);
        }

        self.lower_to_keep = working_buffer.get_lower();
        self.sample_window = working_buffer.get_requested_sample_window()
            .min(self.max_sample_window);

        self.working_buffer = Some(working_buffer);
        // self.working_buffer is now the buffer previously in
        // self.exposed_buffer_mtx
    }

    pub fn refresh(&mut self, is_playing: bool) {
        // refresh with current conditions
        let mut working_buffer = self.working_buffer
            .take()
            .expect("DoubleAudioBuffer::refresh: failed to take working buffer");

        {
            let exposed_buffer_box = &mut *self.exposed_buffer_mtx
                .lock()
                .expect("DoubleAudioBuffer:::refresh: failed to lock the exposed buffer");

            if !is_playing {
                exposed_buffer_box.switch_to_paused();
            }

            // get latest state from the previously exposed buffer
            working_buffer.update_concrete_state(exposed_buffer_box.as_mut());

            // refresh working buffer
            working_buffer.refresh(&self.audio_buffer);

            // swap buffers
            mem::swap(exposed_buffer_box, &mut working_buffer);
        }

        self.lower_to_keep = working_buffer.get_lower();

        self.working_buffer = Some(working_buffer);
        // self.working_buffer is now the buffer previously in
        // self.exposed_buffer_mtx
    }

    // Refresh the working buffer with the provided conditions and swap.
    // Conditions concrete type must conform to a struct expected
    // by the concrete implementation of the SampleExtractor.
    pub fn refresh_with_conditions<T: Any + Clone>(
        &mut self,
        conditions: Box<T>,
        is_playing: bool,
    ) {
        let mut working_buffer = self.working_buffer
            .take()
            .expect("DoubleAudioBuffer::refresh: failed to take working buffer");

        {
            let exposed_buffer_box = &mut *self.exposed_buffer_mtx
                .lock()
                .expect("DoubleAudioBuffer:::refresh: failed to lock the exposed buffer");

            if !is_playing {
                exposed_buffer_box.switch_to_paused();
            }

            // get latest state from the previously exposed buffer
            working_buffer.update_concrete_state(exposed_buffer_box.as_mut());

            // refresh working buffer
            working_buffer.refresh_with_conditions(&self.audio_buffer, conditions.clone());

            // swap buffers
            mem::swap(exposed_buffer_box, &mut working_buffer);
        }

        // working buffer is last exposed buffer
        // refresh with new conditions
        working_buffer.refresh_with_conditions(&self.audio_buffer, conditions);

        self.lower_to_keep = working_buffer.get_lower();

        self.working_buffer = Some(working_buffer);
        // self.working_buffer is now the buffer previously in
        // self.exposed_buffer_mtx
    }
}
