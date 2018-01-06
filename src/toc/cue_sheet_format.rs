use std::io::Write;

// FIXME: rename toc to metadata and move media_info to metadata
use media::MediaInfo;

use super::{Chapter, Writer};

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

impl Writer for CueSheetFormat {
    fn write(
        &self,
        info: &MediaInfo,
        chapters: &[Chapter],
        destination: &mut Write,
    ) {
        let title = info.get_title();
        if let Some(title) = title {
            destination
                .write_fmt(format_args!("TITLE \"{}\"\n", title))
                .expect("CueSheetFormat::write clicked, failed to write to file");
        }

        let artist = info.get_artist();
        if let Some(artist) = artist {
            destination
                .write_fmt(format_args!("PERFORMER \"{}\"\n", artist))
                .expect("CueSheetFormat::write clicked, failed to write to file");
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
        destination
            .write_fmt(format_args!("FILE \"{}\" {}\n", info.get_file_name(), audio_codec))
            .expect("CueSheetFormat::write clicked, failed to write to file");

        for (index, chapter) in chapters.iter().enumerate() {
            // FIXME: are there other TRACK types than AUDIO?
            destination
                .write_fmt(format_args!("  TRACK{:02} AUDIO\n", index + 1))
                .expect("CueSheetFormat::write clicked, failed to write to file");

            destination
                .write_fmt(format_args!(
                    "    TITLE \"{}\"\n",
                    chapter.get_title().unwrap_or(super::DEFAULT_TITLE)
                ))
                .expect("CueSheetFormat::write clicked, failed to write to file");

            if let Some(artist) = artist {
                destination
                    .write_fmt(format_args!("    PERFORMER \"{}\"\n", artist))
                    .expect("CueSheetFormat::write clicked, failed to write to file");
            }

            let start_ts = chapter.start;
            destination
                .write_fmt(format_args!(
                    "    INDEX 01 {:02}:{:02}:{:02}\n",
                    start_ts.h * 60 + start_ts.m,
                    start_ts.s,
                    (((start_ts.ms * 1_000 + start_ts.us) * 1_000 + start_ts.nano) as f64 /
                         1_000_000_000f64 * 75f64)
                        .round() // frame nb (75 frames/s for Cue Sheets)
                ))
                .expect("CueSheetFormat::write clicked, failed to write to file");
        }
    }
}
