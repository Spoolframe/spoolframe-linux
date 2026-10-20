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
    AddressNetwork, DeviceAddress, TrackPreferencesInfo, TrustedProfile,
};
pub use spoolframe_protocol::{
    ArtworkSize, ChosenTracks, DeviceInfo, DeviceSignInRequest, HomeScreen, PlaybackHandle,
    PlaybackPlan, Selection, TrackReason,
};
pub use spoolframe_protocol::{
    FinishPasskeyRegistration, FinishPasskeySignIn, PasskeyCreation, PasskeyInfo, PasskeyRequest,
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
/// Subtitles from subtitle sites (PX-02) and their timing (PX-03), in the
/// types' own shapes.
pub use spoolframe_types::{
    QuotaOf, SubtitleDownload, SubtitleDownloaded, SubtitleOrigin, SubtitleResult, SubtitleSearch,
    SubtitleSource, SubtitleSourceState, SubtitleSourceStatus, SubtitleSync, SubtitleTiming,
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
    /// A request had no answer in time (P1-15): the connection may have
    /// died quietly. The SDK checks it and dials again; an action may
    /// still have gone through, so re-read before offering it again.
    #[error("{0} did not answer in time")]
    TimedOut(String),
    /// The server's admin stopped this playback (P3-15); nothing plays it
    /// again.
    #[error("playback was stopped by the server's admin")]
    PlaybackStopped,
    #[error("the username or password is wrong")]
    WrongCredentials,
    /// Too many failed sign-ins, or (PX-10) too many changes in a row from
    /// this user or device: wait, then try again.
    #[error("too many tries; try again in {} s", .0.as_secs().max(1))]
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
    /// Off its home network the server plays only `max` playbacks at once
    /// (PX-46), and that many are playing. At home this never happens.
    #[error(
        "Too many people are watching from outside the server’s home right now (it allows \
         {max} at once). Try again when someone stops, or watch at home."
    )]
    WanStreamsFull { max: u32 },
    /// The server is converting as many files as it may (P2-03).
    #[error("the server is busy converting other files; try again shortly")]
    Busy,
    /// This app and the server speak different protocol revisions, so
    /// they cannot talk; one of them needs updating. `server_protocol` is
    /// the server's revision when it said it: above this app's
    /// [`spoolframe_protocol::PROTOCOL_VERSION`] the app is the older.
    #[error("the server speaks another protocol revision; update Spoolframe or the server")]
    UpdateRequired { server_protocol: Option<u32> },
    /// Pasted or opened text is not a Spoolframe invite link (P6-01);
    /// nothing was sent.
    #[error("this isn’t a Spoolframe invite")]
    NotAnInvite,
    /// The invite's code is wrong or was revoked (P6-01). Saving the server
    /// still works.
    #[error("this invite doesn’t work")]
    InviteInvalid,
    /// The invite's code has expired (P6-01).
    #[error("this invite has expired")]
    InviteExpired,
    /// Every account the invite could make is made (P6-01).
    #[error("this invite has been used")]
    InviteExhausted,
    /// Someone on the server has the username asked for; the invite is
    /// still unused (P6-01).
    #[error("that username is taken")]
    UsernameTaken,
    /// The server takes no more connections from devices not yet signed
    /// in (PX-10): try again later, by hand.
    #[error("the server is busy; try again later")]
    ServerFull,
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
    /// A saved server answered with its key but another server id
    /// (PX-14): not the same server, and never trusted as it. Removing it
    /// and adding it again is the way on.
    #[error("the server answered with another server id")]
    ServerIdChanged { saved: String, now: String },
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
    /// Adding or removing a passkey needs a sign-in in the last 10
    /// minutes (P3-19): sign in again, then ask.
    #[error("confirm it's you: sign in again first")]
    RecentSignInRequired,
    /// The passkey is the user's last way to sign in, and is kept.
    #[error("this is your only way to sign in")]
    LastSignInMethod,
    /// This device has no live sign-in for the portable identity (P7-08):
    /// none here, past its time, or revoked. Pairing it again gives one.
    #[error("this computer needs to be paired with your identity again")]
    NotPaired,
    /// An admin reset the password to a one-time one (c86e6d3a): this
    /// session may only change it, or sign out.
    #[error("choose a new password first")]
    PasswordChangeRequired,
    /// Only the server's admin may do this (PX-44's invites).
    #[error("only the server’s admin can do that")]
    AdminOnly,
    /// Leaving the server (PX-47) is refused to its last enabled admin:
    /// someone else has to be made an admin first.
    #[error("You’re the only admin. Make someone else an admin first.")]
    LastAdmin,
    /// The server does not have this feature (PX-16): nothing was sent.
    /// What needs it is hidden.
    #[error("this server does not have {0}")]
    Unsupported(String),
    /// The user sees the playlist but may not change it this way (PX-40):
    /// an edit of a read-only share, or a rename, sharing change or
    /// delete by anyone but its owner (or an admin, for a server-wide
    /// one).
    #[error("you can’t change this playlist")]
    PlaylistReadOnly,
    /// The playlist does not take items of this kind (PX-40).
    #[error("this playlist doesn’t take {}", kind_name(*.0))]
    KindNotAllowed(ItemKind),
    /// Someone changed the same entries since the revision the edit was
    /// made on (PX-40); `revision` is the playlist's now. Nothing changed.
    #[error("Someone changed this playlist; reload.")]
    PlaylistConflict { revision: u64 },
    /// Not the user's to do (PX-53): a playlist's cover changed by
    /// someone who may not edit it, or on Liked or All. Nothing changed.
    #[error("You can’t do that.")]
    NotAllowed,
    /// A picture past the server's limits (PX-53): over `max_bytes`, or
    /// over `max_pixels` by its header. Nothing changed.
    #[error(
        "That picture is too large: at most {} MB and {} megapixels.",
        .max_bytes / (1 << 20),
        .max_pixels / 1_000_000
    )]
    ImageTooLarge { max_bytes: u64, max_pixels: u64 },
    /// Not a JPEG, PNG, WebP or HEIC the server can read (PX-53).
    #[error("That picture can’t be read. Choose a JPEG, PNG, WebP or HEIC.")]
    ImageUnreadable,
    /// The server has no subtitle site set up (PX-02): "Search for
    /// subtitles" says it is off. Nothing was asked of any site.
    #[error("Subtitle search is off on this server.")]
    NoSubtitleSource,
    /// The downloads of `whose` are used up for today (PX-02): the
    /// server's account at subtitle site `site`, or the user's own daily
    /// limit. Searching still works. The SDK (0.12.0) passes this on as
    /// a server message only, so `site` and `resets_at` are `None` from
    /// it: see `sdk::backend_error`.
    #[error("No subtitle downloads are left today.")]
    QuotaExhausted {
        whose: QuotaOf,
        site: Option<String>,
        resets_at: Option<i64>,
    },
    #[error("{0}")]
    Other(String),
}

