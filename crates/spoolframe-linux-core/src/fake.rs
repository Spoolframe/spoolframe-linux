//! A client and sessions that answer from fixed data, so the UI can be
//! built and tried without a server. The catalog is the content of the UI
//! mockups. Watch state, playback handles and sign-in limits behave like
//! the server's (P1-08, P1-11, P1-14), so the UI meets the same cases.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spoolframe_protocol::{PlannedAudio, PlannedSubtitle, SubtitleDelivery};
use spoolframe_types::language::same_language;
use spoolframe_types::{
    Album, Artist, ArtworkId, AudioCodec, AudioRole, AudioTrack, Container, Episode, Item, Library,
    LibraryId, LibraryKind, MediaId, MediaKind, MediaTracks, Movie, Namespace, PlaybackState,
    SdhPreference, Season, Show, SubtitleFormat, SubtitleMode, SubtitleRole, SubtitleTrack, Track,
    TrackPreferences, VideoCodec,
};
use tokio::sync::watch;

use crate::backend::{
    ArtworkSize, BackendError, BoxFuture, ChosenTracks, Client, ConnectionState, HomeScreen,
    MAX_READ, PlaybackHandle, PlaybackPlan, PlaybackStart, Profile, Selection, Server, ServerKey,
    Session, TrackReason,
};

/// Every fake account's password.
pub const FAKE_PASSWORD: &str = "spoolframe";

/// Watch-state rules, the server's defaults (P1-14).
const STARTED_AFTER: Duration = Duration::from_secs(60);
const WATCHED_FRACTION: f64 = 0.9;
const WATCHED_WHEN_LEFT: Duration = Duration::from_secs(5 * 60);
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
}

impl Catalog {
    fn new() -> Self {
        let mut catalog = Catalog {
            libraries: Vec::new(),
            items: HashMap::new(),
            children: HashMap::new(),
            order: Vec::new(),
            recently_added: Vec::new(),
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
        let season_id = media(MediaKind::Season, &(base + 1).to_string());
        self.add(Item::Show(Show {
            id: show.clone(),
            title: title.into(),
            year: Some(year),
            summary: None,
            artwork: Vec::new(),
        }));
        self.add(Item::Season(Season {
            id: season_id.clone(),
            show_id: show.clone(),
            show_title: Some(title.into()),
            season,
            title: format!("Season {season}"),
            artwork: Vec::new(),
        }));
        self.children.insert(show.clone(), vec![season_id.clone()]);
        let mut ids = Vec::new();
        let show_title = title;
        for (i, title) in episodes.iter().enumerate() {
            let id = media(MediaKind::Episode, &(base + 2 + i as u32).to_string());
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
            }));
        }
        self.children.insert(season_id, ids);
        show
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

    fn progress(&mut self, id: &MediaId, length: Option<Duration>, position: Duration) {
        if position < STARTED_AFTER {
            return;
        }
        let watched = length.is_some_and(|length| {
            position.as_secs_f64() >= length.as_secs_f64() * WATCHED_FRACTION
                || length.saturating_sub(position) < WATCHED_WHEN_LEFT
        });
        self.set(
            id,
            PlaybackState {
                position: (!watched).then_some(position),
                watched,
            },
        );
    }
}

struct Playback {
    item: MediaId,
    selection: Selection,
    last_used: Instant,
}

/// One user's view of the fake server.
pub struct FakeSession {
    catalog: Arc<Catalog>,
    watch: Mutex<WatchState>,
    /// The file every playback reads; `None` when the fake has no media.
    media: Option<PathBuf>,
    /// The most one read returns, like a server that answers short.
    read_limit: u64,
    playbacks: Mutex<HashMap<PlaybackHandle, Playback>>,
    next_handle: AtomicU64,
    preferences: Mutex<TrackPreferences>,
    /// Tracks remembered per movie, or per show for its episodes.
    overrides: Mutex<HashMap<MediaId, Selection>>,
}

