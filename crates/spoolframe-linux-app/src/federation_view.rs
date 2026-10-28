//! The widgets federation (P8) adds, as the LinuxFed boards draw them:
//! a paired server's badge, the notes on a paired item's page, and its
//! Play from list. What they say is `federation`'s.

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use spoolframe_linux_core::backend::LibraryServer;

use crate::federation::{self, Choice};

/// A paired server's name in a pill, its dot before it: by a library's
/// title, over a row, and (`on_tile`) on a poster's corner.
pub fn badge(server: &LibraryServer, on_tile: bool) -> gtk::Box {
    let pill = gtk::Box::builder()
        .spacing(5)
        .valign(gtk::Align::Center)
        .css_classes(["server-badge"])
        .tooltip_text(match server.online {
            true => format!("From {}", server.name),
            false => format!("From {}, offline", server.name),
        })
        .build();
    if on_tile {
        pill.add_css_class("on-tile");
        pill.set_halign(gtk::Align::Start);
        pill.set_valign(gtk::Align::Start);
        pill.set_margin_start(8);
        pill.set_margin_top(8);
    }
    if !server.online {
        pill.add_css_class("offline");
    }
    pill.append(&dot(Some(server)));
    pill.append(&gtk::Label::new(Some(&server.name)));
    pill
}

/// A server's dot: the accent for this server, the paired servers'
/// colour for one online, grey for one offline.
pub fn dot(server: Option<&LibraryServer>) -> gtk::Box {
    let dot = gtk::Box::builder()
        .width_request(8)
        .height_request(8)
        .valign(gtk::Align::Center)
        .css_classes(["server-dot"])
        .build();
    match server {
        None => dot.add_css_class("home"),
        Some(s) if !s.online => dot.add_css_class("offline"),
        Some(_) => {}
    }
    dot
}

/// The badge over `tile`, for a paired server's item.
pub fn badge_tile(tile: &gtk::Overlay, server: Option<&LibraryServer>) {
    if let Some(server) = server {
        tile.add_overlay(&badge(server, true));
    }
}

/// One of the boards' notes: an icon, a bold line, and what it means.
pub fn note(icon: &str, title: &str, body: &str) -> gtk::Box {
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .hexpand(true)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    text.append(
        &gtk::Label::builder()
            .label(body)
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(60)
            .css_classes(["dim-label"])
            .build(),
    );
    let row = gtk::Box::builder()
        .spacing(10)
        .css_classes(["fed-note"])
        .build();
    row.append(
        &gtk::Image::builder()
            .icon_name(icon)
            .valign(gtk::Align::Start)
            .build(),
    );
    row.append(&text);
    row
}

/// What a paired or several-server item's page shows under its buttons.
pub struct ItemNotes<'a> {
    /// The paired server it is from; `None` for this server's own.
    pub server: Option<&'a LibraryServer>,
    pub home: &'a str,
    /// Where the user got to in it, if anywhere.
    pub position: Option<Duration>,
    pub title: &'a str,
    /// Its title's copies (States 4); empty for a title on one server.
    pub choices: Vec<Choice>,
    /// Plays the title from a paired server (by id), or this one.
    pub pick: Rc<dyn Fn(Option<String>)>,
}

/// The notes of an item's page (Item, States 1 and 4): an offline
/// server's card, or how it plays; where the place is kept; and the Play
/// from list. `None` when there is nothing to say: this server's own item
/// on one server.
pub fn item_notes(notes: ItemNotes<'_>) -> Option<gtk::Box> {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .css_classes(["fed-notes"])
        .build();
    let home = notes.home;
    if let Some(server) = notes.server {
        if server.online {
            column.append(&note(
                crate::icons::CONNECTION,
                &federation::plays_from_title(&server.name),
                &federation::plays_from_body(&server.name, home),
            ));
        } else {
            column.append(&note(
                crate::icons::SERVER,
                &federation::offline_title(&server.name),
                &federation::offline_body(home),
            ));
        }
        if let Some(position) = notes.position {
            column.append(&note(
                crate::icons::HISTORY,
                &federation::place_kept_title(home),
                &federation::place_kept_body(position, &server.name),
            ));
        }
    }
    if !notes.choices.is_empty() {
        column.append(&play_from(&notes));
    }
    column.first_child().is_some().then_some(column)
}

