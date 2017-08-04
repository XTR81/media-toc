extern crate gtk;

use gtk::WidgetExt;

use ::media::{Context, MediaInfo};

pub trait MediaHandler {
    fn new_media(&mut self, context: &Context, info: &MediaInfo);
}

pub struct MediaController {
    pub container: gtk::Container,
}

impl MediaController {
    pub fn new(container: gtk::Container) -> Self {
        MediaController{
            container: container,
        }
    }

    pub fn show(&self) {
        self.container.show();
    }

    pub fn hide(&self) {
        self.container.hide();
    }
}
