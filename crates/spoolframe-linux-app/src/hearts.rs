//! Hearts (PX-43, board LinuxHearts): a heart on each track row, on an
//! album's page (full, half or empty) and in the now-playing bar. A tap
//! likes or un-likes at once on screen and then on the server; every
//! heart standing for the same track follows, and a refusal puts them
//! back.
//!
//! The server says whose hearts are full in the replies the app already
//! reads (an album's details, a playlist's entries, the entry playing),
//! so the app learns them as it goes; only the bar, for a track played
//! from elsewhere, asks.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use spoolframe_client_sdk::{LikeCache, Pending};
use spoolframe_linux_core::backend::Liked;
use spoolframe_types::MediaId;

/// Sends the tap on `items` (a like when [`Pending::likes`]); on a
/// refusal the app gives it back to [`Hearts::refused`].
pub type Send = Rc<dyn Fn(Vec<MediaId>, Pending)>;

/// Asks the server for a track's heart, which comes back by
/// [`Hearts::learn`].
pub type Ask = Rc<dyn Fn(MediaId)>;

/// Points the bar's heart at the track playing, or hides it (`None`).
pub type Follow = Rc<dyn Fn(Option<MediaId>)>;

/// One heart shown, and the tracks it stands for: one, or an album's.
struct Shown {
    button: glib::WeakRef<gtk::Button>,
    tracks: Rc<RefCell<Vec<MediaId>>>,
}

/// The hearts of a session: what is known of each track (the SDK's
/// cache), and the hearts shown for them.
pub struct Hearts {
    known: LikeCache,
    shown: RefCell<Vec<Shown>>,
    send: Send,
    ask: Ask,
}

impl Hearts {
    pub fn new(send: Send, ask: Ask) -> Rc<Self> {
        Rc::new(Hearts {
            known: LikeCache::default(),
            shown: RefCell::default(),
            send,
            ask,
        })
    }

    /// What the server says of these tracks now; every heart showing one
    /// of them is drawn again.
    pub fn learn(&self, hearts: impl IntoIterator<Item = (MediaId, bool)>) {
        let changed: Vec<(MediaId, bool)> = hearts
            .into_iter()
            .filter(|(id, liked)| self.known.liked(id) != Some(*liked))
            .collect();
        if changed.is_empty() {
            return;
        }
        let ids: Vec<MediaId> = changed.iter().map(|(id, _)| id.clone()).collect();
        self.known.learn(changed);
        self.redraw(|tracks| tracks.iter().any(|t| ids.contains(t)));
    }

    /// The server refused a tap: its hearts go back as they were.
    pub fn refused(&self, tap: Pending) {
        tap.rollback(&self.known);
        // Which tracks it held is the SDK's: every heart is drawn again.
        self.redraw(|_| true);
    }

    /// A track row's heart.
    pub fn track(self: &Rc<Self>, track: &MediaId) -> gtk::Button {
        let button = heart_button(&["flat", "heart"]);
        self.show(&button, vec![track.clone()], Some(track.clone()));
        button
    }

    /// An album's heart, beside its Play: full when every one of
    /// `tracks` is liked, half when some are. A tap on a full one
    /// un-likes the album, on any other likes it.
    pub fn album(self: &Rc<Self>, album: &MediaId, tracks: Vec<MediaId>) -> gtk::Button {
        let button = heart_button(&["action", "outlined", "square-action", "heart"]);
        self.show(&button, tracks, Some(album.clone()));
        button
    }

    /// The bar's heart, for whatever track plays: `follow` points it at
    /// one, or hides it (`None`, for video). One not known yet is asked
    /// for.
    pub fn following(self: &Rc<Self>) -> (gtk::Button, Follow) {
        let button = heart_button(&["flat", "heart"]);
        let tracks: Rc<RefCell<Vec<MediaId>>> = Rc::default();
        let sends: Rc<RefCell<Option<MediaId>>> = Rc::default();
        self.wire(&button, &tracks, &sends);
        self.shown.borrow_mut().push(Shown {
            button: button.downgrade(),
            tracks: tracks.clone(),
        });
        let (this, weak) = (Rc::downgrade(self), button.downgrade());
        let follow = Rc::new(move |track: Option<MediaId>| {
            let (Some(hearts), Some(button)) = (this.upgrade(), weak.upgrade()) else {
                return;
            };
            button.set_visible(track.is_some());
            *tracks.borrow_mut() = track.iter().cloned().collect();
            *sends.borrow_mut() = track.clone();
            if let Some(track) = &track
                && hearts.known.liked(track).is_none()
            {
                (hearts.ask)(track.clone());
            }
            draw(&button, hearts.state(&tracks.borrow()));
        });
        (button, follow)
    }

