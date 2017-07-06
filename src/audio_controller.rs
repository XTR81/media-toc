extern crate gtk;
extern crate cairo;

use std::rc::Rc;
use std::cell::RefCell;

use gtk::prelude::*;
use cairo::enums::{FontSlant, FontWeight};

use controller_ext::Notifiable;

pub struct AudioController {
    drawingarea: gtk::DrawingArea,
    message: String,
}

impl AudioController {
    pub fn new(builder: &gtk::Builder) -> Rc<RefCell<AudioController>> {
        // need a RefCell because the callbacks will use immutable versions of ac
        // when the UI controllers will get a mutable version from time to time
        let ac = Rc::new(RefCell::new(AudioController {
            drawingarea: builder.get_object("audio-drawingarea").unwrap(),
            message: String::from("audio place holder"),
        }));

        let ac_for_cb = ac.clone();
        ac.borrow().drawingarea.connect_draw(move |_, cairo_ctx| {
            ac_for_cb.borrow().draw(&cairo_ctx);
            Inhibit(false)
        });

        ac
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

impl Notifiable for AudioController {
    fn notify_new_media(&mut self) {
        self.message = String::from("new media opened");
        self.drawingarea.queue_draw();
    }
}
