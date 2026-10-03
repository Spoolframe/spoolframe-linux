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
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use gst::prelude::*;
use gtk::glib;
use gtk::glib::thread_guard::ThreadGuard;
use spoolframe_linux_core::backend::{
    BackendError, PlaybackHandle, PlaybackPlan, Selection, Session,
};
use spoolframe_linux_core::progressive::ProgressiveReader;
use spoolframe_linux_core::reader::{self, ByteSource, MediaReader, Spawn};
use spoolframe_protocol::{PlannedSubtitle, SubtitleAction, SubtitleDelivery};
use spoolframe_types::Delivery;
use spoolframe_types::{Album, Item, MediaTracks, SubtitleFormat};

pub use crate::queue::Repeat;
use crate::queue::{Queue, Queued};
use crate::source::Published;
use crate::tracks;

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
    autoplay: bool,
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
pub fn shutdown() {
    let live = LIVE.with(|live| std::mem::take(&mut *live.borrow_mut()));
    for playback in live.iter().filter_map(Weak::upgrade) {
        playback.stop();
    }
}

/// How long to wait before asking again for a subtitle the server is
/// still reading as text, in seconds.
const SUBTITLE_RETRY: u32 = 5;

/// How often progress goes to the server while playing (P1-14 throttles
/// on its side too).
const PROGRESS_EVERY: u32 = 10;

