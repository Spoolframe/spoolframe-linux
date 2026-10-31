//! What is playing, apart from any window onto it (P1-16). GstPlay
//! (playbin3) reads the server's file through the spoolframe:// source, or
//! a download's file on this computer (PX-38), and draws into
//! gtk4paintablesink. The playlist, the track choices and the progress
//! reports live here.
//!
//! The app owns one [`Playback`] at a time, and views attach to it: the
//! full player page, and the bar along the bottom of the window when the
//! player is minimized. Closing the page by minimizing keeps playing;
//! dropping the playback stops it. A view subscribes to [`Change`]s and is
//! told the current state at once, so a view made mid-play shows what is
//! playing straight away.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::glib::thread_guard::ThreadGuard;
use spoolframe_client_sdk::queue::Previous;
use spoolframe_client_sdk::tracks::{self, StreamPicks, SubtitleHandling};
use spoolframe_client_sdk::{
    LocalController, LocalSpawn, PlaybackController, PlaybackDriver, PlayerSession, SdkError,
};
use spoolframe_client_sdk::{MarkerAction, MarkerOffer, MarkerTracker, Markers};
use spoolframe_client_sdk::{PlaybackOptions, PlayerState};
use spoolframe_client_sdk::{PlaylistCursor, UpNext as PlaylistUpNext, UpNextNote};
use spoolframe_linux_core::backend::{
    BackendError, Client, DownloadPhase, EntryId, LocalDownload, Marker, PlaySettings,
    PlaybackPlan, PlaylistEntry, PlaylistNext, Selection, Session, SubtitleTiming,
};
use spoolframe_linux_core::playback::{self as managed, ManagedPlayback};
use spoolframe_linux_core::playlists::{Advance, Playing};
use spoolframe_linux_core::progressive::ProgressiveReader;
use spoolframe_linux_core::reader::{self, ByteSource, MediaReader, Spawn};
use spoolframe_linux_core::sdk::backend_error;
use spoolframe_protocol::ConvertedVideo;
use spoolframe_types::Delivery;
use spoolframe_types::{Album, Item, MediaId, MediaTracks, SubtitleFormat};

use crate::external_subtitle;
use crate::pipeline::Pipeline;
pub use crate::pipeline::VideoSink;
use crate::queue::{Queue, Queued};
pub use spoolframe_linux_core::backend::Repeat;

/// Takes the demuxers and decoders the app never needs out of the
/// registry, and registers the linked-in gtk4paintablesink and the
/// spoolframe:// source. Call once, after GTK is up, before any pipeline
/// is built or the capabilities are read.
pub fn init() -> anyhow::Result<()> {
    gst::init()?;
    crate::denylist::apply();
    // A gst-plugin-gtk4 among the system's plugins (GNOME's Flatpak
    // runtime has one) is used instead: the sink's GLib type can be
    // registered once, and a scan in the process (as confined) already
    // did. That one is built for the system's GTK, which is then the
    // app's.
    if gst::ElementFactory::find("gtk4paintablesink").is_none() {
        gstgtk4::plugin_register_static()?;
    }
    crate::source::register()?;
    Ok(())
}

/// What to play: items of one session, from one of them. A movie or an
/// episode is a list of one; an album is its tracks. That is the context;
/// `queued` is the play queue (P1-23), carried from the playback before.
pub struct Playlist {
    pub session: Arc<dyn Session>,
    pub items: Vec<Item>,
    pub index: usize,
    pub queued: Vec<Queued>,
    /// Where to start the first item; later items start at the beginning.
    pub from: Option<Duration>,
    /// The tracks picked for the first item; `None` lets the server choose
    /// from the user's preferences (later items always do).
    pub selection: Option<Selection>,
    /// The first item's track list, to map the tracks onto the file's
    /// streams; fetched when not given.
    pub tracks: Option<MediaTracks>,
    /// The album the tracks are from, for its cover and name.
    pub album: Option<Album>,
    /// The quality the viewer chose (P6-06), for every item it plays
    /// until they choose again.
    pub quality: spoolframe_client_sdk::PlaybackQuality,
    /// What the screen shows (PGPU-06), told with every start.
    pub display: Option<spoolframe_types::DisplayCapabilities>,
    /// The server's playlist (PX-40) the item is from: what follows it is
    /// the server's next entry. Its settings are those it was started
    /// with: shuffle and repeat are the playlist's from the first song.
    pub from_playlist: Option<crate::saved_queue::PlaylistResume>,
}

/// A film or an episode downloaded to this computer (PX-38), played from
/// its file with no server: what it is, where the file is, and the client
/// that keeps where the user got to in it until the server hears it.
pub struct LocalItem {
    /// The download's key, for the client's calls.
    pub key: String,
    pub item: Item,
    /// The file, as a `file://` URL.
    pub file_url: String,
    pub markers: Vec<Marker>,
    /// The tracks the file holds, in its order (see
    /// `LocalDownload::file_tracks`).
    pub tracks: MediaTracks,
    pub client: Arc<dyn Client>,
}

impl LocalItem {
    /// `download`'s file to play, once it is all here.
    pub fn of(download: &LocalDownload, client: Arc<dyn Client>) -> Option<Self> {
        if download.phase != DownloadPhase::Done {
            return None;
        }
        Some(Self {
            key: download.key.clone(),
            item: download.item.clone(),
            file_url: download.file_url.clone()?,
            markers: download.markers.clone(),
            tracks: download.file_tracks(),
            client,
        })
    }
}

/// How the first item starts: see [`Playlist`].
struct Start {
    from: Option<Duration>,
    selection: Option<Selection>,
    tracks: Option<MediaTracks>,
    quality: spoolframe_client_sdk::PlaybackQuality,
    display: Option<spoolframe_types::DisplayCapabilities>,
}

