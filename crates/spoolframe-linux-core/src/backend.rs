//! What the UI talks to. `spoolframe-client-sdk` implements it, through
//! the thin adapter in [`crate::sdk`]; tests use a fake server answering
//! from fixed data (`fake`, behind the `testing` feature). The shapes
//! follow the SDK's API and use the protocol's own types, so the adapter
//! only passes calls through.
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
    ArtworkSize, ChosenTracks, DeviceInfo, DeviceSignInRequest, HomeScreen, PlaybackHandle,
    PlaybackPlan, Selection, TrackReason,
};
pub use spoolframe_protocol::{
    PinProof, PinState, PlexLink, PlexSyncSummary, PreviewTile, Previews, SearchCounts,
    SearchGroup, SearchRequest, SearchResults, TopResult,
};
pub use spoolframe_types::Platform;
use spoolframe_types::{
    ArtworkId, Item, Library, LibraryId, MediaId, MediaTracks, SubtitleFormat, TrackPreferences,
};
pub use spoolframe_types::{
    Marker, MarkerKind, Person, PersonId, PersonRole, SearchKind, SearchScope,
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
    /// This profile needs its PIN on this device before anything else
    /// (P2-08).
    #[error("this profile is locked; enter its PIN")]
    ProfileLocked,
    /// The server refused what was sent, saying why (a PIN that is not 4
    /// to 8 digits).
    #[error("{0}")]
    InvalidInput(String),
    /// The server is converting as many files as it may (P2-03).
    #[error("the server is busy converting other files; try again shortly")]
    Busy,
    /// A seek restarted a converted playback's encode (P2-04): read
    /// `epoch` from offset 0.
    #[error("the playback restarted (epoch {epoch})")]
    PlaybackRestarted { epoch: u32 },
    /// The subtitle is still being read as text (P2-07): ask again in a
    /// few seconds.
    #[error("the subtitle is still being read ({0}% done)")]
    SubtitleNotReady(u8),
    /// Something the server depends on is down (one of its Plex servers,
    /// a media mount), in the server's words.
    #[error("{0}")]
    Unavailable(String),
    /// The server is not saved on this device yet: its fingerprint has to
    /// be checked and the server pinned first (P1-21).
    #[error("{0} is not saved on this device yet")]
    NotPinned(String),
    /// A saved server answered with another key (P1-13): reinstalled, or
    /// another server. Both are keys, hex; the user may trust the new one.
    #[error("the server answered with another key")]
    IdentityChanged { saved: String, now: String },
    /// The server runs, but its admin has not finished setup (P3-10): it
    /// has no catalog yet. Signing in still works.
    #[error("this server is not set up yet")]
    NotSetUp,
    /// The item's preview frames (P3-13) are still being made: ask again
    /// in a few seconds.
    #[error("the previews are still being made ({0}% done)")]
    PreviewsNotReady(u8),
    /// The item has no preview frames (music, a file that cannot be read,
    /// or previews turned off): do not ask again.
    #[error("there are no previews for this item")]
    NoPreviews,
    /// The sign-in this device asked another to approve was refused
    /// (P3-07).
    #[error("the sign-in was refused")]
    SignInDenied,
    /// Nobody approved the sign-in in time (P3-07): begin again for a new
    /// code.
    #[error("the sign-in code expired")]
    SignInExpired,
    /// The account has no password to prove it with (passkey or provider
    /// only); the PIN, or My profile in the web UI, does instead.
    #[error("this account has no password")]
    NoPassword,
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
    /// When the user said its fingerprint matched; `None` if they have not,
    /// or it was saved before the date was kept.
    pub verified_at: Option<std::time::SystemTime>,
    /// When this device last connected to it.
    pub last_connected_at: Option<std::time::SystemTime>,
}

/// A user signed in on a server from this device: one profile on "Who's
/// watching", backed by one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub server: ServerKey,
    pub username: String,
    pub display_name: String,
    /// Its PIN is asked for on this device before anything of it shows
    /// (P2-08). As the server said when the profiles were listed.
    pub locked: bool,
    pub sign_in: ProfileSignIn,
}

