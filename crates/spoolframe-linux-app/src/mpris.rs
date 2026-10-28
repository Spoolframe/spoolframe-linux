//! MPRIS on the session bus (P1-24): the desktop's Now Playing (GNOME's
//! media controls, KDE's, playerctl) and the media keys reach whatever
//! plays, video as well as music.
//!
//! The bus name is registered at start-up and stays for the app's life,
//! saying Stopped while nothing plays. Like the mini bar, the server is
//! made once and the playback is attached and detached.
//!
//! Served with GIO's own D-Bus (the connection GTK already has), from the
//! interfaces' introspection XML. Its handlers run on the main loop, next
//! to GTK, and signals leave in the order they are emitted: GIO queues
//! each one on the connection as it is called.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib};
use spoolframe_linux_core::backend::ArtworkSize;
use spoolframe_types::{Album, ArtworkKind, Item};

use crate::artwork::Art;
use crate::playback::{Change, Playback, Repeat};

const IDENTITY: &str = "Spoolframe";
/// The `.desktop` file's name, without the extension.
const DESKTOP_ENTRY: &str = "app.spoolframe.relm4";
/// Track ids are object paths under this.
const TRACK_PATH: &str = "/casa/coldsteel/Spoolframe/item";
/// A position further than this from where playback should be is a seek,
/// told with `Seeked`. Position updates come every 250 ms.
const SEEK_SLACK: Duration = Duration::from_millis(1500);

/// Where MPRIS players live, and their two interfaces.
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The two interfaces as the MPRIS 2.2 specification has them, without
/// the optional TrackList and Playlists. Position changes all the time
/// and is never in PropertiesChanged; the desktop reads it, and hears
/// `Seeked` when it jumps.
const INTROSPECTION: &str = r#"
<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="Fullscreen" type="b" access="readwrite"/>
    <property name="CanSetFullscreen" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="DesktopEntry" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek">
      <arg name="Offset" type="x" direction="in"/>
    </method>
    <method name="SetPosition">
      <arg name="TrackId" type="o" direction="in"/>
      <arg name="Position" type="x" direction="in"/>
    </method>
    <method name="OpenUri">
      <arg name="Uri" type="s" direction="in"/>
    </method>
    <signal name="Seeked">
      <arg name="Position" type="x"/>
    </signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="LoopStatus" type="s" access="readwrite"/>
    <property name="Rate" type="d" access="readwrite"/>
    <property name="Shuffle" type="b" access="readwrite"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read">
      <annotation name="org.freedesktop.DBus.Property.EmitsChangedSignal" value="false"/>
    </property>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read">
      <annotation name="org.freedesktop.DBus.Property.EmitsChangedSignal" value="false"/>
    </property>
  </interface>
</node>
"#;

/// MPRIS's time: microseconds.
type Time = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopStatus {
    None,
    Track,
    Playlist,
}

impl LoopStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Track => "Track",
            Self::Playlist => "Playlist",
        }
    }

    fn parse(status: &str) -> Option<Self> {
        match status {
            "None" => Some(Self::None),
            "Track" => Some(Self::Track),
            "Playlist" => Some(Self::Playlist),
            _ => None,
        }
    }
}

/// What the desktop shows of the item playing: the `Metadata` property,
/// an `a{sv}` with only the keys that have a value.
#[derive(Debug, Clone, Default, PartialEq)]
struct Metadata {
    /// An object path, from [`track_id`].
    trackid: Option<String>,
    title: Option<String>,
    artist: Option<Vec<String>>,
    album: Option<String>,
    length: Option<Time>,
    art_url: Option<String>,
}

impl Metadata {
    fn to_variant(&self) -> glib::Variant {
        let trackid = self.trackid.as_deref().map(|trackid| {
            glib::variant::ObjectPath::try_from(trackid)
                .expect("track ids are made valid object paths")
                .to_variant()
        });
        asv([
            ("mpris:trackid", trackid),
            ("mpris:length", self.length.map(|l| l.to_variant())),
            (
                "mpris:artUrl",
                self.art_url.as_ref().map(|u| u.to_variant()),
            ),
            ("xesam:title", self.title.as_ref().map(|t| t.to_variant())),
            ("xesam:artist", self.artist.as_ref().map(|a| a.to_variant())),
            ("xesam:album", self.album.as_ref().map(|a| a.to_variant())),
        ]
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?))))
    }
}

