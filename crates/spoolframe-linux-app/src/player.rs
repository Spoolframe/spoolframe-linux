//! The full player page, a window onto a [`Playback`]: the video under a
//! top bar (back, the title) and the controls (seek bar,
//! transport, tracks, diagnostics, fullscreen), as in the design. The bars
//! fade out while the video plays and come back on a mouse move. Music
//! has its own page, Now Playing.
//!
//! Going back (the button, Esc, the window's back gesture) leaves the page
//! and keeps playing in the bar along the bottom of the window; the bar's
//! stop ends playback. The page holds its widgets and the playback only
//! through the page itself, so leaving frees the page.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gst::prelude::*;
use gst_play::prelude::*;
use gtk::glib;
use spoolframe_linux_core::format;
use spoolframe_linux_core::reader;
use spoolframe_types::Item;

use crate::playback::{Change, Playback, VideoSink};
use crate::tracks;
use crate::volume::Volume;

/// What the page asks of the app.
pub struct Actions {
    /// Leave the page and keep playing in the bar.
    pub minimize: Rc<dyn Fn()>,
}

/// How long the controls stay after the mouse last moved, while playing.
const CONTROLS_LINGER: Duration = Duration::from_secs(3);

/// The player page for `playback`.
pub fn page(playback: &Rc<Playback>, actions: Actions) -> adw::NavigationPage {
    let picture = gtk::Picture::builder()
        .paintable(playback.paintable())
        .content_fit(gtk::ContentFit::Contain)
        .hexpand(true)
        .vexpand(true)
        .build();
    let video = gtk::Overlay::builder()
        .child(&picture)
        .css_classes(["player"])
        .build();

    let starting = gtk::Spinner::builder()
        .spinning(true)
        .width_request(48)
        .height_request(48)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    video.add_overlay(&starting);
    let buffering = gtk::Label::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .visible(false)
        .css_classes(["osd", "buffering", "title-3"])
        .build();
    video.add_overlay(&buffering);
    let error = adw::StatusPage::builder()
        .icon_name(crate::icons::ERROR)
        .title("Cannot play")
        .visible(false)
        .css_classes(["osd"])
        .build();
    video.add_overlay(&error);

    let top = TopBar::new();
    let top_revealer = fade(&top.bar, gtk::Align::Start);
    video.add_overlay(&top_revealer);

    let controls = Controls::new(playback);
    let bottom_revealer = fade(&controls.bar, gtk::Align::End);
    video.add_overlay(&bottom_revealer);

    let diagnostics = Diagnostics::new();
    video.add_overlay(&diagnostics.panel);
    controls
        .diagnostics
        .bind_property("active", &diagnostics.panel, "visible")
        .sync_create()
        .build();

    let page = adw::NavigationPage::builder()
        .title("Player")
        .child(&video)
        .css_classes(["player-page"])
        .build();
    follow_fullscreen(&page, &video, &controls.fullscreen, &actions);
    hide_when_idle(
        &video,
        &[top_revealer.clone(), bottom_revealer.clone()],
        playback,
        &[
            controls.tracks.upcast_ref(),
            controls.diagnostics.upcast_ref(),
        ],
    );

    // What the playback says, onto the widgets, for as long as the page
    // lives.
    let view = Rc::new(View {
        page: page.downgrade(),
        top,
        controls,
        diagnostics,
        starting,
        buffering,
        error,
        tracks_built: Cell::new(None),
    });
    {
        let view = Rc::downgrade(&view);
        let weak_playback = Rc::downgrade(playback);
        playback.subscribe(move |change| {
            let (Some(view), Some(playback)) = (view.upgrade(), weak_playback.upgrade()) else {
                return false;
            };
            view.update(&playback, change);
            true
        });
    }

    // The diagnostics follow the read-ahead and the sink while open.
    {
        let view = Rc::downgrade(&view);
        let weak_playback = Rc::downgrade(playback);
        glib::timeout_add_local(Duration::from_millis(500), move || {
            let (Some(view), Some(playback)) = (view.upgrade(), weak_playback.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            if let Some(status) = playback.reader_status() {
                view.controls.show_buffered(status.ready);
                if view.diagnostics.panel.is_visible() {
                    view.diagnostics.show_reader(&status);
                }
            }
            if view.diagnostics.panel.is_visible() {
                view.diagnostics.show_video(playback.video_sink());
            }
            glib::ControlFlow::Continue
        });
    }

    // Leaving the page keeps playing, in the bar.
    // The spinner stays until a picture arrives: from opening, or a resume
    // seek, the screen is black until the first frame.
    let frames = {
        let (spinner, buffering) = (view.starting.downgrade(), view.buffering.downgrade());
        playback.paintable().connect_invalidate_contents(move |_| {
            let (Some(spinner), Some(buffering)) = (spinner.upgrade(), buffering.upgrade()) else {
                return;
            };
            if spinner.is_visible() && !buffering.is_visible() {
                spinner.set_visible(false);
            }
        })
    };
    let paintable = playback.paintable().clone();
    let frames = std::cell::Cell::new(Some(frames));
    // The player's keys (Space, the arrows, f, Esc) reach it only when
    // something in it has the focus; nothing did until the first click.
    video.set_focusable(true);
    page.connect_shown(glib::clone!(
        #[weak]
        video,
        move |_| {
            video.grab_focus();
        }
    ));
    page.connect_hidden(move |page| {
        // The page owns its view.
        let _ = &view;
        if let Some(frames) = frames.take() {
            paintable.disconnect(frames);
        }
        if let Some(window) = page.root().and_downcast::<gtk::Window>() {
            window.unfullscreen();
        }
    });
    page
}

/// The widgets the playback's changes reach.
struct View {
    page: glib::WeakRef<adw::NavigationPage>,
    top: TopBar,
    controls: Controls,
    diagnostics: Diagnostics,
    starting: gtk::Spinner,
    buffering: gtk::Label,
    error: adw::StatusPage,
    /// The audio and subtitle stream counts the menu was built for.
    tracks_built: Cell<Option<(usize, usize)>>,
}

impl View {
    fn update(&self, playback: &Playback, change: &Change) {
        match change {
            Change::Item(item) => self.item(item),
            Change::Starting => {
                self.starting.set_visible(true);
                self.error.set_visible(false);
            }
            Change::Playing(playing) => {
                self.controls.show_playing(*playing);
                if *playing {
                    self.diagnostics.show_decoders(&playback.pipeline());
                }
            }
            Change::Position { at, length } => self.controls.show_position(*at, *length),
            Change::MediaInfo(info) => self.media_info(playback, info),
            Change::Buffering(percent) => {
                // The picture freezes or stays black meanwhile: say why.
                self.starting.set_visible(*percent < 100);
                self.buffering.set_visible(*percent < 100);
                self.buffering.set_label(&format!("Buffering… {percent}%"));
            }
            Change::Failed(error) => {
                self.starting.set_visible(false);
                self.error.set_description(Some(error));
                self.error.set_visible(true);
            }
            Change::Volume(_) | Change::Muted(_) => self.controls.volume.show(change),
            Change::Modes { .. } | Change::Queue => {}
        }
    }

    fn item(&self, item: &Item) {
        let facts = crate::views::Facts::of(item);
        if let Some(page) = self.page.upgrade() {
            page.set_title(&facts.title);
        }
        self.top.title.set_label(&facts.title);
        let detail = facts.detail.clone();
        self.top
            .detail
            .set_label(detail.as_deref().unwrap_or_default());
        self.top.detail.set_visible(detail.is_some());
        self.tracks_built.set(None);
    }

    fn media_info(&self, playback: &Playback, info: &gst_play::PlayMediaInfo) {
        self.diagnostics.show_media(info);
        // GstPlay updates the media info as streams turn up; the menu is
        // built again whenever the stream lists change.
        let counts = (info.audio_streams().len(), info.subtitle_streams().len());
        if counts == (0, 0) || self.tracks_built.replace(Some(counts)) == Some(counts) {
            return;
        }
        self.controls.tracks.set_popover(Some(&tracks::menu(
            playback.play(),
            info,
            playback.subtitles_on(),
        )));
    }
}

/// Fades `bar` in and out over the video, at the top or the bottom.
fn fade(bar: &gtk::Box, valign: gtk::Align) -> gtk::Revealer {
    gtk::Revealer::builder()
        .child(bar)
        .valign(valign)
        .transition_type(gtk::RevealerTransitionType::Crossfade)
        .reveal_child(true)
        .build()
}

/// Hides the bars and the pointer a few seconds after the mouse last moved,
/// while playing; a move, a click or a key brings them back. Not while a
/// menu or the diagnostics are open.
fn hide_when_idle(
    video: &gtk::Overlay,
    bars: &[gtk::Revealer],
    playback: &Rc<Playback>,
    keep: &[&gtk::Widget],
) {
    let bars: Vec<_> = bars.iter().map(|b| b.downgrade()).collect();
    let keep: Vec<_> = keep.iter().map(|w| w.downgrade()).collect();
    let weak_playback = Rc::downgrade(playback);
    let weak_video = video.downgrade();
    let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    let show = Rc::new(move || {
        for bar in bars.iter().filter_map(|b| b.upgrade()) {
            bar.set_reveal_child(true);
        }
        if let Some(video) = weak_video.upgrade() {
            video.set_cursor(None);
        }
        if let Some(id) = timer.take() {
            id.remove();
        }
        let (bars, keep, weak_playback, weak_video, timer_slot) = (
            bars.clone(),
            keep.clone(),
            weak_playback.clone(),
            weak_video.clone(),
            timer.clone(),
        );
        let id = glib::timeout_add_local_once(CONTROLS_LINGER, move || {
            timer_slot.take();
            let playing = weak_playback.upgrade().is_some_and(|p| p.is_playing());
            let busy = keep.iter().filter_map(|w| w.upgrade()).any(|w| {
                w.downcast_ref::<gtk::MenuButton>()
                    .is_some_and(|m| m.is_active())
                    || w.downcast_ref::<gtk::ToggleButton>()
                        .is_some_and(|t| t.is_active())
            });
            if !playing || busy {
                return;
            }
            for bar in bars.iter().filter_map(|b| b.upgrade()) {
                bar.set_reveal_child(false);
            }
            if let Some(video) = weak_video.upgrade() {
                video.set_cursor_from_name(Some("none"));
            }
        });
        timer.replace(Some(id));
    });
    let motion = gtk::EventControllerMotion::new();
    let last = Rc::new(Cell::new((f64::NAN, f64::NAN)));
    {
        let show = show.clone();
        motion.connect_motion(move |_, x, y| {
            // GTK repeats the pointer's position, give or take a fraction,
            // when widgets change under a still pointer (the bars fading);
            // only a move of a pixel or more counts.
            let (lx, ly) = last.get();
            if lx.is_nan() || (x - lx).abs() >= 1.0 || (y - ly).abs() >= 1.0 {
                last.set((x, y));
                show();
            }
        });
    }
    video.add_controller(motion);
    let click = gtk::GestureClick::new();
    {
        let show = show.clone();
        click.connect_pressed(move |_, _, _, _| show());
    }
    video.add_controller(click);
    // The first countdown runs out before playback has started, finding
    // nothing playing; start it again each time playback starts.
    {
        let (show, page_video) = (show.clone(), video.downgrade());
        playback.subscribe(move |change| {
            if let Change::Playing(true) = change {
                show();
            }
            // Ends with the page.
            page_video.upgrade().is_some()
        });
    }
    show();
}

/// Along the top: back (keeps playing in the bar) and the title.
struct TopBar {
    bar: gtk::Box,
    title: gtk::Label,
    detail: gtk::Label,
}

impl TopBar {
    fn new() -> Self {
        let back = gtk::Button::builder()
            .icon_name(crate::icons::BACK)
            .tooltip_text("Back (Esc) · keeps playing in the bar")
            .valign(gtk::Align::Center)
            .css_classes(["circular", "flat"])
            .build();
        back.connect_clicked(|button| {
            button.activate_action("navigation.pop", None).ok();
        });
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["player-title"])
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        let titles = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        titles.append(&title);
        titles.append(&detail);
        let bar = gtk::Box::builder()
            .spacing(14)
            .css_classes(["player-top"])
            .build();
        bar.append(&back);
        bar.append(&titles);
        Self { bar, title, detail }
    }
}

