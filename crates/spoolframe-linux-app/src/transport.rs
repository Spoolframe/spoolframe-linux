//! The pieces the full player, Now Playing and the bar share: the seek
//! bar and the play/pause button's look.

use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

/// A seek bar over the item's length, calling `seek` with where the user
/// moved it. `change-value` fires only for the user's moves, never for
/// [`show_position`]'s.
pub fn seek_scale(seek: impl Fn(Duration) + 'static) -> gtk::Scale {
    let scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 10.0, 0.0))
        .hexpand(true)
        .css_classes(["seek"])
        .build();
    scale.connect_change_value(move |_, _, value| {
        seek(Duration::from_secs_f64(value.max(0.0)));
        glib::Propagation::Proceed
    });
    scale
}

/// Puts `scale` at `at` of `length`.
pub fn show_position(scale: &gtk::Scale, at: Duration, length: Duration) {
    scale.adjustment().set_upper(length.as_secs_f64());
    scale.set_value(at.as_secs_f64());
}

/// Play/pause shows what pressing it does.
pub fn show_playing(toggle: &gtk::Button, playing: bool) {
    toggle.set_icon_name(if playing {
        crate::icons::PAUSE
    } else {
        crate::icons::PLAY
    });
    toggle.set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
}
