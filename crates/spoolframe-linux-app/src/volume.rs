//! Volume: a mute button showing the speaker, and a slider. One widget
//! for the full player, Now Playing and the bar, each pointing it at its
//! playback.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::icons;
use crate::playback::{Change, Playback};

/// The slider's position as the level the pipeline takes, and back.
/// Loudness is heard roughly logarithmically, so a slider straight onto
/// the linear level put all the quiet settings in its first tenth and
/// everything above sounded loud. The slider is cubic instead, as
/// GStreamer's own `StreamVolumeFormat::Cubic` and desktop mixers are:
/// halfway is about half as loud.
pub fn level_of(position: f64) -> f64 {
    position.clamp(0.0, 1.0).powi(3)
}

pub fn position_of(level: f64) -> f64 {
    level.clamp(0.0, 1.0).cbrt()
}

/// Which playback the control acts on: a fixed one, or the bar's current.
pub type Target = Rc<dyn Fn() -> Option<Rc<Playback>>>;

pub struct Volume {
    pub row: gtk::Box,
    scale: gtk::Scale,
    mute: gtk::Button,
    /// True while the slider is moved by the playback, not the user.
    syncing: Rc<Cell<bool>>,
}

impl Volume {
    pub fn new(target: Target, width: i32) -> Self {
        let syncing = Rc::new(Cell::new(false));
        let mute = gtk::Button::builder()
            .icon_name(icons::VOLUME)
            .tooltip_text("Mute")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        {
            let target = target.clone();
            mute.connect_clicked(move |_| {
                if let Some(playback) = target() {
                    playback.set_muted(!playback.is_muted());
                }
            });
        }
        let scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&gtk::Adjustment::new(1.0, 0.0, 1.0, 0.05, 0.1, 0.0))
            .width_request(width)
            .valign(gtk::Align::Center)
            .tooltip_text("Volume")
            .css_classes(["volume"])
            .build();
        {
            let syncing = syncing.clone();
            scale.connect_value_changed(move |scale| {
                if syncing.get() {
                    return;
                }
                if let Some(playback) = target() {
                    playback.set_volume(level_of(scale.value()));
                    // Turning it up says "I want to hear this".
                    if playback.is_muted() && scale.value() > 0.0 {
                        playback.set_muted(false);
                    }
                }
            });
        }
        let row = gtk::Box::builder()
            .spacing(4)
            .valign(gtk::Align::Center)
            .build();
        row.append(&mute);
        row.append(&scale);
        Self {
            row,
            scale,
            mute,
            syncing,
        }
    }

    /// For a fixed playback.
    pub fn of(playback: &Rc<Playback>, width: i32) -> Self {
        let weak = Rc::downgrade(playback);
        Self::new(Rc::new(move || weak.upgrade()), width)
    }

    /// Follows the playback's volume and mute.
    pub fn show(&self, change: &Change) {
        match change {
            Change::Volume(volume) => {
                self.syncing.set(true);
                self.scale.set_value(position_of(*volume));
                self.syncing.set(false);
            }
            Change::Muted(muted) => {
                self.mute
                    .set_icon_name(if *muted { icons::MUTED } else { icons::VOLUME });
                self.mute
                    .set_tooltip_text(Some(if *muted { "Unmute" } else { "Mute" }));
                self.scale.set_sensitive(!*muted);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halfway_on_the_slider_is_an_eighth_of_the_level() {
        assert!((level_of(0.5) - 0.125).abs() < 1e-9);
    }

    #[test]
    fn the_slider_and_the_level_map_back_and_forth() {
        for position in [0.0, 0.2, 0.5, 0.8, 1.0] {
            assert!((position_of(level_of(position)) - position).abs() < 1e-9);
        }
    }
}
