//! The player page: GStreamer's GstPlay (playbin3) reads the server's file
//! through the spoolframe:// source and draws into gtk4paintablesink, with
//! controls, a tracks menu and a diagnostics panel over it, as in the
//! mockups. The page is dark in every theme. It reports progress to the
//! server, and plays an album's tracks one after another.
//!
//! Closures attached to widgets hold the widgets weakly, so closing the
//! page frees the page and stops the pipeline.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gst::prelude::*;
use gst_play::prelude::*;
use gtk::glib;
use gtk::glib::thread_guard::ThreadGuard;
use spoolframe_linux_core::backend::{PlaybackHandle, PlaybackPlan, Selection, Session};
use spoolframe_linux_core::format;
use spoolframe_linux_core::reader::{self, MediaReader, Spawn};
use spoolframe_protocol::{PlannedSubtitle, SubtitleDelivery};
use spoolframe_types::{Item, MediaTracks, SubtitleFormat};

use crate::source::Published;

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
}

/// A page that plays the playlist.
pub fn page(playlist: Playlist) -> adw::NavigationPage {
    let title = playlist
        .items
        .get(playlist.index)
        .map(|i| crate::views::Facts::of(i).title)
        .unwrap_or_default();
    match Player::new(playlist) {
        Ok(player) => player.page(&title),
        Err(e) => {
            tracing::error!(error = %e, "cannot build the player");
            crate::pages::page(
                &title,
                &adw::StatusPage::builder()
                    .icon_name("dialog-error-symbolic")
                    .title("Cannot play")
                    .description(e.to_string())
                    .vexpand(true)
                    .build(),
            )
        }
    }
}

/// How often progress goes to the server while playing (P1-14 throttles
/// on its side too).
const PROGRESS_EVERY: u32 = 10;

struct Player {
    play: gst_play::Play,
    adapter: gst_play::PlaySignalAdapter,
    picture: gtk::Picture,
    playlist: Playlist,
}

