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
    APP_RP_ID, FinishPasskeyRegistration, FinishPasskeySignIn, PasskeyCreation, PasskeyInfo,
    PasskeyRequest,
};
use crate::backend::{
    ArtworkSize, BackendError, BoxFuture, ChosenTracks, Chunk, Client, ConnectionPath,
    ConnectionState, DeviceInfo, DeviceSignIn, DeviceSignInRequest, HomeScreen, MAX_READ, Marker,
    Person, PersonId, PersonRole, PinProof, PinState, Platform, PlaybackHandle, PlaybackPlan,
    PlaybackStart, PlexLink, Previews, Profile, ProfileSignIn, SearchCounts, SearchGroup,
    SearchKind, SearchRequest, SearchResults, SearchScope, Selection, Server, ServerKey, Session,
    TopResult,
};

/// Every fake account's password.
pub const FAKE_PASSWORD: &str = "spoolframe";

/// What the fake server asks a passkey to sign (P3-19).
pub const FAKE_PASSKEY_CHALLENGE: [u8; 32] = [7; 32];

/// The passkeys the fake server keeps, for every session of the device.
#[derive(Default)]
struct FakePasskeys {
    keys: Vec<FakePasskey>,
    /// Set by [`FakeClient::age_sign_ins`]: every sign-in is older than
    /// the 10 minutes adding or removing a passkey allows.
    stale: bool,
    /// Who last signed in here with a passkey, as the SDK remembers it.
    by_passkey: std::collections::HashSet<String>,
}

struct FakePasskey {
    username: String,
    credential_id: Vec<u8>,
    name: String,
    created_at: i64,
    last_used_at: Option<i64>,
    /// Made in an app, for APP_RP_ID; a web UI's otherwise.
    app: bool,
}

