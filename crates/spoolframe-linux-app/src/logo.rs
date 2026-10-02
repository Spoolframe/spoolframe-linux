//! The Spoolframe mark, drawn: a reel in a frame with film running out
//! under it, as in the design's sidebar. Cairo draws the design's paths
//! (a 64-unit square) so no SVG loader is needed at run time. The reel and
//! frame take the text colour, the film the accent; the holes are cut out,
//! so the sidebar shows through in any theme.

use gtk::prelude::*;
use gtk::{cairo, gdk};

/// The mark at `size` pixels square.
pub fn mark(size: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(size)
        .content_height(size)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(|area, cr, width, height| {
        let scale = f64::from(width.min(height)) / 64.0;
        cr.scale(scale, scale);
        let ink = area.color();
        let accent = accent(area);
        let _ = draw(cr, &ink, &accent);
    });
    area
}

/// The theme's accent; libadwaita names it `accent_color`.
#[allow(deprecated)]
fn accent(widget: &gtk::DrawingArea) -> gdk::RGBA {
    widget
        .style_context()
        .lookup_color("accent_color")
        .unwrap_or(gdk::RGBA::new(0.58, 0.886, 0.835, 1.0))
}

fn set(cr: &cairo::Context, colour: &gdk::RGBA) {
    cr.set_source_rgba(
        f64::from(colour.red()),
        f64::from(colour.green()),
        f64::from(colour.blue()),
        f64::from(colour.alpha()),
    );
}

/// A quadratic Bézier from the current point, as SVG's `Q`, in cubic form.
fn quad(cr: &cairo::Context, cx: f64, cy: f64, x: f64, y: f64) -> Result<(), cairo::Error> {
    let (x0, y0) = cr.current_point()?;
    cr.curve_to(
        x0 + 2.0 / 3.0 * (cx - x0),
        y0 + 2.0 / 3.0 * (cy - y0),
        x + 2.0 / 3.0 * (cx - x),
        y + 2.0 / 3.0 * (cy - y),
        x,
        y,
    );
    Ok(())
}

fn draw(cr: &cairo::Context, ink: &gdk::RGBA, accent: &gdk::RGBA) -> Result<(), cairo::Error> {
    // The frame, open at the right where the film leaves it.
    set(cr, ink);
    cr.set_line_width(4.0);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.move_to(9.0, 38.0);
    cr.line_to(9.0, 9.0);
    quad(cr, 9.0, 6.0, 12.0, 6.3)?;
    cr.line_to(52.0, 9.5);
    quad(cr, 55.0, 9.8, 55.0, 13.0)?;
    cr.line_to(55.0, 20.0);
    cr.stroke()?;
    cr.move_to(55.0, 44.0);
    cr.line_to(55.0, 52.0);
    quad(cr, 55.0, 55.0, 52.0, 55.0)?;
    cr.line_to(46.0, 55.0);
    cr.stroke()?;

    // The reel: a disc with six holes and a play mark cut out.
    cr.set_fill_rule(cairo::FillRule::EvenOdd);
    cr.arc(31.0, 30.0, 15.0, 0.0, std::f64::consts::TAU);
    for (x, y) in [
        (31.0, 21.0),
        (38.8, 25.5),
        (38.8, 34.5),
        (31.0, 39.0),
        (23.2, 34.5),
        (23.2, 25.5),
    ] {
        cr.new_sub_path();
        cr.arc(x, y, 3.2, 0.0, std::f64::consts::TAU);
    }
    cr.new_sub_path();
    cr.move_to(28.0, 26.5);
    cr.line_to(34.5, 30.0);
    cr.line_to(28.0, 33.5);
    cr.close_path();
    cr.fill()?;

    // The film running out, its perforations cut out.
    set(cr, accent);
    cr.move_to(6.0, 41.0);
    quad(cr, 28.0, 46.0, 51.0, 53.0)?;
    cr.line_to(49.6, 58.2);
    quad(cr, 27.0, 51.5, 5.0, 46.6)?;
    cr.close_path();
    cr.fill()?;
    cr.set_operator(cairo::Operator::Clear);
    cr.set_line_width(2.0);
    cr.set_line_cap(cairo::LineCap::Butt);
    cr.set_dash(&[3.0, 3.0], 0.0);
    cr.move_to(10.0, 45.6);
    cr.line_to(47.0, 55.0);
    cr.stroke()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mark drawn at 64×64, as ARGB pixels.
    fn drawn() -> cairo::ImageSurface {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 64, 64).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        let white = gdk::RGBA::new(1.0, 1.0, 1.0, 1.0);
        let teal = gdk::RGBA::new(0.58, 0.886, 0.835, 1.0);
        draw(&cr, &white, &teal).unwrap();
        drop(cr);
        surface
    }

    fn alpha(surface: &mut cairo::ImageSurface, x: usize, y: usize) -> u8 {
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        // ARGB32 is native-endian; alpha is the high byte.
        u32::from_ne_bytes(data[y * stride + x * 4..][..4].try_into().unwrap()).to_be_bytes()[0]
    }

    #[test]
    fn the_reel_is_solid_between_its_holes() {
        assert_eq!(alpha(&mut drawn(), 24, 30), 255);
    }

    #[test]
    fn the_reels_holes_are_cut_out() {
        assert_eq!(alpha(&mut drawn(), 31, 21), 0);
    }

    /// Writes the mark at 256 px to the path in SPOOLFRAME_LOGO_PNG, to
    /// look at; run with `--ignored`.
    #[test]
    #[ignore = "writes a picture to look at"]
    fn the_mark_can_be_looked_at() {
        let path = std::env::var("SPOOLFRAME_LOGO_PNG").expect("set SPOOLFRAME_LOGO_PNG");
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 256, 256).unwrap();
        {
            let cr = cairo::Context::new(&surface).unwrap();
            cr.set_source_rgb(0.094, 0.094, 0.145);
            cr.paint().unwrap();
            cr.push_group();
            cr.scale(4.0, 4.0);
            let white = gdk::RGBA::new(0.804, 0.839, 0.957, 1.0);
            let teal = gdk::RGBA::new(0.58, 0.886, 0.835, 1.0);
            draw(&cr, &white, &teal).unwrap();
            cr.pop_group_to_source().unwrap();
            cr.paint().unwrap();
        }
        let stride = surface.stride() as usize;
        let bytes = gtk::glib::Bytes::from(&*surface.data().unwrap());
        let texture = gdk::MemoryTexture::new(
            256,
            256,
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &bytes,
            stride,
        );
        texture.save_to_png(path).unwrap();
    }
}
