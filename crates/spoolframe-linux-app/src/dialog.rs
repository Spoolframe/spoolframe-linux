//! What the app's small dialogs are built from: a padded column, a
//! dimmed paragraph, a row of buttons at the end, and the view a dialog
//! owns while it shows.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

/// A dialog's body: a column padded from the dialog's edges.
pub fn column() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build()
}

/// A dimmed paragraph, aligned to the start.
pub fn dim(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build()
}

/// A row of buttons at the end of a dialog.
pub fn buttons() -> gtk::Box {
    gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .build()
}

/// `button` closes `dialog`.
pub fn closes(button: &gtk::Button, dialog: &adw::Dialog) {
    let dialog = dialog.downgrade();
    button.connect_clicked(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
}

/// `dialog` owns `view` until it closes, then lets it go. Its handlers
/// hold the view weakly, and the view may hold the dialog: held from a
/// handler for good, the two would keep each other alive.
pub fn keep_until_closed<T: 'static>(dialog: &adw::Dialog, view: Rc<T>) {
    let held = RefCell::new(Some(view));
    dialog.connect_closed(move |_| {
        held.borrow_mut().take();
    });
}