/// An item kind as a refusal names it: "films", "episodes".
fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Movie => "films",
        ItemKind::Show | ItemKind::Season | ItemKind::Episode => "episodes",
        ItemKind::Artist | ItemKind::Album | ItemKind::Track => "music",
        ItemKind::Unknown => "this kind of item",
    }
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
    /// Its server id (PX-14): stable across key changes, and the name of
    /// its passkeys' relying party. `None` until it has been reached.
    pub server_id: Option<String>,
    /// The addresses the server says it has, in its order (protocol 7):
    /// the latest list it pushed. Empty until it has told this device.
    pub advertised: Vec<String>,
    /// The relays it says it can be reached through; none while relays
    /// are off.
    pub relays: Vec<String>,
    /// When it last told this device its addresses.
    pub advertised_at: Option<std::time::SystemTime>,
    /// Whether it is reached through relays: `Some` whatever the app's
    /// "Use relays" says, `None` as it says.
    pub use_relays: Option<bool>,
    /// `relays` is the user's own list: what the server says does not
    /// change it.
    pub relays_edited: bool,
}

impl Server {
    /// Where to say the server is: the first address it advertises, else
    /// the first this device has for it (a reached one may be stale).
    pub fn shown_address(&self) -> Option<&str> {
        self.advertised
            .first()
            .or(self.addresses.first())
            .map(String::as_str)
    }

    /// The relying party its passkeys are for, `<server id>.spoolframe.app`,
    /// once its id is known.
    pub fn passkey_site(&self) -> Option<String> {
        let id = spoolframe_protocol::ServerId::parse(self.server_id.as_deref()?).ok()?;
        Some(id.rp_id())
    }
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
    /// This device is trusted for the user (P4-01): the profile opens
    /// without a password, with [`Client::resume_profile`].
    pub trust: Option<Trust>,
}

/// A profile remembered on this device (P4-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    /// What [`Client::resume_profile`] takes.
    pub user_id: String,
    /// The PIN is asked for here when it opens (P2-08).
    pub pin_required: bool,
    /// It opens when Spoolframe starts; one profile per device at most.
    pub auto_login: bool,
    /// Its last sign-in here was a passkey, as this device saved it: Who's
    /// watching says so.
    pub by_passkey: bool,
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
    /// As `Password`, for someone whose last sign-in here was a passkey:
    /// they are offered a passkey again.
    Passkey,
    /// No session, but the device is trusted for them (P4-01): it opens
    /// with [`Client::resume_profile`], without a password.
    Resume,
    /// Its session ended, and it last signed in as this portable identity
    /// (P7-08): [`Client::sign_in_portable`] opens it again, no password.
    Portable(spoolframe_protocol::portable::PortableUserId),
}

