//! MPRIS on the session bus (P1-24): the desktop's Now Playing (GNOME's
//! media controls, KDE's, playerctl) and the media keys reach whatever
//! plays, video as well as music.
//!
//! The bus name is registered at start-up and stays for the app's life,
//! saying Stopped while nothing plays. Like the mini bar, the server is
//! made once and the playback is attached and detached.
//!
//! zbus is built with its tokio backend here (oo7 asks for it), so every
//! poll of the server's futures happens inside Relm4's runtime; the
//! server's own handlers still run on the main loop, next to GTK.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use gtk::glib;
use mpris_server::zbus::{Result, fdo};
use mpris_server::{
    LocalPlayerInterface, LocalRootInterface, LocalServer, LoopStatus, Metadata, PlaybackRate,
    PlaybackStatus, Property, Signal, Time, TrackId, Volume,
};
use spoolframe_linux_core::backend::ArtworkSize;
use spoolframe_types::{Album, ArtworkKind, Item};

use crate::artwork::Art;
use crate::playback::{Change, Playback, Repeat};

const IDENTITY: &str = "Spoolframe";
/// The `.desktop` file's name, without the extension.
const DESKTOP_ENTRY: &str = "casa.coldsteel.Spoolframe";
/// Track ids are object paths under this.
const TRACK_PATH: &str = "/casa/coldsteel/Spoolframe/item";
/// A position further than this from where playback should be is a seek,
/// told with `Seeked`. Position updates come every 250 ms.
const SEEK_SLACK: Duration = Duration::from_millis(1500);

/// What the D-Bus side reads, shared with the main loop's listener.
struct State {
    current: RefCell<Option<Rc<Playback>>>,
    /// Bumped on each item, so each one has its own track id; and on
    /// each attach and detach, so an old listener lets go.
    track: Cell<u64>,
    generation: Cell<u64>,
    metadata: RefCell<Metadata>,
    /// The current item's cover, written to the cache, as a file:// URL.
    art_url: RefCell<Option<String>>,
    /// The current item's length as the file says; zero until it does.
    length: Cell<Duration>,
    /// The last position heard, and when, to tell a seek from playing on.
    heard: Cell<(Duration, Instant)>,
    raise: Rc<dyn Fn()>,
    quit: Rc<dyn Fn()>,
    stop: Rc<dyn Fn()>,
}

impl State {
    fn playback(&self) -> Option<Rc<Playback>> {
        self.current.borrow().clone()
    }

    fn status(&self) -> PlaybackStatus {
        match self.playback() {
            None => PlaybackStatus::Stopped,
            Some(p) if p.is_playing() => PlaybackStatus::Playing,
            Some(_) => PlaybackStatus::Paused,
        }
    }

    fn track_id(&self) -> TrackId {
        track_id(self.track.get())
    }

    fn volume(&self) -> Volume {
        self.playback()
            .map_or(1.0, |p| if p.is_muted() { 0.0 } else { p.volume() })
    }

    /// The metadata of what plays now, kept until the next change.
    fn refresh_metadata(&self) {
        let metadata = match self.playback() {
            Some(playback) => match playback.current() {
                Some(item) => metadata(
                    self.track.get(),
                    &item,
                    playback.album().as_ref(),
                    self.length.get(),
                    self.art_url.borrow().as_deref(),
                ),
                None => Metadata::new(),
            },
            None => Metadata::new(),
        };
        self.metadata.replace(metadata);
    }

    fn controls(&self) -> Vec<Property> {
        let playback = self.playback();
        let some = playback.is_some();
        vec![
            Property::CanGoNext(playback.as_ref().is_some_and(|p| p.has_next())),
            // Previous restarts the item when there is none before it.
            Property::CanGoPrevious(some),
            Property::CanPlay(some),
            Property::CanPause(some),
            Property::CanSeek(some),
        ]
    }
}

pub struct Mpris {
    state: Rc<State>,
    /// What to tell the bus, in the order it happened. One task sends it
    /// all, one at a time: a task per signal let a Paused sent at a track
    /// change land after the Playing that followed it, and desktops that
    /// only follow the signals kept showing Paused.
    outbox: tokio::sync::mpsc::UnboundedSender<Outgoing>,
}

