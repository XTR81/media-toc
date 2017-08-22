extern crate gstreamer as gst;
use gstreamer::{BinExt, BinExtManual, Caps, ElementExt, ElementFactory, GstObjectExt,
                PadExt, TocScope, TocEntryType};

extern crate gstreamer_audio as gst_audio;
extern crate gstreamer_app as gst_app;

extern crate glib;
use glib::{Cast, ObjectExt, ToValue};

extern crate gtk;
use gtk::{BoxExt, ContainerExt};

extern crate url;
use url::Url;

use std::path::PathBuf;

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use std::i32;

use super::{AlignedImage, AudioBuffer, WaveformBuffer, Chapter, MediaInfo, Timestamp};

macro_rules! build_audio_pipeline(
    (
        $pipeline:expr,
        $src_pad:expr,
        $audio_sink:expr,
        $buffering_duration:expr,
        $waveform_buffer_mtx:expr
    ) =>
    {
        let playback_queue = gst::ElementFactory::make("queue", "playback_queue").unwrap();
        playback_queue.set_property("max-size-time", &gst::Value::from(&$buffering_duration)).unwrap();

        let playback_convert = gst::ElementFactory::make("audioconvert", None).unwrap();
        let playback_resample = gst::ElementFactory::make("audioresample", None).unwrap();
        let playback_sink_pad = playback_queue.get_static_pad("sink").unwrap();
        let playback_elements = &[
            &playback_queue, &playback_convert, &playback_resample, &$audio_sink,
        ];

        let visu_queue = gst::ElementFactory::make("queue", "visu_queue").unwrap();
        visu_queue.set_property("max-size-time", &gst::Value::from(&$buffering_duration)).unwrap();

        let visu_convert = gst::ElementFactory::make("audioconvert", None).unwrap();
        let visu_sink = gst::ElementFactory::make("appsink", "audio_visu_sink").unwrap();
        let visu_sink_pad = visu_queue.get_static_pad("sink").unwrap();

        {
            let visu_elements = &[&visu_queue, &visu_convert, &visu_sink];
            let tee = gst::ElementFactory::make("tee", "audio_tee").unwrap();
            let mut elements = vec!(&tee);
            elements.extend_from_slice(playback_elements);
            elements.extend_from_slice(visu_elements);
            $pipeline.add_many(elements.as_slice()).unwrap();

            gst::Element::link_many(playback_elements).unwrap();
            gst::Element::link_many(visu_elements).unwrap();

            let tee_sink = tee.get_static_pad("sink").unwrap();
            assert_eq!($src_pad.link(&tee_sink), gst::PadLinkReturn::Ok);

            // TODO: requested pads must be released when done
            let tee_playback_src_pad = tee.get_request_pad("src_%u").unwrap();
            assert_eq!(tee_playback_src_pad.link(&playback_sink_pad), gst::PadLinkReturn::Ok);

            let tee_visu_src_pad = tee.get_request_pad("src_%u").unwrap();
            assert_eq!(tee_visu_src_pad.link(&visu_sink_pad), gst::PadLinkReturn::Ok);

            for e in elements { e.sync_state_with_parent().unwrap(); }
        }

        let appsink = visu_sink.dynamic_cast::<gst_app::AppSink>()
            .expect("Sink element is expected to be an appsink!");
        appsink.set_caps(&Caps::new_simple(
            "audio/x-raw",
            &[
                ("format", &gst_audio::AUDIO_FORMAT_S16.to_string()),
                ("layout", &"interleaved"),
                ("channels", &gst::IntRange::<i32>::new(1, i32::MAX)),
                ("rate", &gst::IntRange::<i32>::new(1, i32::MAX)),
            ],
        ));

        // TODO: make this configurable
        appsink.set_property("ts-offset", &gst::Value::from(&-2_000_000_000i64)).unwrap();
        //appsink.set_property("async", &gst::Value::from(&false)).unwrap();
        //appsink.set_property("sync", &gst::Value::from(&false)).unwrap();

        // TODO: caps can change so it might be necessary to
        let audio_buffer = Arc::new(Mutex::new(AudioBuffer::new(
            &$src_pad.get_current_caps().unwrap(),
            2_000_000_000, // appsink offset (see above)
            $buffering_duration,
            $waveform_buffer_mtx.clone(),
        )));
        appsink.set_callbacks(gst_app::AppSinkCallbacks::new(
            /* eos: handled by pipeline */
            |_| {},
            /* new_preroll */
            |_| { gst::FlowReturn::Ok },
            /* new_samples */
            move |appsink| {
                let sample = match appsink.pull_sample() {
                    None => return gst::FlowReturn::Eos,
                    Some(sample) => sample,
                };

                audio_buffer.lock().unwrap().push_gst_sample(sample);

                gst::FlowReturn::Ok
            },
        ));
    };
);

