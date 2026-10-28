//! Federation (P8) as the LinuxFed boards (Browse, Home, Item, Search,
//! States) show it: what is said and how libraries group, apart from the
//! widgets (`federation_view`), so it is tested without a display.
//!
//! A paired server's libraries come with this server's own, each naming
//! its server; its items have `fed:` ids. The server's own name is the
//! one the user signed in to ("Home Media"); a paired server's is what it
//! calls itself ("Bob's Media"). An empty federation (no paired server, or
//! a portable account, which sees nothing federated) is the app as it is
//! without federation: no groups, no badges, no error.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use spoolframe_linux_core::backend::{Copies, LibraryServer, RemoteMode, SearchHit, SearchResults};
use spoolframe_linux_core::format;
use spoolframe_types::{Item, Library, LibraryKind, MediaId};

/// The servers cards and pages name: this one, and each paired server by
/// its server id, as the libraries last listed them.
#[derive(Debug, Default)]
pub struct Servers {
    home: RefCell<String>,
    paired: RefCell<HashMap<String, LibraryServer>>,
}

impl Servers {
    /// This server's name, as the user knows it.
    pub fn set_home(&self, name: &str) {
        self.home.replace(name.to_owned());
    }

    pub fn home(&self) -> String {
        let home = self.home.borrow();
        match home.is_empty() {
            true => "your server".to_owned(),
            false => home.clone(),
        }
    }

    /// The paired servers `libraries` name, in place of those known.
    pub fn learn(&self, libraries: &[Library]) {
        let mut paired = self.paired.borrow_mut();
        paired.clear();
        for server in libraries.iter().filter_map(|l| l.server.as_ref()) {
            paired.insert(server.server_id.clone(), server.clone());
        }
    }

    /// The paired server `id` is from; `None` for this server's own, or a
    /// server not listed.
    pub fn of(&self, id: &MediaId) -> Option<LibraryServer> {
        self.paired.borrow().get(id.origin()?).cloned()
    }

    /// The paired servers online, by name, for the search's note.
    pub fn online(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .paired
            .borrow()
            .values()
            .filter(|s| s.online)
            .map(|s| s.name.clone())
            .collect();
        names.sort();
        names
    }
}

/// One server's libraries in the sidebar: this server's (`server` is
/// `None`) or a paired one's, by their index in the session's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub server: Option<LibraryServer>,
    pub libraries: Vec<usize>,
}

impl Group {
    /// What the sidebar knows the group by: its server id, or "" for
    /// this server.
    pub fn key(&self) -> String {
        self.server
            .as_ref()
            .map_or_else(String::new, |s| s.server_id.clone())
    }

    /// An offline server's group is greyed and folded at the end, not
    /// hidden (States 1).
    pub fn offline(&self) -> bool {
        self.server.as_ref().is_some_and(|s| !s.online)
    }
}

/// Whether any library is a paired server's: the sidebar groups them then.
pub fn paired(libraries: &[Library]) -> bool {
    libraries.iter().any(|l| l.server.is_some())
}

/// The libraries by server: this server's first, then the paired servers
/// online by name, then those offline by name; each group's libraries in
/// the order listed.
pub fn groups(libraries: &[Library]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (index, library) in libraries.iter().enumerate() {
        let id = library.server.as_ref().map(|s| &s.server_id);
        match groups
            .iter_mut()
            .find(|g| g.server.as_ref().map(|s| &s.server_id) == id)
        {
            Some(group) => group.libraries.push(index),
            None => groups.push(Group {
                server: library.server.clone(),
                libraries: vec![index],
            }),
        }
    }
    groups.sort_by_key(|g| match &g.server {
        None => (0, String::new()),
        Some(s) => (if s.online { 1 } else { 2 }, s.name.to_lowercase()),
    });
    groups
}

/// A library's page title in the header: "Bob's Media › Movies" for a
/// paired server's, its own name for this server's.
pub fn library_title(library: &Library) -> String {
    match &library.server {
        Some(server) => format!("{} › {}", server.name, library.title),
        None => library.title.clone(),
    }
}

/// What a kind's items are called where their place is kept.
fn places_in(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Movies => "films",
        LibraryKind::Shows => "episodes",
        LibraryKind::Music => "tracks",
    }
}

/// A paired server's library that is gone from the list while its server
/// is still there: it stopped sharing it (States 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unshared {
    pub server: String,
    pub library: String,
    pub kind: LibraryKind,
}