/// A signal for the bus.
enum Outgoing {
    Changed(Vec<Property>),
    Signal(Signal),
}

impl Mpris {
    /// Registers `org.mpris.MediaPlayer2.spoolframe.instance<pid>`.
    /// `raise` shows the window and the full player; `stop` ends playback;
    /// `quit` closes the app. Without a session bus there is no MPRIS, and
    /// the app plays on.
    pub fn start(raise: Rc<dyn Fn()>, stop: Rc<dyn Fn()>, quit: Rc<dyn Fn()>) -> Rc<Self> {
        let state = Rc::new(State {
            current: RefCell::new(None),
            track: Cell::new(0),
            generation: Cell::new(0),
            metadata: RefCell::new(Metadata::new()),
            art_url: RefCell::new(None),
            length: Cell::new(Duration::ZERO),
            heard: Cell::new((Duration::ZERO, Instant::now())),
            raise,
            quit,
            stop,
        });
        let (outbox, sent) = tokio::sync::mpsc::unbounded_channel();
        let this = Rc::new(Self {
            state: state.clone(),
            outbox,
        });
        glib::spawn_future_local(async move {
            let handle = match relm4::spawn(async { tokio::runtime::Handle::current() }).await {
                Ok(handle) => handle,
                Err(e) => return tracing::warn!(error = %e, "no runtime for MPRIS"),
            };
            let suffix = format!("spoolframe.instance{}", std::process::id());
            let made = InRuntime::new(&handle, LocalServer::new(&suffix, Player(state))).await;
            let made = match made {
                Ok(made) => made,
                Err(e) => return tracing::warn!(error = %e, "MPRIS is not available"),
            };
            tracing::debug!(bus_name = %made.bus_name(), "MPRIS registered");
            glib::spawn_future_local(InRuntime::new(&handle, made.run()));
            // Whatever was told before the bus name was up goes first.
            drain(sent, |outgoing| {
                let (made, handle) = (&made, &handle);
                async move {
                    let sent = match outgoing {
                        Outgoing::Changed(properties) => {
                            InRuntime::new(handle, made.properties_changed(properties)).await
                        }
                        Outgoing::Signal(signal) => InRuntime::new(handle, made.emit(signal)).await,
                    };
                    if let Err(e) = sent {
                        tracing::debug!(error = %e, "MPRIS signal");
                    }
                }
            })
            .await;
        });
        this
    }

    /// Tells the desktop about `playback`. `art` fetches its cover.
    pub fn attach(self: &Rc<Self>, playback: &Rc<Playback>, art: Option<Rc<Art>>) {
        let generation = self.state.generation.get() + 1;
        self.state.generation.set(generation);
        self.state.current.replace(Some(playback.clone()));
        let this = Rc::downgrade(self);
        playback.subscribe(move |change| {
            let Some(this) = this.upgrade() else {
                return false;
            };
            if this.state.generation.get() != generation {
                return false;
            }
            this.update(change, art.as_ref());
            true
        });
    }

    /// Nothing plays: Stopped, with no metadata.
    pub fn detach(&self) {
        self.state.generation.set(self.state.generation.get() + 1);
        self.state.current.replace(None);
        self.state.art_url.replace(None);
        self.state.length.set(Duration::ZERO);
        self.state.refresh_metadata();
        let mut changed = vec![
            Property::PlaybackStatus(PlaybackStatus::Stopped),
            Property::Metadata(Metadata::new()),
        ];
        changed.extend(self.state.controls());
        self.changed(changed);
    }