/// An `a{sv}` of `entries`, in their order.
fn asv<'a>(entries: impl IntoIterator<Item = (&'a str, glib::Variant)>) -> glib::Variant {
    let entry = glib::VariantTy::new("{sv}").expect("a valid type");
    glib::Variant::array_from_iter_with_type(
        entry,
        entries.into_iter().map(|(key, value)| {
            glib::Variant::from_dict_entry(&key.to_variant(), &glib::Variant::from_variant(&value))
        }),
    )
}

/// A Player property whose change the desktop is told of.
#[derive(Debug, Clone, PartialEq)]
enum Property {
    PlaybackStatus(PlaybackStatus),
    LoopStatus(LoopStatus),
    Shuffle(bool),
    Metadata(Metadata),
    Volume(f64),
    CanGoNext(bool),
    CanGoPrevious(bool),
    CanPlay(bool),
    CanPause(bool),
    CanSeek(bool),
}

impl Property {
    fn name(&self) -> &'static str {
        match self {
            Self::PlaybackStatus(_) => "PlaybackStatus",
            Self::LoopStatus(_) => "LoopStatus",
            Self::Shuffle(_) => "Shuffle",
            Self::Metadata(_) => "Metadata",
            Self::Volume(_) => "Volume",
            Self::CanGoNext(_) => "CanGoNext",
            Self::CanGoPrevious(_) => "CanGoPrevious",
            Self::CanPlay(_) => "CanPlay",
            Self::CanPause(_) => "CanPause",
            Self::CanSeek(_) => "CanSeek",
        }
    }

    fn value(&self) -> glib::Variant {
        match self {
            Self::PlaybackStatus(status) => status.as_str().to_variant(),
            Self::LoopStatus(status) => status.as_str().to_variant(),
            Self::Metadata(metadata) => metadata.to_variant(),
            Self::Volume(volume) => volume.to_variant(),
            Self::Shuffle(on)
            | Self::CanGoNext(on)
            | Self::CanGoPrevious(on)
            | Self::CanPlay(on)
            | Self::CanPause(on)
            | Self::CanSeek(on) => on.to_variant(),
        }
    }
}

/// PropertiesChanged's body, `(sa{sv}as)`: the Player's `properties`
/// with their new values, none only invalidated.
fn properties_changed(properties: &[Property]) -> glib::Variant {
    glib::Variant::tuple_from_iter([
        PLAYER.to_variant(),
        asv(properties.iter().map(|p| (p.name(), p.value()))),
        Vec::<String>::new().to_variant(),
    ])
}

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
    /// The session bus, once the objects are on it; until then there is
    /// no one to tell of a change.
    bus: RefCell<Option<gio::DBusConnection>>,
    raise: Rc<dyn Fn()>,
    quit: Rc<dyn Fn()>,
    stop: Rc<dyn Fn()>,
}

impl State {
    fn new(raise: Rc<dyn Fn()>, stop: Rc<dyn Fn()>, quit: Rc<dyn Fn()>) -> Rc<Self> {
        Rc::new(Self {
            current: RefCell::new(None),
            track: Cell::new(0),
            generation: Cell::new(0),
            metadata: RefCell::new(Metadata::default()),
            art_url: RefCell::new(None),
            length: Cell::new(Duration::ZERO),
            heard: Cell::new((Duration::ZERO, Instant::now())),
            bus: RefCell::new(None),
            raise,
            quit,
            stop,
        })
    }

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

    fn track_id(&self) -> String {
        track_id(self.track.get())
    }

    fn volume(&self) -> f64 {
        self.playback()
            .map_or(1.0, |p| if p.is_muted() { 0.0 } else { p.volume() })
    }

