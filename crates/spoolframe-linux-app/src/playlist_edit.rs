//! A playlist's editor (PX-40), as the LinuxPlaylistEdit board has it:
//! Cancel and Done over the entries, each with a handle to drag and a
//! Remove; beside them, for whoever manages the playlist, its name, the
//! kinds it holds, who sees it, and Delete. Each change to the entries,
//! kinds or sharing is sent as it is made; the name goes with Done.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use spoolframe_linux_core::backend::{
    EntryId, Kinds, PlaylistEdit, PlaylistEntry, PlaylistPage, PlaylistUpdate, Visibility,
};
use spoolframe_linux_core::playlists::{self, ShareChange};

use crate::playlists::PageActions;

/// "7 items · Drag to reorder · an item may appear twice".
pub fn hint(total: u32) -> String {
    let items = match total {
        1 => "1 item".to_owned(),
        n => format!("{n} items"),
    };
    format!("{items} · Drag to reorder · an item may appear twice")
}

/// The editor's header: its title, the hint, Cancel and Done. Done sends
/// the name typed in `name`, if it changed.
pub fn header(
    page: &PlaylistPage,
    actions: &Rc<PageActions>,
    name: Option<&adw::EntryRow>,
) -> gtk::Box {
    let header = gtk::Box::builder().spacing(10).build();
    let titles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .build();
    titles.append(&label("Edit playlist", &["album-title"]));
    titles.append(&label(&hint(page.total), &["dim-label"]));
    header.append(&titles);
    let cancel = gtk::Button::builder()
        .label("Cancel")
        .valign(gtk::Align::Center)
        .css_classes(["action", "compact", "outlined"])
        .build();
    {
        let edit = actions.edit.clone();
        cancel.connect_clicked(move |_| edit(false));
    }
    let done = gtk::Button::builder()
        .label("Done")
        .valign(gtk::Align::Center)
        .css_classes(["action", "compact", "suggested-action"])
        .build();
    {
        let (finish, name, was) = (
            actions.done.clone(),
            name.cloned(),
            page.playlist.name.clone(),
        );
        done.connect_clicked(move |_| {
            let typed = name.as_ref().map(|n| n.text().trim().to_owned());
            finish(typed.filter(|t| !t.is_empty() && *t != was));
        });
    }
    header.append(&cancel);
    header.append(&done);
    header
}

/// An entry in the editor: its handle, number, title and where it is
/// from, and Remove. Dropped onto another entry, it takes that one's
/// place.
pub fn entry_row(
    number: usize,
    entry: &PlaylistEntry,
    revision: u64,
    ids: &Rc<RefCell<Vec<EntryId>>>,
    actions: &Rc<PageActions>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().use_markup(false).build();
    row.set_title(&playlists::title(&entry.item));
    row.set_subtitle(&playlists::entry_context(&entry.item));
    row.add_prefix(
        &gtk::Image::builder()
            .icon_name(crate::icons::DRAG)
            .tooltip_text("Drag to reorder")
            .css_classes(["dim-label"])
            .build(),
    );
    let count = label(&number.to_string(), &["dim-label", "numeric", "small-text"]);
    count.set_width_chars(3);
    count.set_xalign(1.0);
    row.add_prefix(&count);
    if entry.unavailable {
        row.add_css_class("unavailable");
    }
    let remove = gtk::Button::builder()
        .icon_name(crate::icons::CLOSE)
        .tooltip_text("Remove from playlist")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    {
        let (edit, at) = (actions.edit_entries.clone(), entry.entry);
        remove.connect_clicked(move |_| edit(revision, vec![PlaylistEdit::Remove { entry: at }]));
    }
    row.add_suffix(&remove);
    let index = number - 1;
    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();
    source.connect_prepare(move |_, _, _| {
        Some(gdk::ContentProvider::for_value(&(index as u32).to_value()))
    });
    {
        let row = row.downgrade();
        source.connect_drag_begin(move |source, _| {
            if let Some(row) = row.upgrade() {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
            }
        });
    }
    row.add_controller(source);
    let target = gtk::DropTarget::new(glib::Type::U32, gdk::DragAction::MOVE);
    {
        let (ids, edit) = (ids.clone(), actions.edit_entries.clone());
        target.connect_drop(move |_, value, _, _| {
            let Ok(from) = value.get::<u32>() else {
                return false;
            };
            let moved = playlists::move_edit(&ids.borrow(), from as usize, index);
            if let Some(moved) = moved {
                edit(revision, vec![moved]);
            }
            true
        });
    }
    row.add_controller(target);
    row
}

