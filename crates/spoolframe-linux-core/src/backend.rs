//! What the UI talks to. Today the fakes in [`crate::fake`] implement it;
//! `spoolframe-client-sdk` will, through a thin adapter, once it lands
//! (P1-15). The shapes follow the SDK's API and use the protocol's own
//! types, so that adapter only passes calls through.
//!
//! The split follows ARCHITECTURE §13–16: a [`Client`] is this device, with
//! its one endpoint and the servers it knows; a [`Session`] is one user
//! signed in on one server from this device. Several sessions share the
//! device's connection, so switching profile never reconnects.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub use spoolframe_protocol::{
    ArtworkSize, ChosenTracks, HomeScreen, PlaybackHandle, PlaybackPlan, Selection, TrackReason,
};
use spoolframe_types::{
    ArtworkId, Item, Library, LibraryId, MediaId, MediaTracks, SubtitleFormat, TrackPreferences,
};
use tokio::sync::watch;

/// A boxed future, so that the UI can hold a backend as `dyn Client` or
/// `dyn Session`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Why a call failed, in the kinds the UI acts on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("{0} does not exist on this server")]
    NotFound(String),
    #[error("{0} cannot be reached")]
    Unreachable(String),
    #[error("the username or password is wrong")]
    WrongCredentials,
    #[error("too many failed sign-ins; try again in {} s", .0.as_secs().max(1))]
    RateLimited(Duration),
    /// The session is gone (signed out elsewhere, expired, user disabled):
    /// the user has to sign in again.
    #[error("you have been signed out; sign in again")]
    SignInRequired,
    /// This device cannot play the item; one reason per stream.
    #[error("cannot play: {}", .0.join("; "))]
    CannotPlay(Vec<String>),
    /// A playback handle idled out. Start playback again and carry on.
    #[error("the playback has expired")]
    PlaybackExpired,
    /// Something the server depends on is down (Plex, a media mount).
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// The server is not saved on this device yet: its fingerprint has to
    /// be checked and the server pinned first (P1-21).
    #[error("{0} is not saved on this device yet")]
    NotPinned(String),
    /// A saved server answered with another key (P1-13): reinstalled, or
    /// another server. Both are keys, hex; the user may trust the new one.
    #[error("the server answered with another key")]
    IdentityChanged { saved: String, now: String },
    #[error("{0}")]
    Other(String),
}

/// A server's identity: its endpoint key (§12). Addresses change; this does
/// not, and a client only ever talks to the key it pinned (§19).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerKey(pub String);

impl ServerKey {
    /// The key shortened for people to compare, as `k5d2…q7fa`.
    pub fn short(&self) -> String {
        let key = &self.0;
        if key.chars().count() <= 9 {
            return key.clone();
        }
        let head: String = key.chars().take(4).collect();
        let tail: String = key.chars().skip(key.chars().count() - 4).collect();
        format!("{head}…{tail}")
    }
}

/// A server this device found on the network or remembers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub key: ServerKey,
    pub name: String,
    /// Where it was last seen. Hints for dialing, never its identity.
    pub addresses: Vec<String>,
    /// Whether this device has pinned the server's key (§19).
    pub pinned: bool,
    /// The key's fingerprint, to compare with the server's own
    /// (`spoolframe-server fingerprint`, P1-21).
    pub fingerprint: String,
    /// Pinned after the user said the fingerprint matched; a pinned server
    /// that is not shows "Not verified".
    pub verified: bool,
    /// Whether it answered on this network just now (mDNS).
    pub on_network: bool,
}

/// A user signed in on a server from this device: one profile on "Who's
/// watching", backed by one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub server: ServerKey,
    pub username: String,
    pub display_name: String,
}

/// How the device's connection to a server is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Closed,
}

/// A started playback: what to read, and how much there is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackStart {
    pub handle: PlaybackHandle,
    pub plan: PlaybackPlan,
    /// The size of the file the handle reads, in bytes.
    pub size: u64,
    /// The tracks the server chose and why, when none were asked for.
    pub chosen: Option<ChosenTracks>,
}

/// The longest read the server serves at once (P1-08).
pub const MAX_READ: u64 = 16 * 1024 * 1024;

/// This device: the servers it knows, and the users signed in on them.
pub trait Client: Send + Sync {
    /// Servers on this network, and saved ones.
    fn servers(&self) -> BoxFuture<'_, Result<Vec<Server>, BackendError>>;