/// Fullscreen shows the video alone; the window's `fullscreened` property
/// drives the button, so the button, `f`, a double-click, F11 and the
/// window manager agree. Esc leaves fullscreen, and otherwise minimizes:
/// a stray Esc never stops the film. Space plays and pauses, the arrows
/// jump.
///
/// Nothing here may move the page in the widget tree: the window is
/// followed between map and unmap, and moving the page (as an earlier
/// version did, collapsing the split view) remapped it and looped forever.
fn follow_fullscreen(
    page: &adw::NavigationPage,
    video: &gtk::Overlay,
    button: &gtk::ToggleButton,
    actions: &Actions,
) {
    let apply = {
        let button = button.downgrade();
        move |on: bool| {
            if let Some(button) = button.upgrade() {
                button.set_active(on);
                button.set_icon_name(if on {
                    crate::icons::LEAVE_FULLSCREEN
                } else {
                    crate::icons::FULLSCREEN
                });
            }
        }
    };
    let watch: Rc<RefCell<Option<(gtk::Window, glib::SignalHandlerId)>>> = Rc::default();
    {
        let watch = watch.clone();
        let apply = apply.clone();
        page.connect_map(move |page| {
            let Some(window) = page.root().and_downcast::<gtk::Window>() else {
                return;
            };
            apply(window.is_fullscreen());
            let apply = apply.clone();
            let id = window.connect_fullscreened_notify(move |w| apply(w.is_fullscreen()));
            watch.replace(Some((window, id)));
        });
    }
    page.connect_unmap(move |_| {
        if let Some((window, id)) = watch.take() {
            window.disconnect(id);
        }
        apply(false);
    });

    let toggle = |widget: &gtk::Widget| {
        if let Some(window) = widget.root().and_downcast::<gtk::Window>() {
            if window.is_fullscreen() {
                window.unfullscreen();
            } else {
                window.fullscreen();
            }
        }
    };
    // Capture phase: keys reach the navigation view (Esc would go back)
    // only after the player had its say.
    let keys = gtk::EventControllerKey::builder()
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let minimize = actions.minimize.clone();
    let play_pause = button.clone();
    keys.connect_key_pressed(move |keys, key, _, modifiers| {
        let Some(widget) = keys.widget() else {
            return glib::Propagation::Proceed;
        };
        let fullscreen = widget
            .root()
            .and_downcast::<gtk::Window>()
            .is_some_and(|w| w.is_fullscreen());
        if !modifiers.is_empty() {
            return glib::Propagation::Proceed;
        }
        let action = |name: &str| {
            play_pause.activate_action(name, None).ok();
        };
        match key {
            gtk::gdk::Key::f => toggle(&widget),
            gtk::gdk::Key::Escape if fullscreen => toggle(&widget),
            gtk::gdk::Key::Escape => minimize(),
            gtk::gdk::Key::space => action("player.toggle"),
            gtk::gdk::Key::Left => action("player.back"),
            gtk::gdk::Key::Right => action("player.forward"),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    page.add_controller(keys);

    let clicks = gtk::GestureClick::new();
    clicks.connect_pressed(move |gesture, presses, _, _| {
        if presses == 2
            && let Some(widget) = gesture.widget()
        {
            toggle(&widget);
        }
    });
    video.add_controller(clicks);
}

/// Along the bottom: the seek bar between the times, the transport on the
/// left, and tracks, diagnostics and fullscreen on the right.
struct Controls {
    bar: gtk::Box,
    scale: gtk::Scale,
    position: gtk::Label,
    left: gtk::Label,
    toggle: gtk::Button,
    tracks: gtk::MenuButton,
    diagnostics: gtk::ToggleButton,
    fullscreen: gtk::ToggleButton,
    volume: Volume,
}

impl Controls {
    fn new(playback: &Rc<Playback>) -> Self {
        let scale = seek_scale(playback);
        let position = mono("0:00");
        let left = mono("");
        left.add_css_class("dim-label");
        let seek_row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        seek_row.append(&position);
        seek_row.append(&scale);
        seek_row.append(&left);

        let back = round_button("Back 10 seconds", crate::icons::BACK_10, &["flat"]);
        back.set_action_name(Some("player.back"));
        let toggle = round_button("Pause", crate::icons::PAUSE, &["player-play"]);
        toggle.set_action_name(Some("player.toggle"));
        let forward = round_button("Forward 30 seconds", crate::icons::FORWARD_30, &["flat"]);
        forward.set_action_name(Some("player.forward"));
        let transport = gtk::Box::builder().spacing(8).build();
        transport.append(&back);
        transport.append(&toggle);
        transport.append(&forward);

        // The bar is at the bottom of the video: the menu opens upwards,
        // over the picture, never down off the controls.
        let tracks = gtk::MenuButton::builder()
            .direction(gtk::ArrowType::Up)
            .icon_name(crate::icons::TRACKS)
            .tooltip_text("Audio and subtitles")
            .css_classes(["player-square"])
            .build();
        let diagnostics = gtk::ToggleButton::builder()
            .icon_name(crate::icons::DIAGNOSTICS)
            .tooltip_text("Playback diagnostics")
            .css_classes(["player-square"])
            .build();
        let fullscreen = gtk::ToggleButton::builder()
            .icon_name(crate::icons::FULLSCREEN)
            .tooltip_text("Fullscreen")
            .css_classes(["player-square", "flat"])
            .build();
        // The window's state drives the button, not the other way round:
        // see `follow_fullscreen`.
        fullscreen.connect_toggled(|button| {
            let Some(window) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            match (button.is_active(), window.is_fullscreen()) {
                (true, false) => window.fullscreen(),
                (false, true) => window.unfullscreen(),
                _ => {}
            }
        });
        let volume = Volume::of(playback, 120);
        let extras = gtk::Box::builder().spacing(8).build();
        extras.append(&tracks);
        extras.append(&diagnostics);
        extras.append(&fullscreen);

        // As designed: volume at the left, the transport in the middle,
        // tracks, diagnostics and fullscreen at the right.
        extras.set_halign(gtk::Align::End);
        let button_row = gtk::CenterBox::builder()
            .start_widget(&volume.row)
            .center_widget(&transport)
            .end_widget(&extras)
            .build();

        let bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .css_classes(["player-bottom"])
            .build();
        bar.append(&seek_row);
        bar.append(&button_row);
        bar.insert_action_group("player", Some(&transport_actions(playback)));

        Self {
            bar,
            scale,
            position,
            left,
            toggle,
            tracks,
            diagnostics,
            fullscreen,
            volume,
        }
    }

    fn show_playing(&self, playing: bool) {
        self.toggle.set_icon_name(if playing {
            crate::icons::PAUSE
        } else {
            crate::icons::PLAY
        });
        self.toggle
            .set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
    }

    /// Shades what is buffered ahead of the playhead, when the read-ahead
    /// can say it in time.
    fn show_buffered(&self, ready: Option<Duration>) {
        let Some(ready) = ready else { return };
        self.scale
            .set_fill_level(self.scale.value() + ready.as_secs_f64());
    }

    fn show_position(&self, at: Duration, length: Duration) {
        self.scale.adjustment().set_upper(length.as_secs_f64());
        self.scale.set_value(at.as_secs_f64());
        self.position.set_label(&format::timestamp(at));
        self.left.set_label(&format!(
            "−{}",
            format::timestamp(length.saturating_sub(at))
        ));
    }
}

/// `player.toggle`, `player.back` (10 s) and `player.forward` (30 s),
/// shared by the buttons and the keys.
fn transport_actions(playback: &Rc<Playback>) -> gtk::gio::SimpleActionGroup {
    let group = gtk::gio::SimpleActionGroup::new();
    let add = |name: &str, run: Box<dyn Fn(&Playback)>| {
        let action = gtk::gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(playback);
        action.connect_activate(move |_, _| {
            if let Some(playback) = weak.upgrade() {
                run(&playback);
            }
        });
        group.add_action(&action);
    };
    add("toggle", Box::new(|p| p.toggle()));
    add("back", Box::new(|p| p.jump(-10.0)));
    add("forward", Box::new(|p| p.jump(30.0)));
    group
}

/// A seek bar for `playback`; moving it seeks, playback moves it.
fn seek_scale(playback: &Rc<Playback>) -> gtk::Scale {
    let scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 10.0, 0.0))
        .hexpand(true)
        .css_classes(["seek"])
        .show_fill_level(true)
        .restrict_to_fill_level(false)
        .fill_level(0.0)
        .build();
    // change-value fires only for the user's moves, never for set_value.
    let weak = Rc::downgrade(playback);
    scale.connect_change_value(move |_, _, value| {
        if let Some(playback) = weak.upgrade() {
            playback.seek(Duration::from_secs_f64(value.max(0.0)));
        }
        glib::Propagation::Proceed
    });
    scale
}

pub fn round_button(tooltip: &str, icon: &str, classes: &[&str]) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("circular");
    for class in classes {
        button.add_css_class(class);
    }
    button
}

