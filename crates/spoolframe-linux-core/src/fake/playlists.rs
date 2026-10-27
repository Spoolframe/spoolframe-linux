//! The fake server's playlists (PX-40), kept for every user of the fake
//! server: what each user sees of them, their pins and pin notes, their
//! place and play settings in each, and the events another device would
//! hear. The rules are the server's as the protocol states them: kinds
//! only widen, an edit on an old revision is redone unless it touches an
//! entry changed since, and a pin that goes away leaves a note once.
//! Each user also has Liked and All (PX-43), listed first, made the first
//! time the user is seen: Liked changes by likes and by removes and
//! moves, All is every track and read-only, and neither is renamed,
//! shared or deleted.

use std::collections::{HashMap, HashSet};

use spoolframe_policy::playlists as policy;
use spoolframe_types::{ArtworkId, Item, MediaId};
use tokio::sync::broadcast;

use crate::backend::{
    BackendError, Builtin, EntryId, ItemKind, Kinds, PinNote, PinNoteReason, PlaySettings,
    PlaylistChange, PlaylistEdit, PlaylistEvent, PlaylistList, PlaylistOwnerKind, PlaylistRef,
    PlaylistShare, PlaylistSummary, PlaylistUpdate, Repeat, ShareCandidate, ShareCandidatePage,
    Visibility,
};

pub use spoolframe_protocol::playlists::{MAX_ART_BYTES, MAX_ART_PIXELS};

/// A playlist's picture as the server names it (PX-53): its uploaded
/// cover, Liked's and All's tiles, a placeholder while it holds nothing,
/// and else a mosaic of its first covers, named by what it holds so it
/// changes with them.
fn art_of(stored: &Stored) -> ArtworkId {
    let key = match (&stored.cover, stored.builtin) {
        (Some(cover), _) => format!("playlist/{cover}"),
        (None, Some(Builtin::Liked)) => "tile/liked/2".to_owned(),
        (None, Some(_)) => "tile/all/2".to_owned(),
        (None, None) if stored.entries.is_empty() => "tile/empty/1".to_owned(),
        (None, None) => {
            let mut hasher = std::hash::DefaultHasher::new();
            for (_, item) in stored.entries.iter().take(4) {
                std::hash::Hash::hash(&item.to_string(), &mut hasher);
            }
            format!("mosaic/{:016x}", std::hash::Hasher::finish(&hasher))
        }
    };
    ArtworkId::new(spoolframe_types::Namespace::Image, &key).expect("a valid key")
}

/// Whether `bytes` start as a JPEG, PNG, WebP or HEIC does: the fake's
/// stand-in for the server's decode.
fn looks_like_an_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"\x89PNG")
        || (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        || (bytes.len() >= 12 && &bytes[4..8] == b"ftyp")
}

/// The most share candidates one page gives, as on the server.
pub const PEOPLE_LIMIT: u32 = 200;

/// The most items one like or unlike takes, as on the server.
pub const LIKE_LIMIT: usize = 100;

/// The most entries one `GetPlaylist` gives, as on the server.
pub const PAGE_LIMIT: u32 = 200;

/// One playlist as the server keeps it.
#[derive(Debug, Clone)]
pub(super) struct Stored {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub kinds: Kinds,
    pub visibility: Visibility,
    pub revision: u64,
    pub entries: Vec<(EntryId, MediaId)>,
    /// The entries each revision touched, for redoing an edit made on an
    /// older one.
    pub touched: Vec<(u64, Vec<EntryId>)>,
    /// Liked or All (PX-43), its owner's alone; `None` for one a user made.
    pub builtin: Option<Builtin>,
    /// The cover an editor uploaded (PX-53), by its key: what its
    /// `image:playlist/<key>` names.
    pub cover: Option<String>,
}

/// Who a user is to the fake server.
#[derive(Debug, Clone, Copy)]
pub(super) struct Viewer<'a> {
    pub username: &'a str,
    pub admin: bool,
}

/// What a user may do with a playlist they see.
struct Access {
    owner_kind: PlaylistOwnerKind,
    can_edit: bool,
    can_manage: bool,
}

pub(super) struct Store {
    server: String,
    next_id: u64,
    next_entry: EntryId,
    pub playlists: Vec<Stored>,
    /// Each user's Pinned, in their order.
    pins: HashMap<String, Vec<String>>,
    notes: HashMap<String, Vec<PinNote>>,
    /// Each user's place and settings in each playlist.
    places: HashMap<(String, String), EntryId>,
    settings: HashMap<(String, String), PlaySettings>,
    /// Each user's shuffled round in each playlist, as the server keeps
    /// it: the order this pass through it plays in.
    rounds: HashMap<(String, String), Vec<EntryId>>,
    /// Items still in the library whose file cannot be read now.
    pub unavailable: HashSet<MediaId>,
    /// Every track in the library, in order: what each user's All holds.
    pub all_tracks: Vec<MediaId>,
    /// Everyone a playlist can be shared with, by display name: the
    /// accounts here and the portable users granted access.
    pub people: Vec<ShareCandidate>,
    events: broadcast::Sender<PlaylistEvent>,
}

