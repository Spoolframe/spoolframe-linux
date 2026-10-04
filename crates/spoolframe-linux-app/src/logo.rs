//! The Spoolframe mark and wordmark, from the "Spoolframe Logo" design: a
//! film reel with five holes and a play cut-out, inside four frame
//! corners, at every size the app draws it. The three-hole reel is only
//! the desktop icon's, at sizes where the corners no longer read. Cairo
//! draws the design's paths, so no SVG loader is needed at run time.
//!
//! The logo's colours are its own, never the palette's accent: Mocha's
//! on a dark scheme, Latte's on a light one. The holes and the play mark
//! are cut out, so what is behind shows through, as the design's ground
//! colour does.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use gtk::prelude::*;
use gtk::{cairo, gdk};

/// The logo's two colours: the corners and "spool", the reel and "frame".
#[derive(Debug, Clone, Copy)]
pub struct Colours {
    pub ink: gdk::RGBA,
    pub reel: gdk::RGBA,
}

/// On a dark ground: ink `#cdd6f4`, reel `#94e2d5`.
pub const MOCHA: Colours = Colours {
    ink: gdk::RGBA::new(0.804, 0.839, 0.957, 1.0),
    reel: gdk::RGBA::new(0.58, 0.886, 0.835, 1.0),
};

/// On a light ground: ink `#4c4f69`, reel `#179299`.
pub const LATTE: Colours = Colours {
    ink: gdk::RGBA::new(0.298, 0.31, 0.412, 1.0),
    reel: gdk::RGBA::new(0.09, 0.573, 0.6, 1.0),
};

/// The colours for the scheme drawn now.
fn colours() -> Colours {
    if adw::StyleManager::default().is_dark() {
        MOCHA
    } else {
        LATTE
    }
}

/// The mark at `size` pixels square.
pub fn mark(size: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(size)
        .content_height(size)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    area.set_draw_func(|_, cr, width, height| {
        let _ = paint(cr, f64::from(width.min(height)), colours());
    });
    let weak = area.downgrade();
    adw::StyleManager::default().connect_dark_notify(move |_| {
        if let Some(area) = weak.upgrade() {
            area.queue_draw();
        }
    });
    area
}

/// "spool" in the logo's ink and "frame" in its reel colour: the design's
/// wordmark, beside the mark. Window titles keep plain "Spoolframe".
pub fn wordmark() -> gtk::Box {
    let wordmark = gtk::Box::builder()
        .css_classes(["wordmark"])
        .valign(gtk::Align::Center)
        .build();
    wordmark.update_property(&[gtk::accessible::Property::Label("Spoolframe")]);
    let spool = part("spool");
    let frame = part("frame");
    wordmark.append(&spool);
    wordmark.append(&frame);
    // Coloured by Pango, not the stylesheet: its colours follow the
    // palette, and the logo's do not.
    let colour = move || {
        let Colours { ink, reel } = colours();
        spool.set_attributes(Some(&foreground(&ink)));
        frame.set_attributes(Some(&foreground(&reel)));
    };
    colour();
    adw::StyleManager::default().connect_dark_notify(move |_| colour());
    wordmark
}

fn part(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build()
}

fn foreground(colour: &gdk::RGBA) -> pango::AttrList {
    let attributes = pango::AttrList::new();
    let channel = |c: f32| (c * 65535.0).round() as u16;
    attributes.insert(pango::AttrColor::new_foreground(
        channel(colour.red()),
        channel(colour.green()),
        channel(colour.blue()),
    ));
    attributes
}

fn set(cr: &cairo::Context, colour: &gdk::RGBA) {
    cr.set_source_rgba(
        f64::from(colour.red()),
        f64::from(colour.green()),
        f64::from(colour.blue()),
        f64::from(colour.alpha()),
    );
}

/// Paints the mark `side` pixels square at the current point. In a group
/// of its own, so cutting the holes clears only the mark.
pub fn paint(cr: &cairo::Context, side: f64, colours: Colours) -> Result<(), cairo::Error> {
    cr.save()?;
    cr.push_group();
    full(cr, side, &colours.ink, &colours.reel)?;
    cr.pop_group_to_source()?;
    cr.paint()?;
    cr.restore()
}

