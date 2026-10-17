//! What is playing, apart from any window onto it (P1-16). GstPlay
//! (playbin3) reads the server's file through the spoolframe:// source and
//! draws into gtk4paintablesink. The playlist, the track choices and the
//! progress reports live here.
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
use spoolframe_client_sdk::PlaybackOptions;
use spoolframe_linux_core::backend::{
    BackendError, Marker, PlaybackHandle, PlaybackPlan, Selection, Session,
};
use spoolframe_linux_core::markers;
pub use spoolframe_linux_core::markers::Offer;
use spoolframe_linux_core::next_episode::next_episode;
use spoolframe_linux_core::playback::{self as managed, ManagedPlayback};
use spoolframe_linux_core::playlists::{Advance, Playing};
use spoolframe_linux_core::progressive::ProgressiveReader;
use spoolframe_linux_core::reader::{self, ByteSource, MediaReader, Spawn};
use spoolframe_linux_core::sdk::backend_error;
use spoolframe_protocol::{ConvertedVideo, PlannedSubtitle, SubtitleAction, SubtitleDelivery};
use spoolframe_types::Delivery;
use spoolframe_types::{Album, Item, MediaTracks, SubtitleFormat};

use crate::pipeline::Pipeline;
pub use crate::pipeline::VideoSink;
pub use crate::queue::Repeat;
use crate::queue::{Queue, Queued};

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
    /// the server's next entry.
    pub from_playlist: Option<Playing>,
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
    Offer(Option<Offer>),
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
    /// The rest of the context, each with its position for
    /// [`Playback::play_context_at`].
    pub context: Vec<(usize, Item)>,
    /// The context's album, for "Next from: <album>".
    pub from: Option<Album>,
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

/// How long to wait before asking again for a subtitle the server is
/// still reading as text; less in tests, whose clip is four seconds.
const SUBTITLE_RETRY: Duration = if cfg!(test) {
    Duration::from_millis(1_000)
} else {
    Duration::from_secs(5)
};

/// How often a playback the server would otherwise see unused is used:
/// it ends one with no reads or progress for 5 minutes, and a paused
/// film, or a slow converted one with a full buffer, reads nothing for
/// longer. Less in tests, which wait past a short idle limit.
const KEEP_ALIVE: Duration = if cfg!(test) {
    Duration::from_millis(500)
} else {
    Duration::from_secs(60)
};

/// How often progress goes to the server while playing (P1-14 throttles
/// on its side too). Ten seconds is what play history asks for (PX-52):
/// one report credits at most 30 s of play.
const PROGRESS_EVERY: u32 = 10;

/// How long an automatic intro skip can be undone.
pub const UNDO_WINDOW: Duration = Duration::from_secs(5);

/// How long a direct seek's positions are held back at most, should
/// GstPlay not say it landed (a seek before the file is open).
const SEEK_SETTLE: Duration = Duration::from_secs(2);

/// An automatic intro skip: where from, and when.
#[derive(Clone, Copy)]
struct Skip {
    from: Duration,
    at: Instant,
}

pub struct Playback {
    /// GstPlay and what it plays through; nothing else touches them.
    pipeline: Pipeline,
    session: Arc<dyn Session>,
    /// What plays, and what plays next.
    queue: RefCell<Queue>,
    /// The item playing, and everything that is its alone; `None` once
    /// stopped.
    current: RefCell<Option<Rc<Current>>>,
    /// The user paused (and has not played since): a converted seek's new
    /// encode stays paused too.
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
    /// The server's playlist being played, and the entry playing.
    from_playlist: RefCell<Option<Playing>>,
    /// Its next entry is being asked for: once at a time.
    asking_playlist: Cell<bool>,
    /// Itself, for callbacks that outlive a borrow (the track menu's).
    me: Weak<Self>,
    progress_tail: RefCell<Option<tokio::sync::oneshot::Receiver<()>>>,
}

/// What belongs to the item playing now, and lives exactly as long as it
/// plays: moving on builds a new one. Work in flight holds it weakly, so
/// an answer for an item already left finds nothing and is dropped; what
/// is left to tell apart within the item is its [`Stage`].
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
    stage: RefCell<Stage>,
    /// Bumped on each converted restart, so a slow one does not replace a
    /// newer.
    restarts: Cell<u64>,
    /// The playback the server opened, for its subtitle.
    managed: RefCell<Option<ManagedPlayback>>,
    /// How the server plays the item, for the diagnostics.
    plan: RefCell<Option<PlaybackPlan>>,
    track_list: RefCell<Option<MediaTracks>>,
    /// The tracks that play, to pick once the file's streams are known.
    pending: Cell<Option<Selection>>,
    media_info: RefCell<Option<gst_play::PlayMediaInfo>>,
    /// The item's intro and credits (P2-10), in its own time.
    markers: RefCell<Vec<Marker>>,
    /// The button over the video now: emitted only when it changes.
    offered: Cell<Option<Offer>>,
    /// The automatic intro skip, to undo; set once skipped, so the item
    /// skips its intro once (a restart of it keeps this).
    intro_skipped: Cell<Option<Skip>>,
    /// An external subtitle, written where GstPlay can open it.
    subtitle_file: RefCell<Option<PathBuf>>,
    failure: RefCell<Option<String>>,
}

/// Where the current item is. An event that does not belong to the stage
/// it finds is dropped: this replaces the counters and flags that told a
/// late answer from a current one.
enum Stage {
    /// The server is asked to start the item.
    Asking,
    /// GstPlay has the file. `resume`: where to seek once its length is
    /// known (direct play; a converted encode begins there).
    Open {
        mode: Mode,
        resume: Option<Duration>,
    },
    /// A direct seek in flight, until GstPlay says it landed (or
    /// [`SEEK_SETTLE`]): the pipeline's positions meanwhile are from
    /// before it.
    Seeking {
        since: Instant,
    },
    /// A converted restart (P2-06): the server is asked, once `sent`, for
    /// a new encode at `to`, and the old encode's positions are ignored.
    /// Seeks while scrubbing gather into the one not yet sent.
    Restarting {
        to: Duration,
        sent: bool,
    },
    Failed,
}

/// How the file reaches GstPlay.
#[derive(Clone)]
enum Mode {
    Direct,
    Converted,
}

impl Current {
    fn new(item: Item, requested: Option<Selection>, tracks: Option<MediaTracks>) -> Self {
        Self {
            catalog_length: crate::views::Facts::of(&item).length,
            item,
            requested,
            asked: Instant::now(),
            pictured: Cell::new(false),
            stage: RefCell::new(Stage::Asking),
            restarts: Cell::new(0),
            managed: RefCell::new(None),
            plan: RefCell::new(None),
            track_list: RefCell::new(tracks),
            pending: Cell::new(None),
            media_info: RefCell::new(None),
            markers: RefCell::new(Vec::new()),
            offered: Cell::new(None),
            intro_skipped: Cell::new(None),
            subtitle_file: RefCell::new(None),
            failure: RefCell::new(None),
        }
    }

    fn is_converted(&self) -> bool {
        matches!(
            &*self.stage.borrow(),
            Stage::Open {
                mode: Mode::Converted,
                ..
            } | Stage::Restarting { .. }
        )
    }

    fn settled(&self) -> bool {
        matches!(&*self.stage.borrow(), Stage::Open { resume: None, .. })
    }

