//! Preview frames while scrubbing (P3-13): a frame above the seek bar
//! while it is hovered or dragged, and briefly after a keyboard seek.
//!
//! The server makes an item's frames into sheets of tiles. They are asked
//! for once per item (which also puts the item first in the server's
//! queue); while still being made they are asked for again every few
//! seconds, and an item without any is not asked again. Each sheet is
//! fetched once, at full size, and kept as pixels, so a frame is a copy of
//! one tile's rectangle.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};
use spoolframe_linux_core::backend::{ArtworkSize, BackendError, Previews, Session};
use spoolframe_linux_core::format;
use spoolframe_types::MediaId;

/// How long to wait before asking again for frames still being made;
/// less in tests.
const ASK_AGAIN: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(5)
};

/// How long a frame stays after a drag or a keyboard seek.
const LINGER: Duration = Duration::from_millis(1200);

/// The frame's width on screen, in pixels; the height follows the tiles'.
const FRAME_WIDTH: i32 = 224;

/// An item's frames, as far as known.
pub struct Frames {
    session: Arc<dyn Session>,
    item: RefCell<Option<MediaId>>,
    state: RefCell<State>,
    /// Bumped per item, so an answer for the last item is dropped.
    generation: Cell<u64>,
    /// Called when a sheet arrives, to draw the frame again.
    changed: RefCell<Option<Rc<dyn Fn()>>>,
}

enum State {
    /// Not asked yet, or asked and waiting.
    Unknown,
    /// None for this item: do not ask again.
    None,
    Ready {
        previews: Previews,
        sheets: Vec<Option<Sheet>>,
        fetching: HashSet<usize>,
    },
}

/// A sheet's pixels, B8G8R8A8 premultiplied, as GDK downloads them.
struct Sheet {
    width: usize,
    height: usize,
    stride: usize,
    pixels: Vec<u8>,
}

impl Frames {
    pub fn new(session: Arc<dyn Session>) -> Rc<Self> {
        Rc::new(Self {
            session,
            item: RefCell::new(None),
            state: RefCell::new(State::Unknown),
            generation: Cell::new(0),
            changed: RefCell::new(None),
        })
    }

    pub fn on_change(&self, changed: impl Fn() + 'static) {
        self.changed.replace(Some(Rc::new(changed)));
    }

    /// Starts on `item`'s frames, letting go of the last item's.
    pub fn for_item(self: &Rc<Self>, item: &MediaId) {
        if self.item.borrow().as_ref() == Some(item) {
            return;
        }
        self.item.replace(Some(item.clone()));
        self.state.replace(State::Unknown);
        self.generation.set(self.generation.get() + 1);
        self.ask(self.generation.get());
    }