/// The libraries of `before` no longer shared in `after`: a paired
/// server's, gone while its server is still listed. A server gone whole
/// was unpaired, which is not a library's to say.
pub fn unshared(before: &[Library], after: &[Library]) -> Vec<Unshared> {
    before
        .iter()
        .filter_map(|l| Some((l, l.server.as_ref()?)))
        .filter(|(l, server)| {
            let still_paired = after
                .iter()
                .any(|a| a.server.as_ref().map(|s| &s.server_id) == Some(&server.server_id));
            still_paired && !after.iter().any(|a| a.id == l.id)
        })
        .map(|(l, server)| Unshared {
            server: server.name.clone(),
            library: l.title.clone(),
            kind: l.kind,
        })
        .collect()
}

/// "Bob's Media stopped sharing TV Shows. Your places in its episodes stay
/// on Home Media."
pub fn unshared_text(gone: &Unshared, home: &str) -> String {
    format!(
        "{} stopped sharing {}. Your places in its {} stay on {home}.",
        gone.server,
        gone.library,
        places_in(gone.kind)
    )
}

/// An offline server's card on its item (States 1).
pub fn offline_title(server: &str) -> String {
    format!("{server} is offline")
}

pub fn offline_body(home: &str) -> String {
    format!("You can still browse what {home} keeps of it. Playing works when it’s back.")
}

/// A paired item's Continue watching line (Home, States 5): "9 min left on
/// Bob's Media · your place is kept on Home Media".
pub fn continue_line(left: Option<&str>, server: &str, home: &str) -> String {
    let kept = format!("your place is kept on {home}");
    match left {
        Some(left) => format!("{left} on {server} · {kept}"),
        None => format!("On {server} · {kept}"),
    }
}

/// "Shared by Bob's Media · Movies" over a paired item's title (Item).
pub fn shared_by(server: &str, library: Option<&str>) -> String {
    match library {
        Some(library) => format!("Shared by {server} · {library}"),
        None => format!("Shared by {server}"),
    }
}

/// The library a paired item is likely in: its server's one library of
/// the item's kind, when there is just one.
pub fn library_of<'a>(item: &Item, libraries: &'a [Library]) -> Option<&'a str> {
    let origin = item.id().origin()?;
    let kind = match item {
        Item::Movie(_) => LibraryKind::Movies,
        Item::Show(_) | Item::Season(_) | Item::Episode(_) => LibraryKind::Shows,
        Item::Artist(_) | Item::Album(_) | Item::Track(_) => LibraryKind::Music,
    };
    let mut found = libraries
        .iter()
        .filter(|l| l.kind == kind && l.server.as_ref().is_some_and(|s| s.server_id == origin));
    let first = found.next()?;
    found.next().is_none().then_some(first.title.as_str())
}

/// The Item board's first line: how it plays.
pub fn plays_from_title(server: &str) -> String {
    format!("Plays straight from {server}")
}

pub fn plays_from_body(server: &str, home: &str) -> String {
    format!(
        "Your app connects to {server} directly. If it can’t, {home} fetches it for you; the \
         picture is the same either way."
    )
}

/// The Item board's last line: where the user's place is, once they have
/// one.
pub fn place_kept_title(home: &str) -> String {
    format!("Your place is kept on {home}")
}

pub fn place_kept_body(position: Duration, server: &str) -> String {
    format!(
        "{} shows in Continue watching on every device of yours, and stays if {server} stops \
         sharing.",
        format::timestamp(position)
    )
}

/// After a paired item's play (States 5): "Your place in HalloVeen is kept
/// on Home Media: resume it on any device of yours."
pub fn kept_toast(title: &str, home: &str) -> String {
    format!("Your place in {title} is kept on {home}: resume it on any device of yours.")
}

/// One copy of a title, as the Play from list shows it (States 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The copy's id: what plays when it is picked.
    pub id: MediaId,
    /// Its paired server; `None` for this server's own.
    pub server: Option<LibraryServer>,
    /// The copy that plays now.
    pub picked: bool,
}

impl Choice {
    /// Whether it can play now: this server's, or a paired one online.
    pub fn playable(&self) -> bool {
        self.server.as_ref().is_none_or(|s| s.online)
    }

    pub fn name(&self, home: &str) -> String {
        self.server
            .as_ref()
            .map_or_else(|| home.to_owned(), |s| s.name.clone())
    }
}

