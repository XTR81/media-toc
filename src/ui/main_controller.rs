extern crate gtk;
extern crate glib;
extern crate gstreamer as gst;

use std::rc::{Rc, Weak};
use std::cell::RefCell;

use std::path::PathBuf;

use std::sync::mpsc::{channel, Receiver};

use gtk::prelude::*;
use gtk::{ApplicationWindow, Button, FileChooserAction, FileChooserDialog,
          HeaderBar, ResponseType, Label, ToolButton};

use ::media::{Context, ContextMessage, Timestamp};
use ::media::ContextMessage::*;

use super::{AudioController, InfoController, VideoController};

pub struct MainController {
    window: ApplicationWindow,
    header_bar: HeaderBar,
    play_pause_btn: ToolButton,
    position_lbl: Label,
    info_ctrl: InfoController,
    video_ctrl: VideoController,
    audio_ctrl: Rc<RefCell<AudioController>>,

    ctx: Option<Context>,

    self_weak: Option<Weak<RefCell<MainController>>>,
    keep_going: bool,
    listener_src: Option<glib::SourceId>,
    tracker_src: Option<glib::SourceId>,
}

impl MainController {
    pub fn new(builder: gtk::Builder) -> Rc<RefCell<Self>> {
        let this = Rc::new(RefCell::new(MainController {
            window: builder.get_object("application-window").unwrap(),
            header_bar: builder.get_object("header-bar").unwrap(),
            play_pause_btn: builder.get_object("play_pause-toolbutton").unwrap(),
            position_lbl: builder.get_object("position-lbl").unwrap(),
            info_ctrl: InfoController::new(&builder),
            video_ctrl: VideoController::new(&builder),
            audio_ctrl: AudioController::new(&builder),
            ctx: None,
            self_weak: None,
            keep_going: true,
            listener_src: None,
            tracker_src: None,
        }));

        let this_weak = Rc::downgrade(&this);
        {
            let mut this_mut = this.borrow_mut();
            this_mut.window.connect_delete_event(|_, _| {
                gtk::main_quit();
                Inhibit(false)
            });
            this_mut.window.set_titlebar(&this_mut.header_bar);

            let this_weak_clone = this_weak.clone();
            this_mut.play_pause_btn.connect_clicked(move |_| {
                let this = this_weak_clone.upgrade()
                    .expect("Main controller is no longer available for play/pause");
                this.borrow_mut().play_pause();
            });

            let this_weak = Rc::downgrade(&this);
            this_mut.self_weak = Some(this_weak);
        }

        let open_btn: Button = builder.get_object("open-btn").unwrap();
        let this_weak_clone = this_weak.clone();
        open_btn.connect_clicked(move |_| {
            let this = this_weak_clone.upgrade()
                .expect("Main controller is no longer available for select_media");
            this.borrow_mut().select_media();
        });

        this
    }

    pub fn show_all(&self) {
        self.window.show_all();
    }

    pub fn play_pause(&mut self) {
        match self.ctx {
            Some(ref ctx) => {
                match ctx.get_state() {
                    gst::State::Paused => {
                        self.play_pause_btn.set_icon_name("media-playback-pause");
                        ctx.play().unwrap();
                    }
                    gst::State::Playing => {
                        ctx.pause().unwrap();
                        self.play_pause_btn.set_icon_name("media-playback-start");
                    },
                    state => println!("Can't play/pause in state {:?}", state),
                }
            },
            None => (),
        };
    }

    pub fn stop(&mut self) {
        if let Some(context) = self.ctx.as_mut() {
            context.stop();

            // remove callbacks in order to avoid conflict on borrowing of self
            if let Some(source_id) = self.listener_src {
                glib::source_remove(source_id);
            }
            self.listener_src = None;
            if let Some(source_id) = self.tracker_src {
                glib::source_remove(source_id);
            }
            self.tracker_src = None;
        }
        self.play_pause_btn.set_icon_name("media-playback-start");
    }

