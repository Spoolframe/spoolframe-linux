//! A client and sessions that answer from fixed data, for tests: the
//! reader, the player and the screens' logic meet a server's cases without
//! one. Built only for tests (core's own, or the app's through the
//! `testing` feature); the app itself always talks to real servers. The catalog is the content of the UI
//! mockups. Watch state, playback handles and sign-in limits behave like
//! the server's (P1-08, P1-11, P1-14), so the UI meets the same cases.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spoolframe_policy::{
    plays::{self, PlayKind, PlayProgress},
    tracks::{self, TrackOverride},
    watch::{WatchRules, credits_to_end},
};
use spoolframe_protocol::{
    AudioAction, ConvertedAudio, ConvertedSubtitle, ConvertedVideo, PlannedAudio, PlannedSubtitle,
    SubtitleAction, SubtitleDelivery,
};
use spoolframe_types::{
    Album, Artist, ArtworkId, AudioCodec, AudioRole, AudioTrack, Container, Delivery, Episode,
    HdrFormat, Item, Library, LibraryId, LibraryKind, MediaId, MediaKind, MediaTracks, Movie,
    Namespace, PlaybackState, Season, Show, SubtitleFormat, SubtitleRole, SubtitleTrack, Track,
    TrackPreferences, VideoCodec, VideoTrack,
};
use tokio::sync::watch;

use crate::backend::{
    ArtworkSize, BackendError, BoxFuture, ChosenTracks, Chunk, Client, ConnectionPath,
    ConnectionState, DeviceInfo, DeviceSignIn, DeviceSignInRequest, HomeScreen, Invite, MAX_READ,
    Marker, Person, PersonId, PersonRole, PinProof, PinState, Platform, PlaybackHandle,
    PlaybackPlan, PlaybackStart, PlexLink, Previews, Profile, ProfileSignIn, SearchCounts,
    SearchGroup, SearchKind, SearchRequest, SearchResults, SearchScope, Selection, Server,
    ServerKey, Session, TopResult, TrustedProfile,
};
use crate::backend::{
    EntryId, Features, ItemDetail, Kinds, PlaySettings, PlaylistEdit, PlaylistEvent, PlaylistList,
    PlaylistNext, PlaylistPage, PlaylistSummary, PlaylistUpdate, Visibility,
};
use crate::backend::{
    FinishPasskeyRegistration, FinishPasskeySignIn, PasskeyCreation, PasskeyInfo, PasskeyRequest,
    TrackPreferencesInfo,
};
use crate::backend::{InviteKind, InviteOrigin, InviteSpec, InviteSummary, MintedInvite};

mod playlists;
mod subtitles;

use crate::backend::{SubtitleDownloaded, SubtitleSearch};

pub use playlists::PAGE_LIMIT as PLAYLIST_PAGE_LIMIT;
pub use subtitles::{QUOTA_RESETS_AT, Sites};

/// Every fake account's password.
pub const FAKE_PASSWORD: &str = "spoolframe";
/// The one-time password [`FakeClient::reset_password`] sets.
pub const FAKE_ONE_TIME_PASSWORD: &str = "one-time-7KQ2";
/// The username the fake's portable identity signs in as (P7-08).
pub const FAKE_GUEST: &str = "guest";

/// What the fake server asks a passkey to sign (P3-19).
pub const FAKE_PASSKEY_CHALLENGE: [u8; 32] = [7; 32];

/// Home Media's server id (PX-14), which names its passkeys' relying
/// party.
pub fn fake_server_id() -> spoolframe_protocol::ServerId {
    spoolframe_protocol::ServerId::parse("homemediaserver2").expect("a server id")
}

/// What the fake server keeps about this device's sign-ins, for every
/// session of it: passkeys (P3-19) and trust (P4-01).
#[derive(Default)]
struct FakeAuth {
    /// The users this device is trusted for, by username, with whether
    /// theirs is the auto-login profile (one at most).
    trusted: HashMap<String, bool>,
    /// Whether the living-room TV is still trusted (it is to begin with).
    tv_untrusted: bool,
    /// Signed out here (P4-01): gone from "Who's watching" until they sign
    /// in again.
    signed_out: std::collections::HashSet<String>,
    keys: Vec<FakePasskey>,
    /// Set by [`FakeClient::age_sign_ins`]: every sign-in is older than
    /// the 10 minutes adding or removing a passkey allows.
    stale: bool,
    /// Who last signed in here with a passkey, as the SDK remembers it.
    by_passkey: std::collections::HashSet<String>,
    /// Invites admins made from the app (PX-44), oldest first.
    invites: Vec<InviteSummary>,
    /// Users an admin reset to a one-time password (c86e6d3a), and those
    /// signed in with it, who must choose a new one.
    one_time: std::collections::HashSet<String>,
    must_change: std::collections::HashSet<String>,
    /// Accounts' ties to portable identities (P7-04, P7-09), by username,
    /// the oldest first. One identity is tied to one account at most.
    ties: HashMap<String, Vec<crate::backend::Tie>>,
    /// The identity [`FAKE_GUEST`] last signed in with (P7-06).
    guest: Option<spoolframe_protocol::portable::PortableUserId>,
}

struct FakePasskey {
    username: String,
    credential_id: Vec<u8>,
    name: String,
    created_at: i64,
    last_used_at: Option<i64>,
    /// The relying party it was made for: Home Media's own, the old
    /// shared `spoolframe.app`, or the web UI's host.
    rp_id: String,
}

impl FakePasskey {
    /// The id the server names a passkey by, as the real one does: so a
    /// passkey made in the app is renamed by the id the app works out.
    fn id(&self) -> String {
        crate::passkey::passkey_id(&self.credential_id)
    }

    fn info(&self) -> PasskeyInfo {
        PasskeyInfo {
            id: self.id(),
            name: self.name.clone(),
            created_at: self.created_at,
            last_used_at: self.last_used_at,
            rp_id: self.rp_id.clone(),
            app: spoolframe_protocol::is_app_rp_id(&self.rp_id),
            usable_here: self.usable(),
        }
    }

    /// An app signs in with it: made for Home Media's relying party.
    fn usable(&self) -> bool {
        self.rp_id == fake_server_id().rp_id()
    }
}

/// A passkey's name as the server takes it: 1 to 64 characters, trimmed.
fn passkey_name(name: &str) -> Result<String, BackendError> {
    let name = name.trim();
    match name.chars().count() {
        1..=64 => Ok(name.to_owned()),
        _ => Err(BackendError::InvalidInput(
            "a passkey's name is 1 to 64 characters".into(),
        )),
    }
}

/// Watch-state rules, the server's defaults (P1-14).
/// A playback handle idle this long expires, as on the server (P1-08).
const HANDLE_IDLE: Duration = Duration::from_secs(5 * 60);
/// Failed sign-ins before the fake rate-limits, and for how long.
const SIGN_IN_ATTEMPTS: u32 = 3;
const SIGN_IN_BACKOFF: Duration = Duration::from_secs(30);

fn media(kind: MediaKind, key: &str) -> MediaId {
    MediaId::new(Namespace::Plex, kind, key).expect("fixture keys are not empty")
}

fn library(key: &str, title: &str, kind: LibraryKind) -> Library {
    Library {
        id: LibraryId::new(Namespace::Plex, key).expect("fixture keys are not empty"),
        title: title.into(),
        kind,
    }
}

fn mins(m: u64) -> Option<Duration> {
    Some(Duration::from_secs(m * 60))
}

fn movie(key: &str, title: &str, year: u16, length: u64) -> Item {
    Item::Movie(Movie {
        id: media(MediaKind::Movie, key),
        title: title.into(),
        year: Some(year),
        summary: None,
        duration: mins(length),
        artwork: Vec::new(),
        playback: PlaybackState::default(),
        content_rating: Some("PG-13".into()),
        cast: Vec::new(),
        release_date: None,
    })
}

/// The library everyone on the fake server shares.
struct Catalog {
    libraries: Vec<(Library, Vec<MediaId>)>,
    items: HashMap<MediaId, Item>,
    /// Each parent's children, in display order.
    children: HashMap<MediaId, Vec<MediaId>>,
    /// Every item in fixture order, for search.
    order: Vec<MediaId>,
    recently_added: Vec<MediaId>,
    /// The people a search finds (P3-09), with the items they are in.
    people: Vec<(Person, Vec<MediaId>)>,
}

impl Catalog {
    fn new() -> Self {
        let mut catalog = Catalog {
            libraries: Vec::new(),
            items: HashMap::new(),
            children: HashMap::new(),
            order: Vec::new(),
            recently_added: Vec::new(),
            people: Vec::new(),
        };

        let mut alien = movie("1", "Alien", 1979, 117);
        if let Item::Movie(m) = &mut alien {
            m.summary = Some(
                "The crew of a commercial towing ship answers a distress call from a distant \
                 moon and brings something back on board."
                    .into(),
            );
        }
        let movies = [
            movie("2", "Arrival", 2016, 116),
            movie("3", "Heat", 1995, 170),
            movie("4", "Blade Runner", 1982, 117),
            movie("5", "Moon", 2009, 97),
            movie("6", "Sunshine", 2007, 107),
            movie("7", "Contact", 1997, 150),
            movie("8", "Dune: Part Two", 2024, 166),
            alien,
        ];
        catalog.recently_added = movies[..6].iter().map(|m| m.id().clone()).collect();
        catalog.people = vec![(
            Person {
                id: PersonId::new(Namespace::Plex, "director-villeneuve").expect("not empty"),
                name: "Denis Villeneuve".into(),
                role: PersonRole::Director,
                count: Some(2),
            },
            vec![media(MediaKind::Movie, "2"), media(MediaKind::Movie, "8")],
        )];
        let movie_ids = movies.iter().map(|m| m.id().clone()).collect();
        for m in movies {
            catalog.add(m);
        }

        let severance = catalog.show(
            "20",
            "Severance",
            2022,
            2,
            &[
                "Hello, Ms. Cobel",
                "Goodbye, Mrs. Selvig",
                "Who Is Alive?",
                "Woe's Hollow",
                "Trojan's Horse",
            ],
        );
        let andor = catalog.show(
            "60",
            "Andor",
            2022,
            1,
            &["Kassa", "That Would Be Me", "Reckoning", "Aldhani"],
        );
        // A second season, for what follows a season's last episode.
        catalog.season(
            &andor,
            "Andor",
            "70",
            2,
            &["One Year Later", "Sagrona Teema", "Harvest"],
        );

        let artist = media(MediaKind::Artist, "30");
        let album = media(MediaKind::Album, "31");
        catalog.add(Item::Artist(Artist {
            id: artist.clone(),
            name: "The Night Shift".into(),
            summary: None,
            artwork: Vec::new(),
        }));
        catalog.add(Item::Album(Album {
            id: album.clone(),
            artist_id: artist.clone(),
            artist_name: Some("The Night Shift".into()),
            title: "Low Tide".into(),
            year: Some(2021),
            artwork: Vec::new(),
            track_count: Some(9),
        }));
        catalog.children.insert(artist.clone(), vec![album.clone()]);
        let mut tracks = Vec::new();
        for (i, (title, secs)) in [
            ("Undertow", 252),
            ("Salt Lines", 238),
            ("Harbour Lights", 282),
            ("Breakwater", 305),
            ("Night Ferry", 271),
            ("Driftwood", 227),
            ("Ebb", 320),
            ("Fog Signal", 249),
            ("Low Tide", 344),
        ]
        .into_iter()
        .enumerate()
        {
            let id = media(MediaKind::Track, &format!("4{i}"));
            tracks.push(id.clone());
            catalog.add(Item::Track(Track {
                id,
                album_id: album.clone(),
                artist_id: artist.clone(),
                title: title.into(),
                track: Some(i as u32 + 1),
                disc: Some(1),
                duration: Some(Duration::from_secs(secs)),
                playback: PlaybackState::default(),
                codec: Some(AudioCodec::Flac),
                bit_depth: Some(16),
                sample_rate: Some(44_100),
                album_track_count: Some(9),
                artist_name: Some("The Night Shift".into()),
                album_title: Some("Low Tide".into()),
                artwork: Vec::new(),
            }));
        }
        catalog.children.insert(album, tracks);

        catalog.libraries = vec![
            (library("1", "Movies", LibraryKind::Movies), movie_ids),
            (
                library("2", "TV Shows", LibraryKind::Shows),
                vec![severance, andor],
            ),
            (library("3", "Music", LibraryKind::Music), vec![artist]),
        ];
        catalog
    }

    fn add(&mut self, item: Item) {
        self.order.push(item.id().clone());
        self.items.insert(item.id().clone(), item);
    }

    /// A show with one season of 50-minute episodes. Keys: the show's,
    /// then +1 for the season and +2… for the episodes.
    fn show(
        &mut self,
        key: &str,
        title: &str,
        year: u16,
        season: u32,
        episodes: &[&str],
    ) -> MediaId {
        let base: u32 = key.parse().expect("fixture show keys are numbers");
        let show = media(MediaKind::Show, key);
        self.add(Item::Show(Show {
            id: show.clone(),
            title: title.into(),
            year: Some(year),
            summary: None,
            artwork: Vec::new(),
            content_rating: Some("TV-MA".into()),
            season_count: Some(0),
            genres: vec!["Drama".into(), "Science Fiction".into()],
            cast: Vec::new(),
            last_aired: None,
        }));
        self.season(&show, title, &(base + 1).to_string(), season, episodes);
        show
    }

    /// Adds a season of 50-minute episodes to `show`, after its others.
    /// Keys: the season's, then +1… for the episodes.
    fn season(
        &mut self,
        show: &MediaId,
        show_title: &str,
        key: &str,
        season: u32,
        episodes: &[&str],
    ) {
        let base: u32 = key.parse().expect("fixture season keys are numbers");
        let season_id = media(MediaKind::Season, key);
        self.add(Item::Season(Season {
            id: season_id.clone(),
            show_id: show.clone(),
            show_title: Some(show_title.into()),
            season,
            title: format!("Season {season}"),
            artwork: Vec::new(),
        }));
        self.children
            .entry(show.clone())
            .or_default()
            .push(season_id.clone());
        let mut ids = Vec::new();
        for (i, title) in episodes.iter().enumerate() {
            let id = media(MediaKind::Episode, &(base + 1 + i as u32).to_string());
            ids.push(id.clone());
            self.add(Item::Episode(Episode {
                id,
                show_id: show.clone(),
                show_title: Some(show_title.into()),
                season_id: season_id.clone(),
                season,
                episode: i as u32 + 1,
                title: (*title).into(),
                summary: None,
                duration: mins(50),
                artwork: Vec::new(),
                playback: PlaybackState::default(),
                content_rating: Some("TV-MA".into()),
                year: None,
                air_date: None,
                cast: Vec::new(),
            }));
        }
        self.children.insert(season_id, ids);
        if let Some(Item::Show(show)) = self.items.get_mut(show) {
            show.season_count = Some(show.season_count.unwrap_or(0) + 1);
        }
    }

    fn get(&self, id: &MediaId) -> Result<&Item, BackendError> {
        self.items
            .get(id)
            .ok_or_else(|| BackendError::NotFound(id.to_string()))
    }
}

fn length_of(item: &Item) -> Option<Duration> {
    match item {
        Item::Movie(m) => m.duration,
        Item::Episode(e) => e.duration,
        Item::Track(t) => t.duration,
        _ => None,
    }
}

/// One user's watch state: the server keeps it per principal (§36).
#[derive(Default)]
struct WatchState {
    states: HashMap<MediaId, PlaybackState>,
    /// Movies and episodes in the order their state last changed, newest
    /// last: Continue watching and Next up are ordered by it.
    touched: Vec<MediaId>,
    /// Movies and shows taken out of Continue watching and Next up (P1-22),
    /// until their progress next changes.
    hidden: std::collections::HashSet<MediaId>,
    /// Each item's counted plays (PX-52): how many, and when the latest
    /// was counted, Unix seconds.
    plays: HashMap<MediaId, (u32, i64)>,
    /// When each item was last viewed, Unix seconds: its last progress
    /// report or mark as watched or unwatched.
    viewed: HashMap<MediaId, i64>,
}

impl WatchState {
    fn get(&self, id: &MediaId) -> PlaybackState {
        self.states.get(id).cloned().unwrap_or_default()
    }

    /// `id`'s state as the session is shown it: with when it was last
    /// viewed and its plays, for the features it has. The real server
    /// sends them only on some replies (Home's rows, an item's details,
    /// Recently played); the fake sends them wherever an item has a
    /// state, which no screen relies on.
    fn shown(&self, id: &MediaId, features: Features) -> PlaybackState {
        let mut state = self.get(id);
        if features.last_viewed {
            state.last_viewed_at = self.viewed.get(id).copied();
        }
        if features.play_history
            && let Some((count, at)) = self.plays.get(id)
        {
            state.play_count = *count;
            state.last_played_at = Some(*at);
        }
        state
    }

    /// A counted play of `id`, at `at`.
    fn played(&mut self, id: &MediaId, at: i64) {
        let plays = self.plays.entry(id.clone()).or_insert((0, at));
        *plays = (plays.0 + 1, at);
    }

    fn set(&mut self, id: &MediaId, state: PlaybackState) {
        self.touched.retain(|t| t != id);
        self.touched.push(id.clone());
        self.states.insert(id.clone(), state);
    }

    fn progress(
        &mut self,
        id: &MediaId,
        length: Option<Duration>,
        position: Duration,
        markers: &[Marker],
    ) {
        let rules = WatchRules::default();
        let watched = rules.is_watched(position, length, credits_to_end(markers));
        self.set(id, rules.playback_state(Some(position), watched));
    }
}

struct Playback {
    item: MediaId,
    selection: Selection,
    last_used: Instant,
    /// A converted playback's encode, and how much of it is written.
    epoch: u32,
    written: u64,
    /// This play so far (PX-52): where it started, the last position
    /// reported, how much has played, and whether it was counted.
    from: Option<Duration>,
    last: Option<Duration>,
    played: Duration,
    counted: bool,
}

/// The most one progress report credits as played (PX-52): a client
/// reports every 10 s, and a gap longer than this was not all playing.
const MOST_CREDITED: Duration = Duration::from_secs(30);

/// One user's view of the fake server.
pub struct FakeSession {
    catalog: Arc<Catalog>,
    watch: Mutex<WatchState>,
    serving: Serving,
    /// How often the subtitle being read as text was asked for.
    subtitle_asks: AtomicU32,
    /// How often previews were asked for.
    preview_asks: AtomicU32,
    /// Set by [`FakeClient::go_away`]: the server stopped answering.
    gone: Arc<std::sync::atomic::AtomicBool>,
    /// Every progress report, in order, for tests.
    reports: Mutex<Vec<(MediaId, Duration)>>,
    /// The display each playback was started for (PGPU-06), in order.
    displays: Mutex<Vec<Option<spoolframe_types::DisplayCapabilities>>>,
    playbacks: Mutex<HashMap<PlaybackHandle, Playback>>,
    next_handle: AtomicU64,
    /// Set once [`Serving::expire_at`] has ended a playback.
    expired_once: std::sync::atomic::AtomicBool,
    preferences: Mutex<TrackPreferences>,
    /// Tracks remembered per movie, or per show for its episodes.
    overrides: Mutex<HashMap<MediaId, TrackOverride>>,
    username: String,
    passkeys: Arc<Mutex<FakeAuth>>,
    pin: Mutex<FakePin>,
    /// Devices waiting to be signed in by code (P3-07), and wrong codes
    /// tried.
    sign_ins: Mutex<SignIns>,
    /// Which profile used the device's connection last: switching to
    /// another and back locks a profile again (P2-08).
    active: Arc<Mutex<std::collections::HashSet<String>>>,
    /// The server this session is on, and what the device keeps saved,
    /// so leaving it (PX-47) drops the profile here, and the server with its
    /// last profile, as the SDK does.
    server: ServerKey,
    saved: Saved,
    /// Every user's playlists (PX-40), the client's.
    playlists: Arc<Mutex<playlists::Store>>,
    /// The features this session has, as the store had them when it was
    /// made: read without its lock, which item reads may hold.
    features: Features,
    /// The server's subtitle sites and what was downloaded (PX-02), for
    /// everyone.
    subtitles: Arc<Mutex<subtitles::Store>>,
}

/// What this device keeps saved, shared by the client and its sessions.
#[derive(Clone)]
struct Saved {
    servers: Arc<Mutex<Vec<Server>>>,
    signed_in: Arc<Mutex<Vec<Profile>>>,
    changed: Arc<watch::Sender<()>>,
}

impl Saved {
    /// Drops `server` and everyone signed in to it, as the SDK's forget.
    fn forget(&self, server: &ServerKey) {
        self.servers
            .lock()
            .expect("fake state is never poisoned")
            .retain(|s| &s.key != server);
        self.signed_in
            .lock()
            .expect("fake state is never poisoned")
            .retain(|p| &p.server != server);
        self.changed.send_replace(());
    }

    /// `username` left `server`, as the SDK's `left` (C7): only its profile
    /// goes, and the server too once no profile is kept for it here,
    /// signed in or `remembered`.
    fn left(&self, server: &ServerKey, username: &str, remembered: bool) {
        let mut signed_in = self.signed_in.lock().expect("fake state is never poisoned");
        signed_in.retain(|p| !(&p.server == server && p.username == username));
        let kept = remembered || signed_in.iter().any(|p| &p.server == server);
        drop(signed_in);
        if kept {
            self.changed.send_replace(());
        } else {
            self.forget(server);
        }
    }
}

/// Device sign-ins waiting for this user's approval (P3-07).
struct SignIns {
    pending: HashMap<String, DeviceSignInRequest>,
    wrong: u32,
    /// Codes approved, and whether the device was remembered (P4-01).
    approved: Vec<(String, bool)>,
}

impl Default for SignIns {
    fn default() -> Self {
        let request = DeviceSignInRequest {
            device_name: Some("Living Room Apple TV".into()),
            client_name: "spoolframe-tvos".into(),
            requested_at: unix_now() - 30,
            platform: Platform::Tvos,
            // It asks to be remembered, so the approver's box shows ticked.
            remember: true,
            // On the home network (PX-09 finding 27); the approval screen
            // does not show it yet.
            requested_from: Some(spoolframe_client_sdk::DeviceAddress {
                address: "192.168.86.20".into(),
                network: spoolframe_client_sdk::AddressNetwork::Local,
            }),
        };
        Self {
            pending: [(FAKE_SIGN_IN_CODE.to_owned(), request)].into(),
            wrong: 0,
            approved: Vec::new(),
        }
    }
}

/// The fake's admin (P2-09); everyone else is a consumer.
pub const FAKE_ADMIN: &str = "andrey";

/// A fake user's display name: their username, capitalized ("Andrey").
fn display_name(username: &str) -> String {
    let mut chars = username.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The code a TV shows in the fixtures, waiting for approval.
pub const FAKE_SIGN_IN_CODE: &str = "BCDFGHJK";
/// The code this device itself would show: approving it is refused.
pub const FAKE_OWN_SIGN_IN_CODE: &str = "ZZZZZZZZ";
/// Wrong codes in a row before lookups are refused for a while.
const WRONG_CODES: u32 = 5;

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// One user's PIN, as the fake server keeps it (P2-08).
#[derive(Default)]
struct FakePin {
    pin: Option<String>,
    /// The fake's devices, by endpoint id, where the PIN is asked for.
    required: std::collections::HashSet<String>,
    locked: bool,
    wrong: u32,
    /// Ten wrong PINs in a row ended the session here.
    revoked: bool,
    /// Devices this user deleted (P4-03): gone from the list.
    deleted: std::collections::HashSet<String>,
}

/// The fake's two devices: this one, and a TV to set from here.
const THIS_DEVICE: &str = "this-device";
const LIVING_ROOM_TV: &str = "living-room-tv";
/// Alice's PIN in the fixtures; asked for on this device.
pub const FAKE_PIN: &str = "1234";
/// Wrong PINs in a row before the session here ends.
const WRONG_PINS: u32 = 10;

/// How sessions serve media: the client's settings, handed to each.
#[derive(Clone)]
struct Serving {
    /// The file every playback reads; `None` when the fake has no media.
    file: Option<PathBuf>,
    /// The most one read returns, like a server that answers short.
    read_limit: u64,
    /// Converts every playback (P2-04): the "encode" is the media file,
    /// revealed this many bytes per chunk read. `None` plays directly.
    converting: Option<u64>,
    /// The markers every playback starts with (P2-10).
    markers: Vec<Marker>,
    /// A converted playback's subtitle is read as text by the server
    /// (P2-07): not ready this many times, then there.
    reading_subtitle: Option<u32>,
    /// The server's admin has not finished setup (P3-10): its catalog
    /// answers nothing yet.
    not_set_up: bool,
    /// Whether the server has a public web address (P3-07, P3-12).
    web: bool,
    /// How the server answers for preview frames (P3-13).
    previews: PreviewsAnswer,
    /// The admin stops every playback (P3-15): reads are refused.
    stopped: bool,
    /// Sessions end under their playbacks (a password changed elsewhere):
    /// reads ask to sign in again.
    signed_out: bool,
    /// How long a playback handle may go unused before it expires.
    handle_idle: Duration,
    /// How fast the link has been, as the SDK would have measured it.
    link_rate: Option<u64>,
    /// The server ends the first playback once a chunk read reaches this
    /// offset, as when it idled out: the read answers PlaybackExpired.
    expire_at: Option<u64>,
}

/// How the fake answers for an item's preview frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewsAnswer {
    /// None: music, previews off.
    None,
    /// Still being made this many times, then ready.
    NotReadyFor(u32),
    Ready,
}

/// The subtitle tracks every film and episode of the fake has of its own;
/// downloaded ones follow them (PX-02).
const OWN_SUBTITLES: usize = 3;

/// The fake's preview sheet: two by two tiles of 64 × 36 px, red, green,
/// blue and yellow, a second apart.
pub const FAKE_PREVIEW_SHEET: &[u8] = include_bytes!("fake-preview-sheet.png");

impl FakeSession {
    fn new(
        catalog: Arc<Catalog>,
        username: &str,
        serving: Serving,
        gone: Arc<std::sync::atomic::AtomicBool>,
        active: Arc<Mutex<std::collections::HashSet<String>>>,
        passkeys: Arc<Mutex<FakeAuth>>,
        // Its server and the device's saved list, together: leaving needs
        // both; and the server's playlists and subtitles.
        (server, saved, playlists, subtitles): (
            ServerKey,
            Saved,
            Arc<Mutex<playlists::Store>>,
            Arc<Mutex<subtitles::Store>>,
        ),
    ) -> Self {
        // Alice has a PIN, asked for here, so the fake opens her locked.
        let pin = match username {
            "alice" => FakePin {
                pin: Some(FAKE_PIN.into()),
                required: [THIS_DEVICE.to_owned()].into(),
                locked: true,
                ..FakePin::default()
            },
            _ => FakePin::default(),
        };
        let features = playlists
            .lock()
            .expect("fake state is never poisoned")
            .features;
        let session = FakeSession {
            features,
            catalog,
            watch: Mutex::new(WatchState::default()),
            serving,
            subtitle_asks: AtomicU32::new(0),
            preview_asks: AtomicU32::new(0),
            gone,
            reports: Mutex::new(Vec::new()),
            displays: Mutex::new(Vec::new()),
            playbacks: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            expired_once: std::sync::atomic::AtomicBool::new(false),
            preferences: Mutex::new(TrackPreferences::default()),
            overrides: Mutex::new(HashMap::new()),
            username: username.to_owned(),
            passkeys,
            pin: Mutex::new(pin),
            sign_ins: Mutex::new(SignIns::default()),
            active,
            server,
            saved,
            playlists,
            subtitles,
        };
        {
            let mut watch = session.watch();
            let watched = || PlaybackState {
                position: None,
                watched: true,
                ..PlaybackState::default()
            };
            let at = |secs: u64| PlaybackState {
                position: Some(Duration::from_secs(secs)),
                watched: false,
                ..PlaybackState::default()
            };
            // Severance S2 E1–3 watched and E4 half-way, for everyone.
            for key in ["22", "23", "24"] {
                watch.set(&media(MediaKind::Episode, key), watched());
            }
            watch.set(&media(MediaKind::Episode, "25"), at(32 * 60));
            if username != "alice" {
                // Andor E1–2 watched, so Next up has E3.
                for key in ["62", "63"] {
                    watch.set(&media(MediaKind::Episode, key), watched());
                }
                watch.set(&media(MediaKind::Movie, "2"), watched());
                watch.set(&media(MediaKind::Movie, "4"), watched());
                watch.set(&media(MediaKind::Movie, "8"), at(114 * 60));
                watch.set(&media(MediaKind::Movie, "1"), at(37 * 60 + 12));
            }
            // Played and viewed, from other devices (PX-52): Severance's
            // watched episodes once each, Low Tide's tracks a few times.
            let days_ago = |days: i64| unix_now() - days * 86_400;
            for (key, days) in [("22", 9), ("23", 8), ("24", 3)] {
                let id = media(MediaKind::Episode, key);
                watch.played(&id, days_ago(days));
                watch.viewed.insert(id, days_ago(days));
            }
            watch
                .viewed
                .insert(media(MediaKind::Episode, "25"), days_ago(2));
            for (key, plays, days) in [("42", 5, 1), ("40", 2, 3), ("46", 1, 6)] {
                let id = media(MediaKind::Track, key);
                for _ in 0..plays {
                    watch.played(&id, days_ago(days));
                }
            }
        }
        session
    }