impl FakePasskey {
    /// The id the server names a passkey by. The real one is base64url;
    /// any stable text does here.
    fn id(&self) -> String {
        self.credential_id
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn info(&self) -> PasskeyInfo {
        PasskeyInfo {
            id: self.id(),
            name: self.name.clone(),
            created_at: self.created_at,
            last_used_at: self.last_used_at,
            rp_id: if self.app {
                APP_RP_ID.to_owned()
            } else {
                "media.example.org".to_owned()
            },
            app: self.app,
            usable_here: self.app,
        }
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
}

impl WatchState {
    fn get(&self, id: &MediaId) -> PlaybackState {
        self.states.get(id).cloned().unwrap_or_default()
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
}

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
    playbacks: Mutex<HashMap<PlaybackHandle, Playback>>,
    next_handle: AtomicU64,
    preferences: Mutex<TrackPreferences>,
    /// Tracks remembered per movie, or per show for its episodes.
    overrides: Mutex<HashMap<MediaId, TrackOverride>>,
    username: String,
    passkeys: Arc<Mutex<FakePasskeys>>,
    pin: Mutex<FakePin>,
    /// Devices waiting to be signed in by code (P3-07), and wrong codes
    /// tried.
    sign_ins: Mutex<SignIns>,
    /// Which profile used the device's connection last: switching to
    /// another and back locks a profile again (P2-08).
    active: Arc<Mutex<std::collections::HashSet<String>>>,
}

/// Device sign-ins waiting for this user's approval (P3-07).
struct SignIns {
    pending: HashMap<String, DeviceSignInRequest>,
    wrong: u32,
}

impl Default for SignIns {
    fn default() -> Self {
        let request = DeviceSignInRequest {
            device_name: Some("Living Room Apple TV".into()),
            client_name: "spoolframe-tvos".into(),
            requested_at: unix_now() - 30,
        };
        Self {
            pending: [(FAKE_SIGN_IN_CODE.to_owned(), request)].into(),
            wrong: 0,
        }
    }
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
        passkeys: Arc<Mutex<FakePasskeys>>,
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
        let session = FakeSession {
            catalog,
            watch: Mutex::new(WatchState::default()),
            serving,
            subtitle_asks: AtomicU32::new(0),
            preview_asks: AtomicU32::new(0),
            gone,
            reports: Mutex::new(Vec::new()),
            playbacks: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            preferences: Mutex::new(TrackPreferences::default()),
            overrides: Mutex::new(HashMap::new()),
            username: username.to_owned(),
            passkeys,
            pin: Mutex::new(pin),
            sign_ins: Mutex::new(SignIns::default()),
            active,
        };
        {
            let mut watch = session.watch();
            let watched = || PlaybackState {
                position: None,
                watched: true,
            };
            let at = |secs: u64| PlaybackState {
                position: Some(Duration::from_secs(secs)),
                watched: false,
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
            Item::Movie(m) => m.playback = watch.get(&m.id),
            Item::Episode(e) => e.playback = watch.get(&e.id),
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
    fn hide_key(&self, id: &MediaId) -> MediaId {
        match self.catalog.items.get(id) {
            Some(Item::Episode(e)) => e.show_id.clone(),
            _ => id.clone(),
        }
    }

    /// Progress or a mark on `id` brings it back, as on the server.
    fn unhide(&self, id: &MediaId) {
        let key = self.hide_key(id);
        self.watch().hidden.remove(&key);
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
        // unwatched episode after the latest watched one, unless it is
        // already in Continue watching.
        let mut next_up = Vec::new();
        let mut shows_seen = Vec::new();
        for id in watch.touched.iter().rev() {
            let Some(Item::Episode(e)) = self.catalog.items.get(id) else {
                continue;
            };
            if shows_seen.contains(&e.show_id) {
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
            if let Some(next) = next
                && !continue_ids.contains(next)
            {
                next_up.push(next.clone());
            }
        }
        let hidden = watch.hidden.clone();
        drop(watch);
        let shown = |id: &MediaId| !hidden.contains(&self.hide_key(id));
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
        };
        match item {
            Item::Track(_) => MediaTracks {
                duration: length_of(item),
                container: Some(Container::Flac),
                video: None,
                audio: vec![audio(None, 2, true, AudioRole::Main)],
                subtitles: Vec::new(),
            },
            _ => MediaTracks {
                duration: length_of(item),
                container: Some(Container::Matroska),
                video: Some(VideoTrack {
                    codec: VideoCodec::Hevc,
                    width: 3840,
                    height: 2160,
                    bit_depth: 10,
                    hdr: Some(HdrFormat::Hdr10),
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
                subtitles: vec![
                    subtitle(None, SubtitleRole::Full, true),
                    subtitle(Some("Forced"), SubtitleRole::Forced, false),
                    subtitle(Some("SDH"), SubtitleRole::Sdh, false),
                ],
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

    fn with_playback<T>(
        &self,
        handle: &PlaybackHandle,
        run: impl FnOnce(&mut Playback) -> T,
    ) -> Result<T, BackendError> {
        let mut playbacks = self.playbacks.lock().expect("fake state is never poisoned");
        let playback = playbacks
            .get_mut(handle)
            .ok_or(BackendError::PlaybackExpired)?;
        if playback.last_used.elapsed() > HANDLE_IDLE {
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
        _start: Option<Duration>,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>> {
        Box::pin(async move {
            self.catalog()?;
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
                    },
                );
            let (plan, size) = match self.serving.converting {
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
            self.playback(handle)?;
            self.file_bytes(offset, length)
        })
    }

    fn read_subtitle<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(SubtitleFormat, Vec<u8>), BackendError>> {
        Box::pin(async move {
            let (_, selection) = self.playback(handle)?;
            if let Some(waits) = self.serving.reading_subtitle {
                if self.subtitle_asks.fetch_add(1, Ordering::SeqCst) < waits {
                    return Err(BackendError::SubtitleNotReady(50));
                }
            } else if selection.subtitle != Some(0) {
                return Err(BackendError::NotFound(
                    "an external subtitle in this playback".into(),
                ));
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
            if id.namespace() == Namespace::Preview {
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
            // Music has no watch state in Phase 1 (P1-14).
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
                },
            );
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
            let key = self.hide_key(id);
            self.watch().hidden.insert(key);
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
            self.watch().set(id, PlaybackState::default());
            self.unhide(id);
            Ok(())
        })
    }
    fn track_preferences(&self) -> BoxFuture<'_, Result<TrackPreferences, BackendError>> {
        Box::pin(async move {
            Ok(self
                .preferences
                .lock()
                .expect("fake state is never poisoned")
                .clone())
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

    fn begin_passkey_registration(&self) -> BoxFuture<'_, Result<PasskeyCreation, BackendError>> {
        Box::pin(async move {
            self.guard()?;
            let passkeys = self.lock_passkeys();
            if passkeys.stale {
                return Err(BackendError::RecentSignInRequired);
            }
            Ok(PasskeyCreation {
                challenge: FAKE_PASSKEY_CHALLENGE.to_vec(),
                user_id: self.username.as_bytes().to_vec(),
                user_name: format!("{} on Home Media", self.username),
                display_name: format!("{} on Home Media", self.username),
                exclude_credential_ids: passkeys
                    .keys
                    .iter()
                    .filter(|k| k.app && k.username == self.username)
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
                app: true,
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
            let device = |id: &str, name: &str, ago: i64| DeviceInfo {
                endpoint_id: id.to_owned(),
                name: Some(name.to_owned()),
                last_seen_at: 1_790_000_000 - ago,
                pin_required: pin.required.contains(id),
                this_device: id == THIS_DEVICE,
                platform: Some(if id == THIS_DEVICE {
                    Platform::Linux
                } else {
                    Platform::Tvos
                }),
            };
            Ok(vec![
                device(THIS_DEVICE, "This computer", 0),
                device(LIVING_ROOM_TV, "Living room TV", 3 * 86_400),
            ])
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
            let device = device.unwrap_or(THIS_DEVICE).to_owned();
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
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.guard()?;
            if code == FAKE_OWN_SIGN_IN_CODE {
                return Err(BackendError::InvalidInput(
                    "this device cannot approve its own sign-in".into(),
                ));
            }
            let mut sign_ins = self.sign_ins.lock().expect("fake state is never poisoned");
            sign_ins
                .pending
                .remove(code)
                .map(|_| ())
                .ok_or_else(|| BackendError::NotFound(format!("sign-in code {code}")))
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
    fn lock_passkeys(&self) -> std::sync::MutexGuard<'_, FakePasskeys> {
        self.passkeys.lock().expect("fake state is never poisoned")
    }

    fn lock_overrides(&self) -> std::sync::MutexGuard<'_, HashMap<MediaId, TrackOverride>> {
        self.overrides.lock().expect("fake state is never poisoned")
    }
}

/// The servers and users the mockups show.
pub struct FakeClient {
    catalog: Arc<Catalog>,
    serving: Serving,
    servers: Mutex<Vec<Server>>,
    /// Accounts each server has: (server, username, display name).
    accounts: Vec<(ServerKey, &'static str, &'static str)>,
    /// Who is signed in on this device, in sign-in order.
    signed_in: Mutex<Vec<Profile>>,
    /// One session per profile, kept so watch state survives switching.
    sessions: Mutex<HashMap<(ServerKey, String), Arc<FakeSession>>>,
    failed_sign_ins: Mutex<(u32, Option<Instant>)>,
    connections: Mutex<HashMap<ServerKey, watch::Sender<ConnectionState>>>,
    /// The fake has no LAN: its list changes only when an address is added.
    servers_changed: watch::Sender<()>,
    gone: Arc<std::sync::atomic::AtomicBool>,
    /// The profile that used the connection last (P2-08).
    active: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Sign-ins this device asked a phone to approve (P3-05, P3-07).
    phone_sign_ins: Mutex<PhoneSignIns>,
    passkeys: Arc<Mutex<FakePasskeys>>,
}

/// How the fake answers a sign-in a phone approves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneAnswer {
    /// Approved by this account's user.
    Approve(&'static str),
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
        };
        FakeClient {
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
            },
            gone: Arc::default(),
            active: Arc::default(),
            phone_sign_ins: Mutex::new(PhoneSignIns {
                answer: PhoneAnswer::Approve("andrey"),
                web: true,
                pending: 0,
            }),
            servers: Mutex::new(vec![
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
                },
            ]),
            accounts: vec![
                (home_media(), "andrey", "Andrey"),
                (home_media(), "alice", "Alice"),
                (home_media(), "steve", "Steve"),
            ],
            signed_in: Mutex::new(vec![profile("andrey", "Andrey"), profile("alice", "Alice")]),
            sessions: Mutex::new(HashMap::new()),
            // Andrey made one in the web UI: listed, but no use in an app.
            passkeys: Arc::new(Mutex::new(FakePasskeys {
                keys: vec![FakePasskey {
                    username: "andrey".into(),
                    credential_id: vec![1, 2, 3, 4],
                    name: "Firefox on nimble".into(),
                    created_at: 1_790_000_000,
                    last_used_at: Some(1_790_500_000),
                    app: false,
                }],
                stale: false,
                by_passkey: Default::default(),
            })),
            failed_sign_ins: Mutex::new((0, None)),
            connections: Mutex::new(HashMap::new()),
            servers_changed: watch::Sender::new(()),
        }
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

    /// Every sign-in on this device is now older than the 10 minutes
    /// adding or removing a passkey allows.
    pub fn age_sign_ins(&self) {
        self.passkeys
            .lock()
            .expect("fake state is never poisoned")
            .stale = true;
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
                ))
            })
            .clone()
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

    /// Basement shows on the network but does not answer: the fixture for
    /// an unreachable server. So does any server added by hand, which the
    /// fake has no accounts for.
    fn reach(&self, key: &ServerKey) -> Result<Server, BackendError> {
        let servers = self.servers.lock().expect("fake state is never poisoned");
        let server = servers
            .iter()
            .find(|s| &s.key == key)
            .ok_or_else(|| BackendError::NotFound(key.short()))?;
        if server.key != home_media() {
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
        let Some((_, username, display_name)) = self
            .accounts
            .iter()
            .find(|(s, u, _)| s == server && *u == username)
            .filter(|_| password == FAKE_PASSWORD)
        else {
            failed.0 += 1;
            if failed.0 >= SIGN_IN_ATTEMPTS {
                failed.1 = Some(Instant::now() + SIGN_IN_BACKOFF);
            }
            return Err(BackendError::WrongCredentials);
        };
        *failed = (0, None);
        drop(failed);
        Ok(self.signed_in_as(server, username, display_name))
    }

    /// `username` signed in from this device, as a password or a phone's
    /// approval does.
    fn signed_in_as(&self, server: &ServerKey, username: &str, display_name: &str) -> Profile {
        // A sign-in is fresh: adding and removing passkeys works again.
        self.passkeys
            .lock()
            .expect("fake state is never poisoned")
            .stale = false;
        let profile = Profile {
            server: server.clone(),
            username: (*username).into(),
            display_name: (*display_name).into(),
            locked: false,
            sign_in: Default::default(),
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
            self.servers
                .lock()
                .expect("fake state is never poisoned")
                .retain(|s| &s.key != server);
            self.signed_in
                .lock()
                .expect("fake state is never poisoned")
                .retain(|p| &p.server != server);
            self.servers_changed.send_replace(());
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
            Ok(signed_in
                .into_iter()
                .filter(|p| &p.server == server)
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
                .collect())
        })
    }

    fn sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        username: &'a str,
        password: &'a str,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move { self.sign_in_now(server, username, password) })
    }

    fn begin_passkey_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        username: Option<&'a str>,
    ) -> BoxFuture<'a, Result<PasskeyRequest, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            let passkeys = self.passkeys.lock().expect("fake state is never poisoned");
            let allow = match username {
                Some(username) => passkeys
                    .keys
                    .iter()
                    .filter(|k| k.app && k.username == username)
                    .map(|k| k.credential_id.clone())
                    .collect(),
                None => Vec::new(),
            };
            Ok(PasskeyRequest {
                challenge: FAKE_PASSKEY_CHALLENGE.to_vec(),
                allow_credential_ids: allow,
            })
        })
    }

    fn finish_passkey_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
        signed: FinishPasskeySignIn,
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            self.pinned(server)?;
            let username = {
                let mut passkeys = self.passkeys.lock().expect("fake state is never poisoned");
                let key = passkeys
                    .keys
                    .iter_mut()
                    .find(|k| k.app && k.credential_id == signed.credential_id)
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
            Ok(self.signed_in_as(server, username, display_name))
        })
    }

    fn begin_device_sign_in<'a>(
        &'a self,
        server: &'a ServerKey,
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
    ) -> BoxFuture<'a, Result<Profile, BackendError>> {
        Box::pin(async move {
            let answer = self.phone_sign_ins().answer;
            if answer == PhoneAnswer::Wait {
                return std::future::pending().await;
            }
            self.phone_sign_ins().pending -= 1;
            match answer {
                PhoneAnswer::Approve(username) => {
                    let (_, username, display_name) = self
                        .accounts
                        .iter()
                        .find(|(s, u, _)| *s == sign_in.server && *u == username)
                        .ok_or_else(|| BackendError::NotFound(username.into()))?;
                    Ok(self.signed_in_as(&sign_in.server, username, display_name))
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
        let people = || -> Vec<Person> {
            if within.is_some() {
                return Vec::new();
            }
            catalog
                .people
                .iter()
                .filter(|(p, _)| matches(&text, &p.name))
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
        let playback = ready(session.start_playback(&heat, None, None)).unwrap();
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
    fn progress_brings_a_hidden_item_back() {
        let (_dir, path) = file_with("progress", b"stand-in");
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let first = ready(session.home()).unwrap().continue_watching[0].clone();
        ready(session.hide_from_continue(first.id())).unwrap();
        let playback = ready(session.start_playback(first.id(), None, None)).unwrap();
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
        let playback = ready(session.start_playback(&heat, None, None)).unwrap();
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
        let playback = ready(session.start_playback(&alien, None, None)).unwrap();
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
        assert_eq!(m.playback, PlaybackState::default());
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
    fn a_library_search_finds_only_its_items_and_no_people() {
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
        let error = ready(andrey().start_playback(&alien, None, None)).unwrap_err();
        assert!(matches!(error, BackendError::Unavailable(m) if m.contains("--fake-media")));
    }

    #[test]
    fn reads_return_the_requested_range_of_the_file() {
        let bytes: Vec<u8> = (0..=255).collect();
        let (_dir, path) = file_with("fake", &bytes);
        let client = FakeClient::new(Some(path));
        let session = session_for(&client, "andrey");
        let alien = media(MediaKind::Movie, "1");
        let start = ready(session.start_playback(&alien, None, None)).unwrap();
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
        let start = ready(session.start_playback(&alien, Some(selection), None)).unwrap();
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
        ready(client.sign_in(&home_media(), "steve", FAKE_PASSWORD)).unwrap();
        let profiles = ready(client.profiles(&home_media())).unwrap();
        assert_eq!(usernames(&profiles), ["andrey", "alice", "steve"]);
    }

    #[test]
    fn an_unknown_user_gets_the_same_refusal_as_a_wrong_password() {
        let client = FakeClient::new(None);
        assert_eq!(
            ready(client.sign_in(&home_media(), "mallory", FAKE_PASSWORD)),
            Err(BackendError::WrongCredentials)
        );
    }

    #[test]
    fn repeated_failures_are_rate_limited() {
        let client = FakeClient::new(None);
        for _ in 0..SIGN_IN_ATTEMPTS {
            let _ = ready(client.sign_in(&home_media(), "steve", "hunter2"));
        }
        let refused = ready(client.sign_in(&home_media(), "steve", FAKE_PASSWORD));
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
        ready(client.sign_in(&key, "andrey", FAKE_PASSWORD)).unwrap();
        ready(client.forget(&key)).unwrap();
        let servers = ready(client.servers()).unwrap();
        assert!(!servers.iter().any(|s| s.key == key));
    }

    #[test]
    fn signing_out_keeps_the_server_but_no_profiles() {
        let client = FakeClient::new(None);
        let key = home_media();
        ready(client.sign_in(&key, "andrey", FAKE_PASSWORD)).unwrap();
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
            ready(client.sign_in(&key, "andrey", FAKE_PASSWORD)),
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
        assert!(ready(client.sign_in(&key, "andrey", FAKE_PASSWORD)).is_ok());
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
        let begun = ready(client.begin_device_sign_in(&home_media())).unwrap();
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
        let begun = ready(client.begin_device_sign_in(&home_media())).unwrap();
        assert_eq!(begun.web_url, None);
    }

    #[test]
    fn an_approved_phone_sign_in_adds_the_profile() {
        let client = FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Approve("steve"));
        let begun = ready(client.begin_device_sign_in(&home_media())).unwrap();
        let profile = ready(client.await_device_sign_in(&begun)).unwrap();
        assert!(usernames(&ready(client.profiles(&home_media())).unwrap()).contains(&"steve"));
        assert_eq!(profile.username, "steve");
    }

    #[test]
    fn a_refused_phone_sign_in_says_so() {
        let client = FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Deny);
        let begun = ready(client.begin_device_sign_in(&home_media())).unwrap();
        assert_eq!(
            ready(client.await_device_sign_in(&begun)),
            Err(BackendError::SignInDenied)
        );
    }

    #[test]
    fn a_fourth_pending_phone_sign_in_is_refused() {
        let client = FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Wait);
        for _ in 0..3 {
            ready(client.begin_device_sign_in(&home_media())).unwrap();
        }
        assert!(matches!(
            ready(client.begin_device_sign_in(&home_media())),
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
        let profile = ready(client.sign_in(&home_media(), "steve", FAKE_PASSWORD)).unwrap();
        let session = client.session(&profile).unwrap();
        assert_eq!(
            [
                ready(session.libraries()).map(|_| ()),
                ready(session.home()).map(|_| ())
            ],
            [Err(BackendError::NotSetUp), Err(BackendError::NotSetUp)]
        );
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
        let mut preferences = ready(session.track_preferences()).unwrap();
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
        };
        let session = client.session(&profile).unwrap();
        let start =
            ready(session.start_playback(&media(MediaKind::Movie, "1"), None, None)).unwrap();
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
            user_handle: b"andrey".to_vec(),
        }
    }

    #[test]
    fn a_passkey_made_in_the_app_signs_its_user_in() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        ready(andrey.begin_passkey_registration()).unwrap();
        ready(andrey.finish_passkey_registration(made(&[9, 9], "YubiKey 5C"))).unwrap();
        let profile = ready(client.finish_passkey_sign_in(&home_media(), signed(&[9, 9]))).unwrap();
        assert_eq!(profile.username, "andrey");
    }

    #[test]
    fn a_web_ui_passkey_is_listed_but_does_not_sign_in_here() {
        let client = FakeClient::new(None);
        let andrey = session_for(&client, "andrey");
        let listed = ready(andrey.passkeys()).unwrap();
        assert_eq!(
            (
                listed.iter().map(|p| p.usable_here).collect::<Vec<_>>(),
                ready(client.finish_passkey_sign_in(&home_media(), signed(&[1, 2, 3, 4])))
                    .unwrap_err(),
            ),
            (vec![false], BackendError::WrongCredentials)
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
        signed.user_handle = b"alice".to_vec();
        ready(client.finish_passkey_sign_in(&home_media(), signed)).unwrap();
        let alice = session_for(&client, "alice");
        for _ in 0..10 {
            let _ = ready(alice.unlock("0000"));
        }
        assert_eq!(profile_of(&client, "alice").sign_in, ProfileSignIn::Passkey);
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
    fn an_approved_code_is_used_up() {
        let andrey = andrey();
        ready(andrey.approve_device_sign_in(FAKE_SIGN_IN_CODE)).unwrap();
        assert!(matches!(
            ready(andrey.lookup_device_sign_in(FAKE_SIGN_IN_CODE)),
            Err(BackendError::NotFound(_))
        ));
    }

    #[test]
    fn approving_this_devices_own_code_is_refused() {
        assert!(matches!(
            ready(andrey().approve_device_sign_in(FAKE_OWN_SIGN_IN_CODE)),
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
            ready(alice.approve_device_sign_in(FAKE_SIGN_IN_CODE)).unwrap_err(),
            BackendError::ProfileLocked
        );
    }
}
