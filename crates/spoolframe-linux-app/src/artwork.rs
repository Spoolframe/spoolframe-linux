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
        let texture = match gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) {
            Ok(texture) => texture,
            Err(e) => return tracing::warn!(error = %e, "artwork is not an image"),
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
        .content_fit(gtk::ContentFit::Cover)
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
    /// A texture that asks for no size of its own, only its shape. A
    /// picture is as large as its paintable by default, and width and height
    /// requests only set a minimum, so a full-size poster pushed the page
    /// out. Wrapped, the picture takes the placeholder's size and covers
    /// it, cropping the image.
    pub struct Unsized(ObjectSubclass<imp::Unsized>) @implements gdk::Paintable;
}

impl Unsized {
    fn new(texture: &gdk::Texture) -> Self {
        let paintable: Self = glib::Object::new();
        paintable.imp().texture.replace(Some(texture.clone()));
        paintable
    }
}

mod imp {
    use std::cell::RefCell;

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{gdk, glib};

    #[derive(Default)]
    pub struct Unsized {
        pub texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Unsized {
        const NAME: &'static str = "SpoolframeUnsizedTexture";
        type Type = super::Unsized;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Unsized {}

    impl PaintableImpl for Unsized {
        fn intrinsic_aspect_ratio(&self) -> f64 {
            self.texture
                .borrow()
                .as_ref()
                .map_or(0.0, |t| f64::from(t.width()) / f64::from(t.height().max(1)))
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            if let Some(texture) = self.texture.borrow().as_ref() {
                texture.snapshot(snapshot, width, height);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_wrapped_texture_keeps_its_shape() {
        let paintable = Unsized::new(&poster());
        assert!((paintable.intrinsic_aspect_ratio() - 2.0 / 3.0).abs() < 1e-9);
    }
}