/// What a view hears from the playback.
pub enum Change<'a> {
    /// A new item started (or the view just attached).
    Item(&'a Item),
    /// The server is being asked to start the item.
    Starting,
    Playing(bool),
    Position {
        at: Duration,
        length: Duration,
    },
    /// The file's streams, as GstPlay knows them so far.
    MediaInfo(&'a gst_play::PlayMediaInfo),
    /// Under 100 while the read-ahead catches up after running dry.
    Buffering(i32),
    Failed(&'a str),
    Volume(f64),
    Muted(bool),
    /// Shuffle and repeat, as set now.
    Modes {
        shuffle: bool,
        repeat: Repeat,
    },
    /// What plays next changed: the queue, or the context's order.
    Queue,
    /// A queued item could not play and was passed over; what to tell
    /// the user.
    Skipped(&'a str),
    /// An audio or subtitle track could not be picked, at the start or
    /// from the menu; what to tell the user.
    TrackFailed(&'a str),
    /// The button over the video from the item's markers (P2-10), or
    /// none.
    Offer(Option<MarkerOffer>),
    /// The intro was skipped by itself; [`Playback::undo_intro_skip`]
    /// goes back.
    IntroSkipped,
    /// The server's admin stopped this playback (P3-15): nothing plays it
    /// again; the player closes.
    Stopped,
    /// The session ended under it (signed out elsewhere, or the password
    /// changed): back to "Who's watching".
    SignedOut,
    /// The server's playlist played to its end (PX-40); it plays no more.
    PlaylistFinished(&'a Playing),
    /// The visualizer failed and is off: the cover shows again.
    VisualizerStopped,
}

/// Where what plays after the current item comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NextFrom {
    /// The queue or the context has it.
    Here,
    /// The server's playlist: asked for when it is needed, since asking
    /// moves the user's place there on.
    Playlist,
    Nothing,
}

/// Whatever is queued or ahead in the context plays first; then the
/// playlist, if one is being played.
fn next_from(here: bool, playlist: bool) -> NextFrom {
    match (here, playlist) {
        (true, _) => NextFrom::Here,
        (false, true) => NextFrom::Playlist,
        (false, false) => NextFrom::Nothing,
    }
}

/// What plays after the current item, for "Up next".
pub struct UpNext {
    /// The hand-added items, in the order they play.
    pub queued: Vec<Queued>,
    /// What follows the queue, each part under its heading: the rest of
    /// the context, then the server's playlist's next entries.
    pub ahead: Vec<Ahead>,
}

/// A part of [`UpNext`] after the queue.
pub struct Ahead {
    /// "Next from <album>", "In <playlist> · Shuffle on".
    pub heading: String,
    /// The album its items are from, for their covers and artists.
    pub album: Option<Album>,
    pub entries: Vec<(Place, Item)>,
    /// A dim line under them, or none: why no songs are named, or that
    /// the one named plays again.
    pub note: Option<&'static str>,
}

/// Where an item of [`Ahead`] is, for [`Playback::play_ahead`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The context's item at this position in its play order.
    Context(usize),
    /// The server's playlist's entry.
    Playlist(EntryId),
}

/// The server's playlist being played (PX-40): what is its alone, gone
/// together once the playlist no longer says what follows.
struct PlaylistContext {
    /// Which playlist, and the entry playing.
    playing: Playing,
    /// Its shuffle, repeat and skip watched; `None` until known (the
    /// cursor's start reads them).
    modes: Option<PlaySettings>,
    /// The SDK's cursor on it, from the entry playing; `None` until the
    /// first call in [`Playback::playlist_calls`] starts it.
    cursor: Option<Arc<PlaylistCursor>>,
    /// What the cursor last said plays after the entry playing; `None`
    /// until read.
    up_next: Option<PlaylistUpNext>,
}

type Listener = Box<dyn Fn(&Change) -> bool>;

thread_local! {
    /// Every playback started, for `shutdown`.
    static LIVE: RefCell<Vec<Weak<Playback>>> = const { RefCell::new(Vec::new()) };
}

/// Stops every playback still alive. Call once the app's main loop has
/// returned, before the process exits. Relm4 does not drop the app's
/// state at quit, so a playback it holds would otherwise keep its GL
/// context and GStreamer threads into the exit handlers, where libEGL's
/// teardown then crashed on them (a segfault on quitting mid-play).
///
/// It waits, up to [`SHUTDOWN_WAIT`], for the last progress report and a
/// converted playback's stop to reach the server: spawned and left, they
/// were cancelled when the process exited, so the position was lost and
/// the server's ffmpeg ran until its handle idled out.
pub fn shutdown() {
    let live = LIVE.with(|live| std::mem::take(&mut *live.borrow_mut()));
    let finals: Vec<_> = live
        .iter()
        .filter_map(Weak::upgrade)
        .flat_map(|playback| playback.stop_waitable())
        .collect();
    if finals.is_empty() {
        return;
    }
    let waited = glib::MainContext::default().block_on(glib::future_with_timeout(
        SHUTDOWN_WAIT,
        async move {
            for call in finals {
                let _ = call.await;
            }
        },
    ));
    if waited.is_err() {
        tracing::warn!("the server did not hear the last progress before quitting");
    }
}

/// How long quitting waits for the last calls to the server.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

/// What a failed pick tells the user.
pub const SUBTITLES_FAILED: &str = "Couldn't turn on subtitles";
pub const AUDIO_FAILED: &str = "Couldn't switch the audio track";
pub const QUALITY_FAILED: &str = "Couldn't change the quality";

/// How often a download's place is kept while it plays, as the SDK
/// reports a server playback's ([`ManagedPlayback::PROGRESS_EVERY`]).
const PROGRESS_EVERY: u32 = 10;

pub struct Playback {
    /// GstPlay and what it plays through; nothing else touches them.
    pipeline: Pipeline,
    /// The server's session it plays from; `None` for a download, which
    /// plays with no server, and has none of what needs one (subtitle
    /// search and timing, quality, the show's next episode, playlists).
    session: Option<Arc<dyn Session>>,
    /// The server playbacks of [`Self::session`], started, replaced and
    /// ended in the order asked: a start the next one overtook never
    /// plays. `None` for a download.
    player: Option<PlayerSession>,
    /// The download playing (PX-38), in place of the server's file: its
    /// place goes to the client, which sends it to the server later.
    local: Option<LocalItem>,
    /// What plays, and what plays next.
    queue: RefCell<Queue>,
    /// The item playing, and everything that is its alone; `None` once
    /// stopped.
    current: RefCell<Option<Rc<Current>>>,
    /// The user paused (and has not played since): a reader read anew
    /// stays paused too. A converted restart's is the controller's.
    held: Cell<bool>,
    /// Whether the current item plays once ready. False for a queue
    /// restored at start-up, which waits, paused, for Play.
    autoplay: Cell<bool>,
    /// Skip intros without asking (Preferences → Playback).
    skip_intros: Cell<bool>,
    /// Whether subtitles show; see [`crate::tracks::menu`].
    subtitles_on: Rc<Cell<bool>>,
    /// The quality the viewer chose (P6-06): each item starts with it.
    quality: Cell<spoolframe_client_sdk::PlaybackQuality>,
    /// What the screen shows (PGPU-06): each item starts with it.
    display: RefCell<Option<spoolframe_types::DisplayCapabilities>>,
    playing: Cell<bool>,
    position: Cell<Duration>,
    length: Cell<Duration>,
    stopped: Cell<bool>,
    listeners: RefCell<Vec<Listener>>,
    /// The server's playlist being played; `None` while what plays
    /// follows the queue and the context alone.
    playlist: RefCell<Option<PlaylistContext>>,
    /// The reads of what comes up, in [`Self::playlist_calls`]: only the
    /// last one asked for is sent.
    upcoming_reads: Latest,
    /// The playlist cursor's calls, in the order asked: the cursor runs
    /// them in turn too, but each here reads the entry and the modes as
    /// it is sent, after the calls before it have moved them.
    playlist_calls: Serial,
    /// A save of the modes is queued in [`Self::playlist_calls`] and not
    /// yet sent: a change meanwhile is sent with it.
    saving_modes: Cell<bool>,
    /// Its next entry is being asked for: once at a time.
    asking_playlist: Cell<bool>,
    /// Itself, for callbacks that outlive a borrow (the track menu's).
    me: Weak<Self>,
    /// A download's places and its watched mark, in order. Apart from
    /// [`Self::playlist_calls`]: neither waits on the other.
    progress_calls: Serial,
    /// The server's name, as Search for subtitles says it (PX-02).
    server_name: RefCell<String>,
    /// The servers a paired item plays from (P8-04), by name.
    servers: RefCell<Option<Rc<crate::federation::Servers>>>,
}

/// What belongs to the item playing now, and lives exactly as long as it
/// plays: moving on builds a new one. Work in flight holds it weakly, so
/// an answer for an item already left finds nothing and is dropped; where
/// it is within the item (restarting, seeking) is its [`Control`]'s.
struct Current {
    item: Item,
    /// The catalog's length: a converted output's own is unknown while it
    /// is encoded.
    catalog_length: Option<Duration>,
    /// The tracks to ask the server for; `None` lets it choose.
    requested: Option<Selection>,
    /// When the item was asked for, for the timings in the log.
    asked: Instant,
    /// Whether the first picture has been logged.
    pictured: Cell<bool>,
    /// The SDK's controller of the item, once it plays; `None` while the
    /// server is asked.
    control: RefCell<Option<Control>>,
    /// Readers GstPlay was given for the item: the first, then one per
    /// converted restart, re-plan or read anew.
    loads: Cell<u64>,
    /// A seek the app makes once the length is known: a download's
    /// resume, or a reader read anew. A server playback's own resume is
    /// its controller's.
    resume: Cell<Option<Duration>>,
    /// The playback the server opened, for its subtitle.
    managed: RefCell<Option<ManagedPlayback>>,
    /// How the server plays the item, for the diagnostics.
    plan: RefCell<Option<PlaybackPlan>>,
    track_list: RefCell<Option<MediaTracks>>,
    /// The tracks that play, to pick once the file's streams are known.
    pending: Cell<Option<Selection>>,
    /// The tracks that play, as asked or as the server chose: what a
    /// change of subtitle track keeps the audio of.
    selected: Cell<Option<Selection>>,
    media_info: RefCell<Option<gst_play::PlayMediaInfo>>,
    /// The markers' button, as the controller's tracker last said.
    marked: Cell<Option<MarkerOffer>>,
    /// Up next is due, as the tracker last said: the app has no card for
    /// it, so Next shows in its place.
    up_next: Cell<bool>,
    /// The button over the video now: emitted only when it changes.
    offered: Cell<Option<MarkerOffer>>,
    /// A download played to its end and marked watched: where it stops
    /// now is not a place to resume from, until a seek back into it.
    ended: Cell<bool>,
    /// The player has played or sought in it: its pauses are the
    /// viewer's, for the server's playback ([`Playback::tell_player`]).
    told: Cell<bool>,
    /// An external subtitle, written where GstPlay can open it.
    subtitle_file: RefCell<Option<PathBuf>>,
    failure: RefCell<Option<String>>,
}

/// The SDK's controller of the item playing: of the server's playback,
/// which restarts a converted encode on a seek (P2-06) and follows a
/// re-plan, or of a download's file (PX-38). Each follows the item's
/// markers (P2-10) and holds a seek's target until it lands.
///
/// A converted restart plays the new encode from its start with a new
/// reader: matroskademux does not pass a time seek up to a source it
/// cannot seek in bytes (checked with a spike), and each encode is a
/// fresh live stream without an index. The output keeps the film's own
/// time (the server's -copyts), but GstPlay's positions in it start at 0:
/// the pipeline adds where it starts back (`Pipeline::film_time`).
enum Control {
    Server(PlaybackController),
    /// Boxed: it holds its markers itself, the server's behind an `Rc`.
    Local(Box<LocalController>),
}

impl Control {
    /// Whether a converted restart is under way: GstPlay's positions are
    /// the old encode's.
    fn restarting(&self) -> bool {
        matches!(self, Self::Server(c) if c.restarting())
    }

    fn seek(&self, to: Duration) {
        match self {
            Self::Server(c) => c.seek_coalesced(to),
            Self::Local(c) => c.seek_to(to),
        }
    }

    fn length_known(&self, length: Duration) {
        match self {
            Self::Server(c) => c.length_known(length),
            Self::Local(c) => c.length_known(length),
        }
    }

    fn seek_done(&self) {
        match self {
            Self::Server(c) => c.seek_done(),
            Self::Local(c) => c.seek_done(),
        }
    }

    /// What the markers say at `at`; an intro skipped by itself is sought
    /// by the controller.
    fn on_position(&self, at: Duration, has_next: bool, auto_skip: bool) -> Vec<MarkerAction> {
        match self {
            Self::Server(c) => {
                c.set_has_next(has_next);
                c.set_auto_skip(auto_skip);
                c.on_position(at)
            }
            Self::Local(c) => {
                c.set_has_next(has_next);
                c.set_auto_skip(auto_skip);
                c.on_position(at)
            }
        }
    }

    /// Where views show the player at `at`: a seek's target while it is
    /// in flight.
    fn position(&self, at: Duration) -> Duration {
        match self {
            Self::Server(c) => c.position(at),
            Self::Local(c) => c.position(at),
        }
    }

    fn undoable(&self) -> bool {
        match self {
            Self::Server(c) => c.undoable(),
            Self::Local(c) => c.undoable(),
        }
    }

    /// Back into the intro skipped by itself: where, or `None` with none
    /// to undo.
    fn undo_skip(&self) -> Option<Duration> {
        match self {
            Self::Server(c) => c.undo_skip(),
            Self::Local(c) => c.undo_skip(),
        }
    }

    fn set_auto_skip(&self, on: bool) {
        match self {
            Self::Server(c) => c.set_auto_skip(on),
            Self::Local(c) => c.set_auto_skip(on),
        }
    }
}

/// GstPlay, as the controller of one server playback drives it: only
/// while that item is the one playing.
struct Driver {
    playback: Weak<Playback>,
    current: Weak<Current>,
}

impl Driver {
    fn playing(&self) -> Option<(Rc<Playback>, Rc<Current>)> {
        let (playback, current) = (self.playback.upgrade()?, self.current.upgrade()?);
        playback.is_current(&current).then_some((playback, current))
    }

    /// The same, once GstPlay has a reader of it.
    fn loaded(&self) -> Option<(Rc<Playback>, Rc<Current>)> {
        self.playing()
            .filter(|(_, current)| current.loads.get() > 0)
    }
}

impl PlaybackDriver for Driver {
    fn load(&self, next: spoolframe_client_sdk::Playback, resume: Option<Duration>) {
        if let Some((playback, current)) = self.playing() {
            playback.load(&current, next, resume);
        }
    }

    fn play(&self) {
        if let Some((playback, _)) = self.loaded() {
            playback.pipeline.play_now();
        }
    }

    fn pause(&self) {
        if let Some((playback, _)) = self.loaded() {
            playback.pipeline.pause();
        }
    }

    fn seek(&self, to: Duration) {
        if let Some((playback, _)) = self.loaded() {
            playback.seek_file(to);
        }
    }

    fn stop_and_wait(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
        match self.playing() {
            Some((playback, _)) => Box::pin(playback.pipeline.stop_and_wait()),
            None => Box::pin(std::future::ready(())),
        }
    }

    fn failed(&self, error: SdkError) {
        if let Some((playback, _)) = self.playing() {
            playback.failed(&backend_error(error).to_string());
        }
    }
}

/// The controller's own work runs on the main loop, beside GstPlay's
/// signals. Its timers run on the playback's runtime.
fn local_spawn() -> LocalSpawn {
    Rc::new(|future| {
        glib::spawn_future_local(future);
    })
}

impl Current {
    fn new(item: Item, requested: Option<Selection>, tracks: Option<MediaTracks>) -> Self {
        Self {
            catalog_length: crate::views::Facts::of(&item).length,
            item,
            requested,
            asked: Instant::now(),
            pictured: Cell::new(false),
            control: RefCell::new(None),
            loads: Cell::new(0),
            resume: Cell::new(None),
            managed: RefCell::new(None),
            plan: RefCell::new(None),
            track_list: RefCell::new(tracks),
            pending: Cell::new(None),
            selected: Cell::new(None),
            media_info: RefCell::new(None),
            marked: Cell::new(None),
            up_next: Cell::new(false),
            offered: Cell::new(None),
            ended: Cell::new(false),
            told: Cell::new(false),
            subtitle_file: RefCell::new(None),
            failure: RefCell::new(None),
        }
    }

    fn is_converted(&self) -> bool {
        self.managed
            .borrow()
            .as_ref()
            .is_some_and(ManagedPlayback::is_converted)
    }

    /// Whether a converted restart is under way.
    fn restarting(&self) -> bool {
        self.control
            .borrow()
            .as_ref()
            .is_some_and(Control::restarting)
    }

    /// Whether the intro skipped by itself can still be undone, as its
    /// controller says.
    fn undoable(&self) -> bool {
        self.control
            .borrow()
            .as_ref()
            .is_some_and(Control::undoable)
    }

    /// Milliseconds since the item was asked for.
    fn since_asked(&self) -> u64 {
        self.asked.elapsed().as_millis() as u64
    }
}

/// The button over the video: the markers', else Next once Up next is
/// due, as the app has no Up next card. Without it an episode with no
/// credits marker running to its end (most of Plex's) offered nothing
/// and went on to the next by itself.
fn shown_offer(marked: Option<MarkerOffer>, up_next: bool) -> Option<MarkerOffer> {
    marked.or(up_next.then_some(MarkerOffer::Next))
}

impl Drop for Current {
    fn drop(&mut self) {
        if let Some(path) = self.subtitle_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// How many of the playlist's entries after the one playing Up next shows
/// at most: All is the whole library, and each shown is a row built anew
/// on every change.
pub const PREVIEW_AHEAD: usize = 50;

/// Calls of which only the last one asked for is needed: each asked for
/// gets a number, and one that is no longer the last is not sent.
#[derive(Default)]
struct Latest(Cell<u64>);

impl Latest {
    /// A new call's number: it is the last now.
    fn ask(&self) -> u64 {
        let n = self.0.get().wrapping_add(1);
        self.0.set(n);
        n
    }

    /// Whether `n` is still the last asked for.
    fn is(&self, n: u64) -> bool {
        self.0.get() == n
    }
}

/// The server's playlist in Up next.
#[derive(Debug, PartialEq)]
struct PlaylistAhead {
    heading: String,
    entries: Vec<PlaylistEntry>,
    note: Option<&'static str>,
}

/// What Up next shows of playlist `name` under `settings`, from what the
/// SDK's cursor says plays after the entry playing (`up`): the server's
/// order, shuffled or not, so what is listed is what plays, each once and
/// [`PREVIEW_AHEAD`] at most. With Repeat One it is the entry playing,
/// which plays again. From a server that cannot say what comes up, a
/// shuffled playlist names no songs: the playlist's own order would be a
/// list of songs that do not play next.
fn playlist_ahead(name: &str, settings: PlaySettings, up: PlaylistUpNext) -> PlaylistAhead {
    let heading = match settings.shuffle {
        true => format!("In {name} · Shuffle on"),
        false => format!("Next from {name}"),
    };
    let note = match up.note {
        UpNextNote::PlaysAgain => Some("This song plays again"),
        UpNextNote::Shuffled => Some("Songs play in a shuffled order"),
        UpNextNote::None | UpNextNote::Ends => None,
    };
    PlaylistAhead {
        heading,
        entries: up.entries,
        note,
    }
}

/// Up next's playlist part, from the entry `cursor` plays.
async fn read_up_next(cursor: &PlaylistCursor) -> Result<PlaylistUpNext, SdkError> {
    cursor.up_next(PREVIEW_AHEAD).await
}

/// Calls to the server run one after another, each once the one before
/// has finished. A job reads what it needs as it starts, not when it was
/// queued.
#[derive(Default)]
struct Serial {
    tail: RefCell<Option<tokio::sync::oneshot::Receiver<()>>>,
}

/// A call to the playlist's cursor, in [`Playback::playlist_calls`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    /// What follows the entry playing.
    Next,
    /// Play from this entry.
    Pick(EntryId),
    /// Save the modes, from the entry playing.
    Save,
    /// Up next's playlist part, the `n`th read asked for.
    Read(u64),
    /// Follow the playlist anew from the entry playing (it changed on
    /// the server), and read Up next.
    Follow,
}

/// What a [`Call`] got from the cursor.
enum Answer {
    /// The entry it moved to, and Up next from it, read at once.
    Moved(
        Result<Option<Box<PlaylistEntry>>, SdkError>,
        Option<Result<PlaylistUpNext, SdkError>>,
    ),
    Saved(Result<(), SdkError>),
    Read(Result<PlaylistUpNext, SdkError>),
}

/// Waits for the end of the job before, if there is one. A job dropped
/// unfinished lets the next run all the same.
async fn after(previous: Option<tokio::sync::oneshot::Receiver<()>>) {
    if let Some(previous) = previous {
        let _ = previous.await;
    }
}

impl Serial {
    /// Joins the line: the end of the job before, and what tells the
    /// next once this one is done.
    fn join(
        &self,
    ) -> (
        Option<tokio::sync::oneshot::Receiver<()>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (done, tail) = tokio::sync::oneshot::channel();
        (self.tail.replace(Some(tail)), done)
    }

    /// Runs `job` on the main loop after the jobs before it, each one's
    /// answer taken in before the next starts.
    fn queue<F>(&self, job: impl FnOnce() -> F + 'static)
    where
        F: std::future::Future<Output = ()> + 'static,
    {
        let (previous, done) = self.join();
        glib::spawn_future_local(async move {
            after(previous).await;
            job().await;
            let _ = done.send(());
        });
    }

    /// Runs `call` on the runtime after the calls before it.
    fn send(
        &self,
        call: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let (previous, done) = self.join();
        relm4::spawn(async move {
            after(previous).await;
            call.await;
            let _ = done.send(());
        })
    }

    /// Done once every job queued so far is.
    #[cfg(test)]
    fn idle(&self) -> tokio::sync::oneshot::Receiver<()> {
        let (done, idle) = tokio::sync::oneshot::channel();
        self.queue(move || async move {
            let _ = done.send(());
        });
        idle
    }
}

impl Playback {
    /// Starts the playlist.
    pub fn start(playlist: Playlist) -> anyhow::Result<Rc<Self>> {
        let queue = Queue::new(
            playlist.items,
            playlist.index,
            playlist.album,
            playlist.queued,
        );
        let start = Start {
            from: playlist.from,
            selection: playlist.selection,
            tracks: playlist.tracks,
            quality: playlist.quality,
            display: playlist.display,
        };
        let playback = Self::build(Some(playlist.session), None, queue, start, true)?;
        tracing::info!(playlist = ?playlist.from_playlist, "starting playback context");
        if let Some(from) = playlist.from_playlist {
            playback.play_from(from);
        }
        Ok(playback)
    }

    /// Plays download `item` from its file (PX-38), from `from`, with the
    /// file's tracks `selection` (see
    /// `LocalDownload::file_selection`); `None` plays
    /// the file's default audio, without subtitles. No server is asked for
    /// anything: it plays offline.
    pub fn play_local(
        item: LocalItem,
        from: Option<Duration>,
        selection: Option<Selection>,
        display: Option<spoolframe_types::DisplayCapabilities>,
    ) -> anyhow::Result<Rc<Self>> {
        let queue = Queue::new(vec![item.item.clone()], 0, None, Vec::new());
        let start = Start {
            from,
            selection,
            tracks: Some(item.tracks.clone()),
            quality: Default::default(),
            display,
        };
        tracing::info!(download = %item.key, "playing a download");
        Self::build(None, Some(item), queue, start, true)
    }

    /// Whether a download plays, from this computer.
    pub fn is_local(&self) -> bool {
        self.local.is_some()
    }

    /// Plays on from the server's playlist `from`, with the settings it
    /// was started (or last saved) with. They are taken as they are, not
    /// written: saving them would ask the server to play from the entry
    /// given, moving the user's place (see [`Self::set_playlist_settings`]).
    fn play_from(self: &Rc<Self>, from: crate::saved_queue::PlaylistResume) {
        self.playlist.replace(Some(PlaylistContext {
            playing: from.playing,
            modes: from.settings,
            cursor: None,
            up_next: None,
        }));
        self.follow_ahead();
    }

    /// The server's playlist being played, if one is.
    pub fn playing_from(&self) -> Option<Playing> {
        self.playlist.borrow().as_ref().map(|p| p.playing.clone())
    }

    fn in_playlist(&self) -> bool {
        self.playlist.borrow().is_some()
    }

    /// Whether playlist `id` is the one being played.
    pub fn plays_playlist(&self, id: &str) -> bool {
        self.playlist
            .borrow()
            .as_ref()
            .is_some_and(|p| p.playing.id == id)
    }

    /// "Playing from <playlist>", under what plays, while one is.
    pub fn source_label(&self) -> Option<String> {
        self.playlist.borrow().as_ref().map(|p| p.playing.line())
    }

    /// The playlist no longer says what follows: the queue and the
    /// context do, as before it. What it was, if anything.
    fn leave_playlist(&self) -> Option<Playing> {
        self.playlist.take().map(|p| p.playing)
    }

    /// The item of the playlist's entry playing: the context's, under a
    /// queued item playing before the playlist goes on.
    fn playlist_item(&self) -> Option<Item> {
        let queue = self.queue.borrow();
        match queue.current_is_queued() {
            true => queue
                .context_behind()
                .last()
                .map(|(_, item)| (*item).clone()),
            false => queue.current().cloned(),
        }
    }

    /// Reads Up next's playlist part again once the entry playing or the
    /// modes have changed, in line after the calls before it. A read
    /// asked for while another waits replaces it.
    fn follow_ahead(&self) {
        if self.in_playlist() {
            self.playlist_call(Call::Read(self.upcoming_reads.ask()));
        }
    }

    /// Playlist `id` changed on the server (an edit, here or elsewhere):
    /// if it is the one playing, it is followed anew from the entry
    /// playing, and what comes up in it read again.
    pub fn playlist_changed(&self, id: &str) {
        if self.plays_playlist(id) {
            self.playlist_call(Call::Follow);
        }
    }

    /// What Up next shows of the playlist (see [`playlist_ahead`]),
    /// without changing the server's play position.
    fn playlist_up_next(&self) -> Option<PlaylistAhead> {
        let context = self.playlist.borrow();
        let context = context.as_ref()?;
        Some(playlist_ahead(
            &context.playing.name,
            context.modes?,
            context.up_next.clone()?,
        ))
    }

    pub fn play_playlist_entry(self: &Rc<Self>, entry: EntryId) {
        if self.playlist_settings().is_none() || self.asking_playlist.get() {
            return;
        }
        // The entry it plays closes this one, with its last report.
        self.playlist_call(Call::Pick(entry));
    }

    /// Queues `call` in [`Self::playlist_calls`]: one Next or pick out at
    /// a time, and one save waiting at a time, which takes in the
    /// changes made meanwhile. Each reads the entry playing and the
    /// modes as it is sent, after the calls before it have moved them,
    /// and starts the playlist's cursor from the entry playing when none
    /// follows it.
    fn playlist_call(&self, call: Call) {
        let waiting = match call {
            Call::Next | Call::Pick(_) => Some(&self.asking_playlist),
            Call::Save => Some(&self.saving_modes),
            Call::Read(_) | Call::Follow => None,
        };
        if waiting.is_some_and(|waiting| waiting.replace(true)) {
            return;
        }
        let weak = self.me.clone();
        self.playlist_calls.queue(move || async move {
            let Some(playback) = weak.upgrade() else {
                return;
            };
            match call {
                Call::Save => playback.saving_modes.set(false),
                Call::Read(n) if !playback.upcoming_reads.is(n) => return,
                _ => {}
            }
            let read = playback.playlist.borrow().as_ref().map(|c| {
                let cursor = c.cursor.clone().filter(|_| call != Call::Follow);
                (c.playing.clone(), c.modes, cursor)
            });
            // Next and a read go by the server's modes while they are not
            // known; a pick and a save send them, so wait for them.
            let sent = match (call, read) {
                (Call::Pick(_) | Call::Save, Some((_, None, _))) => None,
                (_, read) => read.zip(playback.playlist_item()).zip(playback.session.clone()),
            };
            let Some((((playing, modes, cursor), item), session)) = sent else {
                playback.asked(call);
                return;
            };
            if call == Call::Next {
                tracing::info!(playlist = %playing.id, entry = ?playing.entry, "requesting next playlist entry");
            }
            drop(playback);
            let asked = playing.clone();
            let answer = relm4::spawn(async move {
                let cursor = match cursor {
                    Some(cursor) => cursor,
                    None => {
                        let entry = PlaylistNext {
                            entry: asked.entry,
                            item,
                            markers: Default::default(),
                            liked: false,
                        };
                        let follow = managed::follow_playlist(session, &asked.id, entry, modes);
                        Arc::new(follow.await?)
                    }
                };
                let answer = match (call, modes) {
                    (Call::Next | Call::Pick(_), _) => {
                        let moved = match call {
                            Call::Pick(entry) => cursor.pick(entry).await,
                            _ => cursor.advance().await,
                        }
                        .map(|entry| entry.map(Box::new));
                        let up = match moved {
                            Ok(Some(_)) => Some(read_up_next(&cursor).await),
                            _ => None,
                        };
                        Answer::Moved(moved, up)
                    }
                    (Call::Save, Some(modes)) => Answer::Saved(cursor.set_settings(modes).await),
                    (Call::Save, None) => Answer::Saved(Ok(())),
                    (Call::Read(_) | Call::Follow, _) => Answer::Read(read_up_next(&cursor).await),
                };
                Ok::<_, SdkError>((cursor, answer))
            })
            .await;
            // Taken in before the next call runs: it reads the entry.
            let Some(playback) = weak.upgrade() else {
                return;
            };
            match answer {
                Ok(Ok((cursor, answer))) => playback.answered(call, &playing, cursor, answer),
                Ok(Err(error)) => playback.playlist_failed(call, &playing, &error),
                Err(error) => {
                    playback.asked(call);
                    tracing::warn!(%error, ?call, "the playlist's call did not finish");
                }
            }
        });
    }

    /// `call` is no longer out.
    fn asked(&self, call: Call) {
        if matches!(call, Call::Next | Call::Pick(_)) {
            self.asking_playlist.set(false);
        }
    }

    /// `call` failed, the cursor's start or the call itself.
    fn playlist_failed(&self, call: Call, playing: &Playing, error: &SdkError) {
        self.asked(call);
        tracing::warn!(%error, ?call, "the playlist's call failed");
        if self.stopped.get() {
            return;
        }
        match call {
            // Next asks again.
            Call::Next | Call::Pick(_) => self.emit(&Change::Skipped(&format!(
                "Couldn’t go on with {}: {error}",
                playing.name
            ))),
            Call::Save => self.emit(&Change::Skipped("Couldn’t save playlist playback settings")),
            Call::Read(_) | Call::Follow => {}
        }
    }

    /// The cursor answered `call`, asked while `playing` played. The
    /// cursor it started, if none was kept, is kept; one followed anew
    /// replaces the one kept.
    fn answered(
        self: &Rc<Self>,
        call: Call,
        playing: &Playing,
        cursor: Arc<PlaylistCursor>,
        answer: Answer,
    ) {
        self.asked(call);
        if self.stopped.get() {
            return;
        }
        let shown = self.playlist_up_next();
        let found_modes = {
            let mut context = self.playlist.borrow_mut();
            let Some(context) = context.as_mut().filter(|c| c.playing.id == playing.id) else {
                return;
            };
            if context.cursor.is_none() || call == Call::Follow {
                context.cursor = Some(cursor.clone());
            }
            if let Answer::Read(Ok(up)) | Answer::Moved(_, Some(Ok(up))) = &answer {
                context.up_next = Some(up.clone());
            }
            context
                .modes
                .replace(context.modes.unwrap_or(cursor.settings()))
                .is_none()
        };
        if found_modes {
            self.emit_modes();
        }
        match answer {
            // A save never changes what plays; what is to come is read
            // again after it.
            Answer::Saved(Ok(())) => {}
            Answer::Moved(Ok(entry), _) => {
                self.playlist_next(playing.entry, entry);
                if self.playlist_up_next() != shown {
                    self.emit(&Change::Queue);
                }
            }
            Answer::Saved(Err(error)) | Answer::Moved(Err(error), _) => {
                self.playlist_failed(call, playing, &error)
            }
            Answer::Read(Ok(_)) => {
                // Told only when what shows changed: a read again for the
                // same entries rebuilds nothing.
                if self.playlist_up_next() != shown {
                    self.emit(&Change::Queue);
                }
            }
            Answer::Read(Err(error)) => {
                tracing::warn!(%error, "cannot read what comes up in the playlist")
            }
        }
    }

    pub fn playlist_resume(&self) -> Option<crate::saved_queue::PlaylistResume> {
        Some(crate::saved_queue::PlaylistResume {
            playing: self.playing_from()?,
            settings: self.playlist_settings(),
        })
    }

    /// Brings back a queue saved at the last run (P1-23): the current item
    /// prerolled at `at`, paused, waiting for Play, at `quality`.
    pub fn restore(
        session: Arc<dyn Session>,
        queue: Queue,
        at: Duration,
        playlist: Option<crate::saved_queue::PlaylistResume>,
        display: Option<spoolframe_types::DisplayCapabilities>,
        quality: spoolframe_client_sdk::PlaybackQuality,
    ) -> anyhow::Result<Rc<Self>> {
        let start = Start {
            from: (!at.is_zero()).then_some(at),
            selection: None,
            tracks: None,
            quality,
            display,
        };
        let playback = Self::build(Some(session), None, queue, start, false)?;
        if let Some(playlist) = playlist {
            playback.play_from(playlist);
        }
        Ok(playback)
    }

    /// The queue as it stands, to save.
    pub fn snapshot(&self) -> Queue {
        self.queue.borrow().clone()
    }

    fn build(
        session: Option<Arc<dyn Session>>,
        local: Option<LocalItem>,
        queue: Queue,
        start: Start,
        autoplay: bool,
    ) -> anyhow::Result<Rc<Self>> {
        let pipeline = Pipeline::new()?;
        let playback = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            pipeline,
            player: session.clone().map(|session| {
                session
                    .player_session()
                    .unwrap_or_else(|| PlayerSession::new(managed::backend(session)))
            }),
            session,
            local,
            queue: RefCell::new(queue),
            current: RefCell::new(None),
            held: Cell::new(false),
            autoplay: Cell::new(autoplay),
            skip_intros: Cell::new(false),
            subtitles_on: Rc::new(Cell::new(false)),
            quality: Cell::new(start.quality),
            display: RefCell::new(start.display.clone()),
            playing: Cell::new(false),
            position: Cell::new(Duration::ZERO),
            length: Cell::new(Duration::ZERO),
            stopped: Cell::new(false),
            listeners: RefCell::new(Vec::new()),
            playlist: RefCell::new(None),
            asking_playlist: Cell::new(false),
            upcoming_reads: Latest::default(),
            playlist_calls: Serial::default(),
            saving_modes: Cell::new(false),
            progress_calls: Serial::default(),
            server_name: RefCell::new("the server".to_owned()),
            servers: RefCell::new(None),
        });
        connect(playback.pipeline.adapter(), &playback);
        {
            let weak = Rc::downgrade(&playback);
            gtk::prelude::PaintableExt::connect_invalidate_contents(
                playback.pipeline.paintable(),
                move |_| {
                    if let Some(current) = weak.upgrade().and_then(|p| p.now())
                        && !current.pictured.replace(true)
                    {
                        tracing::debug!(ms = current.since_asked(), "first picture");
                    }
                },
            );
        }
        LIVE.with(|live| {
            let mut live = live.borrow_mut();
            live.retain(|p| p.strong_count() > 0);
            live.push(Rc::downgrade(&playback));
        });

        // A server playback reports itself, from what the player does
        // (see `Playback::tell_player`).
        if playback.is_local() {
            let weak = Rc::downgrade(&playback);
            glib::timeout_add_seconds_local(PROGRESS_EVERY, move || {
                let Some(playback) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if playback.playing.get() {
                    playback.keep_place();
                }
                glib::ControlFlow::Continue
            });
        }
        playback.begin(Some(start));
        Ok(playback)
    }

    /// The current item's own state.
    fn now(&self) -> Option<Rc<Current>> {
        self.current.borrow().clone()
    }

    /// Whether `current` is still the item playing.
    fn is_current(&self, current: &Rc<Current>) -> bool {
        self.current
            .borrow()
            .as_ref()
            .is_some_and(|c| Rc::ptr_eq(c, current))
    }

    /// Calls `listener` with the current state now and with every change
    /// after, until it returns false or the playback is dropped.
    pub fn subscribe(&self, listener: impl Fn(&Change) -> bool + 'static) {
        let current = self.now();
        let replay = || -> bool {
            if let Some(item) = self.current()
                && !listener(&Change::Item(&item))
            {
                return false;
            }
            if let Some(current) = &current {
                if let Some(info) = current.media_info.borrow().as_ref()
                    && !listener(&Change::MediaInfo(info))
                {
                    return false;
                }
                if let Some(error) = current.failure.borrow().as_deref() {
                    return listener(&Change::Failed(error));
                }
            }
            listener(&Change::Playing(self.playing.get()))
                && listener(&Change::Volume(self.pipeline.volume()))
                && listener(&Change::Muted(self.pipeline.is_muted()))
                && listener(&Change::Modes {
                    shuffle: self.is_shuffled(),
                    repeat: self.repeat(),
                })
                && listener(&Change::Position {
                    at: self.position.get(),
                    length: self.length.get(),
                })
                && listener(&Change::Offer(
                    current.as_ref().and_then(|c| c.offered.get()),
                ))
                && (!current.as_ref().is_some_and(|c| c.undoable())
                    || listener(&Change::IntroSkipped))
        };
        if replay() {
            self.listeners.borrow_mut().push(Box::new(listener));
        }
    }

    fn emit(&self, change: &Change) {
        // Listeners may subscribe while they run: they go to the emptied
        // list and are kept after.
        let mut listeners = std::mem::take(&mut *self.listeners.borrow_mut());
        listeners.retain(|listener| listener(change));
        let mut current = self.listeners.borrow_mut();
        listeners.append(&mut current);
        *current = listeners;
    }

    pub fn current(&self) -> Option<Item> {
        self.queue.borrow().current().cloned()
    }

    /// The album of what plays, for its cover and name: the context's, or
    /// a queued track's own.
    pub fn album(&self) -> Option<Album> {
        self.queue.borrow().current_album().cloned()
    }

    /// What plays after the current item: the queue, then the context,
    /// then the server's playlist.
    pub fn up_next(&self) -> UpNext {
        let queue = self.queue.borrow();
        let mut ahead = Vec::new();
        let context = queue.context_ahead();
        if !context.is_empty() {
            let album = queue.album().cloned();
            ahead.push(Ahead {
                heading: match &album {
                    Some(album) => format!("Next from {}", album.title),
                    None => "Next".to_owned(),
                },
                album,
                entries: context
                    .into_iter()
                    .map(|(pos, item)| (Place::Context(pos), item.clone()))
                    .collect(),
                note: None,
            });
        }
        if let Some(playlist) = self.playlist_up_next()
            && (!playlist.entries.is_empty() || playlist.note.is_some())
        {
            ahead.push(Ahead {
                heading: playlist.heading,
                album: None,
                entries: playlist
                    .entries
                    .into_iter()
                    .map(|entry| (Place::Playlist(entry.entry), entry.item))
                    .collect(),
                note: playlist.note,
            });
        }
        UpNext {
            queued: queue.queued().cloned().collect(),
            ahead,
        }
    }

    /// Plays what is at `place` (from [`UpNext::ahead`]), keeping the
    /// queue.
    pub fn play_ahead(self: &Rc<Self>, place: Place) {
        match place {
            Place::Context(pos) => self.play_context_at(pos),
            Place::Playlist(entry) => self.play_playlist_entry(entry),
        }
    }

    /// The queue, to carry into the playback that replaces this one.
    pub fn carry(&self) -> Vec<Queued> {
        self.queue.borrow().carry()
    }

    /// Plays the queued item `id`; it leaves the queue.
    pub fn play_queued(self: &Rc<Self>, id: u64) {
        self.keep_place();
        self.step(|q| q.play_queued(id));
    }

    /// Jumps the context to `pos` (from [`UpNext::ahead`]), keeping the
    /// queue.
    pub fn play_context_at(self: &Rc<Self>, pos: usize) {
        self.keep_place();
        // Picked by hand: the server's playlist no longer says what follows.
        self.leave_playlist();
        self.step(|q| q.play_context_at(pos));
    }

    /// "Play next": `items` (of `album`) after the current item.
    pub fn play_next(&self, items: Vec<Item>, album: Option<Album>) {
        self.queue.borrow_mut().play_next(items, album);
        self.emit(&Change::Queue);
    }

    /// "Add to queue": `items` (of `album`) after what is queued already.
    pub fn add_to_queue(&self, items: Vec<Item>, album: Option<Album>) {
        self.queue.borrow_mut().add(items, album);
        self.emit(&Change::Queue);
    }

    pub fn remove_queued(&self, id: u64) {
        if self.queue.borrow_mut().remove(id) {
            self.emit(&Change::Queue);
        }
    }

    pub fn move_queued(&self, id: u64, index: usize) {
        self.queue.borrow_mut().move_to(id, index);
        self.emit(&Change::Queue);
    }

    pub fn clear_queue(&self) {
        self.queue.borrow_mut().clear();
        self.emit(&Change::Queue);
    }

    /// The modes of the server's playlist being played, once known.
    pub fn playlist_settings(&self) -> Option<PlaySettings> {
        self.playlist.borrow().as_ref()?.modes
    }

    /// Saves the playlist's modes without restarting the current stream.
    ///
    /// The cursor saves them alone, keeping the server's place and round,
    /// and reads what is to come anew. The save goes in line with Next and
    /// picked entries ([`Self::playlist_calls`]) and reads the entry and
    /// the modes as it is sent, never before: sent beside a Next it could
    /// land after it and put the place back one entry. Changes made while
    /// one waits are sent with it, as one call.
    pub fn set_playlist_settings(&self, settings: PlaySettings) {
        {
            let mut context = self.playlist.borrow_mut();
            let Some(context) = context.as_mut() else {
                return;
            };
            if context.modes.replace(settings) == Some(settings) {
                return;
            }
        }
        self.emit_modes();
        self.emit(&Change::Queue);
        self.playlist_call(Call::Save);
        // Read after the save: shuffle turned on deals a new round, and
        // off, the playlist's order is what plays.
        self.follow_ahead();
    }

    /// Shuffles what is still to come in the context, or puts it back in
    /// order, keeping the current item playing. The queue is never
    /// shuffled.
    pub fn toggle_shuffle(&self) {
        if let Some(settings) = self.playlist_settings() {
            self.set_playlist_settings(PlaySettings {
                shuffle: !settings.shuffle,
                ..settings
            });
            return;
        }
        let on = !self.is_shuffled();
        self.queue
            .borrow_mut()
            .set_shuffle(on, |n| glib::random_int_range(0, n as i32) as usize);
        self.emit_modes();
        self.emit(&Change::Queue);
    }

    pub fn is_shuffled(&self) -> bool {
        self.playlist_settings()
            .map(|s| s.shuffle)
            .unwrap_or_else(|| self.queue.borrow().shuffle())
    }

    /// A playlist's repeat this build does not know plays as off, so it
    /// shows as off.
    pub fn repeat(&self) -> Repeat {
        match self.playlist_settings() {
            Some(settings) if settings.repeat == Repeat::Unknown => Repeat::Off,
            Some(settings) => settings.repeat,
            None => self.queue.borrow().repeat(),
        }
    }

    /// Off, then the whole context, then the one item, then off again.
    pub fn cycle_repeat(&self) {
        if let Some(settings) = self.playlist_settings() {
            self.set_playlist_settings(PlaySettings {
                repeat: crate::queue::next_repeat(settings.repeat),
                ..settings
            });
            return;
        }
        self.queue.borrow_mut().cycle_repeat();
        self.emit_modes();
        self.emit(&Change::Queue);
    }

    fn emit_modes(&self) {
        self.emit(&Change::Modes {
            shuffle: self.is_shuffled(),
            repeat: self.repeat(),
        });
    }

    /// Whether this is video rather than music: by the item, before the
    /// file's streams are known.
    pub fn is_video(&self) -> bool {
        !matches!(self.queue.borrow().current(), Some(Item::Track(_)))
    }

    /// The session the playback plays from, for what views fetch beside
    /// it (preview frames); none for a download.
    pub fn session(&self) -> Option<Arc<dyn Session>> {
        self.session.clone()
    }

    /// The video frames, for any number of pictures.
    pub fn paintable(&self) -> &gtk::gdk::Paintable {
        self.pipeline.paintable()
    }

    /// Shows `choice` over the music: the audio analysed for one of the
    /// canvas's modes, the GStreamer effect for one; neither for the
    /// cover, or `None` while no visualizer can be seen. False, with
    /// nothing shown and the user told, when the effect cannot start.
    pub fn set_visualizer(&self, choice: Option<&crate::visualizer::Choice>) -> bool {
        use crate::visualizer::Choice;
        let (analysis, effect) = match choice {
            Some(Choice::Canvas(_)) => (true, None),
            Some(Choice::Effect(name)) => (false, Some(name.as_str())),
            Some(Choice::Cover) | None => (false, None),
        };
        self.pipeline.set_analysis(analysis);
        self.set_visualization(effect)
    }

    fn set_visualization(&self, name: Option<&str>) -> bool {
        match self.pipeline.set_visualization(name) {
            Ok(()) => true,
            Err(error) => {
                self.emit(&Change::TrackFailed(&format!(
                    "Could not start visualizer: {error}"
                )));
                false
            }
        }
    }

    /// What the canvas draws from, where the audio can be analysed.
    pub fn measurements(&self) -> Option<crate::visualizer::Measurements> {
        self.pipeline.measurements()
    }

    /// Whether a scope leaves fading trails, now and for each one shown.
    pub fn set_visualizer_trails(&self, enabled: bool) {
        self.pipeline.visualization_trails(enabled);
    }

    /// The audio and subtitle menu for `info`. A pick that fails says so
    /// as [`Change::TrackFailed`], as one at the start does.
    pub fn track_menu(&self, info: &gst_play::PlayMediaInfo) -> gtk::Popover {
        let weak = self.me.clone();
        let failed: Rc<dyn Fn(&str)> = Rc::new(move |message| {
            if let Some(this) = weak.upgrade() {
                this.emit(&Change::TrackFailed(message));
            }
        });
        // Quality is a film's or an episode's; music plays as it is, and
        // a download as it was downloaded.
        let quality = (self.is_video() && !self.is_local()).then(|| {
            let (current, choose) = (self.me.clone(), self.me.clone());
            crate::tracks::QualityMenu {
                current: Rc::new(move || {
                    current
                        .upgrade()
                        .map_or((Default::default(), None), |this| {
                            let height = this.plan().and_then(|plan| plan.encoded_height());
                            (this.quality.get(), height)
                        })
                }),
                choose: Rc::new(move |quality| {
                    if let Some(this) = choose.upgrade() {
                        this.set_quality(quality);
                    }
                }),
            }
        });
        self.pipeline.track_menu(
            info,
            &self.subtitles_on,
            failed,
            quality,
            crate::tracks::SubtitleExtras {
                find: self.subtitle_search(),
                timing: self.timing_menu(),
                elsewhere: self.subtitles_elsewhere(),
                streams: self.subtitle_streams(info),
                picked: Some(self.subtitle_picked()),
            },
        )
    }

    /// The item's track each of `info`'s subtitle streams is, with the
    /// server's name for it (see [`crate::tracks::stream_tracks`]). A
    /// download has no plan: its streams keep their own names.
    fn subtitle_streams(&self, info: &gst_play::PlayMediaInfo) -> Vec<Option<(u32, String)>> {
        let Some(current) = self.now() else {
            return Vec::new();
        };
        let (plan, tracks) = (current.plan.borrow(), current.track_list.borrow());
        let (Some(plan), Some(tracks)) = (plan.as_ref(), tracks.as_ref()) else {
            return Vec::new();
        };
        let ids: Vec<String> = info
            .subtitle_streams()
            .iter()
            .map(|s| gst_play::prelude::PlayStreamInfoExt::stream_id(s).to_string())
            .collect();
        let external = self.pipeline.external_stream();
        crate::tracks::stream_tracks(tracks, plan, &ids, external.as_deref())
            .into_iter()
            .map(|track| {
                let named = tracks.subtitles.get(track? as usize)?;
                Some((track?, spoolframe_linux_core::format::subtitle_track(named)))
            })
            .collect()
    }

    /// A pick from the menu is what shows now: Timing, a timing change and
    /// the next track asked for go by it, as by one chosen at the start.
    fn subtitle_picked(&self) -> Rc<dyn Fn(Option<u32>)> {
        let me = self.me.clone();
        Rc::new(move |track| {
            let Some(current) = me.upgrade().and_then(|this| this.now()) else {
                return;
            };
            let selection = current.selected.get().unwrap_or_default();
            current.selected.set(Some(Selection {
                subtitle: track,
                ..selection
            }));
        })
    }

    /// The item's subtitle tracks this playback has no stream of (see
    /// [`subtitles_not_in_stream`]), with a server to start it again
    /// with one: a download plays its own file's tracks.
    fn subtitles_elsewhere(&self) -> Option<crate::tracks::ServerSubtitles> {
        if self.is_local() {
            return None;
        }
        let current = self.now()?;
        let plan = current.plan.borrow().clone()?;
        let tracks = current.track_list.borrow();
        let tracks = tracks.as_ref()?;
        let named: Vec<(u32, String)> = tracks::offered_but_not_in_stream(tracks, &plan)
            .into_iter()
            .map(|n| {
                let track = &tracks.subtitles[n as usize];
                (n, spoolframe_linux_core::format::subtitle_track(track))
            })
            .collect();
        if named.is_empty() {
            return None;
        }
        let me = self.me.clone();
        Some(crate::tracks::ServerSubtitles {
            tracks: named,
            pick: Rc::new(move |track| {
                if let Some(this) = me.upgrade() {
                    this.use_subtitle(track);
                }
            }),
        })
    }

    /// The server's name, for what the player says of it.
    pub fn set_server_name(&self, name: &str) {
        self.server_name.replace(name.to_owned());
    }

    /// The servers by name, for what the player says of a paired item.
    pub fn set_servers(&self, servers: Rc<crate::federation::Servers>) {
        self.servers.replace(Some(servers));
    }

    /// How a paired server's item plays now (P8-04): from that server, or
    /// through this one, with that server's name and this one's. `None`
    /// for this server's own item, or nothing playing.
    pub fn remote_path(
        &self,
    ) -> Option<(spoolframe_linux_core::backend::RemoteMode, String, String)> {
        let mode = self.now()?.managed.borrow().as_ref()?.route()?;
        let servers = self.servers.borrow().clone()?;
        let server = self
            .current()
            .and_then(|item| servers.of(item.id()))
            .map_or_else(|| "its server".to_owned(), |s| s.name);
        Some((mode, server, servers.home()))
    }

    /// The subtitle menu's Search for subtitles… (PX-02), with a server
    /// and while a film or an episode plays: the panel over `anchor`, and
    /// a track it downloads plays from here.
    fn subtitle_search(&self) -> Option<crate::tracks::FindSubtitles> {
        let session = self.session.clone()?;
        if !self.is_video() {
            return None;
        }
        let me = self.me.clone();
        Some(Rc::new(move |anchor: &gtk::Widget| {
            let Some(this) = me.upgrade() else { return };
            let Some(item) = this.current() else { return };
            let picked: Rc<dyn Fn(u32)> = {
                let me = me.clone();
                Rc::new(move |track| {
                    if let Some(this) = me.upgrade() {
                        this.use_subtitle(track);
                    }
                })
            };
            crate::subtitle_search::present(
                anchor,
                crate::subtitle_search::Search {
                    session: session.clone(),
                    item: item.id().clone(),
                    title: crate::views::Facts::of(&item).title,
                    server: this.server_name.borrow().clone(),
                    picked,
                },
            );
        }))
    }

    /// The subtitle menu's Timing (PX-03), with a server: for the chosen
    /// track while it is an external text one.
    fn timing_menu(&self) -> Option<crate::subtitle_timing::TimingMenu> {
        self.session.as_ref()?;
        let me = self.me.clone();
        let current: Rc<dyn Fn() -> Option<(u32, SubtitleTiming)>> = {
            let me = me.clone();
            Rc::new(move || {
                let this = me.upgrade()?;
                let current = this.now()?;
                if !this.subtitles_on.get() {
                    return None;
                }
                let track = current.selected.get()?.subtitle?;
                let tracks = current.track_list.borrow();
                let timing = tracks
                    .as_ref()?
                    .subtitles
                    .get(track as usize)?
                    .timing
                    .clone()?;
                Some((track, timing))
            })
        };
        let call = |me: Weak<Self>, ask: TimingCall| -> Rc<dyn Fn(u32, i64)> {
            Rc::new(move |track, ms| {
                if let Some(this) = me.upgrade() {
                    this.retime(track, ask, ms);
                }
            })
        };
        let (sync, original, offset) = (
            call(me.clone(), TimingCall::Sync),
            call(me.clone(), TimingCall::Original),
            call(me, TimingCall::Offset),
        );
        Some(crate::subtitle_timing::TimingMenu {
            current,
            sync: Rc::new(move |track| sync(track, 0)),
            original: Rc::new(move |track| original(track, 0)),
            offset,
        })
    }

    /// Asks the server to sync, set back or offset the playing item's
    /// subtitle `track`; its answer is the track's timing now, as an
    /// event would tell it.
    fn retime(self: &Rc<Self>, track: u32, ask: TimingCall, offset_ms: i64) {
        let (Some(item), Some(session)) = (self.current(), self.session.clone()) else {
            return;
        };
        let id = item.id().clone();
        let answer = relm4::spawn(async move {
            match ask {
                TimingCall::Sync => session.sync_subtitle(&id, track).await,
                TimingCall::Original => session.use_original_subtitle_timing(&id, track).await,
                TimingCall::Offset => session.set_subtitle_offset(&id, track, offset_ms).await,
            }
        });
        let (weak, id) = (Rc::downgrade(self), item.id().clone());
        glib::spawn_future_local(async move {
            let answer = answer.await;
            let Some(playback) = weak.upgrade() else {
                return;
            };
            match answer {
                Ok(Ok(timing)) => playback.timing_changed(&id, track, timing),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "the subtitle's timing did not change");
                    playback.emit(&Change::TrackFailed(TIMING_FAILED));
                }
                Err(e) => tracing::warn!(error = %e, "the timing change stopped"),
            }
        });
    }

    /// Item `id`'s subtitle `track` is timed as `timing` now (PX-03): an
    /// event from the server, or the answer to a change made here. When
    /// it is the track showing and what the server serves changed, the
    /// subtitle is read again from its start and swapped in, as one
    /// arriving is (PR #6's path: a fresh file, held until shown).
    pub fn timing_changed(self: &Rc<Self>, id: &MediaId, track: u32, timing: SubtitleTiming) {
        let Some(current) = self.now().filter(|c| c.item.id() == id) else {
            return;
        };
        let reread = {
            let mut tracks = current.track_list.borrow_mut();
            let Some(kept) = tracks
                .as_mut()
                .and_then(|t| t.subtitles.get_mut(track as usize))
            else {
                return;
            };
            let reread = crate::subtitle_timing::rereads(kept.timing.as_ref(), &timing);
            kept.timing = Some(timing);
            reread
        };
        let showing = current.selected.get().and_then(|s| s.subtitle) == Some(track)
            && current.subtitle_file.borrow().is_some();
        if !(reread && showing) {
            return;
        }
        tracing::debug!(track, "the subtitle's timing changed; reading it again");
        self.load_external_subtitle(&current);
    }

    /// Plays the item's subtitle track `track` (a server track, by its
    /// place in the item's subtitles), keeping the audio, from where it
    /// is: the server serves a track only with a playback that asked for
    /// it, so the item starts again there, paused if it was, and the
    /// external subtitle comes as at any start (PR #6's path).
    pub fn use_subtitle(self: &Rc<Self>, track: u32) {
        let Some(current) = self.now() else { return };
        let selection = tracks::subtitle_change(current.selected.get(), track);
        let (at, playing) = (self.position.get(), self.playing.get());
        let start = Start {
            from: Some(at),
            selection: Some(selection),
            // Asked again: the new track is not in the list read before.
            tracks: None,
            quality: self.quality.get(),
            display: self.display.borrow().clone(),
        };
        // The queue stays where it is.
        self.step_starting(|_| true, Some(start));
        if !playing {
            self.autoplay.set(false);
        }
    }

    /// What the screen shows, for the items started from now on: the user
    /// changed Show HDR.
    pub fn set_display(&self, display: Option<spoolframe_types::DisplayCapabilities>) {
        self.display.replace(display);
    }

    /// Plans the playing item again for `quality` (P6-06), from where it
    /// is, and keeps the choice for the items after it. A new plan gets a
    /// new reader from the controller, as a resumed or re-planned playback
    /// does; the same plan plays on.
    pub fn set_quality(self: &Rc<Self>, quality: spoolframe_client_sdk::PlaybackQuality) {
        self.quality.set(quality);
        let Some(current) = self.now() else { return };
        let Some(managed) = current.managed.borrow().clone() else {
            return;
        };
        let before = managed.snapshot();
        let at = self.position.get();
        let changing = relm4::spawn(async move { managed.change_quality(quality, at).await });
        let (weak, current) = (Rc::downgrade(self), Rc::downgrade(&current));
        glib::spawn_future_local(async move {
            let answer = changing.await;
            let Some(playback) = weak.upgrade() else {
                return;
            };
            if !current.upgrade().is_some_and(|c| playback.is_current(&c)) {
                return;
            }
            match answer {
                Ok(Ok(next)) => {
                    if spoolframe_client_sdk::moves_player(&before, &next) {
                        tracing::info!(?quality, "the playback was planned again for its quality");
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, ?quality, "cannot change the quality");
                    playback.emit(&Change::TrackFailed(QUALITY_FAILED));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "the quality change did not finish");
                    playback.emit(&Change::TrackFailed(QUALITY_FAILED));
                }
            }
        });
    }

    pub fn is_playing(&self) -> bool {
        self.playing.get()
    }

    /// Where playback is in the current item, as last reported.
    pub fn position(&self) -> Duration {
        self.position.get()
    }

    pub fn toggle(&self) {
        self.autoplay.set(true);
        self.held.set(self.playing.get());
        if self.playing.get() {
            self.pause_player();
        } else {
            self.play_player();
        }
    }

    /// Plays: through the server playback's controller, which keeps a
    /// converted restart's new encode playing too.
    fn play_player(&self) {
        let current = self.now();
        let control = current.as_ref().map(|c| c.control.borrow());
        match control.as_deref() {
            Some(Some(Control::Server(c))) => c.play(),
            _ => self.pipeline.play_now(),
        }
    }

    /// Pauses, as [`Self::play_player`] plays.
    fn pause_player(&self) {
        let current = self.now();
        let control = current.as_ref().map(|c| c.control.borrow());
        match control.as_deref() {
            Some(Some(Control::Server(c))) => c.pause(),
            _ => self.pipeline.pause(),
        }
    }

    /// Seeks within the item: in the file itself for direct play; a
    /// converted output cannot seek, and the controller restarts the
    /// encode there instead, seeks while scrubbing gathered into one.
    /// Views show the target at once.
    pub fn seek(self: &Rc<Self>, to: Duration) {
        let Some(current) = self.now() else { return };
        if current.failure.borrow().is_some() {
            return;
        }
        if let Some(control) = current.control.borrow().as_ref() {
            control.seek(to);
        }
        // A direct seek shows itself (`seek_file`).
        if current.restarting() {
            self.show_at(to);
        }
    }

    /// The controller's seek in the file: views show the target, and the
    /// controller's position holds it until it lands.
    fn seek_file(&self, to: Duration) {
        self.pipeline.seek(to);
        self.show_at(to);
    }

    /// A download's seek, as its controller makes it.
    fn seek_download(&self, to: Duration) {
        // Back into a download played to its end: its place counts again.
        if let Some(current) = self.now() {
            current.ended.set(false);
        }
        self.pipeline.seek(to);
        self.show_at(to);
    }

    /// Whether a subtitle shows, and whether it came as a file from the
    /// server (an external one, or one read as text) rather than a stream.
    pub fn subtitle_shown(&self) -> (bool, bool) {
        let from_file = self
            .now()
            .is_some_and(|c| c.subtitle_file.borrow().is_some());
        (self.subtitles_on.get(), from_file)
    }

    /// How the server plays the current item, once it has said.
    pub fn plan(&self) -> Option<PlaybackPlan> {
        self.now()?.plan.borrow().clone()
    }

    /// The controller's load: GstPlay gets a new reader of `next`, from
    /// its start, with the external subtitle beside it. `resume`: where a
    /// direct play will seek, for the reader to read ahead there. A new
    /// reader after the first (a converted restart, a re-plan) has the
    /// same streams, picked again once listed.
    fn load(
        &self,
        current: &Current,
        next: spoolframe_client_sdk::Playback,
        resume: Option<Duration>,
    ) {
        current.plan.replace(Some(next.plan.clone()));
        let managed = current.managed.borrow().clone().expect("started playback");
        let reader = match playback_reader(managed, resume, current.catalog_length) {
            Ok(reader) => reader,
            Err(error) => return self.failed(&error),
        };
        let subtitle = current.subtitle_file.borrow().clone();
        self.pipeline.load(reader, subtitle.as_deref());
        let first = current.loads.replace(current.loads.get() + 1) == 0;
        if first {
            return;
        }
        // A restart was a seek: its positions are the viewer's place. A
        // file to resume prerolls at 0 first, which is not.
        current.told.set(resume.is_none());
        current.media_info.replace(None);
        let selected = match next.plan {
            PlaybackPlan::Direct { .. } => current
                .requested
                .or(next.chosen.map(|c| c.selection))
                .unwrap_or_default(),
            _ => Selection::default(),
        };
        current.pending.set(Some(selected));
    }

    /// GstPlay stopped on an error of its own (the external subtitle's,
    /// the visualizer's): a new reader of the same playback plays on from
    /// `at`, paused or playing as the user left it. A converted encode is
    /// read again from its start; a file seeks back to `at` once its
    /// length is known. The playback is unchanged, so its controller
    /// gives no new reader by itself.
    ///
    /// False when there is nothing to read anew: no item, or none the
    /// server started. A restart under way replaces the reader itself,
    /// so it counts as read anew.
    fn read_anew(self: &Rc<Self>, at: Duration) -> bool {
        let Some(current) = self.now() else {
            return false;
        };
        let Some(managed) = current.managed.borrow().clone() else {
            return false;
        };
        if current.restarting() {
            return true;
        }
        let resume = (!managed.is_converted()).then_some(at);
        self.load(&current, managed.snapshot(), resume);
        current.resume.set(resume);
        if resume.is_none() && self.autoplay.get() && !self.held.get() {
            self.play_player();
        } else {
            self.pipeline.pause();
        }
        self.show_at(at);
        true
    }

    /// Tells the server's playback what the player does and where, GstPlay
    /// at `at`: the controller reports it, at a seek's target while the
    /// seek is in flight. The SDK reports from it: every 10 s playing, at
    /// once on a pause or a move back; it keeps a paused one alive, and
    /// reads a converted one whose full buffer reads nothing. Nothing
    /// mid-restart: GstPlay's positions are the old encode's.
    ///
    /// The pause it prerolls in is not one: the item's place goes only
    /// once it has played or a seek has landed in it.
    fn tell_player(&self, current: &Current, at: Duration) {
        if self.stopped.get() {
            return;
        }
        let control = current.control.borrow();
        let Some(Control::Server(controller)) = control.as_ref() else {
            return;
        };
        if controller.restarting() {
            return;
        }
        let state = match self.playing.get() {
            true => PlayerState::Playing { pos: at },
            false if current.told.get() => PlayerState::Paused { pos: at },
            false => return,
        };
        current.told.set(true);
        if let Err(e) = controller.set_state(state) {
            tracing::debug!(error = %e, "the playback did not take the player's state");
        }
    }

    /// Ends the server playback, its last report sent first; a start
    /// still being asked is ended as its answer comes. The session numbers
    /// the end here, as it is called, so it keeps its place before any
    /// start asked after it.
    fn end(&self) -> Option<tokio::task::JoinHandle<()>> {
        let end = self.player.as_ref()?.end();
        Some(relm4::spawn(end))
    }

    /// Seeks `seconds` from where playback is, within the item. Before
    /// GStreamer knows the length, the catalog's stands in; with neither,
    /// only the start bounds it (clamping to a length of 0 sent every
    /// jump back to the start).
    pub fn jump(self: &Rc<Self>, seconds: f64) {
        let at = (self.position.get().as_secs_f64() + seconds).max(0.0);
        let length = Some(self.length.get())
            .filter(|l| !l.is_zero())
            .or(self.now().and_then(|c| c.catalog_length))
            .map(|l| l.as_secs_f64());
        let at = length.map_or(at, |length| at.min(length));
        self.seek(Duration::from_secs_f64(at));
    }

    pub fn volume(&self) -> f64 {
        self.pipeline.volume()
    }

    pub fn set_volume(&self, volume: f64) {
        self.pipeline.set_volume(volume.clamp(0.0, 1.0));
    }

    pub fn is_muted(&self) -> bool {
        self.pipeline.is_muted()
    }

    pub fn set_muted(&self, muted: bool) {
        self.pipeline.set_muted(muted);
    }

    /// Skips intros without asking from now on (P2-10).
    pub fn set_skip_intros(&self, on: bool) {
        self.skip_intros.set(on);
        let Some(current) = self.now() else { return };
        if let Some(control) = current.control.borrow().as_ref() {
            control.set_auto_skip(on);
        }
    }

    /// Does what the button over the video offers: past the intro or the
    /// credits, or on to what follows.
    pub fn take_offer(self: &Rc<Self>) {
        match self.now().and_then(|c| c.offered.get()) {
            Some(MarkerOffer::SkipIntro { to } | MarkerOffer::SkipCredits { to }) => self.seek(to),
            Some(MarkerOffer::Next) => self.next(),
            None => {}
        }
    }

    /// Back to where the intro was skipped from, within the undo window.
    /// It is not skipped again. The controller seeks there, and views show
    /// it at once: a converted restart's new encode is not yet there.
    pub fn undo_intro_skip(self: &Rc<Self>) {
        let Some(current) = self.now() else { return };
        let back = current
            .control
            .borrow()
            .as_ref()
            .and_then(Control::undo_skip);
        if let Some(at) = back {
            self.show_at(at);
        }
    }

    /// Whether Next plays something. Video goes on through its context
    /// (a show's episodes) only; the queue is music's.
    pub fn has_next(&self) -> bool {
        self.next_from() != NextFrom::Nothing
    }

    fn next_from(&self) -> NextFrom {
        let queue = self.queue.borrow();
        let playlist = self.in_playlist();
        let here = match (playlist, self.is_video()) {
            // The context is the entries played so far, which the server
            // moves on from (its repeat, not the queue's wrap): only
            // hand-queued music goes first.
            (true, true) => false,
            (true, false) => queue.queued().next().is_some(),
            (false, true) => queue.context_has_next(),
            (false, false) => queue.has_next(),
        };
        next_from(here, playlist)
    }

    /// The first queued item, else the context's next, if there is one.
    pub fn next(self: &Rc<Self>) {
        self.keep_place();
        self.advance();
    }

    /// Moves on from the current item; false at the end.
    fn advance(self: &Rc<Self>) -> bool {
        match self.next_from() {
            NextFrom::Here if self.is_video() => self.step(Queue::next_in_context),
            // Not as an item ending by itself: Repeat One is the app's, as a
            // playlist's settings may hold it (see `Playback::ended`).
            NextFrom::Here => self.step(|queue| queue.next(false)),
            NextFrom::Playlist => {
                self.ask_playlist();
                true
            }
            NextFrom::Nothing => false,
        }
    }

    /// Asks the server what follows the playlist's entry playing, once
    /// the calls before it (a save of the modes) are done.
    fn ask_playlist(&self) {
        if self.in_playlist() {
            self.playlist_call(Call::Next);
        }
    }

    /// The cursor's next entry after `was`: it plays, from where the user
    /// stopped in it (from its start when Repeat One plays it again); or
    /// the playlist ended.
    fn playlist_next(self: &Rc<Self>, was: EntryId, next: Option<Box<PlaylistEntry>>) {
        // Out of the borrow before anything is told: listeners read it.
        let advance = {
            let mut context = self.playlist.borrow_mut();
            let Some(context) = context.as_mut() else {
                return;
            };
            let next = next.map(|entry| *entry).map(|entry| PlaylistNext {
                entry: entry.entry,
                item: entry.item,
                markers: Default::default(),
                liked: entry.liked,
            });
            match context.playing.advance(next) {
                Advance::Play { item, .. } if context.playing.entry == was => {
                    Advance::Play { item, from: None }
                }
                advance => advance,
            }
        };
        match advance {
            Advance::Play { item, from } => {
                self.queue.borrow_mut().extend_context(vec![*item]);
                let start = Start {
                    from,
                    selection: None,
                    tracks: None,
                    quality: self.quality.get(),
                    display: self.display.borrow().clone(),
                };
                self.step_starting(Queue::next_in_context, Some(start));
            }
            Advance::Finished => {
                if let Some(playing) = self.leave_playlist() {
                    self.emit(&Change::PlaylistFinished(&playing));
                    self.emit(&Change::Queue);
                }
            }
        }
    }

    /// What played before, or the start of this one when it has played a
    /// few seconds, as music players do.
    pub fn previous(self: &Rc<Self>) {
        let position = self.position.get();
        let back = self.step(|queue| queue.previous(position) == Previous::Back);
        if !back {
            self.seek(Duration::ZERO);
        }
    }

    /// Reports where playback got to and stops for good.
    pub fn stop(&self) {
        let _ = self.stop_waitable();
    }

    /// [`Playback::stop`], giving back the last calls to the server, to
    /// wait for when the app quits.
    pub(crate) fn stop_waitable(&self) -> Vec<tokio::task::JoinHandle<()>> {
        if self.stopped.replace(true) {
            return Vec::new();
        }
        let report = self.keep_place();
        // The item goes: answers still on their way find nothing.
        self.current.take();
        // Down to NULL now, not whenever the last reference goes (see
        // `shutdown`), and off the main thread (see `crate::pipeline`).
        [report, self.end(), Some(self.pipeline.dismantle())]
            .into_iter()
            .flatten()
            .collect()
    }

    /// For the diagnostics panel.
    pub fn pipeline(&self) -> gst::Element {
        self.pipeline.element()
    }

    pub fn video_sink(&self) -> &VideoSink {
        self.pipeline.video()
    }

    pub fn reader_status(&self) -> Option<reader::Status> {
        self.pipeline.reader_status()
    }

    /// Moves the queue with `step` and starts what it then points at; false,
    /// with nothing changed, when `step` does not move.
    fn step(self: &Rc<Self>, step: impl FnOnce(&mut Queue) -> bool) -> bool {
        self.step_starting(step, None)
    }

    /// [`Playback::step`], starting the item as `start` says.
    fn step_starting(
        self: &Rc<Self>,
        step: impl FnOnce(&mut Queue) -> bool,
        start: Option<Start>,
    ) -> bool {
        if !step(&mut self.queue.borrow_mut()) {
            return false;
        }
        self.autoplay.set(true);
        self.held.set(false);
        // The next item's start ends the last one's playback first.
        self.current.take();
        // The next load takes the last item's subtitle away, while
        // stopped (see `Pipeline::load`).
        self.pipeline.stop();
        self.begin(start);
        self.emit(&Change::Queue);
        true
    }

    /// Starts the item the queue points at: the server plans it and opens
    /// a playback, then GstPlay reads it through the spoolframe:// source.
    /// `first` is the playlist's start, for the first item only.
    fn begin(self: &Rc<Self>, first: Option<Start>) {
        let Some(item) = self.current() else {
            return;
        };
        let (from, selection, tracks) = match first {
            Some(s) => (s.from, s.selection, s.tracks),
            None => (None, None, None),
        };
        tracing::debug!(item = %item.id(), resume = ?from, "starting");
        let current = Rc::new(Current::new(item.clone(), selection, tracks));
        self.current.replace(Some(current.clone()));
        self.subtitles_on.set(false);
        self.position.set(Duration::ZERO);
        self.length.set(Duration::ZERO);
        self.emit(&Change::Item(&item));
        // The new item starts at nothing: views showed the last one's
        // position, length and offer until told otherwise.
        self.emit(&Change::Position {
            at: Duration::ZERO,
            length: Duration::ZERO,
        });
        self.emit(&Change::Offer(None));
        self.emit(&Change::Starting);
        self.ask(&current, from);
    }

    /// Asks the server to start `current` from `from`, in place of what
    /// plays. A converted playback's encode begins there; direct play
    /// seeks itself.
    fn ask(self: &Rc<Self>, current: &Rc<Current>, from: Option<Duration>) {
        if self.is_local() {
            return self.open_local(current, from);
        }
        let (Some(session), Some(player)) = (self.session.clone(), self.player.clone()) else {
            return self.failed("there is no server to play it from");
        };
        let need_tracks = current.track_list.borrow().is_none();
        let (id, requested) = (current.item.id().clone(), current.requested);
        let quality = self.quality.get();
        let display = self.display.borrow().clone();
        // The session numbers the start here, as it is called: it keeps its
        // place among the starts and ends asked, however the tasks run.
        let opening = player.start(
            id.clone(),
            requested,
            PlaybackOptions {
                start: from,
                quality,
                display,
                ..Default::default()
            },
        );
        let started = relm4::spawn(async move {
            // The track list maps the chosen tracks onto the file's
            // streams; it does not wait on the start.
            let tracks = async {
                match need_tracks {
                    true => session.tracks(&id).await.ok(),
                    false => None,
                }
            };
            tokio::join!(opening, tracks)
        });
        let (weak, current) = (Rc::downgrade(self), Rc::downgrade(current));
        glib::spawn_future_local(async move {
            let answer = started.await;
            if let Some(playback) = weak.upgrade() {
                playback.on(Event::Started {
                    current,
                    from,
                    answer,
                });
            }
        });
    }

    /// GstPlay opens the download's file (PX-38), with nothing to ask
    /// first: its markers came with it, and its tracks are the file's.
    /// From `from` as a direct play resumes, once the length is known.
    fn open_local(self: &Rc<Self>, current: &Rc<Current>, from: Option<Duration>) {
        let Some(local) = &self.local else { return };
        tracing::debug!(download = %local.key, resume = ?from, "opening the download's file");
        let weak = Rc::downgrade(self);
        let controller = LocalController::new(move |to| {
            if let Some(playback) = weak.upgrade() {
                playback.seek_download(to);
            }
        })
        .with_markers(self.marker_tracker(local.markers.clone()));
        current
            .control
            .replace(Some(Control::Local(Box::new(controller))));
        let selection = current.requested.unwrap_or_default();
        current.pending.set(Some(selection));
        current.selected.set(Some(selection));
        self.pipeline.load_uri(&local.file_url);
        current.resume.set(from);
        if from.is_some() || !self.autoplay.get() {
            self.pipeline.pause();
        } else {
            self.pipeline.play_now();
        }
    }

    /// The server's answer to starting an item: its controller gives
    /// GstPlay the file. Resuming, a direct play prerolls paused, seeks
    /// there once the length is known, then plays: playing at once read
    /// and decoded the start of the file first, only to jump away from
    /// it. A restored queue prerolls and waits.
    fn started(self: &Rc<Self>, current: Weak<Current>, from: Option<Duration>, answer: Started) {
        let asking = |c: &Rc<Current>| c.control.borrow().is_none() && c.failure.borrow().is_none();
        let current = current
            .upgrade()
            .filter(|c| self.is_current(c) && asking(c));
        let Some(current) = current else {
            return;
        };
        tracing::debug!(ms = current.since_asked(), "the server answered");
        let item = &current.item;
        let (managed, tracks) = match answer {
            Ok((Ok(start), tracks)) => (start, tracks),
            // Overtaken by a later start or end: that one is what plays.
            Ok((Err(SdkError::PlaybackClosed), _)) => return,
            Ok((Err(e), _)) => return self.could_not_start(item, &backend_error(e).to_string()),
            Err(e) => {
                return self.could_not_start(item, &format!("starting playback failed: {e}"));
            }
        };
        let mut start = managed.snapshot();
        tracing::debug!(
            converted = matches!(start.plan, PlaybackPlan::Converted { .. }),
            source_codec = ?source_codec(&start.plan),
            "the plan"
        );
        if tracks.is_some() {
            current.track_list.replace(tracks);
        }
        // What plays: the tracks asked for, or the server's choice.
        let selection = current
            .requested
            .or(start.chosen.map(|c| c.selection))
            .unwrap_or_default();
        current.pending.set(Some(selection));
        current.selected.set(Some(selection));
        current.plan.replace(Some(start.plan.clone()));
        let markers = self.marker_tracker(std::mem::take(&mut start.markers));
        // After the answer, not alongside the start: it is needed only at
        // the credits.
        self.follow_episode(&current);
        let external = tracks::subtitle_comes_as_file(&start.plan);
        let converted = managed.is_converted();
        current.managed.replace(Some(managed.clone()));
        // A converted encode begins at `from`: that is where playback is
        // until its positions arrive (a restart for a late subtitle goes
        // from here, not from 0).
        if converted && let Some(at) = from {
            self.position.set(at);
        }
        let driver = Rc::new(Driver {
            playback: Rc::downgrade(self),
            current: Rc::downgrade(&current),
        });
        let controller =
            PlaybackController::new(managed, driver, local_spawn(), from, self.autoplay.get())
                .with_markers(markers);
        // A converted output being encoded has no length of its own yet,
        // and GstPlay may say none at all: the catalog's stands in for the
        // markers and for the views.
        if converted && let Some(length) = current.catalog_length {
            controller.length_known(length);
            self.length.set(length);
            self.emit(&Change::Position {
                at: self.position.get(),
                length,
            });
        }
        current.control.replace(Some(Control::Server(controller)));
        // A subtitle already here (the same item started again after its
        // handle idled out) went with the file.
        if external && current.subtitle_file.borrow().is_none() {
            self.load_external_subtitle(&current);
        }
    }

    /// Adds the show's next episode to the context when `current` is an
    /// episode with nothing after it there: Next, Next Episode, the end
    /// of the item and Up next then all go on into it.
    fn follow_episode(self: &Rc<Self>, current: &Rc<Current>) {
        let Item::Episode(episode) = &current.item else {
            return;
        };
        // The server's playlist says what follows, not the show.
        if self.in_playlist() {
            return;
        }
        // What is ahead in the context, not counting Repeat All's wrap:
        // the show's next episode still follows a one-item context.
        if !self.queue.borrow().context_ahead().is_empty() {
            return;
        }
        // A download plays alone: the show is the server's.
        let Some(session) = self.session.clone() else {
            return;
        };
        let episode = episode.clone();
        let found = relm4::spawn(async move { session.next_episode(&episode).await });
        let (weak, current) = (Rc::downgrade(self), Rc::downgrade(current));
        glib::spawn_future_local(async move {
            let Ok(answer) = found.await else { return };
            if let Some(playback) = weak.upgrade() {
                playback.on(Event::NextEpisode { current, answer });
            }
        });
    }

    fn next_episode_found(
        &self,
        current: Weak<Current>,
        answer: Result<Option<Item>, BackendError>,
    ) {
        if !current.upgrade().is_some_and(|c| self.is_current(&c)) {
            return;
        }
        match answer {
            Ok(Some(next)) => {
                self.queue.borrow_mut().extend_context(vec![next]);
                self.emit(&Change::Queue);
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "no next episode"),
        }
    }

    /// The player knows the item's length: the controller's resume seeks
    /// there and plays (a seek before that does not land), and so does
    /// the app's own ([`Current::resume`]). Told again on each preroll: a
    /// new reader of a file whose length GstPlay does not say again still
    /// resumes.
    fn length_known(self: &Rc<Self>, current: &Current) {
        let length = self.length.get();
        if length.is_zero() {
            return;
        }
        if let Some(control) = current.control.borrow().as_ref() {
            control.length_known(length);
        }
        let Some(at) = current.resume.take() else {
            return;
        };
        tracing::debug!(?at, ms = current.since_asked(), "resume seek");
        // The seek shows the position: paused, GstPlay reports none, and
        // views would show the start until Play.
        if let Some(control) = current.control.borrow().as_ref() {
            control.seek(at);
        }
        if self.autoplay.get() && !self.held.get() {
            self.play_player();
        }
    }

    /// Fetches the playback's external subtitle; GstPlay takes subtitles
    /// only by URI, so it is handed over as a file. The SDK asks again
    /// while the server is still reading it as text (P2-07); it plays on
    /// meanwhile.
    fn load_external_subtitle(self: &Rc<Self>, current: &Rc<Current>) {
        let Some(managed) = current.managed.borrow().clone() else {
            return;
        };
        let fetched = relm4::spawn(async move {
            managed
                .subtitle_file()
                .await
                .map(|file| (file.format, file.bytes))
                .map_err(spoolframe_linux_core::sdk::backend_error)
        });
        let (weak, current) = (Rc::downgrade(self), Rc::downgrade(current));
        glib::spawn_future_local(async move {
            let Ok(answer) = fetched.await else { return };
            if let Some(playback) = weak.upgrade() {
                playback.on(Event::Subtitle { current, answer });
            }
        });
    }

    /// The external subtitle arrived. Direct play takes it as it plays. A
    /// converted stream cannot: GstPlay restarts and seeks back on a
    /// subtitle change, and the stream cannot seek. So, as the server does
    /// for a converted subtitle change, the encode restarts where it is
    /// and the subtitle goes with the new one.
    fn subtitle_arrived(
        self: &Rc<Self>,
        current: Weak<Current>,
        answer: Result<(SubtitleFormat, Vec<u8>), BackendError>,
    ) {
        let Some(current) = current.upgrade().filter(|c| self.is_current(c)) else {
            return;
        };
        let (format, bytes) = match answer {
            Ok(subtitle) => subtitle,
            Err(e) => return tracing::warn!(error = %e, "no external subtitle"),
        };
        tracing::debug!(%format, bytes = bytes.len(), "the external subtitle arrived");
        if let Err(why) = external_subtitle::check(&bytes) {
            tracing::warn!(%format, bytes = bytes.len(), why, "not showing the external subtitle");
            return self.emit(&Change::TrackFailed(external_subtitle::NOT_SHOWN));
        }
        let Some(dir) = spoolframe_linux_core::paths::runtime_dir() else {
            return tracing::warn!("no private directory for the subtitle");
        };
        // The app's runtime directory exists only once something makes it.
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return tracing::warn!(error = %e, dir = %dir.display(), "cannot make the runtime directory");
        }
        let path = match external_subtitle::write(&dir, format, &bytes) {
            Ok(path) => path,
            Err(e) => {
                return tracing::warn!(error = %e, dir = %dir.display(), "cannot write the subtitle");
            }
        };
        tracing::debug!(%format, bytes = bytes.len(), path = %path.display(), "the external subtitle is written");
        // A file of its own each time: the one it replaces goes once
        // GstPlay has the new one.
        let replaced = current.subtitle_file.replace(Some(path.clone()));
        self.subtitles_on.set(true);
        if current.is_converted() {
            self.seek(self.position.get());
        } else {
            self.pipeline.set_subtitle(Some(&path));
        }
        if let Some(old) = replaced {
            let _ = std::fs::remove_file(old);
        }
    }

    /// The external subtitle failed in GstPlay, which stopped: the item
    /// plays on from where it was, without it.
    fn subtitle_failed(self: &Rc<Self>, error: &str) {
        tracing::warn!(error, "the external subtitle failed; playing on without it");
        let Some(current) = self.now() else { return };
        if let Some(path) = current.subtitle_file.take() {
            let _ = std::fs::remove_file(path);
        }
        self.subtitles_on.set(false);
        self.emit(&Change::TrackFailed(external_subtitle::NOT_SHOWN));
        self.read_anew(self.position.get());
    }

    /// The visualizer failed in GstPlay, which stopped: an otherwise
    /// playable song must not be stranded, so the same reader plays on
    /// from where it was, once, without it.
    fn visualizer_failed(self: &Rc<Self>, error: &str) {
        tracing::warn!(error, "visualizer failed; resuming audio without it");
        let _ = self.pipeline.set_visualization(None);
        self.emit(&Change::VisualizerStopped);
        if self.read_anew(self.position()) {
            self.emit(&Change::TrackFailed(
                "Visualizer stopped; continuing without it",
            ));
        } else {
            self.failed(error);
        }
    }

    /// The server's playback played to `length`: sent now, not with the
    /// SDK's next report, which Repeat One's seek back would replace. A
    /// track's counts toward its plays (PX-52), though it has no resume
    /// point.
    fn report_end(&self, length: Duration) {
        let Some(current) = self.now() else { return };
        let Some(managed) = current.managed.borrow().clone() else {
            return;
        };
        if let Err(e) = managed.report_position(length) {
            tracing::warn!(error = %e, "progress not queued");
            return;
        }
        relm4::spawn(async move {
            if let Err(e) = managed.flush().await {
                tracing::warn!(error = %e, "progress not saved");
            }
        });
    }

    /// Where a download is goes to the client, for the server later
    /// (PX-38). A server playback's goes with the player's state
    /// ([`Self::tell_player`]), and its end with it.
    fn keep_place(&self) -> Option<tokio::task::JoinHandle<()>> {
        let local = self.local.as_ref()?;
        let position = self.pipeline.position()?;
        let current = self.now()?;
        // Watched at its end: stopping there must not undo that.
        if current.ended.get() {
            return None;
        }
        let (client, key) = (local.client.clone(), local.key.clone());
        Some(self.progress_calls.send(async move {
            if let Err(e) = client.record_download_position(&key, position).await {
                tracing::warn!(error = %e, "the download's place was not kept");
            }
        }))
    }

    fn buffering(&self, percent: i32) {
        if !self.stopped.get() {
            self.emit(&Change::Buffering(percent));
        }
    }

    fn duration_changed(self: &Rc<Self>, duration: Option<gst::ClockTime>) {
        let Some(current) = self.now() else { return };
        tracing::debug!(?duration, ms = current.since_asked(), "length known");
        // A converted output being encoded has no duration of its own yet:
        // the catalog's length stands in.
        let length = duration
            .map(|d| Duration::from_nanos(d.nseconds()))
            .filter(|d| !d.is_zero())
            .or(current.catalog_length.filter(|_| current.is_converted()))
            .unwrap_or_default();
        self.length.set(length);
        self.length_known(&current);
        self.emit(&Change::Position {
            at: self.position.get(),
            length,
        });
    }

    /// A position from GstPlay. Views show where the controller says the
    /// player is (a seek's target until it lands), the server's playback
    /// hears it, and the markers there are followed. Nothing mid-restart:
    /// the positions are the old encode's.
    fn position_updated(self: &Rc<Self>, at: Duration) {
        let Some(current) = self.now() else { return };
        let actions = {
            let control = current.control.borrow();
            let shown = match control.as_ref() {
                None => return,
                Some(control) if control.restarting() => return,
                Some(control) => control.position(at),
            };
            self.show_at(shown);
            // A view that heard it may have moved on to the next item.
            if !self.is_current(&current) {
                return;
            }
            self.tell_player(&current, at);
            control.as_ref().map_or_else(Vec::new, |control| {
                control.on_position(at, self.has_next(), self.skip_intros.get())
            })
        };
        self.markers_said(&current, actions);
    }

    /// GstPlay's seek landed, at `at`: its positions count again, and
    /// once one has (each is the controller's), its pauses are the
    /// viewer's.
    fn seek_done(self: &Rc<Self>, at: Duration) {
        let Some(current) = self.now() else { return };
        if let Some(control) = current.control.borrow().as_ref() {
            current.told.set(true);
            control.seek_done();
        }
        self.position_updated(at);
    }

    /// Views hear that playback is at `at`.
    fn show_at(&self, at: Duration) {
        self.position.set(at);
        self.emit(&Change::Position {
            at,
            length: self.length.get(),
        });
    }

    /// The tracker for `markers`, for the item's controller to follow.
    fn marker_tracker(&self, markers: Vec<Marker>) -> MarkerTracker {
        let markers = Markers::new(markers, self.has_next());
        MarkerTracker::new(markers, self.skip_intros.get())
    }

    /// What the markers said: the button over the video, or the intro
    /// skipped by itself, which the controller sought past. Up next is a
    /// film's or an episode's; a song goes on to the next as it always
    /// has.
    fn markers_said(&self, current: &Current, actions: Vec<MarkerAction>) {
        let mut skip = None;
        for action in actions {
            match action {
                MarkerAction::Offer(offer) => current.marked.set(offer),
                MarkerAction::UpNext(due) => current.up_next.set(due),
                MarkerAction::AutoSkip { to } => skip = Some(to),
            }
        }
        if let Some(to) = skip {
            self.set_offer(current, None);
            // A converted restart shows nothing until it plays.
            self.show_at(to);
            self.emit(&Change::IntroSkipped);
            return;
        }
        let up_next = current.up_next.get() && self.is_video();
        self.set_offer(current, shown_offer(current.marked.get(), up_next));
    }

    /// Emits the button over the video when it changes.
    fn set_offer(&self, current: &Current, offer: Option<MarkerOffer>) {
        if current.offered.replace(offer) != offer {
            self.emit(&Change::Offer(offer));
        }
    }

    fn state_changed(self: &Rc<Self>, state: gst_play::PlayState) {
        let current = self.now();
        tracing::debug!(?state, ms = ?current.as_ref().map(|c| c.since_asked()), "player state");
        // Prerolled for a resume: seek and play, or, with no length to
        // seek in, play from the start rather than stay paused.
        if state == gst_play::PlayState::Paused
            && let Some(current) = &current
        {
            self.length_known(current);
            if current.resume.take().is_some() && self.autoplay.get() {
                self.play_player();
            }
        }
        let playing = state == gst_play::PlayState::Playing;
        if playing {
            self.pipeline.playing();
        }
        let paused = self.playing.replace(playing) && !playing;
        if paused && !self.stopped.get() {
            self.keep_place();
        }
        if let Some(current) = &current {
            let at = self.pipeline.position().unwrap_or(self.position.get());
            self.tell_player(current, at);
        }
        self.emit(&Change::Playing(playing));
    }

    fn media_info_updated(&self, info: &gst_play::PlayMediaInfo) {
        let Some(current) = self.now() else { return };
        current.media_info.replace(Some(info.clone()));
        let has_streams = !info.audio_streams().is_empty() || !info.subtitle_streams().is_empty();
        if has_streams && let Some(selection) = current.pending.take() {
            self.apply(&current, selection, info);
        }
        // GstPlay does not pick an external subtitle by itself, and lists
        // it only once its pipeline has it (on each restart for it).
        if current.subtitle_file.borrow().is_some() && self.pipeline.show_external_subtitle(info) {
            self.subtitles_on.set(true);
        }
        self.emit(&Change::MediaInfo(info));
    }

    /// Picks the streams that carry `selection`: the SDK's mapping for
    /// what the plan plays (`tracks::map_selection`). A converted output
    /// holds only what was picked; one burned in is in the picture.
    fn apply(&self, current: &Current, selection: Selection, info: &gst_play::PlayMediaInfo) {
        let picks = {
            let (plan, track_list) = (current.plan.borrow(), current.track_list.borrow());
            match (plan.as_ref(), track_list.as_ref()) {
                (Some(plan), Some(tracks)) => tracks::map_selection(tracks, plan, selection),
                (Some(plan), None) => {
                    tracks::map_selection(&MediaTracks::default(), plan, selection)
                }
                // A download has no plan: its file is played as it is.
                (None, tracks) => file_picks(tracks, selection),
            }
        };
        let stream = match picks.subtitle {
            SubtitleHandling::Embedded(n) => Some(n),
            SubtitleHandling::InOutput => Some(0),
            SubtitleHandling::Burned | SubtitleHandling::AsFile | SubtitleHandling::None => None,
        };
        // A converted output holds the one audio picked, which GstPlay
        // plays by itself. Picked by place, it could come after the first
        // media info (GstPlay lists streams as they turn up) and "fail":
        // "Couldn't switch the audio track" on every subtitle change.
        let audio = picks.audio_stream.filter(|_| !current.is_converted());
        // The subtitle waits until it is listed.
        if stream.is_some_and(|s| s >= info.subtitle_streams().len()) {
            current.pending.set(Some(selection));
            return;
        }
        if let Some(audio) = audio
            && let Err(e) = self.pipeline.select_audio(info, audio)
        {
            tracing::warn!(error = %e, audio, "cannot pick the audio track");
            self.emit(&Change::TrackFailed(AUDIO_FAILED));
        }
        // A subtitle file is turned on when it arrives.
        if stream.is_none() && current.subtitle_file.borrow().is_some() {
            return;
        }
        match self.pipeline.select_subtitle(info, stream) {
            Ok(()) => self.subtitles_on.set(stream.is_some()),
            Err(e) => {
                tracing::warn!(error = %e, ?stream, "cannot pick the subtitle");
                if stream.is_some() {
                    self.emit(&Change::TrackFailed(SUBTITLES_FAILED));
                }
            }
        }
    }

    /// The item played to its end: report it, then play the next one.
    fn ended(self: &Rc<Self>) {
        tracing::info!(item = ?self.current().map(|item| item.id().clone()), playlist = ?self.playing_from(), "playback reached end of stream");
        if let Some(local) = &self.local {
            // Watched, here: the server hears it at the next sync.
            if let Some(current) = self.now() {
                current.ended.set(true);
            }
            let (client, key) = (local.client.clone(), local.key.clone());
            self.progress_calls.send(async move {
                if let Err(e) = client.set_download_watched(&key, true).await {
                    tracing::warn!(error = %e, "the download was not marked watched");
                }
            });
        } else if let Some(length) = self.pipeline.duration() {
            self.report_end(length);
        }
        if self.repeat() == Repeat::One {
            self.seek(Duration::ZERO);
            self.play_player();
        } else {
            self.advance();
        }
    }

    /// The server would not start `item`. A queued item, or a track with
    /// something after it, is passed over with a word (P1-23); anything
    /// else fails the playback.
    fn could_not_start(self: &Rc<Self>, item: &Item, error: &str) {
        // A track of an album is passed over as a queued item is: one bad
        // file should not end the album. Video fails where it is.
        let skip = self.queue.borrow().current_is_queued()
            || (matches!(item, Item::Track(_)) && self.has_next());
        if !skip {
            return self.failed(error);
        }
        tracing::warn!(error, "skipping an item that cannot play");
        let title = crate::views::Facts::of(item).title;
        self.emit(&Change::Skipped(&format!("Skipped {title}: {error}")));
        if !self.advance() {
            self.failed(error);
        }
    }

    fn failed(&self, error: &str) {
        tracing::warn!(error, "playback failed");
        if let Some(current) = self.now() {
            current.failure.replace(Some(error.to_owned()));
        }
        self.emit(&Change::Failed(error));
    }

    /// New readers GstPlay was given after the item's first: a converted
    /// restart, a re-plan, or a reader read anew.
    #[cfg(test)]
    fn restarts(&self) -> u64 {
        self.now().map_or(0, |c| c.loads.get().saturating_sub(1))
    }

    /// Whether the item failed.
    #[cfg(test)]
    fn has_failed(&self) -> bool {
        self.now().is_some_and(|c| c.failure.borrow().is_some())
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Which timing change the subtitle menu asked for (PX-03).
#[derive(Debug, Clone, Copy)]
enum TimingCall {
    Sync,
    Original,
    Offset,
}

/// What the player says when a timing change is refused.
pub const TIMING_FAILED: &str = "Couldn't change the subtitle's timing";

/// The source's video as the server judged it, an RFC 6381 string
/// ("hvc1.2.4.L153.B0"), for the log: `None` with no video, or from a
/// server before 0.15.0, or for a plan that plays nothing.
fn source_codec(plan: &PlaybackPlan) -> Option<&str> {
    match plan {
        PlaybackPlan::Direct { source_codec, .. }
        | PlaybackPlan::Converted { source_codec, .. } => source_codec.as_deref(),
        _ => None,
    }
}

/// The streams of a download's file that carry `selection`, of the file's
/// tracks (`file_tracks`): the file has every subtitle it took as a
/// stream of its own, an external one too, in that order, so a track's
/// place is its stream. `map_selection` needs a plan, and a download has
/// none; the SDK's `LocalDownload::stream_picks` takes the item's places,
/// and the player holds the file's (`LocalDownload::file_selection`).
fn file_picks(tracks: Option<&MediaTracks>, selection: Selection) -> StreamPicks {
    let carried = tracks.map_or(0, |t| t.subtitles.len());
    let subtitle = selection
        .subtitle
        .map(|n| n as usize)
        .filter(|&n| n < carried);
    StreamPicks {
        audio_stream: selection.audio.map(|a| a as usize),
        subtitle: subtitle.map_or(SubtitleHandling::None, SubtitleHandling::Embedded),
    }
}

/// Whose a GstPlay error is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Blame {
    /// The external subtitle's own source: the item plays on without it.
    Subtitle,
    /// The visualizer's branch: the item plays on without it.
    Visualizer,
    /// The media's, or anything else: the item fails.
    Media,
}