/// What Sharing offers, from who sees the playlist now.
#[derive(Debug, PartialEq, Eq)]
pub struct Sharing {
    /// Which of Personal, Shared and Server-wide is on.
    pub on: Who,
    /// Shared can be picked: only while someone is in the list, since
    /// this app cannot add anyone (the server lists no users to it).
    pub shared: bool,
    pub server_wide: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Personal,
    Shared,
    ServerWide,
}

pub fn sharing(visibility: &Visibility, admin: bool) -> Sharing {
    let on = match visibility {
        Visibility::Shared { with } if !with.is_empty() => Who::Shared,
        Visibility::ServerWide => Who::ServerWide,
        // Shared with nobody is Personal.
        _ => Who::Personal,
    };
    Sharing {
        on,
        shared: on == Who::Shared,
        server_wide: admin,
    }
}

/// The side panel, for whoever manages the playlist: its name (sent with
/// Done, so `name` is returned for the header), what it holds, who sees
/// it, and Delete.
pub fn panel(
    page: &PlaylistPage,
    actions: &Rc<PageActions>,
    admin: bool,
) -> (gtk::Box, adw::EntryRow) {
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .width_request(320)
        .valign(gtk::Align::Start)
        .build();
    let playlist = &page.playlist;
    panel.append(&label("NAME", &["picker-label"]));
    let names = boxed_list();
    let name = adw::EntryRow::builder()
        .use_markup(false)
        .title("Name")
        .build();
    name.set_text(&playlist.name);
    names.append(&name);
    panel.append(&names);

    panel.append(&label("WHAT IT HOLDS", &["picker-label"]));
    let kinds = boxed_list();
    let now = playlist.kinds;
    let tick = |title: &str, on: bool, widen: fn(Kinds) -> Kinds| {
        let check = gtk::CheckButton::builder()
            .active(on)
            // Ticked stays ticked: kinds only widen.
            .sensitive(!on)
            .build();
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .activatable_widget(&check)
            .sensitive(!on)
            .build();
        row.set_title(title);
        row.add_prefix(&check);
        let update = actions.update.clone();
        check.connect_toggled(move |check| {
            if check.is_active() {
                update(PlaylistUpdate {
                    kinds: Some(widen(now)),
                    ..PlaylistUpdate::default()
                });
            }
        });
        kinds.append(&row);
    };
    tick("Movies", now.movies, |k| Kinds { movies: true, ..k });
    tick("Episodes", now.episodes, |k| Kinds {
        episodes: true,
        ..k
    });
    tick("Music tracks", now.tracks, |k| Kinds { tracks: true, ..k });
    panel.append(&kinds);
    panel.append(&dim(
        "Kinds can be added later, never taken away. An item of a kind not ticked is refused.",
    ));

    panel.append(&label("SHARING", &["picker-label"]));
    panel.append(&sharing_controls(&playlist.visibility, actions, admin));
    if let Visibility::Shared { with } = &playlist.visibility
        && !with.is_empty()
    {
        let people = boxed_list();
        for share in with {
            people.append(&share_row(
                &share.user_id,
                share.can_edit,
                &playlist.visibility,
                actions,
            ));
        }
        panel.append(&people);
        panel.append(&dim("Each sees only the items they may see."));
    }
    panel.append(
        &gtk::Button::builder()
            .label("Add someone")
            .sensitive(false)
            .halign(gtk::Align::Start)
            .tooltip_text("The server can’t list its users to this app yet.")
            .css_classes(["action", "compact", "outlined"])
            .build(),
    );

    let delete = gtk::Button::builder()
        .label("Delete playlist")
        .halign(gtk::Align::Start)
        .margin_top(12)
        .css_classes(["action", "compact", "destructive-action"])
        .build();
    {
        let (delete_it, name) = (actions.delete.clone(), playlist.name.clone());
        delete.connect_clicked(move |button| {
            let alert = adw::AlertDialog::new(
                Some(&format!("Delete {name}?")),
                Some("It goes for everyone it is shared with. The items stay in the library."),
            );
            alert.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
            alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            alert.set_close_response("cancel");
            let delete_it = delete_it.clone();
            alert.connect_response(Some("delete"), move |_, _| delete_it());
            alert.present(Some(button));
        });
    }
    panel.append(&delete);
    (panel, name)
}

