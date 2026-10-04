//! A QR code a phone's camera reads from across a desk: dark modules on
//! white, with the standard four-module quiet zone, whatever the theme
//! (many cameras refuse a light-on-dark code). Each module is a whole
//! number of pixels and the picture is shown at that size, so no scaling
//! blurs the edges.
//!
//! The Spoolframe mark sits in the middle on a dark tile, as the design
//! has it, in its Mocha colours whatever the theme: the code is always
//! white. The code uses the highest error correction (H, 30% of it may be
//! lost), and the mark with its clear margin covers far less than that.

use std::f64::consts::{FRAC_PI_2, PI};

use gtk::{cairo, gdk, glib};
use qrcode::{EcLevel, QrCode};

/// Light modules around the code, as the standard asks.
const QUIET: usize = 4;

/// Pixels per module: about 300 px for a web address, which a phone
/// reads from a seat away.
pub const MODULE: usize = 6;

/// How much of the code's width the cleared square in the middle takes:
/// about a sixteenth of its area.
const MARK_SHARE: f64 = 0.25;

/// The tile under the mark: `#11111b`.
const TILE: (f64, f64, f64) = (0.067, 0.067, 0.106);

/// `data` as a QR code texture, `module` pixels per module, with the mark
/// in the middle.
pub fn texture(data: &str, module: usize) -> anyhow::Result<gdk::Texture> {
    let (side, mut surface) = draw(data, module)?;
    let stride = surface.stride() as usize;
    let bytes = glib::Bytes::from(&*surface.data()?);
    let texture = gdk::MemoryTexture::new(
        side as i32,
        side as i32,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        stride,
    );
    Ok(texture.into())
}

/// The code drawn: its side in pixels, and the picture.
fn draw(data: &str, module: usize) -> anyhow::Result<(usize, cairo::ImageSurface)> {
    let code = QrCode::with_error_correction_level(data, EcLevel::H)?;
    let width = code.width();
    let side = (width + 2 * QUIET) * module;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, side as i32, side as i32)?;
    {
        let cr = cairo::Context::new(&surface)?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint()?;
        let m = module as f64;
        cr.set_source_rgb(0.0, 0.0, 0.0);
        for (i, colour) in code.to_colors().into_iter().enumerate() {
            if colour == qrcode::Color::Dark {
                let (x, y) = (i % width + QUIET, i / width + QUIET);
                cr.rectangle(x as f64 * m, y as f64 * m, m, m);
            }
        }
        cr.fill()?;

        // A clear square of whole modules; in it, with a module's margin,
        // the dark tile, a quarter of it rounded, and the mark on it at
        // the design's 37 of 48.
        let (start, span) = mark_square(width);
        let (x, size) = ((start + QUIET) as f64 * m, span as f64 * m);
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(x, x, size, size);
        cr.fill()?;
        let tile = size - 2.0 * m;
        rounded(&cr, x + m, tile, tile / 4.0);
        cr.set_source_rgb(TILE.0, TILE.1, TILE.2);
        cr.fill()?;
        let mark = tile * 37.0 / 48.0;
        let at = x + m + (tile - mark) / 2.0;
        cr.translate(at, at);
        crate::logo::paint(&cr, mark, crate::logo::MOCHA)?;
    }
    Ok((side, surface))
}

/// A square at (`at`, `at`), `side` across, its corners rounded by
/// `radius`.
fn rounded(cr: &cairo::Context, at: f64, side: f64, radius: f64) {
    let (near, far) = (at + radius, at + side - radius);
    cr.new_sub_path();
    cr.arc(far, near, radius, -FRAC_PI_2, 0.0);
    cr.arc(far, far, radius, 0.0, FRAC_PI_2);
    cr.arc(near, far, radius, FRAC_PI_2, PI);
    cr.arc(near, near, radius, PI, PI + FRAC_PI_2);
    cr.close_path();
}

/// Where the cleared square starts in a code `width` modules wide, and
/// how many modules it spans: centred, odd like the code, so it stays
/// symmetric.
fn mark_square(width: usize) -> (usize, usize) {
    let mut span = (width as f64 * MARK_SHARE).round() as usize;
    if span.is_multiple_of(2) {
        span += 1;
    }
    ((width - span) / 2, span)
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://media.example.org/auth/device?code=BCDFGHJK";

    /// The pixel at (`x`, `y`) as 0xRRGGBB.
    fn rgb(surface: &mut cairo::ImageSurface, x: usize, y: usize) -> u32 {
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        u32::from_ne_bytes(data[y * stride + x * 4..][..4].try_into().unwrap()) & 0x00ff_ffff
    }

    #[test]
    fn the_quiet_zone_is_white() {
        let (_, mut surface) = draw(URL, 2).unwrap();
        assert_eq!(rgb(&mut surface, QUIET * 2 - 1, QUIET * 2 - 1), 0xffffff);
    }

    #[test]
    fn a_finder_pattern_starts_where_the_quiet_zone_ends() {
        let (_, mut surface) = draw(URL, 2).unwrap();
        assert_eq!(rgb(&mut surface, QUIET * 2, QUIET * 2), 0x000000);
    }

    #[test]
    fn a_module_is_a_square_of_whole_pixels() {
        let (_, mut surface) = draw(URL, 3).unwrap();
        let corner = QUIET * 3;
        assert_eq!(
            [(0, 0), (2, 0), (0, 2), (2, 2)].map(|(dx, dy)| rgb(
                &mut surface,
                corner + dx,
                corner + dy
            )),
            [0; 4]
        );
    }

    /// The pixel under the mark's point (`ux`, `uy`) of its 100 units,
    /// placed as `draw` places it.
    fn at_mark(ux: f64, uy: f64) -> u32 {
        let (_, mut surface) = draw(URL, MODULE).unwrap();
        let width = QrCode::with_error_correction_level(URL, EcLevel::H)
            .unwrap()
            .width();
        let (start, span) = mark_square(width);
        let m = MODULE as f64;
        let tile = span as f64 * m - 2.0 * m;
        let mark = tile * 37.0 / 48.0;
        let at = (start + QUIET) as f64 * m + m + (tile - mark) / 2.0;
        let pixel = |u: f64| (at + u / 100.0 * mark) as usize;
        rgb(&mut surface, pixel(ux), pixel(uy))
    }

    #[test]
    fn the_reel_sits_in_the_middle_in_its_teal() {
        // Below the play mark, between the lower holes: solid reel.
        assert_eq!(at_mark(50.0, 75.0), 0x94e2d5);
    }

    #[test]
    fn a_reel_hole_shows_the_dark_tile() {
        assert_eq!(at_mark(50.0, 32.0), 0x11111b);
    }

    #[test]
    fn the_reel_covers_far_less_than_the_code_can_lose() {
        // Level H recovers 30% of the code; the cleared square is kept
        // well under that.
        let width = QrCode::with_error_correction_level(URL, EcLevel::H)
            .unwrap()
            .width();
        let (_, span) = mark_square(width);
        let share = (span * span) as f64 / (width * width) as f64;
        assert!(share < 0.1, "{share}");
    }

    #[test]
    fn a_web_address_reads_from_across_a_desk() {
        // A camera wants the code a few centimetres across: about
        // 250 to 360 px on a desktop screen.
        let (side, _) = draw(URL, MODULE).unwrap();
        assert!((250..=360).contains(&side), "{side} px");
    }
}