    fn loop_status(&self) -> LoopStatus {
        self.playback()
            .map_or(LoopStatus::None, |p| loop_status(p.repeat()))
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
                None => Metadata::default(),
            },
            None => Metadata::default(),
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

    /// Runs `run` on the playback, if any.
    fn with(&self, run: impl FnOnce(&Rc<Playback>)) {
        if let Some(playback) = self.playback() {
            run(&playback);
        }
    }

    /// A property of the root interface, or None for one it lacks.
    fn root_property(&self, name: &str) -> Option<glib::Variant> {
        Some(match name {
            "CanQuit" | "CanRaise" => true.to_variant(),
            "Fullscreen" | "CanSetFullscreen" | "HasTrackList" => false.to_variant(),
            "Identity" => IDENTITY.to_variant(),
            "DesktopEntry" => DESKTOP_ENTRY.to_variant(),
            "SupportedUriSchemes" | "SupportedMimeTypes" => Vec::<String>::new().to_variant(),
            _ => return None,
        })
    }

    /// A property of the Player interface, or None for one it lacks.
    fn player_property(&self, name: &str) -> Option<glib::Variant> {
        let some = self.playback().is_some();
        Some(match name {
            "PlaybackStatus" => self.status().as_str().to_variant(),
            "LoopStatus" => self.loop_status().as_str().to_variant(),
            "Rate" | "MinimumRate" | "MaximumRate" => 1.0f64.to_variant(),
            "Shuffle" => self
                .playback()
                .is_some_and(|p| p.is_shuffled())
                .to_variant(),
            "Metadata" => self.metadata.borrow().to_variant(),
            "Volume" => self.volume().to_variant(),
            "Position" => self
                .playback()
                .map_or(0, |p| time(p.position()))
                .to_variant(),
            "CanGoNext" => self.playback().is_some_and(|p| p.has_next()).to_variant(),
            "CanGoPrevious" | "CanPlay" | "CanPause" | "CanSeek" => some.to_variant(),
            "CanControl" => true.to_variant(),
            _ => return None,
        })
    }

    /// Sets a writable property; false for a value that is not one.
    fn set_property(&self, interface: &str, name: &str, value: &glib::Variant) -> bool {
        match (interface, name) {
            // Neither can change: no fullscreen, no other rate.
            (ROOT, "Fullscreen") | (PLAYER, "Rate") => true,
            (PLAYER, "LoopStatus") => {
                let Some(wanted) = value.str().and_then(LoopStatus::parse) else {
                    return false;
                };
                self.with(|p| {
                    // Repeat cycles Off → All → One; at most two steps to any.
                    for _ in 0..3 {
                        if loop_status(p.repeat()) == wanted {
                            break;
                        }
                        p.cycle_repeat();
                    }
                });
                true
            }
            (PLAYER, "Shuffle") => {
                let Some(shuffle) = value.get::<bool>() else {
                    return false;
                };
                self.with(|p| {
                    if p.is_shuffled() != shuffle {
                        p.toggle_shuffle();
                    }
                });
                true
            }
            (PLAYER, "Volume") => {
                let Some(volume) = value.get::<f64>() else {
                    return false;
                };
                self.with(|p| {
                    p.set_muted(false);
                    p.set_volume(volume);
                });
                true
            }
            _ => false,
        }
    }

