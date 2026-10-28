//! The library's sidebar: Home, Search, the libraries, with playlists
//! (PX-40) a Playlists row, with downloads (PX-38) a Downloads row, and the
//! user's Pinned under them, as on the LinuxPlaylists,
//! LinuxPinnedPlaylists and LinuxDownloads boards. Each row is known by
//! what it is, not where it sits, so a row more above it moves nothing.
//!
//! With a paired server (P8, the LinuxFed boards), the libraries group by
//! server under the rest: this server's first, the paired ones online,
//! then those offline, greyed and folded. Each group's heading folds and
//! unfolds it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use spoolframe_linux_core::backend::PlaylistSummary;
use spoolframe_linux_core::playlists;
use spoolframe_types::{Library, LibraryKind};

use crate::federation::{self, Group};

/// What a sidebar row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Home,
    Search,
    /// The library at this index in the session's list.
    Library(usize),
    Playlists,
    /// This computer's downloads (PX-38).
    Downloads,
    /// A pinned playlist, by id.
    Pinned(String),
    /// Pinned's hint while nothing is pinned; not a choice.
    NothingPinned,
    /// A server's heading over its libraries (P8): this server's (""),
    /// or a paired one's, by its server id. Picking it folds or unfolds
    /// them.
    Server(String),
}

impl Row {
    fn activatable(&self) -> bool {
        !matches!(self, Row::NothingPinned)
    }
}

/// What the sidebar knows of the session's playlists.
#[derive(Debug, Clone, Copy)]
pub enum Playlists<'a> {
    /// No session (no server): no row for them.
    Off,
    /// Not listed yet: the Playlists row, and Pinned once they are.
    Loading,
    Listed(&'a [PlaylistSummary]),
}

/// Which server groups the user folded or unfolded (P8), by group key;
/// any other is unfolded, but an offline server's, folded (States 1).
#[derive(Debug, Default, Clone)]
pub struct Folds(HashMap<String, bool>);

impl Folds {
    pub fn folded(&self, group: &Group) -> bool {
        self.0.get(&group.key()).copied().unwrap_or(group.offline())
    }

    /// Folds the group `key` names if it is unfolded, and unfolds it if
    /// not.
    pub fn toggle(&mut self, key: &str, libraries: &[Library]) {
        if let Some(group) = federation::groups(libraries)
            .into_iter()
            .find(|g| g.key() == key)
        {
            let folded = self.folded(&group);
            self.0.insert(key.to_owned(), !folded);
        }
    }
}

/// The rows, in order: Home, Search, the libraries, then Playlists,
/// Downloads, and the pinned ones in the user's order. Downloads are this
/// computer's, so their row does not wait on the server. With a paired
/// server's library (P8), the libraries come last instead, each server's
/// under its heading, those folded without them.
pub fn rows(
    libraries: &[Library],
    folds: &Folds,
    playlists: Playlists<'_>,
    downloads: bool,
) -> Vec<Row> {
    let mut rows = vec![Row::Home, Row::Search];
    let grouped = federation::paired(libraries);
    if !grouped {
        rows.extend((0..libraries.len()).map(Row::Library));
    }
    if !matches!(playlists, Playlists::Off) {
        rows.push(Row::Playlists);
    }
    if downloads {
        rows.push(Row::Downloads);
    }
    match playlists {
        Playlists::Off | Playlists::Loading => {}
        Playlists::Listed(list) => {
            let pinned = playlists::pinned(list);
            if pinned.is_empty() {
                rows.push(Row::NothingPinned);
            }
            rows.extend(pinned.into_iter().map(|p| Row::Pinned(p.id.clone())));
        }
    }
    if grouped {
        for group in federation::groups(libraries) {
            rows.push(Row::Server(group.key()));
            if !folds.folded(&group) {
                rows.extend(group.libraries.into_iter().map(Row::Library));
            }
        }
    }
    rows
}

/// What goes over a row.
#[derive(Debug, PartialEq, Eq)]
pub enum Header {
    /// A rule: after Search, and before Playlists (or Downloads without
    /// them).
    Rule,
    /// "Pinned", over the group.
    Pinned,
}