/// The full mark, in the design's 100-unit square.
fn full(
    cr: &cairo::Context,
    side: f64,
    ink: &gdk::RGBA,
    accent: &gdk::RGBA,
) -> Result<(), cairo::Error> {
    cr.scale(side / 100.0, side / 100.0);
    // The four frame corners, each a line, a quarter circle of radius 5
    // and a line.
    set(cr, ink);
    cr.set_line_width(4.0);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    for (start, corner, from, end) in [
        ((10.0, 26.0), (15.0, 15.0), PI, (26.0, 10.0)),
        ((74.0, 10.0), (85.0, 15.0), PI + FRAC_PI_2, (90.0, 26.0)),
        ((90.0, 74.0), (85.0, 85.0), 0.0, (74.0, 90.0)),
        ((26.0, 90.0), (15.0, 85.0), FRAC_PI_2, (10.0, 74.0)),
    ] {
        cr.move_to(start.0, start.1);
        cr.arc(corner.0, corner.1, 5.0, from, from + FRAC_PI_2);
        cr.line_to(end.0, end.1);
    }
    cr.stroke()?;

    // The reel, its five holes and the play mark cut out.
    set(cr, accent);
    cr.arc(50.0, 50.0, 31.0, 0.0, TAU);
    cr.fill()?;
    cr.set_operator(cairo::Operator::Clear);
    for (x, y) in [
        (50.0, 32.0),
        (67.12, 44.44),
        (60.58, 64.56),
        (39.42, 64.56),
        (32.88, 44.44),
    ] {
        cr.arc(x, y, 7.0, 0.0, TAU);
        cr.fill()?;
    }
    cr.move_to(46.5, 45.0);
    cr.line_to(55.5, 50.0);
    cr.line_to(46.5, 55.0);
    cr.close_path();
    cr.set_line_width(1.5);
    cr.fill_preserve()?;
    cr.stroke()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mark drawn `side` pixels square, as ARGB pixels.
    fn drawn(side: i32) -> cairo::ImageSurface {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, side, side).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        paint(&cr, f64::from(side), MOCHA).unwrap();
        drop(cr);
        surface
    }

    fn alpha(surface: &mut cairo::ImageSurface, x: usize, y: usize) -> u8 {
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        // ARGB32 is native-endian; alpha is the high byte.
        u32::from_ne_bytes(data[y * stride + x * 4..][..4].try_into().unwrap()).to_be_bytes()[0]
    }

    // Drawn at 100 px, the full mark's units are pixels.

    #[test]
    fn the_reel_is_solid_between_its_holes() {
        assert_eq!(alpha(&mut drawn(100), 50, 75), 255);
    }

    #[test]
    fn the_reels_holes_are_cut_out() {
        assert_eq!(alpha(&mut drawn(100), 50, 32), 0);
    }

    #[test]
    fn the_play_mark_is_cut_out() {
        assert_eq!(alpha(&mut drawn(100), 50, 50), 0);
    }

    #[test]
    fn the_frame_has_its_corners() {
        assert_eq!(alpha(&mut drawn(100), 10, 20), 255);
    }

    #[test]
    fn a_sidebar_sized_mark_still_has_its_corners() {
        // The 26 px mark beside the wordmark is the full one: a corner's
        // stroke at (10, 20) of 100 lands on (2, 5).
        assert!(alpha(&mut drawn(26), 2, 5) > 0);
    }

    /// Writes the mark at 256 px and at 26 px, on the design's ground, to
    /// the path in SPOOLFRAME_LOGO_PNG, to look at;
    /// run with `--ignored`.
    #[test]
    #[ignore = "writes a picture to look at"]
    fn the_mark_can_be_looked_at() {
        let path = std::env::var("SPOOLFRAME_LOGO_PNG").expect("set SPOOLFRAME_LOGO_PNG");
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 320, 256).unwrap();
        {
            let cr = cairo::Context::new(&surface).unwrap();
            cr.set_source_rgb(0.118, 0.118, 0.18);
            cr.paint().unwrap();
            paint(&cr, 256.0, MOCHA).unwrap();
            cr.translate(276.0, 115.0);
            paint(&cr, 26.0, MOCHA).unwrap();
        }
        let stride = surface.stride() as usize;
        let bytes = gtk::glib::Bytes::from(&*surface.data().unwrap());
        let texture = gdk::MemoryTexture::new(
            320,
            256,
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &bytes,
            stride,
        );
        texture.save_to_png(path).unwrap();
    }
}