    fn watch(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.watch.lock().expect("fake state is never poisoned")
    }

    /// The item with this user's playback state filled in.
    fn with_state(&self, item: &Item, watch: &WatchState) -> Item {
        let mut item = item.clone();
        match &mut item {
            Item::Movie(m) => m.playback = watch.shown(&m.id, self.features),
            Item::Episode(e) => e.playback = watch.shown(&e.id, self.features),
            // A track has no resume point, but it has plays (PX-52).
            Item::Track(t) => t.playback = watch.shown(&t.id, self.features),
            _ => {}
        }
        item
    }

    fn items_with_state(&self, ids: &[MediaId]) -> Vec<Item> {
        let watch = self.watch();
        ids.iter()
            .filter_map(|id| self.catalog.items.get(id))
            .map(|item| self.with_state(item, &watch))
            .collect()
    }

    /// What hiding `id` hides: an episode hides its whole show.
    /// The show an episode belongs to.
    fn show_of(&self, id: &MediaId) -> Option<MediaId> {
        match self.catalog.items.get(id) {
            Some(Item::Episode(e)) => Some(e.show_id.clone()),
            _ => None,
        }
    }

    /// Progress or a mark on `id` brings that card back, as on the
    /// server: only it, not the show's other removed cards.
    fn unhide(&self, id: &MediaId) {
        self.watch().hidden.remove(id);
    }

    fn home_now(&self) -> HomeScreen {
        let watch = self.watch();
        let continue_ids: Vec<MediaId> = watch
            .touched
            .iter()
            .rev()
            .filter(|id| {
                let s = watch.get(id);
                s.position.is_some() && !s.watched
            })
            .cloned()
            .collect();

        // For each show with recent activity, newest first: the first
        // unwatched episode after the latest watched one. A show with an
        // episode in progress has that in Continue watching instead.
        let in_progress: Vec<MediaId> = continue_ids
            .iter()
            .filter_map(|id| self.show_of(id))
            .collect();
        let mut next_up = Vec::new();
        let mut shows_seen = Vec::new();
        for id in watch.touched.iter().rev() {
            let Some(Item::Episode(e)) = self.catalog.items.get(id) else {
                continue;
            };
            if shows_seen.contains(&e.show_id) || in_progress.contains(&e.show_id) {
                continue;
            }
            shows_seen.push(e.show_id.clone());
            let episodes = self
                .catalog
                .children
                .get(&e.season_id)
                .cloned()
                .unwrap_or_default();
            let last_watched = episodes.iter().rposition(|ep| watch.get(ep).watched);
            let next = last_watched
                .and_then(|i| episodes.get(i + 1..))
                .and_then(|rest| rest.iter().find(|ep| !watch.get(ep).watched));
            if let Some(next) = next {
                next_up.push(next.clone());
            }
        }
        let hidden = watch.hidden.clone();
        drop(watch);
        // Removed cards (P1-22): that card only, as on the server.
        let shown = |id: &MediaId| !hidden.contains(id);
        let continue_ids: Vec<MediaId> = continue_ids.into_iter().filter(|id| shown(id)).collect();
        let next_up: Vec<MediaId> = next_up.into_iter().filter(|id| shown(id)).collect();
        HomeScreen {
            continue_watching: self.items_with_state(&continue_ids),
            next_up: self.items_with_state(&next_up),
            recently_added: self.items_with_state(&self.catalog.recently_added),
        }
    }

    fn tracks_of(&self, item: &Item) -> MediaTracks {
        let audio = |title: Option<&str>, channels, default, role| AudioTrack {
            language: Some("eng".into()),
            title: title.map(str::to_owned),
            channels: Some(channels),
            default,
            role,
        };
        let subtitle = |title: Option<&str>, role, external| SubtitleTrack {
            language: Some("eng".into()),
            title: title.map(str::to_owned),
            forced: role == SubtitleRole::Forced,
            default: false,
            external,
            role,
            timing: None,
            download: None,
        };
        match item {
            Item::Track(_) => MediaTracks {
                duration: length_of(item),
                container: Some(Container::Flac),
                bit_rate: None,
                video: None,
                audio: vec![audio(None, 2, true, AudioRole::Main)],
                subtitles: Vec::new(),
            },
            _ => MediaTracks {
                duration: length_of(item),
                container: Some(Container::Matroska),
                bit_rate: None,
                video: Some(VideoTrack {
                    codec: VideoCodec::Hevc,
                    width: 3840,
                    height: 2160,
                    bit_depth: 10,
                    hdr: Some(HdrFormat::Hdr10),
                    bit_rate: None,
                }),
                audio: vec![
                    audio(None, 6, true, AudioRole::Main),
                    audio(Some("Commentary"), 2, false, AudioRole::Commentary),
                    audio(
                        Some("Audio Description"),
                        2,
                        false,
                        AudioRole::AudioDescription,
                    ),
                ],
                subtitles: [
                    subtitle(None, SubtitleRole::Full, true),
                    subtitle(Some("Forced"), SubtitleRole::Forced, false),
                    subtitle(Some("SDH"), SubtitleRole::Sdh, false),
                ]
                .into_iter()
                // Those downloaded from a subtitle site (PX-02), after.
                .chain(self.lock_subtitles().tracks(
                    item.id(),
                    self.asker(&display_name(&self.username)),
                    self.features.subtitle_search,
                ))
                .collect(),
            },
        }
    }

    /// What the tracks are remembered under: a movie's own ID, an episode's
    /// show, as the server does (P1-20).
    fn override_key(item: &Item) -> MediaId {
        match item {
            Item::Episode(e) => e.show_id.clone(),
            other => other.id().clone(),
        }
    }

    /// The tracks the server would pick: the remembered ones, else the
    /// user's preferences, using the production policy.
    fn choose(&self, item: &Item, media: &MediaTracks) -> ChosenTracks {
        let preferences = self
            .preferences
            .lock()
            .expect("fake state is never poisoned");
        let overrides = self.lock_overrides();
        tracks::choose(
            media,
            &preferences,
            overrides.get(&Self::override_key(item)),
        )
    }

    fn pin_state(&self) -> std::sync::MutexGuard<'_, FakePin> {
        self.pin.lock().expect("fake state is never poisoned")
    }

    /// The proof given to change or remove the PIN: the current PIN, as an
    /// unlock checks it, or the password, which never revokes. "sso" has
    /// no password.
    fn check_proof(&self, proof: &PinProof) -> Result<(), BackendError> {
        match proof {
            PinProof::Pin { pin } => self.check_current(pin),
            PinProof::Password { .. } if self.username == "sso" => Err(BackendError::NoPassword),
            PinProof::Password { password } if password == FAKE_PASSWORD => Ok(()),
            PinProof::Password { .. } => Err(BackendError::WrongCredentials),
        }
    }

    /// The current PIN, given to change or remove it: a wrong one counts
    /// toward the lockout as an unlock does.
    fn check_current(&self, given: &str) -> Result<(), BackendError> {
        let mut pin = self.pin_state();
        if pin.revoked {
            return Err(BackendError::SignInRequired);
        }
        if pin.pin.as_deref() != Some(given) {
            pin.wrong += 1;
            if pin.wrong >= WRONG_PINS {
                pin.revoked = true;
                // Ten wrong PINs end trust too (P4-01).
                self.lock_passkeys().trusted.remove(&self.username);
                return Err(BackendError::SignInRequired);
            }
            return Err(BackendError::WrongCredentials);
        }
        pin.wrong = 0;
        Ok(())
    }

    /// Whether choosing this profile now asks for its PIN: it is locked,
    /// or another profile used the connection since it was unlocked.
    fn would_lock(&self) -> bool {
        let pin = self.pin_state();
        let active = self.active.lock().expect("fake state is never poisoned");
        pin.required.contains(THIS_DEVICE) && (pin.locked || !active.contains(&self.username))
    }

    /// What every call but unlocking goes through, as on the server: a
    /// locked profile answers nothing else.
    /// [`FakeSession::guard`], for the catalog's calls, which a server not
    /// set up yet refuses (P3-10).
    fn catalog(&self) -> Result<(), BackendError> {
        self.guard()?;
        if self.serving.not_set_up {
            return Err(BackendError::NotSetUp);
        }
        Ok(())
    }

    fn guard(&self) -> Result<(), BackendError> {
        if self.lock_passkeys().must_change.contains(&self.username) {
            return Err(BackendError::PasswordChangeRequired);
        }
        let mut pin = self.pin_state();
        if pin.revoked {
            return Err(BackendError::SignInRequired);
        }
        let active = self.active.lock().expect("fake state is never poisoned");
        if pin.required.contains(THIS_DEVICE) && !active.contains(&self.username) {
            pin.locked = true;
        }
        if pin.locked {
            return Err(BackendError::ProfileLocked);
        }
        Ok(())
    }

    /// What the fake converts to: HEVC it "cannot play" encoded to H.264,
    /// the audio copied.
    fn converted_plan(item: &Item, selection: Selection, ocr: bool) -> PlaybackPlan {
        let video = !matches!(item, Item::Track(_));
        PlaybackPlan::Converted {
            delivery: Delivery::Progressive,
            container: Container::Matroska,
            video: video.then_some(ConvertedVideo::Encode {
                codec: VideoCodec::H264,
                height: 1080,
                max_bitrate: Some(8_000_000),
                tone_map: false,
            }),
            audio: Some(ConvertedAudio {
                index: selection.audio.unwrap_or(0),
                action: AudioAction::Copy {
                    codec: AudioCodec::Aac,
                },
            }),
            subtitle: ocr.then_some(ConvertedSubtitle {
                index: 0,
                action: SubtitleAction::Ocr {
                    from: SubtitleFormat::Pgs,
                    to: SubtitleFormat::Srt,
                },
            }),
            reasons: vec!["video 0: hevc is not supported; encoded to h264".into()],
            // Progressive: no HLS variants (PX-21).
            ladder: Vec::new(),
        }
    }

    fn plan_for(item: &Item, tracks: &MediaTracks, selection: Selection) -> PlaybackPlan {
        let subtitle = selection.subtitle.and_then(|i| {
            let track = tracks.subtitles.get(i as usize)?;
            Some(PlannedSubtitle {
                index: i,
                format: if track.external {
                    SubtitleFormat::Srt
                } else {
                    SubtitleFormat::Pgs
                },
                delivery: if track.external {
                    SubtitleDelivery::External
                } else {
                    SubtitleDelivery::Embedded
                },
            })
        });
        PlaybackPlan::Direct {
            container: Container::Matroska,
            video: (!matches!(item, Item::Track(_))).then_some(VideoCodec::H264),
            audio: Some(PlannedAudio {
                index: selection.audio.unwrap_or(0),
                codec: AudioCodec::Aac,
            }),
            subtitle,
        }
    }

    /// The playback behind `handle`, touched; expired when idle too long.
    fn playback(&self, handle: &PlaybackHandle) -> Result<(MediaId, Selection), BackendError> {
        self.with_playback(handle, |p| (p.item.clone(), p.selection))
    }

    /// Credits a report at `position` to its playback's play, as the
    /// server does (PX-52): what the position moved by since the last
    /// report, at most [`MOST_CREDITED`], a seek back nothing. Whether
    /// the play counted just now, by the production rule; once a play.
    fn count_play(&self, handle: &PlaybackHandle, item: &Item, position: Duration) -> bool {
        let kind = match item {
            Item::Track(_) => PlayKind::Track,
            Item::Movie(_) | Item::Episode(_) => PlayKind::Video,
            _ => return false,
        };
        let markers = &self.serving.markers;
        self.with_playback(handle, |p| {
            let moved = match p.last {
                Some(last) => position.saturating_sub(last),
                None => position,
            }
            .min(MOST_CREDITED);
            let from = *p
                .from
                .get_or_insert_with(|| p.last.unwrap_or(position.saturating_sub(moved)));
            p.last = Some(position);
            p.played += moved;
            let progress = PlayProgress {
                played: p.played,
                from,
                position,
                duration: length_of(item),
                credits: credits_to_end(markers),
            };
            let counts = !p.counted && plays::counts(&WatchRules::default(), kind, &progress);
            p.counted |= counts;
            counts
        })
        .unwrap_or(false)
    }

    fn with_playback<T>(
        &self,
        handle: &PlaybackHandle,
        run: impl FnOnce(&mut Playback) -> T,
    ) -> Result<T, BackendError> {
        let mut playbacks = self.playbacks.lock().expect("fake state is never poisoned");
        let playback = playbacks
            .get_mut(handle)
            .ok_or(BackendError::PlaybackExpired)?;
        if playback.last_used.elapsed() > self.serving.handle_idle {
            playbacks.remove(handle);
            return Err(BackendError::PlaybackExpired);
        }
        playback.last_used = Instant::now();
        Ok(run(playback))
    }

    fn media_size(&self) -> Result<(PathBuf, u64), BackendError> {
        let Some(path) = &self.serving.file else {
            return Err(BackendError::Unavailable(
                "the fake server has no media; start the app with --fake-media FILE".into(),
            ));
        };
        let size = std::fs::metadata(path)
            .map_err(|e| BackendError::Unavailable(format!("{}: {e}", path.display())))?
            .len();
        Ok((path.clone(), size))
    }

    /// Up to `length` bytes of the media file from `offset`.
    fn file_bytes(&self, offset: u64, length: u64) -> Result<Vec<u8>, BackendError> {
        let (path, _) = self.media_size()?;
        let io = |e: std::io::Error| BackendError::Unavailable(format!("{}: {e}", path.display()));
        let mut file = std::fs::File::open(&path).map_err(io)?;
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut bytes = Vec::new();
        file.take(length.min(MAX_READ).min(self.serving.read_limit))
            .read_to_end(&mut bytes)
            .map_err(io)?;
        Ok(bytes)
    }
}

