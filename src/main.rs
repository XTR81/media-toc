#![feature(ord_max_min)]

extern crate cairo;
extern crate glib;
extern crate gstreamer;
extern crate gstreamer_audio;
extern crate gstreamer_app as gst_app;
extern crate gtk;

extern crate byte_slice_cast;
extern crate chrono;
extern crate image;
extern crate url;

use gtk::Builder;

mod ui;
use ui::MainController;

mod media;

fn main() {
    if gtk::init().is_err() {
        panic!("Failed to initialize GTK.");
    }

    gstreamer::init().unwrap();

    // TODO: there's a `Settings` struct in GTK:
    // https://github.com/gtk-rs/gtk/blob/master/src/auto/settings.rs

    let builder = Builder::new_from_string(include_str!("ui/media-toc.ui"));
    let main_ctrl = MainController::new(builder);
    main_ctrl.borrow().show_all();

    gtk::main();
}
