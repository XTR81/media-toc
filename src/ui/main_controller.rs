extern crate glib;
extern crate gstreamer as gst;
extern crate gtk;

#[cfg(any(feature = "profiling-listener", feature = "profiling-tracker"))]
use chrono::Utc;

use std::rc::Rc;
use std::cell::RefCell;

use std::path::PathBuf;

use std::sync::mpsc::{channel, Receiver};

use gtk::prelude::*;

use media::{Context, ContextMessage};
use media::ContextMessage::*;

use super::{AudioController, ExportController, InfoController, VideoController};

#[derive(Clone, PartialEq)]
pub enum ControllerState {
    EOS,
    Ready,
    Paused,
    Playing,
    Stopped,
}

const LISTENER_PERIOD: u32 = 250; // 250 ms (4 Hz)
const TRACKER_PERIOD: u32 = 17; //  17 ms (60 Hz)

pub struct MainController {
    window: gtk::ApplicationWindow,
    header_bar: gtk::HeaderBar,
    play_pause_btn: gtk::ToolButton,
    build_toc_btn: gtk::Button,

    video_ctrl: VideoController,
    info_ctrl: Rc<RefCell<InfoController>>,
    audio_ctrl: Rc<RefCell<AudioController>>,
    export_ctrl: Rc<RefCell<ExportController>>,

    context: Option<Context>,
    state: ControllerState,
    duration: Option<u64>, // duration is accurately known at eos
    seeking: bool,

    this_opt: Option<Rc<RefCell<MainController>>>,
    keep_going: bool,
    listener_src: Option<glib::SourceId>,
    tracker_src: Option<glib::SourceId>,
}

impl MainController {
    pub fn new(builder: gtk::Builder) -> Rc<RefCell<Self>> {
        let build_toc_btn: gtk::Button = builder.get_object("build_toc-btn").unwrap();
        build_toc_btn.set_sensitive(false);

        let this = Rc::new(RefCell::new(MainController {
            window: builder.get_object("application-window").unwrap(),
            header_bar: builder.get_object("header-bar").unwrap(),
            play_pause_btn: builder.get_object("play_pause-toolbutton").unwrap(),
            build_toc_btn: build_toc_btn,

            video_ctrl: VideoController::new(&builder),
            info_ctrl: InfoController::new(&builder),
            audio_ctrl: AudioController::new(&builder),
            export_ctrl: ExportController::new(&builder),

            context: None,
            state: ControllerState::Stopped,
            duration: None,
            seeking: false,

            this_opt: None,
            keep_going: true,
            listener_src: None,
            tracker_src: None,
        }));

        {
            let mut this_mut = this.borrow_mut();

            let this_rc = Rc::clone(&this);
            this_mut.this_opt = Some(this_rc);

            this_mut.window.connect_delete_event(|_, _| {
                gtk::main_quit();
                Inhibit(false)
            });
            this_mut.window.set_titlebar(&this_mut.header_bar);

            let this_rc = Rc::clone(&this);
            this_mut.play_pause_btn.connect_clicked(move |_| {
                this_rc.borrow_mut().play_pause();
            });

            // TODO: add key bindings to seek by steps
            // play/pause, etc.

            let this_rc = Rc::clone(&this);
            this_mut.build_toc_btn.connect_clicked(move |_| {
                this_rc.borrow_mut().build_toc();
            });

            this_mut.video_ctrl.register_callbacks(&this);
            InfoController::register_callbacks(&this_mut.info_ctrl, &this);
            AudioController::register_callbacks(&this_mut.audio_ctrl, &this);
            ExportController::register_callbacks(&this_mut.export_ctrl, &this);
        }

        let open_btn: gtk::Button = builder.get_object("open-btn").unwrap();
        let this_rc = Rc::clone(&this);
        open_btn.connect_clicked(move |_| { this_rc.borrow_mut().select_media(); });

        this
    }

    pub fn show_all(&self) {
        self.window.show_all();
    }

    pub fn get_state(&self) -> &ControllerState {
        &self.state
    }

