extern crate gtk;
extern crate cairo;

extern crate ffmpeg;

use std::ops::{Deref, DerefMut};

use std::rc::Rc;
use std::cell::RefCell;

use gtk::prelude::*;
use cairo::enums::{FontSlant, FontWeight};

use ::media::Context;
use ::media::PacketNotifiable;

use super::MediaNotifiable;
use super::MediaController;

pub struct AudioController {
    media_ctl: MediaController,
    drawingarea: gtk::DrawingArea,
    message: String,
    frame: Option<ffmpeg::frame::Audio>,
    graph: Option<ffmpeg::filter::Graph>,
}

impl AudioController {
    pub fn new(builder: &gtk::Builder) -> Rc<RefCell<AudioController>> {
        // need a RefCell because the callbacks will use immutable versions of ac
        // when the UI controllers will get a mutable version from time to time
        let ac = Rc::new(RefCell::new(AudioController {
            media_ctl: MediaController::new(builder.get_object("audio-container").unwrap()),
            drawingarea: builder.get_object("audio-drawingarea").unwrap(),
            message: "audio place holder".to_owned(),
            frame: None,
            graph: None,
        }));

        let ac_for_cb = ac.clone();
        ac.borrow().drawingarea.connect_draw(move |_, cairo_ctx| {
            ac_for_cb.borrow().draw(&cairo_ctx);
            Inhibit(false)
        });

        ac
    }

    fn build_graph(&mut self, frame_in: &mut ffmpeg::frame::Audio, // TODO: use decoder instead of just time_base
                   time_base: ffmpeg::Rational) -> Result<bool, String> { // TODO: check how to return Ok() only
        match self.graph {
            Some(_) => (),
            None => {
                let mut graph = ffmpeg::filter::Graph::new();

	            let args = format!("time_base={}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
		            time_base, frame_in.rate(), frame_in.format().name(), frame_in.channel_layout().bits());

                let in_filter = ffmpeg::filter::find("abuffer").unwrap();
                match graph.add(&in_filter, "in", &args) {
                    Ok(_) => (),
                    Err(error) => return Err(format!("Error adding in pad: {:?}", error)),
                }

                let out_filter = ffmpeg::filter::find("abuffersink").unwrap();
                match graph.add(&out_filter, "out", "") {
                    Ok(_) => (),
                    Err(error) => return Err(format!("Error adding out pad: {:?}", error)),
                }
                {
                    let mut out_pad = graph.get("out").unwrap();
                    out_pad.set_sample_format(ffmpeg::format::Sample::I16(ffmpeg::format::sample::Type::Packed));
                }

                {
                    let in_parser;
                    match graph.output("in", 0) {
                        Ok(value) => in_parser = value,
                        Err(error) => return Err(format!("Error getting output for in pad: {:?}", error)),
                    }
                    let out_parser;
                    match in_parser.input("out", 0) {
                        Ok(value) => out_parser = value,
                        Err(error) => return Err(format!("Error getting input for out pad: {:?}", error)),
                    }
                    match out_parser.parse("anull") {
                        Ok(_) => (),
                        Err(error) => return Err(format!("Error parsing format: {:?}", error)),
                    }
                }

                match graph.validate() {
                    Ok(_) => self.graph = Some(graph),
                    Err(error) => return Err(format!("Error validating graph: {:?}", error)),
                }

                //println!("{}", graph.dump());
            },
        }

        Ok(true)
    }

    fn convert_to_pcm16(&mut self, frame_in: &mut ffmpeg::frame::Audio,
                      time_base: ffmpeg::Rational) -> Result<ffmpeg::frame::Audio, String> {
        match self.build_graph(frame_in, time_base) {
            Ok(_) => {
                let mut graph = self.graph.as_mut().unwrap();
                graph.get("in").unwrap().source().add(&frame_in).unwrap();

                let mut frame_pcm = ffmpeg::frame::Audio::empty();
                while let Ok(..) = graph.get("out").unwrap().sink().frame(&mut frame_pcm) {
                }

                Ok(frame_pcm)
            },
            Err(error) => Err(error),
        }
    }

    fn draw(&self, cr: &cairo::Context) {
        let allocation = self.drawingarea.get_allocation();
        cr.scale(allocation.width as f64, allocation.height as f64);

        cr.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);
        cr.set_font_size(0.07);

        cr.move_to(0.1, 0.53);
        cr.show_text(&self.message);
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

impl MediaNotifiable for AudioController {
    fn new_media(&mut self, context: &mut Context) {
        self.frame = None;
        self.graph = None;
        self.message = match context.audio_stream.as_mut() {
            Some(stream) => {
                self.set_index(stream.index);

                self.show();
                println!("\n** Audio stream\n{:?}", &stream);
                format!("audio stream {}", self.stream_index().unwrap())
            },
            None => {
                self.hide();
                "no audio stream".to_owned()
            },
        };

        self.drawingarea.queue_draw();
    }
}

impl PacketNotifiable for AudioController {
    fn new_packet(&mut self, stream: &ffmpeg::format::stream::Stream, packet: &ffmpeg::codec::packet::Packet) {
        self.print_packet_content(stream, packet);

        let decoder = stream.codec().decoder();
        match decoder.audio() {
            Ok(mut audio) => {
                //let mut frame = ffmpeg::frame::Audio::new(audio.format(), audio.samples(), audio.layout());
                let mut frame = ffmpeg::frame::Audio::empty();
                match audio.decode(packet, &mut frame) {
                    Ok(result) => if result {
                            let planes = frame.planes();
                            println!("\tdecoded audio frame, found {} planes", planes);
                            if planes > 0 {
                                println!("\tdata len: {}", frame.data(0).len());
                                match self.convert_to_pcm16(&mut frame, audio.time_base()) {
                                    Ok(frame_pcm) => {
                                        self.frame = Some(frame_pcm);
                                    }
                                    Err(error) =>  println!("\tError converting to pcm: {:?}", error),
                                }
                            }
                            else {
                                println!("\tno planes found in frame");
                            }
                        }
                        else {
                            println!("\tfailed to decode audio frame");
                        }
                    ,
                    Err(error) => println!("Error decoding audio: {:?}", error),
                }
            },
            Err(error) => println!("Error getting audio decoder: {:?}", error),
        }
    }
}
