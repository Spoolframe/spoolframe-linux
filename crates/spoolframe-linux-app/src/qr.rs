//! A QR code a phone's camera reads from across a desk: dark modules on
//! white, with the standard four-module quiet zone, whatever the theme
//! (many cameras refuse a light-on-dark code). Each module is a whole
//! number of pixels and the picture is shown at that size, so no scaling
//! blurs the edges.
//!
//! The Spoolframe reel sits in the middle, as the design has it. The code
//! uses the highest error correction (H, 30% of it may be lost), and the
//! reel with its clear margin covers far less than that.

use std::f64::consts::TAU;

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

/// `data` as a QR code texture, `module` pixels per module, with the reel
/// in `accent` in the middle.
pub fn texture(data: &str, module: usize, accent: &gdk::RGBA) -> anyhow::Result<gdk::Texture> {
    let (side, mut surface) = draw(data, module, accent)?;
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
fn draw(
    data: &str,
    module: usize,
    accent: &gdk::RGBA,
) -> anyhow::Result<(usize, cairo::ImageSurface)> {
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

        // The reel: a clear square of whole modules, and the three-hole
        // mark inside it with a module's margin.
        let (start, span) = mark_square(width);
        let (x, size) = ((start + QUIET) as f64 * m, span as f64 * m);
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(x, x, size, size);
        cr.fill()?;
        let reel = size - 2.0 * m;
        cr.translate(x + m, x + m);
        // The design's small mark: its square from 12 to 88.
        cr.scale(reel / 76.0, reel / 76.0);
        cr.translate(-12.0, -12.0);
        cr.set_source_rgba(
            f64::from(accent.red()),
            f64::from(accent.green()),
            f64::from(accent.blue()),
            1.0,
        );
        cr.arc(50.0, 50.0, 38.0, 0.0, TAU);
        cr.fill()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        for (hx, hy) in [(50.0, 30.0), (67.32, 60.0), (32.68, 60.0)] {
            cr.arc(hx, hy, 10.5, 0.0, TAU);
            cr.fill()?;
        }
    }
    Ok((side, surface))
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
    const TEAL: gdk::RGBA = gdk::RGBA::new(0.58, 0.886, 0.835, 1.0);

    /// The pixel at (`x`, `y`) as 0xRRGGBB.
    fn rgb(surface: &mut cairo::ImageSurface, x: usize, y: usize) -> u32 {
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        u32::from_ne_bytes(data[y * stride + x * 4..][..4].try_into().unwrap()) & 0x00ff_ffff
    }

    #[test]
    fn the_quiet_zone_is_white() {
        let (_, mut surface) = draw(URL, 2, &TEAL).unwrap();
        assert_eq!(rgb(&mut surface, QUIET * 2 - 1, QUIET * 2 - 1), 0xffffff);
    }

    #[test]
    fn a_finder_pattern_starts_where_the_quiet_zone_ends() {
        let (_, mut surface) = draw(URL, 2, &TEAL).unwrap();
        assert_eq!(rgb(&mut surface, QUIET * 2, QUIET * 2), 0x000000);
    }

    #[test]
    fn a_module_is_a_square_of_whole_pixels() {
        let (_, mut surface) = draw(URL, 3, &TEAL).unwrap();
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

    #[test]
    fn the_reel_sits_in_the_middle_in_the_accent() {
        let (side, mut surface) = draw(URL, MODULE, &TEAL).unwrap();
        // Between the reel's centre and its top hole: solid reel.
        let colour = rgb(&mut surface, side / 2, side / 2 + MODULE * 2);
        assert_eq!(colour >> 16, (0.58_f64 * 255.0).round() as u32);
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
        let (side, _) = draw(URL, MODULE, &TEAL).unwrap();
        assert!((250..=360).contains(&side), "{side} px");
    }
}
