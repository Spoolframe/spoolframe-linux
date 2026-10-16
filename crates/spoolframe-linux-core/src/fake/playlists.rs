//! The fake server's playlists (PX-40), kept for every user of the fake
//! server: what each user sees of them, their pins and pin notes, their
//! place and play settings in each, and the events another device would
//! hear. The rules are the server's as the protocol states them: kinds
//! only widen, an edit on an old revision is redone unless it touches an
//! entry changed since, and a pin that goes away leaves a note once.

use std::collections::{HashMap, HashSet};

use spoolframe_types::{Item, MediaId};
use tokio::sync::broadcast;

use crate::backend::{
    BackendError, EntryId, Features, ItemKind, Kinds, PinNote, PinNoteReason, PlaySettings,
    PlaylistChange, PlaylistEdit, PlaylistEvent, PlaylistList, PlaylistOwnerKind, PlaylistRef,
    PlaylistShare, PlaylistSummary, PlaylistUpdate, Repeat, Visibility,
};

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
    /// The features the session has: what the app and the fake server
    /// both list. All of them unless a test turns some off.
    pub features: Features,
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
    /// Items still in the library whose file cannot be read now.
    pub unavailable: HashSet<MediaId>,
    events: broadcast::Sender<PlaylistEvent>,
}

impl Store {
    pub fn new(server: &str) -> Self {
        Store {
            features: Features {
                playlists: true,
                liked: true,
                last_viewed: true,
                playlist_sharing: true,
                play_history: true,
            },
            server: server.to_owned(),
            next_id: 1,
            next_entry: 1,
            playlists: Vec::new(),
            pins: HashMap::new(),
            notes: HashMap::new(),
            places: HashMap::new(),
            settings: HashMap::new(),
            unavailable: HashSet::new(),
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

    pub fn check(&self) -> Result<(), BackendError> {
        if self.features.playlists {
            Ok(())
        } else {
            Err(BackendError::Unsupported(
                spoolframe_protocol::features::PLAYLISTS.into(),
            ))
        }
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
            return Some(Access {
                owner_kind: PlaylistOwnerKind::Mine,
                can_edit: true,
                can_manage: true,
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
            other => other.clone(),
        };
        let pinned = self
            .pins
            .get(viewer.username)
            .and_then(|pins| pins.iter().position(|p| p == &stored.id))
            .map(|at| at as u32);
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
            builtin: None,
            owner_name: None,
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
        PlaylistList {
            playlists: self
                .playlists
                .iter()
                .filter_map(|p| self.summary(p, viewer))
                .collect(),
            pin_notes: self.notes.get(viewer.username).cloned().unwrap_or_default(),
        }
    }

    /// The playlists `viewer` sees holding any of `items`.
    pub fn holding(&self, items: &HashSet<MediaId>, viewer: Viewer<'_>) -> Vec<PlaylistRef> {
        self.playlists
            .iter()
            .filter(|p| p.entries.iter().any(|(_, item)| items.contains(item)))
            .filter_map(|p| self.summary(p, viewer))
            .map(|s| PlaylistRef {
                id: s.id,
                name: s.name,
                owner_kind: s.owner_kind,
                builtin: None,
            })
            .collect()
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
        self.check()?;
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

    pub fn update(
        &mut self,
        id: &str,
        update: PlaylistUpdate,
        viewer: Viewer<'_>,
    ) -> Result<PlaylistSummary, BackendError> {
        self.check()?;
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
        self.check()?;
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
        self.check()?;
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
        self.check()?;
        self.seen(id, viewer)?;
        let pins = self.pins.entry(viewer.username.to_owned()).or_default();
        pins.retain(|p| p != id);
        let at = (position as usize).min(pins.len());
        pins.insert(at, id.to_owned());
        self.tell(PlaylistChange::PinsChanged);
        Ok(())
    }

    pub fn unpin(&mut self, id: &str, viewer: Viewer<'_>) -> Result<(), BackendError> {
        self.check()?;
        if let Some(pins) = self.pins.get_mut(viewer.username) {
            pins.retain(|p| p != id);
        }
        self.tell(PlaylistChange::PinsChanged);
        Ok(())
    }

    pub fn dismiss_notes(&mut self, viewer: Viewer<'_>) -> Result<(), BackendError> {
        self.check()?;
        self.notes.remove(viewer.username);
        self.tell(PlaylistChange::PinsChanged);
        Ok(())
    }

    /// The entry to play: `from`, else the user's place (or the one after
    /// it, once its item is watched), else the first; passing unavailable
    /// entries, and watched ones with Skip watched. Keeps the settings.
    pub fn play(
        &mut self,
        id: &str,
        from: Option<EntryId>,
        settings: PlaySettings,
        viewer: Viewer<'_>,
        watched: impl Fn(&MediaId) -> bool,
    ) -> Result<Option<(EntryId, MediaId)>, BackendError> {
        self.check()?;
        self.seen(id, viewer)?;
        let key = (viewer.username.to_owned(), id.to_owned());
        self.settings.insert(key.clone(), settings);
        let order = self.order(id, settings);
        let playable = |entry: &(EntryId, MediaId)| {
            !self.unavailable.contains(&entry.1) && !(settings.skip_watched && watched(&entry.1))
        };
        let start = match from.or_else(|| self.places.get(&key).copied()) {
            Some(entry) => order.iter().position(|(e, _)| *e == entry).map(|at| {
                let resumed = from.is_some() || !watched(&order[at].1);
                if resumed { at } else { at + 1 }
            }),
            None => Some(0),
        }
        .unwrap_or(0);
        let next = order[start.min(order.len())..]
            .iter()
            .find(|entry| playable(entry))
            .cloned();
        match &next {
            Some((entry, _)) => self.places.insert(key, *entry),
            None => self.places.remove(&key),
        };
        Ok(next)
    }

    /// What plays after `after`: the next playable one; at the end, the
    /// first again with Repeat all, else nothing and the place cleared.
    /// Repeat one plays `after` again.
    pub fn next(
        &mut self,
        id: &str,
        after: EntryId,
        viewer: Viewer<'_>,
        watched: impl Fn(&MediaId) -> bool,
    ) -> Result<Option<(EntryId, MediaId)>, BackendError> {
        self.check()?;
        self.seen(id, viewer)?;
        let key = (viewer.username.to_owned(), id.to_owned());
        let settings = self.settings.get(&key).copied().unwrap_or_default();
        let order = self.order(id, settings);
        let playable = |entry: &&(EntryId, MediaId)| {
            !self.unavailable.contains(&entry.1) && !(settings.skip_watched && watched(&entry.1))
        };
        let at = order.iter().position(|(e, _)| *e == after);
        let next = match (settings.repeat, at) {
            (Repeat::One, Some(at)) => Some(order[at].clone()),
            (_, Some(at)) => order[at + 1..].iter().find(playable).cloned().or_else(|| {
                matches!(settings.repeat, Repeat::All)
                    .then(|| order.iter().find(playable).cloned())
                    .flatten()
            }),
            (_, None) => None,
        };
        match &next {
            Some((entry, _)) => self.places.insert(key, *entry),
            None => self.places.remove(&key),
        };
        Ok(next)
    }

    /// The order a playlist plays in: its own, or with Shuffle a fixed
    /// one of its own (odd places first, then even), so a test knows it.
    fn order(&self, id: &str, settings: PlaySettings) -> Vec<(EntryId, MediaId)> {
        let entries = self
            .playlists
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.entries.clone())
            .unwrap_or_default();
        if !settings.shuffle {
            return entries;
        }
        let (odd, even): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .enumerate()
            .partition(|(at, _)| at % 2 == 1);
        odd.into_iter().chain(even).map(|(_, e)| e).collect()
    }
}

/// Where an add or move of an entry goes: before `before`, or at the end
/// when it is `None` or gone.
fn position(entries: &[(EntryId, MediaId)], before: Option<EntryId>) -> usize {
    before
        .and_then(|before| entries.iter().position(|(e, _)| *e == before))
        .unwrap_or(entries.len())
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