pub fn header(rows: &[Row], index: usize) -> Option<Header> {
    let row = rows.get(index)?;
    let before = index.checked_sub(1).and_then(|i| rows.get(i))?;
    match (before, row) {
        // The servers' groups are ruled off from what is over them.
        (Row::Library(_) | Row::Server(_), Row::Server(_)) => None,
        (_, Row::Server(_)) => Some(Header::Rule),
        (Row::Search, Row::Library(_))
        | (Row::Search | Row::Library(_), Row::Playlists | Row::Downloads) => Some(Header::Rule),
        (Row::Pinned(_), _) => None,
        (_, Row::Pinned(_) | Row::NothingPinned) => Some(Header::Pinned),
        _ => None,
    }
}

/// Where a pinned row's playlist is among the pinned ones.
fn pinned_index(rows: &[Row], index: usize) -> Option<usize> {
    let first = rows.iter().position(|r| matches!(r, Row::Pinned(_)))?;
    matches!(rows.get(index)?, Row::Pinned(_)).then(|| index - first)
}

/// What a pinned row's menu and a drag do.
pub struct PinnedActions {
    /// Plays it (`true`: shuffled).
    pub play: Rc<dyn Fn(String, bool)>,
    pub unpin: Rc<dyn Fn(String)>,
    /// The pinned one at the first index was dropped on the second's row.
    pub dropped: Rc<dyn Fn(usize, usize)>,
}

pub struct Sidebar {
    pub list: gtk::ListBox,
    rows: Rc<RefCell<Vec<Row>>>,
    /// The server groups folded (P8).
    pub folds: RefCell<Folds>,
}

impl Sidebar {
    /// An empty sidebar; `chosen` hears each row the user picks.
    pub fn new(chosen: Rc<dyn Fn(Row)>) -> Self {
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar", "app-sidebar"])
            .build();
        let rows: Rc<RefCell<Vec<Row>>> = Rc::default();
        {
            let rows = rows.clone();
            list.set_header_func(move |row, _| {
                let header = header(&rows.borrow(), row.index() as usize);
                row.set_header(header.map(header_widget).as_ref());
            });
        }
        {
            let rows = rows.clone();
            list.connect_row_activated(move |_, row| {
                let picked = rows.borrow().get(row.index() as usize).cloned();
                if let Some(picked) = picked.filter(Row::activatable) {
                    chosen(picked);
                }
            });
        }
        Sidebar {
            list,
            rows,
            folds: RefCell::default(),
        }
    }

    /// Shows `rows`, keeping the row picked if it is still there.
    pub fn fill(
        &self,
        rows: Vec<Row>,
        // The libraries, and this server's name for its group's heading.
        (libraries, home): (&[Library], &str),
        playlists: &[PlaylistSummary],
        actions: &Rc<PinnedActions>,
        art: Option<&Rc<crate::artwork::Art>>,
    ) {
        let picked = self
            .list
            .selected_row()
            .and_then(|row| self.rows.borrow().get(row.index() as usize).cloned());
        self.list.remove_all();
        for (index, row) in rows.iter().enumerate() {
            let widget = match row {
                Row::Home => plain_row("Home", crate::icons::HOME),
                Row::Search => plain_row("Search", crate::icons::SEARCH),
                Row::Library(i) => {
                    let row = match libraries.get(*i) {
                        Some(library) => plain_row(&library.title, library_icon(library.kind)),
                        None => plain_row("", crate::icons::MOVIES),
                    };
                    // Under its server's heading, set in (P8).
                    if federation::paired(libraries) {
                        row.add_css_class("in-server");
                        if libraries.get(*i).is_some_and(|l| l.server.is_some()) {
                            row.add_css_class("paired");
                        }
                    }
                    row
                }
                Row::Playlists => plain_row("Playlists", crate::icons::PLAYLISTS),
                Row::Downloads => plain_row("Downloads", crate::icons::DOWNLOADS),
                Row::Pinned(id) => {
                    let playlist = playlists.iter().find(|p| p.id == *id);
                    let name = playlist.map_or("", |p| p.name.as_str());
                    let builtin = playlist.and_then(|p| p.builtin);
                    let artwork = playlist.and_then(|p| p.artwork.as_ref());
                    let at = pinned_index(&rows, index).unwrap_or_default();
                    pinned_row(id, name, (builtin, artwork, art), at, actions)
                }
                Row::NothingPinned => hint_row(),
                Row::Server(key) => {
                    let folded = !matches!(rows.get(index + 1), Some(Row::Library(_)));
                    server_row(key, libraries, home, folded)
                }
            };
            self.list.append(&widget);
        }
        *self.rows.borrow_mut() = rows;
        self.list.invalidate_headers();
        if let Some(picked) = picked {
            self.select(&picked);
        }
    }

