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
use spoolframe_linux_core::backend::{
    BackendError, Marker, PlaybackHandle, PlaybackPlan, PlaybackStart, Selection, Session,
};
use spoolframe_linux_core::markers;
pub use spoolframe_linux_core::markers::Offer;
use spoolframe_linux_core::next_episode::next_episode;
use spoolframe_linux_core::progressive::ProgressiveReader;
use spoolframe_linux_core::reader::{self, ByteSource, MediaReader, Spawn};
use spoolframe_protocol::{ConvertedVideo, PlannedSubtitle, SubtitleAction, SubtitleDelivery};
use spoolframe_types::Delivery;
use spoolframe_types::{Album, Item, MediaTracks, SubtitleFormat};

use crate::pipeline::Pipeline;
pub use crate::pipeline::VideoSink;
pub use crate::queue::Repeat;
use crate::queue::{Queue, Queued};

/// Registers the linked-in gtk4paintablesink and the spoolframe:// source.
/// Call once, after GTK is up.
pub fn init() -> anyhow::Result<()> {
    gst::init()?;
    gstgtk4::plugin_register_static()?;
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
}

/// How the first item starts: see [`Playlist`].
struct Start {
    from: Option<Duration>,
    selection: Option<Selection>,
    tracks: Option<MediaTracks>,
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
    /// The button over the video from the item's markers (P2-10), or
    /// none.
    Offer(Option<Offer>),
    /// The intro was skipped by itself; [`Playback::undo_intro_skip`]
    /// goes back.
    IntroSkipped,
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

/// How long to wait before asking again for a subtitle the server is
/// still reading as text; less in tests, whose clip is four seconds.
const SUBTITLE_RETRY: Duration = if cfg!(test) {
    Duration::from_millis(1_000)
} else {
    Duration::from_secs(5)
};

/// How many progress ticks between touches of a converted playback's
/// handle: the server ends one not read for 5 minutes, and a paused film,
/// or a slow one with a full buffer, reads nothing for longer.
const KEEP_ALIVE_TICKS: u32 = 6;

/// How often progress goes to the server while playing (P1-14 throttles
/// on its side too).
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
    playing: Cell<bool>,
    position: Cell<Duration>,
    length: Cell<Duration>,
    stopped: Cell<bool>,
    listeners: RefCell<Vec<Listener>>,
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
    handle: RefCell<Option<PlaybackHandle>>,
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
        conv: Conversion,
        to: Duration,
        sent: bool,
    },
    Failed,
}

/// How the file reaches GstPlay.
#[derive(Clone)]
enum Mode {
    Direct,
    Converted(Conversion),
}

/// A converted playback's encode (P2-06): a seek restarts it, and moving
/// on ends it.
#[derive(Clone)]
struct Conversion {
    handle: PlaybackHandle,
    epoch: u32,
    bitrate: Option<u64>,
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
            handle: RefCell::new(None),
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

    /// The encode, while the item plays converted.
    fn conversion(&self) -> Option<Conversion> {
        match &*self.stage.borrow() {
            Stage::Open {
                mode: Mode::Converted(conv),
                ..
            }
            | Stage::Restarting { conv, .. } => Some(conv.clone()),
            _ => None,
        }
    }

    fn is_converted(&self) -> bool {
        self.conversion().is_some()
    }

