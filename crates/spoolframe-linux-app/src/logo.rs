//! The Spoolframe mark and wordmark, from the "Spoolframe Logo" design: a
//! film reel with five holes and a play cut-out, inside four frame
//! corners; under 40 px, a three-hole reel alone. Cairo draws the design's
//! paths, so no SVG loader is needed at run time. The corners take the
//! text colour and the reel the theme's accent, like the rest of the
//! chrome; the holes and the play mark are cut out, so what is behind
//! shows through in any theme, as the design's ground colour does.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use gtk::prelude::*;
use gtk::{cairo, gdk};
use spoolframe_linux_core::paths;

/// Under this many pixels the mark is the small reel: the corners and
/// five holes no longer read.
const SMALL: i32 = 40;

/// The mark at `size` pixels square.
pub fn mark(size: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(size)
        .content_height(size)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    area.set_draw_func(|area, cr, width, height| {
        let side = width.min(height);
        let ink = area.color();
        let accent = accent(area);
        let _ = draw(cr, f64::from(side), side < SMALL, &ink, &accent);
    });
    area
}

/// "spool" in the text colour and "frame" in the accent: the design's
/// wordmark, beside the mark. Window titles keep plain "Spoolframe".
pub fn wordmark() -> gtk::Box {
    let wordmark = gtk::Box::builder()
        .css_classes(["wordmark"])
        .valign(gtk::Align::Center)
        .build();
    wordmark.update_property(&[gtk::accessible::Property::Label("Spoolframe")]);
    for (part, classes) in [("spool", &[][..]), ("frame", &["wordmark-accent"][..])] {
        wordmark.append(
            &gtk::Label::builder()
                .label(part)
                .css_classes(classes)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build(),
        );
    }
    wordmark
}

/// Makes the wordmark's face, Bricolage Grotesque (`fonts/`, OFL), known
/// to the app: carried, not hoped to be installed. Pango takes a font only
/// by path, so it is written to the cache directory first. Call once,
/// after GTK is up; without it the wordmark falls back to the UI face.
pub fn register_font() {
    const FONT: &[u8] = include_bytes!("../fonts/BricolageGrotesque.ttf");
    let Some(dir) = paths::cache_dir() else {
        return tracing::warn!("no cache directory; the wordmark uses the UI face");
    };
    let path = dir.join("BricolageGrotesque.ttf");
    let written = std::fs::metadata(&path).is_ok_and(|m| m.len() == FONT.len() as u64);
    if !written
        && let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, FONT))
    {
        return tracing::warn!(error = %e, path = %path.display(), "cannot write the wordmark's font");
    }
    let Some(fonts) = gtk::Label::new(None).pango_context().font_map() else {
        return;
    };
    if let Err(e) = fonts.add_font_file(&path) {
        tracing::warn!(error = %e, path = %path.display(), "cannot load the wordmark's font");
    }
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

/// Draws the mark `side` pixels square, `small` or full. In a group of
/// its own, so cutting the holes clears only the mark.
fn draw(
    cr: &cairo::Context,
    side: f64,
    small: bool,
    ink: &gdk::RGBA,
    accent: &gdk::RGBA,
) -> Result<(), cairo::Error> {
    cr.push_group();
    if small {
        small_reel(cr, side, accent)?;
    } else {
        full(cr, side, ink, accent)?;
    }
    cr.pop_group_to_source()?;
    cr.paint()
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

/// The small mark: the reel with three holes, in the design's square
/// from 12 to 88.
fn small_reel(cr: &cairo::Context, side: f64, accent: &gdk::RGBA) -> Result<(), cairo::Error> {
    cr.scale(side / 76.0, side / 76.0);
    cr.translate(-12.0, -12.0);
    set(cr, accent);
    cr.arc(50.0, 50.0, 38.0, 0.0, TAU);
    cr.fill()?;
    cr.set_operator(cairo::Operator::Clear);
    for (x, y) in [(50.0, 30.0), (67.32, 60.0), (32.68, 60.0)] {
        cr.arc(x, y, 10.5, 0.0, TAU);
        cr.fill()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: gdk::RGBA = gdk::RGBA::new(0.804, 0.839, 0.957, 1.0);
    const TEAL: gdk::RGBA = gdk::RGBA::new(0.58, 0.886, 0.835, 1.0);

    /// The mark drawn `side` pixels square, as ARGB pixels.
    fn drawn(side: i32, small: bool) -> cairo::ImageSurface {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, side, side).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        draw(&cr, f64::from(side), small, &INK, &TEAL).unwrap();
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
        assert_eq!(alpha(&mut drawn(100, false), 50, 75), 255);
    }

    #[test]
    fn the_reels_holes_are_cut_out() {
        assert_eq!(alpha(&mut drawn(100, false), 50, 32), 0);
    }

    #[test]
    fn the_play_mark_is_cut_out() {
        assert_eq!(alpha(&mut drawn(100, false), 50, 50), 0);
    }

    #[test]
    fn the_frame_has_its_corners() {
        assert_eq!(alpha(&mut drawn(100, false), 10, 20), 255);
    }

    // Drawn at 76 px, the small mark's units are pixels, 12 off.

    #[test]
    fn the_small_reel_has_three_holes_cut_out() {
        let mut small = drawn(76, true);
        assert_eq!(
            [(38, 18), (55, 48), (21, 48)].map(|(x, y)| alpha(&mut small, x, y)),
            [0, 0, 0]
        );
    }

    #[test]
    fn the_small_reel_is_solid_between_its_holes() {
        assert_eq!(alpha(&mut drawn(76, true), 38, 50), 255);
    }

    /// Writes the full mark at 256 px and the small one at 32 px, on the
    /// design's ground, to the path in SPOOLFRAME_LOGO_PNG, to look at;
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
            draw(&cr, 256.0, false, &INK, &TEAL).unwrap();
            cr.translate(272.0, 112.0);
            draw(&cr, 32.0, true, &INK, &TEAL).unwrap();
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
