use gettextrs::gettext;
use gstreamer as gst;

use std::io::Write;

use super::{MediaInfo, Timestamp, TocVisitor, Writer};

static EXTENSION: &'static str = "cue";

pub struct CueSheetFormat {}

impl CueSheetFormat {
    pub fn get_extension() -> &'static str {
        EXTENSION
    }

    pub fn new_as_boxed() -> Box<Self> {
        Box::new(CueSheetFormat {})
    }
}

macro_rules! write_fmt(
    ($dest:ident, $fmt:expr, $( $item:expr ),*) => {
        if $dest.write_fmt(format_args!($fmt, $( $item ),*)).is_err() {
            return Err(gettext("Failed to write Cue Sheet file"));
        }
    };
);

impl Writer for CueSheetFormat {
    fn write(&self, info: &MediaInfo, destination: &mut Write) -> Result<(), String> {
        if info.toc.is_none() {
            return Err(gettext("The table of contents is empty"));
        }

        let media_title = info.get_title().map(|title| title.to_owned());
        if let Some(ref title) = media_title {
            write_fmt!(destination, "TITLE \"{}\"\n", title);
        }

        let media_artist = info.get_artist().map(|artist| artist.to_owned());
        if let Some(ref artist) = media_artist {
            write_fmt!(destination, "PERFORMER \"{}\"\n", artist);
        }

        let audio_codec = match info.get_audio_codec() {
            Some(audio_codec) => {
                if audio_codec.to_lowercase().find("mp3").is_some() {
                    "MP3"
                } else if audio_codec.to_lowercase().find("aiff").is_some() {
                    "AIFF"
                } else {
                    "WAVE"
                }
            }
            None => "WAVE",
        };
        write_fmt!(destination, "FILE \"{}\" {}\n", info.get_file_name(), audio_codec);

        let mut index = 0;
        let mut toc_visitor = TocVisitor::new(info.toc.as_ref().unwrap());
        while let Some(chapter) = toc_visitor.next_chapter() {
            index += 1;
            // FIXME: are there other TRACK types than AUDIO?
            write_fmt!(destination, "  TRACK{:02} AUDIO\n", index);

            let title = chapter.get_tags().map_or(None, |tags| {
                tags.get::<gst::tags::Title>().map(|tag| {
                    tag.get().unwrap().to_owned()
                })
            })
                .map_or(media_title.clone(), |track_title| Some(track_title))
                .unwrap_or(super::DEFAULT_TITLE.to_owned());
            write_fmt!(destination, "    TITLE \"{}\"\n", &title);

            let artist = chapter.get_tags().map_or(None, |tags| {
                tags.get::<gst::tags::Artist>().map(|tag| {
                    tag.get().unwrap().to_owned()
                })
            })
                .map_or(media_artist.clone(), |track_artist| Some(track_artist))
                .unwrap_or(super::DEFAULT_TITLE.to_owned());
            write_fmt!(destination, "    PERFORMER \"{}\"\n", &artist);

            if let Some((start, _end)) = chapter.get_start_stop_times() {
                let start_ts = Timestamp::from_nano(start as u64);
                write_fmt!(destination, "    INDEX 01 {:02}:{:02}:{:02}\n",
                    start_ts.h * 60 + start_ts.m,
                    start_ts.s,
                    (((start_ts.ms * 1_000 + start_ts.us) * 1_000 + start_ts.nano) as f64
                        / 1_000_000_000f64 * 75f64)
                        .round() // frame nb (75 frames/s for Cue Sheets)
                );
            }
        }

        Ok(())
    }
}