/// A sign-in this device began and another approves (P3-05, P3-07): a
/// phone opens `web_url` and signs in there, or a signed-in Spoolframe
/// app takes the code. [`Client::await_device_sign_in`] waits for it.
#[derive(Clone)]
pub struct DeviceSignIn {
    pub server: ServerKey,
    /// The code as shown, large: `BCDF-GHJK`.
    pub display_code: String,
    /// This device asked to be remembered; the approver decides (P4-01).
    pub remember: bool,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting {
        attempt: u32,
    },
    Closed,
    /// Not connected, and not dialling again by itself, for a reason that
    /// dialling does not fix (the app needs updating, the server is not
    /// the one saved, the device was signed out): shown in place of
    /// "Reconnecting". The next call dials once, and fails the same way.
    Stopped(BackendError),
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

/// A subtitle track's timing changed (PX-03), as a server told.
pub use spoolframe_client_sdk::SubtitleTimingEvent;
pub use spoolframe_client_sdk::{
    Chunk, Playback as PlaybackStart, PlaybackChange, PlaybackOptions, PlaybackQuality,
};
/// Why a server can or cannot be reached, as the SDK keeps it: plain
/// data, shown as it is.
pub use spoolframe_client_sdk::{
    DialReport, DialStage, KnownPath, LivePath, LookupOutcome, LookupResult, PathKind,
    Reachability, SavedAddr,
};

/// The most a subtitle may be moved either way (PX-03): ten minutes, ms.
pub const MAX_SUBTITLE_OFFSET_MS: i64 = 10 * 60 * 1000;

/// Playlists (PX-40), in the protocol's and the SDK's own types.
pub use spoolframe_client_sdk::{
    Features, PlaylistChange, PlaylistEvent, PlaylistList, PlaylistUpdate,
};
pub use spoolframe_protocol::MAX_READ;
pub use spoolframe_protocol::playlists::{
    Builtin, EntryId, ItemKind, Kinds, Liked, PinNote, PinNoteReason, PlaySettings, PlaylistEdit,
    PlaylistEntry, PlaylistNext, PlaylistOwnerKind, PlaylistPage, PlaylistRef, PlaylistShare,
    PlaylistSummary, Repeat, ShareCandidate, ShareCandidatePage, UserKind, Visibility,
};
/// The most entries one [`Session::upcoming_in_playlist`] gives, as on
/// the server.
pub const MAX_UPCOMING: u32 = 100;

/// The most a playlist's cover may be (PX-53): bytes, and pixels by its
/// header.
pub use spoolframe_protocol::playlists::{MAX_ART_BYTES, MAX_ART_PIXELS};

/// An item, what is under it, and the playlists the user sees that hold
/// it (PX-40): all of one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDetail {
    pub item: Item,
    pub children: Vec<Item>,
    /// Empty when none hold it, or the server has no playlists.
    pub playlists: Vec<PlaylistRef>,
    /// Its heart (PX-43): a track's `All` or `None`, an album's `All`,
    /// `Some` or `None`; `None` for anything else, or without Liked.
    pub liked: Option<Liked>,
    /// An album's tracks among `children` that are liked.
    pub liked_children: Vec<MediaId>,
}

/// What an invite link says (P6-01), read without a word to the server:
/// for the "Add a server" summary and the join form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// The server's key, which the link pins.
    pub key: ServerKey,
    /// The key's fingerprint (P1-21).
    pub fingerprint: String,
    /// What the link calls the server, if it says.
    pub name: Option<String>,
    /// Where the link says the server may be reached.
    pub addresses: Vec<String>,
    /// The relays it may be reached through.
    pub relays: Vec<String>,
    /// Redeeming it makes an account ("Includes an account"); without a
    /// code it only finds and saves the server.
    pub has_code: bool,
    /// When the code stops working, as the link says: a hint.
    pub expires_at: Option<std::time::SystemTime>,
    /// The role the account gets, as the link says: a hint.
    pub role: Option<spoolframe_types::Role>,
    /// The server is saved on this device already.
    pub saved: bool,
}

/// What an invite an admin makes does when it is redeemed (PX-44).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteKind {
    /// Makes an account on this server (P6-01).
    CreateUser,
    /// Lets someone with a portable identity in (P7-05).
    PortableAccount,
    /// Pairs another server with this one (P8-01): refused until then.
    Federation,
    /// A kind this build does not know, from a newer server: listed, never
    /// made.
    Unknown,
}

/// An invite to make (PX-44): the web UI's fields, with its limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteSpec {
    pub kind: InviteKind,
    /// For the admin's list: who it is for.
    pub note: Option<String>,
    /// How many times it may be redeemed; `None` leaves it to the server.
    pub uses: Option<u32>,
    /// How long it works for; `None` leaves it to the server.
    pub expires_in: Option<Duration>,
    /// The role the account gets: only for `CreateUser`.
    pub role: Option<spoolframe_types::Role>,
}

/// An invite just made: its code shows only now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedInvite {
    pub id: String,
    pub kind: InviteKind,
    /// As people read and type it: `QW7K-2MFP-XR4T`.
    pub code: String,
    /// `spoolframe://invite?…`, for the QR code.
    pub link: String,
    /// The same invite as `https://spoolframe.app/i#…` (PX-45): what
    /// Copy link copies. Every app's "Add a server" reads it, and a
    /// browser that opens it sends none of the invite to the site.
    pub share_link: String,
    /// The text to paste into "Add a server": the https link, as the web
    /// UI copies it.
    pub copy_string: String,
    pub expires_at: Option<std::time::SystemTime>,
    pub uses: Option<u32>,
}

/// An invite on the admin's list, without its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteSummary {
    pub id: String,
    pub kind: InviteKind,
    pub note: Option<String>,
    pub uses: Option<u32>,
    /// How many times it has been redeemed.
    pub used: u32,
    pub expires_at: Option<std::time::SystemTime>,
    /// The admin who made it, by display name.
    pub created_by: String,
    /// `None` for an invite made with the server's command line.
    pub created_by_username: Option<String>,
    pub created_from: InviteOrigin,
    pub revoked: bool,
}

/// Where an invite was made (PX-44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteOrigin {
    /// The web UI's admin pages; also every invite from before PX-44.
    WebAdmin,
    /// The server's command line.
    Cli,
    /// An app, by the device it ran on.
    Device {
        name: Option<String>,
        platform: Platform,
        /// The device asking now.
        this_device: bool,
    },
    /// A place this build does not know, from a newer server.
    Unknown,
}