/// Whose an error is, from whether it came from the external subtitle's
/// source and whether from the visualizer's branch. The subtitle's comes
/// first: its source is never in the visualizer's branch.
fn blame(subtitle: bool, visualizer: bool) -> Blame {
    match (subtitle, visualizer) {
        (true, _) => Blame::Subtitle,
        (false, true) => Blame::Visualizer,
        (false, false) => Blame::Media,
    }
}

/// The answer to starting an item, with its track list when asked for.
type Started =
    Result<(Result<ManagedPlayback, SdkError>, Option<MediaTracks>), tokio::task::JoinError>;

/// What happens to a playback from outside its own calls: GstPlay's
/// signals and the server's answers. One entry point, [`Playback::on`];
/// each handler checks the item is still the one playing before acting.
enum Event {
    Duration(Option<gst::ClockTime>),
    Position(gst::ClockTime),
    State(gst_play::PlayState),
    MediaInfo(gst_play::PlayMediaInfo),
    Buffering(i32),
    SeekDone(gst::ClockTime),
    Ended,
    Volume(f64),
    Muted(bool),
    Error(String, String),
    /// A read was refused because the admin stopped the playback.
    Stopped(String),
    /// A read was refused because the session ended.
    SignedOut(String),
    /// The SDK started the playback again after the server ended it; the
    /// new encode starts at the time given.
    Resumed(String, Duration),
    /// The playback was planned again under its reader (P6-06): a direct
    /// play the link was too slow for became a conversion.
    Replanned(String),
    Started {
        current: Weak<Current>,
        from: Option<Duration>,
        answer: Started,
    },
    Subtitle {
        current: Weak<Current>,
        answer: Result<(SubtitleFormat, Vec<u8>), BackendError>,
    },
    NextEpisode {
        current: Weak<Current>,
        answer: Result<Option<Item>, BackendError>,
    },
}

