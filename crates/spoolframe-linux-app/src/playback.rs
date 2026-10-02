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
use spoolframe_linux_core::backend::{PlaybackHandle, PlaybackPlan, Selection, Session};
use spoolframe_linux_core::reader::{self, MediaReader, Spawn};
use spoolframe_protocol::{PlannedSubtitle, SubtitleDelivery};
use spoolframe_types::{Album, Item, MediaTracks, SubtitleFormat};

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
/// episode is a list of one; an album is its tracks.
pub struct Playlist {
    pub session: Arc<dyn Session>,
    pub items: Vec<Item>,
    pub index: usize,
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
    /// Shuffle and repeat, as set now.
    Modes {
        shuffle: bool,
        repeat: Repeat,
    },
    /// The play order changed (shuffle turned on or off).
    Queue,
}

/// What happens at the end of the queue, or of the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Off,
    /// The queue again from the top.
    All,
    /// The same item again.
    One,
}

type Listener = Box<dyn Fn(&Change) -> bool>;

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
    items: Vec<Item>,
    album: Option<Album>,
    /// The play order, as positions in `items`: in order, or shuffled.
    order: RefCell<Vec<usize>>,
    /// Where playback is in `order`.
    index: Cell<usize>,
    shuffle: Cell<bool>,
    repeat: Cell<Repeat>,
    /// Where to seek once the current item's length is known.
    resume: Cell<Option<Duration>>,
    /// The tracks to ask the server for; `None` lets it choose.
    requested: Cell<Option<Selection>>,
    /// The tracks that play, to pick once the file's streams are known.
    pending: Cell<Option<Selection>>,
    track_list: RefCell<Option<MediaTracks>>,
    /// The current item's reader, reachable by its spoolframe:// URI.
    published: RefCell<Option<Published>>,
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
    listeners: RefCell<Vec<Listener>>,
}