pub struct Playback {
    play: gst_play::Play,
    /// Kept for its signal handlers.
    _adapter: gst_play::PlaySignalAdapter,
    video: VideoSink,
    paintable: gtk::gdk::Paintable,
    session: Arc<dyn Session>,
    /// What plays, and what plays next.
    queue: RefCell<Queue>,
    /// Where to seek once the current item's length is known.
    resume: Cell<Option<Duration>>,
    /// The tracks to ask the server for; `None` lets it choose.
    requested: Cell<Option<Selection>>,
    /// The tracks that play, to pick once the file's streams are known.
    pending: Cell<Option<Selection>>,
    track_list: RefCell<Option<MediaTracks>>,
    /// The current item's reader, reachable by its spoolframe:// URI.
    published: RefCell<Option<Published>>,
    /// How the server plays the current item, for the diagnostics.
    plan: RefCell<Option<PlaybackPlan>>,
    /// A converted playback's handle (P2-06): a seek restarts its encode,
    /// and moving on ends it.
    converted: RefCell<Option<PlaybackHandle>>,
    /// Where a converted playback is asked to seek next; seeks while
    /// scrubbing are gathered into one restart.
    seek_to: Cell<Option<Duration>>,
    /// Bumped on each restart, so a slow one does not replace a newer.
    restarts: Cell<u64>,
    /// The catalog's length of the current item: a converted output's own
    /// is unknown while it is encoded.
    catalog_length: Cell<Option<Duration>>,
    /// An external subtitle, written where GstPlay can open it.
    subtitle_file: RefCell<Option<std::path::PathBuf>>,
    /// Whether subtitles show; see [`tracks::menu`].
    subtitles_on: Rc<Cell<bool>>,
    playing: Cell<bool>,
    position: Cell<Duration>,
    length: Cell<Duration>,
    media_info: RefCell<Option<gst_play::PlayMediaInfo>>,
    failure: RefCell<Option<String>>,
    stopped: Cell<bool>,
    /// When the current item was asked for, for the timings in the log.
    asked: Cell<std::time::Instant>,
    /// Whether the current item's first picture has been logged.
    pictured: Rc<Cell<bool>>,
    /// Whether the current item plays once ready. False for a queue
    /// restored at start-up, which waits, paused, for Play.
    autoplay: Cell<bool>,
    listeners: RefCell<Vec<Listener>>,
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
            autoplay: true,
        };
        Self::build(playlist.session, queue, start)
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
            autoplay: false,
        };
        Self::build(session, queue, start)
    }

    /// The queue as it stands, to save.
    pub fn snapshot(&self) -> Queue {
        self.queue.borrow().clone()
    }

    fn build(session: Arc<dyn Session>, queue: Queue, start: Start) -> anyhow::Result<Rc<Self>> {
        let video = VideoSink::new()?;
        let paintable = video.gtk.property::<gtk::gdk::Paintable>("paintable");
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&video.outer);
        let play = gst_play::Play::new(Some(renderer));
        let mut config = play.config();
        config.set_position_update_interval(250);
        play.set_config(config)?;
        let adapter = gst_play::PlaySignalAdapter::new(&play);
        let playback = Rc::new(Self {
            play,
            _adapter: adapter.clone(),
            video,
            paintable,
            session,
            queue: RefCell::new(queue),
            resume: Cell::new(start.from),
            requested: Cell::new(start.selection),
            pending: Cell::new(None),
            track_list: RefCell::new(start.tracks),
            published: RefCell::new(None),
            plan: RefCell::new(None),
            converted: RefCell::new(None),
            seek_to: Cell::new(None),
            restarts: Cell::new(0),
            catalog_length: Cell::new(None),
            subtitle_file: RefCell::new(None),
            subtitles_on: Rc::new(Cell::new(false)),
            playing: Cell::new(false),
            position: Cell::new(Duration::ZERO),
            length: Cell::new(Duration::ZERO),
            media_info: RefCell::new(None),
            failure: RefCell::new(None),
            stopped: Cell::new(false),
            asked: Cell::new(std::time::Instant::now()),
            pictured: Rc::new(Cell::new(false)),
            autoplay: Cell::new(start.autoplay),
            listeners: RefCell::new(Vec::new()),
        });
        connect(&adapter, &playback);
        {
            let weak = Rc::downgrade(&playback);
            gtk::prelude::PaintableExt::connect_invalidate_contents(
                &playback.paintable,
                move |_| {
                    if let Some(playback) = weak.upgrade()
                        && !playback.pictured.replace(true)
                    {
                        tracing::debug!(ms = playback.since_asked(), "first picture");
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
        playback.start_current();
        Ok(playback)
    }

    /// Calls `listener` with the current state now and with every change
    /// after, until it returns false or the playback is dropped.
    pub fn subscribe(&self, listener: impl Fn(&Change) -> bool + 'static) {
        let replay = || -> bool {
            if let Some(item) = self.current()
                && !listener(&Change::Item(&item))
            {
                return false;
            }
            if let Some(info) = self.media_info.borrow().as_ref()
                && !listener(&Change::MediaInfo(info))
            {
                return false;
            }
            if let Some(error) = self.failure.borrow().as_deref() {
                return listener(&Change::Failed(error));
            }
            listener(&Change::Playing(self.playing.get()))
                && listener(&Change::Volume(self.play.volume()))
                && listener(&Change::Muted(self.play.is_muted()))
                && listener(&Change::Modes {
                    shuffle: self.is_shuffled(),
                    repeat: self.repeat(),
                })
                && listener(&Change::Position {
                    at: self.position.get(),
                    length: self.length.get(),
                })
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
        !matches!(self.current(), Some(Item::Track(_)))
    }

    /// The video frames, for any number of pictures.
    pub fn paintable(&self) -> &gtk::gdk::Paintable {
        &self.paintable
    }

    pub fn play(&self) -> &gst_play::Play {
        &self.play
    }

    pub fn subtitles_on(&self) -> &Rc<Cell<bool>> {
        &self.subtitles_on
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
        if self.playing.get() {
            self.play.pause();
        } else {
            self.play.play();
        }
    }

    /// Seeks within the item. A converted playback cannot seek in its
    /// output: the server restarts the encode there instead (see
    /// [`Playback::restart_at`]).
    pub fn seek(self: &Rc<Self>, to: Duration) {
        if self.converted.borrow().is_some() {
            return self.restart_at(to);
        }
        self.seek_direct(to);
    }

    /// A seek in the file itself: direct play.
    fn seek_direct(&self, to: Duration) {
        self.play
            .seek(gst::ClockTime::from_nseconds(to.as_nanos() as u64));
    }

    /// How the server plays the current item, once it has said.
    pub fn plan(&self) -> Option<PlaybackPlan> {
        self.plan.borrow().clone()
    }

    /// Asks the server to restart the converted encode at `to` and plays
    /// the new encode from its start. matroskademux does not pass a time
    /// seek up to a source it cannot seek in bytes (checked with a spike),
    /// so the pipeline is restarted rather than sought. The output keeps
    /// the film's own time (the server's -copyts), so positions need no
    /// offset. Seeks while scrubbing gather into one restart.
    fn restart_at(self: &Rc<Self>, to: Duration) {
        // The seek bar shows the target straight away.
        self.position.set(to);
        self.emit(&Change::Position {
            at: to,
            length: self.length.get(),
        });
        if self.seek_to.replace(Some(to)).is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            if let Some(playback) = weak.upgrade()
                && let Some(to) = playback.seek_to.take()
            {
                playback.restart_now(to);
            }
        });
    }

    fn restart_now(self: &Rc<Self>, to: Duration) {
        let Some(handle) = self.converted.borrow().clone() else {
            return;
        };
        let restart = self.restarts.get() + 1;
        self.restarts.set(restart);
        let session = self.session.clone();
        let sought = {
            let handle = handle.clone();
            relm4::spawn(async move { session.seek_playback(&handle, to).await })
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = sought.await;
            let Some(playback) = weak.upgrade() else {
                return;
            };
            if playback.stopped.get()
                || playback.restarts.get() != restart
                || playback.converted.borrow().as_ref() != Some(&handle)
            {
                return;
            }
            let epoch = match result {
                Ok(Ok(epoch)) => epoch,
                Ok(Err(e)) => return playback.failed(&e.to_string()),
                Err(e) => return playback.failed(&format!("seeking failed: {e}")),
            };
            let playing = playback.playing.get() || playback.autoplay.get();
            let reader = ProgressiveReader::new(playback.session.clone(), handle, epoch, spawn());
            playback.play.stop();
            let published = Published::new(reader);
            playback.play.set_uri(Some(&published.uri()));
            playback.published.replace(Some(published));
            playback.resubtitle();
            playback.media_info.replace(None);
            // The new encode has the same streams; they are picked again.
            playback.pending.set(Some(Selection::default()));
            if playing {
                playback.play.play();
            } else {
                playback.play.pause();
            }
        });
    }

    /// Hands GstPlay the external subtitle again after a new URI. Only
    /// when there is one: any subtitle change makes GstPlay restart and
    /// seek, which a converted stream cannot do.
    fn resubtitle(&self) {
        if let Some(path) = self.subtitle_file.borrow().as_ref() {
            self.play.set_subtitle_uri(Some(&gtk::prelude::FileExt::uri(
                &gtk::gio::File::for_path(path),
            )));
            self.play.set_subtitle_track_enabled(true);
        }
    }

    /// Ends the converted encode of the item that played, on the server:
    /// it would otherwise run on until its handle idles out.
    fn end_conversion(&self) {
        if let Some(handle) = self.converted.take() {
            let session = self.session.clone();
            relm4::spawn(async move {
                if let Err(e) = session.stop_playback(&handle).await {
                    tracing::debug!(error = %e, "stopping the conversion");
                }
            });
        }
    }

    /// Seeks `seconds` from where playback is, within the item.
    pub fn jump(self: &Rc<Self>, seconds: f64) {
        let at = self.position.get().as_secs_f64() + seconds;
        let length = self.length.get().as_secs_f64();
        self.seek(Duration::from_secs_f64(at.clamp(0.0, length.max(0.0))));
    }

    pub fn set_volume(&self, volume: f64) {
        self.play.set_volume(volume.clamp(0.0, 1.0));
    }

    pub fn is_muted(&self) -> bool {
        self.play.is_muted()
    }

    pub fn set_muted(&self, muted: bool) {
        self.play.set_mute(muted);
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
        if !self.stopped.replace(true) {
            self.report_progress();
            self.end_conversion();
            self.play.stop();
            // Down to NULL, not just stopped: the sink lets go of its GL
            // context, and the pipeline of its threads, now rather than
            // whenever the last reference goes (see `shutdown`).
            let _ = self.play.pipeline().set_state(gst::State::Null);
            let _ = self.video.gtk.set_state(gst::State::Null);
        }
    }

    /// For the diagnostics panel.
    pub fn pipeline(&self) -> gst::Element {
        self.play.pipeline()
    }

    pub fn video_sink(&self) -> &VideoSink {
        &self.video
    }

    pub fn reader_status(&self) -> Option<reader::Status> {
        self.published
            .borrow()
            .as_ref()
            .map(|p| p.reader().status())
    }

    /// Moves the queue with `step` and starts what it then points at; false,
    /// with nothing changed, when `step` does not move.
    fn step(self: &Rc<Self>, step: impl FnOnce(&mut Queue) -> bool) -> bool {
        if !step(&mut self.queue.borrow_mut()) {
            return false;
        }
        self.autoplay.set(true);
        self.end_conversion();
        self.resume.set(None);
        self.requested.set(None);
        self.pending.set(None);
        self.track_list.replace(None);
        self.play.stop();
        // The last item's external subtitle goes now, while its pipeline
        // is stopped (see `start_current`).
        if let Some(path) = self.subtitle_file.take() {
            self.play.set_subtitle_uri(None);
            let _ = std::fs::remove_file(path);
        }
        self.start_current();
        self.emit(&Change::Queue);
        true
    }

    /// Starts the current item: the server plans it and opens a playback,
    /// then GstPlay reads it through the spoolframe:// source.
    fn start_current(self: &Rc<Self>) {
        let Some(item) = self.current() else {
            return;
        };
        self.asked.set(std::time::Instant::now());
        self.pictured.set(false);
        tracing::debug!(item = %item.id(), resume = ?self.resume.get(), "starting");
        self.subtitles_on.set(false);
        self.media_info.replace(None);
        self.failure.replace(None);
        self.position.set(Duration::ZERO);
        self.length.set(Duration::ZERO);
        self.emit(&Change::Item(&item));
        // The new item starts at nothing: views showed the last one's
        // position and length until GStreamer reported the new ones.
        self.emit(&Change::Position {
            at: Duration::ZERO,
            length: Duration::ZERO,
        });
        self.emit(&Change::Starting);
        let length = crate::views::Facts::of(&item).length;
        self.catalog_length.set(length);
        self.plan.replace(None);
        let requested = self.requested.get();
        // A converted playback's encode begins here; direct play ignores
        // it and seeks itself.
        let from = self.resume.get();
        let need_tracks = self.track_list.borrow().is_none();
        let session = self.session.clone();
        let id = item.id().clone();
        let started = relm4::spawn(async move {
            let start = session.start_playback(&id, requested, from).await;
            // The track list maps the chosen tracks onto the file's streams.
            let tracks = match need_tracks {
                true => session.tracks(&id).await.ok(),
                false => None,
            };
            (start, tracks)
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = started.await;
            let Some(playback) = weak.upgrade() else {
                return;
            };
            if playback.stopped.get() {
                return;
            }
            tracing::debug!(ms = playback.since_asked(), "the server answered");
            let (start, tracks) = match result {
                Ok((Ok(start), tracks)) => (start, tracks),
                Ok((Err(e), _)) => return playback.could_not_start(&item, &e.to_string()),
                Err(e) => {
                    return playback
                        .could_not_start(&item, &format!("starting playback failed: {e}"));
                }
            };
            if tracks.is_some() {
                playback.track_list.replace(tracks);
            }
            // What plays: the tracks asked for, or the server's choice.
            let selection = requested
                .or(start.chosen.map(|c| c.selection))
                .unwrap_or_default();
            playback.pending.set(Some(selection));
            playback.plan.replace(Some(start.plan.clone()));
            let external = matches!(
                start.plan,
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
            );
            let handle = start.handle.clone();
            let size = start.size;
            let reader: Box<dyn ByteSource> = match &start.plan {
                PlaybackPlan::Converted {
                    delivery: Delivery::Progressive,
                    ..
                } => {
                    // The encode began at the resume point: nothing to
                    // seek, and its output is already in the film's time.
                    playback.resume.set(None);
                    playback.converted.replace(Some(handle.clone()));
                    Box::new(ProgressiveReader::new(
                        playback.session.clone(),
                        handle.clone(),
                        start.epoch,
                        spawn(),
                    ))
                }
                PlaybackPlan::Converted { delivery, .. } => {
                    return playback.failed(&format!(
                        "the server planned {delivery:?} delivery, which this player does not take"
                    ));
                }
                _ => {
                    let reader = MediaReader::with_length(
                        playback.session.clone(),
                        item.id().clone(),
                        selection,
                        start,
                        spawn(),
                        length,
                    );
                    // Resuming: start reading ahead from about where the
                    // resume point is in the file while the demuxer reads
                    // the header, so the seek finds its data on the way
                    // or here.
                    if let (Some(at), Some(length)) = (playback.resume.get(), length) {
                        reader.expect(reader::guess_offset(size, length, at));
                    }
                    Box::new(reader)
                }
            };
            let published = Published::boxed(reader);
            playback.play.set_uri(Some(&published.uri()));
            playback.published.replace(Some(published));
            // The last item's subtitle was let go in `step`. Not cleared
            // here: GstPlay restarts the pipeline and seeks back on any
            // subtitle change, and a converted stream cannot seek, so
            // even clearing an unset one stalled it.
            if external {
                playback.load_external_subtitle(handle);
            }
            // Resuming: preroll paused, seek there once the length is
            // known, then play. Playing at once read and decoded the start
            // of the file first, only to jump away from it.
            // A restored queue prerolls and waits.
            if playback.resume.get().is_some() || !playback.autoplay.get() {
                playback.play.pause();
            } else {
                playback.play.play();
            }
        });
    }

    /// Seeks to where the user stopped and plays, once the length is
    /// known (a seek before that does not land).
    fn resume_if_ready(&self) {
        if !self.length.get().is_zero()
            && let Some(at) = self.resume.take()
        {
            tracing::debug!(?at, ms = self.since_asked(), "resume seek");
            self.seek_direct(at);
            if self.autoplay.get() {
                self.play.play();
            } else {
                // Paused, GstPlay reports no position; views would show
                // the start until Play.
                self.position.set(at);
                self.emit(&Change::Position {
                    at,
                    length: self.length.get(),
                });
            }
        }
    }

    /// Fetches the playback's external subtitle and hands it to GstPlay as a
    /// file: GstPlay takes subtitles only by URI.
    fn load_external_subtitle(self: &Rc<Self>, handle: PlaybackHandle) {
        let session = self.session.clone();
        let fetched = {
            let handle = handle.clone();
            relm4::spawn(async move { session.read_subtitle(&handle).await })
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(result) = fetched.await else { return };
            let Some(playback) = weak.upgrade() else {
                return;
            };
            let (format, bytes) = match result {
                Ok(subtitle) => subtitle,
                // Still being read as text (P2-07): play on, ask again.
                Err(BackendError::SubtitleNotReady(done)) => {
                    tracing::debug!(done, "the subtitle is still being read");
                    let weak = Rc::downgrade(&playback);
                    glib::timeout_add_seconds_local_once(SUBTITLE_RETRY, move || {
                        if let Some(playback) = weak.upgrade()
                            && !playback.stopped.get()
                            && playback.converted.borrow().as_ref() == Some(&handle)
                        {
                            playback.load_external_subtitle(handle);
                        }
                    });
                    return;
                }
                Err(e) => return tracing::warn!(error = %e, "no external subtitle"),
            };
            let path = spoolframe_linux_core::paths::runtime_dir().join(format!(
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
            playback
                .play
                .set_subtitle_uri(Some(&gtk::prelude::FileExt::uri(
                    &gtk::gio::File::for_path(&path),
                )));
            playback.play.set_subtitle_track_enabled(true);
            playback.subtitles_on.set(true);
            playback.subtitle_file.replace(Some(path));
        });
    }

    /// Where playback is, for movies and episodes; music has no watch
    /// state (P1-14).
    fn report_progress(&self) {
        let Some(item) = self.current() else { return };
        if !matches!(item, Item::Movie(_) | Item::Episode(_)) {
            return;
        }
        let Some(position) = self.play.position() else {
            return;
        };
        let position = Duration::from_nanos(position.nseconds());
        let (session, id) = (self.session.clone(), item.id().clone());
        relm4::spawn(async move {
            if let Err(e) = session.update_progress(&id, position).await {
                tracing::warn!(error = %e, "progress not saved");
            }
        });
    }

    fn buffering(&self, percent: i32) {
        if !self.stopped.get() {
            self.emit(&Change::Buffering(percent));
        }
    }

    fn duration_changed(&self, duration: Option<gst::ClockTime>) {
        tracing::debug!(?duration, ms = self.since_asked(), "length known");
        // A converted output being encoded has no duration of its own yet:
        // the catalog's length stands in.
        let length = duration
            .map(|d| Duration::from_nanos(d.nseconds()))
            .filter(|d| !d.is_zero())
            .or(self
                .catalog_length
                .get()
                .filter(|_| self.converted.borrow().is_some()))
            .unwrap_or_default();
        self.length.set(length);
        self.resume_if_ready();
        self.emit(&Change::Position {
            at: self.position.get(),
            length,
        });
    }

    fn position_updated(&self, at: Option<gst::ClockTime>) {
        let Some(at) = at else { return };
        let at = Duration::from_nanos(at.nseconds());
        self.position.set(at);
        self.emit(&Change::Position {
            at,
            length: self.length.get(),
        });
    }

    /// Milliseconds since the current item was asked for.
    fn since_asked(&self) -> u64 {
        self.asked.get().elapsed().as_millis() as u64
    }

    fn state_changed(&self, state: gst_play::PlayState) {
        tracing::debug!(?state, ms = self.since_asked(), "player state");
        // Prerolled for a resume: seek and play, or, with no length to
        // seek in, play from the start rather than stay paused.
        if state == gst_play::PlayState::Paused && self.resume.get().is_some() {
            self.resume_if_ready();
            if self.resume.take().is_some() && self.autoplay.get() {
                self.play.play();
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
        self.media_info.replace(Some(info.clone()));
        let has_streams = !info.audio_streams().is_empty() || !info.subtitle_streams().is_empty();
        if has_streams && let Some(selection) = self.pending.take() {
            if self.converted.borrow().is_some() {
                self.apply_converted(info);
            } else {
                self.apply(selection, info);
            }
        }
        self.emit(&Change::MediaInfo(info));
    }

    /// A converted output holds only the tracks picked: one audio, and the
    /// subtitle when the server copied or converted it into the output.
    /// One read as text (OCR) comes as a file; one burned in is in the
    /// picture.
    fn apply_converted(&self, info: &gst_play::PlayMediaInfo) {
        if self.subtitle_file.borrow().is_some() {
            return;
        }
        let in_output = matches!(
            self.plan.borrow().as_ref(),
            Some(PlaybackPlan::Converted {
                subtitle: Some(spoolframe_protocol::ConvertedSubtitle {
                    action: SubtitleAction::Copy { .. } | SubtitleAction::Convert { .. },
                    ..
                }),
                ..
            })
        );
        match tracks::select_subtitle(&self.play, info, in_output.then_some(0)) {
            Ok(()) => self.subtitles_on.set(in_output),
            Err(e) => tracing::warn!(error = %e, "cannot pick the converted subtitle"),
        }
    }

    /// Picks the tracks that play. The selection counts positions in the
    /// item's track list; the file's streams are in the same order,
    /// external subtitles aside.
    fn apply(&self, selection: Selection, info: &gst_play::PlayMediaInfo) {
        if let Some(audio) = selection.audio
            && let Err(e) = tracks::select_audio(&self.play, info, audio as usize)
        {
            tracing::warn!(error = %e, audio, "cannot pick the audio track");
        }
        let track_list = self.track_list.borrow();
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
        if embedded.is_none() && self.subtitle_file.borrow().is_some() {
            return;
        }
        match tracks::select_subtitle(&self.play, info, embedded) {
            Ok(()) => self.subtitles_on.set(embedded.is_some()),
            Err(e) => tracing::warn!(error = %e, ?embedded, "cannot pick the subtitle"),
        }
    }

    /// The item played to its end: report it, then play the next one.
    fn ended(self: &Rc<Self>) {
        if let Some(length) = self.play.duration() {
            let length = Duration::from_nanos(length.nseconds());
            if let Some(item) = self.current()
                && matches!(item, Item::Movie(_) | Item::Episode(_))
            {
                let (session, id) = (self.session.clone(), item.id().clone());
                relm4::spawn(async move {
                    if let Err(e) = session.update_progress(&id, length).await {
                        tracing::warn!(error = %e, "progress not saved");
                    }
                });
            }
        }
        if self.repeat() == Repeat::One {
            self.seek(Duration::ZERO);
            self.play.play();
        } else {
            self.advance();
        }
    }

    /// The server would not start `item`. A queued item is passed over,
    /// with a word, for what comes after it (P1-23); anything else fails
    /// the playback.
    fn could_not_start(self: &Rc<Self>, item: &Item, error: &str) {
        if !self.queue.borrow().current_is_queued() {
            return self.failed(error);
        }
        tracing::warn!(error, "skipping a queued item");
        let title = crate::views::Facts::of(item).title;
        self.emit(&Change::Skipped(&format!("Skipped {title}: {error}")));
        if !self.advance() {
            self.failed(error);
        }
    }

    fn failed(&self, error: &str) {
        tracing::warn!(error, "playback failed");
        self.failure.replace(Some(error.to_owned()));
        self.emit(&Change::Failed(error));
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop();
        if let Some(path) = self.subtitle_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn connect(adapter: &gst_play::PlaySignalAdapter, playback: &Rc<Playback>) {
    // GstPlay's adapter wants thread-safe callbacks, but it delivers them
    // on the main loop it was made on. The guard satisfies the bound and
    // panics if that ever changes. It holds the playback weakly: the
    // playback owns the adapter.
    let guard = || ThreadGuard::new(Rc::downgrade(playback));
    let with = |g: &ThreadGuard<Weak<Playback>>| g.get_ref().upgrade();
    let g = guard();
    adapter.connect_duration_changed(move |_, d| {
        if let Some(p) = with(&g) {
            p.duration_changed(d)
        }
    });
    let g = guard();
    adapter.connect_position_updated(move |_, at| {
        if let Some(p) = with(&g) {
            p.position_updated(at)
        }
    });
    let g = guard();
    adapter.connect_state_changed(move |_, state| {
        if let Some(p) = with(&g) {
            p.state_changed(state)
        }
    });
    let g = guard();
    adapter.connect_media_info_updated(move |_, info| {
        if let Some(p) = with(&g) {
            p.media_info_updated(info)
        }
    });
    let g = guard();
    adapter.connect_buffering(move |_, percent| {
        if let Some(p) = with(&g) {
            p.buffering(percent)
        }
    });
    let g = guard();
    adapter.connect_end_of_stream(move |_| {
        if let Some(p) = with(&g) {
            p.ended()
        }
    });
    let g = guard();
    adapter.connect_volume_changed(move |_, volume| {
        if let Some(p) = with(&g) {
            p.emit(&Change::Volume(volume))
        }
    });
    let g = guard();
    adapter.connect_mute_changed(move |_, muted| {
        if let Some(p) = with(&g) {
            p.emit(&Change::Muted(muted))
        }
    });
    let g = guard();
    adapter.connect_error(move |_, e, _| {
        if let Some(p) = with(&g) {
            p.failed(&e.to_string())
        }
    });
}

/// Fetches run on Relm4's tokio runtime, where the SDK's networking lives.
fn spawn() -> Spawn {
    Arc::new(|future| {
        relm4::spawn(future);
    })
}

/// gtk4paintablesink itself, and what GstPlay renders into (the same, or a
/// glsinkbin around it); the diagnostics read both.
#[derive(Clone)]
pub struct VideoSink {
    pub gtk: gst::Element,
    pub outer: gst::Element,
}

impl VideoSink {
    /// gtk4paintablesink, in a glsinkbin when it has a GL context, as
    /// gst-plugin-gtk4's own example does. Frames then stay on the GPU:
    /// dmabufs from the decoder go to GTK directly or through GL, and a
    /// 10-bit frame is converted on the GPU. Without it every frame is
    /// copied out and converted on the CPU, which 4K outruns.
    fn new() -> anyhow::Result<Self> {
        let gtk = gst::ElementFactory::make("gtk4paintablesink").build()?;
        // The sink finds its GL context on going to Ready.
        gtk.set_state(gst::State::Ready)?;
        let paintable = gtk.property::<gtk::gdk::Paintable>("paintable");
        let gl = paintable
            .property::<Option<gtk::gdk::GLContext>>("gl-context")
            .is_some();
        let outer = match gl {
            true => gst::ElementFactory::make("glsinkbin")
                .property("sink", &gtk)
                .build()?,
            false => gtk.clone(),
        };
        Ok(Self { gtk, outer })
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::test_dir::TestDir;
    use std::path::Path;

    use spoolframe_linux_core::backend::{Client, Profile, ServerKey};
    use spoolframe_linux_core::fake::FakeClient;

    use super::*;

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
    }

    /// A converted playback (P2-06) plays, a seek restarts the encode and
    /// playback goes on in it, with one encode open on the server, and
    /// stopping ends it.
    fn a_converted_playback_restarts_its_encode_on_a_seek() {
        let dir = TestDir::new_blocking("spoolframe-convert");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path)).converting(64 * 1024);
        let profile = Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: "andrey".into(),
            display_name: "Andrey".into(),
            locked: false,
        };
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
        })
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
        assert!(
            run_until(Duration::from_secs(20), || played.get()
                > Duration::from_secs(1)),
            "the converted playback never got past a second"
        );
        assert!(matches!(
            playback.plan(),
            Some(PlaybackPlan::Converted { .. })
        ));
        let restarts = playback.restarts.get();
        playback.seek(Duration::from_secs(3));
        // The fake's encode starts again from the file's start (it has no
        // -copyts), so the position drops back; playback must go on.
        assert!(
            run_until(Duration::from_secs(20), || playback.restarts.get()
                > restarts
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

    /// A queue saved at the last run comes back prerolled at its position,
    /// paused, and plays on Play (P1-23).
    fn a_restored_queue_waits_paused_where_it_was() {
        let dir = TestDir::new_blocking("spoolframe-restore");
        let path = dir.join("clip.mkv");
        clip(&path);
        let client = FakeClient::new(Some(path));
        let profile = Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: "andrey".into(),
            display_name: "Andrey".into(),
            locked: false,
        };
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
        let profile = Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: "andrey".into(),
            display_name: "Andrey".into(),
            locked: false,
        };
        let session = client.session(&profile).unwrap();
        let item = first_movie(&session);
        let playback = Playback::start(Playlist {
            session,
            items: vec![item],
            index: 0,
            queued: Vec::new(),
            from: Some(Duration::from_secs(2)),
            selection: None,
            tracks: None,
            album: None,
        })
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
        let profile = Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: "andrey".into(),
            display_name: "Andrey".into(),
            locked: false,
        };
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
        })
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
        let profile = Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: "andrey".into(),
            display_name: "Andrey".into(),
            locked: false,
        };
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
        })
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
        let profile = Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: "andrey".into(),
            display_name: "Andrey".into(),
            locked: false,
        };
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