    fn ask(self: &Rc<Self>, generation: u64) {
        let Some(item) = self.item.borrow().clone() else {
            return;
        };
        let session = self.session.clone();
        let asked = relm4::spawn(async move { session.previews(&item).await });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = asked.await else { return };
            let Some(frames) = weak.upgrade().filter(|f| f.generation.get() == generation) else {
                return;
            };
            match answer {
                Ok(previews) => {
                    let sheets = (0..previews.sheets.len()).map(|_| None).collect();
                    frames.state.replace(State::Ready {
                        previews,
                        sheets,
                        fetching: HashSet::new(),
                    });
                }
                Err(BackendError::PreviewsNotReady(done)) => {
                    tracing::debug!(done, "preview frames are still being made");
                    let weak = Rc::downgrade(&frames);
                    glib::timeout_add_local_once(ASK_AGAIN, move || {
                        if let Some(frames) = weak.upgrade()
                            && frames.generation.get() == generation
                        {
                            frames.ask(generation);
                        }
                    });
                }
                Err(BackendError::NoPreviews) => {
                    frames.state.replace(State::None);
                }
                Err(e) => {
                    tracing::debug!(error = %e, "no preview frames");
                    frames.state.replace(State::None);
                }
            }
        });
    }

    /// The frame at `position`, if its sheet is here; fetches the sheet
    /// otherwise, and says so through [`Frames::on_change`] when it is.
    pub fn frame(self: &Rc<Self>, position: Duration) -> Option<gdk::Texture> {
        let mut state = self.state.borrow_mut();
        let State::Ready {
            previews,
            sheets,
            fetching,
        } = &mut *state
        else {
            return None;
        };
        let tile = previews.tile(position.as_millis() as u64)?;
        match sheets.get(tile.sheet)? {
            Some(sheet) => {
                let scale = sheet.width as f64 / f64::from(previews.columns * previews.tile_width);
                let rect = scaled(&tile, scale);
                let (w, h, pixels) = crop(sheet, rect)?;
                Some(
                    gdk::MemoryTexture::new(
                        w as i32,
                        h as i32,
                        gdk::MemoryFormat::B8g8r8a8Premultiplied,
                        &glib::Bytes::from_owned(pixels),
                        w * 4,
                    )
                    .upcast(),
                )
            }
            None => {
                if fetching.insert(tile.sheet) {
                    let id = previews.sheets[tile.sheet].clone();
                    drop(state);
                    self.fetch(tile.sheet, id);
                }
                None
            }
        }
    }

    fn fetch(self: &Rc<Self>, index: usize, id: spoolframe_types::ArtworkId) {
        let generation = self.generation.get();
        let session = self.session.clone();
        let fetched =
            relm4::spawn(async move { session.artwork(&id, ArtworkSize::Original).await });
        let weak: Weak<Self> = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = fetched.await else { return };
            let Some(frames) = weak.upgrade().filter(|f| f.generation.get() == generation) else {
                return;
            };
            let sheet = answer
                .map_err(|e| e.to_string())
                .and_then(|bytes| decode(&bytes).map_err(|e| e.to_string()));
            match sheet {
                Ok(sheet) => {
                    if let State::Ready { sheets, .. } = &mut *frames.state.borrow_mut()
                        && let Some(slot) = sheets.get_mut(index)
                    {
                        *slot = Some(sheet);
                    }
                    let changed = frames.changed.borrow().clone();
                    if let Some(changed) = changed {
                        changed();
                    }
                }
                Err(e) => tracing::warn!(error = %e, "cannot load a preview sheet"),
            }
        });
    }
}

/// A sheet's image as pixels.
fn decode(bytes: &[u8]) -> Result<Sheet, glib::Error> {
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from(bytes))?;
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let stride = width * 4;
    let mut pixels = vec![0u8; stride * height];
    texture.download(&mut pixels, stride);
    Ok(Sheet {
        width,
        height,
        stride,
        pixels,
    })
}

/// A tile's rectangle in a sheet `scale` times its original size.
fn scaled(
    tile: &spoolframe_linux_core::backend::PreviewTile,
    scale: f64,
) -> (usize, usize, usize, usize) {
    let at = |v: u32| (f64::from(v) * scale).round() as usize;
    (
        at(tile.x),
        at(tile.y),
        at(tile.width).max(1),
        at(tile.height).max(1),
    )
}

/// The pixels of `(x, y, width, height)` in `sheet`, clipped to it.
fn crop(
    sheet: &Sheet,
    (x, y, w, h): (usize, usize, usize, usize),
) -> Option<(usize, usize, Vec<u8>)> {
    if x >= sheet.width || y >= sheet.height {
        return None;
    }
    let (w, h) = (w.min(sheet.width - x), h.min(sheet.height - y));
    let mut out = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        let start = row * sheet.stride + x * 4;
        out.extend_from_slice(&sheet.pixels[start..start + w * 4]);
    }
    Some((w, h, out))
}

/// The frame and the time, over the seek bar.
pub struct Preview {
    popover: gtk::Popover,
    picture: gtk::Picture,
    time: gtk::Label,
    frames: Rc<Frames>,
    scale: gtk::Scale,
    /// Bumped on each showing, so an older one's timer does not hide it.
    shown: Rc<Cell<u64>>,
    /// Where it shows, to draw the frame again when its sheet arrives.
    at: Cell<Option<Duration>>,
}