    fn update(self: &Rc<Self>, change: &Change, art: Option<&Rc<Art>>) {
        let state = &self.state;
        match change {
            Change::Item(item) => {
                state.track.set(state.track.get() + 1);
                state.art_url.replace(None);
                state.length.set(Duration::ZERO);
                state.heard.set((Duration::ZERO, Instant::now()));
                state.refresh_metadata();
                let mut changed = vec![Property::Metadata(state.metadata.borrow().clone())];
                changed.extend(state.controls());
                self.changed(changed);
                if let Some(art) = art {
                    self.fetch_cover(art.clone(), item);
                }
            }
            Change::Playing(_) => {
                // Playing on from here, not from where it was paused.
                state.heard.set((state.heard.get().0, Instant::now()));
                self.changed(vec![Property::PlaybackStatus(state.status())]);
            }
            Change::Position { at, length } => {
                if state.length.replace(*length) != *length {
                    state.refresh_metadata();
                    self.changed(vec![Property::Metadata(state.metadata.borrow().clone())]);
                }
                let (before, when) = state.heard.replace((*at, Instant::now()));
                let playing = state.playback().is_some_and(|p| p.is_playing());
                let expected = if playing {
                    before + when.elapsed()
                } else {
                    before
                };
                if at.abs_diff(expected) > SEEK_SLACK {
                    self.emit(Signal::Seeked {
                        position: time(*at),
                    });
                }
            }
            Change::Volume(_) | Change::Muted(_) => {
                self.changed(vec![Property::Volume(state.volume())]);
            }
            Change::Modes { shuffle, repeat } => self.changed(vec![
                Property::Shuffle(*shuffle),
                Property::LoopStatus(loop_status(*repeat)),
            ]),
            Change::Queue => self.changed(state.controls()),
            Change::Starting
            | Change::MediaInfo(_)
            | Change::Buffering(_)
            | Change::Failed(_)
            | Change::Stopped
            | Change::SignedOut
            | Change::Skipped(_)
            | Change::TrackFailed(_)
            | Change::Offer(_)
            | Change::IntroSkipped => {}
        }
    }

    /// Writes the item's cover to the cache and points the metadata at it.
    fn fetch_cover(self: &Rc<Self>, art: Rc<Art>, item: &Item) {
        let playback = self.state.playback();
        let (artwork, kinds) = match item {
            Item::Track(_) => (
                playback
                    .as_ref()
                    .and_then(|p| p.album())
                    .map(|a| a.artwork.clone())
                    .unwrap_or_default(),
                [ArtworkKind::Poster, ArtworkKind::Thumb],
            ),
            Item::Episode(e) => (e.artwork.clone(), [ArtworkKind::Thumb, ArtworkKind::Poster]),
            Item::Movie(m) => (m.artwork.clone(), [ArtworkKind::Poster, ArtworkKind::Thumb]),
            _ => return,
        };
        let track = self.state.track.get();
        let this = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some((id, bytes)) = art.bytes(&artwork, &kinds, ArtworkSize::Medium).await else {
                return;
            };
            let url = match write_cover(&id.to_string(), &bytes) {
                Ok(url) => url,
                Err(e) => return tracing::warn!(error = %e, "cannot write the MPRIS cover"),
            };
            let Some(this) = this.upgrade() else {
                return;
            };
            if this.state.track.get() != track {
                return;
            }
            this.state.art_url.replace(Some(url));
            this.state.refresh_metadata();
            this.changed(vec![Property::Metadata(
                this.state.metadata.borrow().clone(),
            )]);
        });
    }

    /// Emits PropertiesChanged, after everything told before it. Without
    /// a bus there is no one to tell, and it is dropped.
    fn changed(&self, properties: Vec<Property>) {
        let _ = self.outbox.send(Outgoing::Changed(properties));
    }

    fn emit(&self, signal: Signal) {
        let _ = self.outbox.send(Outgoing::Signal(signal));
    }
}

/// Sends what `queue` holds, each send finished before the next begins,
/// until every sender is gone.
async fn drain<T, F, Fut>(mut queue: tokio::sync::mpsc::UnboundedReceiver<T>, mut send: F)
where
    F: FnMut(T) -> Fut,
    Fut: Future<Output = ()>,
{
    while let Some(item) = queue.recv().await {
        send(item).await;
    }
}