impl Player {
    fn new(playlist: Playlist) -> anyhow::Result<Self> {
        let sink = gst::ElementFactory::make("gtk4paintablesink").build()?;
        let paintable = sink.property::<gtk::gdk::Paintable>("paintable");
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&sink);
        let play = gst_play::Play::new(Some(renderer));
        let mut config = play.config();
        config.set_position_update_interval(250);
        play.set_config(config)?;
        let adapter = gst_play::PlaySignalAdapter::new(&play);
        let picture = gtk::Picture::builder()
            .paintable(&paintable)
            .content_fit(gtk::ContentFit::Contain)
            .hexpand(true)
            .vexpand(true)
            .build();
        Ok(Self {
            play,
            adapter,
            picture,
            playlist,
        })
    }

    fn page(self, title: &str) -> adw::NavigationPage {
        let video = gtk::Overlay::builder()
            .child(&self.picture)
            .css_classes(["player"])
            .build();
        // Shown when the file has no video: what is playing, for music.
        let now_playing = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .visible(false)
            .css_classes(["now-playing"])
            .build();
        let now_title = gtk::Label::builder().css_classes(["title-1"]).build();
        let now_detail = gtk::Label::builder().css_classes(["dim-label"]).build();
        now_playing.append(
            &gtk::Image::builder()
                .icon_name("audio-x-generic-symbolic")
                .pixel_size(96)
                .build(),
        );
        now_playing.append(&now_title);
        now_playing.append(&now_detail);
        video.add_overlay(&now_playing);
        let starting = gtk::Spinner::builder()
            .spinning(true)
            .width_request(48)
            .height_request(48)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        video.add_overlay(&starting);
        // Shown while the read-ahead catches up after running dry.
        let buffering = gtk::Label::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .visible(false)
            .css_classes(["osd", "buffering", "title-3"])
            .build();
        video.add_overlay(&buffering);
        let controls = Controls::new(&self.play);
        video.add_overlay(&controls.bar);
        let diagnostics = Diagnostics::new();
        video.add_overlay(&diagnostics.panel);
        controls
            .diagnostics
            .bind_property("active", &diagnostics.panel, "visible")
            .sync_create()
            .build();
        let error = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("Cannot play")
            .visible(false)
            .css_classes(["osd"])
            .build();
        video.add_overlay(&error);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&video));
        let page = adw::NavigationPage::builder()
            .title(title)
            .child(&toolbar)
            .css_classes(["player-page"])
            .build();

        let Player {
            play,
            adapter,
            playlist,
            ..
        } = self;
        let ui = Rc::new(Ui {
            play: play.clone(),
            pipeline: play.pipeline(),
            page: page.downgrade(),
            scale: controls.scale.clone(),
            position: controls.position.clone(),
            left: controls.left.clone(),
            toggle: controls.toggle.clone(),
            tracks: controls.tracks.clone(),
            diagnostics: diagnostics.clone(),
            error: error.clone(),
            starting,
            buffering,
            now_playing,
            now_title,
            now_detail,
            updating: controls.updating.clone(),
            playing: controls.playing.clone(),
            session: playlist.session,
            items: playlist.items,
            index: Cell::new(playlist.index),
            resume: Cell::new(playlist.from),
            requested: Cell::new(playlist.selection),
            pending: Cell::new(None),
            track_list: RefCell::new(playlist.tracks),
            published: RefCell::new(None),
            subtitle_file: RefCell::new(None),
            tracks_built: Cell::new(false),
            stopped: Cell::new(false),
        });
        connect(&adapter, &ui);

        // Progress goes to the server every few seconds while playing.
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_seconds_local(PROGRESS_EVERY, move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.playing.get() {
                ui.report_progress();
            }
            glib::ControlFlow::Continue
        });

        // The read-ahead changes by the second; the panel follows it while
        // it is open.
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_local(Duration::from_millis(500), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.diagnostics.panel.is_visible()
                && let Some(published) = ui.published.borrow().as_ref()
            {
                ui.diagnostics.show_reader(&published.reader().status());
            }
            glib::ControlFlow::Continue
        });

        let first = ui.clone();
        glib::idle_add_local_once(move || first.start());
        // Leaving the page reports where playback got to and stops it.
        // This handler owns the adapter and the player state, so both live
        // exactly as long as the page.
        page.connect_hidden(move |_| {
            let _ = &adapter;
            if !ui.stopped.replace(true) {
                ui.report_progress();
                ui.play.stop();
            }
        });
        page
    }
}

fn connect(adapter: &gst_play::PlaySignalAdapter, ui: &Rc<Ui>) {
    // GstPlay's adapter wants thread-safe callbacks, but it delivers them
    // on the main loop it was made on, where the widgets live. The guard
    // satisfies the bound and panics if that ever changes.
    let guard = || ThreadGuard::new(ui.clone());
    let g = guard();
    adapter.connect_duration_changed(move |_, duration| g.get_ref().duration_changed(duration));
    let g = guard();
    adapter.connect_position_updated(move |_, at| g.get_ref().position_updated(at));
    let g = guard();
    adapter.connect_state_changed(move |_, state| g.get_ref().state_changed(state));
    let g = guard();
    adapter.connect_media_info_updated(move |_, info| g.get_ref().media_info_updated(info));
    let g = guard();
    adapter.connect_buffering(move |_, percent| g.get_ref().buffering(percent));
    let g = guard();
    adapter.connect_end_of_stream(move |_| g.get_ref().ended());
    let g = guard();
    adapter.connect_error(move |_, e, _| g.get_ref().failed(&e.to_string()));
}

/// Fetches run on Relm4's tokio runtime, where the SDK's networking lives.
fn spawn() -> Spawn {
    Arc::new(|future| {
        relm4::spawn(future);
    })
}

/// The widgets and state GstPlay's signals update, and the playlist.
struct Ui {
    play: gst_play::Play,
    pipeline: gst::Element,
    page: glib::WeakRef<adw::NavigationPage>,
    scale: gtk::Scale,
    position: gtk::Label,
    left: gtk::Label,
    toggle: gtk::Button,
    tracks: gtk::MenuButton,
    diagnostics: Diagnostics,
    error: adw::StatusPage,
    starting: gtk::Spinner,
    buffering: gtk::Label,
    now_playing: gtk::Box,
    now_title: gtk::Label,
    now_detail: gtk::Label,
    updating: Rc<Cell<bool>>,
    playing: Rc<Cell<bool>>,
    session: Arc<dyn Session>,
    items: Vec<Item>,
    index: Cell<usize>,
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
    tracks_built: Cell<bool>,
    stopped: Cell<bool>,
}

