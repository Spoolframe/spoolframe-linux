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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use spoolframe_linux_core::backend::Liked;
use spoolframe_linux_core::playlists::{album_heart, heart_likes};
use spoolframe_types::MediaId;

/// Sends a like (`true`) or an un-like of `items`; on a refusal the app
/// gives `before` back to [`Hearts::learn`].
pub type Send = Rc<dyn Fn(Vec<MediaId>, bool, Vec<(MediaId, bool)>)>;

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

/// The hearts of a session: what is known of each track, and the hearts
/// shown for them.
pub struct Hearts {
    known: RefCell<HashMap<MediaId, bool>>,
    shown: RefCell<Vec<Shown>>,
    send: Send,
    ask: Ask,
}

impl Hearts {
    pub fn new(send: Send, ask: Ask) -> Rc<Self> {
        Rc::new(Hearts {
            known: RefCell::default(),
            shown: RefCell::default(),
            send,
            ask,
        })
    }

    /// What the server says of these tracks now; every heart showing one
    /// of them is drawn again.
    pub fn learn(&self, hearts: impl IntoIterator<Item = (MediaId, bool)>) {
        let changed: HashSet<MediaId> = {
            let mut known = self.known.borrow_mut();
            hearts
                .into_iter()
                .filter(|(id, liked)| known.insert(id.clone(), *liked) != Some(*liked))
                .map(|(id, _)| id)
                .collect()
        };
        if !changed.is_empty() {
            self.redraw(|tracks| tracks.iter().any(|t| changed.contains(t)));
        }
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
                && !hearts.known.borrow().contains_key(track)
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
            let likes = heart_likes(hearts.state(&tracks));
            let before: Vec<(MediaId, bool)> = {
                let known = hearts.known.borrow();
                tracks
                    .iter()
                    .map(|t| (t.clone(), known.get(t).copied().unwrap_or(false)))
                    .collect()
            };
            hearts.learn(tracks.iter().map(|t| (t.clone(), likes)));
            (hearts.send)(vec![item], likes, before);
        });
    }

    /// The heart for `tracks`, by what is known: a track not heard of
    /// counts as not liked.
    fn state(&self, tracks: &[MediaId]) -> Liked {
        let known = self.known.borrow();
        let liked: HashSet<MediaId> = tracks
            .iter()
            .filter(|t| known.get(*t).copied().unwrap_or(false))
            .cloned()
            .collect();
        album_heart(tracks, &liked)
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
