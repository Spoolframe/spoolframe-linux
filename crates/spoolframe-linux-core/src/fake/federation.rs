//! Federation (P8) on the fake server, turned on by
//! `FakeClient::with_federation`: Home Media is paired with Bob's Media,
//! online, and Dave's Den, offline, as on the LinuxFed boards. Their
//! libraries come with Home Media's own, their items under `fed:` ids;
//! Heat, Arrival and Moon are on Bob's Media too, the same titles as Home
//! Media's (one hit in a search, a Play from choice on the item).
//!
//! As the 0.16.1 server does, a portable account (the fake's guest) sees
//! nothing federated: no paired server's library, Home row or search hit,
//! and `NotFound` for a `fed:` id. A test moves the rest: a server going
//! offline or coming back (`FakeClient::set_paired_online`), a library no
//! longer shared (`FakeClient::stop_sharing`), and a device that cannot
//! reach a paired server itself, whose plays go through Home Media
//! (`FakeClient::without_direct_path`). Each change is told on
//! `Client::libraries_events`, as the SDK tells it.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use spoolframe_types::{
    Episode, Item, Library, LibraryId, LibraryKind, LibraryServer, MediaId, MediaKind, Movie,
    Namespace, PlaybackState, Season, Show,
};
use tokio::sync::broadcast;

use super::{Catalog, media};
use crate::backend::{Copies, LibrariesChanged, PlaybackHandle, RemoteMode, TitleCopy};

/// Bob's Media's server id: paired, online, sharing Movies and TV Shows.
pub const BOBS_MEDIA: &str = "bobsmediaserver2";
/// Dave's Den's server id: paired, offline, sharing Movies.
pub const DAVES_DEN: &str = "davesdenserver22";

/// The paired servers and what they share, as the catalog has them.
#[derive(Default)]
pub(super) struct Paired {
    /// Each paired server as it calls itself, online to begin with but
    /// Dave's Den: the store says which is online now.
    pub servers: Vec<LibraryServer>,
    /// The library each paired server's item is in, its children's too.
    pub library_of: HashMap<MediaId, LibraryId>,
    /// The same titles on two servers: (Home Media's, the paired one's).
    pub twins: Vec<(MediaId, MediaId)>,
    /// What each paired server shared lately, newest first: its Home row.
    pub recent: HashMap<String, Vec<MediaId>>,
}

/// What changes as the fake runs, for every session.
pub struct Store {
    /// `FakeClient::with_federation`: the server turned federation on.
    pub on: bool,
    /// Paired servers offline to Home Media.
    pub offline: HashSet<String>,
    /// Libraries their server stopped sharing.
    pub unshared: HashSet<LibraryId>,
    /// This device reaches paired servers itself (P8-04); else its plays
    /// go through Home Media.
    pub direct: bool,
    /// The server each title plays from (P8-05), by Home Media's copy:
    /// `None` for Home Media's own.
    pub play_from: HashMap<MediaId, Option<String>>,
    /// How each paired server's item now playing plays.
    pub modes: HashMap<PlaybackHandle, RemoteMode>,
    changes: broadcast::Sender<LibrariesChanged>,
}

