//! Small stand-ins for a server, for tests only. A [`MockSession`]
//! answers with what its test gave it and keeps what it was sent, so the
//! test checks the app's side: what it asked, and what it made of the
//! answer. Anything not given answers as the [`Session`] defaults do:
//! `Unsupported`. What a real server does is checked by hand, on one.
//!
//! The builders make the shapes tests need most, with the fewest fields
//! a test has to care about; a test changes the rest with `..`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use spoolframe_types::{Episode, Item, MediaId, MediaKind, Movie, Namespace, PlaybackState, Track};

use crate::backend::{
    BackendError, BoxFuture, EntryId, Kinds, PlaylistEdit, PlaylistEntry, PlaylistOwnerKind,
    PlaylistPage, PlaylistSummary, Session, Visibility,
};

/// A session answering from canned data.
#[derive(Default)]
pub struct MockSession {
    /// Each playlist's whole list, answered a slice at a time.
    playlists: Mutex<HashMap<String, PlaylistPage>>,
    /// What a playlist lists once an edit of it is answered `Ok`.
    after_edit: Mutex<HashMap<String, Vec<PlaylistEntry>>>,
    /// `edit_playlist`'s answers, in order.
    edit_answers: Mutex<VecDeque<Result<u64, BackendError>>>,
    edits: Mutex<Vec<Edit>>,
}

/// One `edit_playlist` the session was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub id: String,
    pub revision: u64,
    pub edits: Vec<PlaylistEdit>,
}

impl MockSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers `playlist` for `page`'s playlist from `page`: the entries
    /// from the offset asked, at most the limit asked; the rest as given.
    pub fn with_playlist(self, page: PlaylistPage) -> Self {
        self.playlists
            .lock()
            .unwrap()
            .insert(page.playlist.id.clone(), page);
        self
    }

    /// The next answers to `edit_playlist`, in order; past them,
    /// `Unsupported`. An `Ok` revision is the playlist's from then on.
    pub fn with_edit_answers(
        self,
        answers: impl IntoIterator<Item = Result<u64, BackendError>>,
    ) -> Self {
        self.edit_answers.lock().unwrap().extend(answers);
        self
    }

    /// What playlist `id` lists once an edit of it is answered `Ok`.
    pub fn with_entries_after_edit(self, id: &str, entries: Vec<PlaylistEntry>) -> Self {
        self.after_edit
            .lock()
            .unwrap()
            .insert(id.to_owned(), entries);
        self
    }

    /// Every `edit_playlist` sent, in order.
    pub fn edits(&self) -> Vec<Edit> {
        self.edits.lock().unwrap().clone()
    }
}

impl Session for MockSession {
    fn playlist<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<PlaylistPage, BackendError>> {
        let answer = match self.playlists.lock().unwrap().get(id) {
            Some(whole) => {
                let mut page = whole.clone();
                page.entries = whole
                    .entries
                    .iter()
                    .skip(offset as usize)
                    .take(limit as usize)
                    .cloned()
                    .collect();
                page.offset = offset;
                Ok(page)
            }
            None => Err(BackendError::NotFound(id.to_owned())),
        };
        Box::pin(async move { answer })
    }

    fn edit_playlist<'a>(
        &'a self,
        id: &'a str,
        revision: u64,
        edits: Vec<PlaylistEdit>,
    ) -> BoxFuture<'a, Result<u64, BackendError>> {
        self.edits.lock().unwrap().push(Edit {
            id: id.to_owned(),
            revision,
            edits,
        });
        let answer = self
            .edit_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(BackendError::Unsupported("edit_playlist".into())));
        if let Ok(revision) = answer
            && let Some(page) = self.playlists.lock().unwrap().get_mut(id)
        {
            page.playlist.revision = revision;
            if let Some(entries) = self.after_edit.lock().unwrap().remove(id) {
                page.total = entries.len() as u32;
                page.playable = entries.iter().filter(|e| !e.unavailable).count() as u32;
                page.entries = entries;
            }
        }
        Box::pin(async move { answer })
    }
}

/// A Plex id of `kind` and `key`.
pub fn media(kind: MediaKind, key: &str) -> MediaId {
    MediaId::new(Namespace::Plex, kind, key).unwrap()
}

/// Film `key`, never played.
pub fn movie(key: &str, title: &str) -> Item {
    Item::Movie(Movie {
        id: media(MediaKind::Movie, key),
        title: title.into(),
        year: None,
        release_date: None,
        summary: None,
        content_rating: None,
        duration: None,
        cast: Vec::new(),
        artwork: Vec::new(),
        playback: PlaybackState::default(),
    })
}

/// Episode `key`, `show`'s `season` × `number`, never played.
pub fn episode(key: &str, show: &str, season: u32, number: u32, title: &str) -> Item {
    Item::Episode(Episode {
        id: media(MediaKind::Episode, key),
        show_id: media(MediaKind::Show, show),
        show_title: Some(show.into()),
        season_id: media(MediaKind::Season, &format!("{show} {season}")),
        season,
        episode: number,
        title: title.into(),
        summary: None,
        content_rating: None,
        year: None,
        air_date: None,
        duration: None,
        cast: Vec::new(),
        artwork: Vec::new(),
        playback: PlaybackState::default(),
    })
}

/// Track `key`, of no album the test cares about, never played.
pub fn track(key: &str, title: &str) -> Item {
    Item::Track(Track {
        id: media(MediaKind::Track, key),
        album_id: media(MediaKind::Album, "album"),
        artist_id: media(MediaKind::Artist, "artist"),
        artist_name: None,
        album_title: None,
        title: title.into(),
        track: None,
        disc: None,
        album_track_count: None,
        codec: None,
        bit_depth: None,
        sample_rate: None,
        duration: None,
        artwork: Vec::new(),
        playback: PlaybackState::default(),
    })
}

/// The user's own playlist of `kinds`, personal, at revision 1: they may
/// edit and manage it.
pub fn playlist(id: &str, name: &str, kinds: Kinds) -> PlaylistSummary {
    PlaylistSummary {
        id: id.into(),
        name: name.into(),
        kinds,
        owner_kind: PlaylistOwnerKind::Mine,
        visibility: Visibility::Personal,
        can_edit: true,
        can_manage: true,
        revision: 1,
        pinned: None,
        builtin: None,
        owner_name: None,
        artwork: None,
        custom_art: false,
    }
}

/// `playlist` holding `items`, whole: entries 1, 2, … in order, each
/// playable, and no place kept.
pub fn page(playlist: PlaylistSummary, items: Vec<Item>) -> PlaylistPage {
    let entries: Vec<PlaylistEntry> = items
        .into_iter()
        .zip(1..)
        .map(|(item, entry)| entry_of(entry, item))
        .collect();
    PlaylistPage {
        playlist,
        offset: 0,
        total: entries.len() as u32,
        playable: entries.len() as u32,
        entries,
        place: None,
        settings: Default::default(),
    }
}

/// Entry `entry`, of `item`, playable and not liked.
pub fn entry_of(entry: EntryId, item: Item) -> PlaylistEntry {
    PlaylistEntry {
        entry,
        item,
        unavailable: false,
        liked: false,
    }
}
pub mod accounts;
pub mod playback;
pub mod shell;