/// The metadata the desktop shows for `item`, the `track`th item played.
/// The title is never empty; an episode's show stands in as its artist and
/// "S02E05" as its album, which is how the desktop's widgets lay it out
/// best.
fn metadata(
    track: u64,
    item: &Item,
    album: Option<&Album>,
    length: Duration,
    art_url: Option<&str>,
) -> Metadata {
    let facts = crate::views::Facts::of(item);
    let mut metadata = Metadata::new();
    metadata.set_trackid(Some(track_id(track)));
    let title = if facts.title.trim().is_empty() {
        "Untitled".to_owned()
    } else {
        facts.title.clone()
    };
    metadata.set_title(Some(title));
    let (artist, album) = match item {
        Item::Track(_) => (
            crate::views::track_artist(item, album),
            crate::views::track_album(item, album),
        ),
        Item::Episode(e) => (
            e.show_title.clone(),
            Some(format!("S{:02}E{:02}", e.season, e.episode)),
        ),
        _ => (None, None),
    };
    metadata.set_artist(artist.map(|a| [a]));
    metadata.set_album(album);
    let length = Some(length)
        .filter(|l| !l.is_zero())
        .or(facts.length)
        .unwrap_or_default();
    metadata.set_length(length_of(length));
    metadata.set_art_url(art_url.map(str::to_owned));
    metadata
}

fn track_id(track: u64) -> TrackId {
    TrackId::try_from(format!("{TRACK_PATH}/{track}")).expect("a valid object path")
}

fn time(duration: Duration) -> Time {
    Time::from_micros(duration.as_micros() as i64)
}

fn length_of(length: Duration) -> Option<Time> {
    (!length.is_zero()).then(|| time(length))
}

fn loop_status(repeat: Repeat) -> LoopStatus {
    match repeat {
        Repeat::Off => LoopStatus::None,
        Repeat::All => LoopStatus::Playlist,
        Repeat::One => LoopStatus::Track,
    }
}

/// Writes a cover under `mpris/` in the app's cache directory, named
/// by its artwork id, and returns its file:// URL. Desktops read the file
/// themselves; they cannot reach the server.
fn write_cover(id: &str, bytes: &[u8]) -> std::io::Result<String> {
    let dir = spoolframe_linux_core::paths::cache_dir()
        .ok_or_else(|| std::io::Error::other("no cache directory"))?
        .join("mpris");
    std::fs::create_dir_all(&dir)?;
    let name = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, id)
        .map(|sum| sum.to_string())
        .unwrap_or_else(|| "cover".to_owned());
    let path = dir.join(name);
    if !path.exists() {
        std::fs::write(&path, bytes)?;
    }
    glib::filename_to_uri(&path, None)
        .map(|uri| uri.to_string())
        .map_err(std::io::Error::other)
}

/// Polls `inner` inside a tokio runtime, which zbus's tokio backend needs
/// to spawn its tasks and reach its reactor.
struct InRuntime<F> {
    runtime: tokio::runtime::Handle,
    inner: Pin<Box<F>>,
}

impl<F: Future> InRuntime<F> {
    fn new(runtime: &tokio::runtime::Handle, inner: F) -> Self {
        Self {
            runtime: runtime.clone(),
            inner: Box::pin(inner),
        }
    }
}

impl<F: Future> Future for InRuntime<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let _entered = self.runtime.enter();
        self.inner.as_mut().poll(cx)
    }
}

/// The server's handlers: what the desktop asks of the player.
struct Player(Rc<State>);

impl Player {
    fn with(&self, run: impl FnOnce(&Rc<Playback>)) -> fdo::Result<()> {
        if let Some(playback) = self.0.playback() {
            run(&playback);
        }
        Ok(())
    }
}

