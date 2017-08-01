extern crate gtk;
use gtk::prelude::*;

extern crate cairo;

extern crate gstreamer as gst;
use gstreamer::*;

use std::ops::{Deref, DerefMut};

use ::media::Context;

use super::{MediaController, MediaHandler};

pub struct AudioController {
    media_ctl: MediaController,
}

impl AudioController {
    pub fn new(builder: &gtk::Builder) -> Self {
        AudioController {
            media_ctl: MediaController::new(
                builder.get_object("audio-container").unwrap(),
                builder.get_object("audio-drawingarea").unwrap()
            ),
        }
    }
}

impl Deref for AudioController {
	type Target = MediaController;

	fn deref(&self) -> &Self::Target {
		&self.media_ctl
	}
}

impl DerefMut for AudioController {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.media_ctl
	}
}

impl MediaHandler for AudioController {
    fn new_media(&mut self, context: &Context) {
        self.media_ctl.hide();
        // TODO: set an Option to indicate that no stream is initialized
    }
}