impl Playback {
    fn on(self: &Rc<Self>, event: Event) {
        match event {
            Event::Duration(d) => self.duration_changed(d),
            Event::Position(at) => {
                let at = self.pipeline.film_time(Duration::from_nanos(at.nseconds()));
                self.position_updated(at)
            }
            Event::State(state) => self.state_changed(state),
            Event::MediaInfo(info) => self.media_info_updated(&info),
            Event::Buffering(percent) => self.buffering(percent),
            Event::SeekDone(at) => {
                self.seek_done(self.pipeline.film_time(Duration::from_nanos(at.nseconds())))
            }
            Event::Ended => self.ended(),
            Event::Volume(volume) => self.emit(&Change::Volume(volume)),
            Event::Muted(muted) => self.emit(&Change::Muted(muted)),
            Event::Error(uri, e) if self.pipeline.accepts_error(&uri) => {
                let blame = blame(
                    self.pipeline.subtitle_caused(&e),
                    self.pipeline.visualization_caused(&e, !self.is_video()),
                );
                match blame {
                    // The subtitle's own source failing need not end the item.
                    Blame::Subtitle => self.subtitle_failed(&e),
                    Blame::Visualizer => self.visualizer_failed(&e),
                    Blame::Media => self.failed(&e),
                }
            }
            Event::Error(..) => {}
            Event::Stopped(uri) => {
                if self.pipeline.accepts_error(&uri) {
                    tracing::info!("the server's admin stopped the playback");
                    self.emit(&Change::Stopped);
                }
            }
            Event::SignedOut(uri) => {
                if self.pipeline.accepts_error(&uri) {
                    tracing::info!("the session ended under the playback");
                    self.emit(&Change::SignedOut);
                }
            }
            // The controller follows the playback's change with a new
            // reader by itself; the old reader's error only says so.
            Event::Resumed(uri, at) => {
                if self.pipeline.accepts_error(&uri) {
                    tracing::info!(
                        at = at.as_secs(),
                        "the server ended the playback; it was started again"
                    );
                }
            }
            Event::Replanned(uri) => {
                if self.pipeline.accepts_error(&uri) {
                    tracing::info!(
                        at = self.position.get().as_secs(),
                        "the playback was planned again for the link"
                    );
                }
            }
            Event::Started {
                current,
                from,
                answer,
            } => self.started(current, from, answer),
            Event::Subtitle { current, answer } => self.subtitle_arrived(current, answer),
            Event::NextEpisode { current, answer } => self.next_episode_found(current, answer),
        }
    }
}