impl Store {
    pub fn new(server: &str) -> Self {
        Store {
            server: server.to_owned(),
            next_id: 1,
            next_entry: 1,
            playlists: Vec::new(),
            pins: HashMap::new(),
            notes: HashMap::new(),
            places: HashMap::new(),
            settings: HashMap::new(),
            rounds: HashMap::new(),
            unavailable: HashSet::new(),
            all_tracks: Vec::new(),
            people: Vec::new(),
            events: broadcast::channel(64).0,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PlaylistEvent> {
        self.events.subscribe()
    }

    fn tell(&self, change: PlaylistChange) {
        // Nobody listening is fine: nothing is shown.
        let _ = self.events.send(PlaylistEvent {
            server: self.server.clone(),
            change,
        });
    }

    /// Makes `username`'s Liked (empty) and All the first time they are
    /// seen: the server has them for every user.
    pub fn builtins_for(&mut self, username: &str) {
        let has = self
            .playlists
            .iter()
            .any(|p| p.owner == username && p.builtin.is_some());
        if has {
            return;
        }
        let tracks = Kinds {
            tracks: true,
            ..Kinds::default()
        };
        for (builtin, name) in [(Builtin::Liked, "Liked"), (Builtin::All, "All")] {
            let id = self.make(username, name, tracks, Visibility::Personal);
            let items = match builtin {
                Builtin::All => self.all_tracks.clone(),
                _ => Vec::new(),
            };
            let entries = items
                .into_iter()
                .map(|item| {
                    self.next_entry += 1;
                    (self.next_entry - 1, item)
                })
                .collect();
            let stored = self.stored_mut(&id).expect("just made");
            stored.builtin = Some(builtin);
            stored.entries = entries;
        }
    }

    fn liked_mut(&mut self, username: &str) -> &mut Stored {
        self.playlists
            .iter_mut()
            .find(|p| p.owner == username && p.builtin == Some(Builtin::Liked))
            .expect("made with the user's builtins")
    }

    /// The tracks in `username`'s Liked.
    pub fn liked(&self, username: &str) -> HashSet<MediaId> {
        self.playlists
            .iter()
            .filter(|p| p.owner == username && p.builtin == Some(Builtin::Liked))
            .flat_map(|p| p.entries.iter().map(|(_, item)| item.clone()))
            .collect()
    }

    /// Likes `items`, each a track or an album already expanded to its
    /// tracks by `expand`: those not liked yet go to the top, in order.
    pub fn like(
        &mut self,
        items: &[MediaId],
        viewer: Viewer<'_>,
        expand: impl Fn(&MediaId) -> Result<Vec<Item>, BackendError>,
    ) -> Result<PlaylistSummary, BackendError> {
        let tracks = likeable(items, expand)?;
        let liked = self.liked(viewer.username);
        let mut fresh: Vec<MediaId> = Vec::new();
        for track in tracks {
            if !liked.contains(&track) && !fresh.contains(&track) {
                fresh.push(track);
            }
        }
        let made: Vec<(EntryId, MediaId)> = fresh
            .into_iter()
            .map(|item| {
                self.next_entry += 1;
                (self.next_entry - 1, item)
            })
            .collect();
        let stored = self.liked_mut(viewer.username);
        if !made.is_empty() {
            stored.entries.splice(0..0, made);
            stored.revision += 1;
        }
        let id = stored.id.clone();
        self.liked_changed(&id, viewer)
    }

    /// Takes `items` (tracks, or albums' tracks) out of `username`'s Liked.
    pub fn unlike(
        &mut self,
        items: &[MediaId],
        viewer: Viewer<'_>,
        expand: impl Fn(&MediaId) -> Result<Vec<Item>, BackendError>,
    ) -> Result<PlaylistSummary, BackendError> {
        let tracks: HashSet<MediaId> = likeable(items, expand)?.into_iter().collect();
        let stored = self.liked_mut(viewer.username);
        let before = stored.entries.len();
        stored.entries.retain(|(_, item)| !tracks.contains(item));
        if stored.entries.len() != before {
            stored.revision += 1;
        }
        let id = stored.id.clone();
        self.liked_changed(&id, viewer)
    }

    /// Liked as it is now, told to the user's other devices.
    fn liked_changed(
        &mut self,
        id: &str,
        viewer: Viewer<'_>,
    ) -> Result<PlaylistSummary, BackendError> {
        let (summary, _) = self.seen(id, viewer)?;
        self.tell(PlaylistChange::Changed {
            id: id.to_owned(),
            revision: summary.revision,
        });
        Ok(summary)
    }

    /// A playlist the fixtures start with, with its entries.
    pub fn seed(
        &mut self,
        owner: &str,
        name: &str,
        kinds: Kinds,
        visibility: Visibility,
        items: &[MediaId],
    ) -> String {
        let id = self.make(owner, name, kinds, visibility);
        let mut entries = Vec::new();
        for item in items {
            entries.push((self.next_entry, item.clone()));
            self.next_entry += 1;
        }
        self.stored_mut(&id).expect("just made").entries = entries;
        id
    }

    fn make(&mut self, owner: &str, name: &str, kinds: Kinds, visibility: Visibility) -> String {
        let id = format!("pl{}", self.next_id);
        self.next_id += 1;
        self.playlists.push(Stored {
            id: id.clone(),
            name: name.to_owned(),
            owner: owner.to_owned(),
            kinds,
            visibility,
            revision: 1,
            entries: Vec::new(),
            touched: Vec::new(),
            builtin: None,
            cover: None,
        });
        id
    }

    fn stored_mut(&mut self, id: &str) -> Option<&mut Stored> {
        self.playlists.iter_mut().find(|p| p.id == id)
    }

    fn access(stored: &Stored, viewer: Viewer<'_>) -> Option<Access> {
        if let Visibility::ServerWide = stored.visibility {
            return Some(Access {
                owner_kind: PlaylistOwnerKind::ServerWide,
                can_edit: viewer.admin,
                can_manage: viewer.admin,
            });
        }
        if stored.owner == viewer.username {
            // Liked takes removes and moves; All nothing. Neither is
            // renamed, shared or deleted.
            return Some(Access {
                owner_kind: PlaylistOwnerKind::Mine,
                can_edit: stored.builtin != Some(Builtin::All),
                can_manage: stored.builtin.is_none(),
            });
        }
        match &stored.visibility {
            Visibility::Shared { with } => with
                .iter()
                .find(|share| share.user_id == user_id(viewer.username))
                .map(|share| Access {
                    owner_kind: PlaylistOwnerKind::SharedWithMe,
                    can_edit: share.can_edit,
                    can_manage: false,
                }),
            _ => None,
        }
    }

    fn summary(&self, stored: &Stored, viewer: Viewer<'_>) -> Option<PlaylistSummary> {
        let access = Self::access(stored, viewer)?;
        let visibility = match &stored.visibility {
            // Listed only to whoever manages it.
            Visibility::Shared { .. } if !access.can_manage => Visibility::Shared { with: vec![] },
            // With their names and kinds.
            Visibility::Shared { with } => Visibility::Shared {
                with: with.iter().map(|share| self.named(share)).collect(),
            },
            other => other.clone(),
        };
        let owner_name = self
            .person(&user_id(&stored.owner))
            .map(|p| p.display_name.clone());
        let pinned = self
            .pins
            .get(viewer.username)
            .and_then(|pins| pins.iter().position(|p| p == &stored.id))
            .map(|at| at as u32);
        let (artwork, custom_art) = (Some(art_of(stored)), stored.cover.is_some());
        Some(PlaylistSummary {
            id: stored.id.clone(),
            name: stored.name.clone(),
            kinds: stored.kinds,
            owner_kind: access.owner_kind,
            visibility,
            can_edit: access.can_edit,
            can_manage: access.can_manage,
            revision: stored.revision,
            pinned,
            builtin: stored.builtin,
            owner_name,
            artwork,
            custom_art,
        })
    }

    fn person(&self, user_id: &str) -> Option<&ShareCandidate> {
        self.people.iter().find(|p| p.user_id == user_id)
    }

    /// `share` with the name and kind of whom it is with; none for
    /// someone no longer here.
    fn named(&self, share: &PlaylistShare) -> PlaylistShare {
        let person = self.person(&share.user_id);
        PlaylistShare {
            display_name: person.map(|p| p.display_name.clone()),
            kind: person.map(|p| p.kind),
            ..share.clone()
        }
    }

    /// Whom playlist `id` can be shared with: everyone but `viewer`, by
    /// display name, those whose name or username holds `query` in any
    /// case. Only for whoever manages it.
    pub fn candidates(
        &self,
        id: &str,
        query: &str,
        offset: u32,
        limit: u32,
        viewer: Viewer<'_>,
    ) -> Result<ShareCandidatePage, BackendError> {
        let (summary, _) = self.seen(id, viewer)?;
        if !summary.can_manage {
            return Err(BackendError::PlaylistReadOnly);
        }
        let query = query.to_lowercase();
        let me = user_id(viewer.username);
        let holds = |text: &str| text.to_lowercase().contains(&query);
        let mut matching: Vec<&ShareCandidate> = self
            .people
            .iter()
            .filter(|p| p.user_id != me)
            .filter(|p| holds(&p.display_name) || p.username.as_deref().is_some_and(holds))
            .collect();
        matching.sort_by_key(|p| p.display_name.to_lowercase());
        Ok(ShareCandidatePage {
            total: matching.len() as u32,
            candidates: matching
                .into_iter()
                .skip(offset as usize)
                .take(limit.min(PEOPLE_LIMIT) as usize)
                .cloned()
                .collect(),
            offset,
        })
    }

    /// The playlist as `viewer` sees it, or `NotFound` when they do not.
    pub fn seen(
        &self,
        id: &str,
        viewer: Viewer<'_>,
    ) -> Result<(PlaylistSummary, &Stored), BackendError> {
        self.playlists
            .iter()
            .find(|p| p.id == id)
            .and_then(|stored| Some((self.summary(stored, viewer)?, stored)))
            .ok_or_else(|| BackendError::NotFound(format!("playlist {id}")))
    }

    pub fn list(&self, viewer: Viewer<'_>) -> PlaylistList {
        let mut playlists: Vec<PlaylistSummary> = self
            .playlists
            .iter()
            .filter_map(|p| self.summary(p, viewer))
            .collect();
        // Liked, then All, then the rest in the order they were made.
        playlists.sort_by_key(|p| match p.builtin {
            Some(Builtin::Liked) => 0,
            Some(_) => 1,
            None => 2,
        });
        PlaylistList {
            playlists,
            pin_notes: self.notes.get(viewer.username).cloned().unwrap_or_default(),
        }
    }

    /// The playlists `viewer` sees holding any of `items`.
    /// All is never among them: it holds every track.
    pub fn holding(&self, items: &HashSet<MediaId>, viewer: Viewer<'_>) -> Vec<PlaylistRef> {
        let mut refs: Vec<PlaylistRef> = self
            .playlists
            .iter()
            .filter(|p| p.builtin != Some(Builtin::All))
            .filter(|p| p.entries.iter().any(|(_, item)| items.contains(item)))
            .filter_map(|p| self.summary(p, viewer))
            .map(|s| PlaylistRef {
                id: s.id,
                name: s.name,
                owner_kind: s.owner_kind,
                builtin: s.builtin,
                owner_name: s.owner_name,
                artwork: s.artwork,
            })
            .collect();
        refs.sort_by_key(|r| r.builtin.is_none());
        refs
    }

    pub fn place(&self, id: &str, viewer: Viewer<'_>) -> (Option<EntryId>, PlaySettings) {
        let key = (viewer.username.to_owned(), id.to_owned());
        (
            self.places.get(&key).copied(),
            self.settings.get(&key).copied().unwrap_or_default(),
        )
    }

    pub fn create(
        &mut self,
        name: &str,
        kinds: Kinds,
        visibility: Visibility,
        viewer: Viewer<'_>,
    ) -> Result<PlaylistSummary, BackendError> {
        if name.trim().is_empty() {
            return Err(BackendError::InvalidInput("a playlist needs a name".into()));
        }
        if kinds.is_empty() {
            return Err(BackendError::InvalidInput(
                "a playlist holds at least one kind".into(),
            ));
        }
        if matches!(visibility, Visibility::ServerWide) && !viewer.admin {
            return Err(BackendError::AdminOnly);
        }
        let id = self.make(viewer.username, name.trim(), kinds, visibility);
        let (summary, stored) = self.seen(&id, viewer)?;
        self.tell(PlaylistChange::Changed {
            id: id.clone(),
            revision: stored.revision,
        });
        Ok(summary)
    }

    /// Gives playlist `id` the picture `bytes` as its cover (PX-53), as
    /// the server does: whoever may edit its entries may, never on Liked
    /// or All; at most [`MAX_ART_BYTES`], and an image by its header.
    /// Its revision goes up, and everyone who sees it is told.
    pub fn set_art(
        &mut self,
        id: &str,
        bytes: &[u8],
        viewer: Viewer<'_>,
    ) -> Result<ArtworkId, BackendError> {
        self.editable_art(id, viewer)?;
        if bytes.len() as u64 > MAX_ART_BYTES {
            return Err(BackendError::ImageTooLarge {
                max_bytes: MAX_ART_BYTES,
                max_pixels: MAX_ART_PIXELS,
            });
        }
        if !looks_like_an_image(bytes) {
            return Err(BackendError::ImageUnreadable);
        }
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(bytes, &mut hasher);
        let stored = self.stored_mut(id).expect("seen");
        stored.cover = Some(format!("{:016x}", std::hash::Hasher::finish(&hasher)));
        let artwork = art_of(stored);
        self.changed(id);
        Ok(artwork)
    }

    /// Takes playlist `id`'s uploaded cover away (PX-53): its mosaic
    /// again. Who may is as for [`Store::set_art`].
    pub fn remove_art(
        &mut self,
        id: &str,
        viewer: Viewer<'_>,
    ) -> Result<PlaylistSummary, BackendError> {
        self.editable_art(id, viewer)?;
        self.stored_mut(id).expect("seen").cover = None;
        self.changed(id);
        Ok(self.seen(id, viewer)?.0)
    }

    /// Refuses a cover change on a playlist `viewer` does not see; or
    /// from anyone who may not edit it, and on
    /// Liked and All, `NotAllowed`.
    fn editable_art(&self, id: &str, viewer: Viewer<'_>) -> Result<(), BackendError> {
        let (summary, stored) = self.seen(id, viewer)?;
        if !summary.can_edit || stored.builtin.is_some() {
            return Err(BackendError::NotAllowed);
        }
        Ok(())
    }

    /// Playlist `id` changed: a new revision, told to everyone.
    fn changed(&mut self, id: &str) {
        let stored = self.stored_mut(id).expect("seen");
        stored.revision += 1;
        let revision = stored.revision;
        self.tell(PlaylistChange::Changed {
            id: id.to_owned(),
            revision,
        });
    }

    pub fn update(
        &mut self,
        id: &str,
        update: PlaylistUpdate,
        viewer: Viewer<'_>,
    ) -> Result<PlaylistSummary, BackendError> {
        let (summary, _) = self.seen(id, viewer)?;
        if !summary.can_manage {
            return Err(BackendError::PlaylistReadOnly);
        }
        if let Some(kinds) = update.kinds
            && (!summary.kinds.widen_only(kinds) || kinds.is_empty())
        {
            return Err(BackendError::InvalidInput(
                "a playlist's kinds only widen".into(),
            ));
        }
        if matches!(update.visibility, Some(Visibility::ServerWide)) && !viewer.admin {
            return Err(BackendError::AdminOnly);
        }
        let stored = self.stored_mut(id).expect("seen");
        let before = stored.visibility.clone();
        if let Some(name) = update.name {
            stored.name = name.trim().to_owned();
        }
        if let Some(kinds) = update.kinds {
            stored.kinds = kinds;
        }
        if let Some(visibility) = update.visibility {
            stored.visibility = visibility;
        }
        stored.revision += 1;
        let (after, revision, owner) = (
            stored.visibility.clone(),
            stored.revision,
            stored.owner.clone(),
        );
        // Whoever no longer sees it loses its pin, with a note.
        let lost: Vec<String> = self
            .pins
            .iter()
            .filter(|(user, pins)| {
                pins.iter().any(|p| p == id)
                    && **user != owner
                    && sees(&before, user)
                    && !sees(&after, user)
            })
            .map(|(user, _)| user.clone())
            .collect();
        let name = self.stored_mut(id).expect("seen").name.clone();
        for user in lost {
            self.drop_pin(&user, id, &name, PinNoteReason::Unshared);
        }
        self.tell(PlaylistChange::Changed {
            id: id.to_owned(),
            revision,
        });
        Ok(self.seen(id, viewer)?.0)
    }

    fn drop_pin(&mut self, user: &str, id: &str, name: &str, reason: PinNoteReason) {
        if let Some(pins) = self.pins.get_mut(user) {
            pins.retain(|p| p != id);
        }
        self.notes
            .entry(user.to_owned())
            .or_default()
            .push(PinNote {
                playlist_id: id.to_owned(),
                name: name.to_owned(),
                reason,
            });
        self.tell(PlaylistChange::PinsChanged);
    }

    /// The items an add puts in: a season or show its episodes, an album
    /// or artist its tracks, each in order.
    pub fn edit(
        &mut self,
        id: &str,
        revision: u64,
        edits: Vec<PlaylistEdit>,
        viewer: Viewer<'_>,
        expand: impl Fn(&MediaId) -> Result<Vec<Item>, BackendError>,
    ) -> Result<u64, BackendError> {
        let (summary, stored) = self.seen(id, viewer)?;
        if !summary.can_edit {
            return Err(BackendError::PlaylistReadOnly);
        }
        // Touched since the edit's revision: an edit of one is refused.
        let changed: HashSet<EntryId> = stored
            .touched
            .iter()
            .filter(|(at, _)| *at > revision)
            .flat_map(|(_, entries)| entries.iter().copied())
            .collect();
        if edits
            .iter()
            .filter_map(PlaylistEdit::touches)
            .any(|entry| changed.contains(&entry))
        {
            return Err(BackendError::PlaylistConflict {
                revision: stored.revision,
            });
        }
        // Each add's items, checked against the kinds first: a refused
        // edit changes nothing.
        let mut adds = Vec::new();
        for edit in &edits {
            if let PlaylistEdit::Add { id: item, .. } = edit {
                // A track gets into Liked only by a like.
                if summary.builtin.is_some() {
                    return Err(BackendError::InvalidInput(
                        "Liked takes tracks by a like".into(),
                    ));
                }
                if !summary.kinds.allows(item.kind()) {
                    return Err(BackendError::KindNotAllowed(ItemKind::from(item.kind())));
                }
                adds.push(expand(item)?);
            }
        }
        let mut entries = stored.entries.clone();
        let mut touched = Vec::new();
        let mut adds = adds.into_iter();
        for edit in edits {
            match edit {
                PlaylistEdit::Add { before, .. } => {
                    let at = position(&entries, before);
                    let items = adds.next().expect("one per add");
                    let made: Vec<_> = items
                        .iter()
                        .map(|item| {
                            let entry = self.next_entry;
                            self.next_entry += 1;
                            (entry, item.id().clone())
                        })
                        .collect();
                    touched.extend(made.iter().map(|(entry, _)| *entry));
                    entries.splice(at..at, made);
                }
                PlaylistEdit::Remove { entry } => {
                    let len = entries.len();
                    entries.retain(|(e, _)| *e != entry);
                    if entries.len() == len {
                        return Err(BackendError::NotFound(format!("entry {entry}")));
                    }
                    touched.push(entry);
                }
                PlaylistEdit::Move { entry, before } => {
                    let from = entries
                        .iter()
                        .position(|(e, _)| *e == entry)
                        .ok_or_else(|| BackendError::NotFound(format!("entry {entry}")))?;
                    let moved = entries.remove(from);
                    let at = position(&entries, before);
                    entries.insert(at, moved);
                    touched.push(entry);
                }
                PlaylistEdit::Unknown => {
                    return Err(BackendError::InvalidInput("an unknown edit".into()));
                }
            }
        }
        let stored = self.stored_mut(id).expect("seen");
        stored.entries = entries;
        stored.revision += 1;
        let revision = stored.revision;
        stored.touched.push((revision, touched));
        self.tell(PlaylistChange::Changed {
            id: id.to_owned(),
            revision,
        });
        Ok(revision)
    }

    pub fn delete(&mut self, id: &str, viewer: Viewer<'_>) -> Result<(), BackendError> {
        let (summary, stored) = self.seen(id, viewer)?;
        if !summary.can_manage {
            return Err(BackendError::PlaylistReadOnly);
        }
        let name = stored.name.clone();
        self.playlists.retain(|p| p.id != id);
        let pinned: Vec<String> = self
            .pins
            .iter()
            .filter(|(user, pins)| pins.iter().any(|p| p == id) && *user != viewer.username)
            .map(|(user, _)| user.clone())
            .collect();
        if let Some(pins) = self.pins.get_mut(viewer.username) {
            pins.retain(|p| p != id);
        }
        for user in pinned {
            self.drop_pin(&user, id, &name, PinNoteReason::Deleted);
        }
        self.tell(PlaylistChange::Gone { id: id.to_owned() });
        Ok(())
    }

    pub fn pin(&mut self, id: &str, position: u32, viewer: Viewer<'_>) -> Result<(), BackendError> {
        self.seen(id, viewer)?;
        let pins = self.pins.entry(viewer.username.to_owned()).or_default();
        pins.retain(|p| p != id);
        let at = (position as usize).min(pins.len());
        pins.insert(at, id.to_owned());
        self.tell(PlaylistChange::PinsChanged);
        Ok(())
    }

    pub fn unpin(&mut self, id: &str, viewer: Viewer<'_>) -> Result<(), BackendError> {
        if let Some(pins) = self.pins.get_mut(viewer.username) {
            pins.retain(|p| p != id);
        }
        self.tell(PlaylistChange::PinsChanged);
        Ok(())
    }

    pub fn dismiss_notes(&mut self, viewer: Viewer<'_>) -> Result<(), BackendError> {
        self.notes.remove(viewer.username);
        self.tell(PlaylistChange::PinsChanged);
        Ok(())
    }

    /// The entry to play: `from`, else the user's place (or the one after
    /// it, once its item is watched), else the first; passing unavailable
    /// entries, and watched ones with Skip watched (never `from`, nor a
    /// place resumed unfinished). Keeps the settings.
    ///
    /// Shuffled, it plays in a round as the server's does (0.13): a new
    /// one when shuffle is turned on, starting at the place, or at `from`
    /// whenever it is given; else the round kept.
    pub fn play(
        &mut self,
        id: &str,
        from: Option<EntryId>,
        settings: PlaySettings,
        viewer: Viewer<'_>,
        watched: impl Fn(&MediaId) -> bool,
    ) -> Result<Option<(EntryId, MediaId)>, BackendError> {
        self.seen(id, viewer)?;
        let key = (viewer.username.to_owned(), id.to_owned());
        self.choose(id, &key, settings, from);
        let order = self.order(id, &key);
        let ids: Vec<EntryId> = order.iter().map(|(e, _)| *e).collect();
        let tries = match (from, self.places.get(&key).copied()) {
            (Some(from), _) => policy::again_from(&ids, from, settings.repeat),
            (None, Some(at)) => match order.iter().find(|(e, _)| *e == at) {
                Some((_, item)) if !watched(item) => policy::again_from(&ids, at, settings.repeat),
                // Heard to its end, or gone: what follows it.
                _ => policy::next_from(&ids, Some(at), settings.repeat),
            },
            (None, None) => policy::next_from(&ids, None, settings.repeat),
        };
        let next = self.play_on(&order, &tries, settings, &watched);
        self.moved(key, next.as_ref());
        Ok(next)
    }

    /// The round rule, for `key` now played with `settings`, `from`
    /// picked or not: one round per turn-on, kept until shuffle is turned
    /// off. A new one when shuffling starts (at the place, so the whole
    /// playlist is ahead) or when there is none, and when an entry is
    /// picked, that entry first. The settings are kept.
    fn choose(
        &mut self,
        id: &str,
        key: &(String, String),
        settings: PlaySettings,
        from: Option<EntryId>,
    ) {
        let shuffling_already = self.settings.get(key).is_some_and(|s| s.shuffle);
        let entries = self.entry_ids(id);
        let round = match (settings.shuffle, from, self.rounds.get(key)) {
            (false, _, _) => None,
            (true, Some(from), _) => Some(round_from(&entries, Some(from))),
            (true, None, Some(kept)) if shuffling_already => Some(kept.clone()),
            (true, None, _) => Some(round_from(&entries, self.places.get(key).copied())),
        };
        self.keep_round(key, round);
        self.settings.insert(key.clone(), settings);
    }

    /// The user's place on `next`, or, at the end, no place: the next
    /// play starts at the top, of the same round.
    fn moved(&mut self, key: (String, String), next: Option<&(EntryId, MediaId)>) {
        match next {
            Some((entry, _)) => {
                self.places.insert(key, *entry);
            }
            None => {
                self.places.remove(&key);
            }
        }
    }

    fn keep_round(&mut self, key: &(String, String), round: Option<Vec<EntryId>>) {
        match round {
            Some(round) => self.rounds.insert(key.clone(), round),
            None => self.rounds.remove(key),
        };
    }

    /// What plays after `after`: the next playable one; at the end, with
    /// Repeat All, the same order again from its top, else nothing and
    /// the place cleared. Repeat One plays `after` again. An `after` no
    /// longer in the list goes on from the top of the order, as on the
    /// server.
    pub fn next(
        &mut self,
        id: &str,
        after: EntryId,
        viewer: Viewer<'_>,
        watched: impl Fn(&MediaId) -> bool,
    ) -> Result<Option<(EntryId, MediaId)>, BackendError> {
        self.seen(id, viewer)?;
        let key = (viewer.username.to_owned(), id.to_owned());
        let settings = self.settings.get(&key).copied().unwrap_or_default();
        let order = self.order(id, &key);
        let ids: Vec<EntryId> = order.iter().map(|(e, _)| *e).collect();
        let tries = policy::next_from(&ids, Some(after), settings.repeat);
        let next = self.play_on(&order, &tries, settings, &watched);
        self.moved(key, next.as_ref());
        Ok(next)
    }

    /// The next `limit` (100) entries `viewer` plays in playlist `id`
    /// after their place, each the one [`Store::next`] would give after
    /// the one before, as the server lists them: with Repeat One the
    /// place's entry alone; with no place, from the top.
    pub fn upcoming(
        &mut self,
        id: &str,
        limit: u32,
        viewer: Viewer<'_>,
        watched: impl Fn(&MediaId) -> bool,
    ) -> Result<Vec<(EntryId, MediaId)>, BackendError> {
        self.seen(id, viewer)?;
        let key = (viewer.username.to_owned(), id.to_owned());
        let settings = self.settings.get(&key).copied().unwrap_or_default();
        let order = self.order(id, &key);
        let ids: Vec<EntryId> = order.iter().map(|(e, _)| *e).collect();
        let mut after = self.places.get(&key).copied();
        let mut upcoming = Vec::new();
        while upcoming.len() < limit.min(crate::backend::MAX_UPCOMING) as usize {
            let tries = policy::next_from(&ids, after, settings.repeat);
            let Some(next) = self.play_on(&order, &tries, settings, &watched) else {
                break;
            };
            after = Some(next.0);
            upcoming.push(next);
            if settings.repeat == Repeat::One {
                break;
            }
        }
        Ok(upcoming)
    }

    /// Keeps `settings` as how `viewer` plays playlist `id`, playing
    /// nothing: the place stays, and the round by [`Store::choose`].
    pub fn set_settings(
        &mut self,
        id: &str,
        settings: PlaySettings,
        viewer: Viewer<'_>,
    ) -> Result<(), BackendError> {
        self.seen(id, viewer)?;
        let key = (viewer.username.to_owned(), id.to_owned());
        self.choose(id, &key, settings, None);
        Ok(())
    }

    /// What plays by `tries` in `order`: the first of its entries that
    /// can play, else, with Repeat All, the first of the same order again
    /// from its top.
    fn play_on(
        &self,
        order: &[(EntryId, MediaId)],
        tries: &policy::Tries,
        settings: PlaySettings,
        watched: &impl Fn(&MediaId) -> bool,
    ) -> Option<(EntryId, MediaId)> {
        let first = |tried: &mut dyn Iterator<Item = EntryId>, regardless: Option<EntryId>| {
            tried
                .filter_map(|e| order.iter().find(|(x, _)| *x == e))
                .find(|(e, item)| {
                    !self.unavailable.contains(item)
                        && !(settings.skip_watched && watched(item) && regardless != Some(*e))
                })
                .cloned()
        };
        first(&mut tries.tried.iter().copied(), tries.regardless).or_else(|| {
            tries
                .wraps
                .then(|| first(&mut order.iter().map(|(e, _)| *e), None))
                .flatten()
        })
    }

    fn entry_ids(&self, id: &str) -> Vec<EntryId> {
        self.playlists
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.entries.iter().map(|(e, _)| *e).collect())
            .unwrap_or_default()
    }

    /// The order playlist `id` plays in for `key`: its own, or its round
    /// while shuffled, brought up to date as the server's is (and kept
    /// so): entries taken out leave it, and those added since go among
    /// the ones not played yet.
    fn order(&mut self, id: &str, key: &(String, String)) -> Vec<(EntryId, MediaId)> {
        let entries = self
            .playlists
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.entries.clone())
            .unwrap_or_default();
        let Some(round) = self.rounds.get(key) else {
            return entries;
        };
        let now: Vec<EntryId> = entries.iter().map(|(e, _)| *e).collect();
        // A fixed seed: a test knows where an added entry goes.
        let round = policy::reconciled(round, &now, self.places.get(key).copied(), 0);
        let order = round
            .iter()
            .filter_map(|e| entries.iter().find(|(x, _)| x == e).cloned())
            .collect();
        self.rounds.insert(key.clone(), round);
        order
    }
}

