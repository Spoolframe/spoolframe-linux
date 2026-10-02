//! The bar along the bottom of the window while the player is minimized,
//! as in the design: what plays (the live picture for video, the cover for
//! music; a click opens the full player), the transport with a seek bar,
//! and volume, expand and stop. The library stays usable above it.
//!
//! The bar is built once; its buttons act on whichever playback is
//! attached.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::ArtworkSize;
use spoolframe_linux_core::format;
use spoolframe_types::{ArtworkKind, Item};

use crate::nowplaying::Transport;
use crate::playback::{Change, Playback};
use crate::player::{mono, round_button};
use crate::views::Cards;

/// The live picture's size in the bar.
const THUMB: (i32, i32) = (128, 72);
/// A cover's size in the bar.
const COVER: i32 = 52;

type Current = Rc<RefCell<Option<Rc<Playback>>>>;

pub struct MiniBar {
    /// Hidden while nothing plays.
    pub revealer: gtk::Revealer,
    thumb: adw::Bin,
    title: gtk::Label,
    detail: gtk::Label,
    /// Video's transport: 10 s back, play, 30 s forward.
    video_transport: gtk::Box,
    toggle: gtk::Button,
    /// Music's, made for each playback: shuffle, previous, play, next,
    /// repeat.
    music_slot: adw::Bin,
    music: RefCell<Option<Transport>>,
    scale: gtk::Scale,
    position: gtk::Label,
    length: gtk::Label,
    volume: gtk::Scale,
    current: Current,
    /// Bumped on each attach and detach, so an old playback's listener
    /// lets go.
    generation: Rc<Cell<u64>>,
}