/// One of an account's ties to a portable identity (P7-09): devices that
/// sign in with `id` open the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tie {
    pub id: spoolframe_protocol::portable::PortableUserId,
    pub tied_at: std::time::SystemTime,
    /// The identity's last sign-in into the account; `None` until one.
    pub last_used_at: Option<std::time::SystemTime>,
}

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

    /// What invite `link` says (P6-01); nothing is sent or saved.
    /// [`BackendError::NotAnInvite`] for anything that is not one.
    fn read_invite<'a>(&'a self, link: &'a str) -> BoxFuture<'a, Result<Invite, BackendError>>;

    /// Connects to the server invite `link` names and says who answered,
    /// for "Is this the server you were invited to?". Nothing is saved.
    fn connect_invite<'a>(&'a self, link: &'a str) -> BoxFuture<'a, Result<Server, BackendError>>;

    /// This computer's portable identity side (P7-08): its key-rooted
    /// identity and the root actions.
    fn identities(&self) -> crate::identity::Identities;

    /// What this device knows of reaching server `key`: its addresses,
    /// relays, lookups, last dial and live paths. Never dials.
    /// [`BackendError::NotFound`] for a server it knows nothing of.
    fn reachability<'a>(
        &'a self,
        key: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Reachability, BackendError>>;

    /// Whether saved server `key` is reached through relays: `Some` whatever
    /// the app's setting says, `None` as it says. Used from the next
    /// connection. [`BackendError::NotPinned`] for a server not saved.
    fn set_server_relays<'a>(
        &'a self,
        key: &'a ServerKey,
        relays: Option<bool>,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Saved server `key`'s relay list from now on, the user's own; empty
    /// gives it back to the server. [`BackendError::InvalidInput`] for one
    /// that is not an http(s) URL.
    fn set_server_relay_urls<'a>(
        &'a self,
        key: &'a ServerKey,
        relays: Vec<String>,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Saves the server invite `link` names, its key pinned and verified:
    /// "Save server only", and what a link without a code does.
    fn save_invite_server<'a>(
        &'a self,
        link: &'a str,
    ) -> BoxFuture<'a, Result<Server, BackendError>>;

    /// Makes an account with invite `link`'s code and signs it in, adding
    /// a profile; the server is saved with it. Refused with
    /// [`BackendError::InviteInvalid`], [`BackendError::InviteExpired`],
    /// [`BackendError::InviteExhausted`] or
    /// [`BackendError::UsernameTaken`], saving nothing.
    fn redeem_invite<'a>(
        &'a self,
        link: &'a str,
        username: &'a str,
        display_name: Option<&'a str>,
        password: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>>;

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

    /// Checks every open connection after the machine wakes or its network
    /// changes (P1-15): one that does not answer is dropped and dialled
    /// again, so the next request does not wait out its time limit. Never
    /// dials a server it has no connection to.
    fn resume(&self) -> BoxFuture<'_, ()>;

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
    /// `remember` asks to trust this device for the user (P4-01).
    fn sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        username: &'a str,
        password: &'a str,
        remember: bool,
    ) -> BoxFuture<'a, Result<Profile, BackendError>>;

    /// Signs in on `server` as portable identity `identity` (P7-06,
    /// P7-08), with this device's sign-in for it from a pairing, adding a
    /// profile: the account the identity is tied to there, or the
    /// identity as a guest. [`BackendError::WrongCredentials`] where the
    /// server lets it in as neither; [`BackendError::NotPaired`] with no
    /// live sign-in for it here.
    fn sign_in_portable<'a>(
        &'a self,
        server: &'a ServerKey,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'a, Result<Profile, BackendError>>;

    /// The profiles remembered on this device for `server` (P4-01), asked
    /// before any sign-in.
    fn trusted_profiles<'a>(
        &'a self,
        server: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<TrustedProfile>, BackendError>>;

    /// Opens a remembered profile without a password (P4-01): a session,
    /// locked if the PIN is required here. One the device is no longer
    /// trusted for is [`BackendError::SignInRequired`], whatever the
    /// reason. Not a recent sign-in: sensitive actions still confirm.
    fn resume_profile<'a>(
        &'a self,
        server: &'a ServerKey,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>>;

    /// Begins a sign-in another device approves (P3-05, P3-07): what to
    /// show while it waits. A server holds at most three per device, so
    /// begin one per screen, not per redraw.
    fn begin_device_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        remember: bool,
    ) -> BoxFuture<'a, Result<DeviceSignIn, BackendError>>;

    /// Waits until `sign_in` is approved, adding its profile as a password
    /// sign-in does; [`BackendError::SignInDenied`] or
    /// [`BackendError::SignInExpired`] otherwise. Drop the future to stop
    /// waiting.
    /// With the profile comes whether the approver let this device be
    /// remembered (P4-01).
    fn await_device_sign_in<'a>(
        &'a self,
        sign_in: &'a DeviceSignIn,
    ) -> BoxFuture<'a, Result<(Profile, Option<bool>), BackendError>>;

    /// Begins a passkey sign-in on `server` (P3-19): what the passkey
    /// signs, for [`APP_RP_ID`]. With `username`, only that user's
    /// passkeys may answer; without, any of this server's on the key.
    fn begin_passkey_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        username: Option<&'a str>,
    ) -> BoxFuture<'a, Result<PasskeyRequest, BackendError>>;

    /// Finishes it with what the passkey signed, adding a profile as a
    /// password sign-in does. A passkey that does not sign in is
    /// [`BackendError::WrongCredentials`].
    fn finish_passkey_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        signed: FinishPasskeySignIn,
        remember: bool,
    ) -> BoxFuture<'a, Result<Profile, BackendError>>;

    /// The session behind a profile.
    fn session(&self, profile: &Profile) -> Result<Arc<dyn Session>, BackendError>;

    /// The state of the connection to `server`, as it changes.
    fn connection(&self, server: &ServerKey) -> watch::Receiver<ConnectionState>;

    /// Changes each time `server` signs this device out (its last session
    /// ended, a password changed elsewhere, PX-10): the stored sessions
    /// are gone by then, so the app goes back to "Who's watching".
    fn signed_out(&self, server: &ServerKey) -> watch::Receiver<u64>;

    /// Changes when [`Client::servers`] would answer differently: a server
    /// heard, changed or gone on the LAN, an address added, a pin saved or
    /// forgotten. Discovery takes a moment after opening, so the first
    /// answer is often empty; list again on each change.
    fn servers_changed(&self) -> watch::Receiver<()>;

    /// Playlist changes the servers tell this device of (PX-40), for any
    /// user signed in on them: read again what is shown. Changes made
    /// before subscribing, or while not connected, are not seen.
    fn playlist_events(&self) -> tokio::sync::broadcast::Receiver<PlaylistEvent>;

    /// Every subtitle timing change a server this device is connected to
    /// tells of (PX-03): a track synced, set back to its original timing,
    /// or offset, by anyone. A player showing the track reads it again.
    fn subtitle_timing_events(&self) -> tokio::sync::broadcast::Receiver<SubtitleTimingEvent>;
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

    /// How fast the link to the server has carried direct reads lately,
    /// in bits per second (P6-06); `None` until something was read.
    fn link_rate(&self) -> Option<u64>;

    /// Plans a playback again for a link that carries only
    /// `link_bitrate` bits per second, from `position` (P6-06): a direct
    /// play too big for it becomes a conversion that fits. The plan comes
    /// back unchanged when it fits already.
    fn limit_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        link_bitrate: u64,
        position: Duration,
    ) -> BoxFuture<'a, Result<PlaybackChange, BackendError>>;

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

    /// [`Session::item`] with the playlists the user sees that hold the
    /// item (PX-40): a movie, episode or track itself, any of a season's
    /// or show's episodes, any of an album's or artist's tracks.
    fn item_detail<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<ItemDetail, BackendError>>;

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
    /// playback's encode begins at `options.start` (P2-03); direct play
    /// reads from wherever the player likes, and the server ignores it.
    /// `options.quality` is the viewer's choice (P6-06): Auto plans for the
    /// link; `options.display` is what the screen shows (PGPU-06), in place
    /// of what the client said as it connected.
    fn start_playback<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
        options: PlaybackOptions,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>>;

    /// Plans a playing playback again for the quality the viewer chose
    /// now, from `position` (P6-06): the new plan, size and epoch, as a
    /// subtitle change gives them.
    fn change_quality<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        quality: PlaybackQuality,
        position: Duration,
    ) -> BoxFuture<'a, Result<PlaybackChange, BackendError>>;

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

    /// The user's audio and subtitle preferences, and whether the server
    /// reads picture subtitles as text (PX-13).
    fn track_preferences(&self) -> BoxFuture<'_, Result<TrackPreferencesInfo, BackendError>>;

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

    /// Begins making a passkey for this user (P3-19). Only soon after
    /// signing in: [`BackendError::RecentSignInRequired`] otherwise.
    fn begin_passkey_registration(&self) -> BoxFuture<'_, Result<PasskeyCreation, BackendError>>;

    /// Keeps the passkey made for it, named for this device (1 to 64
    /// characters).
    fn finish_passkey_registration(
        &self,
        made: FinishPasskeyRegistration,
    ) -> BoxFuture<'_, Result<(), BackendError>>;

    /// This user's passkeys, the web UI's too, oldest first.
    fn passkeys(&self) -> BoxFuture<'_, Result<Vec<PasskeyInfo>, BackendError>>;

    fn rename_passkey<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Removes one; [`BackendError::RecentSignInRequired`] as for adding,
    /// [`BackendError::LastSignInMethod`] for their only way in.
    fn remove_passkey<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>>;

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
    /// `remember` keeps the device's own request to be remembered; it is
    /// remembered only if it asked and this says yes (P4-01).
    fn approve_device_sign_in<'a>(
        &'a self,
        code: &'a str,
        remember: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Deletes another of this user's devices (P4-03): its sessions for
    /// this user end, its trust goes, and it shows up as new if it signs
    /// in again. Always needs a recent sign-in; this device is
    /// [`BackendError::InvalidInput`] (sign out instead).
    fn forget_device<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Trusts a device for this user, or stops (P4-01); `None` is this
    /// one. Turning trust or auto-login on is for this device only, and
    /// needs a recent sign-in; so does turning trust off for another.
    /// Turning it off here ends this session (the profile stays saved).
    /// Auto-login without trust is [`BackendError::InvalidInput`]; turning
    /// it on turns the PIN off here.
    fn set_device_trust<'a>(
        &'a self,
        device: Option<&'a str>,
        trusted: bool,
        auto_login: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Signs this profile out here: the session ends and the device is no
    /// longer trusted for it (P4-01); the next time is a full sign-in.
    fn sign_out(&self) -> BoxFuture<'_, Result<(), BackendError>>;

    /// Refuses it: the device is told so.
    fn deny_device_sign_in<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<(), BackendError>>;

    /// "Confirm it's you" with the password: refreshes this session's
    /// sign-in time, for the actions that need one in the last 10 minutes
    /// (approving a device, adding or removing a passkey, no longer
    /// requiring the PIN). A wrong one is [`BackendError::WrongCredentials`]
    /// (then [`BackendError::RateLimited`]); an account without a password
    /// is [`BackendError::NoPassword`]. The PIN does not confirm it.
    fn confirm_password<'a>(&'a self, password: &'a str)
    -> BoxFuture<'a, Result<(), BackendError>>;

    /// Begins confirming with one of this user's app passkeys: what the
    /// key or phone signs. A user with none is [`BackendError::NotFound`].
    fn begin_confirm_passkey(&self) -> BoxFuture<'_, Result<PasskeyRequest, BackendError>>;

    /// Finishes it with what the passkey signed.
    fn finish_confirm_passkey(
        &self,
        signed: FinishPasskeySignIn,
    ) -> BoxFuture<'_, Result<(), BackendError>>;

    /// The user is the server's admin (P2-09): the app offers its admin
    /// pages.
    fn is_admin(&self) -> bool;

    /// This session signed in with a one-time password an admin set
    /// (c86e6d3a): it must choose a new one before anything else. A
    /// session found again says so only by refusing its first request
    /// with [`BackendError::PasswordChangeRequired`].
    fn password_change_required(&self) -> bool;

    /// Changes the user's password from `current` to `new`. A wrong
    /// `current` is [`BackendError::WrongCredentials`]; a `new` the server
    /// does not take (too short, or the same) is
    /// [`BackendError::InvalidInput`].
    fn change_password<'a>(
        &'a self,
        current: &'a str,
        new: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Makes an invite (PX-44). Admins only, after a recent confirmation
    /// (`RecentSignInRequired` asks for one).
    fn create_invite(&self, spec: InviteSpec) -> BoxFuture<'_, Result<MintedInvite, BackendError>>;

    /// The server's invites, newest first; revoked ones too. Admins only.
    fn invites(&self) -> BoxFuture<'_, Result<Vec<InviteSummary>, BackendError>>;

    /// Stops an invite working. Admins only, after a recent confirmation.
    fn revoke_invite<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>>;

    /// The portable identity this session is a guest with (P7-06): it
    /// signed in as the identity itself, and has no account here. `None`
    /// for an account, tied to an identity or not. Asks nothing.
    fn guest_identity(&self) -> Option<spoolframe_protocol::portable::PortableUserId>;

    /// The portable identities this account is tied to (P7-04, P7-09), as
    /// the server says now, the oldest tie first.
    fn tie_info(&self) -> BoxFuture<'_, Result<Vec<Tie>, BackendError>>;

    /// Ties this account to portable identity `identity` (P7-04): devices
    /// that sign in with the identity open the account. This device proves
    /// the identity with its own credential for it; with none, or one past
    /// its time, [`BackendError::NotPaired`]. Only soon after signing in
    /// ([`BackendError::RecentSignInRequired`]). The account's other ties
    /// stay (P7-09); an identity tied to it already is `Ok`, changing
    /// nothing, and one tied to another account here is refused.
    fn tie(
        &self,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'_, Result<(), BackendError>>;

    /// Severs the account's tie to `identity` (P7-04, P7-09); its other
    /// ties, its password, its sessions and its history stay. Only soon
    /// after signing in, as for [`Session::tie`]; no such tie is
    /// [`BackendError::NotFound`].
    fn untie(
        &self,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'_, Result<(), BackendError>>;

    /// Leaves the server (PX-47): a local account is disabled (its owner
    /// can restore it), a guest's access ends for good. Once it has, this
    /// profile is removed from this device, and this session ends. The
    /// server is forgotten here, as [`Client::forget`] does, only when no
    /// other profile is kept for it; [`Client::servers`] says which. Only soon after signing in
    /// ([`BackendError::RecentSignInRequired`]); the server's last admin
    /// is refused with [`BackendError::LastAdmin`]. A refused leave changes
    /// nothing.
    fn leave_server(&self) -> BoxFuture<'_, Result<(), BackendError>>;

    /// Whether the user may leave the server, as it said when this
    /// session was made: false only for its last admin. An older server
    /// that does not say reads as true; [`Session::leave_server`]'s
    /// refusal is the backstop.
    fn can_leave(&self) -> bool;

    /// [`Session::can_leave`], asked of the server again.
    fn can_leave_now(&self) -> BoxFuture<'_, Result<bool, BackendError>>;

    /// Whether the server has playlists (PX-40, `spoolframe:playlists`),
    /// as it said when this session was made. Without them every call
    /// below is [`BackendError::Unsupported`] and nothing is sent, so the
    /// whole feature is hidden.
    fn has_playlists(&self) -> bool;

    /// Whether the session has Liked and All, and hearts (PX-43,
    /// `spoolframe:liked`): never without playlists. Without it the
    /// playlists list neither, and no heart is shown.
    fn has_liked(&self) -> bool;

    /// Likes tracks or albums (PX-43): those not liked yet go to the top
    /// of the user's Liked, an album's tracks in order. 1 to 100 at a
    /// time, tracks and albums only ([`BackendError::KindNotAllowed`]).
    /// Liked as it is after. Without [`Session::has_liked`],
    /// [`BackendError::Unsupported`], unsent.
    fn like<'a>(
        &'a self,
        items: &'a [MediaId],
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>>;

    /// Takes tracks, or every track of albums, out of the user's Liked.
    fn unlike<'a>(
        &'a self,
        items: &'a [MediaId],
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>>;

    /// Whether items' playback states say when each was last viewed
    /// (`spoolframe:last-viewed`). Data only: without it the field is
    /// `None` everywhere, and nothing says "Viewed".
    fn has_last_viewed(&self) -> bool;

    /// Whether playlists are shared by name (`spoolframe:playlist-sharing`):
    /// [`Session::share_candidates`], and the names of a playlist's owner
    /// and of whom it is shared with. Never without playlists.
    fn has_playlist_sharing(&self) -> bool;

    /// Whom playlist `id` can be shared with (`spoolframe:playlist-sharing`):
    /// a page from `offset` of at most `limit` (200), by display name,
    /// and how many match in all; with a `query`, only those whose name
    /// or username holds it. Everyone but the user. Only whoever manages
    /// the playlist may ask ([`BackendError::PlaylistReadOnly`]); without
    /// [`Session::has_playlist_sharing`], [`BackendError::Unsupported`].
    fn share_candidates<'a>(
        &'a self,
        id: &'a str,
        query: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<ShareCandidatePage, BackendError>>;

    /// Whether the server keeps play history (PX-52,
    /// `spoolframe:play-history`): [`Session::recently_played`], and play
    /// counts on item pages.
    fn has_play_history(&self) -> bool;

    /// Whether playlists have covers (PX-53, `spoolframe:playlist-art`):
    /// each playlist's `artwork` (an uploaded cover, a mosaic, or Liked's
    /// and All's tiles), fetched as any artwork is, and setting or taking
    /// away a cover. Never without playlists. Without it no playlist has
    /// `artwork`, and the app draws its own mosaic.
    fn has_playlist_art(&self) -> bool;

    /// Whether the server says what comes up in a playlist and saves its
    /// play settings alone (`spoolframe:playlist-upcoming`):
    /// [`Session::upcoming_in_playlist`] and
    /// [`Session::set_playlist_settings`]. Never without playlists.
    /// Without it Up next shows a playlist in its own order, and the
    /// settings are saved by playing.
    fn has_playlist_upcoming(&self) -> bool;

    /// Whether external text subtitles are synced and offset on the server
    /// (PX-03, `spoolframe:subtitle-sync`): a track's `timing`, and Sync,
    /// Use original timing and the offset control.
    fn has_subtitle_sync(&self) -> bool;

    /// Whether the server finds and downloads subtitles from subtitle
    /// sites (PX-02, `spoolframe:subtitle-search`): "Find subtitles…", and
    /// a downloaded track's `download`.
    fn has_subtitle_search(&self) -> bool;

    /// What the server's subtitle sites have for item `id` (PX-02), in
    /// `language` (`en`, `pt-BR`) or, with `None`, the languages the
    /// server offers first: one list, best first, and how each site's
    /// part went. Never kept by the catalog: the server keeps it a day.
    /// [`BackendError::NoSubtitleSource`] when the server has no site set
    /// up. Without [`Session::has_subtitle_search`],
    /// [`BackendError::Unsupported`], unsent.
    fn search_subtitles<'a>(
        &'a self,
        id: &'a MediaId,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<SubtitleSearch, BackendError>>;

    /// Downloads search result `result` (its `SubtitleResult::id`) as a
    /// track of item `id`, for everyone: its place in the item's
    /// subtitles, the track, and the downloads left today. Ask for the
    /// item's tracks again after it: no event says so.
    /// [`BackendError::QuotaExhausted`] when the downloads are used up.
    fn download_subtitle<'a>(
        &'a self,
        id: &'a MediaId,
        result: &'a str,
    ) -> BoxFuture<'a, Result<SubtitleDownloaded, BackendError>>;

    /// Syncs item `id`'s external text subtitle `track` to the file, in
    /// the background (PX-03): its timing now, `Syncing`; the outcome
    /// comes as a [`SubtitleTimingEvent`]. Without
    /// [`Session::has_subtitle_sync`], [`BackendError::Unsupported`],
    /// unsent.
    fn sync_subtitle<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>>;

    /// Serves item `id`'s subtitle `track` with its original timing
    /// again, for everyone; its offset stays.
    fn use_original_subtitle_timing<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>>;

    /// Moves item `id`'s subtitle `track` by `offset_ms` (positive:
    /// later), for everyone; 0 resets it. At most
    /// [`MAX_SUBTITLE_OFFSET_MS`] either way. What the server serves has
    /// it already: a player never adds it itself.
    fn set_subtitle_offset<'a>(
        &'a self,
        id: &'a MediaId,
        track: u32,
        offset_ms: i64,
    ) -> BoxFuture<'a, Result<SubtitleTiming, BackendError>>;

    /// Removes item `id`'s downloaded track `download`
    /// (`SubtitleDownload::id`), for everyone: its downloader or an admin
    /// may.
    fn remove_subtitle_download<'a>(
        &'a self,
        id: &'a MediaId,
        download: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    /// What the user played lately, from any device (PX-52): each item
    /// once, by its latest counted play, newest first, a page of at most
    /// `limit` (500), and how many in all. `kinds` keeps only films,
    /// episodes or tracks; `None`, every kind. Each item's playback
    /// state has its play count and when it was last played. Without
    /// [`Session::has_play_history`], [`BackendError::Unsupported`],
    /// unsent.
    fn recently_played(
        &self,
        kinds: Option<Kinds>,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'_, Result<(Vec<Item>, u32), BackendError>>;

    /// Every playlist the user sees, with whether and where each is
    /// pinned, and the notes of pins that left their Pinned.
    fn playlists(&self) -> BoxFuture<'_, Result<PlaylistList, BackendError>>;

    /// A playlist and a page of its entries (at most 200), with the
    /// user's place and play settings.
    fn playlist<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<PlaylistPage, BackendError>>;

    /// Makes a playlist the user owns. Server-wide is an admin's.
    fn create_playlist<'a>(
        &'a self,
        name: &'a str,
        kinds: Kinds,
        visibility: Visibility,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>>;

    /// Renames a playlist, widens its kinds or changes who sees it: its
    /// owner's, or an admin's for a server-wide one.
    fn update_playlist<'a>(
        &'a self,
        id: &'a str,
        update: PlaylistUpdate,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>>;

    /// Changes a playlist's entries as seen at `revision`: its new
    /// revision, or [`BackendError::PlaylistConflict`].
    fn edit_playlist<'a>(
        &'a self,
        id: &'a str,
        revision: u64,
        edits: Vec<PlaylistEdit>,
    ) -> BoxFuture<'a, Result<u64, BackendError>>;

    fn delete_playlist<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Gives playlist `id` the picture in `bytes` as its cover (PX-53): a
    /// JPEG, PNG, WebP or HEIC of at most [`MAX_ART_BYTES`] and
    /// [`MAX_ART_PIXELS`]. Its new `artwork`. Whoever may edit its entries
    /// may ([`BackendError::NotAllowed`] for anyone else, and on Liked and
    /// All); past the limits [`BackendError::ImageTooLarge`], a picture
    /// the server cannot read [`BackendError::ImageUnreadable`]. Without
    /// [`Session::has_playlist_art`], [`BackendError::Unsupported`],
    /// unsent.
    fn set_playlist_art<'a>(
        &'a self,
        id: &'a str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<ArtworkId, BackendError>>;

    /// Takes playlist `id`'s uploaded cover away (PX-53): the playlist
    /// after, its `artwork` the mosaic again. Who may is as for
    /// [`Session::set_playlist_art`].
    fn remove_playlist_art<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>>;

    /// Pins a playlist at `position` in the user's Pinned (0 first), or
    /// moves it there.
    fn pin_playlist<'a>(
        &'a self,
        id: &'a str,
        position: u32,
    ) -> BoxFuture<'a, Result<(), BackendError>>;

    fn unpin_playlist<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>>;

    /// Forgets the pin notes [`Session::playlists`] gave, on every device:
    /// they were shown.
    fn dismiss_pin_notes(&self) -> BoxFuture<'_, Result<(), BackendError>>;

    /// Starts or goes on playing a playlist: the entry to play, from
    /// `from` or else the user's place, or `None` when nothing is left.
    fn play_playlist<'a>(
        &'a self,
        id: &'a str,
        from: Option<EntryId>,
        settings: PlaySettings,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>>;

    /// What plays after entry `after` ended or was skipped; `None` at the
    /// end, when the server clears the place so the next play starts at
    /// the top.
    fn next_in_playlist<'a>(
        &'a self,
        id: &'a str,
        after: EntryId,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>>;

    /// What plays next in playlist `id`, without playing it: at most
    /// `limit` ([`MAX_UPCOMING`]) entries after the user's place, each the
    /// one [`Session::next_in_playlist`] would give after the one before.
    /// The place's own entry is not among them, except with Repeat One,
    /// when it alone is (it plays again). The shuffled round's order
    /// while shuffling, else the playlist's; from the top again with
    /// Repeat All, so an entry may come more than once. With no place,
    /// from the top. Without [`Session::has_playlist_upcoming`],
    /// [`BackendError::Unsupported`], unsent.
    fn upcoming_in_playlist<'a>(
        &'a self,
        id: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<PlaylistEntry>, BackendError>>;

    /// Keeps `settings` as how the user plays playlist `id`, playing
    /// nothing: the place and the shuffled round stay. Shuffle turned on
    /// shuffles anew from the place, so the whole playlist is ahead;
    /// turned off, the playlist's order from the place. Without
    /// [`Session::has_playlist_upcoming`], [`BackendError::Unsupported`],
    /// unsent.
    fn set_playlist_settings<'a>(
        &'a self,
        id: &'a str,
        settings: PlaySettings,
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

    fn with_id(server_id: Option<&str>) -> Server {
        Server {
            key: ServerKey("k".into()),
            name: "Home Media".into(),
            addresses: Vec::new(),
            pinned: true,
            fingerprint: String::new(),
            verified: true,
            on_network: false,
            verified_at: None,
            last_connected_at: None,
            server_id: server_id.map(str::to_owned),
            advertised: Vec::new(),
            relays: Vec::new(),
            advertised_at: None,
            use_relays: None,
            relays_edited: false,
        }
    }

    #[test]
    fn a_server_is_shown_where_it_says_it_is() {
        let server = Server {
            addresses: vec!["172.17.0.1:7430".into()],
            advertised: vec!["192.168.86.10:7430".into()],
            ..with_id(None)
        };
        assert_eq!(server.shown_address(), Some("192.168.86.10:7430"));
    }

    #[test]
    fn a_server_that_never_said_is_shown_where_it_was_reached() {
        let server = Server {
            addresses: vec!["10.10.0.6:7430".into()],
            ..with_id(None)
        };
        assert_eq!(server.shown_address(), Some("10.10.0.6:7430"));
    }

    #[test]
    fn a_servers_passkeys_are_for_its_own_site() {
        assert_eq!(
            with_id(Some("k7f3q2m4x5pq6z2a")).passkey_site().as_deref(),
            Some("k7f3q2m4x5pq6z2a.spoolframe.app")
        );
    }

    #[test]
    fn a_server_not_yet_heard_from_has_no_passkey_site() {
        assert_eq!(with_id(None).passkey_site(), None);
    }

    #[test]
    fn a_short_key_is_shown_whole() {
        assert_eq!(ServerKey("abc".into()).short(), "abc");
    }
}
