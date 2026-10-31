//! A server's side of playback, for tests: a [`MockPlayback`] session
//! plays its items from one local file, as it is (a direct play) or as a
//! growing output (a conversion), and keeps what it was told: the starts,
//! the reports, the stops, the seeks and re-plans. It follows one
//! playlist in its own order, and answers previews. The knobs give the
//! answers a test needs that a server gives only now and then: a
//! playback ended under the player, a session ended, a server gone.
//!
//! The SDK's playback owner reaches it through
//! [`crate::playback::backend`], as it reaches a real session.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use spoolframe_protocol::{
    AudioAction, ConvertedAudio, ConvertedSubtitle, ConvertedVideo, PlannedAudio, PlannedSubtitle,
    SubtitleAction, SubtitleDelivery,
};
use spoolframe_types::{
    ArtworkId, AudioCodec, AudioRole, AudioTrack, Container, Delivery, HdrFormat, Item, MediaId,
    MediaTracks, Namespace, SubtitleFormat, SubtitleRole, SubtitleTrack, VideoCodec, VideoTrack,
};

use crate::backend::{
    ArtworkSize, BackendError, BoxFuture, Chunk, Client, DownloadPhase, DownloadSource,
    DownloadTracks, EntryId, LocalDownload, MAX_READ, Marker, PlaySettings, PlaybackChange,
    PlaybackHandle, PlaybackOptions, PlaybackPlan, PlaybackQuality, PlaybackStart, PlaylistEntry,
    PlaylistNext, PlaylistPage, Previews, Repeat, Selection, Session,
};

/// The preview sheet [`MockPlayback::with_previews`] answers: 2 × 2 tiles
/// of 64 × 36, each its own colour.
const PREVIEW_SHEET: &[u8] = include_bytes!("preview-sheet.png");

/// How a [`MockPlayback`] answers `previews`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewsAnswer {
    /// None for any item.
    None,
    /// Still being made this many times, then ready.
    NotReadyFor(u32),
    Ready,
}

/// One `start_playback` the session was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartCall {
    pub item: MediaId,
    pub selection: Option<Selection>,
    pub options: PlaybackOptions,
}

/// A playlist call the session was sent, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistCall {
    Play {
        from: Option<EntryId>,
        settings: PlaySettings,
    },
    Next {
        after: EntryId,
    },
    Upcoming,
    Save(PlaySettings),
}

/// A session serving playbacks of one file.
pub struct MockPlayback {
    media: Option<PathBuf>,
    items: HashMap<MediaId, Item>,
    /// Bytes a converted output grows by at each read; `None` plays
    /// direct.
    converting: Option<u64>,
    markers: Vec<Marker>,
    /// A handle unused this long is gone, as on a server.
    handle_idle: Duration,
    /// The first converted read from this offset finds its handle gone.
    expire_at: Option<u64>,
    expired_once: AtomicBool,
    /// Reads answer that the admin stopped the playback.
    stopped: bool,
    /// Reads answer that the session ended.
    signed_out: bool,
    gone: AtomicBool,
    /// The output's subtitle is read as text: `read_subtitle` is not
    /// ready this many times.
    reading_subtitle: Option<u32>,
    subtitle_asks: AtomicU32,
    /// A subtitle asked for is copied into the converted output.
    copying_subtitle: bool,
    previews: PreviewsAnswer,
    preview_asks: AtomicU32,
    next_handle: AtomicU64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    playbacks: HashMap<PlaybackHandle, Open>,
    starts: Vec<StartCall>,
    reports: Vec<(MediaId, Duration)>,
    quality_changes: Vec<(PlaybackQuality, Duration)>,
    limits: Vec<u64>,
    seeks: Vec<Duration>,
    playlist: Option<PlaylistPage>,
    upcoming: Option<Vec<PlaylistEntry>>,
    playlist_calls: Vec<PlaylistCall>,
}