/// "Heat is on both servers", and a row for each copy to play from: the
/// one picked ticked, one on an offline server not to be picked.
fn play_from(notes: &ItemNotes<'_>) -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    section.append(&note(
        crate::icons::SHARED,
        &federation::copies_title(notes.title, notes.choices.len()),
        &federation::copies_body(notes.home),
    ));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let mut group: Option<gtk::CheckButton> = None;
    for choice in &notes.choices {
        let check = gtk::CheckButton::builder()
            .active(choice.picked)
            .valign(gtk::Align::Center)
            .build();
        if let Some(first) = &group {
            check.set_group(Some(first));
        } else {
            group = Some(check.clone());
        }
        // A server's name is its own words: never markup.
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .activatable_widget(&check)
            .sensitive(choice.playable())
            .build();
        row.set_title(&choice.name(notes.home));
        if !choice.playable() {
            row.set_subtitle("Offline");
        }
        row.add_prefix(&check);
        row.add_prefix(&dot(choice.server.as_ref()));
        let (pick, from) = (
            notes.pick.clone(),
            choice.server.as_ref().map(|s| s.server_id.clone()),
        );
        check.connect_toggled(move |check| {
            if check.is_active() {
                pick(from.clone());
            }
        });
        list.append(&row);
    }
    section.append(&list);
    section
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use spoolframe_types::{MediaId, MediaKind, Namespace};

    use super::*;

    fn server(online: bool) -> LibraryServer {
        LibraryServer {
            server_id: "bob".into(),
            name: "Bob’s Media".into(),
            online,
        }
    }

    fn heat(on_bobs: bool) -> MediaId {
        let own = MediaId::new(Namespace::Plex, MediaKind::Movie, "3").unwrap();
        match on_bobs {
            true => MediaId::federated("bob", &own).unwrap(),
            false => own,
        }
    }

    fn notes(server_online: bool, picked: &Rc<RefCell<Vec<Option<String>>>>) -> gtk::Box {
        let bob = server(server_online);
        let choices = vec![
            Choice {
                id: heat(false),
                server: None,
                picked: true,
            },
            Choice {
                id: heat(true),
                server: Some(bob.clone()),
                picked: false,
            },
        ];
        let picked = picked.clone();
        item_notes(ItemNotes {
            server: Some(&bob),
            home: "Home Media",
            position: None,
            title: "Heat",
            choices,
            pick: Rc::new(move |from| picked.borrow_mut().push(from)),
        })
        .expect("a paired item has notes")
    }

    /// Every label's text under `root`.
    fn texts(root: &gtk::Widget) -> Vec<String> {
        let mut found = Vec::new();
        if let Some(label) = root.downcast_ref::<gtk::Label>() {
            found.push(label.label().to_string());
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            found.extend(texts(&widget));
            child = widget.next_sibling();
        }
        found
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn picking_a_paired_copy_plays_the_title_from_its_server() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let picked = Rc::default();
        let notes = notes(true, &picked);
        let row = crate::ui_test::row_titled(notes.upcast_ref(), "Bob’s Media").unwrap();
        let check = row
            .activatable_widget()
            .and_downcast::<gtk::CheckButton>()
            .unwrap();
        check.set_active(true);
        assert_eq!(*picked.borrow(), [Some("bob".to_owned())]);
    }

    #[test]
    #[ignore = "needs a display; run on its own"]
    fn an_offline_servers_item_says_so_and_its_copy_is_not_to_be_picked() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let notes = notes(false, &Rc::default());
        let row = crate::ui_test::row_titled(notes.upcast_ref(), "Bob’s Media").unwrap();
        assert_eq!(
            (
                texts(notes.upcast_ref()).contains(&"Bob’s Media is offline".to_owned()),
                row.is_sensitive(),
            ),
            (true, false)
        );
    }
}