impl Session for FakeSession {
    fn activate(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            if self.pin_state().revoked {
                return Err(BackendError::SignInRequired);
            }
            self.active
                .lock()
                .expect("fake state is never poisoned")
                .retain(|name| name == &self.username);
            Ok(())
        })
    }

    fn home(&self) -> BoxFuture<'_, Result<HomeScreen, BackendError>> {
        Box::pin(async move {
            self.catalog()?;
            Ok(self.home_now())
        })
    }

    fn libraries(&self) -> BoxFuture<'_, Result<Vec<Library>, BackendError>> {
        Box::pin(async move {
            self.catalog()?;
            Ok(self
                .catalog
                .libraries
                .iter()
                .map(|(l, _)| l.clone())
                .collect())
        })
    }

    fn items<'a>(
        &'a self,
        library: &'a LibraryId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>> {
        Box::pin(async move {
            self.catalog()?;
            let (_, ids) = self
                .catalog
                .libraries
                .iter()
                .find(|(l, _)| &l.id == library)
                .ok_or_else(|| BackendError::NotFound(library.to_string()))?;
            let page: Vec<MediaId> = ids
                .iter()
                .skip(offset as usize)
                .take(limit as usize)
                .cloned()
                .collect();
            Ok((self.items_with_state(&page), ids.len() as u32))
        })
    }

    fn item<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(Item, Vec<Item>), BackendError>> {
        Box::pin(async move {
            self.catalog()?;
            let item = self.catalog.get(id)?;
            let item = self.with_state(item, &self.watch());
            let children = self.catalog.children.get(id).cloned().unwrap_or_default();
            Ok((item, self.items_with_state(&children)))
        })
    }

    fn item_detail<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<ItemDetail, BackendError>> {
        Box::pin(async move {
            let (item, children) = self.item(id).await?;
            let store = self.lock_playlists();
            let liked = store.liked(&self.username);
            // A heart for a track or an album only, with Liked.
            let liked_children: Vec<MediaId> = match id.kind() {
                MediaKind::Album => children
                    .iter()
                    .map(|c| c.id().clone())
                    .filter(|c| liked.contains(c))
                    .collect(),
                _ => Vec::new(),
            };
            let heart = match (store.features.liked, id.kind()) {
                (false, _) => None,
                (true, MediaKind::Track) => Some(match liked.contains(id) {
                    true => crate::backend::Liked::All,
                    false => crate::backend::Liked::None,
                }),
                (true, MediaKind::Album) => Some(match liked_children.len() {
                    0 => crate::backend::Liked::None,
                    n if n == children.len() => crate::backend::Liked::All,
                    _ => crate::backend::Liked::Some,
                }),
                (true, _) => None,
            };
            // Only for a client that listed the feature; none otherwise.
            let playlists = match store.features.playlists {
                true => {
                    let mut held: std::collections::HashSet<MediaId> = self
                        .leaves(std::slice::from_ref(id))
                        .iter()
                        .map(|i| i.id().clone())
                        .collect();
                    held.insert(id.clone());
                    store.holding(&held, self.viewer())
                }
                false => Vec::new(),
            };
            Ok(ItemDetail {
                item,
                children,
                playlists,
                liked: heart,
                liked_children,
            })
        })
    }

    fn search<'a>(
        &'a self,
        request: &'a SearchRequest,
    ) -> BoxFuture<'a, Result<SearchResults, BackendError>> {
        Box::pin(async move {
            self.catalog()?;
            let mut results = search::run(&self.catalog, request);
            // As the server answers: with this user's watch state.
            let stated = |items: &[Item]| {
                self.items_with_state(&items.iter().map(|i| i.id().clone()).collect::<Vec<_>>())
            };
            results.top = match results.top {
                Some(TopResult::Item(top)) => stated(&[top]).pop().map(TopResult::Item),
                person => person,
            };
            for group in &mut results.groups {
                group.items = stated(&group.items);
            }
            results.without_year = stated(&results.without_year);
            Ok(results)
        })
    }

    fn person_items<'a>(
        &'a self,
        person: &'a PersonId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>> {
        Box::pin(async move {
            self.catalog()?;
            let (_, items) = self
                .catalog
                .people
                .iter()
                .find(|(p, _)| &p.id == person)
                .ok_or_else(|| BackendError::NotFound(person.to_string()))?;
            let page: Vec<MediaId> = items
                .iter()
                .skip(offset as usize)
                .take(limit as usize)
                .cloned()
                .collect();
            Ok((self.items_with_state(&page), items.len() as u32))
        })
    }

    fn tracks<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<MediaTracks, BackendError>> {
        Box::pin(async move { Ok(self.tracks_of(self.catalog.get(id)?)) })
    }

    fn playback_plan<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
    ) -> BoxFuture<'a, Result<(PlaybackPlan, Option<ChosenTracks>), BackendError>> {
        Box::pin(async move {
            let item = self.catalog.get(id)?;
            let tracks = self.tracks_of(item);
            let chosen = selection.is_none().then(|| self.choose(item, &tracks));
            let effective = selection
                .or(chosen.map(|c| c.selection))
                .unwrap_or_default();
            Ok((Self::plan_for(item, &tracks, effective), chosen))
        })
    }

    fn start_playback<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
        options: crate::backend::PlaybackOptions,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>> {
        Box::pin(async move {
            self.catalog()?;
            let quality = options.quality;
            self.displays
                .lock()
                .expect("fake state is never poisoned")
                .push(options.display);
            let item = self.catalog.get(id)?;
            let tracks = self.tracks_of(item);
            let (_, size) = self.media_size()?;
            let chosen = selection.is_none().then(|| self.choose(item, &tracks));
            let effective = selection
                .or(chosen.map(|c| c.selection))
                .unwrap_or_default();
            let handle = PlaybackHandle(format!(
                "fake-{}",
                self.next_handle.fetch_add(1, Ordering::Relaxed)
            ));
            self.playbacks
                .lock()
                .expect("fake state is never poisoned")
                .insert(
                    handle.clone(),
                    Playback {
                        item: id.clone(),
                        selection: effective,
                        last_used: Instant::now(),
                        epoch: 0,
                        written: 0,
                        from: None,
                        last: options.start,
                        played: Duration::ZERO,
                        counted: false,
                    },
                );
            // A chosen bitrate converts the fake's file, as one that
            // exceeds it would be (P6-06).
            let capped = matches!(quality, crate::backend::PlaybackQuality::Bitrate(_));
            let (plan, size) = match self.serving.converting {
                _ if capped => (Self::converted_plan(item, effective, false), 0),
                Some(_) => (
                    Self::converted_plan(item, effective, self.serving.reading_subtitle.is_some()),
                    0,
                ),
                None => (Self::plan_for(item, &tracks, effective), size),
            };
            Ok(PlaybackStart {
                handle,
                plan,
                size,
                chosen,
                epoch: 0,
                markers: self.serving.markers.clone(),
            })
        })
    }

    fn read_chunk<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        epoch: u32,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Chunk, BackendError>> {
        Box::pin(async move {
            if self.gone.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(BackendError::Unreachable("Home Media".into()));
            }
            let (_, size) = self.media_size()?;
            if self.serving.expire_at.is_some_and(|at| offset >= at)
                && !self.expired_once.swap(true, Ordering::SeqCst)
            {
                self.playbacks
                    .lock()
                    .expect("fake state is never poisoned")
                    .remove(handle);
                return Err(BackendError::PlaybackExpired);
            }
            let grow = self.serving.converting.unwrap_or(size);
            // Each read lets the "encode" write a little more.
            let (current, written) = self.with_playback(handle, |p| {
                p.written = (p.written + grow).min(size);
                (p.epoch, p.written)
            })?;
            if epoch != current {
                return Err(BackendError::PlaybackRestarted { epoch: current });
            }
            let available = written.saturating_sub(offset).min(length);
            let bytes = if available == 0 {
                Vec::new()
            } else {
                self.file_bytes(offset, available)?
            };
            Ok(Chunk {
                bytes,
                size: written,
                complete: written == size,
                epoch: current,
            })
        })
    }

    fn seek_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        _position: Duration,
    ) -> BoxFuture<'a, Result<u32, BackendError>> {
        // The fake's "encode" is the file itself, from its start: the
        // restart is in the epoch and the bytes written, not the time.
        Box::pin(async move {
            self.with_playback(handle, |p| {
                p.epoch += 1;
                p.written = 0;
                p.epoch
            })
        })
    }

    fn stop_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.playbacks
                .lock()
                .expect("fake state is never poisoned")
                .remove(handle);
            Ok(())
        })
    }

    fn read<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>> {
        Box::pin(async move {
            if self.gone.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(BackendError::Unreachable("Home Media".into()));
            }
            if self.serving.stopped {
                return Err(BackendError::PlaybackStopped);
            }
            if self.serving.signed_out {
                return Err(BackendError::SignInRequired);
            }
            self.playback(handle)?;
            self.file_bytes(offset, length)
        })
    }

    fn read_subtitle<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(SubtitleFormat, Vec<u8>), BackendError>> {
        Box::pin(async move {
            let (id, selection) = self.playback(handle)?;
            if let Some(waits) = self.serving.reading_subtitle {
                if self.subtitle_asks.fetch_add(1, Ordering::SeqCst) < waits {
                    return Err(BackendError::SubtitleNotReady(50));
                }
            } else if selection.subtitle != Some(0) {
                // A track downloaded from a subtitle site (PX-02), after
                // the item's three own.
                let site = selection.subtitle.and_then(|n| {
                    self.lock_subtitles()
                        .site_of(&id, n as usize, OWN_SUBTITLES)
                        .map(str::to_owned)
                });
                let Some(site) = site else {
                    return Err(BackendError::NotFound(
                        "an external subtitle in this playback".into(),
                    ));
                };
                let srt = format!(
                    "1\n00:00:01,000 --> 00:00:05,000\nA subtitle downloaded from {site}.\n"
                );
                return Ok((SubtitleFormat::Srt, srt.into_bytes()));
            }
            let srt = "1\n00:00:01,000 --> 00:00:05,000\nA subtitle from the fake server.\n\n\
                       2\n00:00:06,000 --> 00:00:10,000\nIt came over read_subtitle.\n";
            Ok((SubtitleFormat::Srt, srt.as_bytes().to_vec()))
        })
    }

    fn artwork<'a>(
        &'a self,
        id: &'a ArtworkId,
        _size: ArtworkSize,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>> {
        // The fixture items carry no artwork; a preview sheet is the one
        // picture the fake has.
        Box::pin(async move {
            self.catalog()?;
            // A playlist's picture (PX-53) is the one picture too.
            if matches!(id.namespace(), Namespace::Preview | Namespace::Image) {
                return Ok(FAKE_PREVIEW_SHEET.to_vec());
            }
            Err(BackendError::NotFound(id.to_string()))
        })
    }

    fn web_url(&self) -> Option<String> {
        self.serving
            .web
            .then(|| "https://media.example.org".to_owned())
    }

    fn connection_path(&self) -> ConnectionPath {
        ConnectionPath::Lan(([192, 168, 1, 20], 7431).into())
    }

    fn link_rate(&self) -> Option<u64> {
        self.serving.link_rate
    }

    /// A chosen bitrate converts the fake's file; Auto and Original play
    /// it as it is, as the fake's link is never too slow.
    fn change_quality<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        quality: crate::backend::PlaybackQuality,
        _position: Duration,
    ) -> BoxFuture<'a, Result<crate::backend::PlaybackChange, BackendError>> {
        Box::pin(async move {
            let (id, selection, epoch) = self.with_playback(handle, |p| {
                p.epoch += 1;
                p.written = 0;
                (p.item.clone(), p.selection, p.epoch)
            })?;
            let item = self.catalog.get(&id)?;
            let (_, size) = self.media_size()?;
            let (plan, size) = match quality {
                crate::backend::PlaybackQuality::Bitrate(_) => {
                    (Self::converted_plan(item, selection, false), 0)
                }
                _ => (Self::plan_for(item, &self.tracks_of(item), selection), size),
            };
            Ok(crate::backend::PlaybackChange { plan, size, epoch })
        })
    }

    /// The fake's file is too big for any link it is told of: the
    /// playback becomes a conversion, its encode started again, as the
    /// server re-plans a direct play on a slow link (P6-06).
    fn limit_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        _link_bitrate: u64,
        _position: Duration,
    ) -> BoxFuture<'a, Result<crate::backend::PlaybackChange, BackendError>> {
        Box::pin(async move {
            let (id, selection, epoch) = self.with_playback(handle, |p| {
                p.epoch += 1;
                p.written = 0;
                (p.item.clone(), p.selection, p.epoch)
            })?;
            let item = self.catalog.get(&id)?;
            Ok(crate::backend::PlaybackChange {
                plan: Self::converted_plan(item, selection, false),
                size: 0,
                epoch,
            })
        })
    }

    fn previews<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<Previews, BackendError>> {
        Box::pin(async move {
            self.catalog()?;
            let item = self.catalog.get(id)?;
            match self.serving.previews {
                PreviewsAnswer::None => return Err(BackendError::NoPreviews),
                _ if matches!(item, Item::Track(_)) => return Err(BackendError::NoPreviews),
                PreviewsAnswer::NotReadyFor(times)
                    if self.preview_asks.fetch_add(1, Ordering::SeqCst) < times =>
                {
                    return Err(BackendError::PreviewsNotReady(30));
                }
                _ => {}
            }
            Ok(Previews {
                interval_ms: 1000,
                tile_width: 64,
                tile_height: 36,
                columns: 2,
                rows: 2,
                count: 4,
                sheets: vec![ArtworkId::new(Namespace::Preview, "fake/0").expect("not empty")],
            })
        })
    }

    fn update_progress<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        position: Duration,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let (id, _) = self.playback(handle)?;
            let id = &id;
            let item = self.catalog.get(id)?;
            self.reports
                .lock()
                .expect("fake state is never poisoned")
                .push((id.clone(), position));
            let counted = self.count_play(handle, item, position);
            let mut watch = self.watch();
            watch.viewed.insert(id.clone(), unix_now());
            if counted {
                watch.played(id, unix_now());
            }
            drop(watch);
            // A track is recorded (PX-52), but has no resume point.
            if matches!(item, Item::Movie(_) | Item::Episode(_)) {
                self.watch()
                    .progress(id, length_of(item), position, &self.serving.markers);
                self.unhide(id);
            }
            Ok(())
        })
    }

    fn mark_watched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.catalog.get(id)?;
            self.watch().set(
                id,
                PlaybackState {
                    position: None,
                    watched: true,
                    ..PlaybackState::default()
                },
            );
            self.watch().viewed.insert(id.clone(), unix_now());
            self.unhide(id);
            Ok(())
        })
    }

    fn hide_from_continue<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.catalog.get(id)?;
            self.watch().hidden.insert(id.clone());
            Ok(())
        })
    }

    fn unhide_from_continue<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.catalog.get(id)?;
            self.unhide(id);
            Ok(())
        })
    }

    fn mark_unwatched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.catalog.get(id)?;
            let mut watch = self.watch();
            watch.set(id, PlaybackState::default());
            watch.viewed.insert(id.clone(), unix_now());
            drop(watch);
            self.unhide(id);
            Ok(())
        })
    }
    fn track_preferences(&self) -> BoxFuture<'_, Result<TrackPreferencesInfo, BackendError>> {
        Box::pin(async move {
            Ok(TrackPreferencesInfo {
                preferences: self
                    .preferences
                    .lock()
                    .expect("fake state is never poisoned")
                    .clone(),
                // The fake reads picture subtitles as text.
                ocr_offered: true,
            })
        })
    }

    fn set_track_preferences<'a>(
        &'a self,
        preferences: &'a TrackPreferences,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            *self
                .preferences
                .lock()
                .expect("fake state is never poisoned") = preferences.clone();
            Ok(())
        })
    }

    fn set_track_override<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Selection,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let key = Self::override_key(self.catalog.get(id)?);
            let tracks = self.tracks_of(self.catalog.get(id)?);
            self.lock_overrides()
                .insert(key, TrackOverride::from_selection(&tracks, selection));
            Ok(())
        })
    }

    fn clear_track_override<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let key = Self::override_key(self.catalog.get(id)?);
            self.lock_overrides().remove(&key);
            Ok(())
        })
    }

    fn unlock<'a>(&'a self, given: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let mut pin = self.pin_state();
            if pin.revoked {
                return Err(BackendError::SignInRequired);
            }
            if pin.pin.as_deref() != Some(given) {
                pin.wrong += 1;
                if pin.wrong >= WRONG_PINS {
                    pin.revoked = true;
                    // Ten wrong PINs end trust too (P4-01).
                    self.lock_passkeys().trusted.remove(&self.username);
                    return Err(BackendError::SignInRequired);
                }
                return Err(BackendError::WrongCredentials);
            }
            pin.wrong = 0;
            pin.locked = false;
            self.active
                .lock()
                .expect("fake state is never poisoned")
                .insert(self.username.clone());
            Ok(())
        })
    }

    // The unlocked session is the proof: it came from a sign-in with the
    // password, and a PIN only ever locks such a session.
    fn pin(&self) -> BoxFuture<'_, Result<Option<PinState>, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let pin = self.pin_state();
            Ok(pin.pin.as_ref().map(|p| PinState {
                digits: Some(p.len() as u8),
            }))
        })
    }

    fn set_pin<'a>(
        &'a self,
        new: &'a str,
        proof: Option<PinProof>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if spoolframe_types::pin::pin_digits(new).is_none() {
                return Err(BackendError::InvalidInput("a PIN is 4 to 8 digits".into()));
            }
            let has_pin = self.pin_state().pin.is_some();
            if has_pin {
                let Some(proof) = proof else {
                    return Err(BackendError::InvalidInput(
                        "give the current PIN or your password".into(),
                    ));
                };
                self.check_proof(&proof)?;
            } else if proof.is_none() && self.lock_passkeys().stale {
                // A first PIN has nothing to prove it by: it wants a
                // recent sign-in, as on the server since protocol 6.
                return Err(BackendError::RecentSignInRequired);
            }
            self.pin_state().pin = Some(new.to_owned());
            Ok(())
        })
    }

    fn clear_pin(&self, proof: PinProof) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.check_proof(&proof)?;
            let mut pin = self.pin_state();
            pin.pin = None;
            pin.required.clear();
            Ok(())
        })
    }

    fn plex_link(&self) -> BoxFuture<'_, Result<Option<PlexLink>, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            Ok(None)
        })
    }

    fn set_device_trust<'a>(
        &'a self,
        device: Option<&'a str>,
        trusted: bool,
        auto_login: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let own = device.is_none_or(|d| d == THIS_DEVICE);
            if auto_login && !trusted {
                return Err(BackendError::InvalidInput(
                    "auto-login needs the device trusted".into(),
                ));
            }
            if trusted && !own {
                return Err(BackendError::InvalidInput(
                    "only this device can be trusted from here".into(),
                ));
            }
            let mut auth = self.lock_passkeys();
            if (trusted || !own) && auth.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            match (own, trusted) {
                (true, true) => {
                    if auto_login {
                        // One auto-login profile per device.
                        for auto in auth.trusted.values_mut() {
                            *auto = false;
                        }
                    }
                    auth.trusted.insert(self.username.clone(), auto_login);
                    drop(auth);
                    if auto_login {
                        self.pin_state().required.remove(THIS_DEVICE);
                    }
                }
                // Stopping here ends this session; the profile stays.
                (true, false) => {
                    auth.trusted.remove(&self.username);
                    drop(auth);
                    self.pin_state().revoked = true;
                }
                (false, _) => auth.tv_untrusted = true,
            }
            Ok(())
        })
    }

    fn sign_out(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            let mut auth = self.lock_passkeys();
            auth.trusted.remove(&self.username);
            auth.signed_out.insert(self.username.clone());
            drop(auth);
            self.pin_state().revoked = true;
            Ok(())
        })
    }

    fn confirm_password<'a>(
        &'a self,
        password: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if self.username == "sso" {
                return Err(BackendError::NoPassword);
            }
            if password != FAKE_PASSWORD {
                return Err(BackendError::WrongCredentials);
            }
            self.lock_passkeys().stale = false;
            Ok(())
        })
    }

    fn begin_confirm_passkey(&self) -> BoxFuture<'_, Result<PasskeyRequest, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let allow: Vec<Vec<u8>> = self
                .lock_passkeys()
                .keys
                .iter()
                .filter(|k| k.usable() && k.username == self.username)
                .map(|k| k.credential_id.clone())
                .collect();
            if allow.is_empty() {
                return Err(BackendError::NotFound("app passkey".into()));
            }
            Ok(PasskeyRequest {
                rp_id: fake_server_id().rp_id(),
                origin: fake_server_id().origin(),
                challenge: FAKE_PASSKEY_CHALLENGE.to_vec(),
                allow_credential_ids: allow,
            })
        })
    }

    fn finish_confirm_passkey(
        &self,
        signed: FinishPasskeySignIn,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let mut passkeys = self.lock_passkeys();
            let theirs = passkeys.keys.iter().any(|k| {
                k.usable() && k.username == self.username && k.credential_id == signed.credential_id
            });
            if !theirs {
                return Err(BackendError::WrongCredentials);
            }
            passkeys.stale = false;
            Ok(())
        })
    }

    fn is_admin(&self) -> bool {
        self.username == FAKE_ADMIN
    }

    fn password_change_required(&self) -> bool {
        self.lock_passkeys().must_change.contains(&self.username)
    }

    fn change_password<'a>(
        &'a self,
        current: &'a str,
        new: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let mut auth = self.lock_passkeys();
            let reset = auth.one_time.contains(&self.username);
            let right = if reset {
                current == FAKE_ONE_TIME_PASSWORD
            } else {
                current == FAKE_PASSWORD
            };
            if !right {
                return Err(BackendError::WrongCredentials);
            }
            if new.chars().count() < 8 {
                return Err(BackendError::InvalidInput(
                    "a password is at least 8 characters".into(),
                ));
            }
            if new == current {
                return Err(BackendError::InvalidInput(
                    "the new password is the same as the current one".into(),
                ));
            }
            auth.one_time.remove(&self.username);
            auth.must_change.remove(&self.username);
            Ok(())
        })
    }

    fn create_invite(&self, spec: InviteSpec) -> BoxFuture<'_, Result<MintedInvite, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if !self.is_admin() {
                return Err(BackendError::AdminOnly);
            }
            match spec.kind {
                InviteKind::Federation => {
                    return Err(BackendError::InvalidInput(
                        "federation invites arrive with federation".into(),
                    ));
                }
                InviteKind::PortableAccount if spec.role.is_some() => {
                    return Err(BackendError::InvalidInput(
                        "a portable account invite takes no role".into(),
                    ));
                }
                _ => {}
            }
            let mut auth = self.lock_passkeys();
            if auth.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            let n = auth.invites.len() + 1;
            let id = format!("inv-{n}");
            // A code the protocol reads, different for each invite.
            let alphabet = spoolframe_protocol::invite::CODE_ALPHABET;
            let code: String = (0..12)
                .map(|i| char::from(alphabet[(n * 7 + i * 3) % alphabet.len()]))
                .collect();
            let expires_at = spec.expires_in.map(|d| std::time::SystemTime::now() + d);
            let invite = spoolframe_protocol::invite::Invite {
                server: home_media().0,
                name: Some("Home Media".into()),
                addrs: vec!["192.168.1.20:7430".parse().expect("an address")],
                relays: Vec::new(),
                code: Some(code.clone()),
                expires_at: expires_at.and_then(|at| {
                    at.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs())
                }),
                role: spec.role,
                kind: match spec.kind {
                    InviteKind::PortableAccount => {
                        spoolframe_protocol::invite::InviteLinkKind::PortableAccount
                    }
                    _ => spoolframe_protocol::invite::InviteLinkKind::CreateUser,
                },
                lookup: None,
            };
            let (link, share_link) = (invite.link(), invite.share_link());
            auth.invites.push(InviteSummary {
                id: id.clone(),
                kind: spec.kind,
                note: spec.note,
                uses: spec.uses,
                used: 0,
                expires_at,
                created_by: display_name(&self.username),
                created_by_username: Some(self.username.clone()),
                created_from: InviteOrigin::Device {
                    name: Some("This computer".into()),
                    platform: Platform::Linux,
                    this_device: true,
                },
                revoked: false,
            });
            Ok(MintedInvite {
                id,
                kind: spec.kind,
                code: spoolframe_protocol::invite::display_code(&code),
                copy_string: share_link.clone(),
                share_link,
                link,
                expires_at,
                uses: spec.uses,
            })
        })
    }

    fn invites(&self) -> BoxFuture<'_, Result<Vec<InviteSummary>, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if !self.is_admin() {
                return Err(BackendError::AdminOnly);
            }
            Ok(self.lock_passkeys().invites.iter().rev().cloned().collect())
        })
    }

    fn revoke_invite<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if !self.is_admin() {
                return Err(BackendError::AdminOnly);
            }
            let mut auth = self.lock_passkeys();
            if auth.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            let invite = auth
                .invites
                .iter_mut()
                .find(|i| i.id == id)
                .ok_or_else(|| BackendError::NotFound(format!("invite {id}")))?;
            invite.revoked = true;
            Ok(())
        })
    }

    fn guest_identity(&self) -> Option<spoolframe_protocol::portable::PortableUserId> {
        if self.username != FAKE_GUEST {
            return None;
        }
        self.lock_passkeys().guest
    }

    fn tie_info(&self) -> BoxFuture<'_, Result<Vec<crate::backend::Tie>, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.not_a_guest()?;
            Ok(self
                .lock_passkeys()
                .ties
                .get(&self.username)
                .cloned()
                .unwrap_or_default())
        })
    }

    fn tie(
        &self,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.not_a_guest()?;
            let mut auth = self.lock_passkeys();
            // As the server: recency first, then the identity. An account
            // ties any number (P7-09); one tied here already changes
            // nothing.
            if auth.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            let holder = auth
                .ties
                .iter()
                .find(|(_, ties)| ties.iter().any(|t| t.id == identity))
                .map(|(username, _)| username.clone());
            match holder {
                Some(username) if username == self.username => Ok(()),
                // As the SDK's PortableIdTaken reaches the UI.
                Some(_) => Err(BackendError::Other(
                    "that identity is tied to another account on this server".into(),
                )),
                None => {
                    auth.ties
                        .entry(self.username.clone())
                        .or_default()
                        .push(crate::backend::Tie {
                            id: identity,
                            tied_at: std::time::SystemTime::now(),
                            last_used_at: None,
                        });
                    Ok(())
                }
            }
        })
    }

    fn untie(
        &self,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.not_a_guest()?;
            let mut auth = self.lock_passkeys();
            if auth.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            // Only that tie goes: the account's others and its sessions stay.
            let ties = auth.ties.entry(self.username.clone()).or_default();
            let before = ties.len();
            ties.retain(|t| t.id != identity);
            if ties.len() == before {
                return Err(BackendError::NotFound("tie".into()));
            }
            Ok(())
        })
    }

    /// As the server: recency first, then the last admin. The fake's one
    /// admin is [`FAKE_ADMIN`], so it is always the last. Then, as the
    /// SDK, this profile goes here, and the server with the last one.
    fn leave_server(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if self.lock_passkeys().stale {
                return Err(BackendError::RecentSignInRequired);
            }
            if self.is_admin() {
                return Err(BackendError::LastAdmin);
            }
            // Its trust goes with it; another's keeps the server. The
            // fake keeps trust by username, not server: any is taken as
            // kept for this one.
            let remembered = {
                let mut auth = self.lock_passkeys();
                auth.trusted.remove(&self.username);
                !auth.trusted.is_empty()
            };
            self.saved.left(&self.server, &self.username, remembered);
            // And the session ends, as the SDK's does.
            self.pin_state().revoked = true;
            Ok(())
        })
    }

    /// The fake's one admin is its last, as `leave_server` refuses.
    fn can_leave(&self) -> bool {
        !self.is_admin()
    }

    fn can_leave_now(&self) -> BoxFuture<'_, Result<bool, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            Ok(self.can_leave())
        })
    }

    fn has_playlists(&self) -> bool {
        self.features.playlists
    }

    fn has_liked(&self) -> bool {
        self.features.liked
    }

    fn like<'a>(
        &'a self,
        items: &'a [MediaId],
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists()
                .like(items, self.viewer(), |item| self.tracks_for(item))
        })
    }

    fn unlike<'a>(
        &'a self,
        items: &'a [MediaId],
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists()
                .unlike(items, self.viewer(), |item| self.tracks_for(item))
        })
    }

    fn has_last_viewed(&self) -> bool {
        self.features.last_viewed
    }

    fn has_playlist_sharing(&self) -> bool {
        self.features.playlist_sharing
    }

    fn has_play_history(&self) -> bool {
        self.features.play_history
    }

    fn has_playlist_art(&self) -> bool {
        self.features.playlist_art
    }

    fn has_subtitle_sync(&self) -> bool {
        self.features.subtitle_sync
    }

    fn has_subtitle_search(&self) -> bool {
        self.features.subtitle_search
    }

    fn search_subtitles<'a>(
        &'a self,
        id: &'a MediaId,
        language: Option<&'a str>,
    ) -> BoxFuture<'a, Result<SubtitleSearch, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.subtitle_search_listed()?;
            let title = crate::playlists::title(self.catalog.get(id)?);
            self.lock_subtitles().search(id, &title, language)
        })
    }

    fn download_subtitle<'a>(
        &'a self,
        id: &'a MediaId,
        result: &'a str,
    ) -> BoxFuture<'a, Result<SubtitleDownloaded, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.subtitle_search_listed()?;
            let title = crate::playlists::title(self.catalog.get(id)?);
            let name = display_name(&self.username);
            let asker = self.asker(&name);
            self.lock_subtitles()
                .download(id, &title, result, OWN_SUBTITLES, asker)
        })
    }

    fn remove_subtitle_download<'a>(
        &'a self,
        id: &'a MediaId,
        download: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.subtitle_search_listed()?;
            let name = display_name(&self.username);
            let asker = self.asker(&name);
            self.lock_subtitles().remove(id, download, asker)
        })
    }

    fn share_candidates<'a>(
        &'a self,
        id: &'a str,
        query: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<crate::backend::ShareCandidatePage, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists()
                .candidates(id, query, offset, limit, self.viewer())
        })
    }

    fn recently_played(
        &self,
        kinds: Option<Kinds>,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'_, Result<(Vec<Item>, u32), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if !self.features.play_history {
                return Err(BackendError::Unsupported(
                    spoolframe_protocol::features::PLAY_HISTORY.into(),
                ));
            }
            let watch = self.watch();
            let mut played: Vec<(&MediaId, i64)> =
                watch.plays.iter().map(|(id, (_, at))| (id, *at)).collect();
            // Newest first; plays at the same second by id, so a test
            // knows the order.
            played.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| a.0.to_string().cmp(&b.0.to_string()))
            });
            let items: Vec<Item> = played
                .into_iter()
                .filter_map(|(id, _)| self.catalog.items.get(id))
                .filter(|item| kinds.is_none_or(|k| k.allows(item.id().kind())))
                .map(|item| self.with_state(item, &watch))
                .collect();
            let total = items.len() as u32;
            let page = items
                .into_iter()
                .skip(offset as usize)
                .take(limit.min(500) as usize)
                .collect();
            Ok((page, total))
        })
    }

    fn playlists(&self) -> BoxFuture<'_, Result<PlaylistList, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let store = self.lock_playlists();
            store.check()?;
            Ok(store.list(self.viewer()))
        })
    }

    fn playlist<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<PlaylistPage, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let store = self.lock_playlists();
            store.check()?;
            let viewer = self.viewer();
            let (playlist, stored) = store.seen(id, viewer)?;
            let liked = store.liked(&self.username);
            let watch = self.watch();
            // An item gone from the library drops out.
            let entries: Vec<_> = stored
                .entries
                .iter()
                .filter_map(|(entry, item)| {
                    let found = self.catalog.items.get(item)?;
                    Some(crate::backend::PlaylistEntry {
                        entry: *entry,
                        item: self.with_state(found, &watch),
                        unavailable: store.unavailable.contains(item),
                        liked: liked.contains(item),
                    })
                })
                .collect();
            let total = entries.len() as u32;
            let playable = entries.iter().filter(|e| !e.unavailable).count() as u32;
            let (place, settings) = store.place(id, viewer);
            Ok(PlaylistPage {
                playlist,
                entries: entries
                    .into_iter()
                    .skip(offset as usize)
                    .take(limit.min(playlists::PAGE_LIMIT) as usize)
                    .collect(),
                offset,
                total,
                playable,
                place,
                settings,
            })
        })
    }

    fn create_playlist<'a>(
        &'a self,
        name: &'a str,
        kinds: Kinds,
        visibility: Visibility,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists()
                .create(name, kinds, visibility, self.viewer())
        })
    }

    fn update_playlist<'a>(
        &'a self,
        id: &'a str,
        update: PlaylistUpdate,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists().update(id, update, self.viewer())
        })
    }

    fn set_playlist_art<'a>(
        &'a self,
        id: &'a str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<ArtworkId, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists().set_art(id, &bytes, self.viewer())
        })
    }

    fn remove_playlist_art<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<PlaylistSummary, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists().remove_art(id, self.viewer())
        })
    }

    fn edit_playlist<'a>(
        &'a self,
        id: &'a str,
        revision: u64,
        edits: Vec<PlaylistEdit>,
    ) -> BoxFuture<'a, Result<u64, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists()
                .edit(id, revision, edits, self.viewer(), |item| {
                    self.tracks_for(item)
                })
        })
    }

    fn delete_playlist<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists().delete(id, self.viewer())
        })
    }

    fn pin_playlist<'a>(
        &'a self,
        id: &'a str,
        position: u32,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists().pin(id, position, self.viewer())
        })
    }

    fn unpin_playlist<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists().unpin(id, self.viewer())
        })
    }

    fn dismiss_pin_notes(&self) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            self.lock_playlists().dismiss_notes(self.viewer())
        })
    }

    fn play_playlist<'a>(
        &'a self,
        id: &'a str,
        from: Option<EntryId>,
        settings: PlaySettings,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let next = {
                let watch = self.watch();
                self.lock_playlists()
                    .play(id, from, settings, self.viewer(), |item| {
                        watch.get(item).watched
                    })?
            };
            Ok(next.and_then(|(entry, item)| self.playlist_next(entry, &item)))
        })
    }

    fn next_in_playlist<'a>(
        &'a self,
        id: &'a str,
        after: EntryId,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let next = {
                let watch = self.watch();
                self.lock_playlists()
                    .next(id, after, self.viewer(), |item| watch.get(item).watched)?
            };
            Ok(next.and_then(|(entry, item)| self.playlist_next(entry, &item)))
        })
    }

    fn begin_passkey_registration(&self) -> BoxFuture<'_, Result<PasskeyCreation, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let passkeys = self.lock_passkeys();
            if passkeys.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            Ok(PasskeyCreation {
                rp_id: fake_server_id().rp_id(),
                origin: fake_server_id().origin(),
                challenge: FAKE_PASSKEY_CHALLENGE.to_vec(),
                user_id: self.username.as_bytes().to_vec(),
                user_name: format!("{} on Home Media", self.username),
                display_name: format!("{} on Home Media", self.username),
                exclude_credential_ids: passkeys
                    .keys
                    .iter()
                    .filter(|k| k.usable() && k.username == self.username)
                    .map(|k| k.credential_id.clone())
                    .collect(),
            })
        })
    }

    fn finish_passkey_registration(
        &self,
        made: FinishPasskeyRegistration,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let name = passkey_name(&made.name)?;
            let mut passkeys = self.lock_passkeys();
            if passkeys.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            if made.client_data_json.is_empty() || made.attestation_object.is_empty() {
                return Err(BackendError::WrongCredentials);
            }
            passkeys.keys.push(FakePasskey {
                username: self.username.clone(),
                credential_id: made.credential_id,
                name,
                created_at: 1_791_000_000,
                last_used_at: None,
                rp_id: fake_server_id().rp_id(),
            });
            Ok(())
        })
    }

    fn passkeys(&self) -> BoxFuture<'_, Result<Vec<PasskeyInfo>, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            Ok(self
                .lock_passkeys()
                .keys
                .iter()
                .filter(|k| k.username == self.username)
                .map(FakePasskey::info)
                .collect())
        })
    }

    fn rename_passkey<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let name = passkey_name(name)?;
            let mut passkeys = self.lock_passkeys();
            let key = passkeys
                .keys
                .iter_mut()
                .find(|k| k.username == self.username && k.id() == id)
                .ok_or_else(|| BackendError::NotFound(format!("passkey {id}")))?;
            key.name = name;
            Ok(())
        })
    }

    fn remove_passkey<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let mut passkeys = self.lock_passkeys();
            if passkeys.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            let mine = |k: &FakePasskey| k.username == self.username;
            let at = passkeys
                .keys
                .iter()
                .position(|k| mine(k) && k.id() == id)
                .ok_or_else(|| BackendError::NotFound(format!("passkey {id}")))?;
            // "sso" has no password: their last passkey is their way in.
            let theirs = passkeys.keys.iter().filter(|k| mine(k)).count();
            if self.username == "sso" && theirs == 1 {
                return Err(BackendError::LastSignInMethod);
            }
            passkeys.keys.remove(at);
            Ok(())
        })
    }

    fn devices(&self) -> BoxFuture<'_, Result<Vec<DeviceInfo>, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let pin = self.pin_state();
            let auth = self.lock_passkeys();
            let here = auth.trusted.get(&self.username).copied();
            let device = |id: &str, name: &str, ago: i64| DeviceInfo {
                endpoint_id: id.to_owned(),
                name: Some(name.to_owned()),
                last_seen_at: 1_790_000_000 - ago,
                pin_required: pin.required.contains(id),
                trusted: if id == THIS_DEVICE {
                    here.is_some()
                } else {
                    !auth.tv_untrusted
                },
                auto_login: id == THIS_DEVICE && here == Some(true),
                this_device: id == THIS_DEVICE,
                platform: Some(if id == THIS_DEVICE {
                    Platform::Linux
                } else {
                    Platform::Tvos
                }),
            };
            Ok([
                device(THIS_DEVICE, "This computer", 0),
                device(LIVING_ROOM_TV, "Living room TV", 3 * 86_400),
            ]
            .into_iter()
            .filter(|d| !pin.deleted.contains(&d.endpoint_id))
            .collect())
        })
    }

    fn forget_device<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if device == THIS_DEVICE {
                return Err(BackendError::InvalidInput(
                    "this device signs out instead".into(),
                ));
            }
            if self.lock_passkeys().stale {
                return Err(BackendError::RecentSignInRequired);
            }
            let mut pin = self.pin_state();
            if device != LIVING_ROOM_TV || pin.deleted.contains(device) {
                return Err(BackendError::NotFound(format!("device {device}")));
            }
            pin.deleted.insert(device.to_owned());
            pin.required.remove(device);
            Ok(())
        })
    }

    fn set_pin_required<'a>(
        &'a self,
        device: Option<&'a str>,
        required: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let mut pin = self.pin_state();
            if pin.pin.is_none() {
                return Err(BackendError::InvalidInput("set a PIN first".into()));
            }
            // No longer asking for it needs a recent sign-in, as on the
            // server.
            if !required && self.lock_passkeys().stale {
                return Err(BackendError::RecentSignInRequired);
            }
            let device = device.unwrap_or(THIS_DEVICE).to_owned();
            // The PIN back on here turns auto-login off (P4-01).
            if required
                && device == THIS_DEVICE
                && let Some(auto) = self.lock_passkeys().trusted.get_mut(&self.username)
            {
                *auto = false;
            }
            if required {
                pin.required.insert(device);
            } else {
                pin.required.remove(&device);
            }
            Ok(())
        })
    }

    fn lookup_device_sign_in<'a>(
        &'a self,
        code: &'a str,
    ) -> BoxFuture<'a, Result<DeviceSignInRequest, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let mut sign_ins = self.sign_ins.lock().expect("fake state is never poisoned");
            if sign_ins.wrong >= WRONG_CODES {
                return Err(BackendError::RateLimited(Duration::from_secs(15 * 60)));
            }
            if code == FAKE_OWN_SIGN_IN_CODE {
                return Ok(DeviceSignInRequest {
                    device_name: Some("This computer".into()),
                    client_name: "spoolframe-linux".into(),
                    requested_at: unix_now(),
                    platform: Platform::Linux,
                    remember: false,
                    requested_from: Some(spoolframe_client_sdk::DeviceAddress {
                        address: "127.0.0.1".into(),
                        network: spoolframe_client_sdk::AddressNetwork::Loopback,
                    }),
                });
            }
            match sign_ins.pending.get(code) {
                Some(request) => Ok(request.clone()),
                None => {
                    sign_ins.wrong += 1;
                    Err(BackendError::NotFound(format!("sign-in code {code}")))
                }
            }
        })
    }

    fn approve_device_sign_in<'a>(
        &'a self,
        code: &'a str,
        remember: bool,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if code == FAKE_OWN_SIGN_IN_CODE {
                return Err(BackendError::InvalidInput(
                    "this device cannot approve its own sign-in".into(),
                ));
            }
            if self.lock_passkeys().stale {
                return Err(BackendError::RecentSignInRequired);
            }
            let mut sign_ins = self.sign_ins.lock().expect("fake state is never poisoned");
            sign_ins
                .pending
                .remove(code)
                .ok_or_else(|| BackendError::NotFound(format!("sign-in code {code}")))?;
            sign_ins.approved.push((code.to_owned(), remember));
            Ok(())
        })
    }

    fn deny_device_sign_in<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let mut sign_ins = self.sign_ins.lock().expect("fake state is never poisoned");
            sign_ins
                .pending
                .remove(code)
                .map(|_| ())
                .ok_or_else(|| BackendError::NotFound(format!("sign-in code {code}")))
        })
    }
}