/// A playback the session has open.
struct Open {
    item: MediaId,
    selection: Selection,
    last_used: Instant,
    epoch: u32,
    /// How much of the converted output is written.
    written: u64,
}

impl MockPlayback {
    /// Plays `items` from `media`, direct; with no file, a start fails as
    /// unavailable.
    pub fn new(media: Option<PathBuf>, items: impl IntoIterator<Item = Item>) -> Self {
        Self {
            media,
            items: items
                .into_iter()
                .map(|item| (item.id().clone(), item))
                .collect(),
            converting: None,
            markers: Vec::new(),
            handle_idle: Duration::MAX,
            expire_at: None,
            expired_once: AtomicBool::new(false),
            stopped: false,
            signed_out: false,
            gone: AtomicBool::new(false),
            reading_subtitle: None,
            subtitle_asks: AtomicU32::new(0),
            copying_subtitle: false,
            previews: PreviewsAnswer::None,
            preview_asks: AtomicU32::new(0),
            next_handle: AtomicU64::new(1),
            state: Mutex::default(),
        }
    }

    /// Converts every start: the output is the file, written `bytes` more
    /// at each read, and a seek starts it again from its start.
    pub fn converting(mut self, bytes: u64) -> Self {
        self.converting = Some(bytes);
        self
    }

    /// Every start has these markers.
    pub fn with_markers(mut self, markers: Vec<Marker>) -> Self {
        self.markers = markers;
        self
    }

    /// A handle unused for `idle` is gone: what uses it next is told it
    /// expired.
    pub fn with_handle_idle(mut self, idle: Duration) -> Self {
        self.handle_idle = idle;
        self
    }

    /// The first converted read at or past `offset` is told its playback
    /// expired, as one the server ended.
    pub fn expiring_at(mut self, offset: u64) -> Self {
        self.expire_at = Some(offset);
        self
    }

    /// Reads are told the admin stopped the playback.
    pub fn with_playbacks_stopped(mut self) -> Self {
        self.stopped = true;
        self
    }

    /// Reads are told the session ended.
    pub fn with_sessions_ending(mut self) -> Self {
        self.signed_out = true;
        self
    }

    /// A converted output's subtitle is read as text, not ready the first
    /// `waits` times it is asked for.
    pub fn reading_subtitle(mut self, waits: u32) -> Self {
        self.reading_subtitle = Some(waits);
        self
    }

    /// A subtitle asked for is copied into the converted output.
    pub fn copying_subtitle(mut self) -> Self {
        self.copying_subtitle = true;
        self
    }

    pub fn with_previews(mut self, answer: PreviewsAnswer) -> Self {
        self.previews = answer;
        self
    }

    /// Follows `page`'s playlist, whole: its place and settings are the
    /// page's, until the calls move them.
    pub fn with_playlist(self, page: PlaylistPage) -> Self {
        self.lock().playlist = Some(page);
        self
    }

    /// What `upcoming_in_playlist` answers, whatever the place: a
    /// server's shuffled order, say.
    pub fn with_upcoming(self, entries: Vec<PlaylistEntry>) -> Self {
        self.lock().upcoming = Some(entries);
        self
    }

    /// Reads from now on find the server unreachable.
    pub fn go_away(&self) {
        self.gone.store(true, Ordering::SeqCst);
    }

    pub fn starts(&self) -> Vec<StartCall> {
        self.lock().starts.clone()
    }

    /// Playbacks started and not stopped or expired.
    pub fn open(&self) -> usize {
        self.lock().playbacks.len()
    }

    /// Open playbacks used within the idle limit.
    pub fn live(&self) -> usize {
        let idle = self.handle_idle;
        let state = self.lock();
        let live = state.playbacks.values();
        live.filter(|p| p.last_used.elapsed() <= idle).count()
    }

    /// Every progress report, in order.
    pub fn reports(&self) -> Vec<(MediaId, Duration)> {
        self.lock().reports.clone()
    }