impl LocalRootInterface for Player {
    async fn raise(&self) -> fdo::Result<()> {
        (self.0.raise)();
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        (self.0.quit)();
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _fullscreen: bool) -> Result<()> {
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok(IDENTITY.to_owned())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok(DESKTOP_ENTRY.to_owned())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

impl LocalPlayerInterface for Player {
    async fn next(&self) -> fdo::Result<()> {
        self.with(|p| p.next())
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.with(|p| p.previous())
    }

    async fn pause(&self) -> fdo::Result<()> {
        self.with(|p| {
            if p.is_playing() {
                p.toggle();
            }
        })
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        self.with(|p| p.toggle())
    }

    async fn stop(&self) -> fdo::Result<()> {
        (self.0.stop)();
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        self.with(|p| {
            if !p.is_playing() {
                p.toggle();
            }
        })
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        self.with(|p| p.jump(offset.as_micros() as f64 / 1e6))
    }

    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        // Only for the item playing now, and within it (the spec says to
        // ignore anything else).
        if track_id != self.0.track_id() || position.as_micros() < 0 {
            return Ok(());
        }
        let at = Duration::from_micros(position.as_micros() as u64);
        let length = self.0.metadata.borrow().length();
        if length.is_some_and(|l| position > l) {
            return Ok(());
        }
        self.with(|p| p.seek(at))
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported(
            "Spoolframe plays from its server only".to_owned(),
        ))
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(self.0.status())
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(self
            .0
            .playback()
            .map_or(LoopStatus::None, |p| loop_status(p.repeat())))
    }