/// GstPlay's signals, as [`Event`]s.
fn connect(adapter: &gst_play::PlaySignalAdapter, playback: &Rc<Playback>) {
    // GstPlay's adapter wants thread-safe callbacks, but it delivers them
    // on the main loop it was made on. The guard satisfies the bound and
    // panics if that ever changes. It holds the playback weakly: the
    // playback owns the adapter.
    let guard = || ThreadGuard::new(Rc::downgrade(playback));
    let on = |g: &ThreadGuard<Weak<Playback>>, event: Event| {
        if let Some(p) = g.get_ref().upgrade() {
            p.on(event)
        }
    };
    let g = guard();
    adapter.connect_duration_changed(move |_, d| on(&g, Event::Duration(d)));
    let g = guard();
    adapter.connect_position_updated(move |_, at| {
        if let Some(at) = at {
            on(&g, Event::Position(at))
        }
    });
    let g = guard();
    adapter.connect_state_changed(move |_, state| on(&g, Event::State(state)));
    let g = guard();
    adapter.connect_media_info_updated(move |_, info| on(&g, Event::MediaInfo(info.clone())));
    let g = guard();
    adapter.connect_buffering(move |_, percent| on(&g, Event::Buffering(percent)));
    let g = guard();
    adapter.connect_seek_done(move |_, at| on(&g, Event::SeekDone(at)));
    let g = guard();
    adapter.connect_end_of_stream(move |_| on(&g, Event::Ended));
    let g = guard();
    adapter.connect_volume_changed(move |_, volume| on(&g, Event::Volume(volume)));
    let g = guard();
    adapter.connect_mute_changed(move |_, muted| on(&g, Event::Muted(muted)));
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    playback
        .pipeline
        .message_bus()
        .set_sync_handler(move |_, message| {
            if let Ok(gst_play::PlayMessage::Error(error)) = gst_play::PlayMessage::parse(message) {
                let marked = error.details().map(|d| {
                    let at = d.get::<u64>(crate::source::RESUMED_AT).ok();
                    (d.name().to_string(), at.map(Duration::from_millis))
                });
                let _ = send.send((error.uri().to_string(), error.error().to_string(), marked));
            }
            gst::BusSyncReply::Pass
        });
    let weak = Rc::downgrade(playback);
    glib::spawn_future_local(async move {
        while let Some((uri, error, marked)) = receive.recv().await {
            let Some(playback) = weak.upgrade() else {
                break;
            };
            playback.on(match marked {
                Some((name, _)) if name == crate::source::STOPPED => Event::Stopped(uri),
                Some((name, _)) if name == crate::source::SIGNED_OUT => Event::SignedOut(uri),
                Some((name, Some(at))) if name == crate::source::RESUMED => Event::Resumed(uri, at),
                Some((name, _)) if name == crate::source::REPLANNED => Event::Replanned(uri),
                _ => Event::Error(uri, error),
            });
        }
    });
}