impl FakeSession {
    /// The codes approved from this session, each with whether the device
    /// was remembered.
    pub fn approvals(&self) -> Vec<(String, bool)> {
        self.sign_ins
            .lock()
            .expect("fake state is never poisoned")
            .approved
            .clone()
    }

    fn lock_passkeys(&self) -> std::sync::MutexGuard<'_, FakeAuth> {
        self.passkeys.lock().expect("fake state is never poisoned")
    }

    /// A guest has no account here to tie (P7-06), as the server refuses.
    fn not_a_guest(&self) -> Result<(), BackendError> {
        if self.username == FAKE_GUEST {
            return Err(BackendError::Other(
                "not for a guest from another identity".into(),
            ));
        }
        Ok(())
    }

    fn lock_overrides(&self) -> std::sync::MutexGuard<'_, HashMap<MediaId, TrackOverride>> {
        self.overrides.lock().expect("fake state is never poisoned")
    }

    /// The store, with this user's Liked and All made if they are new.
    fn lock_playlists(&self) -> std::sync::MutexGuard<'_, playlists::Store> {
        let mut store = self.playlists.lock().expect("fake state is never poisoned");
        store.builtins_for(&self.username);
        store
    }

    fn lock_subtitles(&self) -> std::sync::MutexGuard<'_, subtitles::Store> {
        self.subtitles.lock().expect("fake state is never poisoned")
    }

    /// Who asks, by the display name `name` (see [`display_name`]).
    fn asker<'a>(&'a self, name: &'a str) -> subtitles::Asker<'a> {
        subtitles::Asker {
            username: &self.username,
            display_name: name,
            admin: self.is_admin(),
        }
    }

    /// Refuses a subtitle search call without the feature, unsent.
    fn subtitle_search_listed(&self) -> Result<(), BackendError> {
        match self.features.subtitle_search {
            true => Ok(()),
            false => Err(BackendError::Unsupported(
                spoolframe_protocol::features::SUBTITLE_SEARCH.into(),
            )),
        }
    }

    /// An item's tracks, for an edit's add or a like: a track itself, an
    /// album or artist its tracks, in order.
    fn tracks_for(&self, item: &MediaId) -> Result<Vec<Item>, BackendError> {
        let leaves = self.leaves(std::slice::from_ref(item));
        match leaves.is_empty() {
            true => Err(BackendError::NotFound(item.to_string())),
            false => Ok(leaves),
        }
    }

    fn viewer(&self) -> playlists::Viewer<'_> {
        playlists::Viewer {
            username: &self.username,
            admin: self.is_admin(),
        }
    }

    /// What `ids` add to a playlist, in order: a movie, episode or track
    /// itself; a season's episodes, each of a show's seasons' in turn; an
    /// album's tracks, each of an artist's albums' in turn.
    fn leaves(&self, ids: &[MediaId]) -> Vec<Item> {
        let mut leaves = Vec::new();
        for id in ids {
            let Some(item) = self.catalog.items.get(id) else {
                continue;
            };
            match item.id().kind() {
                MediaKind::Movie | MediaKind::Episode | MediaKind::Track => {
                    leaves.push(item.clone());
                }
                _ => {
                    let children = self.catalog.children.get(id).cloned().unwrap_or_default();
                    leaves.extend(self.leaves(&children));
                }
            }
        }
        leaves
    }

    /// An entry to play now, with the user's watch state on its item and
    /// the markers every playback has.
    fn playlist_next(&self, entry: EntryId, item: &MediaId) -> Option<PlaylistNext> {
        let found = self.catalog.items.get(item)?;
        Some(PlaylistNext {
            entry,
            item: self.with_state(found, &self.watch()),
            markers: self.serving.markers.clone(),
            liked: self.lock_playlists().liked(&self.username).contains(item),
        })
    }
}

/// The servers and users the mockups show.
pub struct FakeClient {
    /// The identity side, in memory with one software key (P7-08).
    identities: crate::identity::Identities,
    catalog: Arc<Catalog>,
    serving: Serving,
    servers: Arc<Mutex<Vec<Server>>>,
    /// Accounts each server has: (server, username, display name).
    accounts: Vec<(ServerKey, &'static str, &'static str)>,
    /// Who is signed in on this device, in sign-in order.
    signed_in: Arc<Mutex<Vec<Profile>>>,
    /// One session per profile, kept so watch state survives switching.
    sessions: Mutex<HashMap<(ServerKey, String), Arc<FakeSession>>>,
    failed_sign_ins: Mutex<(u32, Option<Instant>)>,
    connections: Mutex<HashMap<ServerKey, watch::Sender<ConnectionState>>>,
    /// The fake has no LAN: its list changes only when an address is added.
    servers_changed: Arc<watch::Sender<()>>,
    /// Accounts made on Cabin from an invite (P6-01), and whether
    /// [`INVITE_CODE`]'s one use is spent.
    joined: Mutex<(Vec<String>, bool)>,
    gone: Arc<std::sync::atomic::AtomicBool>,
    /// The profile that used the connection last (P2-08).
    active: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Sign-ins this device asked a phone to approve (P3-05, P3-07).
    phone_sign_ins: Mutex<PhoneSignIns>,
    /// Bumped by [`FakeClient::sign_out_elsewhere`].
    sign_outs: watch::Sender<u64>,
    passkeys: Arc<Mutex<FakeAuth>>,
    /// Every user's playlists (PX-40), as the server keeps them.
    playlists: Arc<Mutex<playlists::Store>>,
    /// The server's subtitle sites and downloads (PX-02).
    subtitles: Arc<Mutex<subtitles::Store>>,
}

/// How the fake answers a sign-in a phone approves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneAnswer {
    /// Approved by this account's user, remembering the device if it
    /// asked.
    Approve(&'static str),
    /// Approved, with "This device will be remembered" unticked.
    ApproveForgetting(&'static str),
    Deny,
    Expire,
    /// Never: the phone is still deciding.
    Wait,
}

struct PhoneSignIns {
    answer: PhoneAnswer,
    /// Whether the server has a public web address for the phone to open.
    web: bool,
    /// Begun and not yet answered: the server holds at most three.
    pending: u32,
}

/// The code a phone sign-in shows (P3-07's alphabet).
pub const FAKE_PHONE_CODE: &str = "BCDFGHJK";

/// A fake key's fingerprint, made the real way from its bytes.
fn fingerprint(key: &ServerKey) -> String {
    let mut bytes = [0u8; 32];
    for (slot, byte) in bytes.iter_mut().zip(key.0.bytes()) {
        *slot = byte;
    }
    spoolframe_protocol::fingerprint::fingerprint(&bytes)
}

fn home_media() -> ServerKey {
    ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into())
}

/// The server the fake's invites are for (P6-01): reachable, not on this
/// network, not saved, and with a key an invite link can name.
fn cabin() -> ServerKey {
    ServerKey("c4b1".repeat(16))
}

/// An invite code of the fake's that makes one account, then is used up.
pub const INVITE_CODE: &str = "QW7K2MFPXR4T";
/// An invite code of the fake's that has expired.
pub const EXPIRED_INVITE_CODE: &str = "EXPDEXPDEXPD";
/// An invite code of the fake's whose every use is spent.
pub const USED_INVITE_CODE: &str = "SPNTSPNTSPNT";

fn basement() -> ServerKey {
    ServerKey("9fm0x2kq7wv4hj3n8spe1ac21".into())
}

impl FakeClient {
    /// `username`'s profile on the fake's connected server, Home Media.
    pub fn profile(username: &str) -> Profile {
        Profile {
            server: home_media(),
            username: username.into(),
            display_name: username.into(),
            locked: false,
            sign_in: Default::default(),
            trust: None,
        }
    }

    /// `media` is the file every playback reads; without one, starting
    /// playback fails with a message saying so.
    pub fn new(media: Option<PathBuf>) -> Self {
        let profile = |username: &str, display_name: &str| Profile {
            server: home_media(),
            username: username.into(),
            display_name: display_name.into(),
            locked: false,
            sign_in: Default::default(),
            trust: None,
        };
        FakeClient {
            identities: crate::identity::in_memory(),
            catalog: Arc::new(Catalog::new()),
            serving: Serving {
                file: media,
                read_limit: MAX_READ,
                converting: None,
                markers: Vec::new(),
                reading_subtitle: None,
                not_set_up: false,
                web: true,
                previews: PreviewsAnswer::None,
                stopped: false,
                signed_out: false,
                handle_idle: HANDLE_IDLE,
                link_rate: None,
                expire_at: None,
            },
            gone: Arc::default(),
            active: Arc::default(),
            phone_sign_ins: Mutex::new(PhoneSignIns {
                answer: PhoneAnswer::Approve("andrey"),
                web: true,
                pending: 0,
            }),
            servers: Arc::new(Mutex::new(vec![
                Server {
                    key: home_media(),
                    name: "Home Media".into(),
                    addresses: vec!["192.168.1.20".into()],
                    pinned: true,
                    fingerprint: fingerprint(&home_media()),
                    verified: true,
                    on_network: true,
                    verified_at: None,
                    last_connected_at: None,
                    server_id: Some(fake_server_id().as_str().to_owned()),
                    advertised: vec!["192.168.1.20:7430".into(), "10.10.0.6:7430".into()],
                    relays: Vec::new(),
                    advertised_at: None,
                    use_relays: None,
                    relays_edited: false,
                },
                Server {
                    key: basement(),
                    name: "Basement".into(),
                    addresses: vec!["192.168.1.34".into()],
                    pinned: false,
                    fingerprint: fingerprint(&basement()),
                    verified: false,
                    on_network: true,
                    verified_at: None,
                    last_connected_at: None,
                    server_id: None,
                    advertised: Vec::new(),
                    relays: Vec::new(),
                    advertised_at: None,
                    use_relays: None,
                    relays_edited: false,
                },
            ])),
            accounts: vec![
                (home_media(), "andrey", "Andrey"),
                (home_media(), "alice", "Alice"),
                (home_media(), "steve", "Steve"),
                (cabin(), "alice", "Alice"),
            ],
            signed_in: Arc::new(Mutex::new(vec![
                profile("andrey", "Andrey"),
                profile("alice", "Alice"),
            ])),
            sessions: Mutex::new(HashMap::new()),
            // Andrey made one in the web UI, and one in an app before each
            // server had its own relying party (PX-14): both listed, no
            // use in an app.
            passkeys: Arc::new(Mutex::new(FakeAuth {
                keys: vec![
                    FakePasskey {
                        username: "andrey".into(),
                        credential_id: vec![1, 2, 3, 4],
                        name: "Firefox on nimble".into(),
                        created_at: 1_790_000_000,
                        last_used_at: Some(1_790_500_000),
                        rp_id: "media.example.org".into(),
                    },
                    FakePasskey {
                        username: "andrey".into(),
                        credential_id: vec![5, 6, 7, 8],
                        name: "Old YubiKey".into(),
                        created_at: 1_789_000_000,
                        last_used_at: None,
                        rp_id: spoolframe_protocol::LEGACY_APP_RP_ID.into(),
                    },
                ],
                stale: false,
                by_passkey: Default::default(),
                invites: Vec::new(),
                one_time: Default::default(),
                must_change: Default::default(),
                trusted: HashMap::new(),
                tv_untrusted: false,
                signed_out: Default::default(),
                ties: HashMap::new(),
                guest: None,
            })),
            failed_sign_ins: Mutex::new((0, None)),
            connections: Mutex::new(HashMap::new()),
            sign_outs: watch::Sender::new(0),
            servers_changed: Arc::new(watch::Sender::new(())),
            joined: Mutex::default(),
            playlists: Arc::new(Mutex::new(seeded_playlists())),
            subtitles: Arc::default(),
        }
    }

    /// The server has no playlists (PX-40): an older one, without the
    /// feature, and so without Liked and sharing, which need it.
    pub fn without_playlists(self) -> Self {
        let features = self.lock_playlists().features;
        self.with_features(Features {
            playlists: false,
            ..features
        })
    }

    /// The session has only these features, as when the server or the
    /// app lists fewer: Liked, sharing and covers go with playlists, as
    /// the SDK has it.
    pub fn with_features(self, features: Features) -> Self {
        self.lock_playlists().features = Features {
            liked: features.liked && features.playlists,
            playlist_sharing: features.playlist_sharing && features.playlists,
            playlist_art: features.playlist_art && features.playlists,
            ..features
        };
        self
    }

    /// The server's subtitle sites (PX-02) answer as `sites` says.
    pub fn with_subtitle_sites(self, sites: Sites) -> Self {
        self.subtitles
            .lock()
            .expect("fake state is never poisoned")
            .sites = sites;
        self
    }

    /// `item`'s file can no longer be read (a drive offline): it stays in
    /// playlists, marked unavailable, and is skipped when playing.
    pub fn make_unavailable(&self, item: &MediaId) {
        self.lock_playlists().unavailable.insert(item.clone());
    }