/// How choosing a profile signs it in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProfileSignIn {
    /// Its session is kept: it opens without asking (or with its PIN).
    #[default]
    Automatic,
    /// The server ended its session: the user signs in again, and only
    /// the profile's name is kept here.
    Password,
}

/// A sign-in this device began and another approves (P3-05, P3-07): a
/// phone opens `web_url` and signs in there, or a signed-in Spoolframe
/// app takes the code. [`Client::await_device_sign_in`] waits for it.
#[derive(Clone)]
pub struct DeviceSignIn {
    pub server: ServerKey,
    /// The code as shown, large: `BCDF-GHJK`.
    pub display_code: String,
    /// The server's web page that approves this code, for a QR code any
    /// phone's camera opens; `None` when the server has no public web
    /// address, and only a Spoolframe app can approve.
    pub web_url: Option<String>,
    /// How long it can be approved, from when it began.
    pub expires_in: Duration,
    /// The backend's own record of the request.
    pub(crate) request: Arc<dyn std::any::Any + Send + Sync>,
}

impl std::fmt::Debug for DeviceSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceSignIn")
            .field("server", &self.server)
            .field("display_code", &self.display_code)
            .field("web_url", &self.web_url)
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// How the device's connection to a server is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Closed,
}

/// How packets go to the server now, for the player's diagnostics. A
/// hole punched through NAT and an open port look the same: `Direct` is
/// any public address, `Lan` a private, link-local or loopback one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionPath {
    Lan(std::net::SocketAddr),
    Direct(std::net::SocketAddr),
    Relay,
    /// Not connected now, or no path chosen yet.
    Unknown,
}

