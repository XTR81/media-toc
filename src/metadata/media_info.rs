extern crate gstreamer as gst;

use metadata::Chapter;

pub struct MediaInfo {
    pub file_name: String,
    pub tags: gst::TagList,
    pub description: String,
    pub duration: u64,
    pub chapters: Vec<Chapter>,

    pub audio_streams: Vec<(String, gst::Caps, Option<gst::TagList>)>,
    pub audio_selected: Option<(String, String)>, // (stream_id, codec for display)

    pub video_streams: Vec<(String, gst::Caps, Option<gst::TagList>)>,
    pub video_selected: Option<(String, String)>, // (stream_id, codec for display)

    pub text_streams: Vec<(String, gst::Caps, Option<gst::TagList>)>,
    pub text_selected: Option<(String, String)>, // (stream_id, codec for display)
}

impl MediaInfo {
    pub fn new() -> Self {
        MediaInfo {
            file_name: String::new(),
            tags: gst::TagList::new(),
            description: String::new(),
            duration: 0,
            chapters: Vec::new(),

            audio_streams: Vec::new(),
            audio_selected: None,

            video_streams: Vec::new(),
            video_selected: None,

            text_streams: Vec::new(),
            text_selected: None,
        }
    }

    pub fn get_file_name(&self) -> &str {
        &self.file_name
    }

    pub fn get_artist(&self) -> Option<&str> {
        self.tags
            .get_index::<gst::tags::Artist>(0)
            .map(|value| value.get().unwrap())
            .or_else(|| {
                self.tags
                    .get_index::<gst::tags::AlbumArtist>(0)
                    .map(|value| value.get().unwrap())
            })
            .or_else(|| {
                self.tags
                    .get_index::<gst::tags::ArtistSortname>(0)
                    .map(|value| value.get().unwrap())
            })
            .or_else(|| {
                self.tags
                    .get_index::<gst::tags::AlbumArtistSortname>(0)
                    .map(|value| value.get().unwrap())
            })
    }

    pub fn get_title(&self) -> Option<&str> {
        self.tags
            .get_index::<gst::tags::Title>(0)
            .map(|value| value.get().unwrap())
    }

    pub fn get_image(&self, index: u32) -> Option<gst::Sample> {
        self.tags
            .get_index::<gst::tags::Image>(index)
            .map(|value| value.get().unwrap())
    }

    pub fn get_audio_codec(&self) -> Option<&str> {
        self.audio_selected
            .as_ref()
            .map(|&(ref _stream_id, ref codec)| codec.as_str())
    }

    pub fn get_video_codec(&self) -> Option<&str> {
        self.video_selected
            .as_ref()
            .map(|&(ref _stream_id, ref codec)| codec.as_str())
    }

    pub fn get_container(&self) -> Option<&str> {
        // in case of an mp3 audio file, container comes as `ID3 label`
        // => bypass it
        if let Some(audio_codec) = self.get_audio_codec() {
            if self.get_video_codec().is_none() && audio_codec.to_lowercase().find("mp3").is_some()
            {
                return None;
            }
        }

        self.tags
            .get_index::<gst::tags::ContainerFormat>(0)
            .map(|value| value.get().unwrap())
    }
}