/// A reader of `playback` as it is planned now. `from`: where a direct
/// play will seek, read ahead first.
fn playback_reader(
    playback: ManagedPlayback,
    from: Option<Duration>,
    length: Option<Duration>,
) -> Result<Box<dyn ByteSource>, String> {
    let start = playback.snapshot();
    match &start.plan {
        PlaybackPlan::Converted {
            delivery: Delivery::Progressive,
            video,
            ..
        } => {
            let bitrate = match video {
                Some(ConvertedVideo::Encode { max_bitrate, .. }) => *max_bitrate,
                _ => None,
            };
            Ok(Box::new(ProgressiveReader::new(playback, spawn(), bitrate)))
        }
        PlaybackPlan::Direct { .. } => {
            let reader = MediaReader::new(playback, spawn(), length);
            if let (Some(at), Some(length)) = (from, length) {
                reader.expect(reader::guess_offset(start.size, length, at));
            }
            Ok(Box::new(reader))
        }
        PlaybackPlan::Converted { delivery, .. } => Err(format!(
            "the server planned {delivery:?} delivery, which this player does not take"
        )),
        PlaybackPlan::Unsupported { reasons } => {
            Err(format!("cannot play: {}", reasons.join("; ")))
        }
        PlaybackPlan::Unknown => {
            Err("the server planned playback this version cannot take; update it".into())
        }
    }
}

/// Fetches run on Relm4's tokio runtime, where the SDK's networking lives.
fn spawn() -> Spawn {
    Arc::new(|future| {
        relm4::spawn(future);
    })
}

#[cfg(test)]
mod tests {
    use gst::prelude::*;
    use spoolframe_linux_core::testing::TestDir;
    use std::path::Path;

    use spoolframe_linux_core::backend::{Kinds, PlaylistPage};
    use spoolframe_linux_core::mock::playback::{DownloadCall, MockDownloads, MockPlayback};
    use spoolframe_linux_core::mock::{self, playback::PlaylistCall};

    use super::*;

