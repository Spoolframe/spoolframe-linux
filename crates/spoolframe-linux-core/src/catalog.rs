//! Catalog reads kept in memory and shown at once, re-read behind what is
//! shown when they are old (stale-while-revalidate), as the Apple apps do.
//!
//! [`CachedSession`] wraps a [`Session`] and answers the catalog's reads
//! (Home, the libraries, a library's pages, an item and its details, the
//! playlists and a playlist's pages) from a [`Catalog`] the app keeps for
//! as long as it runs. A kept value comes back at once. When it is older
//! than its kind's freshness, one re-read starts behind it (one at a time
//! for each read); when what comes back differs, the catalog says which
//! read changed on [`Catalog::changes`], and the page showing it reads it
//! again, getting the new value, which is fresh, so no re-read follows.
//! Search is not kept: each one is asked of the server.
//!
//! Entries are kept per session (server and user), so switching profile
//! back shows that profile's Home at once and never another's. A change
//! made through the wrapper (watched, hidden, a playback started or
//! stopped, a playlist made, changed, pinned, deleted or played, a like)
//! clears the session's entries before and after it is sent, so what is
//! read next is the server's. Nothing is kept on disk.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spoolframe_types::{
    ArtworkId, Item, Library, LibraryId, MediaId, MediaTracks, SubtitleFormat, TrackPreferences,
};
use tokio::sync::broadcast;

use crate::backend::*;

/// The most entries kept, over every session; the least recently read go
/// first.
pub const CAPACITY: usize = 3000;

/// What tells the time, so tests can move it.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// The system's monotonic clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Runs a re-read behind what is shown: the app's async runtime.
pub use crate::reader::Spawn;

/// Whose entries: one user signed in on one server.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    pub server: ServerKey,
    pub user: String,
}

impl Scope {
    pub fn of(profile: &Profile) -> Self {
        Self {
            server: profile.server.clone(),
            user: profile.username.clone(),
        }
    }
}

/// One read the catalog keeps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Read {
    Home,
    Libraries,
    Items {
        library: LibraryId,
        offset: u32,
        limit: u32,
    },
    Item(MediaId),
    ItemDetail(MediaId),
    Playlists,
    Playlist {
        id: String,
        offset: u32,
        limit: u32,
    },
}

/// A kept read whose re-read came back different.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub scope: Scope,
    pub read: Read,
}

/// A read's answer, as kept.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Home(Box<HomeScreen>),
    Libraries(Vec<Library>),
    Items(Vec<Item>, u32),
    Item(Box<Item>, Vec<Item>),
    Detail(Box<ItemDetail>),
    Playlists(Box<PlaylistList>),
    Playlist(Box<PlaylistPage>),
}

impl Value {
    /// How long it is shown before a re-read starts behind it: what
    /// changes often (Home, playlists) soonest, music latest.
    fn freshness(&self) -> Duration {
        const SECOND: Duration = Duration::from_secs(1);
        const MINUTE: Duration = Duration::from_secs(60);
        let of_item = |item: &Item| match item {
            Item::Movie(_) | Item::Episode(_) => 2 * MINUTE,
            Item::Show(_) | Item::Season(_) => 5 * MINUTE,
            Item::Artist(_) | Item::Album(_) | Item::Track(_) => 10 * MINUTE,
        };
        match self {
            Value::Home(_) => 10 * SECOND,
            Value::Playlists(_) | Value::Playlist(_) => 30 * SECOND,
            Value::Items(..) => 2 * MINUTE,
            Value::Item(item, _) => of_item(item),
            Value::Detail(detail) => of_item(&detail.item),
            Value::Libraries(_) => 10 * MINUTE,
        }
    }
}

type Key = (Scope, Read);

struct Entry {
    value: Value,
    fetched: Instant,
    last_read: u64,
    /// A re-read of it is in flight.
    revalidating: bool,
}

#[derive(Default)]
struct State {
    entries: HashMap<Key, Entry>,
    tick: u64,
    /// Bumped by each clear: a read begun before it is not kept.
    generations: HashMap<Scope, u64>,
}

impl State {
    fn generation(&self, scope: &Scope) -> u64 {
        self.generations.get(scope).copied().unwrap_or(0)
    }
}

/// The kept reads of every session, and who hears of changes to them.
pub struct Catalog {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
    spawn: Spawn,
    changes: broadcast::Sender<Change>,
    capacity: usize,
}

impl Catalog {
    pub fn new(clock: Arc<dyn Clock>, spawn: Spawn) -> Arc<Self> {
        Self::with_capacity(clock, spawn, CAPACITY)
    }