    fn lock_playlists(&self) -> std::sync::MutexGuard<'_, playlists::Store> {
        self.playlists.lock().expect("fake state is never poisoned")
    }

    /// The progress reports `profile`'s session has had, in order.
    pub fn progress_reports(&self, profile: &Profile) -> Vec<(MediaId, Duration)> {
        self.sessions
            .lock()
            .expect("fake state is never poisoned")
            .get(&(profile.server.clone(), profile.username.clone()))
            .map(|s| {
                s.reports
                    .lock()
                    .expect("fake state is never poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    /// The server signs this device out, as when its password changes on
    /// another device: every profile's session ends, and the watch fires.
    pub fn sign_out_elsewhere(&self) {
        for profile in self
            .signed_in
            .lock()
            .expect("fake state is never poisoned")
            .iter()
        {
            self.fake_session(profile).pin_state().revoked = true;
        }
        self.sign_outs.send_modify(|n| *n += 1);
    }

    /// An admin resets `username`'s password to [`FAKE_ONE_TIME_PASSWORD`]:
    /// signing in with it, they must choose a new one.
    pub fn reset_password(&self, username: &str) {
        self.lock_auth().one_time.insert(username.to_owned());
    }

    /// Every sign-in on this device is now older than the 10 minutes
    /// adding or removing a passkey allows.
    pub fn age_sign_ins(&self) {
        self.passkeys
            .lock()
            .expect("fake state is never poisoned")
            .stale = true;
    }

    /// The device stops dialling `server` for `why`, as the SDK does on
    /// an error that dialling again does not fix (an app too old for the
    /// server, a server that is not the one saved).
    pub fn stop_dialling(&self, server: &ServerKey, why: BackendError) {
        self.connections
            .lock()
            .expect("fake state is never poisoned")
            .entry(server.clone())
            .or_insert_with(|| watch::Sender::new(ConnectionState::Closed))
            .send_replace(ConnectionState::Stopped(why));
    }

    /// An invite link to Cabin (P6-01) with `code`, or one that only finds
    /// the server; any code but the fake's own is wrong.
    pub fn invite_link(code: Option<&str>) -> String {
        let in_six_days = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + 6 * 24 * 60 * 60;
        spoolframe_protocol::invite::Invite {
            server: cabin().0,
            name: Some("Cabin".into()),
            addrs: vec!["203.0.113.24:41641".parse().expect("an address")],
            relays: vec!["https://relay.cabin.example".into()],
            code: code.map(str::to_owned),
            expires_at: Some(in_six_days),
            role: Some(spoolframe_types::Role::Consumer),
            kind: spoolframe_protocol::invite::InviteLinkKind::CreateUser,
            lookup: None,
        }
        .link()
    }

    /// Cabin, as an invite finds it: saved if it was.
    fn invited(
        &self,
        link: &str,
    ) -> Result<(spoolframe_protocol::invite::Invite, Server), BackendError> {
        let invite =
            spoolframe_protocol::invite::parse_link(link).ok_or(BackendError::NotAnInvite)?;
        if invite.server != cabin().0 {
            return Err(BackendError::Unreachable(
                invite.name.unwrap_or_else(|| "the server".into()),
            ));
        }
        let saved = self
            .servers
            .lock()
            .expect("fake state is never poisoned")
            .iter()
            .find(|s| s.key == cabin())
            .cloned();
        let server = saved.unwrap_or_else(|| Server {
            key: cabin(),
            name: "Cabin".into(),
            addresses: invite.addrs.iter().map(ToString::to_string).collect(),
            pinned: false,
            fingerprint: fingerprint(&cabin()),
            verified: false,
            on_network: false,
            verified_at: None,
            last_connected_at: None,
            server_id: Some("k7f3q2m4x6pq5z2a".into()),
            advertised: Vec::new(),
            relays: invite.relays.clone(),
            advertised_at: None,
            use_relays: None,
            relays_edited: false,
        });
        Ok((invite, server))
    }

    /// Cabin saved, verified: the invite came from its admin.
    fn save_invited(&self, mut server: Server) -> Server {
        server.pinned = true;
        server.verified = true;
        // As the SDK saves one: through the relays the link named.
        if !server.relays.is_empty() {
            server.use_relays = Some(true);
        }
        let mut servers = self.servers.lock().expect("fake state is never poisoned");
        servers.retain(|s| s.key != server.key);
        servers.push(server.clone());
        drop(servers);
        self.servers_changed.send_replace(());
        server
    }

    /// Changes saved server `key` as `edit` says; NotPinned for one not
    /// saved, as the SDK refuses it.
    fn edit_saved(
        &self,
        key: &ServerKey,
        edit: impl FnOnce(&mut Server),
    ) -> Result<(), BackendError> {
        let mut servers = self.servers.lock().expect("fake state is never poisoned");
        let server = servers
            .iter_mut()
            .find(|s| &s.key == key && s.pinned)
            .ok_or_else(|| BackendError::NotPinned(key.0.clone()))?;
        edit(server);
        drop(servers);
        self.servers_changed.send_replace(());
        Ok(())
    }

    /// The server stops answering, as when it is shut down mid-play: every
    /// read fails as unreachable, and connections say closed.
    pub fn go_away(&self) {
        self.gone.store(true, std::sync::atomic::Ordering::SeqCst);
        for state in self
            .connections
            .lock()
            .expect("fake state is never poisoned")
            .values()
        {
            state.send_replace(ConnectionState::Closed);
        }
    }

    /// The device sign-ins `username` approved on Home Media, each with
    /// whether the device was remembered.
    pub fn approvals_by(&self, username: &str) -> Vec<(String, bool)> {
        self.fake_session(&Self::profile(username)).approvals()
    }

    fn saved(&self) -> Saved {
        Saved {
            servers: self.servers.clone(),
            signed_in: self.signed_in.clone(),
            changed: self.servers_changed.clone(),
        }
    }

    /// The session behind `profile`, made on first use and kept, so watch
    /// state and the PIN's lock survive switching.
    fn fake_session(&self, profile: &Profile) -> Arc<FakeSession> {
        let mut sessions = self.sessions.lock().expect("fake state is never poisoned");
        sessions
            .entry((profile.server.clone(), profile.username.clone()))
            .or_insert_with(|| {
                Arc::new(FakeSession::new(
                    self.catalog.clone(),
                    &profile.username,
                    self.serving.clone(),
                    self.gone.clone(),
                    self.active.clone(),
                    self.passkeys.clone(),
                    (
                        profile.server.clone(),
                        self.saved(),
                        self.playlists.clone(),
                        self.subtitles.clone(),
                    ),
                ))
            })
            .clone()
    }

    /// Every playback's session ends under it, as when the password
    /// changes on another device: its reads ask to sign in again.
    pub fn with_sessions_ending(mut self) -> Self {
        self.serving.signed_out = true;
        self
    }

    /// The server's admin stops every playback (P3-15): its reads are
    /// refused with PlaybackStopped.
    pub fn with_playbacks_stopped(mut self) -> Self {
        self.serving.stopped = true;
        self
    }

    /// A playback handle unused for `idle` expires, instead of after the
    /// server's 5 minutes, so tests can wait past it.
    pub fn with_handle_idle(mut self, idle: Duration) -> Self {
        self.serving.handle_idle = idle;
        self
    }

    /// The link has been carrying `bits_per_second` (P6-06).
    pub fn with_link_rate(mut self, bits_per_second: u64) -> Self {
        self.serving.link_rate = Some(bits_per_second);
        self
    }

    /// The server ends the first playback once a chunk read reaches
    /// `offset`, as one that idled out, so tests can see it resumed.
    pub fn expiring_at(mut self, offset: u64) -> Self {
        self.serving.expire_at = Some(offset);
        self
    }

    /// How many playbacks `profile`'s session has started: one more for
    /// each the client started again.
    pub fn started_playbacks(&self, profile: &Profile) -> u64 {
        self.sessions
            .lock()
            .expect("fake state is never poisoned")
            .get(&(profile.server.clone(), profile.username.clone()))
            .map_or(0, |s| s.next_handle.load(Ordering::Relaxed) - 1)
    }

    /// The display each of `profile`'s playbacks was started for, in order.
    pub fn displays_asked(
        &self,
        profile: &Profile,
    ) -> Vec<Option<spoolframe_types::DisplayCapabilities>> {
        self.sessions
            .lock()
            .expect("fake state is never poisoned")
            .get(&(profile.server.clone(), profile.username.clone()))
            .map_or_else(Vec::new, |s| {
                s.displays
                    .lock()
                    .expect("fake state is never poisoned")
                    .clone()
            })
    }

    /// Reads answer at most `limit` bytes, like a server that sends less
    /// than asked: clients must ask again from where a read stopped.
    pub fn with_read_limit(mut self, limit: u64) -> Self {
        self.serving.read_limit = limit;
        self
    }

    /// Every playback is converted (P2-04): a `Converted` progressive plan
    /// whose output, the media file, grows by `bytes` with each chunk
    /// read, as an encode would.
    pub fn converting(mut self, bytes: u64) -> Self {
        self.serving.converting = Some(bytes);
        self
    }

    /// A phone sign-in (P3-05) is answered so; approved by andrey unless
    /// told otherwise.
    pub fn answering_phone_sign_in(self, answer: PhoneAnswer) -> Self {
        self.phone_sign_ins().answer = answer;
        self
    }

    /// The server has no public web address: a phone sign-in has no page
    /// for a phone to open, only a code for a Spoolframe app.
    pub fn without_web_address(mut self) -> Self {
        self.phone_sign_ins().web = false;
        self.serving.web = false;
        self
    }

    /// Items' preview frames (P3-13) are answered so; none unless told.
    pub fn with_previews(mut self, answer: PreviewsAnswer) -> Self {
        self.serving.previews = answer;
        self
    }

    /// The server's admin has not finished setup (P3-10): signing in works,
    /// and the catalog answers [`BackendError::NotSetUp`].
    pub fn not_set_up(mut self) -> Self {
        self.serving.not_set_up = true;
        self
    }

    /// Every converted playback's subtitle is read as text by the server
    /// (P2-07), and not ready the first `waits` times it is asked for.
    pub fn reading_subtitle(mut self, waits: u32) -> Self {
        self.serving.reading_subtitle = Some(waits);
        self
    }

    /// Every playback starts with `markers` (P2-10), so tests can see the
    /// player offer Skip Intro and the rest.
    pub fn with_markers(mut self, markers: Vec<Marker>) -> Self {
        self.serving.markers = markers;
        self
    }

    /// How many playbacks `profile`'s session holds open: a converted one
    /// is an encode running on the server.
    pub fn open_playbacks(&self, profile: &Profile) -> usize {
        self.sessions
            .lock()
            .expect("fake state is never poisoned")
            .get(&(profile.server.clone(), profile.username.clone()))
            .map_or(0, |s| {
                s.playbacks
                    .lock()
                    .expect("fake state is never poisoned")
                    .len()
            })
    }

    /// How many of `profile`'s playbacks were used within the idle limit:
    /// the ones a read or report would still find.
    pub fn live_playbacks(&self, profile: &Profile) -> usize {
        let idle = self.serving.handle_idle;
        self.sessions
            .lock()
            .expect("fake state is never poisoned")
            .get(&(profile.server.clone(), profile.username.clone()))
            .map_or(0, |s| {
                s.playbacks
                    .lock()
                    .expect("fake state is never poisoned")
                    .values()
                    .filter(|p| p.last_used.elapsed() <= idle)
                    .count()
            })
    }

    /// Basement shows on the network but does not answer: the fixture for
    /// an unreachable server. So does any server added by hand, which the
    /// fake has no accounts for.
    fn reach(&self, key: &ServerKey) -> Result<Server, BackendError> {
        let servers = self.servers.lock().expect("fake state is never poisoned");
        let server = servers
            .iter()
            .find(|s| &s.key == key)
            .ok_or_else(|| BackendError::NotFound(key.short()))?;
        if server.key != home_media() && server.key != cabin() {
            return Err(BackendError::Unreachable(server.name.clone()));
        }
        Ok(server.clone())
    }

    /// Like the SDK: a server must be pinned before signing in to it.
    fn pinned(&self, key: &ServerKey) -> Result<Server, BackendError> {
        let server = self.reach(key)?;
        if !server.pinned {
            return Err(BackendError::NotPinned(key.0.clone()));
        }
        Ok(server)
    }

    fn sign_in_now(
        &self,
        server: &ServerKey,
        username: &str,
        password: &str,
    ) -> Result<Profile, BackendError> {
        self.pinned(server)?;
        let mut failed = self
            .failed_sign_ins
            .lock()
            .expect("fake state is never poisoned");
        if let (_, Some(until)) = *failed {
            if Instant::now() < until {
                return Err(BackendError::RateLimited(until - Instant::now()));
            }
            *failed = (0, None);
        }
        let one_time =
            password == FAKE_ONE_TIME_PASSWORD && self.lock_auth().one_time.contains(username);
        let Some((_, username, display_name)) = self
            .accounts
            .iter()
            .find(|(s, u, _)| s == server && *u == username)
            .filter(|_| password == FAKE_PASSWORD || one_time)
        else {
            failed.0 += 1;
            if failed.0 >= SIGN_IN_ATTEMPTS {
                failed.1 = Some(Instant::now() + SIGN_IN_BACKOFF);
            }
            return Err(BackendError::WrongCredentials);
        };
        *failed = (0, None);
        drop(failed);
        if one_time {
            self.lock_auth().must_change.insert((*username).to_owned());
        }
        Ok(self.signed_in_as(server, username, display_name))
    }

    /// `username` signed in from this device, as a password or a phone's
    /// approval does.
    fn lock_auth(&self) -> std::sync::MutexGuard<'_, FakeAuth> {
        self.passkeys.lock().expect("fake state is never poisoned")
    }

    /// This device is now trusted for `username` (P4-01), not as the
    /// auto-login profile.
    fn trust(&self, username: &str) {
        self.lock_auth()
            .trusted
            .entry(username.to_owned())
            .or_insert(false);
    }

    /// The remembered profiles, as TrustedProfiles answers them.
    fn remembered(&self, server: &ServerKey) -> Vec<TrustedProfile> {
        let trusted = self.lock_auth().trusted.clone();
        let mut remembered: Vec<TrustedProfile> = self
            .accounts
            .iter()
            .filter(|(s, u, _)| s == server && trusted.contains_key(*u))
            .map(|(_, username, display_name)| {
                let profile = FakeClient::profile(username);
                TrustedProfile {
                    user_id: (*username).to_owned(),
                    username: (*username).to_owned(),
                    display_name: (*display_name).to_owned(),
                    pin_required: self
                        .fake_session(&profile)
                        .pin_state()
                        .required
                        .contains(THIS_DEVICE),
                    auto_login: trusted[*username],
                }
            })
            .collect();
        remembered.sort_by(|a, b| a.username.cmp(&b.username));
        remembered
    }

    /// A remembered profile opened without a password: a new session,
    /// locked if the PIN is required here, and no recent sign-in.
    fn resumed_as(&self, server: &ServerKey, username: &str, display_name: &str) -> Profile {
        let profile = Profile {
            server: server.clone(),
            username: username.into(),
            display_name: display_name.into(),
            locked: false,
            sign_in: Default::default(),
            trust: None,
        };
        let mut signed_in = self.signed_in.lock().expect("fake state is never poisoned");
        if !signed_in.contains(&profile) {
            signed_in.push(profile.clone());
        }
        drop(signed_in);
        let session = self.fake_session(&profile);
        let mut pin = session.pin_state();
        pin.revoked = false;
        let locked = pin.required.contains(THIS_DEVICE) && pin.pin.is_some();
        pin.locked = locked;
        Profile { locked, ..profile }
    }

    fn signed_in_as(&self, server: &ServerKey, username: &str, display_name: &str) -> Profile {
        // A sign-in is fresh: adding and removing passkeys works again.
        let mut auth = self.lock_auth();
        auth.stale = false;
        auth.signed_out.remove(username);
        drop(auth);
        let profile = Profile {
            server: server.clone(),
            username: (*username).into(),
            display_name: (*display_name).into(),
            locked: false,
            sign_in: Default::default(),
            trust: None,
        };
        let mut signed_in = self.signed_in.lock().expect("fake state is never poisoned");
        if !signed_in.contains(&profile) {
            signed_in.push(profile.clone());
        }
        drop(signed_in);
        // A sign-in with the password is a new session, unlocked: the
        // password proves more than the PIN would.
        let session = self.fake_session(&profile);
        let mut pin = session.pin_state();
        pin.revoked = false;
        pin.wrong = 0;
        pin.locked = false;
        drop(pin);
        self.active
            .lock()
            .expect("fake state is never poisoned")
            .insert(profile.username.clone());
        profile
    }

    fn phone_sign_ins(&self) -> std::sync::MutexGuard<'_, PhoneSignIns> {
        self.phone_sign_ins
            .lock()
            .expect("fake state is never poisoned")
    }
}

impl Client for FakeClient {
    fn servers(&self) -> BoxFuture<'_, Result<Vec<Server>, BackendError>> {
        Box::pin(async move {
            Ok(self
                .servers
                .lock()
                .expect("fake state is never poisoned")
                .clone())
        })
    }

    fn add_address<'a>(
        &'a self,
        server: &'a ServerKey,
        address: &'a str,
    ) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let mut servers = self.servers.lock().expect("fake state is never poisoned");
            let entry = match servers.iter_mut().find(|s| &s.key == server) {
                Some(entry) => entry,
                None => {
                    servers.push(Server {
                        key: server.clone(),
                        name: format!("Server {}", server.short()),
                        addresses: Vec::new(),
                        pinned: false,
                        fingerprint: fingerprint(server),
                        verified: false,
                        on_network: false,
                        verified_at: None,
                        last_connected_at: None,
                        server_id: None,
                        advertised: Vec::new(),
                        relays: Vec::new(),
                        advertised_at: None,
                        use_relays: None,
                        relays_edited: false,
                    });
                    servers.last_mut().expect("just pushed")
                }
            };
            if !entry.addresses.iter().any(|a| a == address) {
                entry.addresses.push(address.into());
                self.servers_changed.send_replace(());
            }
            Ok(entry.clone())
        })
    }

    fn read_invite<'a>(&'a self, link: &'a str) -> BoxFuture<'a, Result<Invite, BackendError>> {
        Box::pin(async move {
            let invite =
                spoolframe_protocol::invite::parse_link(link).ok_or(BackendError::NotAnInvite)?;
            let key = ServerKey(invite.server.clone());
            let saved = self
                .servers
                .lock()
                .expect("fake state is never poisoned")
                .iter()
                .any(|s| s.key == key && s.pinned);
            Ok(Invite {
                fingerprint: fingerprint(&key),
                key,
                name: invite.name,
                addresses: invite.addrs.iter().map(ToString::to_string).collect(),
                relays: invite.relays,
                has_code: invite.code.is_some(),
                expires_at: invite
                    .expires_at
                    .map(|secs| std::time::UNIX_EPOCH + Duration::from_secs(secs)),
                role: invite.role,
                saved,
            })
        })
    }

    fn connect_invite<'a>(&'a self, link: &'a str) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move { self.invited(link).map(|(_, server)| server) })
    }

    fn save_invite_server<'a>(
        &'a self,
        link: &'a str,
    ) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let (_, server) = self.invited(link)?;
            Ok(self.save_invited(server))
        })
    }

    fn identities(&self) -> crate::identity::Identities {
        self.identities.clone()
    }

    fn reachability<'a>(
        &'a self,
        key: &'a ServerKey,
    ) -> BoxFuture<'a, Result<crate::backend::Reachability, BackendError>> {
        Box::pin(async move {
            use crate::backend::{
                DialReport, DialStage, KnownPath, PathKind, Reachability, SavedAddr,
            };
            let server = self
                .servers
                .lock()
                .expect("fake state is never poisoned")
                .iter()
                .find(|s| &s.key == key)
                .cloned()
                .ok_or_else(|| BackendError::NotFound(key.0.clone()))?;
            let gone = self.gone.load(std::sync::atomic::Ordering::SeqCst);
            let now = unix_now() as u64;
            Ok(Reachability {
                endpoint_id: server.key.0.clone(),
                name: Some(server.name.clone()),
                saved: server.pinned,
                addrs: server
                    .addresses
                    .iter()
                    .map(|addr| SavedAddr {
                        addr: addr.clone(),
                        reached: !gone,
                        last_seen: now - 3600,
                    })
                    .collect(),
                advertised_addrs: server.advertised.clone(),
                advertised_at: None,
                relays: server.relays.clone(),
                relays_edited: server.relays_edited,
                lookup: None,
                use_relays: server.use_relays,
                relays_effective: server.use_relays.unwrap_or(true),
                heard_on_lan: if server.on_network {
                    server.addresses.clone()
                } else {
                    Vec::new()
                },
                hinted_addrs: Vec::new(),
                invite_relays: Vec::new(),
                lookups: vec!["mdns".into()],
                lookup_results: Vec::new(),
                last_dial: Some(DialReport {
                    at: now - 10,
                    took_ms: if gone { 10_000 } else { 40 },
                    connected: !gone,
                    stage: gone.then_some(DialStage::Timeout),
                    error: gone.then(|| "timed out".to_owned()),
                    dialled: server.addresses.clone(),
                    known: server
                        .addresses
                        .iter()
                        .map(|addr| KnownPath {
                            kind: PathKind::Lan,
                            addr: addr.clone(),
                            active: !gone,
                        })
                        .collect(),
                }),
                last_connected_at: Some(now - if gone { 3 * 24 * 3600 } else { 10 }),
                live: Vec::new(),
            })
        })
    }

    fn set_server_relays<'a>(
        &'a self,
        key: &'a ServerKey,
        relays: Option<bool>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move { self.edit_saved(key, |server| server.use_relays = relays) })
    }

    fn set_server_relay_urls<'a>(
        &'a self,
        key: &'a ServerKey,
        relays: Vec<String>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            if relays
                .iter()
                .any(|r| !(r.starts_with("https://") || r.starts_with("http://")))
            {
                return Err(BackendError::InvalidInput(
                    "a relay is an http:// or https:// URL".into(),
                ));
            }
            self.edit_saved(key, |server| {
                server.relays_edited = !relays.is_empty();
                server.relays = relays;
            })
        })
    }

    fn redeem_invite<'a>(
        &'a self,
        link: &'a str,
        username: &'a str,
        display_name: Option<&'a str>,
        password: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let (invite, server) = self.invited(link)?;
            let Some(code) = invite.code else {
                return Err(BackendError::InvalidInput(
                    "this invite has no code: it only finds the server; sign in instead".into(),
                ));
            };
            let mut joined = self.joined.lock().expect("fake state is never poisoned");
            match code.as_str() {
                INVITE_CODE if joined.1 => return Err(BackendError::InviteExhausted),
                INVITE_CODE => {}
                EXPIRED_INVITE_CODE => return Err(BackendError::InviteExpired),
                USED_INVITE_CODE => return Err(BackendError::InviteExhausted),
                _ => return Err(BackendError::InviteInvalid),
            }
            let taken = self
                .accounts
                .iter()
                .any(|(s, u, _)| *s == server.key && *u == username)
                || joined.0.iter().any(|u| u == username);
            if taken {
                return Err(BackendError::UsernameTaken);
            }
            if password.chars().count() < 8 {
                return Err(BackendError::InvalidInput(
                    "the password needs at least 8 characters".into(),
                ));
            }
            joined.0.push(username.to_owned());
            joined.1 = true;
            drop(joined);
            let server = self.save_invited(server);
            Ok(self.signed_in_as(&server.key, username, display_name.unwrap_or(username)))
        })
    }

    fn connect<'a>(&'a self, server: &'a ServerKey) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move { self.reach(server) })
    }

    fn pin<'a>(
        &'a self,
        server: &'a ServerKey,
        verified: bool,
    ) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            self.reach(server)?;
            let mut servers = self.servers.lock().expect("fake state is never poisoned");
            let entry = servers
                .iter_mut()
                .find(|s| &s.key == server)
                .ok_or_else(|| BackendError::NotFound(server.short()))?;
            // A saved server is only ever verified, never unverified again.
            entry.verified = verified || (entry.pinned && entry.verified);
            entry.pinned = true;
            let entry = entry.clone();
            drop(servers);
            self.servers_changed.send_replace(());
            Ok(entry)
        })
    }

    fn trust_new_identity<'a>(
        &'a self,
        saved: &'a ServerKey,
        now: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Server, BackendError>> {
        Box::pin(async move {
            let mut servers = self.servers.lock().expect("fake state is never poisoned");
            let name = servers
                .iter()
                .find(|s| &s.key == saved)
                .map(|s| s.name.clone())
                .ok_or_else(|| BackendError::NotFound(saved.short()))?;
            servers.retain(|s| &s.key != saved && &s.key != now);
            let entry = Server {
                key: now.clone(),
                name,
                addresses: Vec::new(),
                pinned: true,
                fingerprint: fingerprint(now),
                verified: true,
                on_network: true,
                verified_at: None,
                last_connected_at: None,
                server_id: None,
                advertised: Vec::new(),
                relays: Vec::new(),
                advertised_at: None,
                use_relays: None,
                relays_edited: false,
            };
            servers.push(entry.clone());
            drop(servers);
            self.signed_in
                .lock()
                .expect("fake state is never poisoned")
                .retain(|p| &p.server != saved);
            self.servers_changed.send_replace(());
            Ok(entry)
        })
    }

    fn forget<'a>(&'a self, server: &'a ServerKey) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            self.saved().forget(server);
            Ok(())
        })
    }

    fn resume(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn connection_path(&self, _server: &ServerKey) -> ConnectionPath {
        ConnectionPath::Lan(([192, 168, 1, 20], 7431).into())
    }

    fn forget_profile<'a>(
        &'a self,
        server: &'a ServerKey,
        username: &'a str,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            self.signed_in
                .lock()
                .expect("fake state is never poisoned")
                .retain(|p| !(&p.server == server && p.username == username));
            Ok(())
        })
    }

    fn sign_out<'a>(&'a self, server: &'a ServerKey) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            self.signed_in
                .lock()
                .expect("fake state is never poisoned")
                .retain(|p| &p.server != server);
            self.lock_auth().trusted.clear();
            Ok(())
        })
    }

    fn profiles<'a>(
        &'a self,
        server: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<Profile>, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            let signed_in = self
                .signed_in
                .lock()
                .expect("fake state is never poisoned")
                .clone();
            // A revoked session is kept by name, as the SDK keeps it, to
            // sign in again with a password.
            let gone = self.lock_auth().signed_out.clone();
            let listed: Vec<Profile> = signed_in
                .into_iter()
                .filter(|p| &p.server == server && !gone.contains(&p.username))
                .map(|p| {
                    if self.fake_session(&p).pin_state().revoked {
                        let by_passkey = self
                            .passkeys
                            .lock()
                            .expect("fake state is never poisoned")
                            .by_passkey
                            .contains(&p.username);
                        Profile {
                            locked: false,
                            sign_in: if by_passkey {
                                ProfileSignIn::Passkey
                            } else {
                                ProfileSignIn::Password
                            },
                            ..p
                        }
                    } else {
                        Profile {
                            locked: self.fake_session(&p).would_lock(),
                            ..p
                        }
                    }
                })
                .collect();
            // The remembered ones folded in, as the SDK backend does.
            Ok(crate::sdk::with_trust(
                listed,
                self.remembered(server),
                server,
            ))
        })
    }

    fn sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        username: &'a str,
        password: &'a str,
        remember: bool,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let profile = self.sign_in_now(server, username, password)?;
            if remember {
                self.trust(username);
            }
            Ok(profile)
        })
    }

    /// The fake's identity is a guest everywhere: it signs in as
    /// "Guest", if it is on this computer.
    fn sign_in_portable<'a>(
        &'a self,
        server: &'a ServerKey,
        identity: spoolframe_protocol::portable::PortableUserId,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            let here = self
                .identities
                .current()
                .await
                .map_err(|e| BackendError::Other(e.to_string()))?
                .is_some_and(|view| view.identity.id == identity);
            if !here {
                return Err(BackendError::NotPaired);
            }
            self.lock_auth().guest = Some(identity);
            Ok(self.signed_in_as(server, FAKE_GUEST, "Guest"))
        })
    }

    fn trusted_profiles<'a>(
        &'a self,
        server: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<TrustedProfile>, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            Ok(self.remembered(server))
        })
    }

    fn resume_profile<'a>(
        &'a self,
        server: &'a ServerKey,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            if !self.lock_auth().trusted.contains_key(user_id) {
                return Err(BackendError::SignInRequired);
            }
            let (_, username, display_name) = self
                .accounts
                .iter()
                .find(|(s, u, _)| s == server && *u == user_id)
                .ok_or(BackendError::SignInRequired)?;
            let profile = self.resumed_as(server, username, display_name);
            Ok(profile)
        })
    }

    fn begin_passkey_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        username: Option<&'a str>,
    ) -> BoxFuture<'a, Result<PasskeyRequest, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            // The server lists no passkeys, with a username or without
            // (PX-10): the key offers whichever it holds for this server.
            let _ = username;
            Ok(PasskeyRequest {
                rp_id: fake_server_id().rp_id(),
                origin: fake_server_id().origin(),
                challenge: FAKE_PASSKEY_CHALLENGE.to_vec(),
                allow_credential_ids: Vec::new(),
            })
        })
    }

    fn finish_passkey_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        signed: FinishPasskeySignIn,
        remember: bool,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            let username = {
                let mut passkeys = self.passkeys.lock().expect("fake state is never poisoned");
                let key = passkeys
                    .keys
                    .iter_mut()
                    .find(|k| k.usable() && k.credential_id == signed.credential_id)
                    .ok_or(BackendError::WrongCredentials)?;
                key.last_used_at = Some(1_791_000_000);
                let username = key.username.clone();
                passkeys.by_passkey.insert(username.clone());
                username
            };
            let (_, username, display_name) = self
                .accounts
                .iter()
                .find(|(s, u, _)| s == server && *u == username)
                .ok_or(BackendError::WrongCredentials)?;
            let profile = self.signed_in_as(server, username, display_name);
            if remember {
                self.trust(username);
            }
            Ok(profile)
        })
    }

    fn begin_device_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        remember: bool,
    ) -> BoxFuture<'a, Result<DeviceSignIn, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            let mut sign_ins = self.phone_sign_ins();
            if sign_ins.pending >= 3 {
                return Err(BackendError::RateLimited(Duration::from_secs(60)));
            }
            sign_ins.pending += 1;
            let (head, tail) = FAKE_PHONE_CODE.split_at(4);
            Ok(DeviceSignIn {
                server: server.clone(),
                remember,
                display_code: format!("{head}-{tail}"),
                web_url: sign_ins.web.then(|| {
                    format!("https://media.example.org/auth/device?code={FAKE_PHONE_CODE}")
                }),
                expires_in: Duration::from_secs(300),
                request: Arc::new(()),
            })
        })
    }

    fn await_device_sign_in<'a>(
        &'a self,
        sign_in: &'a DeviceSignIn,
    ) -> BoxFuture<'a, Result<(Profile, Option<bool>), BackendError>> {
        Box::pin(async move {
            let answer = self.phone_sign_ins().answer;
            if answer == PhoneAnswer::Wait {
                return std::future::pending().await;
            }
            self.phone_sign_ins().pending -= 1;
            match answer {
                PhoneAnswer::Approve(username) | PhoneAnswer::ApproveForgetting(username) => {
                    let (_, username, display_name) = self
                        .accounts
                        .iter()
                        .find(|(s, u, _)| *s == sign_in.server && *u == username)
                        .ok_or_else(|| BackendError::NotFound(username.into()))?;
                    let profile = self.signed_in_as(&sign_in.server, username, display_name);
                    // Remembered only if it asked and the approver let it.
                    let remembered = sign_in.remember && matches!(answer, PhoneAnswer::Approve(_));
                    if remembered {
                        self.trust(username);
                    }
                    Ok((profile, Some(remembered)))
                }
                PhoneAnswer::Deny => Err(BackendError::SignInDenied),
                PhoneAnswer::Expire => Err(BackendError::SignInExpired),
                PhoneAnswer::Wait => unreachable!("waited above"),
            }
        })
    }

    fn session(&self, profile: &Profile) -> Result<Arc<dyn Session>, BackendError> {
        self.reach(&profile.server)?;
        Ok(self.fake_session(profile))
    }

    fn signed_out(&self, _server: &ServerKey) -> watch::Receiver<u64> {
        self.sign_outs.subscribe()
    }

    fn connection(&self, server: &ServerKey) -> watch::Receiver<ConnectionState> {
        let state = if self.reach(server).is_ok() {
            ConnectionState::Connected
        } else {
            ConnectionState::Closed
        };
        self.connections
            .lock()
            .expect("fake state is never poisoned")
            .entry(server.clone())
            .or_insert_with(|| watch::Sender::new(state))
            .subscribe()
    }

    fn servers_changed(&self) -> watch::Receiver<()> {
        self.servers_changed.subscribe()
    }

    fn playlist_events(&self) -> tokio::sync::broadcast::Receiver<PlaylistEvent> {
        self.lock_playlists().subscribe()
    }
}

/// The playlists the fake server starts with (PX-40), as the boards show
/// them: Andrey's own, pinned, and shared with Alice to read; his movie
/// night; Alice's road trip, which Andrey may edit; and a server-wide one.
fn seeded_playlists() -> playlists::Store {
    use playlists::share;
    let episodes = Kinds {
        episodes: true,
        ..Kinds::default()
    };
    let mut store = playlists::Store::new(&home_media().0);
    store.all_tracks = (0..9)
        .map(|i| media(MediaKind::Track, &format!("4{i}")))
        .collect();
    // The fake's accounts on Home Media, and a portable user granted it.
    let account = |username: &str| crate::backend::ShareCandidate {
        user_id: playlists::user_id(username),
        display_name: display_name(username),
        username: Some(username.to_owned()),
        kind: crate::backend::UserKind::Local,
    };
    store.people = ["andrey", "alice", "steve", "mum"]
        .map(account)
        .into_iter()
        .chain([crate::backend::ShareCandidate {
            user_id: format!("portable:{}", "7e".repeat(32)),
            display_name: "Kim".into(),
            username: None,
            kind: crate::backend::UserKind::Portable,
        }])
        .collect();
    let heists = store.seed(
        FAKE_ADMIN,
        "Andor: the heists",
        episodes,
        Visibility::Shared {
            with: vec![share("alice", false)],
        },
        &[
            media(MediaKind::Episode, "64"),
            media(MediaKind::Episode, "65"),
            media(MediaKind::Episode, "72"),
        ],
    );
    store.seed(
        FAKE_ADMIN,
        "Movie night",
        Kinds {
            movies: true,
            episodes: true,
            tracks: false,
        },
        Visibility::Personal,
        &[
            media(MediaKind::Movie, "1"),
            media(MediaKind::Movie, "3"),
            media(MediaKind::Episode, "22"),
        ],
    );
    store.seed(
        "alice",
        "Road trip",
        Kinds {
            tracks: true,
            ..Kinds::default()
        },
        Visibility::Shared {
            with: vec![share(FAKE_ADMIN, true)],
        },
        &[
            media(MediaKind::Track, "40"),
            media(MediaKind::Track, "42"),
            media(MediaKind::Track, "44"),
        ],
    );
    store.seed(
        FAKE_ADMIN,
        "New this month",
        Kinds {
            movies: true,
            ..Kinds::default()
        },
        Visibility::ServerWide,
        &[media(MediaKind::Movie, "2"), media(MediaKind::Movie, "8")],
    );
    let andrey = playlists::Viewer {
        username: FAKE_ADMIN,
        admin: true,
    };
    store.pin(&heists, 0, andrey).expect("seeded");
    store
}

/// The fake's search (P3-09), shaped as the server answers: a trailing
/// year as a filter, titles matched by a word's start, the top match left
/// out of its group, and in All the episodes under a listed show counted
/// rather than shown.
mod search {
    use std::collections::HashSet;

    use super::*;

    /// The top match's tie order: a show before its own episodes.
    const TOP_ORDER: [SearchKind; 6] = [
        SearchKind::Shows,
        SearchKind::Movies,
        SearchKind::Artists,
        SearchKind::Albums,
        SearchKind::Tracks,
        SearchKind::Episodes,
    ];

    /// Episodes shown in All, where they are quiet.
    const QUIET_EPISODES: usize = 3;

    pub fn run(catalog: &Catalog, request: &SearchRequest) -> SearchResults {
        let (text, year) = split_year(&request.query);
        let within = request
            .library
            .as_ref()
            .map(|id| library_items(catalog, id));
        let hits = |kind: SearchKind, year: Option<u16>| -> Vec<Item> {
            catalog
                .order
                .iter()
                .map(|id| &catalog.items[id])
                .filter(|item| kind_of(item) == Some(kind))
                .filter(|item| within.as_ref().is_none_or(|w| w.contains(item.id())))
                .filter(|item| matches(&text, title(item)))
                .filter(|item| year.is_none_or(|y| year_of(item).is_none_or(|it| it == y)))
                .cloned()
                .collect()
        };
        // Inside a library, as the server does since P3-18: the people in
        // one of its items.
        let people = || -> Vec<Person> {
            catalog
                .people
                .iter()
                .filter(|(p, _)| matches(&text, &p.name))
                .filter(|(_, items)| {
                    within
                        .as_ref()
                        .is_none_or(|w| items.iter().any(|i| w.contains(i)))
                })
                .map(|(p, _)| p.clone())
                .collect()
        };
        let kinds: Vec<SearchKind> = match request.kind {
            Some(kind) => vec![kind],
            None => request.scope.kinds().to_vec(),
        };
        let mut found: Vec<(SearchKind, Vec<Item>)> = kinds
            .iter()
            .filter(|k| **k != SearchKind::People)
            .map(|k| (*k, hits(*k, year)))
            .collect();

        // As the server picks: a person only on a strictly better name
        // match than the best item; ties go to the item.
        let top = match request.kind {
            Some(_) => None,
            None => {
                let item = pick_top(&found, &text);
                let person = people()
                    .into_iter()
                    .min_by_key(|p| name_rank(&p.name, &text));
                match (item, person) {
                    (Some(item), Some(person))
                        if name_rank(&person.name, &text) < name_rank(title(&item), &text) =>
                    {
                        Some(TopResult::Person(person))
                    }
                    (Some(item), _) => Some(TopResult::Item(item)),
                    (None, Some(person)) => Some(TopResult::Person(person)),
                    (None, None) => None,
                }
            }
        };
        let top_item = match &top {
            Some(TopResult::Item(item)) => Some(item.clone()),
            _ => None,
        };
        let top_person = match &top {
            Some(TopResult::Person(person)) => Some(person.id.clone()),
            _ => None,
        };
        if let Some(top) = &top_item {
            for (_, items) in &mut found {
                items.retain(|i| i.id() != top.id());
            }
        }

        // In All, episodes whose show is listed are counted, not shown.
        let mut hidden = 0;
        let quiet = request.kind.is_none() && request.scope == SearchScope::All;
        if quiet {
            let mut shows: HashSet<MediaId> = found
                .iter()
                .filter(|(k, _)| *k == SearchKind::Shows)
                .flat_map(|(_, items)| items.iter().map(|i| i.id().clone()))
                .collect();
            if let Some(Item::Show(show)) = &top_item {
                shows.insert(show.id.clone());
            }
            for (kind, items) in &mut found {
                if *kind == SearchKind::Episodes {
                    let before = items.len();
                    items.retain(|i| !matches!(i, Item::Episode(e) if shows.contains(&e.show_id)));
                    hidden = (before - items.len()) as u32;
                }
            }
        }

        let limit = request.limit as usize;
        let mut groups = Vec::new();
        for kind in &kinds {
            let one_kind = request.kind.is_some();
            if *kind == SearchKind::People {
                let mut people = people();
                // The top match counts in the total, as the server's does.
                let total = people.len() as u32;
                people.retain(|p| Some(&p.id) != top_person.as_ref());
                if people.is_empty() && !one_kind {
                    continue;
                }
                let more = people.len() > limit;
                groups.push(SearchGroup {
                    kind: *kind,
                    items: Vec::new(),
                    total: (!more).then_some(total),
                    more,
                    people: people.into_iter().take(limit).collect(),
                    hidden: 0,
                });
                continue;
            }
            let items = found
                .iter()
                .find(|(k, _)| k == kind)
                .map(|(_, i)| i.clone())
                .unwrap_or_default();
            let hidden = if *kind == SearchKind::Episodes {
                hidden
            } else {
                0
            };
            if items.is_empty() && hidden == 0 && !one_kind {
                continue;
            }
            let shown = if quiet && *kind == SearchKind::Episodes {
                QUIET_EPISODES.min(limit)
            } else {
                limit
            };
            // As the server counts: the top match and the hidden episodes
            // too.
            let top_here = top_item.as_ref().is_some_and(|t| kind_of(t) == Some(*kind));
            groups.push(SearchGroup {
                kind: *kind,
                total: Some(items.len() as u32 + hidden + u32::from(top_here)),
                more: items.len() > shown,
                items: items.into_iter().take(shown).collect(),
                people: Vec::new(),
                hidden,
            });
        }

        let count = |scope: SearchScope| -> Option<u32> {
            let mut n: usize = scope
                .kinds()
                .iter()
                .filter(|k| **k != SearchKind::People)
                .map(|k| hits(*k, year).len())
                .sum();
            if scope == SearchScope::All {
                n += people().len();
            }
            Some(n as u32)
        };
        let counts = SearchCounts {
            all: count(SearchScope::All),
            movies: count(SearchScope::Movies),
            shows: count(SearchScope::Shows),
            episodes: count(SearchScope::Episodes),
            music: count(SearchScope::Music),
        };

        let without_year = match (year, request.kind) {
            (Some(_), None) => {
                let with: HashSet<MediaId> = found
                    .iter()
                    .flat_map(|(_, i)| i.iter().map(|i| i.id().clone()))
                    .chain(top_item.iter().map(|t| t.id().clone()))
                    .collect();
                kinds
                    .iter()
                    .filter(|k| **k != SearchKind::People)
                    .flat_map(|k| hits(*k, None))
                    .filter(|i| !with.contains(i.id()))
                    .take(3)
                    .collect()
            }
            _ => Vec::new(),
        };

        SearchResults {
            top,
            groups,
            counts,
            year,
            without_year,
        }
    }