    fn play_pause(&mut self) {
        let context = match self.context.take() {
            Some(context) => context,
            None => {
                self.select_media();
                return;
            }
        };

        if self.state != ControllerState::EOS {
            match context.get_state() {
                gst::State::Paused => {
                    self.register_tracker();
                    self.play_pause_btn.set_icon_name("media-playback-pause");
                    self.state = ControllerState::Playing;
                    context.play().unwrap();
                    self.context = Some(context);
                }
                gst::State::Playing => {
                    context.pause().unwrap();
                    self.play_pause_btn.set_icon_name("media-playback-start");
                    self.remove_tracker();
                    self.state = ControllerState::Paused;
                    self.context = Some(context);
                }
                _ => {
                    self.context = Some(context);
                    self.select_media();
                }
            };
        } else {
            // Restart the stream from the begining
            self.context = Some(context);
            self.seek(0, true); // accurate (slow)
        }
    }

    fn stop(&mut self) {
        if let Some(context) = self.context.as_mut() {
            context.stop();
        };

        // remove callbacks in order to avoid conflict on borrowing of self
        self.remove_listener();
        self.remove_tracker();

        self.audio_ctrl.borrow_mut().cleanup();

        self.play_pause_btn.set_icon_name("media-playback-start");

        self.state = ControllerState::Stopped;
        self.duration = None;
    }

    pub fn seek(&mut self, position: u64, accurate: bool) {
        if self.state != ControllerState::Stopped {
            self.seeking = true;

            self.context
                .as_ref()
                .expect("MainController::seek no context")
                .seek(position, accurate);

            if self.state == ControllerState::EOS || self.state == ControllerState::Ready {
                if self.state == ControllerState::Ready {
                    self.context
                        .as_ref()
                        .expect("MainController::seek no context")
                        .play()
                        .unwrap();
                }
                self.register_tracker();
                self.play_pause_btn.set_icon_name("media-playback-pause");
                self.state = ControllerState::Playing;
            }

            self.info_ctrl.borrow_mut().seek(position);
            self.audio_ctrl.borrow_mut().seek(position, &self.state);
        }
    }

    pub fn get_position(&mut self) -> u64 {
        self.context
            .as_mut()
            .expect("MainController::get_position no context")
            .get_position()
    }

    fn select_media(&mut self) {
        self.stop();

        // can't cleanup controllers here because ticks might be still pending
        // => need to wait for the main loop to terminate

        self.build_toc_btn.set_sensitive(false);

        let file_dlg = gtk::FileChooserDialog::new(
            Some("Open a media file"),
            Some(&self.window),
            gtk::FileChooserAction::Open,
        );
        // Note: couldn't find equivalents for STOCK_OK
        file_dlg.add_button("Open", gtk::ResponseType::Ok.into());

        if file_dlg.run() == gtk::ResponseType::Ok.into() {
            self.open_media(file_dlg.get_filename().unwrap());
        } else {
            self.info_ctrl.borrow_mut().cleanup();
            self.audio_ctrl.borrow_mut().cleanup();
            self.header_bar.set_subtitle("");
        }

        file_dlg.close();
    }

    fn build_toc(&mut self) {
        match self.context.take() {
            Some(mut context) => {
                self.stop();
                self.info_ctrl.borrow().export_chapters(&mut context);
                self.export_ctrl.borrow_mut().open(context);
            }
            None => (),
        }
    }

    fn handle_eos(&mut self) {
        #[cfg(feature = "trace-main-controller")]
        println!("MainController::handle_eos");

        self.play_pause_btn.set_icon_name("media-playback-start");
        self.state = ControllerState::EOS;
    }

    fn remove_listener(&mut self) {
        if let Some(source_id) = self.listener_src.take() {
            glib::source_remove(source_id);
        }
    }