impl Ui {
    fn current(&self) -> Option<&Item> {
        self.items.get(self.index.get())
    }

    /// Starts the current item: the server plans it and opens a playback,
    /// then GstPlay reads it through the spoolframe:// source.
    fn start(self: &Rc<Self>) {
        let Some(item) = self.current().cloned() else {
            return;
        };
        let facts = crate::views::Facts::of(&item);
        if let Some(page) = self.page.upgrade() {
            page.set_title(&facts.title);
        }
        self.now_title.set_label(&facts.title);
        self.now_detail
            .set_label(facts.detail.as_deref().unwrap_or_default());
        self.starting.set_visible(true);
        self.error.set_visible(false);
        self.tracks_built.set(false);
        let length = facts.length;
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
            let Some(ui) = weak.upgrade() else { return };
            ui.starting.set_visible(false);
            let (start, tracks) = match result {
                Ok((Ok(start), tracks)) => (start, tracks),
                Ok((Err(e), _)) => return ui.failed(&e.to_string()),
                Err(e) => return ui.failed(&format!("starting playback failed: {e}")),
            };
            if tracks.is_some() {
                ui.track_list.replace(tracks);
            }
            // What plays: the tracks asked for, or the server's choice.
            let selection = requested
                .or(start.chosen.map(|c| c.selection))
                .unwrap_or_default();
            ui.pending.set(Some(selection));
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
                ui.session.clone(),
                item.id().clone(),
                selection,
                start,
                spawn(),
                length,
            );
            let published = Published::new(reader);
            ui.play.set_uri(Some(&published.uri()));
            ui.published.replace(Some(published));
            if external {
                ui.load_external_subtitle(handle);
            } else {
                ui.play.set_subtitle_uri(None);
            }
            ui.play.play();
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
            let Some(ui) = weak.upgrade() else { return };
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
            ui.play
                .set_subtitle_uri(Some(&gtk::gio::File::for_path(&path).uri()));
            ui.play.set_subtitle_track_enabled(true);
            ui.subtitle_file.replace(Some(path));
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
        let buffering = percent < 100 && !self.stopped.get();
        self.buffering.set_visible(buffering);
        if buffering {
            self.buffering.set_label(&format!("Buffering… {percent}%"));
        }
    }

    fn duration_changed(&self, duration: Option<gst::ClockTime>) {
        let secs = duration.map_or(0.0, |d| d.seconds_f64());
        self.scale.adjustment().set_upper(secs);
        // Seek to where the user stopped once the length is known, which is
        // when a seek can land.
        if secs > 0.0
            && let Some(at) = self.resume.take()
        {
            self.play
                .seek(gst::ClockTime::from_nseconds(at.as_nanos() as u64));
        }
    }

    fn position_updated(&self, at: Option<gst::ClockTime>) {
        let Some(at) = at else { return };
        let at = Duration::from_nanos(at.nseconds());
        let length = Duration::from_secs_f64(self.scale.adjustment().upper());
        self.updating.set(true);
        self.scale.set_value(at.as_secs_f64());
        self.updating.set(false);
        self.position.set_label(&format::timestamp(at));
        self.left.set_label(&format!(
            "−{}",
            format::timestamp(length.saturating_sub(at))
        ));
    }

    fn state_changed(&self, state: gst_play::PlayState) {
        let playing = state == gst_play::PlayState::Playing;
        let paused = self.playing.replace(playing) && !playing;
        self.toggle.set_icon_name(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        self.toggle
            .set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
        // Decoders exist once the pipeline plays.
        if playing {
            self.diagnostics.show_decoders(&self.pipeline);
        }
        if paused && !self.stopped.get() {
            self.report_progress();
        }
    }

    fn media_info_updated(&self, info: &gst_play::PlayMediaInfo) {
        self.diagnostics.show_media(info);
        self.now_playing
            .set_visible(info.video_streams().is_empty());
        if self.tracks_built.replace(true) {
            return;
        }
        self.tracks
            .set_popover(Some(&tracks_menu(&self.play, info)));
        if let Some(selection) = self.pending.take() {
            self.apply(selection, info);
        }
    }

    /// Picks the tracks that play. The selection counts
    /// positions in the item's track list; the file's streams are in the
    /// same order, external subtitles aside.
    fn apply(&self, selection: Selection, info: &gst_play::PlayMediaInfo) {
        if let Some(audio) = selection.audio
            && let Some(stream) = info.audio_streams().get(audio as usize)
            && let Err(e) = self.play.set_audio_track(stream.index())
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
        match embedded.and_then(|pos| info.subtitle_streams().get(pos).map(|s| s.index())) {
            Some(index) => match self.play.set_subtitle_track(index) {
                Ok(()) => self.play.set_subtitle_track_enabled(true),
                Err(e) => tracing::warn!(error = %e, index, "cannot pick the subtitle"),
            },
            // An external subtitle is turned on when it arrives.
            None if self.subtitle_file.borrow().is_some() => {}
            None => self.play.set_subtitle_track_enabled(false),
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
        if self.index.get() + 1 < self.items.len() {
            self.index.set(self.index.get() + 1);
            self.resume.set(None);
            self.requested.set(None);
            self.pending.set(None);
            self.track_list.replace(None);
            self.play.stop();
            self.start();
        }
    }

    fn failed(&self, error: &str) {
        tracing::warn!(error, "playback failed");
        self.starting.set_visible(false);
        self.error.set_description(Some(error));
        self.error.set_visible(true);
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        if let Some(path) = self.subtitle_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The OSD bar along the bottom: seek bar, transport, tracks, diagnostics
/// and fullscreen.
struct Controls {
    bar: gtk::Box,
    scale: gtk::Scale,
    position: gtk::Label,
    left: gtk::Label,
    toggle: gtk::Button,
    tracks: gtk::MenuButton,
    diagnostics: gtk::ToggleButton,
    /// True while the scale is moved by playback rather than by the user.
    updating: Rc<Cell<bool>>,
    /// Whether GstPlay last reported playing; the toggle reads it.
    playing: Rc<Cell<bool>>,
}

impl Controls {
    fn new(play: &gst_play::Play) -> Self {
        let updating = Rc::new(Cell::new(false));
        let scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 10.0, 0.0))
            .hexpand(true)
            .build();
        // change-value fires only for the user's moves, never for set_value.
        let seek_play = play.clone();
        scale.connect_change_value(move |_, _, value| {
            seek_play.seek(gst::ClockTime::from_seconds_f64(value.max(0.0)));
            glib::Propagation::Proceed
        });
        let position = mono("0:00");
        let left = mono("");

        let seek_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        seek_row.append(&position);
        seek_row.append(&scale);
        seek_row.append(&left);

        let back = jump_button(
            "Back 10 seconds",
            "media-seek-backward-symbolic",
            play,
            -10.0,
            &scale,
        );
        let toggle = gtk::Button::builder()
            .icon_name("media-playback-pause-symbolic")
            .tooltip_text("Pause")
            .css_classes(["circular", "suggested-action", "play-toggle"])
            .build();
        let toggle_play = play.clone();
        let playing = Rc::new(Cell::new(false));
        let is_playing = playing.clone();
        toggle.connect_clicked(move |_| {
            if is_playing.get() {
                toggle_play.pause();
            } else {
                toggle_play.play();
            }
        });
        let forward = jump_button(
            "Forward 30 seconds",
            "media-seek-forward-symbolic",
            play,
            30.0,
            &scale,
        );
        let transport = gtk::Box::builder().spacing(8).hexpand(true).build();
        transport.append(&back);
        transport.append(&toggle);
        transport.append(&forward);

        let tracks = gtk::MenuButton::builder()
            .icon_name("media-view-subtitles-symbolic")
            .tooltip_text("Audio and subtitles")
            .build();
        let diagnostics = gtk::ToggleButton::builder()
            .icon_name("utilities-system-monitor-symbolic")
            .tooltip_text("Playback diagnostics")
            .build();
        let fullscreen = gtk::ToggleButton::builder()
            .icon_name("view-fullscreen-symbolic")
            .tooltip_text("Fullscreen")
            .build();
        fullscreen.connect_toggled(|button| {
            let Some(window) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            if button.is_active() {
                window.fullscreen();
            } else {
                window.unfullscreen();
            }
        });
        let extras = gtk::Box::builder().spacing(8).build();
        extras.append(&tracks);
        extras.append(&diagnostics);
        extras.append(&fullscreen);

        let button_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        button_row.append(&transport);
        button_row.append(&extras);

        let bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::End)
            .css_classes(["osd", "player-controls"])
            .build();
        bar.append(&seek_row);
        bar.append(&button_row);

        Self {
            bar,
            scale,
            position,
            left,
            toggle,
            tracks,
            diagnostics,
            updating,
            playing,
        }
    }
}