    /// "dune 2021": the words, and the year at the end as a filter (1900
    /// to next year). A query that is only a year is searched for.
    fn split_year(query: &str) -> (String, Option<u16>) {
        let query = query.trim().to_lowercase();
        let next_year = 1970
            + std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() / 31_556_952) as u16
            + 1;
        if let Some((words, last)) = query.rsplit_once(' ')
            && last.len() == 4
            && let Ok(year) = last.parse::<u16>()
            && (1900..=next_year).contains(&year)
            && !words.trim().is_empty()
        {
            return (words.trim().to_owned(), Some(year));
        }
        (query, None)
    }

    /// Whether `title` has a word starting with `text`, or starts with it.
    fn matches(text: &str, title: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let title = title.to_lowercase();
        title.starts_with(text)
            || title
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word.starts_with(text))
    }

    /// How well a name matches: the whole of it, its start, or within.
    fn name_rank(name: &str, text: &str) -> u8 {
        let name = name.to_lowercase();
        if name == text {
            0
        } else if name.starts_with(text) {
            1
        } else {
            2
        }
    }

    fn pick_top(found: &[(SearchKind, Vec<Item>)], text: &str) -> Option<Item> {
        let all: Vec<&Item> = TOP_ORDER
            .iter()
            .flat_map(|kind| found.iter().filter(move |(k, _)| k == kind))
            .flat_map(|(_, items)| items.iter())
            .collect();
        let exact: Vec<&Item> = all
            .iter()
            .copied()
            .filter(|i| title(i).to_lowercase() == text)
            .collect();
        let prefix: Vec<&Item> = all
            .iter()
            .copied()
            .filter(|i| title(i).to_lowercase().starts_with(text))
            .collect();
        [exact, prefix, all]
            .into_iter()
            .find(|pool| !pool.is_empty())
            .and_then(|pool| pool.first().map(|i| (*i).clone()))
    }

    fn kind_of(item: &Item) -> Option<SearchKind> {
        Some(match item {
            Item::Movie(_) => SearchKind::Movies,
            Item::Show(_) => SearchKind::Shows,
            Item::Episode(_) => SearchKind::Episodes,
            Item::Artist(_) => SearchKind::Artists,
            Item::Album(_) => SearchKind::Albums,
            Item::Track(_) => SearchKind::Tracks,
            Item::Season(_) => return None,
        })
    }

    fn title(item: &Item) -> &str {
        match item {
            Item::Movie(m) => &m.title,
            Item::Show(s) => &s.title,
            Item::Season(s) => &s.title,
            Item::Episode(e) => &e.title,
            Item::Artist(a) => &a.name,
            Item::Album(a) => &a.title,
            Item::Track(t) => &t.title,
        }
    }

    fn year_of(item: &Item) -> Option<u16> {
        match item {
            Item::Movie(m) => m.year,
            Item::Show(s) => s.year,
            Item::Album(a) => a.year,
            _ => None,
        }
    }

    /// A library's items and everything under them.
    fn library_items(catalog: &Catalog, id: &LibraryId) -> HashSet<MediaId> {
        let mut out = HashSet::new();
        let mut todo: Vec<MediaId> = catalog
            .libraries
            .iter()
            .find(|(l, _)| &l.id == id)
            .map(|(_, items)| items.clone())
            .unwrap_or_default();
        while let Some(item) = todo.pop() {
            if let Some(children) = catalog.children.get(&item) {
                todo.extend(children.iter().cloned());
            }
            out.insert(item);
        }
        out
    }
}

#[cfg(test)]
use crate::backend::TrackReason;
#[cfg(test)]
use spoolframe_types::{SdhPreference, SubtitleMode};

#[cfg(test)]
mod tests {

    use super::*;
    use crate::testing::{file_with, ready};

    fn title(item: &Item) -> &str {
        match item {
            Item::Movie(m) => &m.title,
            Item::Show(s) => &s.title,
            Item::Season(s) => &s.title,
            Item::Episode(e) => &e.title,
            Item::Artist(a) => &a.name,
            Item::Album(a) => &a.title,
            Item::Track(t) => &t.title,
        }
    }

    fn titles(items: &[Item]) -> Vec<&str> {
        items.iter().map(title).collect()
    }

    fn session_for(client: &FakeClient, username: &str) -> Arc<dyn Session> {
        client.session(&FakeClient::profile(username)).unwrap()
    }

    fn andrey() -> Arc<dyn Session> {
        session_for(&FakeClient::new(None), "andrey")
    }

    fn usernames(profiles: &[Profile]) -> Vec<&str> {
        profiles.iter().map(|p| p.username.as_str()).collect()
    }

    #[test]
    fn there_is_one_library_of_each_kind() {
        let kinds: Vec<_> = ready(andrey().libraries())
            .unwrap()
            .into_iter()
            .map(|l| l.kind)
            .collect();
        assert_eq!(
            kinds,
            [LibraryKind::Movies, LibraryKind::Shows, LibraryKind::Music]
        );
    }

    #[test]
    fn continue_watching_is_started_unfinished_items_newest_first() {
        let home = ready(andrey().home()).unwrap();
        assert_eq!(
            titles(&home.continue_watching),
            ["Alien", "Dune: Part Two", "Woe's Hollow"]
        );
    }

    #[test]
    fn next_up_is_the_episode_after_the_latest_watched_one() {
        let home = ready(andrey().home()).unwrap();
        assert_eq!(titles(&home.next_up), ["Reckoning"]);
    }