impl FakeSession {
    fn new(
        catalog: Arc<Catalog>,
        username: &str,
        media_file: Option<PathBuf>,
        read_limit: u64,
    ) -> Self {
        let session = FakeSession {
            catalog,
            watch: Mutex::new(WatchState::default()),
            media: media_file,
            read_limit,
            playbacks: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            preferences: Mutex::new(TrackPreferences::default()),
            overrides: Mutex::new(HashMap::new()),
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
        drop(watch);
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
                audio: vec![audio(None, 2, true, AudioRole::Main)],
                subtitles: Vec::new(),
            },
            _ => MediaTracks {
                duration: length_of(item),
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
    /// user's preferences, simplified from P1-20's chooser.
    fn choose(&self, item: &Item, tracks: &MediaTracks) -> ChosenTracks {
        if let Some(selection) = self
            .overrides
            .lock()
            .expect("fake state is never poisoned")
            .get(&Self::override_key(item))
        {
            return ChosenTracks {
                selection: *selection,
                audio: TrackReason::Override,
                subtitle: TrackReason::Override,
            };
        }
        let preferences = self
            .preferences
            .lock()
            .expect("fake state is never poisoned")
            .clone();

        // Audio: the first track in a preferred language whose role is not
        // avoided; else the file's default.
        let allowed: Vec<usize> = (0..tracks.audio.len())
            .filter(|i| !preferences.audio.avoid.contains(&tracks.audio[*i].role))
            .collect();
        let allowed = if allowed.is_empty() {
            (0..tracks.audio.len()).collect()
        } else {
            allowed
        };
        let default = allowed
            .iter()
            .copied()
            .find(|i| tracks.audio[*i].default)
            .or(allowed.first().copied());
        let by_language = preferences.audio.languages.iter().find_map(|wanted| {
            allowed.iter().copied().find(|i| {
                tracks.audio[*i]
                    .language
                    .as_deref()
                    .is_some_and(|l| same_language(l, wanted))
            })
        });
        let (audio, audio_reason) = match (by_language, default) {
            (Some(i), _) => (Some(i), TrackReason::Preferences),
            (None, Some(i)) => (Some(i), TrackReason::FileDefault),
            (None, None) => (None, TrackReason::NoTracks),
        };
        let audio_language = audio.and_then(|i| tracks.audio[i].language.clone());

        // Subtitles, by mode.
        let in_audio_language = |t: &SubtitleTrack| match (&t.language, &audio_language) {
            (Some(a), Some(b)) => same_language(a, b),
            _ => false,
        };
        let languages = if preferences.subtitles.languages.is_empty() {
            audio_language.iter().cloned().collect()
        } else {
            preferences.subtitles.languages.clone()
        };
        let (subtitle, subtitle_reason) = if tracks.subtitles.is_empty() {
            (None, TrackReason::NoTracks)
        } else {
            match preferences.subtitles.mode {
                SubtitleMode::Off => (None, TrackReason::NotWanted),
                SubtitleMode::ForcedOnly => {
                    match tracks
                        .subtitles
                        .iter()
                        .position(|t| t.forced && in_audio_language(t))
                    {
                        Some(i) => (Some(i), TrackReason::Preferences),
                        None => (None, TrackReason::NoneMatched),
                    }
                }
                SubtitleMode::ForeignOnly
                    if audio_language
                        .as_deref()
                        .is_some_and(|a| languages.iter().any(|l| same_language(l, a))) =>
                {
                    (None, TrackReason::NotWanted)
                }
                SubtitleMode::Always | SubtitleMode::ForeignOnly => {
                    let fits = |t: &SubtitleTrack, lang: &str, sdh: bool| {
                        t.language
                            .as_deref()
                            .is_some_and(|l| same_language(l, lang))
                            && matches!(t.role, SubtitleRole::Full | SubtitleRole::Sdh)
                            && (t.role == SubtitleRole::Sdh) == sdh
                    };
                    let order: &[bool] = match preferences.subtitles.sdh {
                        SdhPreference::Prefer => &[true, false],
                        SdhPreference::Accept => &[false, true],
                        SdhPreference::Avoid => &[false],
                    };
                    let found = languages.iter().find_map(|lang| {
                        order.iter().find_map(|sdh| {
                            tracks.subtitles.iter().position(|t| fits(t, lang, *sdh))
                        })
                    });
                    match found {
                        Some(i) => (Some(i), TrackReason::Preferences),
                        None => (None, TrackReason::NoneMatched),
                    }
                }
            }
        };
        ChosenTracks {
            selection: Selection {
                audio: audio.map(|i| i as u32),
                subtitle: subtitle.map(|i| i as u32),
            },
            audio: audio_reason,
            subtitle: subtitle_reason,
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
        let mut playbacks = self.playbacks.lock().expect("fake state is never poisoned");
        let playback = playbacks
            .get_mut(handle)
            .ok_or(BackendError::PlaybackExpired)?;
        if playback.last_used.elapsed() > HANDLE_IDLE {
            playbacks.remove(handle);
            return Err(BackendError::PlaybackExpired);
        }
        playback.last_used = Instant::now();
        Ok((playback.item.clone(), playback.selection))
    }
}

impl Session for FakeSession {
    fn home(&self) -> BoxFuture<'_, Result<HomeScreen, BackendError>> {
        Box::pin(async move { Ok(self.home_now()) })
    }

    fn libraries(&self) -> BoxFuture<'_, Result<Vec<Library>, BackendError>> {
        Box::pin(async move {
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
            let item = self.catalog.get(id)?;
            let item = self.with_state(item, &self.watch());
            let children = self.catalog.children.get(id).cloned().unwrap_or_default();
            Ok((item, self.items_with_state(&children)))
        })
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<Item>, BackendError>> {
        Box::pin(async move {
            let query = query.to_lowercase();
            let hits: Vec<MediaId> = self
                .catalog
                .order
                .iter()
                .filter(|id| {
                    let name = match &self.catalog.items[*id] {
                        Item::Movie(m) => &m.title,
                        Item::Show(s) => &s.title,
                        Item::Season(_) => return false,
                        Item::Episode(e) => &e.title,
                        Item::Artist(a) => &a.name,
                        Item::Album(a) => &a.title,
                        Item::Track(t) => &t.title,
                    };
                    name.to_lowercase().contains(&query)
                })
                .take(limit as usize)
                .cloned()
                .collect();
            Ok(self.items_with_state(&hits))
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
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>> {
        Box::pin(async move {
            let item = self.catalog.get(id)?;
            let tracks = self.tracks_of(item);
            let Some(path) = &self.media else {
                return Err(BackendError::Unavailable(
                    "the fake server has no media; start the app with --fake-media FILE".into(),
                ));
            };
            let size = std::fs::metadata(path)
                .map_err(|e| BackendError::Unavailable(format!("{}: {e}", path.display())))?
                .len();
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
                    },
                );
            Ok(PlaybackStart {
                handle,
                plan: Self::plan_for(item, &tracks, effective),
                size,
                chosen,
            })
        })
    }

    fn read<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        offset: u64,
        length: u64,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>> {
        Box::pin(async move {
            self.playback(handle)?;
            let path = self.media.as_ref().ok_or(BackendError::PlaybackExpired)?;
            let io =
                |e: std::io::Error| BackendError::Unavailable(format!("{}: {e}", path.display()));
            let mut file = std::fs::File::open(path).map_err(io)?;
            file.seek(SeekFrom::Start(offset)).map_err(io)?;
            let mut bytes = Vec::new();
            file.take(length.min(MAX_READ).min(self.read_limit))
                .read_to_end(&mut bytes)
                .map_err(io)?;
            Ok(bytes)
        })
    }

    fn read_subtitle<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(SubtitleFormat, Vec<u8>), BackendError>> {
        Box::pin(async move {
            let (_, selection) = self.playback(handle)?;
            if selection.subtitle != Some(0) {
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
        // The fixture items carry no artwork; nothing asks for any.
        Box::pin(async move { Err(BackendError::NotFound(id.to_string())) })
    }

    fn update_progress<'a>(
        &'a self,
        id: &'a MediaId,
        position: Duration,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let item = self.catalog.get(id)?;
            // Music has no watch state in Phase 1 (P1-14).
            if matches!(item, Item::Movie(_) | Item::Episode(_)) {
                self.watch().progress(id, length_of(item), position);
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
            Ok(())
        })
    }

    fn mark_unwatched<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.catalog.get(id)?;
            self.watch().set(id, PlaybackState::default());
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
            self.lock_overrides().insert(key, selection);
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
}

impl FakeSession {
    fn lock_overrides(&self) -> std::sync::MutexGuard<'_, HashMap<MediaId, Selection>> {
        self.overrides.lock().expect("fake state is never poisoned")
    }
}

/// The servers and users the mockups show.
pub struct FakeClient {
    catalog: Arc<Catalog>,
    media: Option<PathBuf>,
    read_limit: u64,
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
}

fn home_media() -> ServerKey {
    ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into())
}

fn basement() -> ServerKey {
    ServerKey("9fm0x2kq7wv4hj3n8spe1ac21".into())
}

impl FakeClient {
    /// `media` is the file every playback reads; without one, starting
    /// playback fails with a message saying so.
    pub fn new(media: Option<PathBuf>) -> Self {
        let profile = |username: &str, display_name: &str| Profile {
            server: home_media(),
            username: username.into(),
            display_name: display_name.into(),
        };
        FakeClient {
            catalog: Arc::new(Catalog::new()),
            media,
            read_limit: MAX_READ,
            servers: Mutex::new(vec![
                Server {
                    key: home_media(),
                    name: "Home Media".into(),
                    addresses: vec!["192.168.1.20".into()],
                    pinned: true,
                    on_network: true,
                },
                Server {
                    key: basement(),
                    name: "Basement".into(),
                    addresses: vec!["192.168.1.34".into()],
                    pinned: false,
                    on_network: true,
                },
            ]),
            accounts: vec![
                (home_media(), "andrey", "Andrey"),
                (home_media(), "alice", "Alice"),
                (home_media(), "steve", "Steve"),
            ],
            signed_in: Mutex::new(vec![profile("andrey", "Andrey"), profile("alice", "Alice")]),
            sessions: Mutex::new(HashMap::new()),
            failed_sign_ins: Mutex::new((0, None)),
            connections: Mutex::new(HashMap::new()),
            servers_changed: watch::Sender::new(()),
        }
    }

    /// Reads answer at most `limit` bytes, like a server that sends less
    /// than asked: clients must ask again from where a read stopped.
    pub fn with_read_limit(mut self, limit: u64) -> Self {
        self.read_limit = limit;
        self
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

    fn sign_in_now(
        &self,
        server: &ServerKey,
        username: &str,
        password: &str,
    ) -> Result<Profile, BackendError> {
        self.reach(server)?;
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
        let profile = Profile {
            server: server.clone(),
            username: (*username).into(),
            display_name: (*display_name).into(),
        };
        let mut signed_in = self.signed_in.lock().expect("fake state is never poisoned");
        if !signed_in.contains(&profile) {
            signed_in.push(profile.clone());
        }
        Ok(profile)
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
                        on_network: false,
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

    fn profiles<'a>(
        &'a self,
        server: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<Profile>, BackendError>> {
        Box::pin(async move {
            self.reach(server)?;
            let signed_in = self.signed_in.lock().expect("fake state is never poisoned");
            Ok(signed_in
                .iter()
                .filter(|p| &p.server == server)
                .cloned()
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

    fn session(&self, profile: &Profile) -> Result<Arc<dyn Session>, BackendError> {
        self.reach(&profile.server)?;
        let mut sessions = self.sessions.lock().expect("fake state is never poisoned");
        let session = sessions
            .entry((profile.server.clone(), profile.username.clone()))
            .or_insert_with(|| {
                Arc::new(FakeSession::new(
                    self.catalog.clone(),
                    &profile.username,
                    self.media.clone(),
                    self.read_limit,
                ))
            });
        Ok(session.clone())
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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::io::Write;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::*;

    /// The fake never waits, so its futures finish on the first poll.
    fn ready<T>(future: impl Future<Output = T>) -> T {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the fake backend never waits"),
        }
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

    fn titles(items: &[Item]) -> Vec<&str> {
        items.iter().map(title).collect()
    }

    fn session_for(client: &FakeClient, username: &str) -> Arc<dyn Session> {
        let profile = Profile {
            server: home_media(),
            username: username.into(),
            display_name: username.into(),
        };
        client.session(&profile).unwrap()
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
        let home = ready(alice.home()).unwrap();
        assert!(home.next_up.is_empty(), "{:?}", titles(&home.next_up));
    }

    #[test]
    fn each_user_has_their_own_continue_watching() {
        let client = FakeClient::new(None);
        let andrey = ready(session_for(&client, "andrey").home()).unwrap();
        let alice = ready(session_for(&client, "alice").home()).unwrap();
        assert_ne!(andrey.continue_watching, alice.continue_watching);
    }

    #[test]
    fn progress_under_a_minute_does_not_start_an_item() {
        let session = andrey();
        let heat = media(MediaKind::Movie, "3");
        ready(session.update_progress(&heat, Duration::from_secs(30))).unwrap();
        let home = ready(session.home()).unwrap();
        assert!(!titles(&home.continue_watching).contains(&"Heat"));
    }

    #[test]
    fn progress_puts_an_item_first_in_continue_watching() {
        let session = andrey();
        let heat = media(MediaKind::Movie, "3");
        ready(session.update_progress(&heat, Duration::from_secs(600))).unwrap();
        let home = ready(session.home()).unwrap();
        assert_eq!(title(&home.continue_watching[0]), "Heat");
    }

    #[test]
    fn progress_past_ninety_percent_marks_the_item_watched() {
        let session = andrey();
        let alien = media(MediaKind::Movie, "1");
        ready(session.update_progress(&alien, Duration::from_secs(106 * 60))).unwrap();
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

    #[test]
    fn search_matches_titles_case_insensitively() {
        let hits = ready(andrey().search("TIDE", 10)).unwrap();
        assert_eq!(titles(&hits), ["Low Tide", "Low Tide"]);
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
        let error = ready(andrey().start_playback(&alien, None)).unwrap_err();
        assert!(matches!(error, BackendError::Unavailable(m) if m.contains("--fake-media")));
    }

    fn media_file(bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "spoolframe-fake-{}-{}",
            std::process::id(),
            bytes.len()
        ));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        path
    }

    #[test]
    fn reads_return_the_requested_range_of_the_file() {
        let bytes: Vec<u8> = (0..=255).collect();
        let client = FakeClient::new(Some(media_file(&bytes)));
        let session = session_for(&client, "andrey");
        let alien = media(MediaKind::Movie, "1");
        let start = ready(session.start_playback(&alien, None)).unwrap();
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
        let client = FakeClient::new(Some(media_file(&[0; 17])));
        let session = session_for(&client, "andrey");
        let alien = media(MediaKind::Movie, "1");
        let selection = Selection {
            audio: None,
            subtitle: Some(0),
        };
        let start = ready(session.start_playback(&alien, Some(selection))).unwrap();
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
    fn adding_a_known_address_again_changes_nothing() {
        let client = FakeClient::new(None);
        let key = ServerKey("abcdefghijklmnop".into());
        ready(client.add_address(&key, "10.0.0.5:4433")).unwrap();
        let changes = client.servers_changed();
        ready(client.add_address(&key, "10.0.0.5:4433")).unwrap();
        assert!(!changes.has_changed().unwrap());
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
}