fn jump_button(
    label: &str,
    icon: &str,
    play: &gst_play::Play,
    seconds: f64,
    scale: &gtk::Scale,
) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .css_classes(["circular", "flat"])
        .build();
    let play = play.clone();
    button.connect_clicked(glib::clone!(
        #[weak]
        scale,
        move |_| {
            let adjustment = scale.adjustment();
            let target = (adjustment.value() + seconds).clamp(0.0, adjustment.upper());
            play.seek(gst::ClockTime::from_seconds_f64(target));
        }
    ));
    button
}

/// The audio and subtitle choices, built from the file's streams.
fn tracks_menu(play: &gst_play::Play, info: &gst_play::PlayMediaInfo) -> gtk::Popover {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();

    column.append(&heading("Audio"));
    let mut group: Option<gtk::CheckButton> = None;
    for (i, stream) in info.audio_streams().iter().enumerate() {
        let label = stream_label(
            stream.language().as_deref(),
            stream.codec().as_deref(),
            Some(stream.channels())
                .filter(|c| *c > 0)
                .map(channel_layout),
        );
        let check = choice(&label, &mut group, i == 0);
        let play = play.clone();
        let index = stream.index();
        check.connect_toggled(move |c| {
            if c.is_active()
                && let Err(e) = play.set_audio_track(index)
            {
                tracing::warn!(error = %e, index, "cannot switch audio track");
            }
        });
        column.append(&check);
    }

    column.append(&heading("Subtitles"));
    let mut group: Option<gtk::CheckButton> = None;
    let off = choice("Off", &mut group, true);
    let off_play = play.clone();
    off.connect_toggled(move |c| {
        if c.is_active() {
            off_play.set_subtitle_track_enabled(false);
        }
    });
    column.append(&off);
    for stream in info.subtitle_streams() {
        let label = stream_label(
            stream.language().as_deref(),
            stream.codec().as_deref(),
            None,
        );
        let check = choice(&label, &mut group, false);
        let play = play.clone();
        let index = stream.index();
        check.connect_toggled(move |c| {
            if c.is_active() {
                match play.set_subtitle_track(index) {
                    Ok(()) => play.set_subtitle_track_enabled(true),
                    Err(e) => tracing::warn!(error = %e, index, "cannot switch subtitles"),
                }
            }
        });
        column.append(&check);
    }
    // The file opens with subtitles off; say so to the pipeline as well.
    play.set_subtitle_track_enabled(false);

    gtk::Popover::builder().child(&column).build()
}