pub use spoolframe_client_sdk::{Chunk, Playback as PlaybackStart};
pub use spoolframe_protocol::MAX_READ;

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

    /// Forgets saved server `server`: its pin, so its fingerprint is
    /// checked again if it is added back, and every session on it.
    fn forget<'a>(&'a self, server: &'a ServerKey) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Signs every user on `server` out, on the server and here; the
    /// server stays saved.
    fn sign_out<'a>(&'a self, server: &'a ServerKey) -> BoxFuture<'a, Result<(), BackendError>>;

    /// How this device reaches `server` now, before any profile is
    /// chosen. Asks nothing and never dials.
    fn connection_path(&self, server: &ServerKey) -> ConnectionPath;

    /// Forgets a profile kept on this device for `server`: its session,
    /// if any, and its name.
    fn forget_profile<'a>(
        &'a self,
        server: &'a ServerKey,
        username: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// The users signed in on `server` from this device, and those whose
    /// session the server ended (to sign in again).
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

    /// Begins a sign-in another device approves (P3-05, P3-07): what to
    /// show while it waits. A server holds at most three per device, so
    /// begin one per screen, not per redraw.
    fn begin_device_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
    ) -> BoxFuture<'a, Result<DeviceSignIn, BackendError>>;

    /// Waits until `sign_in` is approved, adding its profile as a password
    /// sign-in does; [`BackendError::SignInDenied`] or
    /// [`BackendError::SignInExpired`] otherwise. Drop the future to stop
    /// waiting.
    fn await_device_sign_in<'a>(
        &'a self,
        sign_in: &'a DeviceSignIn,
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
    /// Explicit foreground selection; background calls never select a profile.
    fn activate(&self) -> BoxFuture<'_, Result<(), BackendError>>;

    /// Where a browser reaches the server's web UI, if it has a public
    /// address: for "open in the browser" links (P3-12's Plex account).
    fn web_url(&self) -> Option<String>;

    /// How this device reaches the server now. Asks nothing.
    fn connection_path(&self) -> ConnectionPath;

    /// The item's preview frames for scrubbing (P3-13). Asking also puts
    /// the item first in the server's queue to make them.
    fn previews<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<Previews, BackendError>>;

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

    /// A search (P3-09): in a scope, or one group for "See all"; the top
    /// match, then groups of shows, movies, people, artists, albums,
    /// tracks and episodes, with counts per scope.
    fn search<'a>(
        &'a self,
        request: &'a SearchRequest,
    ) -> BoxFuture<'a, Result<SearchResults, BackendError>>;

    /// A page of the items a person a search found is in, and how many
    /// there are in all.
    fn person_items<'a>(
        &'a self,
        person: &'a PersonId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>>;

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
    /// the user's preferences and series override (`None`). A converted
    /// playback's encode begins at `start` (P2-03); direct play reads from
    /// wherever the player likes, and the server ignores it.
    fn start_playback<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
        start: Option<Duration>,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>>;

    /// Up to `length` bytes of a converted playback's output from
    /// `offset`, for its `epoch` (P2-04). A read past what is encoded
    /// waits a while on the server and may come back empty and not
    /// `complete`: ask again. [`BackendError::PlaybackRestarted`] means a
    /// seek replaced `epoch`.
    fn read_chunk<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        epoch: u32,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Chunk, BackendError>>;

    /// Restarts a converted playback's encode at `position`; reads go on
    /// with the returned epoch, from offset 0.
    fn seek_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        position: Duration,
    ) -> BoxFuture<'a, Result<u32, BackendError>>;

    /// Ends a playback now, and its encode. Otherwise it ends when it has
    /// not been read for a while.
    fn stop_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

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
        handle: &'a PlaybackHandle,
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

    /// Unlocks this profile on this device with its PIN (P2-08). A wrong
    /// PIN is [`BackendError::WrongCredentials`], too many in a while
    /// [`BackendError::RateLimited`]; the tenth wrong in a row ends the
    /// session here ([`BackendError::SignInRequired`]).
    fn unlock<'a>(&'a self, pin: &'a str) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Whether the user has a PIN, and how many digits; `None` for none.
    fn pin(&self) -> BoxFuture<'_, Result<Option<PinState>, BackendError>>;

    /// Sets or changes the user's PIN, 4 to 8 digits. The first PIN takes no
    /// proof; changing one takes the current PIN or the account password.
    /// A wrong PIN counts toward [`Session::unlock`]'s lockout, a wrong
    /// password toward the sign-in limit; either is
    /// [`BackendError::WrongCredentials`].
    fn set_pin<'a>(
        &'a self,
        pin: &'a str,
        proof: Option<PinProof>,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Removes the user's PIN, and with it every device's requirement, on
    /// the current PIN or the account password.
    fn clear_pin(&self, proof: PinProof) -> BoxFuture<'_, Result<(), BackendError>>;

    /// The user's Plex account link, if any (P3-12); linking is the web
    /// UI's.
    fn plex_link(&self) -> BoxFuture<'_, Result<Option<PlexLink>, BackendError>>;

    /// The devices the user has signed in from, and where the PIN is
    /// asked for.
    fn devices(&self) -> BoxFuture<'_, Result<Vec<DeviceInfo>, BackendError>>;

    /// Asks for the PIN on `device` (an endpoint id from
    /// [`Session::devices`]; this device for `None`), or stops asking.
    fn set_pin_required<'a>(
        &'a self,
        device: Option<&'a str>,
        required: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// The device asking to be signed in with `code` (P3-07), normalized:
    /// what it calls itself and when it asked. A code that is not pending
    /// is [`BackendError::NotFound`]; too many wrong codes,
    /// [`BackendError::RateLimited`].
    fn lookup_device_sign_in<'a>(
        &'a self,
        code: &'a str,
    ) -> BoxFuture<'a, Result<DeviceSignInRequest, BackendError>>;

    /// Signs that device in as this user. Approving the device that shows
    /// the code is [`BackendError::InvalidInput`].
    fn approve_device_sign_in<'a>(
        &'a self,
        code: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Refuses it: the device is told so.
    fn deny_device_sign_in<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<(), BackendError>>;
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