    fn with_capacity(clock: Arc<dyn Clock>, spawn: Spawn, capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            clock,
            spawn,
            changes: broadcast::channel(64).0,
            capacity,
        })
    }

    /// The kept reads whose re-reads came back different.
    pub fn changes(&self) -> broadcast::Receiver<Change> {
        self.changes.subscribe()
    }

    /// Forgets what `scope` read: the next reads ask the server, and
    /// re-reads in flight are not kept.
    pub fn clear(&self, scope: &Scope) {
        let mut state = self.state.lock().expect("catalog");
        state.entries.retain(|(s, _), _| s != scope);
        *state.generations.entry(scope.clone()).or_default() += 1;
    }

    /// How many reads `scope` has kept.
    pub fn len(&self, scope: &Scope) -> usize {
        let state = self.state.lock().expect("catalog");
        state.entries.keys().filter(|(s, _)| s == scope).count()
    }

    /// The kept value of `read` at once, a re-read started behind it when
    /// it is old; else `fetch`ed now, and kept.
    pub(crate) async fn read<F>(
        self: &Arc<Self>,
        scope: &Scope,
        read: Read,
        fetch: F,
    ) -> Result<Value, BackendError>
    where
        F: FnOnce() -> BoxFuture<'static, Result<Value, BackendError>>,
    {
        let key = (scope.clone(), read);
        let now = self.clock.now();
        let (kept, generation, revalidate) = {
            let mut state = self.state.lock().expect("catalog");
            state.tick += 1;
            let (tick, generation) = (state.tick, state.generation(scope));
            match state.entries.get_mut(&key) {
                Some(entry) => {
                    entry.last_read = tick;
                    let stale = now.duration_since(entry.fetched) >= entry.value.freshness();
                    let revalidate = stale && !entry.revalidating;
                    entry.revalidating |= revalidate;
                    (Some(entry.value.clone()), generation, revalidate)
                }
                None => (None, generation, false),
            }
        };
        if let Some(value) = kept {
            if revalidate {
                let (catalog, fetching) = (self.clone(), fetch());
                (self.spawn)(Box::pin(async move {
                    let fetched = fetching.await;
                    catalog.revalidated(key, generation, fetched);
                }));
            }
            return Ok(value);
        }
        let value = fetch().await?;
        self.keep(key, generation, value.clone());
        Ok(value)
    }

    /// Keeps a value read now, unless its scope was cleared since the
    /// read began.
    fn keep(&self, key: Key, generation: u64, value: Value) {
        let mut state = self.state.lock().expect("catalog");
        if state.generation(&key.0) != generation {
            return;
        }
        state.tick += 1;
        let entry = Entry {
            value,
            fetched: self.clock.now(),
            last_read: state.tick,
            revalidating: false,
        };
        state.entries.insert(key, entry);
        if state.entries.len() > self.capacity {
            let oldest = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_read)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                state.entries.remove(&oldest);
            }
        }
    }

    /// A re-read is in: kept, and said when it differs.
    fn revalidated(&self, key: Key, generation: u64, fetched: Result<Value, BackendError>) {
        let change = {
            let mut state = self.state.lock().expect("catalog");
            if state.generation(&key.0) != generation {
                return;
            }
            let now = self.clock.now();
            let Some(entry) = state.entries.get_mut(&key) else {
                return;
            };
            entry.revalidating = false;
            match fetched {
                Ok(value) => {
                    let changed = entry.value != value;
                    entry.value = value;
                    entry.fetched = now;
                    changed
                }
                Err(e) => {
                    // The kept value stays; the next read tries again.
                    tracing::debug!(error = %e, read = ?key.1, "catalog re-read failed");
                    false
                }
            }
        };
        if change {
            let (scope, read) = key;
            // Nobody listening is fine: nothing shows it.
            let _ = self.changes.send(Change { scope, read });
        }
    }
}

/// A session whose catalog reads go through a [`Catalog`].
pub struct CachedSession {
    inner: Arc<dyn Session>,
    catalog: Arc<Catalog>,
    scope: Scope,
}

impl CachedSession {
    pub fn new(inner: Arc<dyn Session>, catalog: Arc<Catalog>, scope: Scope) -> Self {
        Self {
            inner,
            catalog,
            scope,
        }
    }

    /// Forgets this session's kept reads: for changes made elsewhere (a
    /// playlist event from the server).
    pub fn clear(&self) {
        self.catalog.clear(&self.scope);
    }

    async fn cached(&self, read: Read) -> Result<Value, BackendError> {
        let (inner, asked) = (self.inner.clone(), read.clone());
        self.catalog
            .read(&self.scope, read, move || fetch(inner, asked))
            .await
    }

    /// `call`, a change: the session's kept reads are cleared before it
    /// is sent (a read meanwhile asks the server) and after it is
    /// answered (a read begun before it is not kept), whatever the answer:
    /// a change that timed out may still have been made.
    fn changing<'a, T: Send + 'a>(
        &'a self,
        call: impl FnOnce() -> BoxFuture<'a, T> + Send + 'a,
    ) -> BoxFuture<'a, T> {
        Box::pin(async move {
            self.clear();
            let answer = call().await;
            self.clear();
            answer
        })
    }
}