impl Preview {
    pub fn new(scale: &gtk::Scale, frames: Rc<Frames>) -> Rc<Self> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .width_request(FRAME_WIDTH)
            .height_request(FRAME_WIDTH * 9 / 16)
            .css_classes(["scrub-frame"])
            .visible(false)
            .build();
        let time = gtk::Label::builder()
            .css_classes(["numeric", "heading"])
            .build();
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        body.append(&picture);
        body.append(&time);
        let popover = gtk::Popover::builder()
            .child(&body)
            .position(gtk::PositionType::Top)
            .has_arrow(false)
            .autohide(false)
            .can_focus(false)
            .css_classes(["scrub-preview"])
            .build();
        popover.set_parent(scale);
        let preview = Rc::new(Self {
            popover,
            picture,
            time,
            frames,
            scale: scale.clone(),
            shown: Rc::new(Cell::new(0)),
            at: Cell::new(None),
        });
        let weak = Rc::downgrade(&preview);
        preview.frames.on_change(move || {
            if let Some(preview) = weak.upgrade()
                && let Some(at) = preview.at.get()
                && preview.popover.is_visible()
            {
                preview.draw(at);
            }
        });
        preview.follow_pointer();
        preview
    }

    /// Hovering shows the frame under the pointer; leaving hides it.
    fn follow_pointer(self: &Rc<Self>) {
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, x, _| {
            if let Some(preview) = weak.upgrade()
                && let Some(at) = preview.time_at(x)
            {
                preview.show_at(x, at);
                preview.shown.set(preview.shown.get() + 1);
            }
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(preview) = weak.upgrade() {
                preview.hide();
            }
        });
        self.scale.add_controller(motion);
    }

    /// Shows the frame at `position` for a moment: a drag, or a keyboard
    /// seek.
    pub fn flash(self: &Rc<Self>, position: Duration) {
        let Some(x) = self.x_of(position) else { return };
        self.show_at(x, position);
        let shown = self.shown.get() + 1;
        self.shown.set(shown);
        let (weak, count) = (Rc::downgrade(self), self.shown.clone());
        glib::timeout_add_local_once(LINGER, move || {
            if let Some(preview) = weak.upgrade()
                && count.get() == shown
            {
                preview.hide();
            }
        });
    }

    fn show_at(&self, x: f64, position: Duration) {
        self.at.set(Some(position));
        self.draw(position);
        self.popover
            .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, 0, 1, 1)));
        if !self.popover.is_visible() {
            self.popover.popup();
        }
    }

    fn draw(&self, position: Duration) {
        let frame = self.frames.frame(position);
        self.picture.set_paintable(frame.as_ref());
        self.picture.set_visible(frame.is_some());
        self.time.set_label(&format::timestamp(position));
    }

    fn hide(&self) {
        self.shown.set(self.shown.get() + 1);
        self.at.set(None);
        self.popover.popdown();
    }

    /// The time under `x` on the bar, within the item.
    fn time_at(&self, x: f64) -> Option<Duration> {
        let (width, upper) = (
            f64::from(self.scale.width()),
            self.scale.adjustment().upper(),
        );
        (width > 0.0 && upper > 0.0)
            .then(|| Duration::from_secs_f64((x / width).clamp(0.0, 1.0) * upper))
    }

    /// Where `position` is along the bar.
    fn x_of(&self, position: Duration) -> Option<f64> {
        let (width, upper) = (
            f64::from(self.scale.width()),
            self.scale.adjustment().upper(),
        );
        (width > 0.0 && upper > 0.0)
            .then(|| (position.as_secs_f64() / upper).clamp(0.0, 1.0) * width)
    }

    pub fn frames(&self) -> &Rc<Frames> {
        &self.frames
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.popover.unparent();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use spoolframe_linux_core::backend::{Client, PreviewTile};
    use spoolframe_linux_core::fake::{FakeClient, PreviewsAnswer};
    use spoolframe_types::{MediaKind, Namespace};

    use super::*;

    /// Runs the main loop until `done` or `limit`.
    fn run_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    fn frames(answer: PreviewsAnswer) -> Rc<Frames> {
        let client = FakeClient::new(None).with_previews(answer);
        Frames::new(client.session(&FakeClient::profile("andrey")).unwrap())
    }

    fn arrival() -> MediaId {
        MediaId::new(Namespace::Plex, MediaKind::Movie, "2").unwrap()
    }

    /// The colour of a texture's first pixel, as 0xRRGGBB.
    fn colour(texture: &gdk::Texture) -> u32 {
        let mut pixel = vec![0u8; texture.width() as usize * texture.height() as usize * 4];
        texture.download(&mut pixel, texture.width() as usize * 4);
        // B, G, R, A.
        u32::from_be_bytes([0, pixel[2], pixel[1], pixel[0]])
    }

    /// One test: the frames go through GLib's main loop.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn frames_on_the_fake_server() {
        gtk::init().unwrap();
        a_frame_comes_once_its_sheet_has();
        frames_still_being_made_are_asked_for_again();
        an_item_without_frames_has_none();
    }

    fn a_frame_comes_once_its_sheet_has() {
        let frames = frames(PreviewsAnswer::Ready);
        frames.for_item(&arrival());
        assert!(run_until(Duration::from_secs(5), || matches!(
            &*frames.state.borrow(),
            State::Ready { .. }
        )));
        // 1.5 s: tile 1, the green one, once the sheet is fetched.
        let at = Duration::from_millis(1500);
        assert!(frames.frame(at).is_none(), "the sheet was not fetched yet");
        let frame = Rc::new(RefCell::new(None));
        assert!(run_until(Duration::from_secs(5), || {
            frame.replace(frames.frame(at));
            frame.borrow().is_some()
        }));
        let frame = frame.borrow().clone().unwrap();
        assert_eq!(
            (frame.width(), frame.height(), colour(&frame)),
            (64, 36, 0xa6e3a1)
        );
    }

    fn frames_still_being_made_are_asked_for_again() {
        let frames = frames(PreviewsAnswer::NotReadyFor(2));
        frames.for_item(&arrival());
        assert!(run_until(Duration::from_secs(5), || matches!(
            &*frames.state.borrow(),
            State::Ready { .. }
        )));
    }

    fn an_item_without_frames_has_none() {
        let frames = frames(PreviewsAnswer::None);
        frames.for_item(&arrival());
        assert!(run_until(Duration::from_secs(5), || matches!(
            &*frames.state.borrow(),
            State::None
        )));
        assert!(frames.frame(Duration::from_secs(1)).is_none());
    }

    /// A 4 × 2 sheet of one-pixel tiles, each pixel its own index in every
    /// byte.
    fn sheet() -> Sheet {
        let (width, height) = (4, 2);
        let pixels = (0..width * height).flat_map(|i| [i as u8; 4]).collect();
        Sheet {
            width,
            height,
            stride: width * 4,
            pixels,
        }
    }

    #[test]
    fn a_tile_is_cut_from_its_place_in_the_sheet() {
        let (w, h, pixels) = crop(&sheet(), (1, 1, 2, 1)).unwrap();
        assert_eq!((w, h, pixels), (2, 1, vec![5, 5, 5, 5, 6, 6, 6, 6]));
    }

    #[test]
    fn a_tile_past_the_edge_is_clipped() {
        let (w, h, _) = crop(&sheet(), (3, 0, 4, 4)).unwrap();
        assert_eq!((w, h), (1, 2));
    }

    #[test]
    fn a_smaller_sheet_scales_the_rectangle() {
        let tile = PreviewTile {
            index: 3,
            sheet: 0,
            x: 320,
            y: 180,
            width: 320,
            height: 180,
        };
        assert_eq!(scaled(&tile, 0.5), (160, 90, 160, 90));
    }
}