    fn select_media(&mut self) {
        self.stop();

        let file_dlg = FileChooserDialog::new(
            Some("Open a media file"),
            Some(&self.window),
            FileChooserAction::Open,
        );
        // Note: couldn't find equivalents for STOCK_OK
        file_dlg.add_button("Open", ResponseType::Ok.into());

        let result = file_dlg.run();

        if result == ResponseType::Ok.into() {
            self.open_media(file_dlg.get_filename().unwrap());
        }
        else { () }

        file_dlg.close();
    }

    fn register_listener(&mut self,
        timeout: u32,
        ui_rx: Receiver<ContextMessage>,
    )
    {
        let this_weak = self.self_weak.as_ref()
            .expect("Failed to get ref on MainController's weak Rc for register_listener")
            .clone();

        self.listener_src = Some(gtk::timeout_add(timeout, move || {
            let mut message_iter = ui_rx.try_iter();

            let this = this_weak.upgrade()
                .expect("Main controller is no longer available for ctx channel listener");
            let mut this_mut = this.borrow_mut();

            for message in message_iter.next() {
                match message {
                    AsyncDone => {
                        println!("Received AsyncDone");
                    },
                    InitDone => {
                        println!("Received InitDone");

                        let context = this_mut.ctx.take()
                            .expect("Received InitDone, but context is not available");

                        this_mut.info_ctrl.new_media(&context);
                        this_mut.video_ctrl.new_media(&context);
                        this_mut.audio_ctrl.borrow_mut().new_media(&context);
                        {
                            this_mut.header_bar.set_subtitle(Some(context.file_name.as_str()));
                        }

                        this_mut.ctx = Some(context);
                    },
                    Eos => {
                        println!("Received Eos");
                        this_mut.play_pause_btn.set_icon_name("media-playback-start");
                    },
                    FailedToOpenMedia => {
                        eprintln!("ERROR: failed to open media");
                        this_mut.ctx = None;
                        this_mut.keep_going = false;
                    },
                };

                if !this_mut.keep_going { break; }
            }

            if !this_mut.keep_going {
                this_mut.listener_src = None;
                println!("Exiting listener");
            }

            glib::Continue(this_mut.keep_going)
        }));
    }

    fn register_tracker(&mut self, timeout: u32) {
        let this_weak = self.self_weak.as_ref()
            .expect("Failed to get ref on MainController's weak Rc for register_tracker")
            .clone();

        self.tracker_src = Some(gtk::timeout_add(timeout, move || {
            let this = this_weak.upgrade()
                .expect("Main controller is no longer available for tracker");
            let mut this_mut = this.borrow_mut();
            if !this_mut.keep_going {
            }

            let position = match this_mut.ctx {
                Some(ref ctx) => if ctx.get_state() == gst::State::Playing {
                    let position = Timestamp::from_nano(ctx.get_position());
                    this_mut.position_lbl.set_text(&format!("{}", position));
                    position
                }
                else {
                    Timestamp::new()
                },
                None => Timestamp::new(),
            };

            if position.nano > 0 {
                this_mut.audio_ctrl.borrow_mut().have_position(position.nano);
            }

            if !this_mut.keep_going {
                this_mut.tracker_src = None;
                println!("Exiting tracker");
            }

            glib::Continue(this_mut.keep_going)
        }));
    }

    fn open_media(&mut self, filepath: PathBuf) {
        assert_eq!(self.listener_src, None);

        self.audio_ctrl.borrow_mut().cleanup();
        self.video_ctrl.cleanup();

        self.position_lbl.set_text("00:00.000");

        let (ctx_tx, ui_rx) = channel();

        self.keep_going = true;
        self.register_listener(500, ui_rx);
        self.register_tracker(33); // 30 Hz

        match Context::open_media_path(
                filepath,
                10_000_000_000,
                self.video_ctrl.video_box.clone(),
                ctx_tx
            )
            {
            Ok(ctx) => self.ctx = Some(ctx),
            Err(error) => eprintln!("Error opening media: {}", error),
        };
    }
}