macro_rules! build_video_pipeline(
    ($pipeline:expr, $src_pad:expr, $video_sink:expr) => {
        let queue = gst::ElementFactory::make("queue", None).unwrap();
        let convert = gst::ElementFactory::make("videoconvert", None).unwrap();
        let scale = gst::ElementFactory::make("videoscale", None).unwrap();

        let elements = &[&queue, &convert, &scale, &$video_sink];
        $pipeline.add_many(elements).unwrap();
        gst::Element::link_many(elements).unwrap();

        for e in elements { e.sync_state_with_parent().unwrap(); }

        let sink_pad = queue.get_static_pad("sink").unwrap();
        assert_eq!($src_pad.link(&sink_pad), gst::PadLinkReturn::Ok);
    };
);

macro_rules! assign_str_tag(
    ($target:expr, $tags:expr, $TagType:ty) => {
        if $target.is_empty() {
            if let Some(tag) = $tags.get::<$TagType>() {
                $target = tag.get().unwrap().to_owned();
            }
        }
    };
);

pub enum ContextMessage {
    AsyncDone,
    Eos,
    FailedToOpenMedia,
    InitDone,
}

pub struct Context {
    pipeline: gst::Pipeline,
    audio_sink: gst::Element,
    video_sink: gst::Element,

    pub path: PathBuf,
    pub file_name: String,
    pub name: String,

    pub info: Arc<Mutex<MediaInfo>>,
    pub waveform_buffer_mtx: Arc<Mutex<Option<WaveformBuffer>>>,
}

// FIXME: need to `release_request_pad` on the tee
// maybe this should be done in a `drop`. At least, it
// should be done before the pipeline is reconstructed
impl Context {
    fn new(path: PathBuf, video_widget_box: gtk::Box) -> Self {
        let pipeline = gst::Pipeline::new("pipeline");

        let audio_sink = gst::ElementFactory::make("autoaudiosink", "audio_playback_sink").unwrap();

        let (video_sink, widget_val) = if let Some(gtkglsink) = ElementFactory::make("gtkglsink", None) {
            let glsinkbin = ElementFactory::make("glsinkbin", "video_sink").unwrap();
            glsinkbin.set_property("sink", &gtkglsink.to_value()).unwrap();
            let widget_val = gtkglsink.get_property("widget").unwrap();
            (glsinkbin, widget_val)
        } else {
            let sink = ElementFactory::make("gtksink", "video_sink").unwrap();
            let widget_val = sink.get_property("widget").unwrap();
            (sink, widget_val)
        };

        // cleanups the box
        for child in video_widget_box.get_children() {
            video_widget_box.remove(&child);
        }
        // Embed the video widget in the UI container
        let widget = widget_val.get::<gtk::Widget>()
            .expect("Failed to get GstGtkWidget glib::Value as gtk::Widget");
        video_widget_box.pack_start(&widget, true, true, 0);

        Context {
            pipeline: pipeline,
            audio_sink: audio_sink,
            video_sink: video_sink,

            file_name: String::from(path.file_name().unwrap().to_str().unwrap()),
            name: String::from(path.file_stem().unwrap().to_str().unwrap()),
            path: path,

            info: Arc::new(Mutex::new(MediaInfo::new())),
            waveform_buffer_mtx: Arc::new(Mutex::new(Some(WaveformBuffer::new()))),
        }
    }