/// Reads `read` from the server.
fn fetch(inner: Arc<dyn Session>, read: Read) -> BoxFuture<'static, Result<Value, BackendError>> {
    Box::pin(async move {
        Ok(match read {
            Read::Home => Value::Home(Box::new(inner.home().await?)),
            Read::Libraries => Value::Libraries(inner.libraries().await?),
            Read::Items {
                library,
                offset,
                limit,
            } => {
                let (items, total) = inner.items(&library, offset, limit).await?;
                Value::Items(items, total)
            }
            Read::Item(id) => {
                let (item, children) = inner.item(&id).await?;
                Value::Item(Box::new(item), children)
            }
            Read::ItemDetail(id) => Value::Detail(Box::new(inner.item_detail(&id).await?)),
            Read::Playlists => Value::Playlists(Box::new(inner.playlists().await?)),
            Read::Playlist { id, offset, limit } => {
                Value::Playlist(Box::new(inner.playlist(&id, offset, limit).await?))
            }
        })
    })
}

/// A kept value of another kind than its read gives: never, since the
/// read decides the kind.
fn mismatch() -> BackendError {
    BackendError::Other("the catalog kept another kind of value".into())
}

impl Session for CachedSession {
    fn activate(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        self.inner.activate()
    }

    fn web_url(&self) -> Option<String> {
        self.inner.web_url()
    }

    fn connection_path(&self) -> ConnectionPath {
        self.inner.connection_path()
    }

    fn link_rate(&self) -> Option<u64> {
        self.inner.link_rate()
    }