    /// Open and not waiting to resume: a seek now lands.
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
        };
        Self::build(playlist.session, queue, start, true)
    }

    /// Brings back a queue saved at the last run (P1-23): the current item
    /// prerolled at `at`, paused, waiting for Play.
    pub fn restore(
        session: Arc<dyn Session>,
        queue: Queue,
        at: Duration,
    ) -> anyhow::Result<Rc<Self>> {
        let start = Start {
            from: (!at.is_zero()).then_some(at),
            selection: None,
            tracks: None,
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
        let playback = Rc::new(Self {
            pipeline: Pipeline::new()?,
            session,
            queue: RefCell::new(queue),
            current: RefCell::new(None),
            held: Cell::new(false),
            autoplay: Cell::new(autoplay),
            skip_intros: Cell::new(false),
            subtitles_on: Rc::new(Cell::new(false)),
            playing: Cell::new(false),
            position: Cell::new(Duration::ZERO),
            length: Cell::new(Duration::ZERO),
            stopped: Cell::new(false),
            listeners: RefCell::new(Vec::new()),
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
        let ticks = Cell::new(0u32);
        glib::timeout_add_seconds_local(PROGRESS_EVERY, move || {
            let Some(playback) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if playback.playing.get() {
                playback.report_progress();
            }
            ticks.set(ticks.get() + 1);
            if ticks.get().is_multiple_of(KEEP_ALIVE_TICKS) {
                playback.keep_conversion_alive();
            }
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

    /// The audio and subtitle menu for `info`.
    pub fn track_menu(&self, info: &gst_play::PlayMediaInfo) -> gtk::Popover {
        self.pipeline.track_menu(info, &self.subtitles_on)
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
            let conv = match &mut *stage {
                Stage::Restarting {
                    to: unsent,
                    sent: false,
                    ..
                } => {
                    *unsent = to;
                    None
                }
                Stage::Restarting { conv, .. }
                | Stage::Open {
                    mode: Mode::Converted(conv),
                    ..
                } => Some(conv.clone()),
                _ => return,
            };
            if let Some(conv) = conv {
                *stage = Stage::Restarting {
                    conv,
                    to,
                    sent: false,
                };
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
        let (handle, to) = match &mut *current.stage.borrow_mut() {
            Stage::Restarting { conv, to, sent } if !*sent => {
                *sent = true;
                (conv.handle.clone(), *to)
            }
            _ => return,
        };
        let session = self.session.clone();
        let sought = relm4::spawn(async move { session.seek_playback(&handle, to).await });
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
        answer: Answer<u32>,
    ) {
        let Some(current) = current.upgrade().filter(|c| self.is_current(c)) else {
            return;
        };
        let conv = match &*current.stage.borrow() {
            Stage::Restarting {
                conv, sent: true, ..
            } if current.restarts.get() == restart => conv.clone(),
            _ => return,
        };
        let epoch = match answer {
            Ok(Ok(epoch)) => epoch,
            // The handle idled out (the app was away): a new playback of
            // the same item, whose encode begins where the user sought.
            Ok(Err(BackendError::PlaybackExpired)) => return self.ask(&current, Some(to)),
            Ok(Err(e)) => return self.failed(&e.to_string()),
            Err(e) => return self.failed(&format!("seeking failed: {e}")),
        };
        let conv = Conversion { epoch, ..conv };
        let reader = ProgressiveReader::new(
            self.session.clone(),
            conv.handle.clone(),
            epoch,
            spawn(),
            conv.bitrate,
        );
        let subtitle = current.subtitle_file.borrow().clone();
        self.pipeline.load(Box::new(reader), subtitle.as_deref());
        current.stage.replace(Stage::Open {
            mode: Mode::Converted(conv),
            resume: None,
        });
        current.media_info.replace(None);
        // The new encode has the same streams; they are picked again.
        current.pending.set(Some(Selection::default()));
        if self.autoplay.get() && !self.held.get() {
            self.pipeline.play_now();
        } else {
            self.pipeline.pause();
        }
    }

    /// Reads a byte of the converted output, which the server counts as
    /// use: its handle stays alive while the reader has no reason to read.
    fn keep_conversion_alive(&self) {
        let Some(conv) = self.now().and_then(|c| c.conversion()) else {
            return;
        };
        let session = self.session.clone();
        relm4::spawn(async move {
            if let Err(e) = session.read_chunk(&conv.handle, conv.epoch, 0, 1).await {
                tracing::debug!(error = %e, "keeping the conversion alive");
            }
        });
    }

    /// Ends `current`'s encode on the server: it would otherwise run on
    /// until its handle idles out.
    fn end_conversion(&self, current: &Current) -> Option<tokio::task::JoinHandle<()>> {
        let conv = current.conversion()?;
        let session = self.session.clone();
        Some(relm4::spawn(async move {
            if let Err(e) = session.stop_playback(&conv.handle).await {
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
        let queue = self.queue.borrow();
        if self.is_video() {
            queue.context_has_next()
        } else {
            queue.has_next()
        }
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
        if self.is_video() {
            self.step(Queue::next_in_context)
        } else {
            self.step(Queue::next)
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
    fn stop_waitable(&self) -> Vec<tokio::task::JoinHandle<()>> {
        if self.stopped.replace(true) {
            return Vec::new();
        }
        // The item goes: answers still on their way find nothing.
        let current = self.current.take();
        // Down to NULL now, not whenever the last reference goes (see
        // `shutdown`), and off the main thread (see `crate::pipeline`).
        [
            self.report_progress(),
            current.and_then(|c| self.end_conversion(&c)),
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
        if !step(&mut self.queue.borrow_mut()) {
            return false;
        }
        self.autoplay.set(true);
        self.held.set(false);
        if let Some(last) = self.current.take() {
            self.end_conversion(&last);
        }
        // The next load takes the last item's subtitle away, while
        // stopped (see `Pipeline::load`).
        self.pipeline.stop();
        self.begin(None);
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
        let started = relm4::spawn(async move {
            // The track list maps the chosen tracks onto the file's
            // streams; it does not wait on the start.
            let tracks = async {
                match need_tracks {
                    true => session.tracks(&id).await.ok(),
                    false => None,
                }
            };
            tokio::join!(session.start_playback(&id, requested, from), tracks)
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
            // Left before the server answered (stopped, or moved on): a
            // converted start's encode is ended, or it would run until
            // its handle idles out.
            if let Ok((Ok(start), _)) = &answer
                && matches!(start.plan, PlaybackPlan::Converted { .. })
            {
                let (session, handle) = (self.session.clone(), start.handle.clone());
                relm4::spawn(async move {
                    let _ = session.stop_playback(&handle).await;
                });
            }
            return;
        };
        tracing::debug!(ms = current.since_asked(), "the server answered");
        let item = &current.item;
        let (mut start, tracks) = match answer {
            Ok((Ok(start), tracks)) => (start, tracks),
            Ok((Err(e), _)) => return self.could_not_start(item, &e.to_string()),
            Err(e) => {
                return self.could_not_start(item, &format!("starting playback failed: {e}"));
            }
        };
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
        current.handle.replace(Some(handle.clone()));
        let (mode, resume, reader): (Mode, Option<Duration>, Box<dyn ByteSource>) =
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
                    let conv = Conversion {
                        handle: handle.clone(),
                        epoch: start.epoch,
                        bitrate,
                    };
                    let reader = ProgressiveReader::new(
                        self.session.clone(),
                        handle.clone(),
                        start.epoch,
                        spawn(),
                        bitrate,
                    );
                    // The encode began at the resume point: nothing to seek,
                    // and its output is already in the film's time.
                    (Mode::Converted(conv), None, Box::new(reader))
                }
                PlaybackPlan::Converted { delivery, .. } => {
                    return self.failed(&format!(
                        "the server planned {delivery:?} delivery, which this player does not take"
                    ));
                }
                // Not played as direct play: an empty "file" made the reader's
                // rate 0 (see MediaReader's bitrate).
                PlaybackPlan::Unsupported { reasons } => {
                    return self.failed(&format!("cannot play: {}", reasons.join("; ")));
                }
                PlaybackPlan::Unknown => {
                    return self
                        .failed("the server planned playback this version cannot take; update it");
                }
                PlaybackPlan::Direct { .. } => {
                    let (size, length) = (start.size, current.catalog_length);
                    let reader = MediaReader::new(
                        self.session.clone(),
                        item.id().clone(),
                        selection,
                        start,
                        spawn(),
                        length,
                    );
                    // Resuming: start reading ahead from about where the
                    // resume point is in the file while the demuxer reads the
                    // header, so the seek finds its data on the way or here.
                    if let (Some(at), Some(length)) = (from, length) {
                        reader.expect(reader::guess_offset(size, length, at));
                    }
                    (Mode::Direct, from, Box::new(reader))
                }
            };
        // A subtitle already here (the same item started again after its
        // handle idled out) goes with the file.
        let subtitle = current.subtitle_file.borrow().clone();
        self.pipeline.load(reader, subtitle.as_deref());
        // A converted encode begins at `from`: that is where playback is
        // until its positions arrive (a restart for a late subtitle goes
        // from here, not from 0).
        if let (Mode::Converted(_), Some(at)) = (&mode, from) {
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
        let dir = spoolframe_linux_core::paths::runtime_dir();
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

    /// Where playback is, for movies and episodes; music has no watch
    /// state (P1-14).
    fn report_progress(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.send_progress(self.pipeline.position()?)
    }

    /// Tells the server the current item is at `position`; nothing for
    /// music.
    fn send_progress(&self, position: Duration) -> Option<tokio::task::JoinHandle<()>> {
        let item = self.current()?;
        if !matches!(item, Item::Movie(_) | Item::Episode(_)) {
            return None;
        }
        let (session, id) = (self.session.clone(), item.id().clone());
        Some(relm4::spawn(async move {
            if let Err(e) = session.update_progress(&id, position).await {
                tracing::warn!(error = %e, "progress not saved");
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
            Err(e) => tracing::warn!(error = %e, "cannot pick the converted subtitle"),
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
            Err(e) => tracing::warn!(error = %e, ?embedded, "cannot pick the subtitle"),
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
    Result<(Result<PlaybackStart, BackendError>, Option<MediaTracks>), tokio::task::JoinError>;

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
    Error(String),
    Started {
        current: Weak<Current>,
        from: Option<Duration>,
        answer: Started,
    },
    Sought {
        current: Weak<Current>,
        restart: u64,
        to: Duration,
        answer: Answer<u32>,
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
            Event::Error(e) => self.failed(&e),
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
    let g = guard();
    adapter.connect_error(move |_, e, _| on(&g, Event::Error(e.to_string())));
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
    #[ignore = "needs a display for gtk4paintablesink"]
    fn playback_on_the_fake_server() {
        gtk::init().unwrap();
        init().unwrap();
        a_playback_plays_and_tells_a_late_view_where_it_is();
        a_server_that_goes_away_fails_the_playback();
        pausing_saves_where_playback_got_to();
        resuming_plays_from_where_it_stopped();
        a_restored_queue_waits_paused_where_it_was();
        a_converted_playback_restarts_its_encode_on_a_seek();
        a_late_subtitle_restarts_a_converted_playback_with_it();
        the_intro_is_offered_and_skipped_on_asking();
        an_automatic_intro_skip_happens_once_and_undoes();
        quitting_waits_for_the_last_progress();
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
        let restarts = playback.restarts();
        playback.seek(Duration::from_secs(3));
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
        let playback = Playback::restore(session, queue, Duration::from_secs(2)).unwrap();
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
        })
        .unwrap()
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