    pub fn open_media_path(
        path: PathBuf,
        buffering_duration: u64,
        video_widget_box: gtk::Box,
        ctx_tx: Sender<ContextMessage>,
    ) -> Result<Context, String>
    {
        println!("\n\n* Attempting to open {:?}", path);

        let ctx = Context::new(path, video_widget_box);
        ctx.build_pipeline(buffering_duration);
        ctx.register_bus_inspector(ctx_tx);

        match ctx.pause() {
            Ok(_) => Ok(ctx),
            Err(error) => Err(error),
        }
    }

    pub fn get_position(&self) -> i64 {
        match self.pipeline.query_position(gst::Format::Time) {
            Some(duration) => duration,
            None => 0,
        }
    }

    pub fn get_duration(&self) -> i64 {
        match self.pipeline.query_duration(gst::Format::Time) {
            Some(duration) => duration,
            None => 0,
        }
    }

    pub fn get_state(&self) -> gst::State {
        let (_, current, _) = self.pipeline.get_state(10_000_000);
        current
    }

    pub fn play(&self) -> Result<(), String> {
        if self.pipeline.set_state(gst::State::Playing) == gst::StateChangeReturn::Failure {
            return Err("Could not set media in palying state".into());
        }
        Ok(())
    }

    pub fn pause(&self) -> Result<(), String> {
        if self.pipeline.set_state(gst::State::Paused) == gst::StateChangeReturn::Failure {
            return Err("Could not set media in Paused state".into());
        }
        Ok(())
    }

    pub fn stop(&self) {
        if self.pipeline.set_state(gst::State::Null) == gst::StateChangeReturn::Failure {
            println!("Could not set media in Null state");
            //return Err("could not set media in Null state".into());
        }
    }

    // TODO: handle errors
    fn build_pipeline(&self, buffering_duration: u64) {
        let src = gst::ElementFactory::make("uridecodebin", "input").unwrap();
        let url = match Url::from_file_path(self.path.as_path()) {
            Ok(url) => url.into_string(),
            Err(_) => "Failed to convert path to URL".to_owned(),
        };
        src.set_property("uri", &gst::Value::from(&url)).unwrap();
        src.set_property("buffer-duration", &gst::Value::from(&(buffering_duration as i64))).unwrap();
        self.pipeline.add(&src).unwrap();

        let pipeline_clone = self.pipeline.clone();
        let waveform_buffer_mtx = self.waveform_buffer_mtx.clone();
        let audio_sink = self.audio_sink.clone();
        let video_sink = self.video_sink.clone();
        let info_arc_mtx = self.info.clone();
        src.connect_pad_added(move |_, src_pad| {
            let pipeline = &pipeline_clone;

            let caps = src_pad.get_current_caps().unwrap();
            let structure = caps.get_structure(0).unwrap();
            let name = structure.get_name();

            // TODO: build only one queue by stream type (audio / video)
            if name.starts_with("audio/") {
                let is_first = {
                    let info = &mut info_arc_mtx.lock()
                        .expect("Failed to lock media info while initializing audio stream");
                    info.audio_streams.insert(name.to_owned(), caps.clone());
                    let is_first = info.audio_best.is_none();
                    info.audio_best.get_or_insert(name.to_owned());

                    is_first
                };

                if is_first {
                    build_audio_pipeline!(
                        pipeline, src_pad, audio_sink, buffering_duration, waveform_buffer_mtx
                    );
                }
            } else if name.starts_with("video/") {
                let is_first = {
                    let info = &mut info_arc_mtx.lock()
                        .expect("Failed to lock media info while initializing audio stream");
                    info.video_streams.insert(name.to_owned(), caps.clone());
                    let is_first = info.video_best.is_none();
                    info.video_best.get_or_insert(name.to_owned());

                    is_first
                };

                if is_first {
                    build_video_pipeline!(pipeline, src_pad, video_sink);
                }
            }
        });
    }