/// What the player is doing, for debugging playback: the path, which
/// decoders run and whether in hardware, the read-ahead and the frames.
#[derive(Clone)]
struct Diagnostics {
    panel: gtk::Box,
    badge: gtk::Label,
    rows: Rc<RefCell<Vec<(&'static str, gtk::Label)>>>,
}

impl Diagnostics {
    const ROWS: [&'static str; 9] = [
        "path",
        "video",
        "decoder",
        "audio",
        "buffer",
        "reads",
        "throughput",
        "frames",
        "video path",
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
        let heading = gtk::Label::builder()
            .label("DIAGNOSTICS")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["caption-heading", "dim-label"])
            .build();
        let badge = gtk::Label::builder()
            .label("Direct")
            .css_classes(["caption-heading", "success"])
            .build();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.append(&heading);
        header.append(&badge);
        let panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .width_request(330)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(80)
            .margin_end(20)
            .visible(false)
            .css_classes(["diagnostics"])
            .build();
        panel.append(&header);
        panel.append(&grid);
        Self {
            panel,
            badge,
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
                format!("{:.1} s of {} s", ready.as_secs_f64(), target.as_secs())
            }
            _ => format!("{} MiB", status.ready_bytes / (1024 * 1024)),
        };
        self.set("buffer", &ahead);
        self.set(
            "reads",
            &format!(
                "{} × {} MiB in flight",
                status.in_flight,
                reader::BLOCK / (1024 * 1024)
            ),
        );
        let throughput = status.throughput.map_or("—".to_owned(), |rate| {
            format!("{:.0} Mbit/s", rate * 8.0 / 1_000_000.0)
        });
        self.set("throughput", &throughput);
    }

    /// The memory frames arrive in at the sink and at GTK (DMABuf and
    /// GLMemory stay on the GPU; plain memory was copied out), and the
    /// frames shown and dropped so far.
    fn show_video(&self, video: &VideoSink) {
        let memory = |element: &gst::Element| {
            let caps = element.static_pad("sink")?.current_caps()?;
            let features = caps.features(0)?;
            Some(
                ["memory:DMABuf", "memory:GLMemory", "memory:VAMemory"]
                    .into_iter()
                    .find(|f| features.contains(*f))
                    .map_or("system memory", |f| f.trim_start_matches("memory:"))
                    .to_owned(),
            )
        };
        let path = match (memory(&video.outer), memory(&video.gtk)) {
            (Some(input), Some(shown)) if video.outer != video.gtk => {
                format!("{input} → GL → {shown}")
            }
            (_, Some(shown)) => shown,
            _ => "—".to_owned(),
        };
        self.set("video path", &path);
        let stats = video.gtk.property::<gst::Structure>("stats");
        let count = |name| stats.get::<u64>(name).unwrap_or(0);
        self.set(
            "frames",
            &format!("{} shown · {} dropped", count("rendered"), count("dropped")),
        );
    }

    fn show_media(&self, info: &gst_play::PlayMediaInfo) {
        self.set("path", "direct · range reads");
        self.badge.set_label("Direct");
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
                &format!("{codec} · {}", tracks::channel_layout(audio.channels())),
            );
        }
    }

    /// Finds the decoders playbin3 picked. VA-API (`va…`), NVIDIA
    /// (`nv…`) and V4L2 decoders run in hardware.
    fn show_decoders(&self, pipeline: &gst::Element) {
        let Some(bin) = pipeline.downcast_ref::<gst::Bin>() else {
            return;
        };
        let mut decoders = Vec::new();
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
                decoders.insert(
                    0,
                    format!(
                        "{name} · {}",
                        if hardware { "hardware" } else { "software" }
                    ),
                );
            } else if klass.contains("Audio") {
                decoders.push(name.to_string());
            }
        }
        if !decoders.is_empty() {
            self.set("decoder", &decoders.join(", "));
        }
    }
}

pub fn mono(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["monospace", "numeric"])
        .build()
}