    /// Shows `button` as the heart of `tracks`; a tap sends `sends`, the
    /// track or the album, which the server expands.
    fn show(self: &Rc<Self>, button: &gtk::Button, tracks: Vec<MediaId>, sends: Option<MediaId>) {
        let tracks = Rc::new(RefCell::new(tracks));
        let sends = Rc::new(RefCell::new(sends));
        self.wire(button, &tracks, &sends);
        draw(button, self.state(&tracks.borrow()));
        let mut shown = self.shown.borrow_mut();
        shown.retain(|s| s.button.upgrade().is_some());
        shown.push(Shown {
            button: button.downgrade(),
            tracks,
        });
    }

    /// A tap: shown at once, then sent.
    fn wire(
        self: &Rc<Self>,
        button: &gtk::Button,
        tracks: &Rc<RefCell<Vec<MediaId>>>,
        sends: &Rc<RefCell<Option<MediaId>>>,
    ) {
        let (this, tracks, sends) = (Rc::downgrade(self), tracks.clone(), sends.clone());
        button.connect_clicked(move |_| {
            let Some(hearts) = this.upgrade() else { return };
            let Some(item) = sends.borrow().clone() else {
                return;
            };
            let tracks = tracks.borrow().clone();
            let tap = hearts.known.toggle(&tracks);
            hearts.redraw(|shown| shown.iter().any(|t| tracks.contains(t)));
            (hearts.send)(vec![item], tap);
        });
    }

    /// The heart for `tracks`, by what is known: a track not heard of
    /// counts as not liked.
    fn state(&self, tracks: &[MediaId]) -> Liked {
        self.known.heart(tracks)
    }

    fn redraw(&self, affected: impl Fn(&[MediaId]) -> bool) {
        let shown: Vec<(gtk::Button, Vec<MediaId>)> = self
            .shown
            .borrow()
            .iter()
            .filter_map(|s| Some((s.button.upgrade()?, s.tracks.borrow().clone())))
            .collect();
        for (button, tracks) in shown {
            if affected(&tracks) {
                draw(&button, self.state(&tracks));
            }
        }
    }
}

fn heart_button(classes: &[&str]) -> gtk::Button {
    gtk::Button::builder()
        .valign(gtk::Align::Center)
        .css_classes(classes)
        .build()
}

/// A heart as `liked` has it: full and in the heart's colour, half, or
/// an empty outline, with what a tap does.
fn draw(button: &gtk::Button, liked: Liked) {
    let (icon, tooltip) = match liked {
        Liked::All => (crate::icons::HEART_FULL, "Unlike"),
        Liked::Some => (crate::icons::HEART_HALF, "Like the rest"),
        Liked::None | Liked::Unknown => (crate::icons::HEART, "Like"),
    };
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    if matches!(liked, Liked::None | Liked::Unknown) {
        button.remove_css_class("liked");
    } else {
        button.add_css_class("liked");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spoolframe_types::{MediaKind, Namespace};

    fn track(key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, MediaKind::Track, key).unwrap()
    }

    /// A tap fills every heart of the track at once; the server's refusal
    /// empties them all again.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_refused_like_puts_every_heart_of_the_track_back() {
        gtk::init().unwrap();
        let sent: Rc<RefCell<Option<Pending>>> = Rc::default();
        let hearts = {
            let sent = sent.clone();
            Hearts::new(
                Rc::new(move |_, tap| *sent.borrow_mut() = Some(tap)),
                Rc::new(|_| {}),
            )
        };
        let one = track("1");
        let (row, album) = (
            hearts.track(&one),
            hearts.album(&track("album"), vec![one.clone()]),
        );
        row.emit_clicked();
        assert!(
            row.has_css_class("liked") && album.has_css_class("liked"),
            "the tap is not shown on both hearts"
        );
        hearts.refused(sent.take().expect("the tap is sent"));
        assert!(
            !row.has_css_class("liked") && !album.has_css_class("liked"),
            "the refusal did not empty both hearts"
        );
    }
}