    fn register_listener(&mut self, timeout: u32, ui_rx: Receiver<ContextMessage>) {
        if self.listener_src.is_some() {
            return;
        }

        let this_rc = Rc::clone(self.this_opt.as_ref().unwrap());

        self.listener_src = Some(gtk::timeout_add(timeout, move || {
            let mut keep_going = true;

            for message in ui_rx.try_iter() {
                match message {
                    AsyncDone => {
                        this_rc.borrow_mut().seeking = false;
                    }
                    InitDone => {
                        let mut this_mut = this_rc.borrow_mut();

                        let context = this_mut.context.take().expect(
                            "MainController: InitDone but no context available",
                        );

                        this_mut.header_bar.set_subtitle(
                            Some(context.file_name.as_str()),
                        );

                        this_mut.video_ctrl.new_media(&context);
                        this_mut.info_ctrl.borrow_mut().new_media(&context);
                        this_mut.audio_ctrl.borrow_mut().new_media(&context);

                        this_mut.context = Some(context);

                        this_mut.state = ControllerState::Ready;
                    }
                    Eos => {
                        let mut this_mut = this_rc.borrow_mut();

                        let position = this_mut.get_position();
                        this_mut.duration = Some(position);

                        {
                            let mut info_ctrl = this_mut.info_ctrl.borrow_mut();
                            info_ctrl.update_duration(position);
                            info_ctrl.tick(position, true);
                        }

                        this_mut.audio_ctrl.borrow_mut().tick();

                        this_mut.handle_eos();

                        // Remove listener and tracker.
                        // Note: tracker will be register again in case of
                        // a seek. Listener is of no use anymore because
                        // the context won't send any more Eos nor AsyncDone
                        // after an EOS.
                        keep_going = false;
                    }
                    FailedToOpenMedia => {
                        eprintln!("ERROR: failed to open media");

                        let mut this_mut = this_rc.borrow_mut();

                        this_mut.context = None;
                        this_mut.keep_going = false;
                        keep_going = false;
                    }
                };

                if !keep_going {
                    break;
                }
            }

            if !keep_going {
                let mut this_mut = this_rc.borrow_mut();
                this_mut.listener_src = None;
                this_mut.tracker_src = None;
            }

            glib::Continue(keep_going)
        }));
    }

    fn remove_tracker(&mut self) {
        if let Some(source_id) = self.tracker_src.take() {
            glib::source_remove(source_id);
        }
    }

    fn register_tracker(&mut self) {
        if self.tracker_src.is_some() {
            return;
        }

        let this_rc = Rc::clone(self.this_opt.as_ref().unwrap());

        self.tracker_src = Some(gtk::timeout_add(TRACKER_PERIOD, move || {
            #[cfg(feature = "profiling-tracker")]
            let start = Utc::now();

            let mut keep_going = true;

            let mut this_mut = this_rc.borrow_mut();

            #[cfg(feature = "profiling-tracker")]
            let before_tick = Utc::now();

            let position = this_mut
                .context
                .as_mut()
                .expect("MainController::tracker no context while getting position")
                .get_position();

            let is_eos = if let Some(duration) = this_mut.duration {
                if position >= duration {
                    if !this_mut.seeking {
                        // this check is necessary as EOS is not sent
                        // in case of a seek after EOS
                        this_mut.handle_eos();
                        this_mut.tracker_src = None;
                        keep_going = false;
                    }
                    true
                } else if this_mut.seeking {
                    // this check is necessary as AsyncDone is not sent
                    // in case of a seek after EOS
                    this_mut.seeking = false;
                    false
                } else {
                    false
                }
            } else {
                false
            };

            if !this_mut.seeking {
                this_mut.info_ctrl.borrow_mut().tick(position, is_eos);
                this_mut.audio_ctrl.borrow_mut().tick();
            }

            #[cfg(feature = "profiling-tracker")]
            let end = Utc::now();

            #[cfg(feature = "profiling-tracker")]
            println!(
                "tracker,{},{},{}",
                start.time().format("%H:%M:%S%.6f"),
                before_tick.time().format("%H:%M:%S%.6f"),
                end.time().format("%H:%M:%S%.6f"),
            );

            glib::Continue(this_mut.keep_going && keep_going)
        }));
    }

    fn open_media(&mut self, filepath: PathBuf) {
        assert_eq!(self.listener_src, None);

        self.info_ctrl.borrow_mut().cleanup();
        self.audio_ctrl.borrow_mut().cleanup();
        self.header_bar.set_subtitle("");

        let (ctx_tx, ui_rx) = channel();

        self.seeking = false;
        self.keep_going = true;
        self.register_listener(LISTENER_PERIOD, ui_rx);

        match Context::new(
            filepath,
            self.audio_ctrl.borrow().get_dbl_buffer_mtx(),
            ctx_tx,
        ) {
            Ok(context) => {
                self.context = Some(context);
                self.build_toc_btn.set_sensitive(true);
            }
            Err(error) => eprintln!("Error opening media: {}", error),
        };
    }
}