    /// Every `change_quality`, with the position it was sent from.
    pub fn quality_changes(&self) -> Vec<(PlaybackQuality, Duration)> {
        self.lock().quality_changes.clone()
    }

    /// The link rates of every `limit_playback`.
    pub fn limits(&self) -> Vec<u64> {
        self.lock().limits.clone()
    }

    /// Where each `seek_playback` went.
    pub fn seeks(&self) -> Vec<Duration> {
        self.lock().seeks.clone()
    }

    pub fn playlist_calls(&self) -> Vec<PlaylistCall> {
        self.lock().playlist_calls.clone()
    }

    /// The playlist's place: the entry last played or told.
    pub fn place(&self) -> Option<EntryId> {
        self.lock().playlist.as_ref()?.place
    }

    /// The playlist's settings, as last saved.
    pub fn settings(&self) -> Option<PlaySettings> {
        Some(self.lock().playlist.as_ref()?.settings)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("mock state is never poisoned")
    }

    fn item(&self, id: &MediaId) -> Result<&Item, BackendError> {
        self.items
            .get(id)
            .ok_or_else(|| BackendError::NotFound(id.to_string()))
    }

    /// The playback behind `handle`, touched; gone when idle too long.
    fn with_open<T>(
        &self,
        handle: &PlaybackHandle,
        run: impl FnOnce(&mut Open) -> T,
    ) -> Result<T, BackendError> {
        let mut state = self.lock();
        let open = state
            .playbacks
            .get_mut(handle)
            .ok_or(BackendError::PlaybackExpired)?;
        if open.last_used.elapsed() > self.handle_idle {
            state.playbacks.remove(handle);
            return Err(BackendError::PlaybackExpired);
        }
        open.last_used = Instant::now();
        Ok(run(open))
    }

    fn media_size(&self) -> Result<u64, BackendError> {
        let path = self.media.as_ref().ok_or_else(|| {
            BackendError::Unavailable("the mock server was given no media".into())
        })?;
        let size = std::fs::metadata(path)
            .map_err(|e| BackendError::Unavailable(format!("{}: {e}", path.display())))?
            .len();
        Ok(size)
    }

    /// Up to `length` bytes of the file from `offset`.
    fn file_bytes(&self, offset: u64, length: u64) -> Result<Vec<u8>, BackendError> {
        let path = self.media.as_ref().ok_or_else(|| {
            BackendError::Unavailable("the mock server was given no media".into())
        })?;
        let io = |e: std::io::Error| BackendError::Unavailable(format!("{}: {e}", path.display()));
        let mut file = std::fs::File::open(path).map_err(io)?;
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut bytes = Vec::new();
        file.take(length.min(MAX_READ))
            .read_to_end(&mut bytes)
            .map_err(io)?;
        Ok(bytes)
    }

    /// What a converted output does with its subtitle: read as text,
    /// copied in when one is asked for, or nothing.
    fn converted_subtitle(&self, selection: Selection) -> Option<ConvertedSubtitle> {
        if self.reading_subtitle.is_some() {
            return Some(ConvertedSubtitle {
                index: 0,
                action: SubtitleAction::Ocr {
                    from: SubtitleFormat::Pgs,
                    to: SubtitleFormat::Srt,
                },
            });
        }
        let index = selection.subtitle.filter(|_| self.copying_subtitle)?;
        Some(ConvertedSubtitle {
            index,
            action: SubtitleAction::Copy {
                format: SubtitleFormat::Srt,
            },
        })
    }

    /// Plans the playback behind `handle` again, its encode started anew:
    /// converted, or the file as it is.
    fn replan(
        &self,
        handle: &PlaybackHandle,
        converted: bool,
    ) -> Result<PlaybackChange, BackendError> {
        let (id, selection, epoch) = self.with_open(handle, |p| {
            p.epoch += 1;
            p.written = 0;
            (p.item.clone(), p.selection, p.epoch)
        })?;
        let item = self.item(&id)?;
        Ok(match converted {
            true => PlaybackChange {
                plan: converted_plan(item, selection, None),
                size: 0,
                epoch,
            },
            false => PlaybackChange {
                plan: direct_plan(item, &tracks_of(item), selection),
                size: self.media_size()?,
                epoch,
            },
        })
    }