    #[test]
    fn a_show_with_an_episode_in_progress_is_not_in_next_up() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        // Andor E1–2 watched; E4 started, past E3, as with Vigil S2E3.
        let e4 = media(MediaKind::Episode, "65");
        let playback = ready(session.start_playback(&e4, None, Default::default())).unwrap();
        ready(session.update_progress(&playback.handle, Duration::from_secs(120))).unwrap();
        let home = ready(session.home()).unwrap();
        assert!(
            !titles(&home.next_up).contains(&"Reckoning"),
            "Next up: {:?}",
            titles(&home.next_up)
        );
    }

    #[test]
    fn next_up_skips_an_episode_already_in_continue_watching() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        // Her PIN is asked for here (P2-08).
        ready(alice.unlock(FAKE_PIN)).unwrap();
        let home = ready(alice.home()).unwrap();
        assert!(home.next_up.is_empty(), "{:?}", titles(&home.next_up));
    }

    #[test]
    fn each_user_has_their_own_continue_watching() {
        let client = FakeClient::new(None);
        let andrey = ready(session_for(&client, "andrey").home()).unwrap();
        let alice = session_for(&client, "alice");
        // Her PIN is asked for here (P2-08).
        ready(alice.unlock(FAKE_PIN)).unwrap();
        let alice = ready(alice.home()).unwrap();
        assert_ne!(andrey.continue_watching, alice.continue_watching);
    }

    #[test]
    fn progress_under_a_minute_does_not_start_an_item() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let heat = media(MediaKind::Movie, "3");
        let playback = ready(session.start_playback(&heat, None, Default::default())).unwrap();
        ready(session.update_progress(&playback.handle, Duration::from_secs(30))).unwrap();
        let home = ready(session.home()).unwrap();
        assert!(!titles(&home.continue_watching).contains(&"Heat"));
    }

    #[test]
    fn a_hidden_item_leaves_continue_watching_and_keeps_its_progress() {
        let session = andrey();
        let first = ready(session.home()).unwrap().continue_watching[0].clone();
        ready(session.hide_from_continue(first.id())).unwrap();
        let home = ready(session.home()).unwrap();
        assert!(!home.continue_watching.iter().any(|i| i.id() == first.id()));
    }

    #[test]
    fn playing_another_episode_leaves_a_removed_one_removed() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        // Severance E4 is in Continue watching; E5 is the show's next.
        let e4 = media(MediaKind::Episode, "25");
        ready(session.hide_from_continue(&e4)).unwrap();
        let e5 = media(MediaKind::Episode, "26");
        let playback = ready(session.start_playback(&e5, None, Default::default())).unwrap();
        ready(session.update_progress(&playback.handle, Duration::from_secs(120))).unwrap();
        let home = ready(session.home()).unwrap();
        assert!(
            !home.continue_watching.iter().any(|i| i.id() == &e4),
            "{:?}",
            titles(&home.continue_watching)
        );
    }

    #[test]
    fn progress_brings_a_hidden_item_back() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let first = ready(session.home()).unwrap().continue_watching[0].clone();
        ready(session.hide_from_continue(first.id())).unwrap();
        let playback = ready(session.start_playback(first.id(), None, Default::default())).unwrap();
        ready(session.update_progress(&playback.handle, Duration::from_secs(120))).unwrap();
        let home = ready(session.home()).unwrap();
        assert!(home.continue_watching.iter().any(|i| i.id() == first.id()));
    }

    #[test]
    fn progress_puts_an_item_first_in_continue_watching() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let heat = media(MediaKind::Movie, "3");
        let playback = ready(session.start_playback(&heat, None, Default::default())).unwrap();
        ready(session.update_progress(&playback.handle, Duration::from_secs(600))).unwrap();
        let home = ready(session.home()).unwrap();
        assert_eq!(title(&home.continue_watching[0]), "Heat");
    }

    #[test]
    fn progress_past_ninety_five_percent_marks_the_item_watched() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let alien = media(MediaKind::Movie, "1");
        let playback = ready(session.start_playback(&alien, None, Default::default())).unwrap();
        ready(session.update_progress(&playback.handle, Duration::from_secs(112 * 60))).unwrap();
        let (item, _) = ready(session.item(&alien)).unwrap();
        let Item::Movie(m) = item else { panic!() };
        assert!(m.playback.watched && m.playback.position.is_none());
    }

    #[test]
    fn marking_watched_then_unwatched_clears_the_state() {
        let session = andrey();
        let alien = media(MediaKind::Movie, "1");
        ready(session.mark_watched(&alien)).unwrap();
        ready(session.mark_unwatched(&alien)).unwrap();
        let (item, _) = ready(session.item(&alien)).unwrap();
        let Item::Movie(m) = item else { panic!() };
        // Cleared, though the mark leaves it viewed just now.
        assert_eq!(
            PlaybackState {
                last_viewed_at: None,
                ..m.playback
            },
            PlaybackState::default()
        );
    }

    #[test]
    fn watch_state_survives_switching_profiles() {
        let client = FakeClient::new(None);
        let heat = media(MediaKind::Movie, "3");
        ready(session_for(&client, "andrey").mark_watched(&heat)).unwrap();
        let (item, _) = ready(session_for(&client, "andrey").item(&heat)).unwrap();
        let Item::Movie(m) = item else { panic!() };
        assert!(m.playback.watched);
    }

    #[test]
    fn an_album_lists_its_tracks_in_order() {
        let (_, tracks) = ready(andrey().item(&media(MediaKind::Album, "31"))).unwrap();
        let numbers: Vec<_> = tracks
            .iter()
            .map(|t| match t {
                Item::Track(t) => t.track.unwrap(),
                other => panic!("{} is not a track", title(other)),
            })
            .collect();
        assert_eq!(numbers, (1..=9).collect::<Vec<_>>());
    }

    #[test]
    fn a_page_of_a_library_comes_with_the_total() {
        let session = andrey();
        let movies = LibraryId::new(Namespace::Plex, "1").unwrap();
        let (page, total) = ready(session.items(&movies, 2, 3)).unwrap();
        assert_eq!(
            (titles(&page), total),
            (vec!["Blade Runner", "Moon", "Sunshine"], 8)
        );
    }

    fn search(query: &str, scope: SearchScope, kind: Option<SearchKind>) -> SearchResults {
        let request = SearchRequest {
            query: query.into(),
            limit: 10,
            scope,
            kind,
            library: None,
        };
        ready(andrey().search(&request)).unwrap()
    }

    fn group(results: &SearchResults, kind: SearchKind) -> Option<&SearchGroup> {
        results.groups.iter().find(|g| g.kind == kind)
    }

    #[test]
    fn a_person_named_in_full_is_the_top_match_and_leaves_people() {
        let results = search("denis villeneuve", SearchScope::All, None);
        assert_eq!(
            (
                results
                    .top
                    .as_ref()
                    .map(|t| matches!(t, TopResult::Person(_))),
                group(&results, SearchKind::People).map(|g| g.people.len()),
            ),
            (Some(true), None)
        );
    }

    #[test]
    fn the_top_match_is_not_repeated_in_its_group() {
        // "Low Tide" is an album and its title track; the album wins.
        let results = search("LOW TIDE", SearchScope::All, None);
        assert_eq!(
            (
                results
                    .top
                    .as_ref()
                    .map(|t| matches!(t, TopResult::Item(Item::Album(_)))),
                group(&results, SearchKind::Albums).map(|g| g.items.len()),
                group(&results, SearchKind::Tracks).map(|g| titles(&g.items)),
            ),
            (Some(true), None, Some(vec!["Low Tide"]))
        );
    }

    #[test]
    fn see_all_is_one_group_even_when_empty() {
        let results = search("zzz", SearchScope::All, Some(SearchKind::Movies));
        assert_eq!(
            (
                results.top.is_none(),
                results.groups.len(),
                results.groups[0].kind
            ),
            (true, 1, SearchKind::Movies)
        );
    }

    #[test]
    fn a_trailing_year_narrows_and_the_rest_is_offered() {
        let results = search("arrival 1990", SearchScope::All, None);
        assert_eq!(
            (
                results.year,
                results.top.is_none(),
                titles(&results.without_year)
            ),
            (Some(1990), true, vec!["Arrival"])
        );
    }

    #[test]
    fn every_scope_is_counted() {
        let counts = search("low tide", SearchScope::Movies, None).counts;
        assert_eq!(
            (counts.music, counts.movies, counts.all),
            (Some(2), Some(0), Some(2))
        );
    }

    #[test]
    fn people_are_found_and_their_items_listed() {
        // Listed under People: in All, the only match is the top result.
        let results = search("villeneuve", SearchScope::All, Some(SearchKind::People));
        let person = &group(&results, SearchKind::People).unwrap().people[0];
        let (items, total) = ready(andrey().person_items(&person.id, 0, 10)).unwrap();
        assert_eq!(
            (person.name.as_str(), titles(&items), total),
            ("Denis Villeneuve", vec!["Arrival", "Dune: Part Two"], 2)
        );
    }

    #[test]
    fn in_all_episodes_under_a_listed_show_are_counted_not_shown() {
        // "a": the show Andor, and its episode "Aldhani".
        let results = search("a", SearchScope::All, None);
        let episodes = group(&results, SearchKind::Episodes).unwrap();
        assert!(!titles(&episodes.items).contains(&"Aldhani"));
        assert_eq!(episodes.hidden, 1);
    }

    #[test]
    fn a_library_search_finds_only_its_items() {
        let request = SearchRequest {
            query: "low".into(),
            limit: 10,
            scope: SearchScope::All,
            kind: None,
            library: Some(library("1", "Movies", LibraryKind::Movies).id),
        };
        let results = ready(andrey().search(&request)).unwrap();
        assert!(results.top.is_none() && results.groups.is_empty());
    }

    #[test]
    fn a_library_search_finds_the_people_in_its_items() {
        let in_library = |key: &str, name: &str| SearchRequest {
            query: "villeneuve".into(),
            limit: 10,
            scope: SearchScope::All,
            kind: Some(SearchKind::People),
            library: Some(library(key, name, LibraryKind::Movies).id),
        };
        let people = |request: &SearchRequest| {
            ready(andrey().search(request))
                .unwrap()
                .groups
                .iter()
                .map(|g| g.people.len())
                .sum::<usize>()
        };
        assert_eq!(
            (
                people(&in_library("1", "Movies")),
                people(&in_library("3", "Music"))
            ),
            (1, 0)
        );
    }

    #[test]
    fn an_unknown_item_is_not_found() {
        let missing = media(MediaKind::Show, "999");
        assert_eq!(
            ready(andrey().item(&missing)).map(|_| ()),
            Err(BackendError::NotFound("plex:show:999".into()))
        );
    }

    #[test]
    fn playback_without_media_says_how_to_get_some() {
        let alien = media(MediaKind::Movie, "1");
        let error = ready(andrey().start_playback(&alien, None, Default::default())).unwrap_err();
        assert!(matches!(error, BackendError::Unavailable(m) if m.contains("--fake-media")));
    }

    #[test]
    fn reads_return_the_requested_range_of_the_file() {
        let bytes: Vec<u8> = (0..=255).collect();
        let (_dir, path) = file_with("fake", &bytes);
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let alien = media(MediaKind::Movie, "1");
        let start = ready(session.start_playback(&alien, None, Default::default())).unwrap();
        assert_eq!(start.size, 256);
        let read = ready(session.read(&start.handle, 250, 100)).unwrap();
        assert_eq!(read, (250..=255).collect::<Vec<u8>>());
    }

    #[test]
    fn a_read_on_an_unknown_handle_has_expired() {
        let session = andrey();
        let handle = PlaybackHandle("nope".into());
        assert_eq!(
            ready(session.read(&handle, 0, 10)),
            Err(BackendError::PlaybackExpired)
        );
    }

    #[test]
    fn the_external_subtitle_comes_with_its_playback() {
        let (_dir, path) = file_with("fake", &[0; 17]);
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let alien = media(MediaKind::Movie, "1");
        let selection = Selection {
            audio: None,
            subtitle: Some(0),
        };
        let start =
            ready(session.start_playback(&alien, Some(selection), Default::default())).unwrap();
        let (format, bytes) = ready(session.read_subtitle(&start.handle)).unwrap();
        assert_eq!(format, SubtitleFormat::Srt);
        assert!(String::from_utf8(bytes).unwrap().contains("read_subtitle"));
    }

    #[test]
    fn two_users_start_signed_in_on_home_media() {
        let client = FakeClient::new(None);
        let profiles = ready(client.profiles(&home_media())).unwrap();
        assert_eq!(usernames(&profiles), ["andrey", "alice"]);
    }

    #[test]
    fn signing_in_adds_a_profile() {
        let client = FakeClient::new(None);
        ready(client.sign_in(&home_media(), "steve", FAKE_PASSWORD, false)).unwrap();
        let profiles = ready(client.profiles(&home_media())).unwrap();
        assert_eq!(usernames(&profiles), ["andrey", "alice", "steve"]);
    }

    #[test]
    fn an_unknown_user_gets_the_same_refusal_as_a_wrong_password() {
        let client = FakeClient::new(None);
        assert_eq!(
            ready(client.sign_in(&home_media(), "mallory", FAKE_PASSWORD, false)),
            Err(BackendError::WrongCredentials)
        );
    }

    #[test]
    fn repeated_failures_are_rate_limited() {
        let client = FakeClient::new(None);
        for _ in 0..SIGN_IN_ATTEMPTS {
            let _ = ready(client.sign_in(&home_media(), "steve", "hunter2", false));
        }
        let refused = ready(client.sign_in(&home_media(), "steve", FAKE_PASSWORD, false));
        assert!(
            matches!(refused, Err(BackendError::RateLimited(_))),
            "{refused:?}"
        );
    }

    #[test]
    fn the_basement_server_is_unreachable() {
        let client = FakeClient::new(None);
        assert_eq!(
            ready(client.profiles(&basement())),
            Err(BackendError::Unreachable("Basement".into()))
        );
    }

    #[test]
    fn adding_an_address_for_a_new_key_adds_an_unpinned_server() {
        let client = FakeClient::new(None);
        let key = ServerKey("abcdefghijklmnop".into());
        let server = ready(client.add_address(&key, "10.0.0.5:4433")).unwrap();
        assert_eq!(
            (server.addresses, server.pinned, server.on_network),
            (vec!["10.0.0.5:4433".to_owned()], false, false)
        );
    }

    #[test]
    fn adding_an_address_changes_the_server_list() {
        let client = FakeClient::new(None);
        let changes = client.servers_changed();
        let key = ServerKey("abcdefghijklmnop".into());
        ready(client.add_address(&key, "10.0.0.5:4433")).unwrap();
        assert!(changes.has_changed().unwrap());
    }

    #[test]
    fn forgetting_a_server_drops_it_and_its_profiles() {
        let client = FakeClient::new(None);
        let key = home_media();
        ready(client.sign_in(&key, "andrey", FAKE_PASSWORD, false)).unwrap();
        ready(client.forget(&key)).unwrap();
        let servers = ready(client.servers()).unwrap();
        assert!(!servers.iter().any(|s| s.key == key));
    }

    #[test]
    fn signing_out_keeps_the_server_but_no_profiles() {
        let client = FakeClient::new(None);
        let key = home_media();
        ready(client.sign_in(&key, "andrey", FAKE_PASSWORD, false)).unwrap();
        ready(client.sign_out(&key)).unwrap();
        assert_eq!(ready(client.profiles(&key)), Ok(Vec::new()));
    }

    fn added_server(client: &FakeClient) -> ServerKey {
        // Home Media's address under a key not saved yet: it answers, as an
        // unsaved server would.
        let key = home_media();
        let mut servers = client.servers.lock().unwrap();
        let home = servers.iter_mut().find(|s| s.key == key).unwrap();
        home.pinned = false;
        home.verified = false;
        key
    }

    #[test]
    fn an_unpinned_server_refuses_sign_in() {
        let client = FakeClient::new(None);
        let key = added_server(&client);
        assert_eq!(
            ready(client.sign_in(&key, "andrey", FAKE_PASSWORD, false)),
            Err(BackendError::NotPinned(key.0.clone()))
        );
    }

    #[test]
    fn connecting_shows_the_fingerprint_without_saving() {
        let client = FakeClient::new(None);
        let key = added_server(&client);
        let server = ready(client.connect(&key)).unwrap();
        assert_eq!(
            (server.fingerprint.len(), server.pinned),
            ("K7QX-3MPA-9TRD-WF2H".len(), false)
        );
    }

    #[test]
    fn pinning_unverified_saves_and_lets_sign_in() {
        let client = FakeClient::new(None);
        let key = added_server(&client);
        let server = ready(client.pin(&key, false)).unwrap();
        assert_eq!((server.pinned, server.verified), (true, false));
        assert!(ready(client.sign_in(&key, "andrey", FAKE_PASSWORD, false)).is_ok());
    }

    #[test]
    fn verifying_later_marks_it_verified() {
        let client = FakeClient::new(None);
        let key = added_server(&client);
        ready(client.pin(&key, false)).unwrap();
        assert!(ready(client.pin(&key, true)).unwrap().verified);
    }

    #[test]
    fn trusting_a_new_identity_replaces_the_saved_server() {
        let client = FakeClient::new(None);
        let now = ServerKey("z9z9z9z9z9z9z9z9z9z9z9z9".into());
        let server = ready(client.trust_new_identity(&home_media(), &now)).unwrap();
        let keys: Vec<_> = ready(client.servers())
            .unwrap()
            .into_iter()
            .map(|s| s.key)
            .collect();
        assert_eq!(
            (
                server.name.as_str(),
                keys.contains(&home_media()),
                keys.contains(&now)
            ),
            ("Home Media", false, true)
        );
    }

    #[test]
    fn adding_a_known_address_again_changes_nothing() {
        let client = FakeClient::new(None);
        let key = ServerKey("abcdefghijklmnop".into());
        ready(client.add_address(&key, "10.0.0.5:4433")).unwrap();
        let changes = client.servers_changed();
        ready(client.add_address(&key, "10.0.0.5:4433")).unwrap();
        assert!(!changes.has_changed().unwrap());
    }

    #[test]
    fn a_phone_sign_in_shows_a_code_and_the_page_to_open() {
        let client = FakeClient::new(None);
        let begun = ready(client.begin_device_sign_in(&home_media(), false)).unwrap();
        assert_eq!(
            (begun.display_code.as_str(), begun.web_url.as_deref()),
            (
                "BCDF-GHJK",
                Some("https://media.example.org/auth/device?code=BCDFGHJK")
            )
        );
    }

    #[test]
    fn a_server_without_a_web_address_gives_only_the_code() {
        let client = FakeClient::new(None).without_web_address();
        let begun = ready(client.begin_device_sign_in(&home_media(), false)).unwrap();
        assert_eq!(begun.web_url, None);
    }

    #[test]
    fn an_approved_phone_sign_in_adds_the_profile() {
        let client = FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Approve("steve"));
        let begun = ready(client.begin_device_sign_in(&home_media(), false)).unwrap();
        let profile = ready(client.await_device_sign_in(&begun)).unwrap();
        assert!(usernames(&ready(client.profiles(&home_media())).unwrap()).contains(&"steve"));
        assert_eq!(profile.0.username, "steve");
    }

    #[test]
    fn a_refused_phone_sign_in_says_so() {
        let client = FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Deny);
        let begun = ready(client.begin_device_sign_in(&home_media(), false)).unwrap();
        assert_eq!(
            ready(client.await_device_sign_in(&begun)),
            Err(BackendError::SignInDenied)
        );
    }

    #[test]
    fn a_fourth_pending_phone_sign_in_is_refused() {
        let client = FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Wait);
        for _ in 0..3 {
            ready(client.begin_device_sign_in(&home_media(), false)).unwrap();
        }
        assert!(matches!(
            ready(client.begin_device_sign_in(&home_media(), false)),
            Err(BackendError::RateLimited(_))
        ));
    }

    #[test]
    fn previews_come_once_made_with_their_sheet() {
        let client = FakeClient::new(None).with_previews(PreviewsAnswer::NotReadyFor(1));
        let session = session_for(&client, "andrey");
        let movie = media(MediaKind::Movie, "2");
        let first = ready(session.previews(&movie));
        let previews = ready(session.previews(&movie)).unwrap();
        let sheet = ready(session.artwork(&previews.sheets[0], ArtworkSize::Original)).unwrap();
        assert_eq!(
            (first.map(|_| ()), previews.count, sheet.as_slice()),
            (
                Err(BackendError::PreviewsNotReady(30)),
                4,
                FAKE_PREVIEW_SHEET
            )
        );
    }

    #[test]
    fn music_has_no_previews() {
        let client = FakeClient::new(None).with_previews(PreviewsAnswer::Ready);
        let track = media(MediaKind::Track, "40");
        assert_eq!(
            ready(session_for(&client, "andrey").previews(&track)).map(|_| ()),
            Err(BackendError::NoPreviews)
        );
    }

    #[test]
    fn a_server_not_set_up_signs_in_but_has_no_catalog() {
        let client = FakeClient::new(None).not_set_up();
        let profile = ready(client.sign_in(&home_media(), "steve", FAKE_PASSWORD, false)).unwrap();
        let session = client.session(&profile).unwrap();
        assert_eq!(
            [
                ready(session.libraries()).map(|_| ()),
                ready(session.home()).map(|_| ())
            ],
            [Err(BackendError::NotSetUp), Err(BackendError::NotSetUp)]
        );
    }

    /// Each invite state the join flow draws (P6-01), from the fake.
    mod invites {
        use super::*;

        fn saved(client: &FakeClient) -> bool {
            ready(client.servers())
                .unwrap()
                .iter()
                .any(|s| s.key == cabin() && s.pinned)
        }

        #[test]
        fn a_link_reads_without_saving_and_a_paste_that_is_not_one_is_refused() {
            let client = FakeClient::new(None);
            let invite =
                ready(client.read_invite(&FakeClient::invite_link(Some(INVITE_CODE)))).unwrap();
            assert_eq!(
                (invite.name.as_deref(), invite.has_code, invite.saved),
                (Some("Cabin"), true, false)
            );
            assert_eq!(invite.role, Some(spoolframe_types::Role::Consumer));
            assert_eq!(
                ready(client.read_invite("https://home-media.example/admin/invites")),
                Err(BackendError::NotAnInvite)
            );
            assert!(!saved(&client));
        }

        #[test]
        fn connecting_shows_who_answers_and_saves_nothing() {
            let client = FakeClient::new(None);
            let server =
                ready(client.connect_invite(&FakeClient::invite_link(Some(INVITE_CODE)))).unwrap();
            assert_eq!(server.fingerprint, fingerprint(&cabin()));
            assert!(server.passkey_site().is_some());
            assert!(!saved(&client));
        }

        #[test]
        fn a_link_without_a_code_saves_the_server_verified() {
            let client = FakeClient::new(None);
            let server = ready(client.save_invite_server(&FakeClient::invite_link(None))).unwrap();
            assert!(server.pinned && server.verified);
            assert!(saved(&client));
        }

        #[test]
        fn redeeming_makes_the_account_signs_in_and_saves_the_server() {
            let client = FakeClient::new(None);
            let link = FakeClient::invite_link(Some(INVITE_CODE));
            let profile =
                ready(client.redeem_invite(&link, "mum", Some("Mum"), "long enough")).unwrap();
            assert_eq!(
                (
                    profile.server,
                    profile.username.as_str(),
                    profile.display_name.as_str()
                ),
                (cabin(), "mum", "Mum")
            );
            assert!(saved(&client));
            // Its one use is spent.
            assert_eq!(
                ready(client.redeem_invite(&link, "dad", None, "long enough")),
                Err(BackendError::InviteExhausted)
            );
        }

        #[test]
        fn refused_codes_save_nothing() {
            let client = FakeClient::new(None);
            for (code, refusal) in [
                ("ABCDABCDABCD", BackendError::InviteInvalid),
                (EXPIRED_INVITE_CODE, BackendError::InviteExpired),
                (USED_INVITE_CODE, BackendError::InviteExhausted),
            ] {
                let link = FakeClient::invite_link(Some(code));
                assert_eq!(
                    ready(client.redeem_invite(&link, "mum", None, "long enough")),
                    Err(refusal)
                );
            }
            assert!(!saved(&client));
        }

        /// A taken name leaves the invite unused: the next name works.
        #[test]
        fn a_taken_username_leaves_the_invite_unused() {
            let client = FakeClient::new(None);
            let link = FakeClient::invite_link(Some(INVITE_CODE));
            assert_eq!(
                ready(client.redeem_invite(&link, "alice", None, "long enough")),
                Err(BackendError::UsernameTaken)
            );
            assert!(!saved(&client));
            assert!(ready(client.redeem_invite(&link, "mum", None, "long enough")).is_ok());
        }
    }

    #[test]
    fn home_media_is_connected_and_basement_is_not() {
        let client = FakeClient::new(None);
        assert_eq!(
            *client.connection(&home_media()).borrow(),
            ConnectionState::Connected
        );
        assert_eq!(
            *client.connection(&basement()).borrow(),
            ConnectionState::Closed
        );
    }

    fn chosen_for(session: &Arc<dyn Session>, key: &str) -> ChosenTracks {
        let episode = media(MediaKind::Episode, key);
        ready(session.playback_plan(&episode, None))
            .unwrap()
            .1
            .unwrap()
    }

    #[test]
    fn by_default_the_main_audio_and_forced_subtitles_are_chosen() {
        let chosen = chosen_for(&andrey(), "22");
        assert_eq!(
            chosen.selection,
            Selection {
                audio: Some(0),
                subtitle: Some(1)
            }
        );
    }

    #[test]
    fn always_with_sdh_preferred_picks_the_sdh_track() {
        let session = andrey();
        let mut preferences = ready(session.track_preferences()).unwrap().preferences;
        preferences.subtitles.mode = SubtitleMode::Always;
        preferences.subtitles.sdh = SdhPreference::Prefer;
        ready(session.set_track_preferences(&preferences)).unwrap();
        assert_eq!(chosen_for(&session, "22").selection.subtitle, Some(2));
    }

    #[test]
    fn a_series_override_applies_to_its_other_episodes() {
        let session = andrey();
        let picked = Selection {
            audio: Some(1),
            subtitle: None,
        };
        ready(session.set_track_override(&media(MediaKind::Episode, "22"), picked)).unwrap();
        let chosen = chosen_for(&session, "23");
        assert_eq!(
            (chosen.selection, chosen.audio),
            (picked, TrackReason::Override)
        );
    }

    #[test]
    fn clearing_the_override_goes_back_to_the_preferences() {
        let session = andrey();
        let episode = media(MediaKind::Episode, "22");
        let picked = Selection {
            audio: Some(1),
            subtitle: None,
        };
        ready(session.set_track_override(&episode, picked)).unwrap();
        ready(session.clear_track_override(&episode)).unwrap();
        assert_ne!(chosen_for(&session, "23").audio, TrackReason::Override);
    }

    #[test]
    fn a_requested_selection_is_played_as_asked_and_nothing_is_chosen() {
        let session = andrey();
        let episode = media(MediaKind::Episode, "22");
        let asked = Selection {
            audio: Some(2),
            subtitle: None,
        };
        let (_, chosen) = ready(session.playback_plan(&episode, Some(asked))).unwrap();
        assert_eq!(chosen, None);
    }

    #[test]
    fn stopping_a_converted_playback_ends_its_encode() {
        let (_dir, path) = file_with("fake", b"output");
        let client = FakeClient::new(Some(path)).converting(2);
        let profile = Profile {
            server: home_media(),
            username: "andrey".into(),
            display_name: "Andrey".into(),
            locked: false,
            sign_in: Default::default(),
            trust: None,
        };
        let session = client.session(&profile).unwrap();
        let start =
            ready(session.start_playback(&media(MediaKind::Movie, "1"), None, Default::default()))
                .unwrap();
        ready(session.stop_playback(&start.handle)).unwrap();
        assert_eq!(client.open_playbacks(&profile), 0);
    }

    fn profile_of(client: &FakeClient, username: &str) -> Profile {
        let profiles = ready(client.profiles(&home_media())).unwrap();
        profiles
            .into_iter()
            .find(|p| p.username == username)
            .unwrap()
    }

    #[test]
    fn a_profile_with_a_pin_required_here_is_listed_locked() {
        let client = FakeClient::new(None);
        assert!(profile_of(&client, "alice").locked);
        assert!(!profile_of(&client, "andrey").locked);
    }

    #[test]
    fn a_locked_profile_answers_nothing_until_unlocked() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        assert_eq!(
            ready(alice.libraries()).unwrap_err(),
            BackendError::ProfileLocked
        );
        ready(alice.unlock(FAKE_PIN)).unwrap();
        assert!(ready(alice.libraries()).is_ok());
    }

    #[test]
    fn a_wrong_pin_is_wrong_credentials() {
        let alice = session_for(&FakeClient::new(None), "alice");
        assert_eq!(
            ready(alice.unlock("0000")).unwrap_err(),
            BackendError::WrongCredentials
        );
    }

    fn made(credential_id: &[u8], name: &str) -> FinishPasskeyRegistration {
        FinishPasskeyRegistration {
            credential_id: credential_id.to_vec(),
            attestation_object: vec![0xa0],
            client_data_json: b"{}".to_vec(),
            name: name.into(),
        }
    }

    fn signed(credential_id: &[u8]) -> FinishPasskeySignIn {
        FinishPasskeySignIn {
            credential_id: credential_id.to_vec(),
            authenticator_data: vec![0],
            client_data_json: b"{}".to_vec(),
            signature: vec![0],
            user_handle: Some(b"andrey".to_vec()),
        }
    }

    #[test]
    fn a_passkey_made_in_the_app_signs_its_user_in() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        ready(andrey.begin_passkey_registration()).unwrap();
        ready(andrey.finish_passkey_registration(made(&[9, 9], "YubiKey 5C"))).unwrap();
        let profile =
            ready(client.finish_passkey_sign_in(&home_media(), signed(&[9, 9]), false)).unwrap();
        assert_eq!(profile.username, "andrey");
    }

    #[test]
    fn web_ui_and_old_app_passkeys_are_listed_but_do_not_sign_in_here() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let listed = ready(andrey.passkeys()).unwrap();
        let refused = |id: &[u8]| {
            ready(client.finish_passkey_sign_in(&home_media(), signed(id), false)).unwrap_err()
        };
        assert_eq!(
            (
                listed
                    .iter()
                    .map(|p| (p.app, p.usable_here))
                    .collect::<Vec<_>>(),
                refused(&[1, 2, 3, 4]),
                refused(&[5, 6, 7, 8]),
            ),
            (
                vec![(false, false), (true, false)],
                BackendError::WrongCredentials,
                BackendError::WrongCredentials
            )
        );
    }

    #[test]
    fn adding_a_passkey_long_after_signing_in_asks_to_sign_in_again() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        client.age_sign_ins();
        assert_eq!(
            ready(andrey.begin_passkey_registration()).unwrap_err(),
            BackendError::RecentSignInRequired
        );
    }

    #[test]
    fn a_passkeys_name_is_one_to_sixty_four_characters() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        assert!(matches!(
            ready(andrey.finish_passkey_registration(made(&[5], "   "))),
            Err(BackendError::InvalidInput(_))
        ));
    }

    #[test]
    fn the_last_way_in_cannot_be_removed() {
        let client = FakeClient::new(None);
        let sso = session_for(&client, "sso");
        ready(sso.finish_passkey_registration(made(&[6], "Phone"))).unwrap();
        let id = ready(sso.passkeys()).unwrap()[0].id.clone();
        assert_eq!(
            ready(sso.remove_passkey(&id)).unwrap_err(),
            BackendError::LastSignInMethod
        );
    }

    #[test]
    fn ten_wrong_pins_end_the_session_here() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        for _ in 0..9 {
            let _ = ready(alice.unlock("0000"));
        }
        assert_eq!(
            ready(alice.unlock("0000")).unwrap_err(),
            BackendError::SignInRequired
        );
        // Kept by name, as the SDK keeps an ended session, to sign in
        // again with a password.
        assert_eq!(
            profile_of(&client, "alice").sign_in,
            ProfileSignIn::Password
        );
    }

    #[test]
    fn an_ended_session_last_signed_in_by_passkey_is_offered_a_passkey() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        ready(alice.finish_passkey_registration(made(&[8], "Key"))).unwrap();
        let mut signed = signed(&[8]);
        signed.user_handle = Some(b"alice".to_vec());
        ready(client.finish_passkey_sign_in(&home_media(), signed, false)).unwrap();
        let alice = session_for(&client, "alice");
        for _ in 0..10 {
            let _ = ready(alice.unlock("0000"));
        }
        assert_eq!(profile_of(&client, "alice").sign_in, ProfileSignIn::Passkey);
    }

    #[test]
    fn confirming_with_the_password_lets_an_old_sign_in_add_a_passkey() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        client.age_sign_ins();
        assert_eq!(
            ready(andrey.begin_passkey_registration()).unwrap_err(),
            BackendError::RecentSignInRequired
        );
        ready(andrey.confirm_password(FAKE_PASSWORD)).unwrap();
        assert!(ready(andrey.begin_passkey_registration()).is_ok());
    }

    #[test]
    fn a_user_without_an_app_passkey_cannot_confirm_with_one() {
        let client = FakeClient::new(None);
        // Andrey's only passkey is the web UI's.
        assert_eq!(
            ready(session_for(&client, "andrey").begin_confirm_passkey()).unwrap_err(),
            BackendError::NotFound("app passkey".into())
        );
    }

    #[test]
    fn signed_out_elsewhere_the_watch_fires_and_profiles_ask_again() {
        let client = FakeClient::new(None);
        session_for(&client, "andrey");
        let mut sign_outs = client.signed_out(&home_media());
        client.sign_out_elsewhere();
        assert_eq!(
            (
                sign_outs.has_changed().unwrap(),
                profile_of(&client, "andrey").sign_in
            ),
            (true, ProfileSignIn::Password)
        );
        sign_outs.mark_unchanged();
    }

    fn remembered_sign_in(client: &FakeClient, username: &str) {
        ready(client.sign_in(&home_media(), username, FAKE_PASSWORD, true)).unwrap();
    }

    #[test]
    fn a_remembered_profile_resumes_after_its_session_ends() {
        let client = FakeClient::new(None);
        remembered_sign_in(&client, "andrey");
        // The session ends (signed out elsewhere); the trust stays.
        client
            .fake_session(&FakeClient::profile("andrey"))
            .pin_state()
            .revoked = true;
        let profile = profile_of(&client, "andrey");
        assert_eq!(profile.sign_in, ProfileSignIn::Resume);
        let user_id = profile.trust.unwrap().user_id;
        assert!(ready(client.resume_profile(&home_media(), &user_id)).is_ok());
    }

    #[test]
    fn remember_is_off_unless_asked() {
        let client = FakeClient::new(None);
        ready(client.sign_in(&home_media(), "andrey", FAKE_PASSWORD, false)).unwrap();
        assert!(
            ready(client.trusted_profiles(&home_media()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn signing_out_forgets_the_profile_and_its_trust() {
        let client = FakeClient::new(None);
        remembered_sign_in(&client, "andrey");
        ready(session_for(&client, "andrey").sign_out()).unwrap();
        assert!(
            !usernames(&ready(client.profiles(&home_media())).unwrap()).contains(&"andrey")
                && ready(client.resume_profile(&home_media(), "andrey")).unwrap_err()
                    == BackendError::SignInRequired
        );
    }

    #[test]
    fn one_profile_opens_at_start_and_it_has_no_pin_here() {
        let client = FakeClient::new(None);
        remembered_sign_in(&client, "andrey");
        remembered_sign_in(&client, "alice");
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        ready(session_for(&client, "andrey").set_device_trust(None, true, true)).unwrap();
        ready(alice.set_device_trust(None, true, true)).unwrap();
        let remembered = ready(client.trusted_profiles(&home_media())).unwrap();
        let auto: Vec<(&str, bool, bool)> = remembered
            .iter()
            .map(|t| (t.username.as_str(), t.auto_login, t.pin_required))
            .collect();
        assert_eq!(auto, [("alice", true, false), ("andrey", false, false)]);
    }

    #[test]
    fn a_first_pin_long_after_signing_in_asks_to_sign_in_again() {
        let client = FakeClient::new(None);
        client.age_sign_ins();
        assert_eq!(
            ready(session_for(&client, "andrey").set_pin("2468", None)),
            Err(BackendError::RecentSignInRequired)
        );
    }

    #[test]
    fn a_deleted_device_leaves_the_list() {
        let andrey = andrey();
        ready(andrey.forget_device(LIVING_ROOM_TV)).unwrap();
        let ids: Vec<String> = ready(andrey.devices())
            .unwrap()
            .into_iter()
            .map(|d| d.endpoint_id)
            .collect();
        assert_eq!(ids, [THIS_DEVICE]);
    }

    #[test]
    fn this_device_cannot_be_deleted_from_here() {
        assert!(matches!(
            ready(andrey().forget_device(THIS_DEVICE)),
            Err(BackendError::InvalidInput(_))
        ));
    }

    #[test]
    fn deleting_a_device_long_after_signing_in_asks_to_sign_in_again() {
        let client = FakeClient::new(None);
        client.age_sign_ins();
        assert_eq!(
            ready(session_for(&client, "andrey").forget_device(LIVING_ROOM_TV)),
            Err(BackendError::RecentSignInRequired)
        );
    }

    #[test]
    fn an_unknown_device_is_not_found() {
        assert!(matches!(
            ready(andrey().forget_device("someone-elses")),
            Err(BackendError::NotFound(_))
        ));
    }

    #[test]
    fn the_pin_back_on_turns_auto_login_off() {
        let client = FakeClient::new(None);
        remembered_sign_in(&client, "alice");
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        ready(alice.set_device_trust(None, true, true)).unwrap();
        ready(alice.set_pin_required(None, true)).unwrap();
        assert!(!ready(client.trusted_profiles(&home_media())).unwrap()[0].auto_login);
    }

    #[test]
    fn auto_login_without_trust_is_refused() {
        let client = FakeClient::new(None);
        assert!(matches!(
            ready(session_for(&client, "andrey").set_device_trust(None, false, true)),
            Err(BackendError::InvalidInput(_))
        ));
    }

    #[test]
    fn a_device_approved_with_remember_unticked_is_not_remembered() {
        let client =
            FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::ApproveForgetting("andrey"));
        let begun = ready(client.begin_device_sign_in(&home_media(), true)).unwrap();
        let (_, remembered) = ready(client.await_device_sign_in(&begun)).unwrap();
        assert_eq!(remembered, Some(false));
    }

    #[test]
    fn a_forgotten_profile_is_no_longer_listed() {
        let client = FakeClient::new(None);
        session_for(&client, "alice");
        ready(client.forget_profile(&home_media(), "alice")).unwrap();
        assert!(!usernames(&ready(client.profiles(&home_media())).unwrap()).contains(&"alice"));
    }

    #[test]
    fn switching_to_another_profile_and_back_locks_it_again() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        let other = session_for(&client, "andrey");
        ready(other.libraries()).unwrap();
        assert!(!profile_of(&client, "alice").locked);
        ready(other.activate()).unwrap();
        assert!(profile_of(&client, "alice").locked);
    }

    #[test]
    fn an_unlocked_session_turns_the_requirement_off() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        ready(alice.set_pin_required(None, false)).unwrap();
        assert!(!profile_of(&client, "alice").locked);
    }

    #[test]
    fn a_locked_session_cannot_remove_its_pin() {
        let alice = session_for(&FakeClient::new(None), "alice");
        assert_eq!(
            ready(alice.clear_pin(PinProof::Pin {
                pin: FAKE_PIN.into()
            }))
            .unwrap_err(),
            BackendError::ProfileLocked
        );
    }

    #[test]
    fn a_pin_must_be_four_to_eight_digits() {
        let andrey = andrey();
        assert!(matches!(
            ready(andrey.set_pin("12a4", None)),
            Err(BackendError::InvalidInput(_))
        ));
    }

    #[test]
    fn the_tv_can_be_set_from_here() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        ready(alice.set_pin_required(Some(LIVING_ROOM_TV), true)).unwrap();
        let devices = ready(alice.devices()).unwrap();
        let tv = devices
            .iter()
            .find(|d| d.endpoint_id == LIVING_ROOM_TV)
            .unwrap();
        assert!(tv.pin_required && !tv.this_device);
    }

    #[test]
    fn clearing_the_pin_stops_asking_everywhere() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        ready(alice.set_pin_required(Some(LIVING_ROOM_TV), true)).unwrap();
        ready(alice.clear_pin(PinProof::Pin {
            pin: FAKE_PIN.into(),
        }))
        .unwrap();
        let devices = ready(alice.devices()).unwrap();
        assert!(devices.iter().all(|d| !d.pin_required));
    }

    #[test]
    fn changing_a_pin_needs_the_current_one() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        assert!(matches!(
            ready(alice.set_pin("5678", None)),
            Err(BackendError::InvalidInput(_))
        ));
    }

    #[test]
    fn a_wrong_current_pin_is_refused() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        assert_eq!(
            ready(alice.clear_pin(PinProof::Pin { pin: "0000".into() })),
            Err(BackendError::WrongCredentials)
        );
    }

    #[test]
    fn the_password_removes_a_forgotten_pin() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        ready(alice.clear_pin(PinProof::Password {
            password: FAKE_PASSWORD.into(),
        }))
        .unwrap();
        assert_eq!(ready(alice.pin()), Ok(None));
    }

    #[test]
    fn the_pin_state_says_how_many_digits() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        ready(alice.unlock(FAKE_PIN)).unwrap();
        assert_eq!(
            ready(alice.pin()).unwrap().and_then(|p| p.digits),
            Some(FAKE_PIN.len() as u8)
        );
    }

    #[test]
    fn a_pending_device_sign_in_is_looked_up_by_its_code() {
        let request = ready(andrey().lookup_device_sign_in(FAKE_SIGN_IN_CODE)).unwrap();
        assert_eq!(request.device_name.as_deref(), Some("Living Room Apple TV"));
    }

    #[test]
    fn the_approver_decides_whether_the_device_is_remembered() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let asked = ready(andrey.lookup_device_sign_in(FAKE_SIGN_IN_CODE)).unwrap();
        assert!(asked.remember, "the fixture TV asks to be remembered");
        ready(andrey.approve_device_sign_in(FAKE_SIGN_IN_CODE, false)).unwrap();
        assert_eq!(
            client.approvals_by("andrey"),
            [(FAKE_SIGN_IN_CODE.to_owned(), false)]
        );
    }

    #[test]
    fn an_approved_code_is_used_up() {
        let andrey = andrey();
        ready(andrey.approve_device_sign_in(FAKE_SIGN_IN_CODE, false)).unwrap();
        assert!(matches!(
            ready(andrey.lookup_device_sign_in(FAKE_SIGN_IN_CODE)),
            Err(BackendError::NotFound(_))
        ));
    }

    #[test]
    fn approving_this_devices_own_code_is_refused() {
        assert!(matches!(
            ready(andrey().approve_device_sign_in(FAKE_OWN_SIGN_IN_CODE, false)),
            Err(BackendError::InvalidInput(_))
        ));
    }

    #[test]
    fn five_wrong_codes_are_rate_limited() {
        let andrey = andrey();
        for _ in 0..5 {
            let _ = ready(andrey.lookup_device_sign_in("BBBBBBBB"));
        }
        assert!(matches!(
            ready(andrey.lookup_device_sign_in(FAKE_SIGN_IN_CODE)),
            Err(BackendError::RateLimited(_))
        ));
    }

    #[test]
    fn a_locked_profile_cannot_approve() {
        let alice = session_for(&FakeClient::new(None), "alice");
        assert_eq!(
            ready(alice.approve_device_sign_in(FAKE_SIGN_IN_CODE, false)).unwrap_err(),
            BackendError::ProfileLocked
        );
    }

    /// Invites an admin makes from the app (PX-44).
    mod minted_invites {
        use super::*;

        fn create_user() -> InviteSpec {
            InviteSpec {
                kind: InviteKind::CreateUser,
                note: Some("For Sam".into()),
                uses: Some(2),
                expires_in: Some(Duration::from_secs(7 * 24 * 3600)),
                role: Some(spoolframe_types::Role::Consumer),
            }
        }

        #[test]
        fn only_an_admins_session_is_an_admins() {
            let client = FakeClient::new(None);
            assert_eq!(
                (
                    session_for(&client, "andrey").is_admin(),
                    session_for(&client, "bob").is_admin()
                ),
                (true, false)
            );
        }

        #[test]
        fn a_made_invite_is_listed_with_its_maker_and_no_uses() {
            let andrey = andrey();
            let made = ready(andrey.create_invite(create_user())).unwrap();
            let listed = ready(andrey.invites()).unwrap();
            assert_eq!(
                listed,
                vec![InviteSummary {
                    id: made.id,
                    kind: InviteKind::CreateUser,
                    note: Some("For Sam".into()),
                    uses: Some(2),
                    used: 0,
                    expires_at: made.expires_at,
                    created_by: "Andrey".into(),
                    created_by_username: Some("andrey".into()),
                    created_from: InviteOrigin::Device {
                        name: Some("This computer".into()),
                        platform: Platform::Linux,
                        this_device: true,
                    },
                    revoked: false,
                }]
            );
        }

        #[test]
        fn a_made_invite_is_copied_as_its_https_link() {
            let made = ready(andrey().create_invite(create_user())).unwrap();
            // The same parameters as the QR code's link, after the https
            // prefix: the code with them.
            let query = |link: &str| link.split_once(['?', '#']).map(|(_, q)| q.to_owned());
            assert_eq!(
                (
                    made.share_link.starts_with("https://"),
                    query(&made.share_link),
                    made.copy_string.clone(),
                ),
                (true, query(&made.link), made.share_link.clone())
            );
        }

        #[test]
        fn a_made_invites_link_carries_its_code_as_shown() {
            let made = ready(andrey().create_invite(create_user())).unwrap();
            assert!(
                made.link.ends_with(&format!("&cap={}", made.code)),
                "{}",
                made.link
            );
        }

        #[test]
        fn a_revoked_invite_stays_listed_as_revoked() {
            let andrey = andrey();
            let made = ready(andrey.create_invite(create_user())).unwrap();
            ready(andrey.revoke_invite(&made.id)).unwrap();
            assert!(ready(andrey.invites()).unwrap()[0].revoked);
        }

        #[test]
        fn revoking_an_unknown_invite_is_not_found() {
            assert!(matches!(
                ready(andrey().revoke_invite("nope")),
                Err(BackendError::NotFound(_))
            ));
        }

        #[test]
        fn someone_not_an_admin_is_refused_every_invite_request() {
            let bob = session_for(&FakeClient::new(None), "bob");
            assert_eq!(
                (
                    ready(bob.create_invite(create_user())).unwrap_err(),
                    ready(bob.invites()).unwrap_err(),
                    ready(bob.revoke_invite("any")).unwrap_err(),
                ),
                (
                    BackendError::AdminOnly,
                    BackendError::AdminOnly,
                    BackendError::AdminOnly
                )
            );
        }

        #[test]
        fn making_or_revoking_long_after_signing_in_asks_to_confirm() {
            let client = FakeClient::new(None);
            let andrey = session_for(&client, "andrey");
            let made = ready(andrey.create_invite(create_user())).unwrap();
            client.age_sign_ins();
            assert_eq!(
                (
                    ready(andrey.create_invite(create_user())).unwrap_err(),
                    ready(andrey.revoke_invite(&made.id)).unwrap_err(),
                ),
                (
                    BackendError::RecentSignInRequired,
                    BackendError::RecentSignInRequired
                )
            );
        }

        #[test]
        fn listing_long_after_signing_in_needs_no_confirmation() {
            let client = FakeClient::new(None);
            let andrey = session_for(&client, "andrey");
            client.age_sign_ins();
            assert!(ready(andrey.invites()).is_ok());
        }

        #[test]
        fn confirming_lets_an_old_sign_in_make_an_invite() {
            let client = FakeClient::new(None);
            let andrey = session_for(&client, "andrey");
            client.age_sign_ins();
            ready(andrey.confirm_password(FAKE_PASSWORD)).unwrap();
            assert!(ready(andrey.create_invite(create_user())).is_ok());
        }

        #[test]
        fn a_portable_account_invite_takes_no_role() {
            let spec = InviteSpec {
                kind: InviteKind::PortableAccount,
                ..create_user()
            };
            assert!(matches!(
                ready(andrey().create_invite(spec)),
                Err(BackendError::InvalidInput(_))
            ));
        }

        #[test]
        fn a_federation_invite_is_refused_until_federation_arrives() {
            let spec = InviteSpec {
                kind: InviteKind::Federation,
                role: None,
                ..create_user()
            };
            assert!(matches!(
                ready(andrey().create_invite(spec)),
                Err(BackendError::InvalidInput(_))
            ));
        }
    }

    /// Each saved server's relays (per-server relays, d0d0c701).
    mod server_relays {
        use super::*;

        fn home(client: &FakeClient) -> Server {
            ready(client.servers())
                .unwrap()
                .into_iter()
                .find(|s| s.key == home_media())
                .unwrap()
        }

        #[test]
        fn a_servers_relay_choice_is_kept() {
            let client = FakeClient::new(None);
            assert_eq!(home(&client).use_relays, None, "follows the app's setting");
            ready(client.set_server_relays(&home_media(), Some(false))).unwrap();
            assert_eq!(home(&client).use_relays, Some(false));
        }

        #[test]
        fn a_list_set_is_the_users_and_an_empty_one_goes_back_to_the_server() {
            let client = FakeClient::new(None);
            let mine = vec!["https://relay.example".to_owned()];
            ready(client.set_server_relay_urls(&home_media(), mine.clone())).unwrap();
            let edited = home(&client);
            ready(client.set_server_relay_urls(&home_media(), Vec::new())).unwrap();
            let back = home(&client);
            assert_eq!(
                (edited.relays, edited.relays_edited, back.relays_edited),
                (mine, true, false)
            );
        }

        #[test]
        fn a_relay_is_an_http_or_https_url() {
            let client = FakeClient::new(None);
            assert!(matches!(
                ready(client.set_server_relay_urls(&home_media(), vec!["relay.example".into()])),
                Err(BackendError::InvalidInput(_))
            ));
        }

        #[test]
        fn a_server_not_saved_has_no_relay_settings() {
            let client = FakeClient::new(None);
            assert!(matches!(
                ready(client.set_server_relays(&basement(), Some(true))),
                Err(BackendError::NotPinned(_))
            ));
        }

        #[test]
        fn a_server_saved_from_an_invite_with_relays_uses_them() {
            let client = FakeClient::new(None);
            let saved = ready(client.save_invite_server(&FakeClient::invite_link(None))).unwrap();
            assert_eq!(saved.use_relays, Some(true));
        }
    }

    /// Why a server can or cannot be reached (0d0c4ff4).
    mod reachability {
        use super::*;

        #[test]
        fn a_saved_server_says_where_it_was_reached() {
            let client = FakeClient::new(None);
            let reach = ready(client.reachability(&home_media())).unwrap();
            assert_eq!(
                (
                    reach.saved,
                    reach
                        .addrs
                        .iter()
                        .map(|a| a.addr.as_str())
                        .collect::<Vec<_>>(),
                    reach.last_dial.map(|d| d.connected)
                ),
                (true, vec!["192.168.1.20"], Some(true))
            );
        }

        #[test]
        fn a_server_gone_quiet_says_its_last_dial_timed_out() {
            let client = FakeClient::new(None);
            client.go_away();
            let dial = ready(client.reachability(&home_media()))
                .unwrap()
                .last_dial
                .unwrap();
            assert_eq!(
                (dial.connected, dial.stage),
                (false, Some(crate::backend::DialStage::Timeout))
            );
        }

        #[test]
        fn a_server_never_seen_is_not_found() {
            let client = FakeClient::new(None);
            assert!(matches!(
                ready(client.reachability(&ServerKey("f00d".repeat(16)))),
                Err(BackendError::NotFound(_))
            ));
        }
    }

    /// An admin reset a password to a one-time one (c86e6d3a).
    mod portable_sign_in {
        use super::*;

        use crate::testing::NoPrompt;

        #[test]
        fn an_identity_this_computer_lacks_needs_pairing() {
            let client = FakeClient::new(None);
            let stranger = crate::identity::in_memory();
            let key = stranger.key(Arc::new(NoPrompt));
            let label = crate::identity::KeyLabel {
                name: "Main key".into(),
                maker: None,
                added_at: 0,
            };
            let id = ready(stranger.create("Alice", &[(key, label)])).unwrap();
            assert_eq!(
                ready(client.sign_in_portable(&home_media(), id)).unwrap_err(),
                BackendError::NotPaired
            );
        }

        #[test]
        fn the_identity_here_signs_in_as_a_guest() {
            let client = FakeClient::new(None);
            let identities = client.identities();
            let key = identities.key(Arc::new(NoPrompt));
            let label = crate::identity::KeyLabel {
                name: "Main key".into(),
                maker: None,
                added_at: 0,
            };
            let id = ready(identities.create("Alice", &[(key, label)])).unwrap();
            let profile = ready(client.sign_in_portable(&home_media(), id)).unwrap();
            assert_eq!(profile.username, FAKE_GUEST);
        }
    }

    mod password_reset {
        use super::*;

        fn reset() -> (FakeClient, Profile) {
            let client = FakeClient::new(None);
            client.reset_password("andrey");
            let profile =
                ready(client.sign_in(&home_media(), "andrey", FAKE_ONE_TIME_PASSWORD, false))
                    .unwrap();
            (client, profile)
        }

        #[test]
        fn signing_in_with_the_one_time_password_asks_for_a_new_one() {
            let (client, profile) = reset();
            assert!(client.session(&profile).unwrap().password_change_required());
        }

        #[test]
        fn until_then_everything_else_is_refused() {
            let (client, profile) = reset();
            let session = client.session(&profile).unwrap();
            assert_eq!(
                ready(session.libraries()).unwrap_err(),
                BackendError::PasswordChangeRequired
            );
        }

        #[test]
        fn a_new_password_lets_the_session_do_anything_again() {
            let (client, profile) = reset();
            let session = client.session(&profile).unwrap();
            ready(session.change_password(FAKE_ONE_TIME_PASSWORD, "a much better one")).unwrap();
            assert!(!session.password_change_required());
            assert!(ready(session.libraries()).is_ok());
        }

        #[test]
        fn a_wrong_one_time_password_is_wrong_credentials() {
            let (client, profile) = reset();
            let session = client.session(&profile).unwrap();
            assert_eq!(
                ready(session.change_password("nope", "a much better one")).unwrap_err(),
                BackendError::WrongCredentials
            );
        }

        #[test]
        fn a_short_new_password_is_refused_with_why() {
            let (client, profile) = reset();
            let session = client.session(&profile).unwrap();
            assert!(matches!(
                ready(session.change_password(FAKE_ONE_TIME_PASSWORD, "short")),
                Err(BackendError::InvalidInput(_))
            ));
        }

        #[test]
        fn a_password_that_was_not_reset_signs_in_as_ever() {
            let client = FakeClient::new(None);
            let profile =
                ready(client.sign_in(&home_media(), "andrey", FAKE_PASSWORD, false)).unwrap();
            assert!(!client.session(&profile).unwrap().password_change_required());
        }
    }

    /// Tying an account to a portable identity (P7-04), and the guest
    /// with one (P7-06).
    mod ties {
        use super::*;
        use crate::testing::NoPrompt;

        fn id(byte: u8) -> spoolframe_protocol::portable::PortableUserId {
            spoolframe_protocol::portable::PortableUserId::from_bytes([byte; 32]).unwrap()
        }

        fn tied(session: &Arc<dyn Session>) -> Vec<spoolframe_protocol::portable::PortableUserId> {
            ready(session.tie_info())
                .unwrap()
                .into_iter()
                .map(|t| t.id)
                .collect()
        }

        #[test]
        fn an_account_starts_untied() {
            assert!(ready(andrey().tie_info()).unwrap().is_empty());
        }

        #[test]
        fn a_tie_is_what_the_server_then_says() {
            let session = andrey();
            ready(session.tie(id(7))).unwrap();
            assert_eq!(tied(&session), [id(7)]);
        }

        #[test]
        fn a_new_tie_is_not_used_yet() {
            let session = andrey();
            ready(session.tie(id(7))).unwrap();
            assert_eq!(ready(session.tie_info()).unwrap()[0].last_used_at, None);
        }

        #[test]
        fn an_account_ties_several_identities_oldest_first() {
            let session = andrey();
            ready(session.tie(id(7))).unwrap();
            ready(session.tie(id(8))).unwrap();
            assert_eq!(tied(&session), [id(7), id(8)]);
        }

        #[test]
        fn tying_an_identity_tied_here_already_changes_nothing() {
            let session = andrey();
            ready(session.tie(id(7))).unwrap();
            ready(session.tie(id(7))).unwrap();
            assert_eq!(tied(&session), [id(7)]);
        }

        #[test]
        fn tying_after_ten_minutes_wants_a_recent_sign_in() {
            let client = FakeClient::new(None);
            client.age_sign_ins();
            assert_eq!(
                ready(session_for(&client, "andrey").tie(id(7))).unwrap_err(),
                BackendError::RecentSignInRequired
            );
        }

        #[test]
        fn confirming_it_is_them_lets_the_tie_through() {
            let client = FakeClient::new(None);
            client.age_sign_ins();
            let session = session_for(&client, "andrey");
            ready(session.confirm_password(FAKE_PASSWORD)).unwrap();
            assert!(ready(session.tie(id(7))).is_ok());
        }

        #[test]
        fn an_identity_ties_to_one_account_per_server() {
            let client = FakeClient::new(None);
            ready(session_for(&client, "andrey").tie(id(7))).unwrap();
            assert!(matches!(
                ready(session_for(&client, "bob").tie(id(7))),
                Err(BackendError::Other(_))
            ));
        }

        #[test]
        fn severing_leaves_the_account_untied() {
            let session = andrey();
            ready(session.tie(id(7))).unwrap();
            ready(session.untie(id(7))).unwrap();
            assert!(ready(session.tie_info()).unwrap().is_empty());
        }

        #[test]
        fn severing_one_tie_leaves_the_others() {
            let session = andrey();
            ready(session.tie(id(7))).unwrap();
            ready(session.tie(id(8))).unwrap();
            ready(session.untie(id(7))).unwrap();
            assert_eq!(tied(&session), [id(8)]);
        }

        #[test]
        fn severing_leaves_the_session_signed_in() {
            let session = andrey();
            ready(session.tie(id(7))).unwrap();
            ready(session.untie(id(7))).unwrap();
            assert!(ready(session.tie_info()).is_ok());
        }

        #[test]
        fn a_severed_identity_ties_to_another_account() {
            let client = FakeClient::new(None);
            let andrey = session_for(&client, "andrey");
            ready(andrey.tie(id(7))).unwrap();
            ready(andrey.untie(id(7))).unwrap();
            assert!(ready(session_for(&client, "bob").tie(id(7))).is_ok());
        }

        #[test]
        fn severing_with_no_tie_finds_none() {
            assert!(matches!(
                ready(andrey().untie(id(7))),
                Err(BackendError::NotFound(_))
            ));
        }

        #[test]
        fn severing_an_identity_not_tied_here_finds_none() {
            let session = andrey();
            ready(session.tie(id(7))).unwrap();
            assert!(matches!(
                ready(session.untie(id(8))),
                Err(BackendError::NotFound(_))
            ));
        }

        #[test]
        fn severing_after_ten_minutes_wants_a_recent_sign_in() {
            let client = FakeClient::new(None);
            let session = session_for(&client, "andrey");
            ready(session.tie(id(7))).unwrap();
            client.age_sign_ins();
            assert_eq!(
                ready(session.untie(id(7))).unwrap_err(),
                BackendError::RecentSignInRequired
            );
        }

        #[test]
        fn an_account_is_no_guest() {
            assert_eq!(andrey().guest_identity(), None);
        }

        #[test]
        fn a_portable_sign_in_is_a_guest_with_that_identity() {
            let client = FakeClient::new(None);
            let identities = client.identities();
            let key = identities.key(Arc::new(NoPrompt));
            let label = crate::identity::KeyLabel {
                name: "Main key".into(),
                maker: None,
                added_at: 0,
            };
            let made = ready(identities.create("Alice", &[(key, label)])).unwrap();
            let profile = ready(client.sign_in_portable(&home_media(), made)).unwrap();
            assert_eq!(
                client.session(&profile).unwrap().guest_identity(),
                Some(made)
            );
        }
    }

    /// Leaving a server (PX-47).
    mod leaving {
        use super::*;
        use crate::testing::NoPrompt;

        fn saved(client: &FakeClient) -> bool {
            ready(client.servers())
                .unwrap()
                .iter()
                .any(|s| s.key == home_media())
        }

        /// Forgets every profile kept for Home Media but `username`'s, so
        /// it is the last.
        fn last_here(client: &FakeClient, username: &str) {
            for profile in ready(client.profiles(&home_media())).unwrap() {
                if profile.username != username {
                    ready(client.forget_profile(&home_media(), &profile.username)).unwrap();
                }
            }
        }

        fn kept(client: &FakeClient) -> Vec<String> {
            ready(client.profiles(&home_media()))
                .unwrap()
                .into_iter()
                .map(|p| p.username)
                .collect()
        }

        #[test]
        fn the_last_profile_leaving_forgets_the_server_here() {
            let client = FakeClient::new(None);
            last_here(&client, "steve");
            ready(session_for(&client, "steve").leave_server()).unwrap();
            assert!(!saved(&client));
        }

        /// Andrey and Alice are signed in here too: the server stays.
        #[test]
        fn another_profile_stays_so_the_server_stays() {
            let client = FakeClient::new(None);
            ready(session_for(&client, "steve").leave_server()).unwrap();
            assert!(saved(&client));
        }

        #[test]
        fn only_the_leaving_profile_goes() {
            let client = FakeClient::new(None);
            ready(client.sign_in(&home_media(), "steve", FAKE_PASSWORD, false)).unwrap();
            ready(session_for(&client, "steve").leave_server()).unwrap();
            assert_eq!(kept(&client), vec!["andrey", "alice"]);
        }

        /// Remembered on this device is kept too: the server stays.
        #[test]
        fn a_remembered_profile_keeps_the_server() {
            let client = FakeClient::new(None);
            ready(client.sign_in(&home_media(), "andrey", FAKE_PASSWORD, true)).unwrap();
            // Signed in no more; remembered all the same.
            last_here(&client, "steve");
            ready(session_for(&client, "steve").leave_server()).unwrap();
            assert!(saved(&client));
        }

        #[test]
        fn leaving_ends_the_session() {
            let client = FakeClient::new(None);
            let session = session_for(&client, "steve");
            ready(session.leave_server()).unwrap();
            assert_eq!(
                ready(session.tie_info()).unwrap_err(),
                BackendError::SignInRequired
            );
        }

        #[test]
        fn the_last_admin_cannot_leave() {
            let client = FakeClient::new(None);
            assert_eq!(
                ready(session_for(&client, FAKE_ADMIN).leave_server()).unwrap_err(),
                BackendError::LastAdmin
            );
        }

        #[test]
        fn the_last_admin_is_told_ahead_they_cannot_leave() {
            let client = FakeClient::new(None);
            assert!(!session_for(&client, FAKE_ADMIN).can_leave());
        }

        #[test]
        fn anyone_else_may_leave() {
            let client = FakeClient::new(None);
            assert!(ready(session_for(&client, "steve").can_leave_now()).unwrap());
        }

        #[test]
        fn the_last_admin_refused_keeps_the_server() {
            let client = FakeClient::new(None);
            let _ = ready(session_for(&client, FAKE_ADMIN).leave_server());
            assert!(saved(&client));
        }

        #[test]
        fn leaving_after_ten_minutes_wants_a_recent_sign_in() {
            let client = FakeClient::new(None);
            client.age_sign_ins();
            assert_eq!(
                ready(session_for(&client, "steve").leave_server()).unwrap_err(),
                BackendError::RecentSignInRequired
            );
        }

        /// Recency is asked before the last admin, as on the server.
        #[test]
        fn a_stale_last_admin_is_asked_to_confirm_first() {
            let client = FakeClient::new(None);
            client.age_sign_ins();
            assert_eq!(
                ready(session_for(&client, FAKE_ADMIN).leave_server()).unwrap_err(),
                BackendError::RecentSignInRequired
            );
        }

        #[test]
        fn confirming_it_is_them_lets_the_leave_through() {
            let client = FakeClient::new(None);
            client.age_sign_ins();
            let session = session_for(&client, "steve");
            ready(session.confirm_password(FAKE_PASSWORD)).unwrap();
            last_here(&client, "steve");
            ready(session.leave_server()).unwrap();
            assert!(!saved(&client));
        }

        /// A guest has no password: a fresh portable sign-in is recent.
        #[test]
        fn a_guest_signed_in_again_can_leave() {
            let client = FakeClient::new(None);
            let identities = client.identities();
            let key = identities.key(Arc::new(NoPrompt));
            let label = crate::identity::KeyLabel {
                name: "Main key".into(),
                maker: None,
                added_at: 0,
            };
            let made = ready(identities.create("Alice", &[(key, label)])).unwrap();
            ready(client.sign_in_portable(&home_media(), made)).unwrap();
            client.age_sign_ins();
            let again = ready(client.sign_in_portable(&home_media(), made)).unwrap();
            last_here(&client, &again.username);
            ready(client.session(&again).unwrap().leave_server()).unwrap();
            assert!(!saved(&client));
        }
    }

    /// A track's playback state as the album's page has it.
    fn track_state(session: &dyn Session, key: &str) -> PlaybackState {
        let album = media(MediaKind::Album, "31");
        let (_, tracks) = ready(session.item(&album)).unwrap();
        tracks
            .into_iter()
            .find_map(|t| match t {
                Item::Track(t) if t.id == media(MediaKind::Track, key) => Some(t.playback),
                _ => None,
            })
            .unwrap()
    }

    /// Reports `positions` for one playback of `id`, as a player does
    /// every 10 s.
    fn play(session: &dyn Session, id: &MediaId, positions: impl IntoIterator<Item = u64>) {
        let playback = ready(session.start_playback(id, None, Default::default())).unwrap();
        for at in positions {
            ready(session.update_progress(&playback.handle, Duration::from_secs(at))).unwrap();
        }
    }

    #[test]
    fn recently_played_is_the_counted_plays_newest_first() {
        let tracks = Kinds {
            tracks: true,
            ..Kinds::default()
        };
        let (items, total) = ready(andrey().recently_played(Some(tracks), 0, 50)).unwrap();
        assert_eq!(
            (titles(&items), total),
            (vec!["Harbour Lights", "Undertow", "Ebb"], 3)
        );
    }

    #[test]
    fn recently_played_items_carry_their_play_counts() {
        let (items, _) = ready(andrey().recently_played(None, 0, 1)).unwrap();
        let Item::Track(harbour) = &items[0] else {
            panic!("{items:?}")
        };
        assert_eq!(harbour.playback.play_count, 5);
    }

    #[test]
    fn a_track_played_past_half_its_length_counts_once() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        // Salt Lines is 3:58: its bar is 1:59.
        let salt_lines = media(MediaKind::Track, "41");
        play(session.as_ref(), &salt_lines, (1..=11).map(|n| n * 10));
        assert_eq!(track_state(session.as_ref(), "41").play_count, 0);
        play(session.as_ref(), &salt_lines, (1..=20).map(|n| n * 10));
        assert_eq!(track_state(session.as_ref(), "41").play_count, 1);
    }

    #[test]
    fn a_seek_to_a_tracks_end_is_not_a_play() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        play(
            session.as_ref(),
            &media(MediaKind::Track, "41"),
            [10, 20, 200, 210, 220, 230],
        );
        assert_eq!(track_state(session.as_ref(), "41").play_count, 0);
    }

    #[test]
    fn a_progress_report_marks_the_item_viewed_now() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let before = unix_now();
        play(session.as_ref(), &media(MediaKind::Track, "41"), [10]);
        let viewed = track_state(session.as_ref(), "41").last_viewed_at;
        assert!(viewed.is_some_and(|at| at >= before), "{viewed:?}");
    }

    #[test]
    fn without_play_history_nothing_is_listed_or_counted() {
        let client = FakeClient::new(None).with_features(Features {
            play_history: false,
            ..Features::default()
        });
        let session = session_for(&client, "andrey");
        assert_eq!(
            (
                ready(session.recently_played(None, 0, 50)),
                track_state(session.as_ref(), "42").play_count
            ),
            (
                Err(BackendError::Unsupported("spoolframe:play-history".into())),
                0
            )
        );
    }
}