    /// Whether the intro was skipped by itself within [`UNDO_WINDOW`].
    fn undoable(&self) -> bool {
        self.intro_skipped
            .get()
            .is_some_and(|skip| skip.at.elapsed() < UNDO_WINDOW)
    }

    /// Milliseconds since the item was asked for.
    fn since_asked(&self) -> u64 {
        self.asked.elapsed().as_millis() as u64
    }
}

impl Drop for Current {
    fn drop(&mut self) {
        if let Some(path) = self.subtitle_file.take() {
            let _ = std::fs::remove_file(path);
        }
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
        let playback = Self::build(playlist.session, queue, start, true)?;
        playback.from_playlist.replace(playlist.from_playlist);
        Ok(playback)
    }

    /// The server's playlist being played, if one is.
    pub fn playing_from(&self) -> Option<Playing> {
        self.from_playlist.borrow().clone()
    }

    /// Brings back a queue saved at the last run (P1-23): the current item
    /// prerolled at `at`, paused, waiting for Play.
    pub fn restore(
        session: Arc<dyn Session>,
        queue: Queue,
        at: Duration,
        display: Option<spoolframe_types::DisplayCapabilities>,
    ) -> anyhow::Result<Rc<Self>> {
        let start = Start {
            from: (!at.is_zero()).then_some(at),
            selection: None,
            tracks: None,
            quality: Default::default(),
            display,
        };
        Self::build(session, queue, start, false)
    }

    /// The queue as it stands, to save.
    pub fn snapshot(&self) -> Queue {
        self.queue.borrow().clone()
    }