/// Every copy of `id`'s title, this server's first, then as the server
/// orders them; none for a title on one server.
pub fn choices(id: &MediaId, copies: &Copies) -> Vec<Choice> {
    if copies.also_on.is_empty() {
        return Vec::new();
    }
    let mut all = vec![Choice {
        id: id.clone(),
        server: copies.origin.clone(),
        picked: false,
    }];
    all.extend(copies.also_on.iter().map(|c| Choice {
        id: c.id.clone(),
        server: c.server.clone(),
        picked: false,
    }));
    all.sort_by_key(|c| c.server.is_some());
    for choice in &mut all {
        choice.picked = choice.id == copies.play_from;
    }
    all
}

/// "Heat is on both servers", or on how many.
pub fn copies_title(title: &str, copies: usize) -> String {
    match copies {
        2 => format!("{title} is on both servers"),
        n => format!("{title} is on {n} servers"),
    }
}

pub fn copies_body(home: &str) -> String {
    format!(
        "Matched by TMDB, IMDb or TVDB IDs. {home}’s copy plays unless you pick another; your \
         pick is remembered for this title."
    )
}

/// `item` as the copy that plays: its own, or the one picked for its
/// title (P8-05), with the same details.
pub fn playing_copy(item: &Item, copies: Option<&Copies>) -> Item {
    let Some(play_from) = copies.map(|c| &c.play_from).filter(|p| *p != item.id()) else {
        return item.clone();
    };
    let mut copy = item.clone();
    match &mut copy {
        Item::Movie(m) => m.id = play_from.clone(),
        Item::Episode(e) => e.id = play_from.clone(),
        _ => return item.clone(),
    }
    copy
}

/// A search hit's line under its title (Search): "on Home Media · also on
/// Bob's Media" for a title on several servers; nothing for one on one
/// server, whose badge says where it is from.
pub fn hit_line(copies: &Copies, home: &str) -> Option<String> {
    if copies.also_on.is_empty() {
        return None;
    }
    let name = |s: Option<&LibraryServer>| s.map_or_else(|| home.to_owned(), |s| s.name.clone());
    let others: Vec<String> = copies
        .also_on
        .iter()
        .map(|c| name(c.server.as_ref()))
        .collect();
    Some(format!(
        "on {} · also on {}",
        name(copies.origin.as_ref()),
        others.join(", ")
    ))
}

/// A search's hits as the results page shows them: their items, and by
/// item where each is from and its title's other copies.
pub fn split_hits(hits: SearchResults<SearchHit>) -> (SearchResults, HashMap<MediaId, Copies>) {
    let mut copies = HashMap::new();
    let results = hits.map(|hit| {
        copies.insert(
            hit.item.id().clone(),
            Copies {
                origin: hit.origin,
                also_on: hit.also_on,
                play_from: hit.play_from,
            },
        );
        hit.item
    });
    (results, copies)
}

/// The note over a search's results (Search): which servers it searched,
/// and that a title on several shows once. `None` with no paired server
/// online.
pub fn search_note(home: &str, paired: &[String]) -> Option<String> {
    let (last, rest) = paired.split_last()?;
    let servers = match rest {
        [] => format!("{home} and {last}"),
        rest => format!("{home}, {} and {last}", rest.join(", ")),
    };
    Some(format!(
        "Searching {servers}. A title on more than one shows once, from {home}; the other copy \
         is one click away."
    ))
}

/// The player's link line for a paired item (Item, States 6): where it
/// plays from.
pub fn path_line(mode: RemoteMode, server: &str, home: &str) -> String {
    match mode {
        RemoteMode::Direct => format!("Straight from {server}"),
        RemoteMode::Proxied => format!("Through {home}"),
    }
}

/// The diagnostics' connection for a paired item (States 6): "via Home
/// Media", or straight from its server.
pub fn path_detail(mode: RemoteMode, server: &str, home: &str) -> String {
    match mode {
        RemoteMode::Direct => format!("direct from {server}"),
        RemoteMode::Proxied => format!("via {home}"),
    }
}

