//! Artwork, fetched through the session (P1-05), kept in the device's
//! artwork cache, and drawn over the coloured placeholder tiles. The
//! placeholder stays when an item has no artwork or the fetch fails, so a
//! missing image never leaves a hole.

use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use spoolframe_linux_core::artcache::ArtworkCache;
use spoolframe_linux_core::backend::{ArtworkSize, BackendError, ServerKey, Session};
use spoolframe_types::{Artwork, ArtworkId, ArtworkKind};

/// The most pixels an image from the server may have: 8192 × 8192, 256 MB
/// decoded. A hostile server could send a few bytes that claim far more
/// (PX-11, C-14).
pub const MAX_PIXELS: u64 = 8192 * 8192;

/// An image's width and height, read from its header; nothing is
/// decoded. None for bytes that are no image format it knows.
pub fn image_size(bytes: &[u8]) -> Option<(u64, u64)> {
    let size = imagesize::blob_size(bytes).ok()?;
    Some((size.width as u64, size.height as u64))
}

/// The server's image bytes as a texture, refused unread when the header
/// says it is larger than [`MAX_PIXELS`].
pub fn decode(bytes: &[u8]) -> Result<gdk::Texture, String> {
    let (width, height) = image_size(bytes).ok_or("not an image")?;
    if width.saturating_mul(height) > MAX_PIXELS {
        return Err(format!("{width}×{height} is too large"));
    }
    gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).map_err(|e| e.to_string())
}

/// Loads one session's artwork.
pub struct Art {
    session: Arc<dyn Session>,
    server: ServerKey,
    /// The device's cache of the server's image bytes; without one (it
    /// could not be opened), every image is fetched.
    cache: Option<Arc<ArtworkCache>>,
}

impl Art {
    pub fn new(
        session: Arc<dyn Session>,
        server: ServerKey,
        cache: Option<Arc<ArtworkCache>>,
    ) -> Rc<Self> {
        Rc::new(Self {
            session,
            server,
            cache,
        })
    }

    /// Draws the first of `kinds` the item has over `tile`, once it loads.
    pub fn show(
        self: &Rc<Self>,
        tile: &gtk::Overlay,
        artwork: &[Artwork],
        kinds: &[ArtworkKind],
        size: ArtworkSize,
    ) {
        let Some(id) = first_of(artwork, kinds) else {
            return;
        };
        let (session, server, cache) = (
            self.session.clone(),
            self.server.clone(),
            self.cache.clone(),
        );
        load(tile, async move {
            fetch(session, server, cache, id, size).await
        });
    }

    /// Draws the artwork `id` names over `tile`, once it loads: a
    /// playlist's cover (PX-53), which is not one of an item's kinds.
    pub fn show_id(self: &Rc<Self>, tile: &gtk::Overlay, id: &ArtworkId, size: ArtworkSize) {
        let (session, server, cache, id) = (
            self.session.clone(),
            self.server.clone(),
            self.cache.clone(),
            id.clone(),
        );
        load(tile, async move {
            fetch(session, server, cache, id, size).await
        });
    }

    /// The first of `kinds` the item has, as image bytes with the
    /// artwork's id; `None` when it has none or the fetch fails. For uses
    /// other than a tile, such as the cover MPRIS points to.
    pub async fn bytes(
        &self,
        artwork: &[Artwork],
        kinds: &[ArtworkKind],
        size: ArtworkSize,
    ) -> Option<(ArtworkId, Vec<u8>)> {
        let id = first_of(artwork, kinds)?;
        let (session, server, cache) = (
            self.session.clone(),
            self.server.clone(),
            self.cache.clone(),
        );
        let fetch = {
            let id = id.clone();
            relm4::spawn(async move { fetch(session, server, cache, id, size).await })
        };
        match fetch.await {
            Ok(Ok(bytes)) => Some((id, bytes)),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "no artwork");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "artwork fetch panicked");
                None
            }
        }
    }
}

/// The first artwork of `kinds`, in the order of `kinds`.
fn first_of(artwork: &[Artwork], kinds: &[ArtworkKind]) -> Option<ArtworkId> {
    kinds
        .iter()
        .find_map(|kind| artwork.iter().find(|a| a.kind == *kind))
        .map(|a| a.id.clone())
}