    fn build(
        session: Arc<dyn Session>,
        queue: Queue,
        start: Start,
        autoplay: bool,
    ) -> anyhow::Result<Rc<Self>> {
        let pipeline = Pipeline::new()?;
        let playback = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            pipeline,
            session,
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
            from_playlist: RefCell::new(None),
            asking_playlist: Cell::new(false),
            progress_tail: RefCell::new(None),
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

        let weak = Rc::downgrade(&playback);
        glib::timeout_add_seconds_local(PROGRESS_EVERY, move || {
            let Some(playback) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if playback.playing.get() {
                playback.report_progress();
            }
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(&playback);
        glib::timeout_add_local(KEEP_ALIVE, move || {
            let Some(playback) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            playback.keep_alive();
            glib::ControlFlow::Continue
        });
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

    /// What plays after the current item: the queue, then the context.
    pub fn up_next(&self) -> UpNext {
        let queue = self.queue.borrow();
        UpNext {
            queued: queue.queued().cloned().collect(),
            context: queue
                .context_ahead()
                .into_iter()
                .map(|(pos, item)| (pos, item.clone()))
                .collect(),
            from: queue.album().cloned(),
        }
    }

    /// The queue, to carry into the playback that replaces this one.
    pub fn carry(&self) -> Vec<Queued> {
        self.queue.borrow().carry()
    }

    /// Plays the queued item `id`; it leaves the queue.
    pub fn play_queued(self: &Rc<Self>, id: u64) {
        self.report_progress();
        self.step(|q| q.play_queued(id));
    }

    /// Jumps the context to `pos` (from [`UpNext::context`]), keeping the
    /// queue.
    pub fn play_context_at(self: &Rc<Self>, pos: usize) {
        self.report_progress();
        // Picked by hand: the server's playlist no longer says what follows.
        self.from_playlist.replace(None);
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

    /// Shuffles what is still to come in the context, or puts it back in
    /// order, keeping the current item playing. The queue is never
    /// shuffled.
    pub fn toggle_shuffle(&self) {
        let on = !self.is_shuffled();
        self.queue
            .borrow_mut()
            .set_shuffle(on, |n| glib::random_int_range(0, n as i32) as usize);
        self.emit_modes();
        self.emit(&Change::Queue);
    }

    pub fn is_shuffled(&self) -> bool {
        self.queue.borrow().shuffle()
    }

    pub fn repeat(&self) -> Repeat {
        self.queue.borrow().repeat()
    }

    /// Off, then the whole context, then the one item, then off again.
    pub fn cycle_repeat(&self) {
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
    /// it (preview frames).
    pub fn session(&self) -> Arc<dyn Session> {
        self.session.clone()
    }

    /// The video frames, for any number of pictures.
    pub fn paintable(&self) -> &gtk::gdk::Paintable {
        self.pipeline.paintable()
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
        // Quality is a film's or an episode's; music plays as it is.
        let quality = self.is_video().then(|| {
            let (current, choose) = (self.me.clone(), self.me.clone());
            crate::tracks::QualityMenu {
                current: Rc::new(move || {
                    current
                        .upgrade()
                        .map_or((Default::default(), None), |this| {
                            let height = crate::quality::encoded_height(this.plan().as_ref());
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
        self.pipeline
            .track_menu(info, &self.subtitles_on, failed, quality)
    }

    /// What the screen shows, for the items started from now on: the user
    /// changed Show HDR.
    pub fn set_display(&self, display: Option<spoolframe_types::DisplayCapabilities>) {
        self.display.replace(display);
    }

    /// Plans the playing item again for `quality` (P6-06), from where it
    /// is, and keeps the choice for the items after it. A new plan gets a
    /// new reader, as a resumed or re-planned playback does; the same plan
    /// plays on.
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
                    let changed = next.handle != before.handle
                        || next.epoch != before.epoch
                        || next.plan != before.plan;
                    if changed {
                        tracing::info!(?quality, "the playback was planned again for its quality");
                        playback.read_anew(at);
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
            self.pipeline.pause();
        } else {
            self.pipeline.play_now();
        }
    }

    /// Seeks within the item: in the file itself for direct play; a
    /// converted output cannot seek, and the server restarts the encode
    /// there instead.
    pub fn seek(self: &Rc<Self>, to: Duration) {
        let Some(current) = self.now() else { return };
        if current.is_converted() {
            return self.restart(&current, to);
        }
        if matches!(
            &*current.stage.borrow(),
            Stage::Open { .. } | Stage::Seeking { .. }
        ) {
            self.seek_direct(&current, to);
        }
    }

    /// A seek in the file. Views show the target at once; the pipeline's
    /// positions wait until it lands.
    fn seek_direct(self: &Rc<Self>, current: &Current, to: Duration) {
        current.stage.replace(Stage::Seeking {
            since: Instant::now(),
        });
        self.pipeline.seek(to);
        self.moved_to(to);
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

    /// Asks the server to restart the converted encode at `to`, and plays
    /// the new encode from its start. matroskademux does not pass a time
    /// seek up to a source it cannot seek in bytes (checked with a spike),
    /// and each encode is a fresh live stream without an index, so the
    /// pipeline is given a new one rather than sought. The output keeps
    /// the film's own time (the server's -copyts), so positions need no
    /// offset. Seeks while scrubbing gather into one restart.
    fn restart(self: &Rc<Self>, current: &Rc<Current>, to: Duration) {
        {
            let mut stage = current.stage.borrow_mut();
            let start_restart = match &mut *stage {
                Stage::Restarting {
                    to: unsent,
                    sent: false,
                    ..
                } => {
                    *unsent = to;
                    false
                }
                Stage::Restarting { .. }
                | Stage::Open {
                    mode: Mode::Converted,
                    ..
                } => true,
                _ => return,
            };
            if start_restart {
                *stage = Stage::Restarting { to, sent: false };
                drop(stage);
                let restart = current.restarts.get() + 1;
                current.restarts.set(restart);
                let (weak, current_weak) = (Rc::downgrade(self), Rc::downgrade(current));
                glib::timeout_add_local_once(Duration::from_millis(250), move || {
                    if let (Some(playback), Some(current)) =
                        (weak.upgrade(), current_weak.upgrade())
                    {
                        playback.send_restart(&current, restart);
                    }
                });
            }
        }
        // The seek bar shows the target straight away.
        self.moved_to(to);
    }

    /// The gathered restart goes to the server.
    fn send_restart(self: &Rc<Self>, current: &Rc<Current>, restart: u64) {
        if !self.is_current(current) || current.restarts.get() != restart {
            return;
        }
        let to = match &mut *current.stage.borrow_mut() {
            Stage::Restarting { to, sent, .. } if !*sent => {
                *sent = true;
                *to
            }
            _ => return,
        };
        let managed = current.managed.borrow().clone().expect("started playback");
        let stopped = self.pipeline.stop_and_wait();
        let sought = relm4::spawn(async move {
            let _ = stopped.await;
            managed.seek(to).await.map_err(backend_error)
        });
        let (weak, current) = (Rc::downgrade(self), Rc::downgrade(current));
        glib::spawn_future_local(async move {
            let answer = sought.await;
            if let Some(playback) = weak.upgrade() {
                playback.on(Event::Sought {
                    current,
                    restart,
                    to,
                    answer,
                });
            }
        });
    }

    /// The server's answer to a restart: the new encode plays, as the
    /// user left it (paused stays paused; the pipeline's own state is no
    /// guide mid-restart, as scrubbing restarts again before the last
    /// restart has started playing).
    fn sought(
        self: &Rc<Self>,
        current: Weak<Current>,
        restart: u64,
        to: Duration,
        answer: Answer<spoolframe_client_sdk::Playback>,
    ) {
        let Some(current) = current.upgrade().filter(|c| self.is_current(c)) else {
            return;
        };
        if !matches!(
            &*current.stage.borrow(),
            Stage::Restarting { sent: true, .. }
        ) || current.restarts.get() != restart
        {
            return;
        }
        let next = match answer {
            Ok(Ok(next)) => next,
            Ok(Err(e)) => return self.failed(&e.to_string()),
            Err(e) => return self.failed(&format!("seeking failed: {e}")),
        };
        self.reload(&current, next, to);
    }

    /// The playback changed under its reader: the SDK started it again
    /// after the server ended it (it idled out), or the server planned it
    /// again for a slow link (P6-06), a direct play becoming a conversion.
    /// Either way the reader's bytes are not the new plan's, so a new
    /// reader, for the plan the SDK now has, plays from `at`, as after a
    /// seek's restart.
    fn read_anew(self: &Rc<Self>, at: Duration) {
        let Some(current) = self.now() else { return };
        let Some(managed) = current.managed.borrow().clone() else {
            return;
        };
        if let Stage::Restarting { .. } = &*current.stage.borrow() {
            // A restart under way replaces the reader itself.
            return;
        }
        self.reload(&current, managed.snapshot(), at);
        self.moved_to(at);
    }

    /// Plays `next`, the current item's new encode, from `to`, with a new
    /// reader, paused or playing as the user left it.
    fn reload(
        self: &Rc<Self>,
        current: &Rc<Current>,
        next: spoolframe_client_sdk::Playback,
        to: Duration,
    ) {
        current.plan.replace(Some(next.plan.clone()));
        let managed = current.managed.borrow().clone().expect("started playback");
        let (mode, resume, reader) =
            match playback_reader(managed, Some(to), current.catalog_length) {
                Ok(parts) => parts,
                Err(error) => return self.failed(&error),
            };
        let subtitle = current.subtitle_file.borrow().clone();
        self.pipeline.load(reader, subtitle.as_deref());
        current.stage.replace(Stage::Open { mode, resume });
        current.media_info.replace(None);
        // The new encode has the same streams; they are picked again.
        let selected = match next.plan {
            PlaybackPlan::Direct { .. } => current
                .requested
                .or(next.chosen.map(|c| c.selection))
                .unwrap_or_default(),
            _ => Selection::default(),
        };
        current.pending.set(Some(selected));
        if self.autoplay.get() && !self.held.get() {
            self.pipeline.play_now();
        } else {
            self.pipeline.pause();
        }
    }

    /// Uses the current playback, so the server does not end it as idle
    /// (see [`KEEP_ALIVE`]). Paused, the item reports where it is paused.
    /// A converted playback, which reads nothing while its buffer is
    /// full, reads a byte.
    fn keep_alive(&self) {
        let Some(current) = self.now() else { return };
        let paused = !self.playing.get() && !self.stopped.get();
        if paused && self.report_progress().is_some() {
            return;
        }
        let converted = current.is_converted();
        if !paused && !converted {
            return;
        }
        let Some(managed) = current.managed.borrow().clone() else {
            return;
        };
        relm4::spawn(async move {
            let read = if converted {
                managed
                    .read_chunk(&managed.snapshot(), 0, 1)
                    .await
                    .map(drop)
            } else {
                managed.read(0, 1).await.map(drop)
            };
            if let Err(e) = read {
                tracing::debug!(error = %e, "keeping the playback alive");
            }
        });
    }

    /// Ends the playback after its queued progress reports finish.
    fn end_handle(&self, current: &Current) -> Option<tokio::task::JoinHandle<()>> {
        let managed = current.managed.borrow().clone()?;
        Some(self.ordered_call(async move {
            if let Err(e) = managed.close().await {
                tracing::debug!(error = %e, "stopping the conversion");
            }
        }))
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
    }

    /// Does what the button over the video offers: past the intro or the
    /// credits, or on to what follows.
    pub fn take_offer(self: &Rc<Self>) {
        match self.now().and_then(|c| c.offered.get()) {
            Some(Offer::SkipIntro { to } | Offer::SkipCredits { to }) => self.seek(to),
            Some(Offer::NextItem) => self.next(),
            None => {}
        }
    }

    /// Back to where the intro was skipped from. It is not skipped again.
    pub fn undo_intro_skip(self: &Rc<Self>) {
        if let Some(skip) = self.now().and_then(|c| c.intro_skipped.get()) {
            self.seek(skip.from);
        }
    }

    /// Whether Next plays something. Video goes on through its context
    /// (a show's episodes) only; the queue is music's.
    pub fn has_next(&self) -> bool {
        self.next_from() != NextFrom::Nothing
    }

    fn next_from(&self) -> NextFrom {
        let queue = self.queue.borrow();
        let playlist = self.from_playlist.borrow().is_some();
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

    pub fn has_previous(&self) -> bool {
        self.queue.borrow().has_previous()
    }

    /// The first queued item, else the context's next, if there is one.
    pub fn next(self: &Rc<Self>) {
        self.report_progress();
        self.advance();
    }

    /// Moves on from the current item; false at the end.
    fn advance(self: &Rc<Self>) -> bool {
        match self.next_from() {
            NextFrom::Here if self.is_video() => self.step(Queue::next_in_context),
            NextFrom::Here => self.step(Queue::next),
            NextFrom::Playlist => {
                self.ask_playlist();
                true
            }
            NextFrom::Nothing => false,
        }
    }

    /// Asks the server what follows the playlist's entry playing.
    fn ask_playlist(self: &Rc<Self>) {
        let Some(playing) = self.playing_from() else {
            return;
        };
        if self.asking_playlist.replace(true) {
            return;
        }
        let session = self.session.clone();
        let asked =
            relm4::spawn(async move { session.next_in_playlist(&playing.id, playing.entry).await });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = asked.await else { return };
            if let Some(playback) = weak.upgrade() {
                playback.on(Event::PlaylistNext(answer));
            }
        });
    }

    /// The server's next entry: it plays, from where the user stopped in
    /// it; or the playlist ended.
    fn playlist_next(
        self: &Rc<Self>,
        answer: Result<Option<spoolframe_linux_core::backend::PlaylistNext>, BackendError>,
    ) {
        self.asking_playlist.set(false);
        if self.stopped.get() {
            return;
        }
        // Out of the borrow before anything is told: listeners read it.
        let advance = {
            let mut playing = self.from_playlist.borrow_mut();
            let Some(playing) = playing.as_mut() else {
                return;
            };
            match answer {
                Ok(next) => Ok(playing.advance(next)),
                Err(e) => Err(format!("Couldn’t go on with {}: {e}", playing.name)),
            }
        };
        let advance = match advance {
            Ok(advance) => advance,
            // Next asks again.
            Err(text) => return self.emit(&Change::Skipped(&text)),
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
                if let Some(playing) = self.from_playlist.take() {
                    self.emit(&Change::PlaylistFinished(&playing));
                }
            }
        }
    }

    /// What played before, or the start of this one when it has played a
    /// few seconds, as music players do.
    pub fn previous(self: &Rc<Self>) {
        if self.position.get() > Duration::from_secs(3) || !self.has_previous() {
            self.seek(Duration::ZERO);
        } else {
            self.step(Queue::previous);
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
        let report = self.report_progress();
        // The item goes: answers still on their way find nothing.
        let current = self.current.take();
        // Down to NULL now, not whenever the last reference goes (see
        // `shutdown`), and off the main thread (see `crate::pipeline`).
        [
            report,
            current.and_then(|c| self.end_handle(&c)),
            Some(self.pipeline.dismantle()),
        ]
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
        if let Some(last) = self.current.take() {
            self.end_handle(&last);
        }
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

    /// Asks the server to start `current` from `from`. A converted
    /// playback's encode begins there; direct play seeks itself.
    fn ask(self: &Rc<Self>, current: &Rc<Current>, from: Option<Duration>) {
        current.stage.replace(Stage::Asking);
        let need_tracks = current.track_list.borrow().is_none();
        let (session, id, requested) = (
            self.session.clone(),
            current.item.id().clone(),
            current.requested,
        );
        let quality = self.quality.get();
        let display = self.display.borrow().clone();
        let pending = self.progress_tail.borrow_mut().take();
        let started = relm4::spawn(async move {
            if let Some(pending) = pending {
                let _ = pending.await;
            }
            // The track list maps the chosen tracks onto the file's
            // streams; it does not wait on the start.
            let tracks = async {
                match need_tracks {
                    true => session.tracks(&id).await.ok(),
                    false => None,
                }
            };
            let opening = ManagedPlayback::open(
                managed::backend(session.clone()),
                id.clone(),
                requested,
                PlaybackOptions {
                    start: from,
                    quality,
                    display,
                    ..Default::default()
                },
            );
            let (playback, tracks) = tokio::join!(opening, tracks);
            (playback.map_err(backend_error), tracks)
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

    /// The server's answer to starting an item: GstPlay gets its file.
    fn started(self: &Rc<Self>, current: Weak<Current>, from: Option<Duration>, answer: Started) {
        let current = current
            .upgrade()
            .filter(|c| self.is_current(c) && matches!(&*c.stage.borrow(), Stage::Asking));
        let Some(current) = current else {
            return;
        };
        tracing::debug!(ms = current.since_asked(), "the server answered");
        let item = &current.item;
        let (managed, tracks) = match answer {
            Ok((Ok(start), tracks)) => (start, tracks),
            Ok((Err(e), _)) => return self.could_not_start(item, &e.to_string()),
            Err(e) => {
                return self.could_not_start(item, &format!("starting playback failed: {e}"));
            }
        };
        let mut start = managed.snapshot();
        if tracks.is_some() {
            current.track_list.replace(tracks);
        }
        // What plays: the tracks asked for, or the server's choice.
        let selection = current
            .requested
            .or(start.chosen.map(|c| c.selection))
            .unwrap_or_default();
        current.pending.set(Some(selection));
        current.plan.replace(Some(start.plan.clone()));
        current.markers.replace(std::mem::take(&mut start.markers));
        // After the answer, not alongside the start: it is needed only at
        // the credits.
        self.follow_episode(&current);
        let external = subtitle_comes_as_file(&start.plan);
        let handle = start.handle.clone();
        current.managed.replace(Some(managed.clone()));
        let (mode, resume, reader) = match playback_reader(managed, from, current.catalog_length) {
            Ok(parts) => parts,
            Err(error) => return self.failed(&error),
        };
        // A subtitle already here (the same item started again after its
        // handle idled out) goes with the file.
        let subtitle = current.subtitle_file.borrow().clone();
        self.pipeline.load(reader, subtitle.as_deref());
        // A converted encode begins at `from`: that is where playback is
        // until its positions arrive (a restart for a late subtitle goes
        // from here, not from 0).
        if let (Mode::Converted, Some(at)) = (&mode, from) {
            self.position.set(at);
        }
        current.stage.replace(Stage::Open { mode, resume });
        if external && subtitle.is_none() {
            self.load_external_subtitle(&current, handle);
        }
        // Resuming: preroll paused, seek there once the length is known,
        // then play. Playing at once read and decoded the start of the
        // file first, only to jump away from it. A restored queue
        // prerolls and waits.
        if resume.is_some() || !self.autoplay.get() {
            self.pipeline.pause();
        } else {
            self.pipeline.play_now();
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
        if self.from_playlist.borrow().is_some() {
            return;
        }
        // What is ahead in the context, not counting Repeat All's wrap:
        // the show's next episode still follows a one-item context.
        if !self.queue.borrow().context_ahead().is_empty() {
            return;
        }
        let (session, episode) = (self.session.clone(), episode.clone());
        let found = relm4::spawn(async move { next_episode(session.as_ref(), &episode).await });
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

    /// Seeks to where the user stopped and plays, once the length is
    /// known (a seek before that does not land).
    fn resume_if_ready(self: &Rc<Self>, current: &Current) {
        if self.length.get().is_zero() {
            return;
        }
        let at = match &mut *current.stage.borrow_mut() {
            Stage::Open { resume, .. } => resume.take(),
            _ => None,
        };
        let Some(at) = at else { return };
        tracing::debug!(?at, ms = current.since_asked(), "resume seek");
        // The seek shows the position: paused, GstPlay reports none, and
        // views would show the start until Play.
        self.seek_direct(current, at);
        if self.autoplay.get() {
            self.pipeline.play_now();
        }
    }

    /// Fetches the playback's external subtitle; GstPlay takes subtitles
    /// only by URI, so it is handed over as a file.
    fn load_external_subtitle(self: &Rc<Self>, current: &Rc<Current>, handle: PlaybackHandle) {
        let session = self.session.clone();
        let fetched = {
            let handle = handle.clone();
            relm4::spawn(async move { session.read_subtitle(&handle).await })
        };
        let (weak, current) = (Rc::downgrade(self), Rc::downgrade(current));
        glib::spawn_future_local(async move {
            let Ok(answer) = fetched.await else { return };
            if let Some(playback) = weak.upgrade() {
                playback.on(Event::Subtitle {
                    current,
                    handle,
                    answer,
                });
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
        handle: PlaybackHandle,
        answer: Result<(SubtitleFormat, Vec<u8>), BackendError>,
    ) {
        let Some(current) = current.upgrade().filter(|c| self.is_current(c)) else {
            return;
        };
        let (format, bytes) = match answer {
            Ok(subtitle) => subtitle,
            // Still being read as text (P2-07): play on, ask again.
            Err(BackendError::SubtitleNotReady(done)) => {
                tracing::debug!(done, "the subtitle is still being read");
                let (weak, current) = (Rc::downgrade(self), Rc::downgrade(&current));
                glib::timeout_add_local_once(SUBTITLE_RETRY, move || {
                    if let (Some(playback), Some(current)) = (weak.upgrade(), current.upgrade())
                        && playback.is_current(&current)
                    {
                        playback.load_external_subtitle(&current, handle);
                    }
                });
                return;
            }
            Err(e) => return tracing::warn!(error = %e, "no external subtitle"),
        };
        let Some(dir) = spoolframe_linux_core::paths::runtime_dir() else {
            return tracing::warn!("no private directory for the subtitle");
        };
        // The app's runtime directory exists only once something makes it.
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return tracing::warn!(error = %e, dir = %dir.display(), "cannot make the runtime directory");
        }
        let path = dir.join(format!(
            "spoolframe-subtitle-{}.{}",
            std::process::id(),
            match format {
                SubtitleFormat::Ass => "ass",
                SubtitleFormat::WebVtt => "vtt",
                _ => "srt",
            }
        ));
        if let Err(e) = std::fs::write(&path, bytes) {
            return tracing::warn!(error = %e, path = %path.display(), "cannot write the subtitle");
        }
        current.subtitle_file.replace(Some(path.clone()));
        self.subtitles_on.set(true);
        if current.is_converted() {
            self.restart(&current, self.position.get());
        } else {
            self.pipeline.set_subtitle(Some(&path));
        }
    }

    /// Where playback is. A track's report counts toward its plays
    /// (PX-52), though a track has no resume point.
    fn report_progress(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.send_progress(self.pipeline.position()?)
    }

    /// Tells the server the current item is at `position`.
    fn send_progress(&self, position: Duration) -> Option<tokio::task::JoinHandle<()>> {
        let current = self.now()?;
        let managed = current.managed.borrow().clone()?;
        Some(self.ordered_call(async move {
            if let Err(e) = managed.report_position(position) {
                tracing::warn!(error = %e, "progress not queued");
                return;
            }
            if let Err(e) = managed.flush().await {
                tracing::warn!(error = %e, "progress not saved");
            }
        }))
    }

    /// Preserve event order and finish progress before closing its playback handle.
    fn ordered_call(
        &self,
        call: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let previous = self.progress_tail.borrow_mut().take();
        let (done, next) = tokio::sync::oneshot::channel();
        self.progress_tail.replace(Some(next));
        relm4::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            call.await;
            let _ = done.send(());
        })
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
        self.resume_if_ready(&current);
        self.emit(&Change::Position {
            at: self.position.get(),
            length,
        });
    }

    /// A position from the pipeline: current only while the item is open
    /// and not mid-seek or mid-restart.
    fn position_updated(self: &Rc<Self>, at: gst::ClockTime) {
        let Some(current) = self.now() else { return };
        let live = {
            let mut stage = current.stage.borrow_mut();
            match &*stage {
                Stage::Open { .. } => true,
                // GstPlay did not say the seek landed; it has by now.
                Stage::Seeking { since } if since.elapsed() >= SEEK_SETTLE => {
                    *stage = Stage::Open {
                        mode: Mode::Direct,
                        resume: None,
                    };
                    true
                }
                _ => false,
            }
        };
        if live {
            self.moved_to(Duration::from_nanos(at.nseconds()));
        }
    }

    /// GstPlay's seek landed: its positions are current again.
    fn seek_done(&self) {
        if let Some(current) = self.now() {
            let mut stage = current.stage.borrow_mut();
            if matches!(&*stage, Stage::Seeking { .. }) {
                *stage = Stage::Open {
                    mode: Mode::Direct,
                    resume: None,
                };
            }
        }
    }

    /// Playback is at `at`, by the pipeline or by a seek: views hear it
    /// and the markers there are checked. Every position goes through
    /// here.
    fn moved_to(self: &Rc<Self>, at: Duration) {
        self.position.set(at);
        self.emit(&Change::Position {
            at,
            length: self.length.get(),
        });
        self.check_markers(at);
    }

    /// At `at`: skips the intro if the user asked for that, else offers
    /// what the markers there call for.
    fn check_markers(self: &Rc<Self>, at: Duration) {
        let Some(current) = self.now() else { return };
        if current.markers.borrow().is_empty() {
            return;
        }
        let offer = markers::offer(&current.markers.borrow(), at, self.has_next());
        // Only where a seek lands: not mid-seek, mid-restart, or before
        // the length is known.
        if let Some(Offer::SkipIntro { to }) = offer
            && self.skip_intros.get()
            && current.intro_skipped.get().is_none()
            && current.settled()
            && !self.length.get().is_zero()
        {
            current.intro_skipped.set(Some(Skip {
                from: at,
                at: Instant::now(),
            }));
            self.set_offer(&current, None);
            self.seek(to);
            self.emit(&Change::IntroSkipped);
            return;
        }
        self.set_offer(&current, offer);
    }

    fn set_offer(&self, current: &Current, offer: Option<Offer>) {
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
            self.resume_if_ready(current);
            let left = match &mut *current.stage.borrow_mut() {
                Stage::Open { resume, .. } => resume.take(),
                _ => None,
            };
            if left.is_some() && self.autoplay.get() {
                self.pipeline.play_now();
            }
        }
        let playing = state == gst_play::PlayState::Playing;
        let paused = self.playing.replace(playing) && !playing;
        if paused && !self.stopped.get() {
            self.report_progress();
        }
        self.emit(&Change::Playing(playing));
    }

    fn media_info_updated(&self, info: &gst_play::PlayMediaInfo) {
        let Some(current) = self.now() else { return };
        current.media_info.replace(Some(info.clone()));
        let has_streams = !info.audio_streams().is_empty() || !info.subtitle_streams().is_empty();
        if has_streams && let Some(selection) = current.pending.take() {
            if current.is_converted() {
                self.apply_converted(&current, info);
            } else {
                self.apply(&current, selection, info);
            }
        }
        // GstPlay does not pick an external subtitle by itself, and lists
        // it only once its pipeline has it (on each restart for it).
        if current.subtitle_file.borrow().is_some() && self.pipeline.show_external_subtitle(info) {
            self.subtitles_on.set(true);
        }
        self.emit(&Change::MediaInfo(info));
    }

    /// A converted output holds only the tracks picked: one audio, and the
    /// subtitle when the server copied or converted it into the output.
    /// One read as text (OCR) comes as a file; one burned in is in the
    /// picture.
    fn apply_converted(&self, current: &Current, info: &gst_play::PlayMediaInfo) {
        if current.subtitle_file.borrow().is_some() {
            return;
        }
        let in_output = current
            .plan
            .borrow()
            .as_ref()
            .is_some_and(subtitle_in_output);
        match self.pipeline.select_subtitle(info, in_output.then_some(0)) {
            Ok(()) => self.subtitles_on.set(in_output),
            Err(e) => {
                tracing::warn!(error = %e, "cannot pick the converted subtitle");
                self.emit(&Change::TrackFailed(SUBTITLES_FAILED));
            }
        }
    }

    /// Picks the tracks that play. The selection counts positions in the
    /// item's track list; the file's streams are in the same order,
    /// external subtitles aside.
    fn apply(&self, current: &Current, selection: Selection, info: &gst_play::PlayMediaInfo) {
        if let Some(audio) = selection.audio
            && let Err(e) = self.pipeline.select_audio(info, audio as usize)
        {
            tracing::warn!(error = %e, audio, "cannot pick the audio track");
            self.emit(&Change::TrackFailed(AUDIO_FAILED));
        }
        let track_list = current.track_list.borrow();
        let embedded = selection.subtitle.and_then(|n| {
            let subtitles = &track_list.as_ref()?.subtitles;
            let track = subtitles.get(n as usize)?;
            (!track.external).then(|| {
                subtitles[..n as usize]
                    .iter()
                    .filter(|t| !t.external)
                    .count()
            })
        });
        // An external subtitle is turned on when it arrives.
        if embedded.is_none() && current.subtitle_file.borrow().is_some() {
            return;
        }
        match self.pipeline.select_subtitle(info, embedded) {
            Ok(()) => self.subtitles_on.set(embedded.is_some()),
            Err(e) => {
                tracing::warn!(error = %e, ?embedded, "cannot pick the subtitle");
                if embedded.is_some() {
                    self.emit(&Change::TrackFailed(SUBTITLES_FAILED));
                }
            }
        }
    }

    /// The item played to its end: report it, then play the next one.
    fn ended(self: &Rc<Self>) {
        if let Some(length) = self.pipeline.duration() {
            self.send_progress(length);
        }
        if self.repeat() == Repeat::One {
            self.seek(Duration::ZERO);
            self.pipeline.play_now();
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
            current.stage.replace(Stage::Failed);
            current.failure.replace(Some(error.to_owned()));
        }
        self.emit(&Change::Failed(error));
    }

    #[cfg(test)]
    fn restarts(&self) -> u64 {
        self.now().map_or(0, |c| c.restarts.get())
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Whether the plan's subtitle comes as a file to fetch: a direct play's
/// external one, or one the server reads as text for a converted
/// playback (P2-07).
fn subtitle_comes_as_file(plan: &PlaybackPlan) -> bool {
    matches!(
        plan,
        PlaybackPlan::Direct {
            subtitle: Some(PlannedSubtitle {
                delivery: SubtitleDelivery::External,
                ..
            }),
            ..
        } | PlaybackPlan::Converted {
            subtitle: Some(spoolframe_protocol::ConvertedSubtitle {
                action: SubtitleAction::Ocr { .. },
                ..
            }),
            ..
        }
    )
}

/// Whether a converted output carries the subtitle as a stream to show:
/// copied or converted into it. One burned in is in the picture already,
/// and the player shows none of its own (P2-07).
fn subtitle_in_output(plan: &PlaybackPlan) -> bool {
    matches!(
        plan,
        PlaybackPlan::Converted {
            subtitle: Some(spoolframe_protocol::ConvertedSubtitle {
                action: SubtitleAction::Copy { .. } | SubtitleAction::Convert { .. },
                ..
            }),
            ..
        }
    )
}

/// An answer from the runtime: the task may have failed to finish.
type Answer<T> = Result<Result<T, BackendError>, tokio::task::JoinError>;

/// The answer to starting an item, with its track list when asked for.
type Started =
    Result<(Result<ManagedPlayback, BackendError>, Option<MediaTracks>), tokio::task::JoinError>;

/// What happens to a playback from outside its own calls: GstPlay's
/// signals and the server's answers. One entry point, [`Playback::on`];
/// each handler checks the item's [`Stage`] before acting.
enum Event {
    Duration(Option<gst::ClockTime>),
    Position(gst::ClockTime),
    State(gst_play::PlayState),
    MediaInfo(gst_play::PlayMediaInfo),
    Buffering(i32),
    SeekDone,
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
    Sought {
        current: Weak<Current>,
        restart: u64,
        to: Duration,
        answer: Answer<spoolframe_client_sdk::Playback>,
    },
    Subtitle {
        current: Weak<Current>,
        handle: PlaybackHandle,
        answer: Result<(SubtitleFormat, Vec<u8>), BackendError>,
    },
    NextEpisode {
        current: Weak<Current>,
        answer: Result<Option<Item>, BackendError>,
    },
    PlaylistNext(Result<Option<spoolframe_linux_core::backend::PlaylistNext>, BackendError>),
}

impl Playback {
    fn on(self: &Rc<Self>, event: Event) {
        match event {
            Event::Duration(d) => self.duration_changed(d),
            Event::Position(at) => self.position_updated(at),
            Event::State(state) => self.state_changed(state),
            Event::MediaInfo(info) => self.media_info_updated(&info),
            Event::Buffering(percent) => self.buffering(percent),
            Event::SeekDone => self.seek_done(),
            Event::Ended => self.ended(),
            Event::Volume(volume) => self.emit(&Change::Volume(volume)),
            Event::Muted(muted) => self.emit(&Change::Muted(muted)),
            Event::Error(uri, e) => {
                if self.pipeline.accepts_error(&uri) {
                    self.failed(&e);
                }
            }
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
            Event::Resumed(uri, at) => {
                if self.pipeline.accepts_error(&uri) {
                    tracing::info!(
                        at = at.as_secs(),
                        "the server ended the playback; it was started again"
                    );
                    self.read_anew(at);
                }
            }
            Event::Replanned(uri) => {
                if self.pipeline.accepts_error(&uri) {
                    let at = self.position.get();
                    tracing::info!(
                        at = at.as_secs(),
                        "the playback was planned again for the link"
                    );
                    self.read_anew(at);
                }
            }
            Event::Started {
                current,
                from,
                answer,
            } => self.started(current, from, answer),
            Event::Sought {
                current,
                restart,
                to,
                answer,
            } => self.sought(current, restart, to, answer),
            Event::Subtitle {
                current,
                handle,
                answer,
            } => self.subtitle_arrived(current, handle, answer),
            Event::NextEpisode { current, answer } => self.next_episode_found(current, answer),
            Event::PlaylistNext(answer) => self.playlist_next(answer),
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
    adapter.connect_seek_done(move |_, _| on(&g, Event::SeekDone));
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

type ReaderParts = (Mode, Option<Duration>, Box<dyn ByteSource>);
fn playback_reader(
    playback: ManagedPlayback,
    from: Option<Duration>,
    length: Option<Duration>,
) -> Result<ReaderParts, String> {
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
            Ok((
                Mode::Converted,
                None,
                Box::new(ProgressiveReader::new(playback, spawn(), bitrate)),
            ))
        }
        PlaybackPlan::Direct { .. } => {
            let reader = MediaReader::new(playback, spawn(), length);
            if let (Some(at), Some(length)) = (from, length) {
                reader.expect(reader::guess_offset(start.size, length, at));
            }
            Ok((Mode::Direct, from, Box::new(reader)))
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

    use spoolframe_linux_core::backend::{Client, Profile};
    use spoolframe_linux_core::fake::FakeClient;

    use super::*;

    fn converted_with(action: SubtitleAction) -> PlaybackPlan {
        PlaybackPlan::Converted {
            delivery: Delivery::Progressive,
            container: spoolframe_types::Container::Matroska,
            video: None,
            audio: None,
            subtitle: Some(spoolframe_protocol::ConvertedSubtitle { index: 0, action }),
            reasons: Vec::new(),
            ladder: Vec::new(),
        }
    }

    /// A subtitle the plan burns into the picture is neither fetched nor
    /// shown by the player: it would show twice.
    #[test]
    fn a_burned_in_subtitle_is_not_drawn_again() {
        let plan = converted_with(SubtitleAction::BurnIn {
            format: SubtitleFormat::Pgs,
        });
        assert_eq!(
            (subtitle_comes_as_file(&plan), subtitle_in_output(&plan)),
            (false, false)
        );
    }

    #[test]
    fn a_copied_subtitle_is_shown_from_the_output() {
        let plan = converted_with(SubtitleAction::Copy {
            format: SubtitleFormat::Srt,
        });
        assert_eq!(
            (subtitle_comes_as_file(&plan), subtitle_in_output(&plan)),
            (false, true)
        );
    }

    #[test]
    fn a_subtitle_read_as_text_is_fetched_as_a_file() {
        let plan = converted_with(SubtitleAction::Ocr {
            from: SubtitleFormat::Pgs,
            to: SubtitleFormat::Srt,
        });
        assert_eq!(
            (subtitle_comes_as_file(&plan), subtitle_in_output(&plan)),
            (true, false)
        );
    }

    /// Four seconds of H.264 in Matroska.
    fn clip(path: &Path) {
        let pipeline = gst::parse::launch(&format!(
            "videotestsrc num-buffers=120 ! video/x-raw,width=320,height=240,framerate=30/1 \
             ! videoconvert ! x264enc ! h264parse ! matroskamux ! filesink location={}",
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

    /// Playback on the fake server, end to end. One test: GTK binds itself
    /// to the first thread that starts it, and each test has its own.
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

    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn playback_on_the_fake_server() {
        gtk::init().unwrap();
        init().unwrap();
        dropping_a_start_cleans_up_its_server_handle();
        a_playback_plays_and_tells_a_late_view_where_it_is();
        a_server_that_goes_away_fails_the_playback();
        a_playback_the_admin_stopped_says_so();
        a_session_ended_under_a_playback_says_so();
        pausing_saves_where_playback_got_to();
        a_paused_film_keeps_its_playback_alive();
        a_paused_song_keeps_its_playback_alive();
        resuming_plays_from_where_it_stopped();
        a_restored_queue_waits_paused_where_it_was();
        a_converted_playback_restarts_its_encode_on_a_seek();
        a_converted_playback_the_server_ended_plays_on();
        a_direct_play_planned_again_plays_on_converted();
        a_quality_chosen_while_playing_plans_it_again();
        a_quality_chosen_before_playing_starts_with_it();
        a_late_subtitle_restarts_a_converted_playback_with_it();
        the_intro_is_offered_and_skipped_on_asking();
        an_automatic_intro_skip_happens_once_and_undoes();
        quitting_waits_for_the_last_progress();
        a_playlist_goes_on_to_the_servers_next_entry_then_ends();
    }

    /// Movie night (two films, then an episode) from the top: each entry
    /// that ends plays the server's next, and the last one ends the
    /// playlist, once.
    fn a_playlist_goes_on_to_the_servers_next_entry_then_ends() {
        use spoolframe_linux_core::backend::PlaySettings;
        use spoolframe_linux_core::playlists::Playing;
        let dir = TestDir::new_blocking("spoolframe-playlist");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path));
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        let context = glib::MainContext::default();
        let list = context.block_on(session.playlists()).unwrap().playlists;
        let night = list.iter().find(|p| p.name == "Movie night").unwrap();
        let first = context
            .block_on(session.play_playlist(&night.id, None, PlaySettings::default()))
            .unwrap()
            .unwrap();
        let playback = Playback::start(Playlist {
            session,
            items: vec![first.item.clone()],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: Default::default(),
            display: None,
            from_playlist: Some(Playing {
                id: night.id.clone(),
                name: night.name.clone(),
                entry: first.entry,
            }),
        })
        .unwrap();
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
        assert!(
            run_until(Duration::from_secs(60), || finished.get() == 1),
            "the playlist did not end (played {:?})",
            played.borrow()
        );
        let mut seen = played.borrow().clone();
        seen.dedup();
        assert_eq!(seen.len(), 3, "played {seen:?}");
        assert!(playback.playing_from().is_none());
        playback.stop();
    }

    /// An intro from 1 s to 2.5 s of the four-second clip.
    fn intro() -> Vec<spoolframe_linux_core::backend::Marker> {
        vec![spoolframe_linux_core::backend::Marker {
            kind: spoolframe_linux_core::backend::MarkerKind::Intro,
            start_ms: 1_000,
            end_ms: 2_500,
            to_end: false,
        }]
    }

    /// The fake's first movie, with an intro, playing from the start.
    fn play_with_intro(dir: &TestDir) -> Rc<Playback> {
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path)).with_markers(intro());
        let profile = FakeClient::profile("andrey");
        start_first_movie(&client, &profile, None)
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
                == Some(Offer::SkipIntro {
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

    /// Quitting mid-film: shutdown returns only once the server has the
    /// position, which used to be cancelled with the process.
    fn quitting_waits_for_the_last_progress() {
        let dir = TestDir::new_blocking("spoolframe-quit");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
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
        let before = client.progress_reports(&profile).len();
        shutdown();
        assert!(
            client.progress_reports(&profile).len() > before,
            "shutdown returned before the last progress reached the server"
        );
    }

    /// A converted playback (P2-06) plays, a seek restarts the encode and
    /// playback goes on in it, with one encode open on the server, and
    /// stopping ends it.
    fn dropping_a_start_cleans_up_its_server_handle() {
        let dir = TestDir::new_blocking("spoolframe-abandoned-start");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path)).converting(64 * 1024);
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
        drop(playback);
        // Do not dispatch the main loop until the server has admitted the start.
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while client.open_playbacks(&profile) == 0 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(client.open_playbacks(&profile), 1);
        assert!(run_until(Duration::from_secs(5), || client
            .open_playbacks(&profile)
            == 0));
    }

    fn a_converted_playback_restarts_its_encode_on_a_seek() {
        let dir = TestDir::new_blocking("spoolframe-convert");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path)).converting(64 * 1024);
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
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
        let current = playback.now().unwrap();
        playback.send_restart(&current, current.restarts.get());
        playback.on(Event::Error(
            old_uri.clone(),
            "old epoch read failed".into(),
        ));
        assert!(matches!(&*current.stage.borrow(), Stage::Restarting { .. }));
        // The fake's encode starts again from the file's start (it has no
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
        assert!(!matches!(
            &*playback.now().unwrap().stage.borrow(),
            Stage::Failed
        ));
        assert_eq!(client.open_playbacks(&profile), 1);
        playback.stop();
        run_until(Duration::from_secs(2), || {
            client.open_playbacks(&profile) == 0
        });
        assert_eq!(
            client.open_playbacks(&profile),
            0,
            "the encode was not ended"
        );
    }

    /// A converted playback's subtitle, read as text by the server
    /// (P2-07), arrives after playback started: GstPlay cannot take it on
    /// a stream that cannot seek, so the encode restarts where it is with
    /// the subtitle, and playback goes on.
    fn a_late_subtitle_restarts_a_converted_playback_with_it() {
        let dir = TestDir::new_blocking("spoolframe-late-subtitle");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path))
            .converting(64 * 1024)
            .reading_subtitle(1);
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
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
        // The fake's new encode starts from the file's start (it has no
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

    /// A queue saved at the last run comes back prerolled at its position,
    /// paused, and plays on Play (P1-23).
    fn a_restored_queue_waits_paused_where_it_was() {
        let dir = TestDir::new_blocking("spoolframe-restore");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let item = first_movie(&session);
        let queue = Queue::new(vec![item], 0, None, Vec::new());
        let playback = Playback::restore(session, queue, Duration::from_secs(2), None).unwrap();
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
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, Some(Duration::from_secs(2)));
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
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
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
            run_until(Duration::from_secs(3), || !client
                .progress_reports(&profile)
                .is_empty()),
            "pausing reported nothing"
        );
        let (_, position) = client.progress_reports(&profile)[0].clone();
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
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
        assert!(run_until(Duration::from_secs(20), || playback.position()
            > Duration::from_secs(1)));
        let failed = || matches!(&*playback.now().unwrap().stage.borrow(), Stage::Failed);
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
        assert_eq!(client.open_playbacks(&profile), 1);
        playback.stop();
    }

    /// A quality picked on the item page is the one the playback starts
    /// with.
    fn a_quality_chosen_before_playing_starts_with_it() {
        let dir = TestDir::new_blocking("spoolframe-quality-start");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let item = first_movie(&session);
        let playback = Playback::start(Playlist {
            session,
            items: vec![item],
            index: 0,
            queued: Vec::new(),
            from: None,
            selection: None,
            tracks: None,
            album: None,
            quality: spoolframe_client_sdk::PlaybackQuality::Bitrate(2_000_000),
            display: None,
            from_playlist: None,
        })
        .unwrap();
        assert!(
            run_until(Duration::from_secs(20), || playback.plan().is_some()),
            "the playback never started"
        );
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
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
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
        let failed = || matches!(&*playback.now().unwrap().stage.borrow(), Stage::Failed);
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
        assert_eq!(client.open_playbacks(&profile), 1);
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
        let client = FakeClient::new(Some(path))
            .converting(16 * 1024)
            .expiring_at(32 * 1024);
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
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
        assert_eq!(client.started_playbacks(&profile), 2);
        assert_eq!(client.open_playbacks(&profile), 1);
        playback.stop();
    }

    /// Paused past the server's idle limit, a film keeps its playback: the
    /// paused position goes to the server every [`KEEP_ALIVE`], which
    /// counts as use. With no reads or reports, the server ends a paused
    /// playback (5 minutes then).
    fn a_paused_film_keeps_its_playback_alive() {
        let dir = TestDir::new_blocking("spoolframe-paused-film");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path)).with_handle_idle(PAUSED_PAST_IDLE / 2);
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);
        assert!(run_until(Duration::from_secs(20), || playback.position()
            > Duration::from_secs(1)));
        playback.toggle();
        assert!(run_until(Duration::from_secs(3), || !playback.is_playing()));
        let paused_at = playback.position();
        run_until(PAUSED_PAST_IDLE, || false);
        assert_eq!(
            client.live_playbacks(&profile),
            1,
            "the paused playback idled out"
        );
        let reports = client.progress_reports(&profile);
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
        let client = FakeClient::new(Some(path)).with_handle_idle(PAUSED_PAST_IDLE / 2);
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let track = first_track(&session);
        let track_id = &track.id().clone();
        let playback = Playback::start(Playlist {
            session,
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
        assert_eq!(
            client.live_playbacks(&profile),
            1,
            "the paused playback idled out"
        );
        let reports = client.progress_reports(&profile);
        assert!(
            reports.len() >= 2 && reports.iter().all(|(id, _)| id == track_id),
            "paused, the song reported only {reports:?}"
        );
        playback.stop();
    }

    /// How long the pause tests wait paused: twice their server's idle
    /// limit, and several [`KEEP_ALIVE`]s.
    const PAUSED_PAST_IDLE: Duration = Duration::from_secs(4);

    /// A movie of the fake catalog played from `clip`: it starts, plays,
    /// moves on, tells a view that attaches late where it is, and stops.
    fn a_playback_plays_and_tells_a_late_view_where_it_is() {
        let dir = TestDir::new_blocking("spoolframe-playback");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let playback = start_first_movie(&client, &profile, None);

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

    /// The server goes away before the file is read: the playback says it
    /// failed (the player shows the error) instead of waiting forever, and
    /// shutting down afterwards releases it.
    fn a_playback_the_admin_stopped_says_so() {
        let dir = TestDir::new_blocking("spoolframe-stopped");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path)).with_playbacks_stopped();
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let item = first_movie(&session);
        let playback = Playback::start(Playlist {
            session,
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
        let client = FakeClient::new(Some(path)).with_sessions_ending();
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let item = first_movie(&session);
        let playback = Playback::start(Playlist {
            session,
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

    fn a_server_that_goes_away_fails_the_playback() {
        let dir = TestDir::new_blocking("spoolframe-gone");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path));
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let item = first_movie(&session);
        client.go_away();
        let playback = Playback::start(Playlist {
            session,
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

    /// `client`'s first movie playing for `profile`, from `from`.
    fn start_first_movie(
        client: &FakeClient,
        profile: &Profile,
        from: Option<Duration>,
    ) -> Rc<Playback> {
        let session = client.session(profile).unwrap();
        let item = first_movie(&session);
        Playback::start(Playlist {
            session,
            items: vec![item],
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

    /// The first track of the fake catalog's first artist's first album.
    fn first_track(session: &Arc<dyn Session>) -> Item {
        let context = glib::MainContext::default();
        let libraries = context.block_on(session.libraries()).unwrap();
        let music = libraries
            .iter()
            .find(|l| l.kind == spoolframe_types::LibraryKind::Music)
            .unwrap();
        let (artists, _) = context.block_on(session.items(&music.id, 0, 1)).unwrap();
        let (_, albums) = context.block_on(session.item(artists[0].id())).unwrap();
        let (_, tracks) = context.block_on(session.item(albums[0].id())).unwrap();
        let track = tracks.into_iter().next().unwrap();
        assert!(matches!(track, Item::Track(_)), "{track:?}");
        track
    }

    /// The fake catalog's first movie.
    fn first_movie(session: &Arc<dyn Session>) -> Item {
        let context = glib::MainContext::default();
        let libraries = context.block_on(session.libraries()).unwrap();
        let movies = libraries
            .iter()
            .find(|l| l.kind == spoolframe_types::LibraryKind::Movies)
            .unwrap();
        let (items, _) = context.block_on(session.items(&movies.id, 0, 1)).unwrap();
        items.into_iter().next().unwrap()
    }
}
