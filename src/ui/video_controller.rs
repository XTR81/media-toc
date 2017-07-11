extern crate gtk;
extern crate cairo;

extern crate ffmpeg;

use std::ops::{Deref, DerefMut};

use std::rc::Rc;
use std::cell::RefCell;

use gtk::prelude::*;
use cairo::enums::{FontSlant, FontWeight};

use ffmpeg::format::stream::disposition::ATTACHED_PIC;

use ::media::Context;
use ::media::PacketNotifiable;

use super::MediaNotifiable;
use super::MediaController;

fn ffmpeg_pixel_format_to_cairo(ffmpeg_px: ffmpeg::format::Pixel) -> cairo::Format {
    match ffmpeg_px {
        ffmpeg::format::Pixel::ARGB => cairo::Format::ARgb32,
        ffmpeg::format::Pixel::RGB24 => cairo::Format::Rgb24,
        ffmpeg::format::Pixel::RGB565LE => cairo::Format::Rgb16_565,
        _ => cairo::Format::Invalid,
    }
}

pub struct VideoController {
    media_ctl: MediaController,
    drawingarea: gtk::DrawingArea,
    message: String,
    frame: Option<ffmpeg::frame::Video>,
}


impl VideoController {
    pub fn new(builder: &gtk::Builder) -> Rc<RefCell<VideoController>> {
        // need a RefCell because the callbacks will use immutable versions of vc
        // when the UI controllers will get a mutable version from time to time
        let vc = Rc::new(RefCell::new(VideoController {
            media_ctl: MediaController::new(builder.get_object("video-container").unwrap()),
            drawingarea: builder.get_object("video-drawingarea").unwrap(),
            message: "video place holder".to_owned(),
            frame: None,
        }));

        let vc_for_cb = vc.clone();
        vc.borrow().drawingarea.connect_draw(move |_, cairo_ctx| {
            vc_for_cb.borrow().draw(&cairo_ctx);
            Inhibit(false)
        });

        vc
    }

    fn convert_to_rgb(&mut self, frame_in: &mut ffmpeg::frame::Video, time_base:
                      ffmpeg::Rational) -> Result<(ffmpeg::frame::Video), String> {
        let mut graph = ffmpeg::filter::Graph::new();
        let in_filter;
        match ffmpeg::filter::find("buffer") {
            Some(value) => in_filter = value,
            None => return Err("Couldn't get buffer".to_owned()),
        }

        // Fix deprecated formats
        let in_format = match frame_in.format() {
            ffmpeg::format::Pixel::YUVJ420P => ffmpeg::format::Pixel::YUV420P,
            ffmpeg::format::Pixel::YUVJ422P => ffmpeg::format::Pixel::YUV422P,
            ffmpeg::format::Pixel::YUVJ444P => ffmpeg::format::Pixel::YUV444P,
            ffmpeg::format::Pixel::YUVJ440P => ffmpeg::format::Pixel::YUV440P,
            other => other,
        };
        frame_in.set_format(in_format);
        let in_format_str = format!("{:?}", in_format).to_lowercase();
        let args = format!("width={}:height={}:pix_fmt={}:time_base={}:pixel_aspect={}",
                           frame_in.width(), frame_in.height(), in_format_str,
                           time_base, frame_in.aspect_ratio());
        println!("pad in args: {}", args);
        match graph.add(&in_filter, "in", &args) {
            Ok(_) => (),
            Err(error) => return Err(format!("Error adding in pad: {:?}", error)),
        }

        let out_filter;
        match ffmpeg::filter::find("buffersink") {
            Some(value) => out_filter = value,
            None => return Err("Couldn't get buffersink".to_owned()),
        }
        match graph.add(&out_filter, "out", "") {
            Ok(_) => (),
            Err(error) => return Err(format!("Error adding out pad: {:?}", error)),
        }
        {
            let mut out_pad = graph.get("out").unwrap();
            out_pad.set_pixel_format(ffmpeg::format::Pixel::RGB565LE);
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
            match out_parser.parse("copy") {
                Ok(_) => (),
                Err(error) => return Err(format!("Error parsing format: {:?}", error)),
            }
        }

        match graph.validate() {
            Ok(_) => (),
            Err(error) => return Err(format!("Error validating graph: {:?}", error)),
        }

        //println!("{}", graph.dump());

        graph.get("in").unwrap().source().add(&frame_in).unwrap();

        let mut frame_rgb = ffmpeg::frame::Video::empty();
		while let Ok(..) = graph.get("out").unwrap().sink().frame(&mut frame_rgb) {
        }

        Ok(frame_rgb)
    }