    /// The playlist's entry after `after` in its order, round again with
    /// repeat all, passing those that cannot play.
    fn entry_after(page: &PlaylistPage, after: EntryId) -> Option<PlaylistEntry> {
        let at = page.entries.iter().position(|e| e.entry == after)?;
        let rest = page.entries[at + 1..].iter();
        let round = match page.settings.repeat {
            Repeat::All => page.entries[..=at].iter().collect(),
            _ => Vec::new(),
        };
        rest.chain(round).find(|e| !e.unavailable).cloned()
    }
}

/// The entry as the server answers it, to play.
fn next_of(entry: PlaylistEntry) -> PlaylistNext {
    PlaylistNext {
        entry: entry.entry,
        item: entry.item,
        markers: Default::default(),
        liked: entry.liked,
    }
}

/// A film's or an episode's running time; a track's.
fn length_of(item: &Item) -> Option<Duration> {
    match item {
        Item::Movie(m) => m.duration,
        Item::Episode(e) => e.duration,
        Item::Track(t) => t.duration,
        _ => None,
    }
}

/// The tracks an item has here: a song one audio track; a film or an
/// episode three, and three subtitles, the first an external file.
pub fn tracks_of(item: &Item) -> MediaTracks {
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
            subtitles: vec![
                subtitle(None, SubtitleRole::Full, true),
                subtitle(Some("Forced"), SubtitleRole::Forced, false),
                subtitle(Some("SDH"), SubtitleRole::Sdh, false),
            ],
        },
    }
}

/// HEVC the client "cannot play", encoded to H.264, the audio copied.
fn converted_plan(
    item: &Item,
    selection: Selection,
    subtitle: Option<ConvertedSubtitle>,
) -> PlaybackPlan {
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
        subtitle,
        reasons: vec!["video 0: hevc is not supported; encoded to h264".into()],
        ladder: Vec::new(),
        source_codec: video.then(|| "hvc1.2.4.L153.B0".into()),
    }
}

/// The file as it is, with the subtitle asked for: an external one as a
/// file of its own.
fn direct_plan(item: &Item, tracks: &MediaTracks, selection: Selection) -> PlaybackPlan {
    let video = !matches!(item, Item::Track(_));
    let subtitle = selection.subtitle.and_then(|i| {
        let track = tracks.subtitles.get(i as usize)?;
        Some(PlannedSubtitle {
            index: i,
            format: match track.external {
                true => SubtitleFormat::Srt,
                false => SubtitleFormat::Pgs,
            },
            delivery: match track.external {
                true => SubtitleDelivery::External,
                false => SubtitleDelivery::Embedded,
            },
        })
    });
    PlaybackPlan::Direct {
        container: Container::Matroska,
        video: video.then_some(VideoCodec::H264),
        audio: Some(PlannedAudio {
            index: selection.audio.unwrap_or(0),
            codec: AudioCodec::Aac,
        }),
        subtitle,
        source_codec: video.then(|| "avc1.640028".into()),
    }
}

impl Session for MockPlayback {
    fn tracks<'a>(&'a self, id: &'a MediaId) -> BoxFuture<'a, Result<MediaTracks, BackendError>> {
        let answer = self.item(id).map(tracks_of);
        Box::pin(async move { answer })
    }