fn choice(label: &str, group: &mut Option<gtk::CheckButton>, active: bool) -> gtk::CheckButton {
    let check = gtk::CheckButton::builder()
        .label(label)
        .active(active)
        .build();
    match group {
        Some(first) => check.set_group(Some(first)),
        None => *group = Some(check.clone()),
    }
    check
}

fn stream_label(language: Option<&str>, codec: Option<&str>, extra: Option<String>) -> String {
    let parts: Vec<String> = [language.map(str::to_owned), codec.map(str::to_owned), extra]
        .into_iter()
        .flatten()
        .collect();
    if parts.is_empty() {
        "Unnamed track".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn channel_layout(channels: i32) -> String {
    match channels {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        6 => "5.1".to_owned(),
        8 => "7.1".to_owned(),
        n => format!("{n} ch"),
    }
}

/// What the player is doing, for debugging playback: which decoders run,
/// whether in hardware, and what the file holds.
#[derive(Clone)]
struct Diagnostics {
    panel: gtk::Box,
    rows: Rc<RefCell<Vec<(&'static str, gtk::Label)>>>,
}

impl Diagnostics {
    const ROWS: [&'static str; 8] = [
        "source",
        "buffered",
        "fetching",
        "video",
        "decoder",
        "hardware",
        "audio",
        "decoder (audio)",
    ];

    fn new() -> Self {
        let grid = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(12)
            .build();
        let mut rows = Vec::new();
        for (i, name) in Self::ROWS.into_iter().enumerate() {
            let key = mono(name);
            key.add_css_class("dim-label");
            let value = mono("—");
            grid.attach(&key, 0, i as i32, 1, 1);
            grid.attach(&value, 1, i as i32, 1, 1);
            rows.push((name, value));
        }
        let panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(16)
            .margin_end(16)
            .visible(false)
            .css_classes(["osd", "diagnostics"])
            .build();
        panel.append(&heading("Diagnostics"));
        panel.append(&grid);
        Self {
            panel,
            rows: Rc::new(RefCell::new(rows)),
        }
    }

    fn set(&self, name: &str, value: &str) {
        if let Some((_, label)) = self.rows.borrow().iter().find(|(n, _)| *n == name) {
            label.set_label(value);
        }
    }

    /// Seconds ready ahead against the target, fetches in flight and the
    /// measured throughput.
    fn show_reader(&self, status: &reader::Status) {
        let ahead = match (status.ready, status.target) {
            (Some(ready), Some(target)) => {
                format!("{:.0} s of {} s", ready.as_secs_f64(), target.as_secs())
            }
            _ => format!("{} MiB", status.ready_bytes / (1024 * 1024)),
        };
        self.set("buffered", &ahead);
        let throughput = status.throughput.map_or("—".to_owned(), |rate| {
            format!("{:.1} Mbit/s", rate * 8.0 / 1_000_000.0)
        });
        self.set(
            "fetching",
            &format!("{} in flight · {throughput}", status.in_flight),
        );
    }

    fn show_media(&self, info: &gst_play::PlayMediaInfo) {
        self.set("source", "server · range reads");
        if let Some(video) = info.video_streams().first() {
            let codec = video.codec().map(|c| c.to_string()).unwrap_or_default();
            self.set(
                "video",
                &format!("{codec} · {}×{}", video.width(), video.height()),
            );
        }
        if let Some(audio) = info.audio_streams().first() {
            let codec = audio.codec().map(|c| c.to_string()).unwrap_or_default();
            self.set(
                "audio",
                &format!("{codec} · {}", channel_layout(audio.channels())),
            );
        }
    }

    /// Finds the decoders playbin3 picked. VA-API (`va…`), NVIDIA
    /// (`nv…`) and V4L2 decoders run in hardware.
    fn show_decoders(&self, pipeline: &gst::Element) {
        let Some(bin) = pipeline.downcast_ref::<gst::Bin>() else {
            return;
        };
        for element in bin.iterate_recurse().into_iter().flatten() {
            let Some(factory) = element.factory() else {
                continue;
            };
            let klass = factory
                .metadata(gst::ELEMENT_METADATA_KLASS)
                .unwrap_or_default();
            if !klass.contains("Decoder") {
                continue;
            }
            let name = factory.name();
            if klass.contains("Video") {
                let hardware = ["va", "nv", "v4l2"].iter().any(|p| name.starts_with(p));
                self.set("decoder", &name);
                self.set("hardware", if hardware { "yes" } else { "no" });
            } else if klass.contains("Audio") {
                self.set("decoder (audio)", &name);
            }
        }
    }
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .build()
}

fn mono(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["monospace", "numeric"])
        .build()
}