    /// Shows `row` as the one picked, if it is there.
    pub fn select(&self, row: &Row) {
        let at = self.rows.borrow().iter().position(|r| r == row);
        if let Some(row) = at.and_then(|at| self.list.row_at_index(at as i32)) {
            self.list.select_row(Some(&row));
        }
    }
}

fn header_widget(header: Header) -> gtk::Widget {
    match header {
        Header::Rule => gtk::Separator::builder()
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(4)
            .margin_end(4)
            .build()
            .upcast(),
        Header::Pinned => {
            let heading = gtk::Box::builder()
                .spacing(6)
                .css_classes(["sidebar-group"])
                .build();
            heading.append(
                &gtk::Image::builder()
                    .icon_name(crate::icons::PIN)
                    .pixel_size(11)
                    .build(),
            );
            heading.append(&gtk::Label::new(Some("PINNED")));
            heading.upcast()
        }
    }
}

/// A server's heading (P8): its dot and name, "· this server" for this
/// one, and a chevron for whether it is folded; an offline one greyed,
/// saying so.
fn server_row(key: &str, libraries: &[Library], home: &str, folded: bool) -> gtk::ListBoxRow {
    let server = libraries
        .iter()
        .filter_map(|l| l.server.as_ref())
        .find(|s| s.server_id == key);
    let body = gtk::Box::builder().spacing(8).build();
    body.append(&crate::federation_view::dot(server));
    let name = server.map_or(home, |s| s.name.as_str());
    body.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    if server.is_none() {
        body.append(
            &gtk::Label::builder()
                .label("· this server")
                .css_classes(["dim-label"])
                .build(),
        );
    }
    let offline = server.is_some_and(|s| !s.online);
    let end: gtk::Widget = if offline {
        gtk::Label::builder()
            .label("offline")
            .css_classes(["caption"])
            .build()
            .upcast()
    } else {
        gtk::Image::builder()
            .icon_name(if folded {
                crate::icons::RIGHT
            } else {
                crate::icons::DOWN
            })
            .pixel_size(14)
            .build()
            .upcast()
    };
    end.set_hexpand(true);
    end.set_halign(gtk::Align::End);
    body.append(&end);
    let row = gtk::ListBoxRow::builder()
        .child(&body)
        .selectable(false)
        .tooltip_text(if folded {
            format!("Show {name}’s libraries")
        } else {
            format!("Hide {name}’s libraries")
        })
        .css_classes(["server-heading"])
        .build();
    if offline {
        row.add_css_class("offline");
    }
    row
}

fn plain_row(title: &str, icon: &str) -> gtk::ListBoxRow {
    let body = gtk::Box::builder().spacing(12).build();
    body.append(&gtk::Image::from_icon_name(icon));
    body.append(&gtk::Label::builder().label(title).xalign(0.0).build());
    gtk::ListBoxRow::builder().child(&body).build()
}

fn library_icon(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Movies => crate::icons::MOVIES,
        LibraryKind::Shows => crate::icons::SHOWS,
        LibraryKind::Music => crate::icons::MUSIC,
    }
}

/// Board state 7: what to do to fill the group.
fn hint_row() -> gtk::ListBoxRow {
    let hint = gtk::Label::builder()
        .label("Pin a playlist from its menu or its page to keep it here.")
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(1)
        .hexpand(true)
        .css_classes(["dim-label", "caption"])
        .build();
    gtk::ListBoxRow::builder()
        .child(&hint)
        .activatable(false)
        .selectable(false)
        .css_classes(["pinned-hint"])
        .build()
}