    /// A method call from the desktop, answered with its error's name and
    /// message when it fails. The bus checks the arguments' types first.
    fn call(&self, interface: &str, method: &str, args: &glib::Variant) -> Result<(), MethodError> {
        match (interface, method) {
            (ROOT, "Raise") => (self.raise)(),
            (ROOT, "Quit") => (self.quit)(),
            (PLAYER, "Next") => self.with(|p| p.next()),
            (PLAYER, "Previous") => self.with(|p| p.previous()),
            (PLAYER, "Pause") => self.with(|p| {
                if p.is_playing() {
                    p.toggle();
                }
            }),
            (PLAYER, "PlayPause") => self.with(|p| p.toggle()),
            (PLAYER, "Stop") => (self.stop)(),
            (PLAYER, "Play") => self.with(|p| {
                if !p.is_playing() {
                    p.toggle();
                }
            }),
            (PLAYER, "Seek") => {
                let (offset,) = args.get::<(Time,)>().ok_or_else(MethodError::args)?;
                self.with(|p| p.jump(offset as f64 / 1e6));
            }
            (PLAYER, "SetPosition") => {
                let (track, position) = args
                    .get::<(glib::variant::ObjectPath, Time)>()
                    .ok_or_else(MethodError::args)?;
                let length = self.metadata.borrow().length;
                if let Some(at) =
                    position_to_set(&self.track_id(), track.as_str(), position, length)
                {
                    self.with(|p| p.seek(at));
                }
            }
            (PLAYER, "OpenUri") => {
                return Err(MethodError {
                    name: "org.freedesktop.DBus.Error.NotSupported",
                    message: "Spoolframe plays from its server only",
                });
            }
            _ => {
                return Err(MethodError {
                    name: "org.freedesktop.DBus.Error.UnknownMethod",
                    message: "no such method",
                });
            }
        }
        Ok(())
    }
}

/// A D-Bus error reply.
#[derive(Debug, PartialEq, Eq)]
struct MethodError {
    name: &'static str,
    message: &'static str,
}

impl MethodError {
    fn args() -> Self {
        Self {
            name: "org.freedesktop.DBus.Error.InvalidArgs",
            message: "arguments of the wrong type",
        }
    }
}

/// Where SetPosition goes: only for the item playing now, and within it
/// (the specification says to ignore anything else).
fn position_to_set(
    playing: &str,
    track: &str,
    position: Time,
    length: Option<Time>,
) -> Option<Duration> {
    if track != playing || position < 0 || length.is_some_and(|l| position > l) {
        return None;
    }
    Some(Duration::from_micros(position as u64))
}

pub struct Mpris {
    state: Rc<State>,
}