impl MiniBar {
    /// `expand` opens the full player; `stop` ends playback.
    pub fn new(expand: Rc<dyn Fn()>, stop: Rc<dyn Fn()>) -> Rc<Self> {
        let current: Current = Rc::default();
        let on = |current: &Current, run: fn(&Rc<Playback>)| {
            let current = current.clone();
            move || {
                let playback = current.borrow().clone();
                if let Some(playback) = playback {
                    run(&playback);
                }
            }
        };

        let thumb = adw::Bin::builder()
            .valign(gtk::Align::Center)
            .css_classes(["mini-thumb"])
            .build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        let titles = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build();
        titles.append(&title);
        titles.append(&detail);
        let what = gtk::Box::builder().spacing(12).build();
        what.append(&thumb);
        what.append(&titles);
        // The picture and the titles open the full player.
        let open = gtk::Button::builder()
            .child(&what)
            .tooltip_text("Back to the full player")
            .width_request(320)
            .css_classes(["flat", "mini-open"])
            .build();
        {
            let expand = expand.clone();
            open.connect_clicked(move |_| expand());
        }

        let back = round_button("Back 10 seconds", crate::icons::BACK_10, &["flat"]);
        let toggle = round_button("Pause", crate::icons::PAUSE, &["player-play"]);
        let forward = round_button("Forward 30 seconds", crate::icons::FORWARD_30, &["flat"]);
        let wire = |button: &gtk::Button, run: fn(&Rc<Playback>)| {
            let run = on(&current, run);
            button.connect_clicked(move |_| run());
        };
        wire(&back, |p| p.jump(-10.0));
        wire(&toggle, |p| p.toggle());
        wire(&forward, |p| p.jump(30.0));
        let video_transport = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .build();
        for button in [&back, &toggle, &forward] {
            video_transport.append(button);
        }
        let music_slot = adw::Bin::builder().halign(gtk::Align::Center).build();

        let scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 10.0, 0.0))
            .hexpand(true)
            .css_classes(["seek"])
            .build();
        {
            // change-value fires only for the user's moves.
            let current = current.clone();
            scale.connect_change_value(move |_, _, value| {
                if let Some(playback) = current.borrow().as_ref() {
                    playback.seek(Duration::from_secs_f64(value.max(0.0)));
                }
                glib::Propagation::Proceed
            });
        }
        let position = mono("0:00");
        let length = mono("0:00");
        for label in [&position, &length] {
            label.add_css_class("dim-label");
            label.add_css_class("caption");
        }
        let seek_row = gtk::Box::builder().spacing(10).width_request(520).build();
        seek_row.append(&position);
        seek_row.append(&scale);
        seek_row.append(&length);
        let centre = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .valign(gtk::Align::Center)
            .build();
        centre.append(&video_transport);
        centre.append(&music_slot);
        centre.append(&seek_row);

        let volume = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&gtk::Adjustment::new(1.0, 0.0, 1.0, 0.05, 0.1, 0.0))
            .width_request(96)
            .tooltip_text("Volume")
            .build();
        {
            let current = current.clone();
            volume.connect_change_value(move |_, _, value| {
                if let Some(playback) = current.borrow().as_ref() {
                    playback.set_volume(value);
                }
                glib::Propagation::Proceed
            });
        }
        let expand_button = gtk::Button::builder()
            .icon_name(crate::icons::UP)
            .tooltip_text("Back to the full player")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        expand_button.connect_clicked(move |_| expand());
        let stop_button = gtk::Button::builder()
            .icon_name(crate::icons::CLOSE)
            .tooltip_text("Stop")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        stop_button.connect_clicked(move |_| stop());
        let end = gtk::Box::builder()
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        end.append(
            &gtk::Image::builder()
                .icon_name(crate::icons::VOLUME)
                .css_classes(["dim-label"])
                .build(),
        );
        end.append(&volume);
        end.append(&expand_button);
        end.append(&stop_button);

        let bar = gtk::CenterBox::builder()
            .start_widget(&open)
            .center_widget(&centre)
            .end_widget(&end)
            .css_classes(["mini-bar"])
            .build();
        let revealer = gtk::Revealer::builder()
            .child(&bar)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .reveal_child(false)
            .build();
        Rc::new(Self {
            revealer,
            thumb,
            title,
            detail,
            video_transport,
            toggle,
            music_slot,
            music: RefCell::new(None),
            scale,
            position,
            length,
            volume,
            current,
            generation: Rc::new(Cell::new(0)),
        })
    }

    /// Shows `playback` in the bar. `cards` draws a music cover.
    pub fn attach(self: &Rc<Self>, playback: &Rc<Playback>, cards: &Cards) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.current.replace(Some(playback.clone()));

        let video = playback.is_video();
        if video {
            let picture = gtk::Picture::builder()
                .paintable(playback.paintable())
                .content_fit(gtk::ContentFit::Cover)
                .width_request(THUMB.0)
                .height_request(THUMB.1)
                .css_classes(["mini-video"])
                .build();
            self.thumb.set_child(Some(&picture));
        } else if let Some(item) = playback.current() {
            let cover = crate::views::tile(item.id(), "", "mini-cover", (COVER, COVER));
            if let Some(album) = playback.album() {
                cards.art(
                    &cover,
                    &album.artwork,
                    &[ArtworkKind::Poster, ArtworkKind::Thumb],
                    ArtworkSize::Small,
                );
            }
            self.thumb.set_child(Some(&cover));
        }
        // Video jumps within the film; music skips between tracks, with
        // shuffle and repeat.
        self.video_transport.set_visible(video);
        let music = (!video).then(|| Transport::new(playback, false));
        self.music_slot.set_child(music.as_ref().map(|t| &t.row));
        self.music.replace(music);

        let this = Rc::downgrade(self);
        let weak_playback = Rc::downgrade(playback);
        playback.subscribe(move |change| {
            let (Some(bar), Some(playback)) = (this.upgrade(), weak_playback.upgrade()) else {
                return false;
            };
            if bar.generation.get() != generation {
                return false;
            }
            bar.update(&playback, change);
            true
        });
        self.revealer.set_reveal_child(true);
    }

    /// Hides the bar and lets go of the playback.
    pub fn detach(&self) {
        self.generation.set(self.generation.get() + 1);
        self.current.replace(None);
        self.thumb.set_child(None::<&gtk::Widget>);
        self.music_slot.set_child(None::<&gtk::Widget>);
        self.music.replace(None);
        self.revealer.set_reveal_child(false);
    }

    fn update(&self, playback: &Playback, change: &Change) {
        match change {
            Change::Item(item) => {
                let facts = crate::views::Facts::of(item);
                self.title.set_label(&facts.title);
                let detail = match (item, playback.album()) {
                    (Item::Track(_), Some(album)) => Some(
                        [album.artist_name.clone(), Some(album.title.clone())]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                    _ => facts.detail.clone(),
                };
                self.detail.set_label(detail.as_deref().unwrap_or_default());
                if let Some(music) = self.music.borrow().as_ref() {
                    music.show_item(playback);
                }
            }
            Change::Playing(playing) => {
                self.toggle.set_icon_name(if *playing {
                    crate::icons::PAUSE
                } else {
                    crate::icons::PLAY
                });
                self.toggle
                    .set_tooltip_text(Some(if *playing { "Pause" } else { "Play" }));
                if let Some(music) = self.music.borrow().as_ref() {
                    music.show_playing(*playing);
                }
            }
            Change::Modes { shuffle, repeat } => {
                if let Some(music) = self.music.borrow().as_ref() {
                    music.show_modes(*shuffle, *repeat);
                }
            }
            Change::Position { at, length } => {
                self.scale.adjustment().set_upper(length.as_secs_f64());
                self.scale.set_value(at.as_secs_f64());
                self.position.set_label(&format::timestamp(*at));
                self.length.set_label(&format::timestamp(*length));
            }
            Change::Volume(volume) => self.volume.set_value(*volume),
            Change::Starting
            | Change::MediaInfo(_)
            | Change::Buffering(_)
            | Change::Failed(_)
            | Change::Queue => {}
        }
    }
}