impl Playback {
    /// Starts the playlist.
    pub fn start(playlist: Playlist) -> anyhow::Result<Rc<Self>> {
        let video = VideoSink::new()?;
        let paintable = video.gtk.property::<gtk::gdk::Paintable>("paintable");
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&video.outer);
        let play = gst_play::Play::new(Some(renderer));
        let mut config = play.config();
        config.set_position_update_interval(250);
        play.set_config(config)?;
        let adapter = gst_play::PlaySignalAdapter::new(&play);
        let count = playlist.items.len();
        let playback = Rc::new(Self {
            play,
            _adapter: adapter.clone(),
            video,
            paintable,
            session: playlist.session,
            items: playlist.items,
            album: playlist.album,
            order: RefCell::new((0..count).collect()),
            index: Cell::new(playlist.index),
            shuffle: Cell::new(false),
            repeat: Cell::new(Repeat::Off),
            resume: Cell::new(playlist.from),
            requested: Cell::new(playlist.selection),
            pending: Cell::new(None),
            track_list: RefCell::new(playlist.tracks),
            published: RefCell::new(None),
            subtitle_file: RefCell::new(None),
            subtitles_on: Rc::new(Cell::new(false)),
            playing: Cell::new(false),
            position: Cell::new(Duration::ZERO),
            length: Cell::new(Duration::ZERO),
            media_info: RefCell::new(None),
            failure: RefCell::new(None),
            stopped: Cell::new(false),
            listeners: RefCell::new(Vec::new()),
        });
        connect(&adapter, &playback);

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
                && !listener(&Change::Item(item))
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
                && listener(&Change::Modes {
                    shuffle: self.shuffle.get(),
                    repeat: self.repeat.get(),
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

    pub fn current(&self) -> Option<&Item> {
        let position = *self.order.borrow().get(self.index.get())?;
        self.items.get(position)
    }

    /// The items in play order, and where playback is among them.
    pub fn queue(&self) -> (Vec<Item>, usize) {
        let order = self.order.borrow();
        let items = order.iter().map(|&i| self.items[i].clone()).collect();
        (items, self.index.get())
    }

    /// Plays the item at `position` in the queue.
    pub fn play_at(self: &Rc<Self>, position: usize) {
        if position < self.items.len() && position != self.index.get() {
            self.report_progress();
            self.go_to(position);
        }
    }

    /// Shuffles what is still to come, or puts the queue back in order,
    /// keeping the current item playing.
    pub fn toggle_shuffle(&self) {
        let on = !self.shuffle.get();
        self.shuffle.set(on);
        let current = self.order.borrow().get(self.index.get()).copied();
        let order = if on {
            shuffled_after(&self.order.borrow(), self.index.get(), |n| {
                glib::random_int_range(0, n as i32) as usize
            })
        } else {
            (0..self.items.len()).collect()
        };
        if !on && let Some(current) = current {
            self.index.set(current);
        }
        self.order.replace(order);
        self.emit_modes();
        self.emit(&Change::Queue);
    }

    /// Off, then the whole queue, then the one item, then off again.
    pub fn cycle_repeat(&self) {
        self.repeat.set(match self.repeat.get() {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        });
        self.emit_modes();
    }

    fn emit_modes(&self) {
        self.emit(&Change::Modes {
            shuffle: self.shuffle.get(),
            repeat: self.repeat.get(),
        });
    }

    pub fn album(&self) -> Option<&Album> {
        self.album.as_ref()
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

    pub fn toggle(&self) {
        if self.playing.get() {
            self.play.pause();
        } else {
            self.play.play();
        }
    }

    pub fn seek(&self, to: Duration) {
        self.play
            .seek(gst::ClockTime::from_nseconds(to.as_nanos() as u64));
    }

    /// Seeks `seconds` from where playback is, within the item.
    pub fn jump(&self, seconds: f64) {
        let at = self.position.get().as_secs_f64() + seconds;
        let length = self.length.get().as_secs_f64();
        self.seek(Duration::from_secs_f64(at.clamp(0.0, length.max(0.0))));
    }

    pub fn set_volume(&self, volume: f64) {
        self.play.set_volume(volume.clamp(0.0, 1.0));
    }

    pub fn has_next(&self) -> bool {
        self.index.get() + 1 < self.items.len()
            || (self.repeat.get() == Repeat::All && self.items.len() > 1)
    }

    /// Where the next item is in the queue, wrapping round on repeat.
    fn next_position(&self) -> Option<usize> {
        let next = self.index.get() + 1;
        if next < self.items.len() {
            Some(next)
        } else if self.repeat.get() == Repeat::All && self.items.len() > 1 {
            Some(0)
        } else {
            None
        }
    }

    pub fn has_previous(&self) -> bool {
        self.index.get() > 0
    }

    /// The next item in the playlist, if there is one.
    pub fn next(self: &Rc<Self>) {
        if let Some(next) = self.next_position() {
            self.report_progress();
            self.go_to(next);
        }
    }

    /// The previous item, or the start of this one when it has played a
    /// few seconds, as music players do.
    pub fn previous(self: &Rc<Self>) {
        if self.position.get() > Duration::from_secs(3) || !self.has_previous() {
            self.seek(Duration::ZERO);
        } else {
            self.go_to(self.index.get() - 1);
        }
    }

    /// Reports where playback got to and stops for good.
    pub fn stop(&self) {
        if !self.stopped.replace(true) {
            self.report_progress();
            self.play.stop();
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

    fn go_to(self: &Rc<Self>, index: usize) {
        self.index.set(index);
        self.resume.set(None);
        self.requested.set(None);
        self.pending.set(None);
        self.track_list.replace(None);
        self.play.stop();
        self.start_current();
    }

    /// Starts the current item: the server plans it and opens a playback,
    /// then GstPlay reads it through the spoolframe:// source.
    fn start_current(self: &Rc<Self>) {
        let Some(item) = self.current().cloned() else {
            return;
        };
        self.subtitles_on.set(false);
        self.media_info.replace(None);
        self.failure.replace(None);
        self.position.set(Duration::ZERO);
        self.length.set(Duration::ZERO);
        self.emit(&Change::Item(&item));
        self.emit(&Change::Starting);
        let length = crate::views::Facts::of(&item).length;
        let requested = self.requested.get();
        let need_tracks = self.track_list.borrow().is_none();
        let session = self.session.clone();
        let id = item.id().clone();
        let started = relm4::spawn(async move {
            let start = session.start_playback(&id, requested).await;
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
            let (start, tracks) = match result {
                Ok((Ok(start), tracks)) => (start, tracks),
                Ok((Err(e), _)) => return playback.failed(&e.to_string()),
                Err(e) => return playback.failed(&format!("starting playback failed: {e}")),
            };
            if tracks.is_some() {
                playback.track_list.replace(tracks);
            }
            // What plays: the tracks asked for, or the server's choice.
            let selection = requested
                .or(start.chosen.map(|c| c.selection))
                .unwrap_or_default();
            playback.pending.set(Some(selection));
            let external = matches!(
                start.plan,
                PlaybackPlan::Direct {
                    subtitle: Some(PlannedSubtitle {
                        delivery: SubtitleDelivery::External,
                        ..
                    }),
                    ..
                }
            );
            let handle = start.handle.clone();
            let reader = MediaReader::with_length(
                playback.session.clone(),
                item.id().clone(),
                selection,
                start,
                spawn(),
                length,
            );
            let published = Published::new(reader);
            playback.play.set_uri(Some(&published.uri()));
            playback.published.replace(Some(published));
            if external {
                playback.load_external_subtitle(handle);
            } else {
                playback.play.set_subtitle_uri(None);
            }
            playback.play.play();
        });
    }

    /// Fetches the playback's external subtitle and hands it to GstPlay as a
    /// file: GstPlay takes subtitles only by URI.
    fn load_external_subtitle(self: &Rc<Self>, handle: PlaybackHandle) {
        let session = self.session.clone();
        let fetched = relm4::spawn(async move { session.read_subtitle(&handle).await });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(result) = fetched.await else { return };
            let Some(playback) = weak.upgrade() else {
                return;
            };
            let (format, bytes) = match result {
                Ok(subtitle) => subtitle,
                Err(e) => return tracing::warn!(error = %e, "no external subtitle"),
            };
            let path = glib::user_runtime_dir().join(format!(
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
        let length = duration.map_or(Duration::ZERO, |d| Duration::from_nanos(d.nseconds()));
        self.length.set(length);
        // Seek to where the user stopped once the length is known, which is
        // when a seek can land.
        if !length.is_zero()
            && let Some(at) = self.resume.take()
        {
            self.seek(at);
        }
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

    fn state_changed(&self, state: gst_play::PlayState) {
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
            self.apply(selection, info);
        }
        self.emit(&Change::MediaInfo(info));
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
        if self.repeat.get() == Repeat::One {
            self.seek(Duration::ZERO);
            self.play.play();
        } else if let Some(next) = self.next_position() {
            self.go_to(next);
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
    adapter.connect_error(move |_, e, _| {
        if let Some(p) = with(&g) {
            p.failed(&e.to_string())
        }
    });
}

/// `order` with everything after `keep` shuffled (Fisher–Yates);
/// `random(n)` is a number below `n`. What has played and what plays stay
/// where they are.
fn shuffled_after(
    order: &[usize],
    keep: usize,
    mut random: impl FnMut(usize) -> usize,
) -> Vec<usize> {
    let mut order = order.to_vec();
    let start = (keep + 1).min(order.len());
    let rest = &mut order[start..];
    for i in (1..rest.len()).rev() {
        rest.swap(i, random(i + 1));
    }
    order
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

    #[test]
    fn shuffling_keeps_what_has_played_and_what_plays() {
        let order: Vec<usize> = (0..6).collect();
        let shuffled = shuffled_after(&order, 2, |n| n - 1 - (n - 1) / 2);
        assert_eq!(shuffled[..3], [0, 1, 2]);
    }

    #[test]
    fn shuffling_keeps_every_item_once() {
        let order: Vec<usize> = (0..6).collect();
        let mut shuffled = shuffled_after(&order, 0, |n| n / 2);
        shuffled.sort();
        assert_eq!(shuffled, order);
    }

    #[test]
    fn shuffling_at_the_last_item_changes_nothing() {
        let order: Vec<usize> = (0..3).collect();
        assert_eq!(shuffled_after(&order, 2, |_| 0), order);
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

    /// A movie of the fake catalog played from `clip`: it starts, plays,
    /// moves on, tells a view that attaches late where it is, and stops.
    #[test]
    #[ignore = "needs a display for gtk4paintablesink"]
    fn a_playback_plays_and_tells_a_late_view_where_it_is() {
        gtk::init().unwrap();
        init().unwrap();
        let path =
            std::env::temp_dir().join(format!("spoolframe-playback-{}.mkv", std::process::id()));
        clip(&path);
        let client = FakeClient::new(Some(path));
        let profile = Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: "andrey".into(),
            display_name: "Andrey".into(),
        };
        let session = client.session(&profile).unwrap();
        let item = first_movie(&session);
        let playback = Playback::start(Playlist {
            session,
            items: vec![item],
            index: 0,
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