/// What the player says once, when a paired item plays through this
/// server (States 6).
pub fn proxied_note(server: &str, home: &str) -> String {
    format!(
        "{server} can’t be reached directly from here, so {home} fetches it and passes it on. \
         {server} still picks the quality."
    )
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::TitleCopy;
    use spoolframe_types::{LibraryId, MediaKind, Namespace};

    use super::*;

    fn server(id: &str, name: &str, online: bool) -> LibraryServer {
        LibraryServer {
            server_id: id.into(),
            name: name.into(),
            online,
        }
    }

    fn library(key: &str, title: &str, server: Option<LibraryServer>) -> Library {
        Library {
            id: LibraryId::new(Namespace::Plex, key).unwrap(),
            title: title.into(),
            kind: LibraryKind::Movies,
            server,
        }
    }

    fn bob() -> LibraryServer {
        server("bob", "Bob’s Media", true)
    }

    fn movie(key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, MediaKind::Movie, key).unwrap()
    }

    fn bobs(key: &str) -> MediaId {
        MediaId::federated("bob", &movie(key)).unwrap()
    }

    #[test]
    fn this_servers_libraries_come_first_then_paired_ones_online_then_offline() {
        let libraries = [
            library("1", "Movies", None),
            library("2", "Films", Some(server("dave", "Dave’s Den", false))),
            library("3", "Movies", Some(server("zed", "Zed", true))),
            library("4", "Movies", Some(bob())),
            library("5", "TV Shows", None),
        ];
        let order: Vec<Vec<usize>> = groups(&libraries)
            .into_iter()
            .map(|g| g.libraries)
            .collect();
        assert_eq!(order, [vec![0, 4], vec![3], vec![2], vec![1]]);
    }

    #[test]
    fn an_offline_servers_group_is_offline() {
        let libraries = [library("2", "Films", Some(server("dave", "Dave", false)))];
        assert!(groups(&libraries)[0].offline());
    }

    #[test]
    fn without_a_paired_library_nothing_is_grouped_by_server() {
        assert!(!paired(&[library("1", "Movies", None)]));
    }

    #[test]
    fn a_paired_library_is_titled_with_its_server() {
        assert_eq!(
            library_title(&library("1", "Movies", Some(bob()))),
            "Bob’s Media › Movies"
        );
    }

    #[test]
    fn a_library_gone_while_its_server_stays_was_unshared() {
        let shows = Library {
            kind: LibraryKind::Shows,
            ..library("2", "TV Shows", Some(bob()))
        };
        let before = [library("1", "Movies", Some(bob())), shows];
        let after = [library("1", "Movies", Some(bob()))];
        let gone = unshared(&before, &after);
        assert_eq!(
            gone.iter()
                .map(|g| unshared_text(g, "Home Media"))
                .collect::<Vec<_>>(),
            [
                "Bob’s Media stopped sharing TV Shows. Your places in its episodes stay on Home Media."
            ]
        );
    }

    #[test]
    fn a_server_gone_whole_is_not_a_library_unshared() {
        let before = [library("1", "Movies", Some(bob()))];
        assert!(unshared(&before, &[]).is_empty());
    }

    #[test]
    fn a_paired_continue_card_says_where_it_plays_and_where_the_place_is() {
        assert_eq!(
            continue_line(Some("9 min left"), "Bob’s Media", "Home Media"),
            "9 min left on Bob’s Media · your place is kept on Home Media"
        );
    }

    #[test]
    fn the_place_kept_says_the_position_and_whose_sharing_it_outlives() {
        assert_eq!(
            place_kept_body(Duration::from_secs(41 * 60 + 10), "Bob’s Media"),
            "41:10 shows in Continue watching on every device of yours, and stays if Bob’s Media \
             stops sharing."
        );
    }

    #[test]
    fn the_offline_card_says_browsing_still_works() {
        assert_eq!(
            (offline_title("Bob’s Media"), offline_body("Home Media")),
            (
                "Bob’s Media is offline".to_owned(),
                "You can still browse what Home Media keeps of it. Playing works when it’s back."
                    .to_owned()
            )
        );
    }

    fn heat_copies(play_from: MediaId) -> Copies {
        Copies {
            origin: None,
            also_on: vec![TitleCopy {
                id: bobs("17"),
                server: Some(bob()),
            }],
            play_from,
        }
    }

    #[test]
    fn this_servers_copy_is_listed_first_and_the_one_that_plays_is_picked() {
        let listed: Vec<(String, bool)> = choices(&movie("3"), &heat_copies(bobs("17")))
            .iter()
            .map(|c| (c.name("Home Media"), c.picked))
            .collect();
        assert_eq!(
            listed,
            [
                ("Home Media".to_owned(), false),
                ("Bob’s Media".to_owned(), true)
            ]
        );
    }

    #[test]
    fn this_servers_copy_comes_first_from_a_paired_items_page_too() {
        let copies = Copies {
            origin: Some(bob()),
            also_on: vec![TitleCopy {
                id: movie("3"),
                server: None,
            }],
            play_from: bobs("17"),
        };
        let first = choices(&bobs("17"), &copies).first().map(|c| c.id.clone());
        assert_eq!(first, Some(movie("3")));
    }

    #[test]
    fn a_title_on_one_server_has_no_play_from_choice() {
        let copies = Copies {
            origin: Some(bob()),
            also_on: Vec::new(),
            play_from: bobs("11"),
        };
        assert!(choices(&bobs("11"), &copies).is_empty());
    }

    #[test]
    fn a_copy_on_an_offline_server_cannot_be_played() {
        let choice = Choice {
            id: bobs("17"),
            server: Some(server("bob", "Bob’s Media", false)),
            picked: false,
        };
        assert!(!choice.playable());
    }

    #[test]
    fn the_picked_copy_plays_with_the_items_details() {
        let item = Item::Movie(spoolframe_types::Movie {
            id: movie("3"),
            title: "Heat".into(),
            year: Some(1995),
            release_date: None,
            summary: None,
            content_rating: None,
            duration: None,
            cast: Vec::new(),
            artwork: Vec::new(),
            playback: Default::default(),
        });
        let copy = playing_copy(&item, Some(&heat_copies(bobs("17"))));
        assert_eq!(copy.id(), &bobs("17"));
    }

    #[test]
    fn a_hit_on_two_servers_says_both() {
        assert_eq!(
            hit_line(&heat_copies(movie("3")), "Home Media").as_deref(),
            Some("on Home Media · also on Bob’s Media")
        );
    }

    #[test]
    fn a_hits_copies_are_kept_by_its_item() {
        let item = Item::Movie(spoolframe_types::Movie {
            id: movie("3"),
            title: "Heat".into(),
            year: None,
            release_date: None,
            summary: None,
            content_rating: None,
            duration: None,
            cast: Vec::new(),
            artwork: Vec::new(),
            playback: Default::default(),
        });
        let hits = SearchResults {
            without_year: vec![SearchHit::new(item, heat_copies(movie("3")))],
            ..SearchResults::default()
        };
        let (results, copies) = split_hits(hits);
        assert_eq!(
            (
                results.without_year.len(),
                copies.get(&movie("3")).map(|c| c.also_on.len())
            ),
            (1, Some(1))
        );
    }

    #[test]
    fn the_search_names_the_servers_it_searched() {
        assert_eq!(
            search_note("Home Media", &["Bob’s Media".to_owned()]).as_deref(),
            Some(
                "Searching Home Media and Bob’s Media. A title on more than one shows once, from \
                 Home Media; the other copy is one click away."
            )
        );
    }

    #[test]
    fn with_no_paired_server_online_the_search_has_no_note() {
        assert_eq!(search_note("Home Media", &[]), None);
    }

    #[test]
    fn a_play_through_this_server_says_so() {
        assert_eq!(
            (
                path_line(RemoteMode::Proxied, "Bob’s Media", "Home Media"),
                path_detail(RemoteMode::Proxied, "Bob’s Media", "Home Media"),
            ),
            ("Through Home Media".to_owned(), "via Home Media".to_owned())
        );
    }

    #[test]
    fn a_direct_play_names_the_paired_server() {
        assert_eq!(
            path_line(RemoteMode::Direct, "Bob’s Media", "Home Media"),
            "Straight from Bob’s Media"
        );
    }

    #[test]
    fn the_servers_known_name_a_paired_item_and_not_this_servers_own() {
        let servers = Servers::default();
        servers.learn(&[
            library("1", "Movies", None),
            library("2", "Movies", Some(bob())),
        ]);
        assert_eq!(
            (
                servers.of(&bobs("11")).map(|s| s.name),
                servers.of(&movie("1"))
            ),
            (Some("Bob’s Media".to_owned()), None)
        );
    }

    #[test]
    fn a_paired_items_library_is_its_servers_one_of_its_kind() {
        let libraries = [
            library("1", "Movies", None),
            library("2", "Films", Some(bob())),
        ];
        let item = Item::Movie(spoolframe_types::Movie {
            id: bobs("11"),
            title: "In the Heat of the Night".into(),
            year: None,
            release_date: None,
            summary: None,
            content_rating: None,
            duration: None,
            cast: Vec::new(),
            artwork: Vec::new(),
            playback: Default::default(),
        });
        assert_eq!(library_of(&item, &libraries), Some("Films"));
    }
}