/// The image's bytes, from the device's cache or the server.
async fn fetch(
    session: Arc<dyn Session>,
    server: ServerKey,
    cache: Option<Arc<ArtworkCache>>,
    id: ArtworkId,
    size: ArtworkSize,
) -> Result<Vec<u8>, BackendError> {
    let network = {
        let id = id.clone();
        async move { session.artwork(&id, size).await }
    };
    match cache {
        Some(cache) => cache.get_or_fetch(&server, &id, size, network).await,
        None => network.await,
    }
}

/// Runs `bytes` on Relm4's tokio runtime, where the SDK's networking lives,
/// and paints the image over `tile` on the main loop. Decoded images are
/// not kept: tiles come and go as a grid scrolls, and decoding cached
/// bytes again is cheap next to holding every image seen.
fn load<F>(tile: &gtk::Overlay, bytes: F)
where
    F: std::future::Future<Output = Result<Vec<u8>, BackendError>> + Send + 'static,
{
    let fetch = relm4::spawn(bytes);
    let tile = tile.downgrade();
    glib::spawn_future_local(async move {
        let bytes = match fetch.await {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(e)) => return tracing::debug!(error = %e, "no artwork"),
            Err(e) => return tracing::warn!(error = %e, "artwork fetch panicked"),
        };
        let texture = match decode(&bytes) {
            Ok(texture) => texture,
            Err(e) => return tracing::warn!(error = %e, "artwork is not an image to show"),
        };
        if let Some(tile) = tile.upgrade() {
            paint(&tile, &texture);
        }
    });
}

/// Replaces the tile's placeholder with the image, at the placeholder's
/// size, and hides the placeholder's title. Overlays on the tile (progress,
/// watched badge) stay on top.
fn paint(tile: &gtk::Overlay, texture: &gdk::Texture) {
    let (width, height) = tile
        .child()
        .map_or((-1, -1), |c| (c.width_request(), c.height_request()));
    let picture = gtk::Picture::builder()
        .paintable(&Unsized::new(texture))
        .content_fit(gtk::ContentFit::Fill)
        .width_request(width)
        .height_request(height)
        .css_classes(["tile-image"])
        .build();
    tile.set_child(Some(&picture));
    let mut child = tile.first_child();
    while let Some(widget) = child {
        if widget.has_css_class("tile-title") {
            widget.set_visible(false);
        }
        child = widget.next_sibling();
    }
}

glib::wrapper! {
    /// A paintable that asks for no size of its own, only its shape. A
    /// picture is as large as its paintable by default, and width and height
    /// requests only set a minimum, so a full-size poster pushed the page
    /// out, and the live video filled the bar. Wrapped, the picture takes
    /// the size it is given and covers it. Changes to the wrapped paintable
    /// (each new video frame) pass through.
    pub struct Unsized(ObjectSubclass<imp::Unsized>) @implements gdk::Paintable;
}

impl Unsized {
    pub fn new(inner: &impl IsA<gdk::Paintable>) -> Self {
        let paintable: Self = glib::Object::new();
        let weak = paintable.downgrade();
        inner.connect_invalidate_contents(move |_| {
            if let Some(outer) = weak.upgrade() {
                outer.invalidate_contents();
            }
        });
        let weak = paintable.downgrade();
        inner.connect_invalidate_size(move |_| {
            if let Some(outer) = weak.upgrade() {
                outer.invalidate_size();
            }
        });
        paintable.imp().inner.replace(Some(inner.clone().upcast()));
        paintable
    }
}

/// Where to draw content of shape `aspect` (width over height) so it
/// covers a `width`×`height` area, centred and cropped: (x, y, w, h).
/// Without a shape, it fills the area.
fn cover(aspect: f64, width: f64, height: f64) -> (f64, f64, f64, f64) {
    if aspect <= 0.0 || height <= 0.0 {
        return (0.0, 0.0, width, height);
    }
    if width / height > aspect {
        let h = width / aspect;
        (0.0, (height - h) / 2.0, width, h)
    } else {
        let w = height * aspect;
        ((width - w) / 2.0, 0.0, w, height)
    }
}