    // Uses ctx_tx to notify the UI controllers about the inspection process
    fn register_bus_inspector(&self, ctx_tx: Sender<ContextMessage>) {
        let info_arc_mtx = self.info.clone();
        let mut init_done = false;
        let bus = self.pipeline.get_bus().unwrap();
        bus.add_watch(move |_, msg| {
            // TODO: exit when pipeline status is null
            // or can we reuse the inspector for subsequent plays?
            match msg.view() {
                gst::MessageView::Eos(..) => {
                    ctx_tx.send(ContextMessage::Eos)
                        .expect("Failed to notify UI");
                    glib::Continue(false)
                },
                gst::MessageView::Error(err) => {
                    eprintln!("Error from {}: {} ({:?})",
                        msg.get_src().get_path_string(),
                        err.get_error(), err.get_debug()
                    );
                    ctx_tx.send(ContextMessage::FailedToOpenMedia)
                        .expect("Failed to notify UI");
                    glib::Continue(false)
                },
                gst::MessageView::AsyncDone(_) => {
                    if !init_done {
                        init_done = true;
                        ctx_tx.send(ContextMessage::InitDone)
                            .expect("Failed to notify UI");
                    }
                    else {
                        ctx_tx.send(ContextMessage::AsyncDone)
                            .expect("Failed to notify UI");
                    }
                    glib::Continue(true)
                },
                gst::MessageView::Tag(msg_tag) => {
                    if !init_done {
                        let tags = msg_tag.get_tags();
                        let info = &mut info_arc_mtx.lock()
                            .expect("Failed to lock media info while reading tag data");
                        assign_str_tag!(info.title, tags, gst::tags::Title);
                        assign_str_tag!(info.artist, tags, gst::tags::Artist);
                        assign_str_tag!(info.artist, tags, gst::tags::AlbumArtist);
                        assign_str_tag!(info.container, tags, gst::tags::ContainerFormat);
                        assign_str_tag!(info.video_codec, tags, gst::tags::VideoCodec);
                        assign_str_tag!(info.audio_codec, tags, gst::tags::AudioCodec);

                        match tags.get::<gst::tags::PreviewImage>() {
                            // TODO: check if that happens, that would be handy for videos
                            Some(_) => println!("** Found a PreviewImage tag **"),
                            None => (),
                        };

                        // TODO: distinguish front/back cover (take the first one?)
                        if let Some(image_tag) = tags.get::<gst::tags::Image>() {
                            if let Some(sample) = image_tag.get() {
                                if let Some(buffer) = sample.get_buffer() {
                                    if let Some(map) = buffer.map_readable() {
                                        info.thumbnail = AlignedImage::from_uknown_buffer(
                                                map.as_slice()
                                            ).ok();
                                    }
                                }
                            }
                        }
                    }
                    glib::Continue(true)
                },
                gst::MessageView::Toc(msg_toc) => {
                    if init_done {
                        return glib::Continue(true);
                    }
                    let (toc, _) = msg_toc.get_toc();
                    if toc.get_scope() != TocScope::Global {
                        println!("Warning: Skipping toc with scope: {:?}", toc.get_scope());
                        return glib::Continue(true);
                    }

                    let info = &mut info_arc_mtx.lock()
                        .expect("Failed to lock media info while reading toc data");
                    if !info.chapters.is_empty() {
                        // chapters already retrieved
                        // TODO: check if there are medias with some sort of
                        // incremental tocs (not likely for files)
                        // or maybe the updated flag (_ above) should be used
                        return glib::Continue(true);
                    }

                    for entry in toc.get_entries() {
                        if entry.get_entry_type() == TocEntryType::Edition {
                            for sub_entry in entry.get_sub_entries() {
                                if sub_entry.get_entry_type() == TocEntryType::Chapter {
                                    if let Some((start, stop)) = sub_entry.get_start_stop_times() {
                                        let mut title = String::new();
                                        if let Some(tags) = sub_entry.get_tags() {
                                            if let Some(tag) = tags.get::<gst::tags::Title>() {
                                                title = tag.get().unwrap().to_owned();
                                            };
                                        };
                                        info.chapters.push(Chapter::new(
                                            sub_entry.get_uid(),
                                            &title,
                                            Timestamp::from_nano(start),
                                            Timestamp::from_nano(stop)
                                        ));
                                    }
                                }
                                else {
                                    println!("Warning: Skipping toc sub entry with entry type: {:?}",
                                        sub_entry.get_entry_type()
                                    );
                                }
                            }
                        }
                        else {
                            println!("Warning: Skipping toc entry with entry type: {:?}",
                                entry.get_entry_type()
                            );
                        }
                    }

                    glib::Continue(true)
                }
                _ => glib::Continue(true),
            }
        });
    }
}