    /// Waits for `future` on a main loop of the test's own: tests run on
    /// threads of their own, and the default one is the first's.
    fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
        glib::MainContext::new().block_on(future)
    }

    /// All, `songs` of them in order, the server's place on the first.
    fn all(songs: u32) -> PlaylistPage {
        let kinds = Kinds {
            tracks: true,
            ..Kinds::default()
        };
        let tracks = (1..=songs).map(|n| mock::track(&format!("song {n}"), &format!("Song {n}")));
        let mut page = mock::page(mock::playlist("all", "All", kinds), tracks.collect());
        page.place = Some(page.entries[0].entry);
        page
    }

    /// `entry`, as the server answers it to play.
    fn playing(entry: &PlaylistEntry) -> PlaylistNext {
        PlaylistNext {
            entry: entry.entry,
            item: entry.item.clone(),
            markers: Default::default(),
            liked: false,
        }
    }

    /// A cursor on `server`'s playlist `page`, from its first entry, with
    /// `settings` or, `None`, the server's.
    fn following(
        server: &Arc<MockPlayback>,
        page: &PlaylistPage,
        settings: Option<PlaySettings>,
    ) -> PlaylistCursor {
        let first = playing(&page.entries[0]);
        let id = &page.playlist.id;
        ready(managed::follow_playlist(
            server.clone(),
            id,
            first,
            settings,
        ))
        .unwrap()
    }

    /// Up next while shuffling names the songs the server says play next,
    /// in its order, under a heading that says so.
    #[test]
    fn up_next_while_shuffling_is_what_next_plays() {
        let mut page = all(5);
        page.settings.shuffle = true;
        let dealt: Vec<_> = [3, 1, 4, 2].map(|n| page.entries[n].clone()).into();
        let server = MockPlayback::new(None, [])
            .with_playlist(page.clone())
            .with_upcoming(dealt.clone());
        let cursor = following(&Arc::new(server), &page, None);
        let settings = cursor.settings();
        let ahead = playlist_ahead("All", settings, ready(read_up_next(&cursor)).unwrap());
        let dealt: Vec<_> = dealt.iter().map(|e| e.entry).collect();
        assert_eq!(
            (ahead.heading.as_str(), entries_of(&ahead), settings.shuffle),
            ("In All · Shuffle on", dealt, true)
        );
    }

    /// Repeat All goes round, the server's list again and again: Up next
    /// lists each song once, up to the one playing.
    #[test]
    fn up_next_from_the_server_stops_where_the_song_playing_comes_again() {
        let round = PlaySettings {
            repeat: spoolframe_linux_core::backend::Repeat::All,
            ..PlaySettings::default()
        };
        let page = all(4);
        let rest = &page.entries[1..];
        let again = [rest, &page.entries[..], rest].concat();
        let server = MockPlayback::new(None, [])
            .with_playlist(page.clone())
            .with_upcoming(again);
        let cursor = following(&Arc::new(server), &page, Some(round));
        let ahead = playlist_ahead("All", round, ready(read_up_next(&cursor)).unwrap());
        let rest: Vec<_> = rest.iter().map(|e| e.entry).collect();
        assert_eq!((entries_of(&ahead), ahead.note), (rest, None));
    }

    /// Reads of what comes up asked for while a save waits are sent once,
    /// after it: the last one asked for runs, and the others not at all.
    #[test]
    fn only_the_last_read_asked_for_is_sent_after_the_save_before_it() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let calls = Serial::default();
                let reads = Rc::new(Latest::default());
                let sent = Rc::new(RefCell::new(Vec::new()));
                let (go, held) = tokio::sync::oneshot::channel::<()>();
                let read = |n: u64| {
                    let (reads, sent) = (reads.clone(), sent.clone());
                    move || async move {
                        if reads.is(n) {
                            sent.borrow_mut().push("read");
                        }
                    }
                };
                {
                    let sent = sent.clone();
                    calls.queue(move || async move {
                        let _ = held.await;
                        sent.borrow_mut().push("next");
                    });
                }
                calls.queue(read(reads.ask()));
                {
                    let sent = sent.clone();
                    calls.queue(move || async move { sent.borrow_mut().push("save") });
                }
                calls.queue(read(reads.ask()));
                let idle = calls.idle();
                go.send(()).unwrap();
                context.block_on(idle).unwrap();
                assert_eq!(*sent.borrow(), ["next", "save", "read"]);
            })
            .unwrap();
    }

    /// A download's file has every subtitle it took as a stream of its
    /// own, an external one too, in the file's order: of an item's [en in
    /// the file, fr external, de in the file], a file with fr and de has
    /// them as its streams 0 and 1.
    #[test]
    fn a_downloads_subtitle_is_the_files_stream_at_its_place() {
        let track = |language: &str, external| spoolframe_types::SubtitleTrack {
            language: Some(language.into()),
            title: None,
            forced: false,
            default: false,
            external,
            role: Default::default(),
            timing: None,
            download: None,
        };
        let file = MediaTracks {
            subtitles: vec![track("fra", true), track("deu", false)],
            ..Default::default()
        };
        let pick = |subtitle| {
            file_picks(
                Some(&file),
                Selection {
                    audio: None,
                    subtitle: Some(subtitle),
                },
            )
            .subtitle
        };
        assert_eq!(
            [pick(1), pick(0), pick(2)],
            [
                SubtitleHandling::Embedded(1),
                SubtitleHandling::Embedded(0),
                SubtitleHandling::None
            ]
        );
    }

    /// Four seconds of H.264 in Matroska.
    fn clip(path: &Path) {
        clip_lasting(path, 4);
    }

    /// `seconds` of H.264 in Matroska.
    fn clip_lasting(path: &Path, seconds: u32) {
        let pipeline = gst::parse::launch(&format!(
            "videotestsrc num-buffers={} ! video/x-raw,width=320,height=240,framerate=30/1 \
             ! videoconvert ! x264enc ! h264parse ! matroskamux ! filesink location={}",
            seconds * 30,
            path.display()
        ))
        .unwrap();
        let bus = pipeline.bus().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("the clip is written");
        pipeline.set_state(gst::State::Null).unwrap();
    }

    /// Eight seconds of H.264 in Matroska with a text subtitle stream, one
    /// cue throughout: what a conversion with `copy:srt` sends.
    fn clip_with_subtitle(path: &Path) {
        let pipeline = gst::parse::launch(&format!(
            "videotestsrc num-buffers=240 ! video/x-raw,width=320,height=240,framerate=30/1 \
             ! videoconvert ! x264enc ! h264parse ! matroskamux name=mux ! filesink location={} \
             appsrc name=text format=time caps=text/x-raw,format=utf8 ! mux.subtitle_%u",
            path.display()
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        // appsrc by its action signals: no gstreamer-app crate for a test.
        let text = pipeline.by_name("text").unwrap();
        let bus = pipeline.bus().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let mut cue = gst::Buffer::from_slice(b"the copied cue".to_vec());
        {
            let cue = cue.get_mut().unwrap();
            cue.set_pts(gst::ClockTime::ZERO);
            cue.set_duration(gst::ClockTime::from_seconds(8));
        }
        let _ = text.emit_by_name::<gst::FlowReturn>("push-buffer", &[&cue]);
        let _ = text.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
        bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("the clip is written");
        pipeline.set_state(gst::State::Null).unwrap();
    }

    /// Runs the main loop until `done` or `limit`.
    fn run_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + limit;
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_visualizer_error_resumes_the_same_song_without_the_effect() {
        gtk::init().unwrap();
        init().unwrap();
        let dir = TestDir::new_blocking("spoolframe-visualizer-recovery");
        let path = dir.join("tone.mkv");
        tone(&path);
        let item = song();
        let id = item.id().clone();
        let server = Arc::new(MockPlayback::new(Some(path), [item.clone()]));
        let playback = Playback::start(Playlist {
            session: server,
            items: vec![item],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: None,
        })
        .unwrap();
        assert!(run_until(Duration::from_secs(8), || playback.position()
            > Duration::from_millis(500)));
        assert!(playback.set_visualization(Some("wavescope")));
        let at = playback.position();
        let uri = playback.pipeline.playback_uri().unwrap();
        playback.on(Event::Error(
            uri,
            "Error from element /GstPlayBin3:playbin3/GstPlaySink:playsink/GstBin:visbin/\
             GstQueue:visqueue: simulated visualizer negotiation error"
                .into(),
        ));
        assert!(!playback.pipeline.visualization_active());
        assert!(run_until(Duration::from_secs(8), || playback.is_playing()
            && playback.position() > at + Duration::from_millis(250)));
        assert_eq!(playback.current().unwrap().id(), &id);
        assert!(playback.now().unwrap().failure.borrow().is_none());
        playback.stop();
    }

    fn entries_of(ahead: &PlaylistAhead) -> Vec<EntryId> {
        ahead.entries.iter().map(|e| e.entry).collect()
    }

    /// Of a playlist of sixty and one, Up next shows fifty: each is a row
    /// built anew on every change.
    #[test]
    fn up_next_from_a_huge_playlist_shows_the_next_fifty() {
        let page = all(61);
        let server = Arc::new(MockPlayback::new(None, []).with_playlist(page.clone()));
        let cursor = following(&server, &page, None);
        let up = ready(read_up_next(&cursor)).unwrap();
        assert_eq!(up.entries.len(), PREVIEW_AHEAD);
    }

    /// Up next's playlist part in this app's words, by what the cursor
    /// says: the heading by shuffle, a note when no songs or the one
    /// playing are named.
    #[test]
    fn up_next_from_the_playlist_follows_its_repeat_and_shuffle_modes() {
        use spoolframe_linux_core::backend::Repeat;
        let entries = all(3).entries;
        let shuffled = PlaySettings {
            shuffle: true,
            ..PlaySettings::default()
        };
        let one = PlaySettings {
            repeat: Repeat::One,
            ..PlaySettings::default()
        };
        let up = |entries: &[PlaylistEntry], note| PlaylistUpNext {
            entries: entries.to_vec(),
            note,
        };
        // (label, settings, what the cursor says, want heading, entries, note)
        let cases = [
            (
                "shuffled on a server that cannot say names no songs and says why",
                shuffled,
                up(&[], UpNextNote::Shuffled),
                "In On Repeat · Shuffle on",
                Vec::new(),
                Some("Songs play in a shuffled order"),
            ),
            (
                "shuffled names what the server plays next",
                shuffled,
                up(&entries[..2], UpNextNote::None),
                "In On Repeat · Shuffle on",
                vec![entries[0].entry, entries[1].entry],
                None,
            ),
            (
                "repeat one is the song playing again",
                one,
                up(&entries[..1], UpNextNote::PlaysAgain),
                "Next from On Repeat",
                vec![entries[0].entry],
                Some("This song plays again"),
            ),
            (
                "the end says nothing",
                PlaySettings::default(),
                up(&[], UpNextNote::Ends),
                "Next from On Repeat",
                Vec::new(),
                None,
            ),
        ];
        for (label, settings, said, heading, listed, note) in cases {
            let ahead = playlist_ahead("On Repeat", settings, said);
            assert_eq!(
                (ahead.heading.as_str(), entries_of(&ahead), ahead.note),
                (heading, listed, note),
                "{label}"
            );
        }
    }

    /// A job queued behind a slow one waits for it, and reads what it
    /// needs as it runs: a save of the modes behind a Next sends the
    /// entry Next moved to, not the one playing when it was asked for.
    #[test]
    fn a_queued_call_waits_for_the_one_before_and_reads_the_state_it_finds() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let calls = Serial::default();
                let entry = Rc::new(Cell::new(1u64));
                let sent = Rc::new(RefCell::new(Vec::new()));
                let (answer, answered) = tokio::sync::oneshot::channel::<u64>();
                {
                    // Next: its answer, when it comes, moves the entry on.
                    let (entry, sent) = (entry.clone(), sent.clone());
                    calls.queue(move || async move {
                        sent.borrow_mut().push(("next", entry.get()));
                        entry.set(answered.await.unwrap());
                    });
                }
                {
                    // A save of the modes, asked for while Next is out.
                    let (entry, sent) = (entry.clone(), sent.clone());
                    calls.queue(move || async move {
                        sent.borrow_mut().push(("save", entry.get()));
                    });
                }
                let idle = calls.idle();
                while context.iteration(false) {}
                assert_eq!(
                    *sent.borrow(),
                    [("next", 1)],
                    "the save went out beside Next"
                );
                answer.send(2).unwrap();
                context.block_on(idle).unwrap();
                assert_eq!(*sent.borrow(), [("next", 1), ("save", 2)]);
            })
            .unwrap();
    }

    /// A download's places go out in order on the runtime.
    #[test]
    fn calls_on_the_runtime_run_in_order() {
        let calls = Serial::default();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (go, held) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let order = order.clone();
            calls.send(async move {
                let _ = held.await;
                order.lock().unwrap().push("first");
            })
        };
        let second = {
            let order = order.clone();
            calls.send(async move { order.lock().unwrap().push("second") })
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(order.lock().unwrap().is_empty(), "went past the first");
        go.send(()).unwrap();
        glib::MainContext::new().block_on(async {
            for call in [first, second] {
                call.await.unwrap();
            }
        });
        assert_eq!(*order.lock().unwrap(), ["first", "second"]);
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_media_error_under_a_visualizer_fails_the_song() {
        gtk::init().unwrap();
        init().unwrap();
        let dir = TestDir::new_blocking("spoolframe-visualizer-media-error");
        let path = dir.join("tone.mkv");
        tone(&path);
        let item = song();
        let server = Arc::new(MockPlayback::new(Some(path), [item.clone()]));
        let playback = Playback::start(Playlist {
            session: server,
            items: vec![item],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: None,
        })
        .unwrap();
        assert!(run_until(Duration::from_secs(8), || playback.position()
            > Duration::from_millis(500)));
        assert!(playback.set_visualization(Some("wavescope")));
        let uri = playback.pipeline.playback_uri().unwrap();
        playback.on(Event::Error(
            uri,
            "Error from element /GstPlayBin3:playbin3/GstURIDecodeBin3:uridecodebin3/\
             GstDecodebin3:decodebin3-0: the file is damaged"
                .into(),
        ));
        assert!(playback.now().unwrap().failure.borrow().is_some());
        assert!(
            playback.pipeline.visualization_active(),
            "the visualizer was blamed"
        );
        playback.stop();
    }

    #[test]
    fn an_error_is_the_subtitles_then_the_visualizers_then_the_medias() {
        assert_eq!(
            [
                blame(true, true),
                blame(true, false),
                blame(false, true),
                blame(false, false),
            ],
            [
                Blame::Subtitle,
                Blame::Subtitle,
                Blame::Visualizer,
                Blame::Media
            ]
        );
    }

    #[test]
    fn queued_music_plays_before_the_playlists_next_entry() {
        assert_eq!(
            [
                next_from(true, true),
                next_from(false, true),
                next_from(false, false),
                next_from(true, false),
            ],
            [
                NextFrom::Here,
                NextFrom::Playlist,
                NextFrom::Nothing,
                NextFrom::Here
            ]
        );
    }

    /// Playback on a mock server, end to end. One test: GTK binds itself
    /// to the first thread that starts it, and each test has its own.
    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn playback_on_a_mock_server() {
        gtk::init().unwrap();
        init().unwrap();
        dropping_a_start_cleans_up_its_server_handle();
        a_start_the_next_overtook_never_plays();
        a_playback_plays_and_tells_a_late_view_where_it_is();
        a_server_that_goes_away_fails_the_playback();
        a_playback_the_admin_stopped_says_so();
        a_session_ended_under_a_playback_says_so();
        pausing_saves_where_playback_got_to();
        resuming_plays_from_where_it_stopped();
        a_restored_queue_waits_paused_where_it_was();
        a_converted_playback_restarts_its_encode_on_a_seek();
        a_converted_playback_the_server_ended_plays_on();
        a_direct_play_planned_again_plays_on_converted();
        a_quality_chosen_while_playing_plans_it_again();
        a_quality_chosen_before_playing_starts_with_it();
        a_late_subtitle_restarts_a_converted_playback_with_it();
        a_converted_playback_shows_the_catalogs_length();
        the_intro_is_offered_and_skipped_on_asking();
        an_automatic_intro_skip_happens_once_and_undoes();
        undoing_a_converted_intro_skip_shows_where_it_goes_back_to();
        quitting_waits_for_the_last_progress();
        a_playlist_goes_on_to_the_servers_next_entry_then_ends();
        a_finished_download_plays_offline_to_its_end();
    }

    /// Movie night (two films, then an episode) from the top: each entry
    /// that ends plays the server's next, and the last one ends the
    /// playlist, once.
    fn a_playlist_goes_on_to_the_servers_next_entry_then_ends() {
        playlist_plays_to_the_end("Movie night", false, false, false);
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_music_playlist_goes_on_to_the_servers_next_entry_then_ends() {
        gtk::init().unwrap();
        init().unwrap();
        playlist_plays_to_the_end("All", false, false, false);
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn selecting_a_playlist_preview_entry_keeps_playing_the_playlist() {
        gtk::init().unwrap();
        init().unwrap();
        playlist_plays_to_the_end("All", true, false, false);
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_playlist_plays_the_manual_queue_then_resumes_its_entries() {
        gtk::init().unwrap();
        init().unwrap();
        playlist_plays_to_the_end("All", false, true, false);
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_restored_playlist_plays_its_queue_then_the_remaining_songs() {
        gtk::init().unwrap();
        init().unwrap();
        playlist_plays_to_the_end("All", false, true, true);
    }

    /// Twelve seconds of a tone, in Opus in Matroska.
    fn tone(path: &Path) {
        let generator = gst::parse::launch(&format!(
            "audiotestsrc num-buffers=600 ! audioconvert ! opusenc ! matroskamux ! filesink location={}",
            path.display()
        ))
        .unwrap();
        generator.set_state(gst::State::Playing).unwrap();
        generator
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Eos])
            .unwrap();
        generator.set_state(gst::State::Null).unwrap();
    }

    /// The settings shuffled, repeating all: not the defaults.
    fn shuffled_repeating() -> spoolframe_linux_core::backend::PlaySettings {
        spoolframe_linux_core::backend::PlaySettings {
            shuffle: true,
            repeat: spoolframe_linux_core::backend::Repeat::All,
            skip_watched: false,
        }
    }

    /// Playing `page`'s `entry` with `settings`, as started or saved.
    fn resume(
        page: &PlaylistPage,
        entry: EntryId,
        settings: PlaySettings,
    ) -> crate::saved_queue::PlaylistResume {
        crate::saved_queue::PlaylistResume {
            playing: Playing {
                id: page.playlist.id.clone(),
                name: page.playlist.name.clone(),
                entry,
            },
            settings: Some(settings),
        }
    }

    /// A server following `page`, its items played from `media`.
    fn serving(media: Option<PathBuf>, page: &PlaylistPage) -> Arc<MockPlayback> {
        let items = page.entries.iter().map(|e| e.item.clone());
        Arc::new(MockPlayback::new(media, items).with_playlist(page.clone()))
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_started_playlist_plays_with_its_settings_at_once() {
        gtk::init().unwrap();
        init().unwrap();
        let page = all(4);
        let first = page.entries[0].clone();
        let modes = shuffled_repeating();
        let playback = Playback::start(Playlist {
            session: serving(None, &page),
            items: vec![first.item],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: Some(resume(&page, first.entry, modes)),
        })
        .unwrap();
        // Before the cursor first reads: Shuffle and Repeat are the
        // playlist's, not the local queue's.
        assert_eq!(
            (playback.is_shuffled(), playback.repeat()),
            (true, Repeat::All)
        );
        playback.stop();
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn modes_changed_while_next_is_out_keep_the_servers_place_on_the_new_entry() {
        gtk::init().unwrap();
        init().unwrap();
        let dir = TestDir::new_blocking("spoolframe-playlist-modes");
        let path = dir.join("tone.mkv");
        tone(&path);
        let page = all(4);
        let first = page.entries[0].clone();
        let server = serving(Some(path), &page);
        let context = glib::MainContext::default();
        let playback = Playback::start(Playlist {
            session: server.clone(),
            items: vec![first.item],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: Some(resume(&page, first.entry, Default::default())),
        })
        .unwrap();
        playback.next();
        playback.toggle_shuffle();
        playback.cycle_repeat();
        context.block_on(playback.playlist_calls.idle()).unwrap();
        let playing = playback.playing_from().unwrap().entry;
        assert_ne!(playing, first.entry);
        assert_eq!(
            (server.place(), server.settings()),
            (Some(playing), Some(shuffled_repeating()))
        );
        // The save went out once Next was answered, not beside it.
        let calls = server.playlist_calls();
        let next = PlaylistCall::Next { after: first.entry };
        let told = calls.iter().position(|c| *c == next);
        let saved = PlaylistCall::Save(shuffled_repeating());
        let saved = calls.iter().position(|c| *c == saved);
        assert!(told.is_some() && told < saved, "{calls:?}");
        playback.stop();
    }

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_restored_playlist_leaves_the_servers_place_where_it_is() {
        gtk::init().unwrap();
        init().unwrap();
        // A song that plays, so nothing moves on by itself.
        let dir = TestDir::new_blocking("spoolframe-restored-playlist");
        let path = dir.join("tone.mkv");
        tone(&path);
        let page = all(4);
        let (saved, later) = (page.entries[0].clone(), page.entries[2].entry);
        // Played on, on another device, since the queue was saved.
        let moved_on = PlaylistPage {
            place: Some(later),
            ..page.clone()
        };
        let server = serving(Some(path), &moved_on);
        let queue = Queue::new(vec![saved.item], 0, None, Vec::new());
        let playback = Playback::restore(
            server.clone(),
            queue,
            Duration::ZERO,
            Some(resume(&page, saved.entry, shuffled_repeating())),
            None,
            Default::default(),
        )
        .unwrap();
        assert_eq!(
            (playback.is_shuffled(), playback.repeat()),
            (true, Repeat::All)
        );
        run_until(Duration::from_secs(1), || false);
        assert_eq!(
            server.place(),
            Some(later),
            "restoring moved the server's place"
        );
        let moving: Vec<_> = server
            .playlist_calls()
            .into_iter()
            .filter(|c| matches!(c, PlaylistCall::Play { .. } | PlaylistCall::Next { .. }))
            .collect();
        assert_eq!(moving, []);
        playback.stop();
    }

    /// Movie night: two films, then an episode.
    fn movie_night() -> PlaylistPage {
        let videos = Kinds {
            movies: true,
            episodes: true,
            ..Kinds::default()
        };
        let items = vec![
            mock::movie("1", "Arrival"),
            mock::movie("2", "Heat"),
            mock::episode("3", "Severance", 1, 1, "Good News About Hell"),
        ];
        let mut page = mock::page(mock::playlist("night", "Movie night", videos), items);
        page.place = Some(page.entries[0].entry);
        page
    }

    fn playlist_plays_to_the_end(
        name: &str,
        select_preview: bool,
        manual_queue: bool,
        restore: bool,
    ) {
        let dir = TestDir::new_blocking(if name == "All" {
            "spoolframe-music-playlist"
        } else {
            "spoolframe-playlist"
        });
        let path = dir.join("clip.mkv");
        let page = if name == "All" {
            let pipeline = gst::parse::launch(&format!(
                "audiotestsrc num-buffers=40 ! audioconvert ! opusenc ! matroskamux ! filesink location={}",
                path.display()
            )).unwrap();
            let bus = pipeline.bus().unwrap();
            pipeline.set_state(gst::State::Playing).unwrap();
            bus.timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Eos])
                .unwrap();
            pipeline.set_state(gst::State::Null).unwrap();
            all(4)
        } else {
            clip(&path);
            movie_night()
        };
        let total = page.total;
        let first = page.entries[0].clone();
        let server = serving(Some(path), &page);
        let context = glib::MainContext::default();
        let playback = Playback::start(Playlist {
            session: server.clone(),
            items: vec![first.item.clone()],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: Some(resume(&page, first.entry, PlaySettings::default())),
        })
        .unwrap();
        assert!(run_until(Duration::from_secs(5), || playback
            .playlist_up_next()
            .is_some()));
        let preview = playback.playlist_up_next().unwrap().entries;
        assert_eq!(preview.len(), total as usize - 1);
        assert_eq!(playback.playing_from().unwrap().entry, first.entry);
        assert_eq!(
            server.place(),
            Some(first.entry),
            "preview must not advance the server's position"
        );
        if name == "All" && !select_preview {
            let current = playback.current().unwrap().id().clone();
            playback.toggle_shuffle();
            playback.cycle_repeat();
            assert!(playback.is_shuffled());
            assert_eq!(playback.repeat(), Repeat::All);
            context.block_on(playback.playlist_calls.idle()).unwrap();
            let saved = server.settings().unwrap();
            assert!(saved.shuffle);
            assert_eq!(saved.repeat, Repeat::All);
            assert_eq!(playback.current().unwrap().id(), &current);
            assert_eq!(server.place(), Some(first.entry));
            playback.toggle_shuffle();
            playback.cycle_repeat();
            assert_eq!(playback.repeat(), Repeat::One);
            playback.cycle_repeat();
            assert_eq!(playback.repeat(), Repeat::Off);
            context.block_on(playback.playlist_calls.idle()).unwrap();
        }
        if select_preview {
            let chosen = preview[1].entry;
            playback.play_playlist_entry(chosen);
            assert!(run_until(Duration::from_secs(5), || playback
                .playing_from()
                .is_some_and(|p| p.entry == chosen)));
            assert_eq!(
                playback.playlist_up_next().unwrap().entries.len(),
                total as usize - 3
            );
        }
        let mut expected = Vec::new();
        if manual_queue {
            let next = preview[preview.len() - 2].item.clone();
            let last = preview[preview.len() - 1].item.clone();
            playback.add_to_queue(vec![last.clone()], None);
            playback.play_next(vec![next.clone()], None);
            assert_eq!(playback.up_next().queued.len(), 2);
            assert_eq!(
                playback.playlist_up_next().unwrap().entries.len(),
                preview.len()
            );
            expected.push(first.item.id().clone());
            expected.push(next.id().clone());
            expected.push(last.id().clone());
            expected.extend(preview.iter().map(|e| e.item.id().clone()));
        }
        let playback = if restore {
            let bytes =
                serde_json::to_vec(&(playback.snapshot(), playback.playlist_resume())).unwrap();
            let (queue, playlist) = serde_json::from_slice(&bytes).unwrap();
            let session = playback.session.clone().unwrap();
            playback.stop();
            let restored = Playback::restore(
                session,
                queue,
                Duration::ZERO,
                playlist,
                None,
                Default::default(),
            )
            .unwrap();
            assert!(restored.has_next());
            assert_eq!(restored.playing_from().unwrap().id, page.playlist.id);
            assert_eq!(restored.up_next().queued.len(), 2);
            assert!(run_until(Duration::from_secs(10), || restored
                .pipeline
                .element()
                .current_state()
                == gst::State::Paused
                && restored.pipeline.duration().is_some()));
            assert!(!restored.is_playing());
            restored
        } else {
            playback
        };
        let (played, finished) = (Rc::new(RefCell::new(Vec::new())), Rc::new(Cell::new(0)));
        {
            let (played, finished) = (played.clone(), finished.clone());
            playback.subscribe(move |change| {
                match change {
                    Change::Item(item) => played.borrow_mut().push(item.id().clone()),
                    Change::PlaylistFinished(_) => finished.set(finished.get() + 1),
                    _ => {}
                }
                true
            });
        }
        if restore {
            playback.toggle();
        }
        assert!(
            run_until(Duration::from_secs(60), || finished.get() == 1),
            "the playlist did not end (played {:?}, position {:?}, state {:?}, reader {:?})",
            played.borrow(),
            playback.pipeline.position(),
            playback.pipeline.element().current_state(),
            playback.reader_status()
        );
        let mut seen = played.borrow().clone();
        seen.dedup();
        assert_eq!(
            seen.len(),
            total as usize - if select_preview { 2 } else { 0 } + if manual_queue { 2 } else { 0 },
            "played {seen:?}"
        );
        if manual_queue {
            assert_eq!(
                seen, expected,
                "manual queue must play before the remaining playlist entries"
            );
        }
        assert!(playback.playing_from().is_none());
        playback.stop();
    }

    /// The button shown at each of `times` (in seconds) of an episode
    /// 2 700 s long played through, as [`Playback::check_markers`] folds
    /// the tracker's actions.
    fn shown_through(
        markers: Vec<Marker>,
        has_next: bool,
        times: &[u64],
    ) -> Vec<Option<MarkerOffer>> {
        let mut tracker = MarkerTracker::new(Markers::new(markers, has_next), false);
        let length = Some(Duration::from_secs(2_700));
        let (mut marked, mut up_next) = (None, false);
        times
            .iter()
            .map(|&at| {
                for action in tracker.on_position(Duration::from_secs(at), length, true) {
                    match action {
                        MarkerAction::Offer(offer) => marked = offer,
                        MarkerAction::UpNext(due) => up_next = due,
                        MarkerAction::AutoSkip { .. } => {}
                    }
                }
                shown_offer(marked, up_next)
            })
            .collect()
    }

    /// An episode with no markers went on to the next with no Next
    /// offered (a user, testing the 0.17 branch): Next shows for its last
    /// 30 s when something follows, and not before.
    #[test]
    fn with_no_credits_marker_next_shows_for_the_last_thirty_seconds() {
        assert_eq!(
            shown_through(Vec::new(), true, &[2_660, 2_675]),
            [None, Some(MarkerOffer::Next)]
        );
        assert_eq!(shown_through(Vec::new(), false, &[2_675]), [None]);
    }

    /// Credits a scene follows offer their skip; past them, Up next is due
    /// from where they started, so Next shows.
    #[test]
    fn past_credits_a_scene_follows_next_shows() {
        let credits = Marker {
            kind: spoolframe_linux_core::backend::MarkerKind::Credits,
            start: Duration::from_secs(2_600),
            end: Duration::from_secs(2_650),
            to_end: false,
        };
        assert_eq!(
            shown_through(vec![credits], true, &[2_610, 2_660]),
            [
                Some(MarkerOffer::SkipCredits {
                    to: Duration::from_secs(2_650)
                }),
                Some(MarkerOffer::Next)
            ]
        );
    }

    /// Credits that run to the end show Next in Up next's place.
    #[test]
    fn credits_to_the_end_show_next() {
        let credits = Marker {
            kind: spoolframe_linux_core::backend::MarkerKind::Credits,
            start: Duration::from_secs(2_600),
            end: Duration::from_secs(2_700),
            to_end: true,
        };
        assert_eq!(
            shown_through(vec![credits], true, &[2_599, 2_650]),
            [None, Some(MarkerOffer::Next)]
        );
    }

    /// An intro from 1 s to 2.5 s of the four-second clip.
    fn intro() -> Vec<spoolframe_linux_core::backend::Marker> {
        vec![spoolframe_linux_core::backend::Marker {
            kind: spoolframe_linux_core::backend::MarkerKind::Intro,
            start: Duration::from_millis(1_000),
            end: Duration::from_millis(2_500),
            to_end: false,
        }]
    }

    /// A film with an intro, playing from the start.
    fn play_with_intro(dir: &TestDir) -> Rc<Playback> {
        let path = dir.join("clip.mkv");
        clip(&path);
        start_film(&Arc::new(films(path).with_markers(intro())), None)
    }

    /// Inside the intro the player offers Skip Intro (P2-10), and taking
    /// it plays on from the intro's end.
    fn the_intro_is_offered_and_skipped_on_asking() {
        let dir = TestDir::new_blocking("spoolframe-offer");
        let playback = play_with_intro(&dir);
        let offered = Rc::new(Cell::new(None));
        {
            let offered = offered.clone();
            playback.subscribe(move |change| {
                if let Change::Offer(offer) = change {
                    offered.set(*offer);
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || offered.get()
                == Some(MarkerOffer::SkipIntro {
                    to: Duration::from_millis(2_500)
                })),
            "Skip Intro was never offered (offered {:?})",
            offered.get()
        );
        playback.take_offer();
        assert!(
            run_until(Duration::from_secs(10), || playback.position()
                >= Duration::from_millis(2_500)
                && offered.get().is_none()),
            "taking the offer did not pass the intro (at {:?}, offered {:?})",
            playback.position(),
            offered.get()
        );
        playback.stop();
    }

    /// With skipping on, the intro is skipped by itself; Undo goes back
    /// into it, and it is not skipped again.
    fn an_automatic_intro_skip_happens_once_and_undoes() {
        let dir = TestDir::new_blocking("spoolframe-autoskip");
        let playback = play_with_intro(&dir);
        playback.set_skip_intros(true);
        let skips = Rc::new(Cell::new(0));
        {
            let skips = skips.clone();
            playback.subscribe(move |change| {
                if let Change::IntroSkipped = change {
                    skips.set(skips.get() + 1);
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || skips.get() == 1
                && playback.position() >= Duration::from_millis(2_500)),
            "the intro was not skipped (skips {}, at {:?})",
            skips.get(),
            playback.position()
        );
        playback.undo_intro_skip();
        assert!(
            run_until(Duration::from_secs(10), || playback.position()
                < Duration::from_millis(2_000)),
            "Undo did not go back into the intro (at {:?})",
            playback.position()
        );
        // Through the rest of the intro without another skip.
        run_until(Duration::from_secs(2), || false);
        assert_eq!(skips.get(), 1);
        playback.stop();
    }

    /// Undo on a converted stream restarts its encode back in the intro,
    /// and views show where at once, before the new encode plays.
    fn undoing_a_converted_intro_skip_shows_where_it_goes_back_to() {
        let dir = TestDir::new_blocking("spoolframe-converted-undo");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = films(path).converting(64 * 1024).with_markers(intro());
        let playback = start_film(&Arc::new(server), None);
        playback.set_skip_intros(true);
        let skipped = Rc::new(Cell::new(false));
        {
            let skipped = skipped.clone();
            playback.subscribe(move |change| {
                if let Change::IntroSkipped = change {
                    skipped.set(true);
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || skipped.get()),
            "the converted intro was not skipped (at {:?})",
            playback.position()
        );
        assert!(playback.position() >= Duration::from_millis(2_500));
        playback.undo_intro_skip();
        assert!(
            playback.position() < Duration::from_millis(2_000),
            "Undo did not show the intro at once (at {:?})",
            playback.position()
        );
        assert!(playback.now().unwrap().restarting());
        playback.stop();
    }

    /// Quitting mid-film: shutdown returns only once the server has the
    /// position, which used to be cancelled with the process.
    fn quitting_waits_for_the_last_progress() {
        let dir = TestDir::new_blocking("spoolframe-quit");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path));
        let playback = start_film(&server, None);
        let played = Rc::new(Cell::new(Duration::ZERO));
        {
            let played = played.clone();
            playback.subscribe(move |change| {
                if let Change::Position { at, .. } = change {
                    played.set(*at);
                }
                true
            });
        }
        assert!(run_until(Duration::from_secs(20), || played.get()
            > Duration::from_secs(1)));
        let before = server.reports().len();
        shutdown();
        assert!(
            server.reports().len() > before,
            "shutdown returned before the last progress reached the server"
        );
    }

    /// A playback dropped while its start is asked: the server's handle
    /// is closed as the answer comes, with no main loop to hear it. The
    /// drop waits for the server to have the start: one dropped before
    /// is overtaken by its end, and never asked.
    fn dropping_a_start_cleans_up_its_server_handle() {
        let dir = TestDir::new_blocking("spoolframe-abandoned-start");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path).converting(64 * 1024));
        let playback = start_film(&server, None);
        // Do not dispatch the main loop until the server has admitted the start.
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while server.open() == 0 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(server.starts().len(), 1);
        drop(playback);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while server.open() > 0 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(server.open(), 0);
    }

    /// Next while the first film is still asked for: the second plays,
    /// nothing says it failed, and the first is closed on the server.
    fn a_start_the_next_overtook_never_plays() {
        let dir = TestDir::new_blocking("spoolframe-overtaken-start");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path));
        let items = vec![film(), second_film()];
        let second = items[1].id().clone();
        let playback = Playback::start(Playlist {
            session: server.clone(),
            items,
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: None,
        })
        .unwrap();
        let failed = Rc::new(Cell::new(false));
        {
            let failed = failed.clone();
            playback.subscribe(move |change| {
                if matches!(change, Change::Failed(_) | Change::Skipped(_)) {
                    failed.set(true);
                }
                true
            });
        }
        playback.next();
        assert!(
            run_until(Duration::from_secs(20), || playback.position()
                > Duration::from_millis(500)),
            "the second film never played"
        );
        assert_eq!(playback.current().map(|i| i.id().clone()), Some(second));
        assert!(!failed.get(), "the overtaken start was shown as a failure");
        assert!(
            run_until(Duration::from_secs(5), || server.open() == 1),
            "the overtaken start was left open ({} open)",
            server.open()
        );
        playback.stop();
        assert!(run_until(Duration::from_secs(5), || server.open() == 0));
    }

    fn a_converted_playback_restarts_its_encode_on_a_seek() {
        let dir = TestDir::new_blocking("spoolframe-convert");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path).converting(64 * 1024));
        let playback = start_film(&server, None);
        let played = Rc::new(Cell::new(Duration::ZERO));
        {
            let played = played.clone();
            playback.subscribe(move |change| {
                if let Change::Position { at, .. } = change {
                    played.set(*at);
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || played.get()
                > Duration::from_secs(1)),
            "the converted playback never got past a second"
        );
        assert!(matches!(
            playback.plan(),
            Some(PlaybackPlan::Converted { .. })
        ));
        let old_uri = playback.pipeline.playback_uri().unwrap();
        let restarts = playback.restarts();
        playback.seek(Duration::from_secs(3));
        assert!(
            playback.now().unwrap().restarting(),
            "a seek did not restart the converted encode"
        );
        // The controller sends the restart once the seeks have gathered,
        // and the old reader is retired then: its read errors from there
        // on are the old epoch's.
        assert!(
            run_until(Duration::from_secs(5), || !playback
                .pipeline
                .accepts_error(&old_uri)),
            "the restart was never sent"
        );
        playback.on(Event::Error(
            old_uri.clone(),
            "old epoch read failed".into(),
        ));
        assert!(!playback.has_failed(), "the old epoch's error failed it");
        // The mock's encode starts again from the file's start (it has no
        // -copyts), so the position drops back; playback must go on.
        assert!(
            run_until(Duration::from_secs(20), || playback.restarts() > restarts
                && playback.is_playing()
                && played.get() < Duration::from_secs(3)
                && played.get() > Duration::from_millis(500)),
            "playback did not go on after the restart (playing {}, at {:?})",
            playback.is_playing(),
            played.get()
        );
        playback.on(Event::Error(old_uri, "late old-reader error".into()));
        assert!(!playback.has_failed());
        assert_eq!(server.seeks(), [Duration::from_secs(3)]);
        assert_eq!(server.open(), 1);
        playback.stop();
        run_until(Duration::from_secs(2), || server.open() == 0);
        assert_eq!(server.open(), 0, "the encode was not ended");
    }

    /// A converted output being encoded has no length of its own, and
    /// GstPlay may say none: the views show the catalog's from the start.
    fn a_converted_playback_shows_the_catalogs_length() {
        let dir = TestDir::new_blocking("spoolframe-converted-length");
        let path = dir.join("clip.mkv");
        clip_lasting(&path, 12);
        let server = Arc::new(films(path).converting(64 * 1024));
        let catalog = crate::views::Facts::of(&film())
            .length
            .expect("the film has a length");
        let playback = start_film(&server, None);
        let seen = Rc::new(Cell::new(false));
        {
            let seen = seen.clone();
            playback.subscribe(move |change| {
                if let Change::Position { length, .. } = change {
                    seen.set(seen.get() || *length == catalog);
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(10), || seen.get()),
            "the views never had the catalog's length ({catalog:?})"
        );
        playback.stop();
    }

    /// A converted playback's subtitle, read as text by the server
    /// (P2-07), arrives after playback started: GstPlay cannot take it on
    /// a stream that cannot seek, so the encode restarts where it is with
    /// the subtitle, and playback goes on.
    fn a_late_subtitle_restarts_a_converted_playback_with_it() {
        let dir = TestDir::new_blocking("spoolframe-late-subtitle");
        let path = dir.join("clip.mkv");
        // Outlasts the SDK's wait before it asks for the subtitle again.
        clip_lasting(&path, 12);
        let server = films(path).converting(64 * 1024).reading_subtitle(1);
        let playback = start_film(&Arc::new(server), None);
        let played = Rc::new(Cell::new(Duration::ZERO));
        {
            let played = played.clone();
            playback.subscribe(move |change| {
                if let Change::Position { at, .. } = change {
                    played.set(*at);
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || played.get()
                > Duration::from_millis(300)),
            "the converted playback never played"
        );
        assert!(
            run_until(Duration::from_secs(20), || playback.restarts() > 0),
            "the late subtitle did not restart the encode"
        );
        assert!(
            playback
                .now()
                .is_some_and(|c| c.subtitle_file.borrow().is_some())
                && playback.subtitles_on.get(),
            "the subtitle is not shown"
        );
        // The mock's new encode starts from the file's start (it has no
        // -copyts): positions from it are under where playback was.
        played.set(Duration::ZERO);
        assert!(
            run_until(Duration::from_secs(20), || playback.is_playing()
                && played.get() > Duration::from_millis(300)),
            "playback did not go on after the restart (playing {}, at {:?})",
            playback.is_playing(),
            played.get()
        );
        playback.stop();
    }

    /// The film converted with no subtitle, as the episode started, on a
    /// server that copies a chosen one into the output; and the track
    /// failures it tells of. Played past its first second.
    fn converted_without_a_subtitle(path: PathBuf) -> (Rc<Playback>, Rc<RefCell<Vec<String>>>) {
        let server = films(path).converting(64 * 1024).copying_subtitle();
        // Without a subtitle, as the episode started.
        let playback = Playback::start(Playlist {
            session: Arc::new(server),
            items: vec![film()],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: Some(Selection::default()),
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: None,
        })
        .unwrap();
        let failed = Rc::new(RefCell::new(Vec::<String>::new()));
        {
            let failed = failed.clone();
            playback.subscribe(move |change| {
                if let Change::TrackFailed(message) = change {
                    failed.borrow_mut().push(message.to_string());
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || playback.position()
                > Duration::from_secs(1)),
            "the converted playback never played"
        );
        assert!(
            !playback.plan().as_ref().is_some_and(carries_its_subtitle),
            "the first encode already had the subtitle"
        );
        (playback, failed)
    }

    /// Whether a converted output has its subtitle as a stream to show.
    fn carries_its_subtitle(plan: &PlaybackPlan) -> bool {
        tracks::map_selection(&MediaTracks::default(), plan, Selection::default()).subtitle
            == SubtitleHandling::InOutput
    }

    /// Whether `playback` shows the subtitle its output now carries.
    fn shows_the_copied_subtitle(playback: &Playback) -> bool {
        playback.plan().as_ref().is_some_and(carries_its_subtitle)
            && playback.is_playing()
            && playback.subtitles_on.get()
    }

    /// A subtitle chosen mid-play (one just downloaded) on a converted
    /// playback starts it again with the subtitle copied into the output
    /// (`copy:srt`), and the player shows that stream. It said "Couldn't
    /// turn on subtitles" instead: the first encode had none, so subtitles
    /// were off, and GstPlay refused the new encode's track until they
    /// were turned on.
    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_subtitle_chosen_mid_play_shows_from_the_converted_output() {
        gtk::init().unwrap();
        init().unwrap();
        let dir = TestDir::new_blocking("spoolframe-copied-subtitle");
        let path = dir.join("clip.mkv");
        clip_with_subtitle(&path);
        let (playback, failed) = converted_without_a_subtitle(path);
        playback.use_subtitle(0);
        assert!(
            run_until(Duration::from_secs(20), || shows_the_copied_subtitle(
                &playback
            ) || !failed.borrow().is_empty()),
            "the subtitle never showed (plan {:?}, playing {})",
            playback.plan(),
            playback.is_playing()
        );
        assert_eq!(*failed.borrow(), Vec::<String>::new());
        playback.stop();
    }

    /// The menu names a subtitle stream the server's way, by the item's
    /// track it carries: GstPlay knows only its format ("SubRip", Banshee
    /// S3E9), which said nothing of which track it was. A pick of it is
    /// what shows, for Timing and the next track asked for.
    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_carried_subtitle_is_named_as_the_server_names_it_and_a_pick_shows_it() {
        gtk::init().unwrap();
        init().unwrap();
        let dir = TestDir::new_blocking("spoolframe-named-subtitle");
        let path = dir.join("clip.mkv");
        clip_with_subtitle(&path);
        let (playback, failed) = converted_without_a_subtitle(path);
        playback.use_subtitle(1);
        assert!(
            run_until(Duration::from_secs(20), || shows_the_copied_subtitle(
                &playback
            ) || !failed.borrow().is_empty()),
            "the subtitle never showed (plan {:?})",
            playback.plan()
        );
        assert_eq!(*failed.borrow(), Vec::<String>::new());
        let info = || playback.now()?.media_info.borrow().clone();
        assert!(
            run_until(Duration::from_secs(20), || info()
                .is_some_and(|i| !i.subtitle_streams().is_empty())),
            "GstPlay never listed the carried subtitle"
        );
        let (current, info) = (playback.now().unwrap(), info().unwrap());
        let name = {
            let tracks = current.track_list.borrow();
            spoolframe_linux_core::format::subtitle_track(&tracks.as_ref().unwrap().subtitles[1])
        };
        assert_eq!(playback.subtitle_streams(&info), [Some((1, name))]);
        playback.subtitle_picked()(None);
        let off = current.selected.get().and_then(|s| s.subtitle);
        playback.subtitle_picked()(Some(1));
        let on = current.selected.get().and_then(|s| s.subtitle);
        assert_eq!((off, on), (None, Some(1)));
        playback.stop();
    }

    /// A playback started without a subtitle offers the item's other
    /// tracks in its menu, a downloaded one among them, though its stream
    /// has none of them; picking one starts it again with that track,
    /// which shows. The menu listed only the stream's own, so a track
    /// downloaded before could be had only by searching for it again.
    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn the_menu_offers_a_track_the_stream_lacks_and_shows_it() {
        use gtk::prelude::*;
        gtk::init().unwrap();
        init().unwrap();
        let dir = TestDir::new_blocking("spoolframe-menu-track");
        let path = dir.join("clip.mkv");
        clip_with_subtitle(&path);
        let (playback, failed) = converted_without_a_subtitle(path);
        let info = playback.now().unwrap().media_info.borrow().clone().unwrap();
        let offered = playback.subtitles_elsewhere().map(|e| e.tracks);
        assert!(
            offered.as_ref().is_some_and(|t| !t.is_empty()),
            "the menu offers no track the stream lacks ({offered:?})"
        );
        let menu = playback.track_menu(&info);
        // Hung from a button, as the player's is.
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&menu));
        // The menu's lists: audio, subtitles, quality.
        let mut lists = Vec::new();
        let mut stack = vec![menu.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if widget.has_css_class("pick-list") {
                lists.push(widget.clone().downcast::<gtk::ListBox>().unwrap());
            }
            let mut child = widget.last_child();
            while let Some(c) = child {
                child = c.prev_sibling();
                stack.push(c);
            }
        }
        let subtitles = &lists[1];
        // Off, the stream's own, then the first track it lacks.
        let row = 1 + info.subtitle_streams().len() as i32;
        subtitles.row_at_index(row).unwrap().emit_activate();
        assert!(
            run_until(Duration::from_secs(20), || shows_the_copied_subtitle(
                &playback
            ) || !failed.borrow().is_empty()),
            "the picked track never showed (plan {:?}, playing {})",
            playback.plan(),
            playback.is_playing()
        );
        assert_eq!(*failed.borrow(), Vec::<String>::new());
        button.set_popover(None::<&gtk::Popover>);
        drop((lists, button, menu));
        playback.stop();
    }

    /// A queue saved at the last run comes back prerolled at its position,
    /// paused, and plays on Play (P1-23).
    fn a_restored_queue_waits_paused_where_it_was() {
        let dir = TestDir::new_blocking("spoolframe-restore");
        let path = dir.join("clip.mkv");
        clip(&path);
        let queue = Queue::new(vec![film()], 0, None, Vec::new());
        let playback = Playback::restore(
            Arc::new(films(path)),
            queue,
            Duration::from_secs(2),
            None,
            None,
            Default::default(),
        )
        .unwrap();
        let played = Rc::new(Cell::new(Duration::ZERO));
        {
            let played = played.clone();
            playback.subscribe(move |change| {
                if let Change::Position { at, .. } = change {
                    played.set(*at);
                }
                true
            });
        }
        // Prerolled and sought, then left paused for a while.
        run_until(Duration::from_secs(5), || false);
        assert!(
            !playback.is_playing() && played.get() >= Duration::from_secs(2),
            "playing {} at {:?}",
            playback.is_playing(),
            played.get()
        );
        playback.toggle();
        assert!(
            run_until(Duration::from_secs(10), || playback.is_playing()
                && played.get() > Duration::from_millis(2_200)),
            "Play did not play on from the saved position"
        );
        playback.stop();
    }

    /// A resume prerolls paused, seeks, and plays from there by itself.
    fn resuming_plays_from_where_it_stopped() {
        let dir = TestDir::new_blocking("spoolframe-resume");
        let path = dir.join("clip.mkv");
        clip(&path);
        let playback = start_film(&Arc::new(films(path)), Some(Duration::from_secs(2)));
        let played = Rc::new(Cell::new(Duration::ZERO));
        {
            let played = played.clone();
            playback.subscribe(move |change| {
                if let Change::Position { at, .. } = change {
                    played.set(*at);
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || playback.is_playing()
                && played.get() > Duration::from_millis(2_200)),
            "playing {} at {:?}",
            playback.is_playing(),
            played.get()
        );
        playback.stop();
    }

    /// Pausing reports the position at once, not at the next 10 s tick.
    fn pausing_saves_where_playback_got_to() {
        let dir = TestDir::new_blocking("spoolframe-pause");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path));
        let playback = start_film(&server, None);
        let played = Rc::new(Cell::new(Duration::ZERO));
        {
            let played = played.clone();
            playback.subscribe(move |change| {
                if let Change::Position { at, .. } = change {
                    played.set(*at);
                }
                true
            });
        }
        assert!(run_until(Duration::from_secs(20), || played.get()
            > Duration::from_secs(1)));
        playback.toggle();
        assert!(
            run_until(Duration::from_secs(3), || !server.reports().is_empty()),
            "pausing reported nothing"
        );
        let (_, position) = server.reports()[0].clone();
        assert!(position >= Duration::from_secs(1), "reported {position:?}");
        playback.stop();
    }

    /// Choosing a bitrate while a film plays makes it a conversion that
    /// plays on from there, and Original brings the file back (P6-06).
    fn a_quality_chosen_while_playing_plans_it_again() {
        use spoolframe_client_sdk::PlaybackQuality;
        let dir = TestDir::new_blocking("spoolframe-quality");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path));
        let playback = start_film(&server, None);
        assert!(run_until(Duration::from_secs(20), || playback.position()
            > Duration::from_secs(1)));
        let failed = || playback.has_failed();
        let plays_as = |converted: bool| {
            run_until(Duration::from_secs(20), || {
                failed()
                    || (matches!(playback.plan(), Some(PlaybackPlan::Converted { .. }))
                        == converted
                        && playback.is_playing())
            }) && !failed()
        };
        playback.set_quality(PlaybackQuality::Bitrate(4_000_000));
        assert!(plays_as(true), "the 4 Mbit/s choice did not play converted");
        playback.set_quality(PlaybackQuality::Original);
        assert!(plays_as(false), "Original did not play the file again");
        let asked: Vec<_> = server
            .quality_changes()
            .into_iter()
            .map(|(q, _)| q)
            .collect();
        assert_eq!(
            asked,
            [
                PlaybackQuality::Bitrate(4_000_000),
                PlaybackQuality::Original
            ]
        );
        assert_eq!(server.open(), 1);
        playback.stop();
    }

    /// A quality picked on the item page is the one the playback starts
    /// with.
    fn a_quality_chosen_before_playing_starts_with_it() {
        let dir = TestDir::new_blocking("spoolframe-quality-start");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path));
        let capped = spoolframe_client_sdk::PlaybackQuality::Bitrate(2_000_000);
        let playback = Playback::start(Playlist {
            session: server.clone(),
            items: vec![film()],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: capped,
            display: None,
            from_playlist: None,
        })
        .unwrap();
        assert!(
            run_until(Duration::from_secs(20), || playback.plan().is_some()),
            "the playback never started"
        );
        let asked: Vec<_> = server
            .starts()
            .into_iter()
            .map(|s| s.options.quality)
            .collect();
        assert_eq!(asked, [capped]);
        assert!(matches!(
            playback.plan(),
            Some(PlaybackPlan::Converted { .. })
        ));
        playback.stop();
    }

    /// A direct play the link is too slow for is planned again as a
    /// conversion (P6-06): the SDK swaps the plan in and the direct
    /// reader's read fails as restarted. The player reads the conversion
    /// from where the viewer was, instead of failing.
    fn a_direct_play_planned_again_plays_on_converted() {
        let dir = TestDir::new_blocking("spoolframe-replanned");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path));
        let playback = start_film(&server, None);
        assert!(run_until(Duration::from_secs(20), || playback.position()
            > Duration::from_secs(1)));
        assert!(matches!(playback.plan(), Some(PlaybackPlan::Direct { .. })));
        let managed = playback.now().unwrap().managed.borrow().clone().unwrap();
        glib::MainContext::default()
            .block_on(managed.limit(1_000_000))
            .unwrap();
        // What the direct reader's next read tells the player.
        let uri = playback.pipeline.playback_uri().unwrap();
        playback.on(Event::Replanned(uri));
        let failed = || playback.has_failed();
        assert!(
            run_until(Duration::from_secs(20), || failed()
                || (matches!(
                    playback.plan(),
                    Some(PlaybackPlan::Converted { .. })
                ) && playback.is_playing()
                    && playback.position() > Duration::from_millis(500))),
            "the conversion did not play (plan {:?}, playing {}, at {:?})",
            playback.plan(),
            playback.is_playing(),
            playback.position()
        );
        assert!(!failed());
        assert_eq!(server.open(), 1);
        playback.stop();
    }

    /// The server ends a converted playback mid-read, as one that idled
    /// out: the SDK starts it again, and its reader, whose bytes were the
    /// old encode's, is replaced from the new one's start. Playback goes
    /// on instead of failing.
    fn a_converted_playback_the_server_ended_plays_on() {
        let dir = TestDir::new_blocking("spoolframe-resumed");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = films(path).converting(16 * 1024).expiring_at(32 * 1024);
        let server = Arc::new(server);
        let playback = start_film(&server, None);
        let failed = Rc::new(RefCell::new(None));
        {
            let failed = failed.clone();
            playback.subscribe(move |change| {
                if let Change::Failed(error) = change {
                    failed.replace(Some(error.to_string()));
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || playback.position()
                > Duration::from_secs(1)
                || failed.borrow().is_some()),
            "the resumed playback never got past a second"
        );
        assert_eq!(failed.borrow().clone(), None);
        assert_eq!(server.starts().len(), 2);
        assert_eq!(server.open(), 1);
        playback.stop();
    }

    /// Paused playbacks on a mock server, apart from
    /// [`playback_on_a_mock_server`]: each waits paused for
    /// [`PAUSED_PAST_IDLE`], minutes of the SDK's real keep-alive.
    #[test]
    #[ignore = "needs a display for gtk4paintablesink; waits 5 minutes"]
    fn paused_playbacks_on_a_mock_server() {
        gtk::init().unwrap();
        init().unwrap();
        a_paused_film_keeps_its_playback_alive();
        a_paused_song_keeps_its_playback_alive();
    }

    /// Paused past the server's idle limit, a film keeps its playback: the
    /// SDK sends the paused position every
    /// [`ManagedPlayback::KEEP_ALIVE`], which counts as use. With no reads
    /// or reports, the server ends a paused playback (5 minutes then).
    fn a_paused_film_keeps_its_playback_alive() {
        let dir = TestDir::new_blocking("spoolframe-paused-film");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path).with_handle_idle(PAUSED_IDLE));
        let playback = start_film(&server, None);
        assert!(run_until(Duration::from_secs(20), || playback.position()
            > Duration::from_secs(1)));
        playback.toggle();
        assert!(run_until(Duration::from_secs(3), || !playback.is_playing()));
        let paused_at = playback.position();
        run_until(PAUSED_PAST_IDLE, || false);
        assert_eq!(server.live(), 1, "the paused playback idled out");
        let reports = server.reports();
        assert!(
            reports.len() >= 3,
            "paused, the film reported only {reports:?}"
        );
        let (_, last) = reports.last().unwrap().clone();
        assert!(
            last.abs_diff(paused_at) < Duration::from_millis(500),
            "reported {last:?}, paused at {paused_at:?}"
        );
        playback.stop();
    }

    /// A paused song keeps its playback too, by reporting where it is
    /// paused: tracks are recorded now (PX-52).
    fn a_paused_song_keeps_its_playback_alive() {
        let dir = TestDir::new_blocking("spoolframe-paused-song");
        let path = dir.join("clip.mkv");
        clip(&path);
        let track = song();
        let track_id = &track.id().clone();
        let server = MockPlayback::new(Some(path), [track.clone()]);
        let server = Arc::new(server.with_handle_idle(PAUSED_IDLE));
        let playback = Playback::start(Playlist {
            session: server.clone(),
            items: vec![track],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: None,
        })
        .unwrap();
        assert!(run_until(Duration::from_secs(20), || playback.position()
            > Duration::from_secs(1)));
        playback.toggle();
        assert!(run_until(Duration::from_secs(3), || !playback.is_playing()));
        run_until(PAUSED_PAST_IDLE, || false);
        assert_eq!(server.live(), 1, "the paused playback idled out");
        let reports = server.reports();
        assert!(
            reports.len() >= 2 && reports.iter().all(|(id, _)| id == track_id),
            "paused, the song reported only {reports:?}"
        );
        playback.stop();
    }

    /// The pause tests' server's idle limit: past the SDK's keep-alive
    /// and the 10 s tick that sends it.
    const PAUSED_IDLE: Duration = Duration::from_secs(75);

    /// How long the pause tests wait paused: twice their server's idle
    /// limit, and two keep-alives.
    const PAUSED_PAST_IDLE: Duration = Duration::from_secs(150);

    /// A film played from `clip`: it starts, plays, moves on, tells a
    /// view that attaches late where it is, and stops.
    fn a_playback_plays_and_tells_a_late_view_where_it_is() {
        let dir = TestDir::new_blocking("spoolframe-playback");
        let path = dir.join("clip.mkv");
        clip(&path);
        let playback = start_film(&Arc::new(films(path)), None);

        let played = Rc::new(Cell::new(Duration::ZERO));
        {
            let played = played.clone();
            playback.subscribe(move |change| {
                if let Change::Position { at, .. } = change {
                    played.set(*at);
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || played.get()
                > Duration::from_secs(1)),
            "playback never got past a second"
        );
        assert!(playback.is_playing());

        // A view made now hears the item and the position at once.
        let heard = Rc::new(RefCell::new((false, Duration::ZERO)));
        {
            let heard = heard.clone();
            playback.subscribe(move |change| {
                match change {
                    Change::Item(_) => heard.borrow_mut().0 = true,
                    Change::Position { at, .. } => heard.borrow_mut().1 = *at,
                    _ => {}
                }
                true
            });
        }
        let (item_heard, position) = *heard.borrow();
        assert!(item_heard && position > Duration::from_secs(1));

        playback.stop();
        run_until(Duration::from_secs(2), || !playback.is_playing());
        assert!(!playback.is_playing());
    }

    /// The admin stops the playback before the file is read: the player
    /// says it stopped, not that it failed.
    fn a_playback_the_admin_stopped_says_so() {
        let dir = TestDir::new_blocking("spoolframe-stopped");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = films(path).with_playbacks_stopped();
        let playback = start_film(&Arc::new(server), None);
        let (stopped, failed) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        {
            let (stopped, failed) = (stopped.clone(), failed.clone());
            playback.subscribe(move |change| {
                match change {
                    Change::Stopped => stopped.set(true),
                    Change::Failed(_) => failed.set(true),
                    _ => {}
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || stopped.get() || failed.get()),
            "neither stopped nor failed"
        );
        assert!(stopped.get() && !failed.get());
        playback.stop();
    }

    fn a_session_ended_under_a_playback_says_so() {
        let dir = TestDir::new_blocking("spoolframe-signed-out");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = films(path).with_sessions_ending();
        let playback = start_film(&Arc::new(server), None);
        let (stopped, failed) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        {
            let (stopped, failed) = (stopped.clone(), failed.clone());
            playback.subscribe(move |change| {
                match change {
                    Change::SignedOut => stopped.set(true),
                    Change::Failed(_) => failed.set(true),
                    _ => {}
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || stopped.get() || failed.get()),
            "neither stopped nor failed"
        );
        assert!(stopped.get() && !failed.get());
        playback.stop();
    }

    /// The server goes away before the file is read: the playback says it
    /// failed (the player shows the error) instead of waiting forever, and
    /// shutting down afterwards releases it.
    fn a_server_that_goes_away_fails_the_playback() {
        let dir = TestDir::new_blocking("spoolframe-gone");
        let path = dir.join("clip.mkv");
        clip(&path);
        let server = Arc::new(films(path));
        server.go_away();
        let playback = start_film(&server, None);
        let failed = Rc::new(RefCell::new(None::<String>));
        {
            let failed = failed.clone();
            playback.subscribe(move |change| {
                if let Change::Failed(error) = change {
                    failed.replace(Some(error.to_string()));
                }
                true
            });
        }
        assert!(
            run_until(Duration::from_secs(20), || failed.borrow().is_some()),
            "the playback never said it failed"
        );
        shutdown();
        assert!(!playback.is_playing());
    }

    /// A finished download (PX-38) plays from its file through playbin3
    /// with no session, alone: no queue to carry or save. Pausing keeps its
    /// place on the client, and playing to its end marks it watched there,
    /// which stopping at the end does not undo.
    fn a_finished_download_plays_offline_to_its_end() {
        let dir = TestDir::new_blocking("spoolframe-download");
        let path = dir.join("clip.mkv");
        clip(&path);
        let download = mock::playback::download("clip", film(), path);
        let client = Arc::new(MockDownloads::new(download.clone()));
        let item = LocalItem::of(&download, client.clone()).unwrap();
        let playback = Playback::play_local(item, None, None, None).unwrap();
        assert!(playback.session().is_none() && playback.carry().is_empty());
        let kept = |call: DownloadCall| client.calls().contains(&call);
        let kept_place = || {
            client.calls().iter().any(
                |call| matches!(call, DownloadCall::Position(at) if *at >= Duration::from_secs(1)),
            )
        };
        assert!(
            run_until(Duration::from_secs(20), || playback.position()
                > Duration::from_secs(1)),
            "the download never got past a second"
        );
        playback.toggle();
        assert!(
            run_until(Duration::from_secs(3), kept_place),
            "pausing kept no place ({:?})",
            client.calls()
        );
        playback.toggle();
        assert!(
            run_until(Duration::from_secs(20), || kept(DownloadCall::Watched(
                true
            ))),
            "the end did not mark it watched ({:?})",
            client.calls()
        );
        playback.stop();
        run_until(Duration::from_secs(1), || false);
        let calls = client.calls();
        let watched = calls.iter().position(|c| *c == DownloadCall::Watched(true));
        assert_eq!(
            calls.last(),
            Some(&DownloadCall::Watched(true)),
            "stopping at the end undid it ({calls:?}, watched at {watched:?})"
        );
    }

    /// The film every film test plays, two hours long in the catalog.
    fn film() -> Item {
        let Item::Movie(movie) = mock::movie("1", "Arrival") else {
            unreachable!("a movie")
        };
        Item::Movie(spoolframe_types::Movie {
            duration: Some(Duration::from_secs(116 * 60)),
            ..movie
        })
    }

    /// The film after it.
    fn second_film() -> Item {
        mock::movie("2", "Heat")
    }

    /// A song.
    fn song() -> Item {
        mock::track("1", "Tone")
    }

    /// A server playing the films from `path`.
    fn films(path: PathBuf) -> MockPlayback {
        MockPlayback::new(Some(path), [film(), second_film()])
    }

    /// The film playing on `server`, from `from`.
    fn start_film(server: &Arc<MockPlayback>, from: Option<Duration>) -> Rc<Playback> {
        Playback::start(Playlist {
            session: server.clone(),
            items: vec![film()],
            index: 0,
            queued: Vec::new(),
            from,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: None,
        })
        .unwrap()
    }
}