    fn draw(&self, cr: &cairo::Context) {
        let allocation = self.drawingarea.get_allocation();

        match self.frame {
            Some(ref frame) => {
                let planes = frame.planes();
                if planes > 0 {
                    /*
                    println!("format: {:?}, width: {}, stride: {}",
                             frame.format(), frame.width(), frame.stride(0));
                    let test_surface = cairo::ImageSurface::create(
                            ffmpeg_pixel_format_to_cairo(frame.format()),
                            frame.width() as i32, frame.height() as i32
                        );
                    println!("expected stride: {}", test_surface.get_stride());
                    */

                    let surface = cairo::ImageSurface::create_for_data(
                            frame.data(0).to_vec().into_boxed_slice(), |_| {},
                            ffmpeg_pixel_format_to_cairo(frame.format()),
                            frame.width() as i32, frame.height() as i32,
                            frame.stride(0) as i32
                        );

                    let x;
                    let y;
                    let mut width_scale = allocation.width as f64 / surface.get_width() as f64;
                    let mut height_scale = allocation.height as f64 / surface.get_height() as f64;
                    let ratio = width_scale / height_scale;
                    if ratio > 0f64 {
                        width_scale /= ratio;
                        x = (allocation.width as f64 - width_scale * (surface.get_width() as f64)).abs();
                        y = 0f64;
                    }
                    else {
                        height_scale /= ratio;
                        x = 0f64;
                        y = (allocation.height as f64 - height_scale * (surface.get_height() as f64)).abs();
                    }
                    println!("aw {}, ah {}, sw {}, sh {}, ratio {}, x {}, y {}",
                             allocation.width, allocation.height, surface.get_width(), surface.get_height(), ratio, x, y);
                    cr.scale(width_scale, height_scale);
                    cr.set_source_surface(&surface, x, y);
                    cr.paint();
                }
            },
            None => {
                cr.scale(allocation.width as f64, allocation.height as f64);
                cr.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);
                cr.set_font_size(0.07);

                cr.move_to(0.1, 0.53);
                cr.show_text(&self.message);
            },
        }
    }
}

impl Deref for VideoController {
	type Target = MediaController;

	fn deref(&self) -> &Self::Target {
		&self.media_ctl
	}
}

impl DerefMut for VideoController {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.media_ctl
	}
}

impl MediaNotifiable for VideoController {
    fn new_media(&mut self, context: &mut Context) {
        self.frame = None;
        self.message = match context.video_stream.as_mut() {
            Some(stream) => {
                self.set_index(stream.index);

                self.show();
                println!("\n** Video stream\n{:?}", &stream);

                let stream_type;
                if stream.disposition | ATTACHED_PIC == ATTACHED_PIC {
                    stream_type = "image";
                }
                else {
                    stream_type = "video stream";
                }
                format!("{} {}", stream_type, self.stream_index().unwrap())
            },
            None => {
                self.hide();
                "no video stream".to_owned()
            },
        };

        self.drawingarea.queue_draw();
    }
}

impl PacketNotifiable for VideoController {
    fn new_packet(&mut self, stream: &ffmpeg::format::stream::Stream, packet: &ffmpeg::codec::packet::Packet) {
        self.print_packet_content(stream, packet);

        let decoder = stream.codec().decoder();
        match decoder.video() {
            Ok(mut video) => {
                let mut frame = ffmpeg::frame::Video::new(video.format(), video.width(), video.height());
                match video.decode(packet, &mut frame) {
                    Ok(result) => if result {
                            let planes = frame.planes();
                            if planes > 0 {
                                println!("\tdecoded video frame, data len: {}", frame.data(0).len());

                                match self.convert_to_rgb(&mut frame, video.time_base()) {
                                    Ok(frame_rgb) => {
                                        self.frame = Some(frame_rgb);
                                    }
                                    Err(error) =>  println!("\tError converting to rgb: {:?}", error),
                                }
                            }
                            else {
                                println!("\tno planes found in frame");
                            }
                        }
                        else {
                            println!("\tfailed to decode video frame");
                        }
                    ,
                    Err(error) => println!("Error decoding video: {:?}", error),
                }
            },
            Err(error) => println!("Error getting video decoder: {:?}", error),
        }
    }
}