/// A pinned playlist: its picture and name; right-click for Play, Shuffle
/// and Unpin; dragged onto another pinned row, it takes that row's place.
fn pinned_row(
    id: &str,
    name: &str,
    // Its picture: built in or not, the server's, and where it loads from.
    (builtin, artwork, art): (
        Option<spoolframe_linux_core::backend::Builtin>,
        Option<&spoolframe_types::ArtworkId>,
        Option<&Rc<crate::artwork::Art>>,
    ),
    at: usize,
    actions: &Rc<PinnedActions>,
) -> gtk::ListBoxRow {
    let body = gtk::Box::builder().spacing(12).build();
    body.append(&crate::playlists::picture(id, builtin, artwork, art, 22));
    body.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .hexpand(true)
            .build(),
    );
    let row = gtk::ListBoxRow::builder()
        .child(&body)
        .tooltip_text(name)
        .css_classes(["pinned-row"])
        .build();
    let entry = |label: &str, icon, run: Rc<dyn Fn(String)>| {
        let id = id.to_owned();
        crate::views::MenuEntry::new(label, icon, move || run(id.clone()))
    };
    let play = |shuffle: bool| -> Rc<dyn Fn(String)> {
        let play = actions.play.clone();
        Rc::new(move |id| play(id, shuffle))
    };
    let menu = crate::views::menu_popover(vec![
        vec![
            entry("Play", crate::icons::PLAY, play(false)),
            entry("Shuffle", crate::icons::SHUFFLE, play(true)),
        ],
        vec![entry("Unpin", crate::icons::UNPIN, actions.unpin.clone())],
    ]);
    crate::views::context_menu(row.upcast_ref(), &menu);

    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();
    source.connect_prepare(move |_, _, _| {
        Some(gdk::ContentProvider::for_value(&(at as u32).to_value()))
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
    let dropped = actions.dropped.clone();
    target.connect_drop(move |_, value, _, _| match value.get::<u32>() {
        Ok(from) => {
            dropped(from as usize, at);
            true
        }
        Err(_) => false,
    });
    row.add_controller(target);
    row
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::Client;
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::testing::ready;

    use super::*;

    /// `n` of this server's own libraries.
    fn own(n: usize) -> Vec<Library> {
        (0..n).map(|i| library(&i.to_string(), None)).collect()
    }

    fn library(key: &str, server: Option<(&str, bool)>) -> Library {
        Library {
            id: spoolframe_types::LibraryId::new(spoolframe_types::Namespace::Plex, key).unwrap(),
            title: format!("Library {key}"),
            kind: LibraryKind::Movies,
            server: server.map(
                |(id, online)| spoolframe_linux_core::backend::LibraryServer {
                    server_id: id.into(),
                    name: id.into(),
                    online,
                },
            ),
        }
    }

    /// This server's two, Bob's one online, Dave's one offline.
    fn paired() -> Vec<Library> {
        vec![
            library("1", None),
            library("2", Some(("dave", false))),
            library("3", Some(("bob", true))),
            library("4", None),
        ]
    }

    fn listed(client: &FakeClient) -> Vec<PlaylistSummary> {
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        ready(session.playlists()).unwrap().playlists
    }

    #[test]
    fn without_the_feature_there_is_no_playlists_row_or_pinned() {
        assert_eq!(
            rows(&own(2), &Folds::default(), Playlists::Off, false),
            [Row::Home, Row::Search, Row::Library(0), Row::Library(1)]
        );
    }

    #[test]
    fn playlists_follow_the_libraries_with_the_pinned_ones_under_them() {
        let client = FakeClient::new(None);
        let list = listed(&client);
        let heists = list.iter().find(|p| p.pinned.is_some()).unwrap().id.clone();
        assert_eq!(
            rows(&own(1), &Folds::default(), Playlists::Listed(&list), false),
            [
                Row::Home,
                Row::Search,
                Row::Library(0),
                Row::Playlists,
                Row::Pinned(heists)
            ]
        );
    }

    #[test]
    fn pinned_rows_are_in_the_users_order() {
        let client = FakeClient::new(None);
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        let list = listed(&client);
        let night = list.iter().find(|p| p.name == "Movie night").unwrap();
        ready(session.pin_playlist(&night.id, 0)).unwrap();
        let list = listed(&client);
        let pinned: Vec<_> = rows(&[], &Folds::default(), Playlists::Listed(&list), false)
            .into_iter()
            .filter_map(|row| match row {
                Row::Pinned(id) => list.iter().find(|p| p.id == id).map(|p| p.name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(pinned, ["Movie night", "Andor: the heists"]);
    }

    #[test]
    fn nothing_pinned_shows_the_hint_in_its_place() {
        let mut list = listed(&FakeClient::new(None));
        for playlist in &mut list {
            playlist.pinned = None;
        }
        assert_eq!(
            rows(&[], &Folds::default(), Playlists::Listed(&list), false),
            [Row::Home, Row::Search, Row::Playlists, Row::NothingPinned]
        );
    }

    #[test]
    fn pinned_waits_for_the_list_but_the_playlists_row_does_not() {
        assert_eq!(
            rows(&[], &Folds::default(), Playlists::Loading, false),
            [Row::Home, Row::Search, Row::Playlists]
        );
    }

    #[test]
    fn rules_part_the_libraries_and_the_playlists_and_pinned_has_a_heading() {
        let rows = vec![
            Row::Home,
            Row::Search,
            Row::Library(0),
            Row::Library(1),
            Row::Playlists,
            Row::Pinned("a".into()),
            Row::Pinned("b".into()),
        ];
        let headers: Vec<_> = (0..rows.len()).map(|i| header(&rows, i)).collect();
        assert_eq!(
            headers,
            [
                None,
                None,
                Some(Header::Rule),
                None,
                Some(Header::Rule),
                Some(Header::Pinned),
                None
            ]
        );
    }

    #[test]
    fn a_pinned_row_knows_its_place_among_the_pinned() {
        let rows = rows_with_two_pinned();
        assert_eq!(
            (pinned_index(&rows, 4), pinned_index(&rows, 2)),
            (Some(1), None)
        );
    }

    #[test]
    fn downloads_follow_playlists_and_come_before_pinned() {
        let list = listed(&FakeClient::new(None));
        let rows = rows(&[], &Folds::default(), Playlists::Listed(&list), true);
        assert_eq!(
            (rows[2..4].to_vec(), header(&rows, 3), header(&rows, 4)),
            (
                vec![Row::Playlists, Row::Downloads],
                None,
                Some(Header::Pinned)
            )
        );
    }

    #[test]
    fn without_playlists_downloads_are_ruled_off_from_the_libraries() {
        let rows = rows(&own(1), &Folds::default(), Playlists::Off, true);
        assert_eq!(
            (rows.last(), header(&rows, 3)),
            (Some(&Row::Downloads), Some(Header::Rule))
        );
    }

    #[test]
    fn with_a_paired_server_the_libraries_group_by_server_after_the_rest() {
        assert_eq!(
            rows(&paired(), &Folds::default(), Playlists::Off, true),
            [
                Row::Home,
                Row::Search,
                Row::Downloads,
                Row::Server(String::new()),
                Row::Library(0),
                Row::Library(3),
                Row::Server("bob".into()),
                Row::Library(2),
                Row::Server("dave".into()),
            ]
        );
    }

    #[test]
    fn the_server_groups_are_ruled_off_once() {
        let rows = rows(&paired(), &Folds::default(), Playlists::Off, true);
        let rules = (0..rows.len())
            .filter(|&i| header(&rows, i) == Some(Header::Rule))
            .collect::<Vec<_>>();
        // The rule after Search, before Downloads, and the one over the
        // groups.
        assert_eq!(rules, [2, 3]);
    }

    #[test]
    fn an_offline_server_unfolds_when_its_heading_is_picked() {
        let mut folds = Folds::default();
        folds.toggle("dave", &paired());
        let rows = rows(&paired(), &folds, Playlists::Off, false);
        assert_eq!(rows.last(), Some(&Row::Library(1)));
    }

    #[test]
    fn a_server_folded_shows_its_heading_without_its_libraries() {
        let mut folds = Folds::default();
        folds.toggle("bob", &paired());
        let rows = rows(&paired(), &folds, Playlists::Off, false);
        assert!(!rows.contains(&Row::Library(2)));
    }

    fn rows_with_two_pinned() -> Vec<Row> {
        vec![
            Row::Home,
            Row::Search,
            Row::Playlists,
            Row::Pinned("a".into()),
            Row::Pinned("b".into()),
        ]
    }
}
