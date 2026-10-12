//! What the display tests share: run the main loop until something holds,
//! and find a row by its title.

use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

/// Runs the main loop until `done` holds, or `limit` passes; whether it
/// held.
pub fn spin_until(limit: Duration, done: impl Fn() -> bool) -> bool {
    let until = Instant::now() + limit;
    let context = glib::MainContext::default();
    while Instant::now() < until {
        if done() {
            return true;
        }
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

/// The first row under `root` titled `title`.
pub fn row_titled(root: &gtk::Widget, title: &str) -> Option<adw::ActionRow> {
    if let Some(row) = root.downcast_ref::<adw::ActionRow>()
        && row.title() == title
    {
        return Some(row.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = row_titled(&widget, title) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}