mod imp {
    use std::cell::RefCell;

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{gdk, glib};

    #[derive(Default)]
    pub struct Unsized {
        pub inner: RefCell<Option<gdk::Paintable>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Unsized {
        const NAME: &'static str = "SpoolframeUnsized";
        type Type = super::Unsized;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Unsized {}

    impl PaintableImpl for Unsized {
        // No intrinsic size and no shape: a picture asks the paintable for
        // both, and with a shape alone it still sizes its height from its
        // width (a 970 px backdrop in a wide window). The cover crop is
        // done here instead.

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(inner) = self.inner.borrow().clone() else {
                return;
            };
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else {
                return inner.snapshot(snapshot, width, height);
            };
            let (x, y, w, h) = super::cover(inner.intrinsic_aspect_ratio(), width, height);
            snapshot.push_clip(&gtk::graphene::Rect::new(
                0.0,
                0.0,
                width as f32,
                height as f32,
            ));
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(x as f32, y as f32));
            inner.snapshot(snapshot, w, h);
            snapshot.restore();
            snapshot.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 PNG.
    const SMALL_PNG: [u8; 70] = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    /// 69 bytes of PNG whose header says 100 000 × 100 000: 40 GB of
    /// pixels once decoded.
    const BOMB_PNG: [u8; 69] = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 1, 134, 160, 0, 1, 134,
        160, 8, 6, 0, 0, 0, 168, 82, 11, 200, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 96, 160,
        12, 0, 0, 0, 64, 0, 1, 183, 52, 124, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[test]
    fn an_images_size_is_read_from_its_header() {
        assert_eq!(image_size(&BOMB_PNG), Some((100_000, 100_000)));
    }

    #[test]
    fn an_image_too_large_is_refused_before_it_is_decoded() {
        assert!(decode(&BOMB_PNG).is_err_and(|e| e.contains("too large")));
    }

    #[test]
    fn a_small_image_is_decoded() {
        assert!(decode(&SMALL_PNG).is_ok_and(|t| t.width() == 1));
    }

    #[test]
    fn bytes_that_are_no_image_are_refused() {
        assert!(decode(b"<html>not found</html>").is_err());
    }

    /// A 200×300 poster, all one colour.
    fn poster() -> gdk::Texture {
        let bytes = glib::Bytes::from_owned(vec![0u8; 200 * 300 * 4]);
        gdk::MemoryTexture::new(200, 300, gdk::MemoryFormat::R8g8b8a8, &bytes, 200 * 4).upcast()
    }

    #[test]
    fn a_wrapped_texture_asks_for_no_size() {
        let paintable = Unsized::new(&poster());
        assert_eq!(
            (paintable.intrinsic_width(), paintable.intrinsic_height()),
            (0, 0)
        );
    }

    #[test]
    fn a_wrapped_texture_has_no_shape_to_size_by() {
        let paintable = Unsized::new(&poster());
        assert_eq!(paintable.intrinsic_aspect_ratio(), 0.0);
    }

    #[test]
    fn a_tall_poster_covers_a_wide_area_cropped_top_and_bottom() {
        // 2:3 into 300×100: 300 wide, 450 tall, centred.
        assert_eq!(cover(2.0 / 3.0, 300.0, 100.0), (0.0, -175.0, 300.0, 450.0));
    }

    #[test]
    fn a_wide_backdrop_covers_a_tall_area_cropped_at_the_sides() {
        // 16:9 into 90×90: 160 wide, centred.
        assert_eq!(cover(16.0 / 9.0, 90.0, 90.0), (-35.0, 0.0, 160.0, 90.0));
    }

    #[test]
    fn a_change_to_the_wrapped_paintable_passes_through() {
        let inner = Unsized::new(&poster());
        let outer = Unsized::new(&inner);
        let heard = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let heard = heard.clone();
            outer.connect_invalidate_contents(move |_| heard.set(true));
        }
        inner.invalidate_contents();
        assert!(heard.get());
    }
}