    async fn set_loop_status(&self, loop_status: LoopStatus) -> Result<()> {
        self.with(|p| {
            // Repeat cycles Off → All → One; at most two steps to any.
            for _ in 0..3 {
                if self::loop_status(p.repeat()) == loop_status {
                    break;
                }
                p.cycle_repeat();
            }
        })?;
        Ok(())
    }

    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn set_rate(&self, _rate: PlaybackRate) -> Result<()> {
        Ok(())
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.0.playback().is_some_and(|p| p.is_shuffled()))
    }

    async fn set_shuffle(&self, shuffle: bool) -> Result<()> {
        self.with(|p| {
            if p.is_shuffled() != shuffle {
                p.toggle_shuffle();
            }
        })?;
        Ok(())
    }

    async fn metadata(&self) -> fdo::Result<Metadata> {
        Ok(self.0.metadata.borrow().clone())
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(self.0.volume())
    }

    async fn set_volume(&self, volume: Volume) -> Result<()> {
        self.with(|p| {
            p.set_muted(false);
            p.set_volume(volume);
        })?;
        Ok(())
    }

    async fn position(&self) -> fdo::Result<Time> {
        Ok(self.0.playback().map_or(Time::ZERO, |p| time(p.position())))
    }

    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(self.0.playback().is_some_and(|p| p.has_next()))
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(self.0.playback().is_some())
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(self.0.playback().is_some())
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(self.0.playback().is_some())
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(self.0.playback().is_some())
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_types::{Episode, MediaId, MediaKind, Movie, Namespace, PlaybackState, Track};

    use super::*;

    /// Ready after `0` more polls: a send the bus is slow to take.
    struct Slow(u32);

    impl Future for Slow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 == 0 {
                return Poll::Ready(());
            }
            self.0 -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    #[test]
    fn a_slow_paused_still_reaches_the_bus_before_the_playing_after_it() {
        let (outbox, queue) = tokio::sync::mpsc::unbounded_channel();
        outbox.send(PlaybackStatus::Paused).unwrap();
        outbox.send(PlaybackStatus::Playing).unwrap();
        drop(outbox);
        let bus = Rc::new(RefCell::new(Vec::new()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(drain(queue, |status| {
            let bus = bus.clone();
            async move {
                // Paused takes longer to send than Playing.
                Slow(if status == PlaybackStatus::Paused {
                    10
                } else {
                    0
                })
                .await;
                bus.borrow_mut().push(status);
            }
        }));
        assert_eq!(bus.borrow().last(), Some(&PlaybackStatus::Playing));
    }

    fn id(kind: MediaKind, key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, kind, key).unwrap()
    }

    fn song() -> (Item, Album) {
        let track = Item::Track(Track {
            id: id(MediaKind::Track, "7"),
            album_id: id(MediaKind::Album, "5"),
            artist_id: id(MediaKind::Artist, "4"),
            title: "Teardrop".into(),
            track: Some(3),
            disc: None,
            duration: Some(Duration::from_secs(330)),
            playback: PlaybackState::default(),
            codec: None,
            bit_depth: None,
            sample_rate: None,
            album_track_count: None,
            artist_name: None,
            album_title: None,
            artwork: Vec::new(),
        });
        let album = Album {
            id: id(MediaKind::Album, "5"),
            artist_id: id(MediaKind::Artist, "4"),
            artist_name: Some("Massive Attack".into()),
            title: "Mezzanine".into(),
            year: Some(1998),
            artwork: Vec::new(),
            track_count: None,
        };
        (track, album)
    }

    fn episode() -> Item {
        Item::Episode(Episode {
            id: id(MediaKind::Episode, "3"),
            show_id: id(MediaKind::Show, "1"),
            show_title: Some("Severance".into()),
            season_id: id(MediaKind::Season, "2"),
            season: 2,
            episode: 5,
            title: "Trojan's Horse".into(),
            summary: None,
            duration: Some(Duration::from_secs(2900)),
            artwork: Vec::new(),
            playback: PlaybackState::default(),
            content_rating: None,
        })
    }

    fn movie(title: &str) -> Item {
        Item::Movie(Movie {
            id: id(MediaKind::Movie, "9"),
            title: title.into(),
            year: Some(1982),
            summary: None,
            duration: Some(Duration::from_secs(7020)),
            artwork: Vec::new(),
            playback: PlaybackState::default(),
            content_rating: None,
        })
    }

    #[test]
    fn a_song_has_its_artist_and_album() {
        let (track, album) = song();
        let m = metadata(1, &track, Some(&album), Duration::ZERO, None);
        assert_eq!(
            (m.title(), m.artist(), m.album()),
            (
                Some("Teardrop"),
                Some(vec!["Massive Attack".to_owned()]),
                Some("Mezzanine")
            )
        );
    }

    #[test]
    fn an_episode_has_its_show_as_artist_and_its_number_as_album() {
        let m = metadata(1, &episode(), None, Duration::ZERO, None);
        assert_eq!(
            (m.title(), m.artist(), m.album()),
            (
                Some("Trojan's Horse"),
                Some(vec!["Severance".to_owned()]),
                Some("S02E05")
            )
        );
    }

    #[test]
    fn a_movie_has_only_its_title() {
        let m = metadata(1, &movie("Blade Runner"), None, Duration::ZERO, None);
        assert_eq!(
            (m.title(), m.artist(), m.album()),
            (Some("Blade Runner"), None, None)
        );
    }

    #[test]
    fn a_title_is_never_empty() {
        let m = metadata(1, &movie("  "), None, Duration::ZERO, None);
        assert_eq!(m.title(), Some("Untitled"));
    }

    #[test]
    fn the_length_heard_from_the_file_wins_over_the_catalog() {
        let m = metadata(
            1,
            &movie("Blade Runner"),
            None,
            Duration::from_secs(7000),
            None,
        );
        assert_eq!(m.length(), Some(Time::from_secs(7000)));
    }

    #[test]
    fn the_catalog_length_stands_in_until_the_file_says() {
        let m = metadata(1, &movie("Blade Runner"), None, Duration::ZERO, None);
        assert_eq!(m.length(), Some(Time::from_secs(7020)));
    }

    #[test]
    fn each_item_played_has_its_own_track_id() {
        let first = metadata(1, &episode(), None, Duration::ZERO, None);
        let second = metadata(2, &episode(), None, Duration::ZERO, None);
        assert_eq!(
            (
                first.trackid().unwrap().as_str(),
                second.trackid().unwrap().as_str()
            ),
            (
                "/casa/coldsteel/Spoolframe/item/1",
                "/casa/coldsteel/Spoolframe/item/2"
            )
        );
    }

    #[test]
    fn the_cover_is_a_file_url() {
        let m = metadata(
            1,
            &movie("Blade Runner"),
            None,
            Duration::ZERO,
            Some("file:///tmp/cover"),
        );
        assert_eq!(m.art_url().as_deref(), Some("file:///tmp/cover"));
    }
}