impl Mpris {
    /// Registers `org.mpris.MediaPlayer2.spoolframe.instance<pid>`.
    /// `raise` shows the window and the full player; `stop` ends playback;
    /// `quit` closes the app. Without a session bus there is no MPRIS, and
    /// the app plays on.
    pub fn start(raise: Rc<dyn Fn()>, stop: Rc<dyn Fn()>, quit: Rc<dyn Fn()>) -> Rc<Self> {
        let state = State::new(raise, stop, quit);
        let this = Rc::new(Self {
            state: state.clone(),
        });
        glib::spawn_future_local(async move {
            let bus = match gio::bus_get_future(gio::BusType::Session).await {
                Ok(bus) => bus,
                Err(e) => return tracing::warn!(error = %e, "MPRIS is not available"),
            };
            let name = bus_name(std::process::id());
            match serve(&bus, &state, &name).await {
                Ok(()) => tracing::debug!(bus_name = %name, "MPRIS registered"),
                Err(e) => tracing::warn!(error = %e, "MPRIS is not available"),
            }
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
            Property::Metadata(Metadata::default()),
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
                    self.seeked(time(*at));
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
            | Change::PlaylistFinished(_)
            | Change::VisualizerStopped
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

    /// Emits PropertiesChanged. Without a bus there is no one to tell,
    /// and it is dropped: a desktop that sees the name appear reads every
    /// property then.
    fn changed(&self, properties: Vec<Property>) {
        self.emit(
            PROPERTIES,
            "PropertiesChanged",
            &properties_changed(&properties),
        );
    }

    fn seeked(&self, position: Time) {
        self.emit(PLAYER, "Seeked", &(position,).to_variant());
    }

    fn emit(&self, interface: &str, signal: &str, body: &glib::Variant) {
        let Some(bus) = self.state.bus.borrow().clone() else {
            return;
        };
        if let Err(e) = bus.emit_signal(None, OBJECT_PATH, interface, signal, Some(body)) {
            tracing::debug!(error = %e, signal, "MPRIS signal");
        }
    }
}

/// `org.mpris.MediaPlayer2.spoolframe.instance<pid>`: the Flatpak may own
/// `org.mpris.MediaPlayer2.spoolframe.*`, and a second copy of the app
/// gets a name of its own.
fn bus_name(pid: u32) -> String {
    format!("{ROOT}.spoolframe.instance{pid}")
}

/// Puts both interfaces on `bus` and owns `name`, returning once the bus
/// has given it.
async fn serve(bus: &gio::DBusConnection, state: &Rc<State>, name: &str) -> Result<(), String> {
    let node = gio::DBusNodeInfo::for_xml(INTROSPECTION).map_err(|e| e.to_string())?;
    for interface in [ROOT, PLAYER] {
        let info = node
            .lookup_interface(interface)
            .ok_or_else(|| format!("{interface} is not in the introspection data"))?;
        let (call, get, set) = (state.clone(), state.clone(), state.clone());
        bus.register_object(OBJECT_PATH, &info)
            .method_call(move |_, _, _, interface, method, args, invocation| {
                match call.call(interface.unwrap_or_default(), method, &args) {
                    Ok(()) => invocation.return_value(None),
                    Err(e) => invocation.return_dbus_error(e.name, e.message),
                }
            })
            .property(move |_, _, _, interface, name| {
                let value = match interface {
                    ROOT => get.root_property(name),
                    _ => get.player_property(name),
                };
                // The bus asks only for properties the XML has.
                value.unwrap_or_else(|| {
                    tracing::warn!(interface, name, "MPRIS has no such property");
                    false.to_variant()
                })
            })
            .set_property(move |_, _, _, interface, name, value| {
                set.set_property(interface, name, &value)
            })
            .build()
            .map_err(|e| format!("cannot register {interface}: {e}"))?;
    }
    state.bus.replace(Some(bus.clone()));
    let (told, owned) = tokio::sync::oneshot::channel();
    let told = Rc::new(RefCell::new(Some(told)));
    let lost = told.clone();
    // Kept for the app's life, as the name is: never given up.
    let _owner = gio::bus_own_name_on_connection(
        bus,
        name,
        gio::BusNameOwnerFlags::DO_NOT_QUEUE,
        move |_, _| {
            if let Some(told) = told.take() {
                let _ = told.send(true);
            }
        },
        move |_, name| match lost.take() {
            Some(told) => {
                let _ = told.send(false);
            }
            None => tracing::warn!(name, "MPRIS lost its bus name"),
        },
    );
    match owned.await {
        Ok(true) => Ok(()),
        _ => Err(format!("the bus refused the name {name}")),
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
    let title = if facts.title.trim().is_empty() {
        "Untitled".to_owned()
    } else {
        facts.title.clone()
    };
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
    let length = Some(length)
        .filter(|l| !l.is_zero())
        .or(facts.length)
        .unwrap_or_default();
    Metadata {
        trackid: Some(track_id(track)),
        title: Some(title),
        artist: artist.map(|a| vec![a]),
        album,
        length: length_of(length),
        art_url: art_url.map(str::to_owned),
    }
}

fn track_id(track: u64) -> String {
    format!("{TRACK_PATH}/{track}")
}

fn time(duration: Duration) -> Time {
    duration.as_micros() as Time
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

#[cfg(test)]
mod tests {
    use spoolframe_types::{Episode, MediaId, MediaKind, Movie, Namespace, PlaybackState, Track};

    use super::*;

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
            year: None,
            air_date: None,
            cast: Vec::new(),
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
            cast: Vec::new(),
            release_date: None,
        })
    }

    #[test]
    fn a_song_has_its_artist_and_album() {
        let (track, album) = song();
        let m = metadata(1, &track, Some(&album), Duration::ZERO, None);
        assert_eq!(
            (m.title.as_deref(), m.artist, m.album.as_deref()),
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
            (m.title.as_deref(), m.artist, m.album.as_deref()),
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
            (m.title.as_deref(), m.artist, m.album.as_deref()),
            (Some("Blade Runner"), None, None)
        );
    }

    #[test]
    fn a_title_is_never_empty() {
        let m = metadata(1, &movie("  "), None, Duration::ZERO, None);
        assert_eq!(m.title.as_deref(), Some("Untitled"));
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
        assert_eq!(m.length, Some(7_000_000_000));
    }

    #[test]
    fn the_catalog_length_stands_in_until_the_file_says() {
        let m = metadata(1, &movie("Blade Runner"), None, Duration::ZERO, None);
        assert_eq!(m.length, Some(7_020_000_000));
    }

    #[test]
    fn each_item_played_has_its_own_track_id() {
        let first = metadata(1, &episode(), None, Duration::ZERO, None);
        let second = metadata(2, &episode(), None, Duration::ZERO, None);
        assert_eq!(
            (first.trackid.as_deref(), second.trackid.as_deref()),
            (
                Some("/casa/coldsteel/Spoolframe/item/1"),
                Some("/casa/coldsteel/Spoolframe/item/2")
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
        assert_eq!(m.art_url.as_deref(), Some("file:///tmp/cover"));
    }

    /// The keys and types MPRIS and xesam give them: an object path, a
    /// 64-bit length, a list of artists.
    #[test]
    fn metadata_goes_on_the_bus_as_mpris_and_xesam_keys() {
        let (track, album) = song();
        let length = Duration::from_secs(330);
        let m = metadata(4, &track, Some(&album), length, Some("file:///c"));
        let dict = glib::VariantDict::new(Some(&m.to_variant()));
        let types: Vec<_> = [
            "mpris:trackid",
            "mpris:length",
            "mpris:artUrl",
            "xesam:title",
            "xesam:artist",
            "xesam:album",
        ]
        .into_iter()
        .map(|key| dict.lookup_value(key, None).map(|v| v.type_().to_string()))
        .collect();
        assert_eq!(
            (m.to_variant().type_().as_str(), types),
            (
                "a{sv}",
                ["o", "x", "s", "s", "as", "s"]
                    .map(|t| Some(t.to_owned()))
                    .to_vec()
            )
        );
    }

    #[test]
    fn no_item_is_empty_metadata() {
        assert_eq!(Metadata::default().to_variant().n_children(), 0);
    }

    #[test]
    fn properties_changed_names_the_player_and_its_new_values() {
        let body = properties_changed(&[
            Property::PlaybackStatus(PlaybackStatus::Paused),
            Property::CanGoNext(true),
        ]);
        assert_eq!(
            body.to_string(),
            "('org.mpris.MediaPlayer2.Player', {'PlaybackStatus': <'Paused'>, \
             'CanGoNext': <true>}, @as [])"
        );
    }

    #[test]
    fn repeat_is_mpris_loop_status() {
        assert_eq!(
            [Repeat::Off, Repeat::All, Repeat::One].map(|r| loop_status(r).as_str()),
            ["None", "Playlist", "Track"]
        );
    }

    #[test]
    fn a_loop_status_reads_back() {
        let all = [LoopStatus::None, LoopStatus::Track, LoopStatus::Playlist];
        assert_eq!(all.map(|s| LoopStatus::parse(s.as_str())), all.map(Some));
    }

    #[test]
    fn an_unknown_loop_status_is_refused() {
        assert_eq!(LoopStatus::parse("Shuffle"), None);
    }

    #[test]
    fn set_position_moves_within_the_item_playing() {
        let now = track_id(3);
        assert_eq!(
            position_to_set(&now, &now, 2_500_000, Some(10_000_000)),
            Some(Duration::from_millis(2500))
        );
    }

    #[test]
    fn set_position_for_another_item_is_ignored() {
        assert_eq!(position_to_set(&track_id(3), &track_id(2), 0, None), None);
    }

    #[test]
    fn set_position_past_the_end_or_before_the_start_is_ignored() {
        let now = track_id(3);
        assert_eq!(
            [
                position_to_set(&now, &now, 11_000_000, Some(10_000_000)),
                position_to_set(&now, &now, -1, Some(10_000_000)),
            ],
            [None, None]
        );
    }

    #[test]
    fn the_bus_name_fits_the_flatpaks_grant() {
        let name = bus_name(4242);
        assert_eq!(
            (
                name.as_str(),
                name.starts_with("org.mpris.MediaPlayer2.spoolframe."),
                gio::dbus_is_name(&name)
            ),
            ("org.mpris.MediaPlayer2.spoolframe.instance4242", true, true)
        );
    }

    #[test]
    fn the_introspection_has_both_interfaces() {
        let node = gio::DBusNodeInfo::for_xml(INTROSPECTION).unwrap();
        assert_eq!(
            [ROOT, PLAYER].map(|i| node.lookup_interface(i).is_some()),
            [true, true]
        );
    }

    /// The (name, signature) of each property `interface` declares.
    fn declared(info: &gio::DBusInterfaceInfo) -> Vec<(String, String)> {
        use glib::translate::ToGlibPtr as _;
        let raw: *mut gio::ffi::GDBusInterfaceInfo = info.to_glib_none().0;
        let mut found = Vec::new();
        // SAFETY: `info` keeps the parsed struct alive; its properties are
        // a NULL-terminated array of structs with C strings.
        unsafe {
            let mut at = (*raw).properties;
            while !at.is_null() && !(*at).is_null() {
                let property = &**at;
                let text = |s| std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned();
                found.push((text(property.name), text(property.signature)));
                at = at.add(1);
            }
        }
        found
    }

    /// Every property the XML declares has a value of the declared type.
    #[test]
    fn every_declared_property_has_a_value_of_its_type() {
        let state = State::new(Rc::new(|| ()), Rc::new(|| ()), Rc::new(|| ()));
        let node = gio::DBusNodeInfo::for_xml(INTROSPECTION).unwrap();
        let mut wrong = Vec::new();
        let mut count = 0;
        for interface in [ROOT, PLAYER] {
            for (name, signature) in declared(&node.lookup_interface(interface).unwrap()) {
                count += 1;
                let value = match interface {
                    ROOT => state.root_property(&name),
                    _ => state.player_property(&name),
                };
                let got = value.map(|v| v.type_().to_string());
                if got.as_deref() != Some(signature.as_str()) {
                    wrong.push((name, signature, got));
                }
            }
        }
        // 9 on the root interface, 15 on the Player.
        assert_eq!((count, wrong), (24, Vec::new()));
    }

    /// With nothing attached, the calls that need no playback still work.
    #[test]
    fn raise_quit_and_stop_reach_the_app() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let call = |what: &'static str| -> Rc<dyn Fn()> {
            let calls = calls.clone();
            Rc::new(move || calls.borrow_mut().push(what))
        };
        let state = State::new(call("raise"), call("stop"), call("quit"));
        let none = ().to_variant();
        for (interface, method) in [(ROOT, "Raise"), (PLAYER, "Stop"), (ROOT, "Quit")] {
            state.call(interface, method, &none).unwrap();
        }
        assert_eq!(*calls.borrow(), ["raise", "stop", "quit"]);
    }

    #[test]
    fn open_uri_is_not_supported() {
        let state = State::new(Rc::new(|| ()), Rc::new(|| ()), Rc::new(|| ()));
        let args = ("file:///x".to_owned(),).to_variant();
        assert_eq!(
            state.call(PLAYER, "OpenUri", &args).map_err(|e| e.name),
            Err("org.freedesktop.DBus.Error.NotSupported")
        );
    }

    async fn connect(address: &str) -> gio::DBusConnection {
        let flags = gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION;
        gio::DBusConnection::for_address_future(address, flags, None)
            .await
            .expect("the test bus takes connections")
    }

    async fn call(
        desktop: &gio::DBusConnection,
        name: &str,
        interface: &str,
        method: &str,
        args: Option<glib::Variant>,
    ) -> Result<glib::Variant, glib::Error> {
        desktop
            .call_future(
                Some(name),
                OBJECT_PATH,
                interface,
                method,
                args.as_ref(),
                None,
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await
    }

    /// What a desktop sees of the player on the bus.
    #[derive(Debug, PartialEq)]
    struct Seen {
        identity: Option<String>,
        status: Option<String>,
        can_play: Option<bool>,
        metadata_keys: usize,
        raised: u32,
        open_uri: Option<String>,
        changed: String,
    }

    async fn what_a_desktop_sees(address: &str) -> Seen {
        let raised = Rc::new(Cell::new(0));
        let raise = raised.clone();
        let state = State::new(
            Rc::new(move || raise.set(raise.get() + 1)),
            Rc::new(|| ()),
            Rc::new(|| ()),
        );
        let (app, desktop) = (connect(address).await, connect(address).await);
        let name = bus_name(1);
        serve(&app, &state, &name).await.expect("the name is free");

        let all = |interface: &str| (interface.to_owned(),).to_variant();
        let root = call(&desktop, &name, PROPERTIES, "GetAll", Some(all(ROOT)));
        let root = glib::VariantDict::new(Some(&root.await.unwrap().child_value(0)));
        let player = call(&desktop, &name, PROPERTIES, "GetAll", Some(all(PLAYER)));
        let player = glib::VariantDict::new(Some(&player.await.unwrap().child_value(0)));
        let metadata = player.lookup_value("Metadata", None).unwrap();

        call(&desktop, &name, ROOT, "Raise", None).await.unwrap();
        let uri = ("file:///x".to_owned(),).to_variant();
        let refused = call(&desktop, &name, PLAYER, "OpenUri", Some(uri)).await;

        let (tell, heard) = tokio::sync::oneshot::channel::<glib::Variant>();
        let tell = RefCell::new(Some(tell));
        let _listening = desktop.subscribe_to_signal(
            None,
            Some(PROPERTIES),
            Some("PropertiesChanged"),
            Some(OBJECT_PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if let Some(tell) = tell.take() {
                    let _ = tell.send(signal.parameters.clone());
                }
            },
        );
        // A call after subscribing: once it is answered, the bus has the
        // match rule, which was sent first.
        call(&desktop, &name, PROPERTIES, "GetAll", Some(all(ROOT)))
            .await
            .unwrap();
        Mpris {
            state: state.clone(),
        }
        .detach();
        let changed = heard.await.unwrap();

        Seen {
            identity: root.lookup::<String>("Identity").unwrap(),
            status: player.lookup::<String>("PlaybackStatus").unwrap(),
            can_play: player.lookup::<bool>("CanPlay").unwrap(),
            metadata_keys: metadata.n_children(),
            raised: raised.get(),
            open_uri: refused
                .err()
                .and_then(|e| gio::DBusError::remote_error(&e))
                .map(|e| e.to_string()),
            changed: glib::VariantDict::new(Some(&changed.child_value(1)))
                .lookup_value("PlaybackStatus", None)
                .map_or_else(|| "no PlaybackStatus".to_owned(), |v| v.to_string()),
        }
    }

    /// The player on a bus of the test's own (dbus-daemon, as GIO's test
    /// bus starts it): what GetAll, a method, a refused method and a
    /// change look like from the desktop's side.
    #[test]
    fn a_desktop_on_the_bus_sees_the_player() {
        assert!(
            glib::find_program_in_path("dbus-daemon").is_some(),
            "this test starts a bus of its own with dbus-daemon, which is not on PATH"
        );
        let context = glib::MainContext::new();
        let seen = context
            .with_thread_default(|| {
                let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
                bus.up();
                let address = bus.bus_address().expect("the test bus is up").to_string();
                let seen = context.block_on(what_a_desktop_sees(&address));
                bus.down();
                seen
            })
            .unwrap();
        assert_eq!(
            seen,
            Seen {
                identity: Some("Spoolframe".into()),
                status: Some("Stopped".into()),
                can_play: Some(false),
                metadata_keys: 0,
                raised: 1,
                open_uri: Some("org.freedesktop.DBus.Error.NotSupported".into()),
                changed: "'Stopped'".into(),
            }
        );
    }

    #[test]
    fn a_loop_status_that_is_not_one_is_refused() {
        let state = State::new(Rc::new(|| ()), Rc::new(|| ()), Rc::new(|| ()));
        assert!(!state.set_property(PLAYER, "LoopStatus", &"Sometimes".to_variant()));
    }
}