impl Store {
    pub fn new() -> Self {
        Self {
            on: false,
            offline: [DAVES_DEN.to_owned()].into(),
            unshared: HashSet::new(),
            direct: true,
            play_from: HashMap::new(),
            modes: HashMap::new(),
            changes: broadcast::channel(16).0,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LibrariesChanged> {
        self.changes.subscribe()
    }

    /// Tells that Home Media's libraries changed, as the SDK does.
    pub fn changed(&self, server: &str) {
        // Nobody listening is fine: nothing shows them.
        let _ = self.changes.send(LibrariesChanged {
            server: server.to_owned(),
        });
    }

    /// `server` as it is now: online or not.
    pub fn now(&self, server: &LibraryServer) -> LibraryServer {
        LibraryServer {
            online: !self.offline.contains(&server.server_id),
            ..server.clone()
        }
    }
}

/// `key` of a paired server's (`origin`'s) catalog, as Home Media names
/// it: a `fed:` id.
fn fed(origin: &str, kind: MediaKind, key: &str) -> MediaId {
    MediaId::federated(origin, &media(kind, key)).expect("fixture ids are ids")
}

fn fed_library(origin: &str, key: &str) -> LibraryId {
    let own = LibraryId::new(Namespace::Plex, key).expect("fixture keys are not empty");
    LibraryId::federated(origin, &own).expect("fixture ids are ids")
}

fn film(id: MediaId, title: &str, year: u16, minutes: u64) -> Item {
    Item::Movie(Movie {
        id,
        title: title.into(),
        year: Some(year),
        summary: None,
        duration: Some(Duration::from_secs(minutes * 60)),
        artwork: Vec::new(),
        playback: PlaybackState::default(),
        content_rating: Some("R".into()),
        cast: Vec::new(),
        release_date: None,
    })
}

impl Catalog {
    /// The fake's catalog with Bob's Media and Dave's Den paired.
    pub(super) fn federated() -> Self {
        let mut catalog = Catalog::new();
        catalog.add_paired();
        catalog
    }

    fn add_paired(&mut self) {
        let server = |id: &str, name: &str| LibraryServer {
            server_id: id.into(),
            name: name.into(),
            online: true,
        };
        let bob = server(BOBS_MEDIA, "Bob’s Media");
        let dave = server(DAVES_DEN, "Dave’s Den");

        // Bob's films, newest shared first, as his Home row lists them;
        // Heat, Arrival and Moon are Home Media's titles too.
        let films = [
            ("11", "In the Heat of the Night", 1967, 110, None),
            ("12", "Paddington 2", 2017, 103, None),
            ("13", "The Grand Budapest Hotel", 2014, 99, None),
            ("14", "Hot Fuzz", 2007, 121, None),
            ("15", "Knives Out", 2019, 130, None),
            ("16", "The Heat", 2013, 117, None),
            ("17", "Heat", 1995, 170, Some("3")),
            ("18", "Arrival", 2016, 116, Some("2")),
            ("19", "Moon", 2009, 97, Some("5")),
        ];
        let movies = fed_library(BOBS_MEDIA, "1");
        let mut ids = Vec::new();
        for (key, title, year, minutes, twin) in films {
            let id = fed(BOBS_MEDIA, MediaKind::Movie, key);
            if let Some(own) = twin {
                self.fed
                    .twins
                    .push((media(MediaKind::Movie, own), id.clone()));
            }
            self.fed.library_of.insert(id.clone(), movies.clone());
            self.add(film(id.clone(), title, year, minutes));
            ids.push(id);
        }
        self.fed
            .recent
            .insert(BOBS_MEDIA.into(), ids.iter().take(5).cloned().collect());
        self.libraries.push((
            Library {
                id: movies,
                title: "Movies".into(),
                kind: LibraryKind::Movies,
                server: Some(bob.clone()),
            },
            ids,
        ));

        let shows = fed_library(BOBS_MEDIA, "2");
        let show = self.paired_show(&shows);
        self.libraries.push((
            Library {
                id: shows,
                title: "TV Shows".into(),
                kind: LibraryKind::Shows,
                server: Some(bob.clone()),
            },
            vec![show],
        ));

        let dens = fed_library(DAVES_DEN, "1");
        let mut dave_ids = Vec::new();
        for (key, title, year, minutes) in
            [("1", "Rear Window", 1954, 112), ("2", "Zodiac", 2007, 157)]
        {
            let id = fed(DAVES_DEN, MediaKind::Movie, key);
            self.fed.library_of.insert(id.clone(), dens.clone());
            self.add(film(id.clone(), title, year, minutes));
            dave_ids.push(id);
        }
        self.fed.recent.insert(DAVES_DEN.into(), dave_ids.clone());
        self.libraries.push((
            Library {
                id: dens,
                title: "Movies".into(),
                kind: LibraryKind::Movies,
                server: Some(dave.clone()),
            },
            dave_ids,
        ));
        self.fed.servers = vec![bob, dave];
    }

    /// Brooklyn Nine-Nine's fifth season on Bob's Media, in `library`.
    fn paired_show(&mut self, library: &LibraryId) -> MediaId {
        let title = "Brooklyn Nine-Nine";
        let show = fed(BOBS_MEDIA, MediaKind::Show, "40");
        let season = fed(BOBS_MEDIA, MediaKind::Season, "41");
        self.add(Item::Show(Show {
            id: show.clone(),
            title: title.into(),
            year: Some(2013),
            summary: None,
            artwork: Vec::new(),
            content_rating: Some("TV-14".into()),
            season_count: Some(1),
            genres: vec!["Comedy".into()],
            cast: Vec::new(),
            last_aired: None,
        }));
        self.add(Item::Season(Season {
            id: season.clone(),
            show_id: show.clone(),
            show_title: Some(title.into()),
            season: 5,
            title: "Season 5".into(),
            artwork: Vec::new(),
        }));
        let mut episodes = Vec::new();
        for (i, name) in [
            "The Big House Pt. 2",
            "The Funeral",
            "Bad Beat",
            "HalloVeen",
        ]
        .into_iter()
        .enumerate()
        {
            let id = fed(BOBS_MEDIA, MediaKind::Episode, &format!("4{}", i + 2));
            self.add(Item::Episode(Episode {
                id: id.clone(),
                show_id: show.clone(),
                show_title: Some(title.into()),
                season_id: season.clone(),
                season: 5,
                episode: i as u32 + 1,
                title: name.into(),
                summary: None,
                duration: Some(Duration::from_secs(22 * 60)),
                artwork: Vec::new(),
                playback: PlaybackState::default(),
                content_rating: Some("TV-14".into()),
                year: None,
                air_date: None,
                cast: Vec::new(),
            }));
            self.fed.library_of.insert(id.clone(), library.clone());
            episodes.push(id);
        }
        for id in [&show, &season] {
            self.fed.library_of.insert(id.clone(), library.clone());
        }
        self.children.insert(show.clone(), vec![season.clone()]);
        self.children.insert(season, episodes);
        show
    }

    /// The paired server `id` is from; `None` for Home Media's own.
    pub(super) fn origin_of(&self, id: &MediaId) -> Option<&LibraryServer> {
        let origin = id.origin()?;
        self.fed.servers.iter().find(|s| s.server_id == origin)
    }

    /// Every copy of `id`'s title, Home Media's first; just `id` for a
    /// title on one server.
    pub(super) fn copies_of(&self, id: &MediaId) -> Vec<MediaId> {
        match self
            .fed
            .twins
            .iter()
            .find(|(own, other)| own == id || other == id)
        {
            Some((own, other)) => vec![own.clone(), other.clone()],
            None => vec![id.clone()],
        }
    }

    /// `id`'s copies as an item and a search hit carry them (P8-05), with
    /// the servers as `store` has them now; the copy that plays is the one
    /// on the server the user picked for the title, else `id` itself.
    pub(super) fn copies(&self, id: &MediaId, store: &Store) -> Copies {
        let server = |id: &MediaId| self.origin_of(id).map(|s| store.now(s));
        let copies = self.copies_of(id);
        let picked = store.play_from.get(&copies[0]);
        let play_from = match picked {
            Some(from) => copies
                .iter()
                .find(|c| c.origin() == from.as_deref())
                .unwrap_or(id)
                .clone(),
            None => id.clone(),
        };
        Copies {
            origin: server(id),
            also_on: copies
                .iter()
                .filter(|c| *c != id)
                .map(|c| TitleCopy {
                    id: c.clone(),
                    server: server(c),
                })
                .collect(),
            play_from,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use spoolframe_types::{Item, MediaKind, SearchScope};

    use super::{BOBS_MEDIA, DAVES_DEN, fed, fed_library};
    use crate::backend::{
        BackendError, Client, RemoteMode, SearchHit, SearchRequest, SearchResults, Session,
        TopResult,
    };
    use crate::fake::{FAKE_GUEST, FakeClient, media};
    use crate::testing::{file_with, ready};

    fn session(client: &FakeClient, username: &str) -> Arc<dyn Session> {
        client.session(&FakeClient::profile(username)).unwrap()
    }

    fn andrey(client: &FakeClient) -> Arc<dyn Session> {
        session(client, "andrey")
    }

    fn title(item: &Item) -> &str {
        match item {
            Item::Movie(m) => &m.title,
            Item::Show(s) => &s.title,
            Item::Episode(e) => &e.title,
            _ => "",
        }
    }

    fn heat_search() -> SearchRequest {
        SearchRequest {
            query: "heat".into(),
            limit: 10,
            scope: SearchScope::All,
            kind: None,
            library: None,
        }
    }

    /// Every hit of a search, the top one with the groups'.
    fn all_hits(hits: &SearchResults<SearchHit>) -> Vec<&SearchHit> {
        let top = hits.top.iter().filter_map(|t| match t {
            TopResult::Item(hit) => Some(hit),
            TopResult::Person(_) => None,
        });
        top.chain(hits.groups.iter().flat_map(|g| &g.items))
            .collect()
    }

    fn bobs_heat() -> spoolframe_types::MediaId {
        fed(BOBS_MEDIA, MediaKind::Movie, "17")
    }

    fn halloveen() -> spoolframe_types::MediaId {
        fed(BOBS_MEDIA, MediaKind::Episode, "45")
    }

    #[test]
    fn without_federation_no_paired_server_is_listed() {
        let client = FakeClient::new(None);
        let libraries = ready(andrey(&client).libraries()).unwrap();
        assert!(libraries.iter().all(|l| l.server.is_none()));
    }

    #[test]
    fn paired_servers_libraries_come_after_the_servers_own_with_whether_each_is_online() {
        let client = FakeClient::new(None).with_federation();
        let listed: Vec<_> = ready(andrey(&client).libraries())
            .unwrap()
            .into_iter()
            .map(|l| (l.title, l.server.map(|s| (s.name, s.online))))
            .collect();
        let bob = Some(("Bob’s Media".to_owned(), true));
        assert_eq!(
            listed[3..],
            [
                ("Movies".to_owned(), bob.clone()),
                ("TV Shows".to_owned(), bob),
                ("Movies".to_owned(), Some(("Dave’s Den".to_owned(), false))),
            ]
        );
    }

    #[test]
    fn a_portable_account_sees_nothing_federated() {
        let client = FakeClient::new(None).with_federation();
        let guest = session(&client, FAKE_GUEST);
        let libraries = ready(guest.libraries()).unwrap();
        let home = ready(guest.home()).unwrap();
        assert_eq!(
            (
                guest.federation(),
                libraries.iter().any(|l| l.server.is_some()),
                home.servers.len(),
                ready(guest.item(&bobs_heat())).unwrap_err(),
            ),
            (
                true,
                false,
                0,
                BackendError::NotFound(bobs_heat().to_string())
            )
        );
    }

    #[test]
    fn a_portable_accounts_search_finds_only_this_servers_copies() {
        let client = FakeClient::new(None).with_federation();
        let hits = ready(session(&client, FAKE_GUEST).search_hits(&heat_search())).unwrap();
        let federated = all_hits(&hits)
            .into_iter()
            .filter(|h| h.origin.is_some() || !h.also_on.is_empty())
            .count();
        assert_eq!(federated, 0);
    }

    #[test]
    fn home_has_a_row_per_paired_server() {
        let client = FakeClient::new(None).with_federation();
        let home = ready(andrey(&client).home()).unwrap();
        let rows: Vec<_> = home
            .servers
            .iter()
            .map(|r| (r.server.name.as_str(), r.server.online, r.items.len()))
            .collect();
        assert_eq!(rows, [("Bob’s Media", true, 5), ("Dave’s Den", false, 2)]);
    }

    #[test]
    fn a_title_on_both_servers_is_one_hit_from_this_servers_copy_also_on_the_other() {
        let client = FakeClient::new(None).with_federation();
        let hits = ready(andrey(&client).search_hits(&heat_search())).unwrap();
        let heat: Vec<_> = all_hits(&hits)
            .into_iter()
            .filter(|h| title(&h.item) == "Heat")
            .map(|h| {
                let also: Vec<_> = h
                    .also_on
                    .iter()
                    .map(|c| c.server.as_ref().map(|s| s.name.clone()))
                    .collect();
                (h.origin.clone(), also)
            })
            .collect();
        assert_eq!(heat, [(None, vec![Some("Bob’s Media".to_owned())])]);
    }

    #[test]
    fn a_title_only_on_a_paired_server_is_a_hit_from_it() {
        let client = FakeClient::new(None).with_federation();
        let hits = ready(andrey(&client).search_hits(&heat_search())).unwrap();
        let origin = all_hits(&hits)
            .into_iter()
            .find(|h| title(&h.item) == "In the Heat of the Night")
            .and_then(|h| h.origin.clone())
            .map(|s| s.name);
        assert_eq!(origin.as_deref(), Some("Bob’s Media"));
    }

    #[test]
    fn play_from_is_remembered_for_every_copy_of_the_title() {
        let client = FakeClient::new(None).with_federation();
        let session = andrey(&client);
        let own = media(MediaKind::Movie, "3");
        ready(session.set_play_from(&own, Some(BOBS_MEDIA))).unwrap();
        let plays = [own, bobs_heat()].map(|id| {
            ready(session.item_detail(&id))
                .unwrap()
                .copies
                .unwrap()
                .play_from
        });
        assert_eq!(plays, [bobs_heat(), bobs_heat()]);
    }

    #[test]
    fn a_title_on_one_server_has_nothing_to_play_from() {
        let client = FakeClient::new(None).with_federation();
        let alien = media(MediaKind::Movie, "1");
        let refused = ready(andrey(&client).set_play_from(&alien, Some(BOBS_MEDIA)));
        assert!(matches!(refused, Err(BackendError::InvalidInput(_))));
    }

    #[test]
    fn a_paired_item_plays_direct_when_reached() {
        let (_dir, path) = file_with("fed", b"stand-in");
        let client = FakeClient::new(Some(path)).with_federation();
        let session = andrey(&client);
        let start = ready(session.start_playback(&bobs_heat(), None, Default::default())).unwrap();
        assert_eq!(session.remote_mode(&start.handle), Some(RemoteMode::Direct));
    }

    #[test]
    fn without_a_direct_path_a_paired_item_plays_through_this_server() {
        let (_dir, path) = file_with("fed", b"stand-in");
        let client = FakeClient::new(Some(path))
            .with_federation()
            .without_direct_path();
        let session = andrey(&client);
        let start = ready(session.start_playback(&bobs_heat(), None, Default::default())).unwrap();
        assert_eq!(
            session.remote_mode(&start.handle),
            Some(RemoteMode::Proxied)
        );
    }

    #[test]
    fn this_servers_own_item_has_no_remote_mode() {
        let (_dir, path) = file_with("own", b"stand-in");
        let client = FakeClient::new(Some(path)).with_federation();
        let session = andrey(&client);
        let own = media(MediaKind::Movie, "3");
        let start = ready(session.start_playback(&own, None, Default::default())).unwrap();
        assert_eq!(session.remote_mode(&start.handle), None);
    }

    #[test]
    fn an_offline_servers_item_is_browsed_but_does_not_play() {
        let (_dir, path) = file_with("fed", b"stand-in");
        let client = FakeClient::new(Some(path)).with_federation();
        let session = andrey(&client);
        let rear_window = fed(DAVES_DEN, MediaKind::Movie, "1");
        assert_eq!(
            (
                ready(session.item(&rear_window)).is_ok(),
                ready(session.start_playback(&rear_window, None, Default::default())).err(),
            ),
            (true, Some(BackendError::OriginOffline))
        );
    }

    #[test]
    fn progress_on_a_paired_item_is_kept_here() {
        let (_dir, path) = file_with("fed", b"stand-in");
        let client = FakeClient::new(Some(path)).with_federation();
        let session = andrey(&client);
        let start = ready(session.start_playback(&halloveen(), None, Default::default())).unwrap();
        ready(session.update_progress(&start.handle, Duration::from_secs(780))).unwrap();
        let home = ready(session.home()).unwrap();
        assert_eq!(
            home.continue_watching.first().map(|i| i.id().clone()),
            Some(halloveen())
        );
    }

    #[test]
    fn a_library_no_longer_shared_goes_and_its_items_in_progress_are_unavailable() {
        let (_dir, path) = file_with("fed", b"stand-in");
        let client = FakeClient::new(Some(path)).with_federation();
        let session = andrey(&client);
        let start = ready(session.start_playback(&halloveen(), None, Default::default())).unwrap();
        ready(session.update_progress(&start.handle, Duration::from_secs(780))).unwrap();
        let shows = fed_library(BOBS_MEDIA, "2");
        let mut told = client.libraries_events();
        client.stop_sharing(&shows);
        let listed = ready(session.libraries()).unwrap();
        let home = ready(session.home()).unwrap();
        assert_eq!(
            (
                told.try_recv().is_ok(),
                listed.iter().any(|l| l.id == shows),
                home.unavailable,
            ),
            (true, false, vec![halloveen()])
        );
    }

    #[test]
    fn a_server_coming_back_is_told_and_listed_online() {
        let client = FakeClient::new(None).with_federation();
        let mut told = client.libraries_events();
        client.set_paired_online(DAVES_DEN, true);
        let dave = ready(andrey(&client).libraries())
            .unwrap()
            .into_iter()
            .find_map(|l| l.server.filter(|s| s.server_id == DAVES_DEN));
        assert_eq!(
            (told.try_recv().is_ok(), dave.map(|s| s.online)),
            (true, Some(true))
        );
    }
}