/// Playlist covers (PX-53) on the fake server.
#[cfg(test)]
mod cover_tests {
    use super::*;
    use crate::backend::{MAX_ART_BYTES, PlaylistChange};
    use crate::testing::ready;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// `username`'s session, unlocked (Alice has a PIN).
    fn session_for(client: &FakeClient, username: &str) -> Arc<dyn Session> {
        let session = client.session(&FakeClient::profile(username)).unwrap();
        if username == "alice" {
            ready(session.unlock(FAKE_PIN)).unwrap();
        }
        session
    }

    fn named(session: &dyn Session, name: &str) -> PlaylistSummary {
        ready(session.playlists())
            .unwrap()
            .playlists
            .into_iter()
            .find(|p| p.name == name)
            .unwrap()
    }

    #[test]
    fn a_playlist_shows_its_mosaic_until_a_cover_is_set() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let before = named(andrey.as_ref(), "Movie night");
        assert_eq!(
            (
                before.artwork.map(|a| a.key().starts_with("mosaic/")),
                before.custom_art
            ),
            (Some(true), false)
        );
    }

    #[test]
    fn a_cover_set_is_the_playlists_artwork_for_everyone() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let id = named(andrey.as_ref(), "Andor: the heists").id;
        let cover = ready(andrey.set_playlist_art(&id, PNG.to_vec())).unwrap();
        let alice = session_for(&client, "alice");
        let seen = named(alice.as_ref(), "Andor: the heists");
        assert_eq!((seen.artwork, seen.custom_art), (Some(cover), true));
    }

    #[test]
    fn a_cover_changes_the_revision_and_is_told() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let before = named(andrey.as_ref(), "Movie night");
        let mut events = client.playlist_events();
        ready(andrey.set_playlist_art(&before.id, PNG.to_vec())).unwrap();
        let after = named(andrey.as_ref(), "Movie night").revision;
        assert_eq!(
            events.try_recv().unwrap().change,
            PlaylistChange::Changed {
                id: before.id,
                revision: after
            }
        );
    }

    #[test]
    fn a_removed_cover_gives_the_mosaic_back() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let id = named(andrey.as_ref(), "Movie night").id;
        ready(andrey.set_playlist_art(&id, PNG.to_vec())).unwrap();
        let after = ready(andrey.remove_playlist_art(&id)).unwrap();
        assert_eq!(
            (
                after.artwork.map(|a| a.key().starts_with("mosaic/")),
                after.custom_art
            ),
            (Some(true), false)
        );
    }

    #[test]
    fn someone_who_only_reads_a_playlist_cannot_change_its_cover() {
        let client = FakeClient::new(None);
        let alice = session_for(&client, "alice");
        let id = named(alice.as_ref(), "Andor: the heists").id;
        assert_eq!(
            ready(alice.set_playlist_art(&id, PNG.to_vec())),
            Err(BackendError::NotAllowed)
        );
    }

    #[test]
    fn liked_keeps_its_own_tile() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let liked = named(andrey.as_ref(), "Liked");
        assert_eq!(
            (
                liked.artwork.as_ref().map(|a| a.key().to_owned()),
                ready(andrey.set_playlist_art(&liked.id, PNG.to_vec()))
            ),
            (
                Some("tile/liked/1".to_owned()),
                Err(BackendError::NotAllowed)
            )
        );
    }

    #[test]
    fn a_picture_past_the_limit_is_refused() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let id = named(andrey.as_ref(), "Movie night").id;
        let mut big = PNG.to_vec();
        big.resize(MAX_ART_BYTES as usize + 1, 0);
        assert!(matches!(
            ready(andrey.set_playlist_art(&id, big)),
            Err(BackendError::ImageTooLarge { .. })
        ));
    }

    #[test]
    fn something_that_is_no_picture_is_refused() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let id = named(andrey.as_ref(), "Movie night").id;
        assert_eq!(
            ready(andrey.set_playlist_art(&id, b"not a picture".to_vec())),
            Err(BackendError::ImageUnreadable)
        );
    }

    #[test]
    fn without_the_feature_there_are_no_covers() {
        let client = FakeClient::new(None).with_features(Features {
            playlists: true,
            ..Features::default()
        });
        let andrey = session_for(&client, "andrey");
        let night = named(andrey.as_ref(), "Movie night");
        assert_eq!(
            (
                night.artwork,
                ready(andrey.set_playlist_art(&night.id, PNG.to_vec()))
            ),
            (
                None,
                Err(BackendError::Unsupported("spoolframe:playlist-art".into()))
            )
        );
    }

    #[test]
    fn an_items_playlists_carry_their_covers() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let id = named(andrey.as_ref(), "Movie night").id;
        let cover = ready(andrey.set_playlist_art(&id, PNG.to_vec())).unwrap();
        let alien = media(MediaKind::Movie, "1");
        let refs = ready(andrey.item_detail(&alien)).unwrap().playlists;
        let night = refs.into_iter().find(|r| r.id == id).unwrap();
        assert_eq!(night.artwork, Some(cover));
    }
}

/// Subtitle search and download (PX-02) on the fake server.
#[cfg(test)]
mod subtitle_search_tests {
    use super::*;
    use crate::backend::{QuotaOf, SubtitleSourceState, SubtitleSourceStatus};
    use crate::testing::{file_with, ready};

    fn session_for(client: &FakeClient, username: &str) -> Arc<dyn Session> {
        let session = client.session(&FakeClient::profile(username)).unwrap();
        if username == "alice" {
            ready(session.unlock(FAKE_PIN)).unwrap();
        }
        session
    }

    fn alien() -> MediaId {
        media(MediaKind::Movie, "1")
    }

    fn found(session: &dyn Session) -> SubtitleSearch {
        ready(session.search_subtitles(&alien(), None)).unwrap()
    }

    #[test]
    fn a_search_lists_a_match_for_this_file_first_and_a_rejected_one_last() {
        let andrey = session_for(&FakeClient::new(None), "andrey");
        let results = found(andrey.as_ref()).results;
        assert_eq!(
            (
                results.first().map(|r| r.hash_match),
                results.last().map(|r| r.rejected)
            ),
            (Some(true), Some(true))
        );
    }

    #[test]
    fn a_search_says_how_each_site_went() {
        let andrey = session_for(&FakeClient::new(None), "andrey");
        assert_eq!(
            found(andrey.as_ref()).sources,
            [
                SubtitleSourceStatus {
                    source: "opensubtitles".into(),
                    state: SubtitleSourceState::Found { found: 3 }
                },
                SubtitleSourceStatus {
                    source: "subdl".into(),
                    state: SubtitleSourceState::Found { found: 1 }
                },
            ]
        );
    }

    #[test]
    fn one_site_down_leaves_the_others_results() {
        let client = FakeClient::new(None).with_subtitle_sites(Sites::SubDlDown);
        let search = found(session_for(&client, "andrey").as_ref());
        assert_eq!(
            (
                search.results.iter().all(|r| r.source == "opensubtitles"),
                matches!(search.sources[1].state, SubtitleSourceState::Failed { .. })
            ),
            (true, true)
        );
    }

    #[test]
    fn a_language_nobody_made_subtitles_in_finds_none() {
        let andrey = session_for(&FakeClient::new(None), "andrey");
        let search = ready(andrey.search_subtitles(&alien(), Some("sv"))).unwrap();
        assert_eq!(
            (search.languages, search.results.len()),
            (vec!["sv".to_owned()], 0)
        );
    }

    #[test]
    fn a_server_without_a_site_says_so() {
        let client = FakeClient::new(None).with_subtitle_sites(Sites::NoSource);
        let andrey = session_for(&client, "andrey");
        assert_eq!(
            ready(andrey.search_subtitles(&alien(), None)),
            Err(BackendError::NoSubtitleSource)
        );
    }

    #[test]
    fn a_download_is_a_track_of_the_item_for_everyone() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let result = found(andrey.as_ref()).results[0].id.clone();
        let got = ready(andrey.download_subtitle(&alien(), &result)).unwrap();
        let alice = session_for(&client, "alice");
        let tracks = ready(alice.tracks(&alien())).unwrap().subtitles;
        let track = &tracks[got.track as usize];
        assert_eq!(
            (
                track.title.as_deref(),
                track
                    .download
                    .as_ref()
                    .map(|d| (d.by.as_str(), d.can_remove))
            ),
            (Some("OpenSubtitles"), Some(("Andrey", false)))
        );
    }

    #[test]
    fn the_same_result_downloaded_again_is_the_same_track() {
        let andrey = session_for(&FakeClient::new(None), "andrey");
        let result = found(andrey.as_ref()).results[0].id.clone();
        let first = ready(andrey.download_subtitle(&alien(), &result)).unwrap();
        let again = ready(andrey.download_subtitle(&alien(), &result)).unwrap();
        assert_eq!(first.track, again.track);
    }

    #[test]
    fn a_downloaded_result_says_so_in_the_next_search() {
        let andrey = session_for(&FakeClient::new(None), "andrey");
        let result = found(andrey.as_ref()).results[1].id.clone();
        ready(andrey.download_subtitle(&alien(), &result)).unwrap();
        assert!(found(andrey.as_ref()).results[1].downloaded);
    }

    #[test]
    fn only_its_downloader_removes_a_download() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let result = found(andrey.as_ref()).results[0].id.clone();
        let got = ready(andrey.download_subtitle(&alien(), &result)).unwrap();
        let download = got.subtitle.download.unwrap().id;
        let alice = session_for(&client, "alice");
        assert_eq!(
            ready(alice.remove_subtitle_download(&alien(), &download)),
            Err(BackendError::NotAllowed)
        );
        ready(andrey.remove_subtitle_download(&alien(), &download)).unwrap();
        let tracks = ready(andrey.tracks(&alien())).unwrap().subtitles;
        assert_eq!(tracks.len(), OWN_SUBTITLES);
    }

    #[test]
    fn a_used_up_quota_refuses_the_download_but_still_searches() {
        let client = FakeClient::new(None).with_subtitle_sites(Sites::QuotaSpent);
        let andrey = session_for(&client, "andrey");
        let search = found(andrey.as_ref());
        assert_eq!(
            ready(andrey.download_subtitle(&alien(), &search.results[0].id)),
            Err(BackendError::QuotaExhausted {
                whose: QuotaOf::Account,
                site: Some("opensubtitles".into()),
                resets_at: Some(QUOTA_RESETS_AT),
            })
        );
    }

    #[test]
    fn without_the_feature_nothing_is_searched_and_no_track_says_where_it_came_from() {
        let full = FakeClient::new(None);
        let andrey = session_for(&full, "andrey");
        let result = found(andrey.as_ref()).results[0].id.clone();
        ready(andrey.download_subtitle(&alien(), &result)).unwrap();
        // The same server, seen by an app that did not list the feature.
        let client = FakeClient::new(None).with_features(Features {
            playlists: true,
            ..Features::default()
        });
        let off = session_for(&client, "andrey");
        assert_eq!(
            ready(off.search_subtitles(&alien(), None)),
            Err(BackendError::Unsupported(
                "spoolframe:subtitle-search".into()
            ))
        );
    }

    #[test]
    fn a_downloaded_track_plays_as_an_external_subtitle() {
        let (_dir, path) = file_with("subtitle-download", &[0; 4096]);
        let client = FakeClient::new(Some(path));
        let andrey = session_for(&client, "andrey");
        let result = found(andrey.as_ref()).results[1].id.clone();
        let got = ready(andrey.download_subtitle(&alien(), &result)).unwrap();
        let selection = Selection {
            audio: None,
            subtitle: Some(got.track),
        };
        let started =
            ready(andrey.start_playback(&alien(), Some(selection), Default::default())).unwrap();
        let (_, bytes) = ready(andrey.read_subtitle(&started.handle)).unwrap();
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .contains("downloaded from SubDL")
        );
    }
}