    /// Remembers an address for a server the network did not show, keyed by
    /// the server's key, which the first connect then pins.
    fn add_address<'a>(
        &'a self,
        server: &'a ServerKey,
        address: &'a str,
    ) -> BoxFuture<'a, Result<Server, BackendError>>;

    /// Connects to `server` and says who it is, its fingerprint included,
    /// without saving anything (P1-21): the user checks it first.
    fn connect<'a>(&'a self, server: &'a ServerKey) -> BoxFuture<'a, Result<Server, BackendError>>;

    /// Saves `server` with its key pinned, verified or not; on a saved
    /// server, only marks it verified.
    fn pin<'a>(
        &'a self,
        server: &'a ServerKey,
        verified: bool,
    ) -> BoxFuture<'a, Result<Server, BackendError>>;

    /// Replaces saved server `saved`'s pin with `now`, which answered in its
    /// place, after the user checked it (P1-13). Its sessions are gone.
    fn trust_new_identity<'a>(
        &'a self,
        saved: &'a ServerKey,
        now: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Server, BackendError>>;

    /// The users signed in on `server` from this device.
    fn profiles<'a>(
        &'a self,
        server: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<Profile>, BackendError>>;

    /// Signs a user in on `server` with a password (§22), adding a profile.
    fn sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        username: &'a str,
        password: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>>;

    /// The session behind a profile.
    fn session(&self, profile: &Profile) -> Result<Arc<dyn Session>, BackendError>;

    /// The state of the connection to `server`, as it changes.
    fn connection(&self, server: &ServerKey) -> watch::Receiver<ConnectionState>;

    /// Changes when [`Client::servers`] would answer differently: a server
    /// heard, changed or gone on the LAN, an address added, a pin saved or
    /// forgotten. Discovery takes a moment after opening, so the first
    /// answer is often empty; list again on each change.
    fn servers_changed(&self) -> watch::Receiver<()>;
}

/// One signed-in user's view of a server: the catalog, their watch state
/// and playback.
pub trait Session: Send + Sync {
    /// Continue watching, next up and recently added, with the user's state.
    fn home(&self) -> BoxFuture<'_, Result<HomeScreen, BackendError>>;

    fn libraries(&self) -> BoxFuture<'_, Result<Vec<Library>, BackendError>>;

    /// A page of a library's top-level items (movies, shows or artists), and
    /// how many there are in all.
    fn items<'a>(
        &'a self,
        library: &'a LibraryId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>>;

    /// An item and its children: a show's seasons, a season's episodes, an
    /// artist's albums, an album's tracks.
    fn item<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(Item, Vec<Item>), BackendError>>;

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<Item>, BackendError>>;

    /// The item's audio and subtitle tracks, for the pickers.
    fn tracks<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<MediaTracks, BackendError>>;

    /// How the server would play `id` with these tracks, or with the ones
    /// it would choose (`None`), and which it chose and why (P1-20).
    fn playback_plan<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
    ) -> BoxFuture<'a, Result<(PlaybackPlan, Option<ChosenTracks>), BackendError>>;

    /// Starts playback with these tracks, or with the server's choice from
    /// the user's preferences and series override (`None`).
    fn start_playback<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>>;

    /// Up to [`MAX_READ`] bytes of a playback's file from `offset`; fewer
    /// only at the end of the file.
    fn read<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>>;

    /// The playback's selected external subtitle, whole.
    fn read_subtitle<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(SubtitleFormat, Vec<u8>), BackendError>>;

    /// An image, at the smallest size that covers what is drawn.
    fn artwork<'a>(
        &'a self,
        id: &'a ArtworkId,
        size: ArtworkSize,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>>;

    fn update_progress<'a>(
        &'a self,
        id: &'a MediaId,
        position: Duration,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    fn mark_watched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>>;

    fn mark_unwatched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Takes `id` out of Continue watching and Next up for this user, its
    /// progress kept (P1-22); an episode takes its whole show. It comes
    /// back with its next progress or mark.
    fn hide_from_continue<'a>(&'a self, id: &'a MediaId)
    -> BoxFuture<'a, Result<(), BackendError>>;

    /// Undoes [`Session::hide_from_continue`].
    fn unhide_from_continue<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// The user's audio and subtitle preferences.
    fn track_preferences(&self) -> BoxFuture<'_, Result<TrackPreferences, BackendError>>;

    fn set_track_preferences<'a>(
        &'a self,
        preferences: &'a TrackPreferences,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Remembers these tracks for the item: for an episode, for its whole
    /// series (as language and role, so other episodes match).
    fn set_track_override<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Selection,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Forgets the item's (or its series') tracks: back to the preferences.
    fn clear_track_override<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_key_is_shortened_to_its_ends() {
        let key = ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into());
        assert_eq!(key.short(), "k5d2…q7fa");
    }

    #[test]
    fn a_short_key_is_shown_whole() {
        assert_eq!(ServerKey("abc".into()).short(), "abc");
    }
}