    fn start_playback<'a>(
        &'a self,
        id: &'a MediaId,
        selection: Option<Selection>,
        options: PlaybackOptions,
    ) -> BoxFuture<'a, Result<PlaybackStart, BackendError>> {
        Box::pin(async move {
            self.lock().starts.push(StartCall {
                item: id.clone(),
                selection,
                options: options.clone(),
            });
            let item = self.item(id)?;
            let size = self.media_size()?;
            let effective = selection.unwrap_or_default();
            let handle = PlaybackHandle(format!(
                "mock-{}",
                self.next_handle.fetch_add(1, Ordering::Relaxed)
            ));
            let open = Open {
                item: id.clone(),
                selection: effective,
                last_used: Instant::now(),
                epoch: 0,
                written: 0,
            };
            self.lock().playbacks.insert(handle.clone(), open);
            // A chosen bitrate converts, as a file over it would be.
            let capped = matches!(options.quality, PlaybackQuality::Bitrate(_));
            let (plan, size) = match self.converting {
                _ if capped => (converted_plan(item, effective, None), 0),
                Some(_) => (
                    converted_plan(item, effective, self.converted_subtitle(effective)),
                    0,
                ),
                None => (direct_plan(item, &tracks_of(item), effective), size),
            };
            Ok(PlaybackStart {
                handle,
                plan,
                size,
                chosen: None,
                epoch: 0,
                markers: self.markers.clone(),
                duration: length_of(item),
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
            if self.gone.load(Ordering::SeqCst) {
                return Err(BackendError::Unreachable("the mock server".into()));
            }
            let size = self.media_size()?;
            if self.expire_at.is_some_and(|at| offset >= at)
                && !self.expired_once.swap(true, Ordering::SeqCst)
            {
                self.lock().playbacks.remove(handle);
                return Err(BackendError::PlaybackExpired);
            }
            let grow = self.converting.unwrap_or(size);
            // Each read lets the "encode" write a little more.
            let (current, written) = self.with_open(handle, |p| {
                p.written = (p.written + grow).min(size);
                (p.epoch, p.written)
            })?;
            if epoch != current {
                return Err(BackendError::PlaybackRestarted { epoch: current });
            }
            let available = written.saturating_sub(offset).min(length);
            let bytes = match available {
                0 => Vec::new(),
                _ => self.file_bytes(offset, available)?,
            };
            Ok(Chunk {
                bytes,
                size: written,
                complete: written == size,
                epoch: current,
            })
        })
    }

    /// The "encode" is the file from its start: the restart is in the
    /// epoch and the bytes written, not the time.
    fn seek_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        position: Duration,
    ) -> BoxFuture<'a, Result<u32, BackendError>> {
        Box::pin(async move {
            let epoch = self.with_open(handle, |p| {
                p.epoch += 1;
                p.written = 0;
                p.epoch
            })?;
            self.lock().seeks.push(position);
            Ok(epoch)
        })
    }

    fn stop_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            self.lock().playbacks.remove(handle);
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
            if self.gone.load(Ordering::SeqCst) {
                return Err(BackendError::Unreachable("the mock server".into()));
            }
            if self.stopped {
                return Err(BackendError::PlaybackStopped);
            }
            if self.signed_out {
                return Err(BackendError::SignInRequired);
            }
            self.with_open(handle, |_| ())?;
            self.file_bytes(offset, length)
        })
    }

    fn read_subtitle<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
    ) -> BoxFuture<'a, Result<(SubtitleFormat, Vec<u8>), BackendError>> {
        Box::pin(async move {
            self.with_open(handle, |_| ())?;
            if let Some(waits) = self.reading_subtitle
                && self.subtitle_asks.fetch_add(1, Ordering::SeqCst) < waits
            {
                return Err(BackendError::SubtitleNotReady(50));
            }
            let srt = "1\n00:00:01,000 --> 00:00:05,000\nA subtitle from the mock server.\n";
            Ok((SubtitleFormat::Srt, srt.as_bytes().to_vec()))
        })
    }

    fn update_progress<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        position: Duration,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async move {
            let id = self.with_open(handle, |p| p.item.clone())?;
            self.lock().reports.push((id, position));
            Ok(())
        })
    }

    fn change_quality<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        quality: PlaybackQuality,
        position: Duration,
    ) -> BoxFuture<'a, Result<PlaybackChange, BackendError>> {
        Box::pin(async move {
            self.lock().quality_changes.push((quality, position));
            self.replan(handle, matches!(quality, PlaybackQuality::Bitrate(_)))
        })
    }

    /// Any link it is told of is too slow for the file: the playback
    /// converts.
    fn limit_playback<'a>(
        &'a self,
        handle: &'a PlaybackHandle,
        link_bitrate: u64,
        _position: Duration,
    ) -> BoxFuture<'a, Result<PlaybackChange, BackendError>> {
        Box::pin(async move {
            self.lock().limits.push(link_bitrate);
            self.replan(handle, true)
        })
    }

    fn set_track_override<'a>(
        &'a self,
        _id: &'a MediaId,
        _selection: Selection,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        Box::pin(async { Ok(()) })
    }

    fn previews<'a>(&'a self, _id: &'a MediaId) -> BoxFuture<'a, Result<Previews, BackendError>> {
        let answer = match self.previews {
            PreviewsAnswer::None => Err(BackendError::NoPreviews),
            PreviewsAnswer::NotReadyFor(times)
                if self.preview_asks.fetch_add(1, Ordering::SeqCst) < times =>
            {
                Err(BackendError::PreviewsNotReady(30))
            }
            _ => Ok(Previews {
                interval: Duration::from_secs(1),
                tile_width: 64,
                tile_height: 36,
                columns: 2,
                rows: 2,
                count: 4,
                sheets: vec![ArtworkId::new(Namespace::Preview, "mock/0").expect("not empty")],
            }),
        };
        Box::pin(async move { answer })
    }

    fn artwork<'a>(
        &'a self,
        id: &'a ArtworkId,
        _size: ArtworkSize,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackendError>> {
        let answer = match id.namespace() {
            Namespace::Preview => Ok(PREVIEW_SHEET.to_vec()),
            _ => Err(BackendError::NotFound(id.to_string())),
        };
        Box::pin(async move { answer })
    }

    fn playlist<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<PlaylistPage, BackendError>> {
        let answer = match &self.lock().playlist {
            Some(whole) if whole.playlist.id == id => {
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
            _ => Err(BackendError::NotFound(id.to_owned())),
        };
        Box::pin(async move { answer })
    }

    fn play_playlist<'a>(
        &'a self,
        id: &'a str,
        from: Option<EntryId>,
        settings: PlaySettings,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>> {
        let mut state = self.lock();
        state
            .playlist_calls
            .push(PlaylistCall::Play { from, settings });
        let answer = match state.playlist.as_mut() {
            Some(page) if page.playlist.id == id => {
                page.settings = settings;
                let first = page
                    .entries
                    .iter()
                    .find(|e| from.is_none_or(|from| e.entry == from) && !e.unavailable)
                    .cloned();
                page.place = first.as_ref().map(|e| e.entry);
                Ok(first.map(next_of))
            }
            _ => Err(BackendError::NotFound(id.to_owned())),
        };
        Box::pin(async move { answer })
    }

    fn next_in_playlist<'a>(
        &'a self,
        id: &'a str,
        after: EntryId,
    ) -> BoxFuture<'a, Result<Option<PlaylistNext>, BackendError>> {
        let mut state = self.lock();
        state.playlist_calls.push(PlaylistCall::Next { after });
        let answer = match state.playlist.as_mut() {
            Some(page) if page.playlist.id == id => {
                let next = Self::entry_after(page, after);
                if let Some(next) = &next {
                    page.place = Some(next.entry);
                }
                Ok(next.map(next_of))
            }
            _ => Err(BackendError::NotFound(id.to_owned())),
        };
        Box::pin(async move { answer })
    }

    /// The canned answer, or the entries after the place in the
    /// playlist's order, once each.
    fn upcoming_in_playlist<'a>(
        &'a self,
        id: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<PlaylistEntry>, BackendError>> {
        let mut state = self.lock();
        state.playlist_calls.push(PlaylistCall::Upcoming);
        let canned = state.upcoming.clone();
        let answer = match &state.playlist {
            Some(page) if page.playlist.id == id => {
                let mut seen = HashSet::new();
                let ahead = canned.unwrap_or_else(|| {
                    let mut ahead = Vec::new();
                    let mut at = page.place;
                    while let Some(next) = at.and_then(|at| Self::entry_after(page, at)) {
                        if !seen.insert(next.entry) {
                            break;
                        }
                        at = Some(next.entry);
                        ahead.push(next);
                    }
                    ahead
                });
                Ok(ahead.into_iter().take(limit as usize).collect())
            }
            _ => Err(BackendError::NotFound(id.to_owned())),
        };
        Box::pin(async move { answer })
    }

    fn set_playlist_settings<'a>(
        &'a self,
        id: &'a str,
        settings: PlaySettings,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let mut state = self.lock();
        state.playlist_calls.push(PlaylistCall::Save(settings));
        let answer = match state.playlist.as_mut() {
            Some(page) if page.playlist.id == id => {
                page.settings = settings;
                Ok(())
            }
            _ => Err(BackendError::NotFound(id.to_owned())),
        };
        Box::pin(async move { answer })
    }
}

