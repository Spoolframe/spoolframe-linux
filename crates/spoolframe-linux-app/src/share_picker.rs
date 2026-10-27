//! "Share with someone", from the editor's Add someone: the people the
//! server says the playlist can go to, searched by name or username and
//! shown a page at a time, each with their username and whether they are an account here or
//! a portable user. No board draws it; it is laid out as "Add to
//! playlist" is: a list in a dialog, one click picking.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{BackendError, ShareCandidate, ShareCandidatePage};
use spoolframe_linux_core::playlists::{candidate_line, user_kind_badge};

/// A page of whom the playlist can be shared with, by a search from an
/// offset, asked of the server off the main thread.
pub type People =
    Rc<dyn Fn(String, u32) -> tokio::task::JoinHandle<Result<ShareCandidatePage, BackendError>>>;

/// Shares the playlist with a user id, read only or able to edit.
pub type Pick = Rc<dyn Fn(String, bool)>;

/// The picker's state: what is shown for which search, so an answer for
/// an older search is dropped.
struct Picker {
    people: People,
    list: gtk::ListBox,
    more: gtk::Button,
    status: gtk::Label,
    search: RefCell<String>,
    shown: Cell<u32>,
    asked: Cell<u64>,
    /// Those it is shared with already, listed but not picked again.
    shared: Vec<String>,
    can_edit: gtk::Switch,
    pick: Pick,
    dialog: glib::WeakRef<adw::Dialog>,
}

/// Opens the picker over `parent`. `shared` are the user ids it is shared
/// with now.
pub fn present(parent: &impl IsA<gtk::Widget>, people: People, shared: Vec<String>, pick: Pick) {
    let dialog = adw::Dialog::builder()
        .title("Share with someone")
        .content_width(440)
        .content_height(560)
        .build();
    let column = crate::dialog::column();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Find someone")
        .build();
    column.append(&search);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let status = crate::dialog::dim("");
    status.set_visible(false);
    let more = gtk::Button::builder()
        .label("Show more")
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .visible(false)
        .build();
    let results = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    results.append(&list);
    results.append(&status);
    results.append(&more);
    column.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&results)
            .build(),
    );
    let access = boxed_list();
    let can_edit = gtk::Switch::builder().valign(gtk::Align::Center).build();
    let access_row = adw::ActionRow::builder()
        .use_markup(false)
        .title("Can edit")
        .subtitle("They may add, remove and move its entries")
        .activatable_widget(&can_edit)
        .build();
    access_row.add_suffix(&can_edit);
    access.append(&access_row);
    column.append(&access);
    let cancel = gtk::Button::builder()
        .label("Cancel")
        .halign(gtk::Align::End)
        .build();
    crate::dialog::closes(&cancel, &dialog);
    column.append(&cancel);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));

    let picker = Rc::new(Picker {
        people,
        list,
        more,
        status,
        search: RefCell::default(),
        shown: Cell::new(0),
        asked: Cell::new(0),
        shared,
        can_edit,
        pick,
        dialog: dialog.downgrade(),
    });
    {
        let picker = Rc::downgrade(&picker);
        search.connect_search_changed(move |entry| {
            if let Some(picker) = picker.upgrade() {
                picker.find(entry.text().trim());
            }
        });
    }
    {
        let weak = Rc::downgrade(&picker);
        picker.more.connect_clicked(move |button| {
            if let Some(picker) = weak.upgrade() {
                button.set_sensitive(false);
                picker.ask(picker.shown.get());
            }
        });
    }
    // The dialog keeps the picker for as long as it is open.
    {
        let keep = RefCell::new(Some(picker.clone()));
        dialog.connect_closed(move |_| {
            keep.borrow_mut().take();
        });
    }
    picker.find("");
    dialog.present(Some(parent));
    search.grab_focus();
}

impl Picker {
    /// A new search: the list from its top.
    fn find(self: &Rc<Self>, text: &str) {
        self.search.replace(text.to_owned());
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        self.shown.set(0);
        self.ask(0);
    }

    /// The page from `offset` of the search shown.
    fn ask(self: &Rc<Self>, offset: u32) {
        let asked = self.asked.get() + 1;
        self.asked.set(asked);
        let answer = (self.people)(self.search.borrow().clone(), offset);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = answer.await else { return };
            let Some(picker) = weak.upgrade() else { return };
            // A later search's answer is the one to show.
            if picker.asked.get() != asked {
                return;
            }
            match answer {
                Ok(page) => picker.show(&page),
                Err(e) => picker.say(&format!("Couldn’t list who can see it: {e}")),
            }
        });
    }

    fn show(self: &Rc<Self>, page: &ShareCandidatePage) {
        for candidate in &page.candidates {
            self.list.append(&self.row(candidate));
        }
        let shown = page.offset + page.candidates.len() as u32;
        self.shown.set(shown);
        self.more.set_sensitive(true);
        self.more
            .set_visible(shown < page.total && !page.candidates.is_empty());
        self.list.set_visible(shown > 0);
        if page.total == 0 {
            self.say(match self.search.borrow().is_empty() {
                true => "There is no one else on this server to share with.",
                false => "No one by that name.",
            });
        } else {
            self.status.set_visible(false);
        }
    }

    fn say(&self, text: &str) {
        self.status.set_label(text);
        self.status.set_visible(true);
    }

    /// Someone: their name, username or what they are, and an account
    /// badge. One it is shared with already is greyed.
    fn row(self: &Rc<Self>, candidate: &ShareCandidate) -> adw::ActionRow {
        let shared = self.shared.contains(&candidate.user_id);
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .activatable(!shared)
            .sensitive(!shared)
            .build();
        row.set_title(&candidate.display_name);
        row.set_subtitle(&match shared {
            true => format!("{} · shared with already", candidate_line(candidate)),
            false => candidate_line(candidate),
        });
        if let Some(badge) = user_kind_badge(candidate.kind) {
            row.add_suffix(
                &gtk::Label::builder()
                    .label(badge)
                    .valign(gtk::Align::Center)
                    .css_classes(["user-kind", "caption"])
                    .build(),
            );
        }
        let (picker, user) = (Rc::downgrade(self), candidate.user_id.clone());
        row.connect_activated(move |_| {
            let Some(picker) = picker.upgrade() else {
                return;
            };
            (picker.pick)(user.clone(), picker.can_edit.is_active());
            if let Some(dialog) = picker.dialog.upgrade() {
                dialog.close();
            }
        });
        row
    }
}

fn boxed_list() -> gtk::ListBox {
    gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build()
}