/// Personal, Shared and (for an admin) Server-wide, the one on lit.
fn sharing_controls(visibility: &Visibility, actions: &Rc<PageActions>, admin: bool) -> gtk::Box {
    let offered = sharing(visibility, admin);
    let group = gtk::Box::builder()
        .css_classes(["linked"])
        .halign(gtk::Align::Start)
        .build();
    let choice = |title: &str, who: Who, change: Option<ShareChange>| {
        let button = gtk::ToggleButton::builder()
            .label(title)
            .active(offered.on == who)
            .css_classes(["season-tab"])
            .build();
        if who == Who::Shared && !offered.shared {
            button.set_sensitive(false);
            button.set_tooltip_text(Some(
                "The server can’t list its users to this app yet, so no one can be added.",
            ));
        }
        if let Some(change) = change {
            let (update, now) = (actions.update.clone(), visibility.clone());
            button.connect_toggled(move |button| {
                if button.is_active() {
                    update(PlaylistUpdate {
                        visibility: Some(playlists::share_change(&now, &change)),
                        ..PlaylistUpdate::default()
                    });
                }
            });
        }
        group.append(&button);
        button
    };
    let personal = choice("Personal", Who::Personal, Some(ShareChange::Personal));
    let shared = choice("Shared", Who::Shared, None);
    shared.set_group(Some(&personal));
    if offered.server_wide {
        let wide = choice(
            "Server-wide",
            Who::ServerWide,
            Some(ShareChange::ServerWide),
        );
        wide.set_group(Some(&personal));
    }
    group
}

/// Someone the playlist is shared with: their user id (the server gives
/// no names), whether they may edit, and Stop sharing.
fn share_row(
    user: &str,
    can_edit: bool,
    visibility: &Visibility,
    actions: &Rc<PageActions>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().use_markup(false).build();
    row.set_title(user);
    let access = gtk::DropDown::from_strings(&["Read only", "Can edit"]);
    access.set_selected(u32::from(can_edit));
    access.set_valign(gtk::Align::Center);
    {
        let (update, now, user) = (actions.update.clone(), visibility.clone(), user.to_owned());
        access.connect_selected_notify(move |access| {
            let change = ShareChange::CanEdit(user.clone(), access.selected() == 1);
            update(PlaylistUpdate {
                visibility: Some(playlists::share_change(&now, &change)),
                ..PlaylistUpdate::default()
            });
        });
    }
    row.add_suffix(&access);
    let stop = gtk::Button::builder()
        .icon_name(crate::icons::CLOSE)
        .tooltip_text(format!("Stop sharing with {user}"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    {
        let (update, now, user) = (actions.update.clone(), visibility.clone(), user.to_owned());
        stop.connect_clicked(move |_| {
            update(PlaylistUpdate {
                visibility: Some(playlists::share_change(
                    &now,
                    &ShareChange::StopSharing(user.clone()),
                )),
                ..PlaylistUpdate::default()
            });
        });
    }
    row.add_suffix(&stop);
    row
}

fn boxed_list() -> gtk::ListBox {
    gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build()
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(classes)
        .build()
}

fn dim(text: &str) -> gtk::Label {
    let dim = crate::dialog::dim(text);
    dim.add_css_class("caption");
    dim
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::PlaylistShare;

    use super::*;

    fn shared_with(users: &[&str]) -> Visibility {
        Visibility::Shared {
            with: users
                .iter()
                .map(|u| PlaylistShare::new(*u, false))
                .collect(),
        }
    }

    #[test]
    fn shared_with_nobody_reads_as_personal_and_shared_cannot_be_picked() {
        assert_eq!(
            sharing(&shared_with(&[]), false),
            Sharing {
                on: Who::Personal,
                shared: false,
                server_wide: false
            }
        );
    }

    #[test]
    fn shared_with_someone_stays_pickable() {
        assert_eq!(
            sharing(&shared_with(&["local:a"]), true),
            Sharing {
                on: Who::Shared,
                shared: true,
                server_wide: true
            }
        );
    }

    #[test]
    fn the_hint_counts_the_items() {
        assert_eq!(
            (hint(1), hint(7)),
            (
                "1 item · Drag to reorder · an item may appear twice".to_owned(),
                "7 items · Drag to reorder · an item may appear twice".to_owned()
            )
        );
    }
}
