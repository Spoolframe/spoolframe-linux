//! Artwork, fetched through the session (P1-05), kept in the device's
//! artwork cache, and drawn over the coloured placeholder tiles. The
//! placeholder stays when an item has no artwork or the fetch fails, so a
//! missing image never leaves a hole.

use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use spoolframe_linux_core::artcache::ArtworkCache;
use spoolframe_linux_core::backend::{ArtworkSize, ServerKey, Session};
use spoolframe_types::{Artwork, ArtworkKind};

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
    /// Decoded images are not kept: tiles come and go as a grid scrolls,
    /// and decoding cached bytes again is cheap next to holding every
    /// image seen.
    pub fn show(
        self: &Rc<Self>,
        tile: &gtk::Overlay,
        artwork: &[Artwork],
        kinds: &[ArtworkKind],
        size: ArtworkSize,
    ) {
        let Some(id) = kinds
            .iter()
            .find_map(|kind| artwork.iter().find(|a| a.kind == *kind))
            .map(|a| a.id.clone())
        else {
            return;
        };
        // The fetch runs on Relm4's tokio runtime, where the SDK's
        // networking lives; drawing waits for it on the main loop.
        let (session, server, cache) = (
            self.session.clone(),
            self.server.clone(),
            self.cache.clone(),
        );
        let fetch = relm4::spawn(async move {
            let network = {
                let id = id.clone();
                async move { session.artwork(&id, size).await }
            };
            match cache {
                Some(cache) => cache.get_or_fetch(&server, &id, size, network).await,
                None => network.await,
            }
        });
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
}

/// Replaces the tile's placeholder with the image, at the placeholder's
/// size, and hides the placeholder's title. Overlays on the tile (progress,
/// watched badge) stay on top.
fn paint(tile: &gtk::Overlay, texture: &gdk::Texture) {
    let (width, height) = tile
        .child()
        .map_or((-1, -1), |c| (c.width_request(), c.height_request()));
    let picture = gtk::Picture::builder()
        .paintable(texture)
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