    fn limit_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        link_bitrate: u64,
        position: Duration,
    ) -> BoxFuture<'a, Result<PlaybackChange, BackendError>> {
        self.inner.limit_playback(handle, link_bitrate, position)
    }

    fn previews<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<Previews, BackendError>> {
        self.inner.previews(id)
    }

    fn home(&self) -> BoxFuture<'_, Result<HomeScreen, BackendError>> {
        Box::pin(async move {
            match self.cached(Read::Home).await? {
                Value::Home(v) => Ok(*v),
                _ => Err(mismatch()),
            }
        })
    }

    fn libraries(&self) -> BoxFuture<'_, Result<Vec<Library>, BackendError>> {
        Box::pin(async move {
            match self.cached(Read::Libraries).await? {
                Value::Libraries(v) => Ok(v),
                _ => Err(mismatch()),
            }
        })
    }

    fn items<'a>(
        &'a self,
        library: &'a LibraryId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>> {
        Box::pin(async move {
            match self
                .cached(Read::Items {
                    library: library.clone(),
                    offset,
                    limit,
                })
                .await?
            {
                Value::Items(items, total) => Ok((items, total)),
                _ => Err(mismatch()),
            }
        })
    }

    fn item<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(Item, Vec<Item>), BackendError>> {
        Box::pin(async move {
            match self.cached(Read::Item(id.clone())).await? {
                Value::Item(item, children) => Ok((*item, children)),
                _ => Err(mismatch()),
            }
        })
    }

    fn item_detail<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<ItemDetail, BackendError>> {
        Box::pin(async move {
            match self.cached(Read::ItemDetail(id.clone())).await? {
                Value::Detail(v) => Ok(*v),
                _ => Err(mismatch()),
            }
        })
    }

    fn search<'a>(
        &'a self,
        request: &'a SearchRequest,
    ) -> BoxFuture<'a, Result<SearchResults, BackendError>> {
        self.inner.search(request)
    }

    fn person_items<'a>(
        &'a self,
        person: &'a PersonId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>> {
        self.inner.person_items(person, offset, limit)
    }

    fn tracks<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<MediaTracks, BackendError>> {
        self.inner.tracks(id)
    }

    fn playback_plan<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
    ) -> BoxFuture<'a, Result<(PlaybackPlan, Option<ChosenTracks>), BackendError>> {
        self.inner.playback_plan(id, selection)
    }

    fn start_playback<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
        options: PlaybackOptions,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>> {
        self.changing(move || self.inner.start_playback(id, selection, options))
    }

    fn change_quality<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        quality: PlaybackQuality,
        position: Duration,
    ) -> BoxFuture<'a, Result<PlaybackChange, BackendError>> {
        self.inner.change_quality(handle, quality, position)
    }

    fn read_chunk<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        epoch: u32,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Chunk, BackendError>> {
        self.inner.read_chunk(handle, epoch, offset, length)
    }

    fn seek_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        position: Duration,
    ) -> BoxFuture<'a, Result<u32, BackendError>> {
        self.inner.seek_playback(handle, position)
    }

    fn stop_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.stop_playback(handle))
    }

    fn read<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>> {
        self.inner.read(handle, offset, length)
    }

    fn read_subtitle<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(SubtitleFormat, Vec<u8>), BackendError>> {
        self.inner.read_subtitle(handle)
    }

    fn artwork<'a>(
        &'a self,
        id: &'a ArtworkId,
        size: ArtworkSize,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>> {
        self.inner.artwork(id, size)
    }

    fn update_progress<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        position: Duration,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.update_progress(handle, position)
    }

    fn mark_watched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.mark_watched(id))
    }

    fn mark_unwatched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.mark_unwatched(id))
    }

    fn hide_from_continue<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.hide_from_continue(id))
    }

    fn unhide_from_continue<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.unhide_from_continue(id))
    }

    fn track_preferences(&self) -> BoxFuture<'_, Result<TrackPreferencesInfo, BackendError>> {
        self.inner.track_preferences()
    }

    fn set_track_preferences<'a>(
        &'a self,
        preferences: &'a TrackPreferences,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.set_track_preferences(preferences)
    }

    fn set_track_override<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Selection,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.set_track_override(id, selection)
    }

    fn clear_track_override<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.clear_track_override(id)
    }

    fn unlock<'a>(&'a self, pin: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.unlock(pin)
    }

    fn pin(&self) -> BoxFuture<'_, Result<Option<PinState>, BackendError>> {
        self.inner.pin()
    }

    fn set_pin<'a>(
        &'a self,
        pin: &'a str,
        proof: Option<PinProof>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.set_pin(pin, proof)
    }

    fn clear_pin(&self, proof: PinProof) -> BoxFuture<'_, Result<(), BackendError>> {
        self.inner.clear_pin(proof)
    }

    fn plex_link(&self) -> BoxFuture<'_, Result<Option<PlexLink>, BackendError>> {
        self.inner.plex_link()
    }

    fn devices(&self) -> BoxFuture<'_, Result<Vec<DeviceInfo>, BackendError>> {
        self.inner.devices()
    }

    fn begin_passkey_registration(&self) -> BoxFuture<'_, Result<PasskeyCreation, BackendError>> {
        self.inner.begin_passkey_registration()
    }

    fn finish_passkey_registration(
        &self,
        made: FinishPasskeyRegistration,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        self.inner.finish_passkey_registration(made)
    }

    fn passkeys(&self) -> BoxFuture<'_, Result<Vec<PasskeyInfo>, BackendError>> {
        self.inner.passkeys()
    }

    fn rename_passkey<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.rename_passkey(id, name)
    }

    fn remove_passkey<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.remove_passkey(id)
    }

    fn set_pin_required<'a>(
        &'a self,
        device: Option<&'a str>,
        required: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.set_pin_required(device, required)
    }

    fn lookup_device_sign_in<'a>(
        &'a self,
        code: &'a str,
    ) -> BoxFuture<'a, Result<DeviceSignInRequest, BackendError>> {
        self.inner.lookup_device_sign_in(code)
    }

    fn approve_device_sign_in<'a>(
        &'a self,
        code: &'a str,
        remember: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.approve_device_sign_in(code, remember)
    }

    fn forget_device<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.forget_device(device)
    }

    fn set_device_trust<'a>(
        &'a self,
        device: Option<&'a str>,
        trusted: bool,
        auto_login: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.set_device_trust(device, trusted, auto_login)
    }

    fn sign_out(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        self.inner.sign_out()
    }

    fn deny_device_sign_in<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.deny_device_sign_in(code)
    }

    fn confirm_password<'a>(
        &'a self,
        password: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.confirm_password(password)
    }

    fn begin_confirm_passkey(&self) -> BoxFuture<'_, Result<PasskeyRequest, BackendError>> {
        self.inner.begin_confirm_passkey()
    }

    fn finish_confirm_passkey(
        &self,
        signed: FinishPasskeySignIn,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        self.inner.finish_confirm_passkey(signed)
    }

    fn is_admin(&self) -> bool {
        self.inner.is_admin()
    }

    fn password_change_required(&self) -> bool {
        self.inner.password_change_required()
    }

    fn change_password<'a>(
        &'a self,
        current: &'a str,
        new: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.change_password(current, new)
    }

    fn create_invite(&self, spec: InviteSpec) -> BoxFuture<'_, Result<MintedInvite, BackendError>> {
        self.inner.create_invite(spec)
    }

    fn invites(&self) -> BoxFuture<'_, Result<Vec<InviteSummary>, BackendError>> {
        self.inner.invites()
    }

    fn revoke_invite<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        self.inner.revoke_invite(id)
    }

    fn guest_identity(&self) -> Option<spoolframe_protocol::portable::PortableUserId> {
        self.inner.guest_identity()
    }

    fn tie_info(&self) -> BoxFuture<'_, Result<Vec<Tie>, BackendError>> {
        self.inner.tie_info()
    }

    fn tie(
        &self,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        self.inner.tie(identity)
    }

    fn untie(
        &self,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        self.inner.untie(identity)
    }

    fn leave_server(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        self.inner.leave_server()
    }

    fn can_leave(&self) -> bool {
        self.inner.can_leave()
    }

    fn can_leave_now(&self) -> BoxFuture<'_, Result<bool, BackendError>> {
        self.inner.can_leave_now()
    }

    fn has_playlists(&self) -> bool {
        self.inner.has_playlists()
    }

    fn has_liked(&self) -> bool {
        self.inner.has_liked()
    }

    fn like<'a>(
        &'a self,
        items: &'a [MediaId],
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        self.changing(move || self.inner.like(items))
    }

    fn unlike<'a>(
        &'a self,
        items: &'a [MediaId],
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        self.changing(move || self.inner.unlike(items))
    }

    fn has_last_viewed(&self) -> bool {
        self.inner.has_last_viewed()
    }

    fn has_playlist_sharing(&self) -> bool {
        self.inner.has_playlist_sharing()
    }

    fn share_candidates<'a>(
        &'a self,
        id: &'a str,
        query: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<ShareCandidatePage, BackendError>> {
        self.inner.share_candidates(id, query, offset, limit)
    }

    fn has_play_history(&self) -> bool {
        self.inner.has_play_history()
    }

    fn has_playlist_art(&self) -> bool {
        self.inner.has_playlist_art()
    }

    fn has_subtitle_sync(&self) -> bool {
        self.inner.has_subtitle_sync()
    }

    fn has_subtitle_search(&self) -> bool {
        self.inner.has_subtitle_search()
    }

    /// Not kept: the server keeps each search a day itself.
    fn search_subtitles<'a>(
        &'a self,
        id: &'a MediaId,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<SubtitleSearch, BackendError>> {
        self.inner.search_subtitles(id, language)
    }

    fn download_subtitle<'a>(
        &'a self,
        id: &'a MediaId,
        result: &'a str,
    ) -> BoxFuture<'a, Result<SubtitleDownloaded, BackendError>> {
        self.changing(move || self.inner.download_subtitle(id, result))
    }

    fn sync_subtitle<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>> {
        self.changing(move || self.inner.sync_subtitle(id, track))
    }

    fn use_original_subtitle_timing<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>> {
        self.changing(move || self.inner.use_original_subtitle_timing(id, track))
    }

    fn set_subtitle_offset<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
        offset_ms: i64,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>> {
        self.changing(move || self.inner.set_subtitle_offset(id, track, offset_ms))
    }

    fn remove_subtitle_download<'a>(
        &'a self,
        id: &'a MediaId,
        download: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.remove_subtitle_download(id, download))
    }

    fn recently_played(
        &self,
        kinds: Option<Kinds>,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'_, Result<(Vec<Item>, u32), BackendError>> {
        self.inner.recently_played(kinds, offset, limit)
    }

    fn playlists(&self) -> BoxFuture<'_, Result<PlaylistList, BackendError>> {
        Box::pin(async move {
            match self.cached(Read::Playlists).await? {
                Value::Playlists(v) => Ok(*v),
                _ => Err(mismatch()),
            }
        })
    }

    fn playlist<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<PlaylistPage, BackendError>> {
        Box::pin(async move {
            match self
                .cached(Read::Playlist {
                    id: id.to_owned(),
                    offset,
                    limit,
                })
                .await?
            {
                Value::Playlist(v) => Ok(*v),
                _ => Err(mismatch()),
            }
        })
    }

    fn create_playlist<'a>(
        &'a self,
        name: &'a str,
        kinds: Kinds,
        visibility: Visibility,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        self.changing(move || self.inner.create_playlist(name, kinds, visibility))
    }

    fn update_playlist<'a>(
        &'a self,
        id: &'a str,
        update: PlaylistUpdate,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        self.changing(move || self.inner.update_playlist(id, update))
    }

    fn edit_playlist<'a>(
        &'a self,
        id: &'a str,
        revision: u64,
        edits: Vec<PlaylistEdit>,
    ) -> BoxFuture<'a, Result<u64, BackendError>> {
        self.changing(move || self.inner.edit_playlist(id, revision, edits))
    }

    fn delete_playlist<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.delete_playlist(id))
    }

    fn set_playlist_art<'a>(
        &'a self,
        id: &'a str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<ArtworkId, BackendError>> {
        self.changing(move || self.inner.set_playlist_art(id, bytes))
    }

    fn remove_playlist_art<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        self.changing(move || self.inner.remove_playlist_art(id))
    }

    fn pin_playlist<'a>(
        &'a self,
        id: &'a str,
        position: u32,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.pin_playlist(id, position))
    }

    fn unpin_playlist<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        self.changing(move || self.inner.unpin_playlist(id))
    }

    fn dismiss_pin_notes(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        self.changing(move || self.inner.dismiss_pin_notes())
    }

    fn play_playlist<'a>(
        &'a self,
        id: &'a str,
        from: Option<EntryId>,
        settings: PlaySettings,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>> {
        self.changing(move || self.inner.play_playlist(id, from, settings))
    }

    fn next_in_playlist<'a>(
        &'a self,
        id: &'a str,
        after: EntryId,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>> {
        self.changing(move || self.inner.next_in_playlist(id, after))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use spoolframe_types::{MediaKind, Namespace};

    use super::*;
    use crate::fake::FakeClient;
    use crate::testing::ready;

    /// A clock tests move by hand.
    struct TestClock(Mutex<Instant>);

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
    }

    impl TestClock {
        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }
    }

    /// Re-reads queued, run when the test says.
    #[derive(Clone, Default)]
    struct Queue(Arc<Mutex<Vec<BoxFuture<'static, ()>>>>);

    impl Queue {
        fn spawn(&self) -> Spawn {
            let queue = self.clone();
            Arc::new(move |future| queue.0.lock().unwrap().push(future))
        }

        fn len(&self) -> usize {
            self.0.lock().unwrap().len()
        }

        fn run(&self) {
            let queued: Vec<_> = self.0.lock().unwrap().drain(..).collect();
            for future in queued {
                ready(future);
            }
        }
    }

    struct Fixture {
        clock: Arc<TestClock>,
        queue: Queue,
        catalog: Arc<Catalog>,
    }

    fn fixture() -> Fixture {
        fixture_holding(CAPACITY)
    }

    fn fixture_holding(capacity: usize) -> Fixture {
        let clock = Arc::new(TestClock(Mutex::new(Instant::now())));
        let queue = Queue::default();
        let catalog = Catalog::with_capacity(clock.clone(), queue.spawn(), capacity);
        Fixture {
            clock,
            queue,
            catalog,
        }
    }

    fn scope(user: &str) -> Scope {
        Scope {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            user: user.into(),
        }
    }

    fn libraries(names: &[&str]) -> Value {
        Value::Libraries(
            names
                .iter()
                .map(|name| Library {
                    id: LibraryId::new(Namespace::Plex, *name).unwrap(),
                    title: (*name).into(),
                    kind: spoolframe_types::LibraryKind::Movies,
                })
                .collect(),
        )
    }

    /// A fetch answering `value`, counting its calls.
    fn answering(
        calls: &Arc<AtomicUsize>,
        value: Value,
    ) -> impl FnOnce() -> BoxFuture<'static, Result<Value, BackendError>> {
        let calls = calls.clone();
        move || {
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(value)
            })
        }
    }

    fn read(
        fixture: &Fixture,
        scope: &Scope,
        read: Read,
        calls: &Arc<AtomicUsize>,
        value: Value,
    ) -> Value {
        ready(fixture.catalog.read(scope, read, answering(calls, value))).unwrap()
    }

    #[test]
    fn a_fresh_read_is_answered_without_the_server() {
        let f = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["films"]),
        );
        f.clock.advance(Duration::from_secs(60));
        let kept = read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["new"]),
        );
        assert_eq!(
            (kept, calls.load(Ordering::SeqCst), f.queue.len()),
            (libraries(&["films"]), 1, 0)
        );
    }

    #[test]
    fn a_stale_read_is_answered_with_what_was_kept() {
        let f = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["films"]),
        );
        f.clock.advance(Duration::from_secs(11 * 60));
        let kept = read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["new"]),
        );
        assert_eq!(kept, libraries(&["films"]));
    }

    #[test]
    fn reads_of_a_stale_value_start_one_re_read_behind_it() {
        let f = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["films"]),
        );
        f.clock.advance(Duration::from_secs(11 * 60));
        // Both before the re-read is in, as two pages at once would.
        for _ in 0..2 {
            read(
                &f,
                &scope("andrey"),
                Read::Libraries,
                &calls,
                libraries(&["new"]),
            );
        }
        f.queue.run();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_re_read_that_differs_is_kept_and_said() {
        let f = fixture();
        let mut changes = f.catalog.changes();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["films"]),
        );
        f.clock.advance(Duration::from_secs(11 * 60));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["new"]),
        );
        f.queue.run();
        let change = changes.try_recv().unwrap();
        let now = read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["newer"]),
        );
        assert_eq!(
            (change, now),
            (
                Change {
                    scope: scope("andrey"),
                    read: Read::Libraries
                },
                libraries(&["new"])
            )
        );
    }

    #[test]
    fn a_re_read_that_is_the_same_is_not_said() {
        let f = fixture();
        let mut changes = f.catalog.changes();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["films"]),
        );
        f.clock.advance(Duration::from_secs(11 * 60));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["films"]),
        );
        f.queue.run();
        assert!(changes.try_recv().is_err());
    }

    #[test]
    fn reading_what_a_change_said_starts_no_other_re_read() {
        let f = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Home,
            &calls,
            libraries(&["films"]),
        );
        f.clock.advance(Duration::from_secs(11));
        read(
            &f,
            &scope("andrey"),
            Read::Home,
            &calls,
            libraries(&["new"]),
        );
        f.queue.run();
        // The page that heard of it reads it again, at once.
        read(
            &f,
            &scope("andrey"),
            Read::Home,
            &calls,
            libraries(&["newer"]),
        );
        assert_eq!(f.queue.len(), 0);
    }

    #[test]
    fn a_re_read_in_flight_when_the_session_is_cleared_is_not_kept() {
        let f = fixture();
        let mut changes = f.catalog.changes();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["films"]),
        );
        f.clock.advance(Duration::from_secs(11 * 60));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["before the change"]),
        );
        f.catalog.clear(&scope("andrey"));
        f.queue.run();
        assert_eq!(
            (f.catalog.len(&scope("andrey")), changes.try_recv().is_err()),
            (0, true)
        );
    }

    #[test]
    fn each_session_has_its_own_reads() {
        let f = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["andrey's"]),
        );
        let mums = read(
            &f,
            &scope("mum"),
            Read::Libraries,
            &calls,
            libraries(&["mum's"]),
        );
        assert_eq!(mums, libraries(&["mum's"]));
    }

    #[test]
    fn clearing_a_session_leaves_the_others() {
        let f = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        read(
            &f,
            &scope("andrey"),
            Read::Libraries,
            &calls,
            libraries(&["andrey's"]),
        );
        read(
            &f,
            &scope("mum"),
            Read::Libraries,
            &calls,
            libraries(&["mum's"]),
        );
        f.catalog.clear(&scope("andrey"));
        assert_eq!(
            (
                f.catalog.len(&scope("andrey")),
                f.catalog.len(&scope("mum"))
            ),
            (0, 1)
        );
    }

    #[test]
    fn over_capacity_the_least_recently_read_goes() {
        let f = fixture_holding(2);
        let calls = Arc::new(AtomicUsize::new(0));
        let (a, b, c) = (scope("a"), scope("b"), scope("c"));
        read(&f, &a, Read::Libraries, &calls, libraries(&["a"]));
        read(&f, &b, Read::Libraries, &calls, libraries(&["b"]));
        read(&f, &a, Read::Libraries, &calls, libraries(&["a"]));
        read(&f, &c, Read::Libraries, &calls, libraries(&["c"]));
        assert_eq!(
            (f.catalog.len(&a), f.catalog.len(&b), f.catalog.len(&c)),
            (1, 0, 1)
        );
    }

    fn detail(session: &Arc<dyn Session>, id: &MediaId) -> Value {
        Value::Detail(Box::new(ready(session.item_detail(id)).unwrap()))
    }

    fn media(kind: MediaKind, key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, kind, key).unwrap()
    }

    #[test]
    fn a_movie_is_fresh_for_two_minutes_and_a_show_for_five() {
        let session = FakeClient::new(None)
            .session(&FakeClient::profile("andrey"))
            .unwrap();
        let movie = detail(&session, &media(MediaKind::Movie, "1"));
        let show = detail(&session, &media(MediaKind::Show, "20"));
        assert_eq!(
            (movie.freshness(), show.freshness()),
            (Duration::from_secs(120), Duration::from_secs(300))
        );
    }

    // The wrapper, over the fake server.

    fn cached(f: &Fixture) -> (crate::testing::TestDir, CachedSession) {
        // Media, for the playbacks.
        let (dir, path) = crate::testing::file_with("catalog", b"stand-in");
        let inner = FakeClient::new(Some(path))
            .session(&FakeClient::profile("andrey"))
            .unwrap();
        (
            dir,
            CachedSession::new(inner, f.catalog.clone(), scope("andrey")),
        )
    }

    #[test]
    fn the_wrapper_keeps_what_it_reads() {
        let f = fixture();
        let (_media, session) = cached(&f);
        ready(session.home()).unwrap();
        ready(session.libraries()).unwrap();
        ready(session.playlists()).unwrap();
        assert_eq!(f.catalog.len(&scope("andrey")), 3);
    }

    #[test]
    fn a_search_is_not_kept() {
        let f = fixture();
        let (_media, session) = cached(&f);
        let request = SearchRequest {
            query: "alien".into(),
            limit: 10,
            scope: SearchScope::All,
            kind: None,
            library: None,
        };
        ready(session.search(&request)).unwrap();
        assert_eq!(f.catalog.len(&scope("andrey")), 0);
    }

    /// Each change through the wrapper, after Home and the playlists were
    /// read: what is kept afterwards.
    fn kept_after(change: impl FnOnce(&CachedSession)) -> usize {
        let f = fixture();
        let (_media, session) = cached(&f);
        ready(session.home()).unwrap();
        ready(session.playlists()).unwrap();
        change(&session);
        f.catalog.len(&scope("andrey"))
    }

    fn alien() -> MediaId {
        media(MediaKind::Movie, "1")
    }

    #[test]
    fn marking_watched_clears_the_session() {
        assert_eq!(kept_after(|s| ready(s.mark_watched(&alien())).unwrap()), 0);
    }

    #[test]
    fn marking_unwatched_clears_the_session() {
        assert_eq!(
            kept_after(|s| ready(s.mark_unwatched(&alien())).unwrap()),
            0
        );
    }

    #[test]
    fn hiding_from_continue_watching_clears_the_session() {
        assert_eq!(
            kept_after(|s| ready(s.hide_from_continue(&alien())).unwrap()),
            0
        );
    }

    #[test]
    fn a_playback_stopping_clears_the_session() {
        let kept = kept_after(|s| {
            let started = ready(s.start_playback(&alien(), None, Default::default())).unwrap();
            ready(s.home()).unwrap();
            ready(s.stop_playback(&started.handle)).unwrap();
        });
        assert_eq!(kept, 0);
    }

    #[test]
    fn a_playback_starting_clears_the_session() {
        assert_eq!(
            kept_after(|s| {
                ready(s.start_playback(&alien(), None, Default::default())).unwrap();
            }),
            0
        );
    }

    fn episodes() -> Kinds {
        Kinds {
            episodes: true,
            ..Kinds::default()
        }
    }

    #[test]
    fn making_a_playlist_clears_the_session() {
        let kept = kept_after(|s| {
            ready(s.create_playlist("Heists", episodes(), Visibility::Personal)).unwrap();
        });
        assert_eq!(kept, 0);
    }

    /// A playlist of the user's, made before the reads, then `change`d.
    fn kept_after_playlist_change(change: impl FnOnce(&CachedSession, &str)) -> usize {
        let f = fixture();
        let (_media, session) = cached(&f);
        let made =
            ready(session.create_playlist("Heists", episodes(), Visibility::Personal)).unwrap();
        ready(session.home()).unwrap();
        ready(session.playlists()).unwrap();
        change(&session, &made.id);
        f.catalog.len(&scope("andrey"))
    }

    #[test]
    fn renaming_a_playlist_clears_the_session() {
        let kept = kept_after_playlist_change(|s, id| {
            let update = PlaylistUpdate {
                name: Some("Capers".into()),
                ..PlaylistUpdate::default()
            };
            ready(s.update_playlist(id, update)).unwrap();
        });
        assert_eq!(kept, 0);
    }

    #[test]
    fn pinning_a_playlist_clears_the_session() {
        let kept = kept_after_playlist_change(|s, id| ready(s.pin_playlist(id, 0)).unwrap());
        assert_eq!(kept, 0);
    }

    #[test]
    fn deleting_a_playlist_clears_the_session() {
        let kept = kept_after_playlist_change(|s, id| ready(s.delete_playlist(id)).unwrap());
        assert_eq!(kept, 0);
    }

    #[test]
    fn playing_a_playlist_clears_the_session() {
        let kept = kept_after_playlist_change(|s, id| {
            ready(s.play_playlist(id, None, PlaySettings::default())).unwrap();
        });
        assert_eq!(kept, 0);
    }

    #[test]
    fn editing_a_playlist_clears_the_session() {
        let kept = kept_after_playlist_change(|s, id| {
            let page = ready(s.playlist(id, 0, 10)).unwrap();
            let edits = vec![PlaylistEdit::Add {
                id: media(MediaKind::Episode, "25"),
                before: None,
            }];
            let _ = ready(s.edit_playlist(id, page.playlist.revision, edits));
        });
        assert_eq!(kept, 0);
    }

    #[test]
    fn liking_clears_the_session() {
        let track = media(MediaKind::Track, "42");
        assert_eq!(
            kept_after(|s| {
                ready(s.like(&[track])).unwrap();
            }),
            0
        );
    }

    #[test]
    fn unliking_clears_the_session() {
        let track = media(MediaKind::Track, "42");
        assert_eq!(
            kept_after(|s| {
                ready(s.unlike(&[track])).unwrap();
            }),
            0
        );
    }

    /// A small PNG's first bytes: enough for the fake's check.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    #[test]
    fn setting_a_cover_clears_the_session() {
        let kept = kept_after_playlist_change(|s, id| {
            ready(s.set_playlist_art(id, PNG.to_vec())).unwrap();
        });
        assert_eq!(kept, 0);
    }

    #[test]
    fn removing_a_cover_clears_the_session() {
        let kept = kept_after_playlist_change(|s, id| {
            ready(s.remove_playlist_art(id)).unwrap();
        });
        assert_eq!(kept, 0);
    }

    fn alien_subtitle_result(s: &CachedSession) -> String {
        ready(s.search_subtitles(&alien(), None)).unwrap().results[0]
            .id
            .clone()
    }

    #[test]
    fn a_subtitle_search_is_not_kept() {
        assert_eq!(
            kept_after(|s| {
                ready(s.search_subtitles(&alien(), None)).unwrap();
            }),
            2
        );
    }

    #[test]
    fn downloading_a_subtitle_clears_the_session() {
        assert_eq!(
            kept_after(|s| {
                let result = alien_subtitle_result(s);
                ready(s.download_subtitle(&alien(), &result)).unwrap();
            }),
            0
        );
    }

    #[test]
    fn removing_a_downloaded_subtitle_clears_the_session() {
        let kept = kept_after(|s| {
            let result = alien_subtitle_result(s);
            let got = ready(s.download_subtitle(&alien(), &result)).unwrap();
            ready(s.home()).unwrap();
            let download = got.subtitle.download.unwrap().id;
            ready(s.remove_subtitle_download(&alien(), &download)).unwrap();
        });
        assert_eq!(kept, 0);
    }

    #[test]
    fn offsetting_a_subtitle_clears_the_session() {
        assert_eq!(
            kept_after(|s| {
                ready(s.set_subtitle_offset(&alien(), 0, 1500)).unwrap();
            }),
            0
        );
    }

    #[test]
    fn syncing_a_subtitle_clears_the_session() {
        assert_eq!(
            kept_after(|s| {
                ready(s.sync_subtitle(&alien(), 0)).unwrap();
            }),
            0
        );
    }

    #[test]
    fn its_original_timing_clears_the_session() {
        assert_eq!(
            kept_after(|s| {
                ready(s.use_original_subtitle_timing(&alien(), 0)).unwrap();
            }),
            0
        );
    }

    #[test]
    fn a_failed_change_clears_the_session_too() {
        // It may have been made before its answer was lost.
        assert_eq!(
            kept_after(|s| assert!(ready(s.delete_playlist("nope")).is_err())),
            0
        );
    }

    #[test]
    fn a_change_from_elsewhere_clears_the_session_when_told() {
        assert_eq!(kept_after(CachedSession::clear), 0);
    }
}