/// A shuffled round of `entries`, as the server makes one: `first`, if
/// it is there, then the rest in a fixed order of their own (odd places
/// first, then even), so a test knows it.
fn round_from(entries: &[EntryId], first: Option<EntryId>) -> Vec<EntryId> {
    let first = first.filter(|f| entries.contains(f));
    let rest = entries.iter().copied().filter(|e| Some(*e) != first);
    let (odd, even): (Vec<_>, Vec<_>) = rest.enumerate().partition(|(at, _)| at % 2 == 1);
    first
        .into_iter()
        .chain(odd.into_iter().chain(even).map(|(_, e)| e))
        .collect()
}

/// Where an add or move of an entry goes: before `before`, or at the end
/// when it is `None` or gone.
fn position(entries: &[(EntryId, MediaId)], before: Option<EntryId>) -> usize {
    before
        .and_then(|before| entries.iter().position(|(e, _)| *e == before))
        .unwrap_or(entries.len())
}

/// The tracks `items` like or unlike: a track itself, an album its
/// tracks. Anything else is refused, as are none or too many at once.
fn likeable(
    items: &[MediaId],
    expand: impl Fn(&MediaId) -> Result<Vec<Item>, BackendError>,
) -> Result<Vec<MediaId>, BackendError> {
    if items.is_empty() || items.len() > LIKE_LIMIT {
        return Err(BackendError::InvalidInput(format!(
            "1 to {LIKE_LIMIT} items at a time"
        )));
    }
    let mut tracks = Vec::new();
    for item in items {
        match item.kind() {
            spoolframe_types::MediaKind::Track | spoolframe_types::MediaKind::Album => {
                tracks.extend(expand(item)?.iter().map(|t| t.id().clone()))
            }
            other => return Err(BackendError::KindNotAllowed(ItemKind::from(other))),
        }
    }
    Ok(tracks)
}

/// A fake user's id as the server gives it.
pub fn user_id(username: &str) -> String {
    format!("local:{username}")
}

/// Whether `username` sees a playlist shared this way (not as its owner).
fn sees(visibility: &Visibility, username: &str) -> bool {
    match visibility {
        Visibility::ServerWide => true,
        Visibility::Shared { with } => with.iter().any(|s| s.user_id == user_id(username)),
        _ => false,
    }
}

/// A share with `username`.
pub fn share(username: &str, can_edit: bool) -> PlaylistShare {
    PlaylistShare::new(user_id(username), can_edit)
}