/// What the client was told of a download played here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadCall {
    Position(Duration),
    Watched(bool),
}

/// A client with one finished download, keeping what it is told of it.
pub struct MockDownloads {
    download: LocalDownload,
    calls: Mutex<Vec<DownloadCall>>,
}

impl MockDownloads {
    pub fn new(download: LocalDownload) -> Self {
        Self {
            download,
            calls: Mutex::default(),
        }
    }

    pub fn calls(&self) -> Vec<DownloadCall> {
        self.calls
            .lock()
            .expect("mock state is never poisoned")
            .clone()
    }

    fn told(&self, key: &str, call: DownloadCall) -> Result<LocalDownload, BackendError> {
        if key != self.download.key {
            return Err(BackendError::NotFound(key.to_owned()));
        }
        let mut calls = self.calls.lock().expect("mock state is never poisoned");
        calls.push(call);
        Ok(self.download.clone())
    }
}

impl Client for MockDownloads {
    fn download<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<LocalDownload, BackendError>> {
        let answer = match key == self.download.key {
            true => Ok(self.download.clone()),
            false => Err(BackendError::NotFound(key.to_owned())),
        };
        Box::pin(async move { answer })
    }

    fn record_download_position<'a>(
        &'a self,
        key: &'a str,
        position: Duration,
    ) -> BoxFuture<'a, Result<LocalDownload, BackendError>> {
        let answer = self.told(key, DownloadCall::Position(position));
        Box::pin(async move { answer })
    }

    fn set_download_watched<'a>(
        &'a self,
        key: &'a str,
        watched: bool,
    ) -> BoxFuture<'a, Result<LocalDownload, BackendError>> {
        let answer = self.told(key, DownloadCall::Watched(watched));
        Box::pin(async move { answer })
    }
}

/// `item` downloaded whole, its original file at `path`, its tracks as
/// [`tracks_of`] and every one of them in the file.
pub fn download(key: &str, item: Item, path: PathBuf) -> LocalDownload {
    LocalDownload {
        key: key.into(),
        server: "mock".into(),
        user_id: "andrey".into(),
        username: "andrey".into(),
        tracks: tracks_of(&item),
        item,
        show: None,
        season: None,
        markers: Vec::new(),
        artwork: Vec::new(),
        source: DownloadSource::Original,
        chosen: DownloadTracks::All,
        file: None,
        phase: DownloadPhase::Done,
        received: 0,
        size: None,
        expected_size: None,
        file_url: Some(format!("file://{}", path.display())),
        file_path: Some(path),
        watch: Default::default(),
        note: None,
        created_at: SystemTime::UNIX_EPOCH,
    }
}
