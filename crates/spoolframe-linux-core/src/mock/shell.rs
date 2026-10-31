//! Stand-ins for the app shell's tests: a [`MockClient`] with the servers,
//! profiles and session a test gives it, and a profile store per profile
//! kept in memory; a [`MockCatalog`] session answering libraries, items,
//! search and the reads under an item from what its test gave it. As with
//! [`super::MockSession`], anything not given answers as the trait
//! defaults do.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use spoolframe_types::{
    Item, ItemFilter, ItemFilterValues, ItemSort, Library, LibraryId, LibraryKind, MediaId,
    Namespace,
};

use crate::backend::{
    BackendError, BoxFuture, Client, Copies, ItemDetail, Kinds, Profile, ProfileStore, SearchHit,
    SearchRequest, SearchResults, Server, ServerKey, Session,
};
use crate::identity::Identities;

/// A client with fixed servers and profiles, handing out one session.
pub struct MockClient {
    servers: Vec<Server>,
    /// `None`: `profiles` never answers, as a server that is down.
    profiles: Option<Vec<Profile>>,
    session: Option<Arc<dyn Session>>,
    stores: Mutex<HashMap<(ServerKey, String), Arc<MemoryStore>>>,
    identities: Identities,
}

impl Default for MockClient {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            profiles: Some(Vec::new()),
            session: None,
            stores: Mutex::default(),
            identities: crate::identity::in_memory(),
        }
    }
}

impl MockClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// `server` saved, listed by `servers` after those given before.
    pub fn with_server(mut self, server: Server) -> Self {
        self.servers.push(server);
        self
    }

    /// What `profiles` answers, for any server.
    pub fn with_profiles(mut self, profiles: Vec<Profile>) -> Self {
        self.profiles = Some(profiles);
        self
    }

    /// `profiles` never answers: the server is down, and its connect
    /// waits out its timeout.
    pub fn with_profiles_pending(mut self) -> Self {
        self.profiles = None;
        self
    }

    /// The session `session` hands out, for any profile.
    pub fn with_session(mut self, session: Arc<dyn Session>) -> Self {
        self.session = Some(session);
        self
    }
}

impl Client for MockClient {
    fn servers(&self) -> BoxFuture<'_, Result<Vec<Server>, BackendError>> {
        let servers = self.servers.clone();
        Box::pin(async move { Ok(servers) })
    }

    fn identities(&self) -> Identities {
        self.identities.clone()
    }

    fn profiles<'a>(
        &'a self,
        _server: &'a ServerKey,
    ) -> BoxFuture<'a, Result<Vec<Profile>, BackendError>> {
        match self.profiles.clone() {
            Some(profiles) => Box::pin(async move { Ok(profiles) }),
            None => Box::pin(std::future::pending()),
        }
    }

    fn session(&self, _profile: &Profile) -> Result<Arc<dyn Session>, BackendError> {
        self.session
            .clone()
            .ok_or_else(|| BackendError::Unsupported("session".into()))
    }

    /// The same store each time for the same profile, so what one read
    /// saved the next reads back.
    fn profile_store(&self, server: &ServerKey, username: &str) -> Arc<dyn ProfileStore> {
        self.stores
            .lock()
            .unwrap()
            .entry((server.clone(), username.to_owned()))
            .or_default()
            .clone()
    }
}

/// One profile's values, in memory.
#[derive(Default)]
struct MemoryStore {
    values: Mutex<HashMap<String, Vec<u8>>>,
}

impl ProfileStore for MemoryStore {
    fn get<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, BackendError>> {
        let value = self.values.lock().unwrap().get(name).cloned();
        Box::pin(async move { Ok(value) })
    }

    fn set<'a>(
        &'a self,
        name: &'a str,
        value: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<(), BackendError>> {
        let mut values = self.values.lock().unwrap();
        match value {
            Some(value) => values.insert(name.to_owned(), value),
            None => values.remove(name),
        };
        Box::pin(async { Ok(()) })
    }
}

type Sorted = dyn Fn(&LibraryId, Option<ItemSort>, Option<&ItemFilter>) -> Result<Vec<Item>, BackendError>
    + Send
    + Sync;
type Search = dyn Fn(&SearchRequest) -> SearchResults + Send + Sync;

/// One `items_sorted` the session was sent: the library, the sort and the
/// filter.
pub type SortedAsk = (LibraryId, Option<ItemSort>, Option<ItemFilter>);

/// A session answering the catalog from canned data.
#[derive(Default)]
pub struct MockCatalog {
    libraries: Vec<Library>,
    items: HashMap<LibraryId, Vec<Item>>,
    values: HashMap<LibraryId, ItemFilterValues>,
    sorted: Option<Box<Sorted>>,
    sorted_asked: Mutex<Vec<SortedAsk>>,
    search: Option<Box<Search>>,
    reads: HashMap<MediaId, (Item, Vec<Item>)>,
    details: HashMap<MediaId, ItemDetail>,
    played: Vec<Item>,
    played_asked: Mutex<Vec<Option<Kinds>>>,
}

impl MockCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// `library` listed by `libraries`, holding `items` in that order
    /// (answered a slice at a time by `items`) and sorted and filtered by
    /// what `values` lists.
    pub fn with_library(
        mut self,
        library: Library,
        items: Vec<Item>,
        values: ItemFilterValues,
    ) -> Self {
        self.items.insert(library.id.clone(), items);
        self.values.insert(library.id.clone(), values);
        self.libraries.push(library);
        self
    }

    /// How `items_sorted` answers: the whole list `answer` makes of what
    /// it was asked, a slice at a time; without it, `Unsupported`.
    pub fn with_sorted(
        mut self,
        answer: impl Fn(
            &LibraryId,
            Option<ItemSort>,
            Option<&ItemFilter>,
        ) -> Result<Vec<Item>, BackendError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.sorted = Some(Box::new(answer));
        self
    }

    /// Every `items_sorted` sent, in order.
    pub fn sorted_asked(&self) -> Vec<SortedAsk> {
        self.sorted_asked.lock().unwrap().clone()
    }

    /// How a search answers, each item a hit of this server's own.
    pub fn with_search(
        mut self,
        answer: impl Fn(&SearchRequest) -> SearchResults + Send + Sync + 'static,
    ) -> Self {
        self.search = Some(Box::new(answer));
        self
    }

    /// What `item` answers for `item`'s id: it and its `children`.
    pub fn with_item(mut self, item: Item, children: Vec<Item>) -> Self {
        self.reads.insert(item.id().clone(), (item, children));
        self
    }

    /// What `item_detail` answers for its item's id.
    pub fn with_detail(mut self, detail: ItemDetail) -> Self {
        self.details.insert(detail.item.id().clone(), detail);
        self
    }

    /// What `recently_played` answers, whatever kinds it is asked for.
    pub fn with_recently_played(mut self, items: Vec<Item>) -> Self {
        self.played = items;
        self
    }

    /// The kinds of every `recently_played` sent, in order.
    pub fn recently_played_asked(&self) -> Vec<Option<Kinds>> {
        self.played_asked.lock().unwrap().clone()
    }
}

/// The part of `whole` from `offset`, at most `limit`, and how many in
/// all.
fn slice(whole: Vec<Item>, offset: u32, limit: u32) -> (Vec<Item>, u32) {
    let total = whole.len() as u32;
    let page = whole
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    (page, total)
}

impl Session for MockCatalog {
    fn libraries(&self) -> BoxFuture<'_, Result<Vec<Library>, BackendError>> {
        let libraries = self.libraries.clone();
        Box::pin(async move { Ok(libraries) })
    }

    fn items<'a>(
        &'a self,
        library: &'a LibraryId,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>> {
        let answer = match self.items.get(library) {
            Some(items) => Ok(slice(items.clone(), offset, limit)),
            None => Err(BackendError::NotFound(library.to_string())),
        };
        Box::pin(async move { answer })
    }

    fn items_sorted<'a>(
        &'a self,
        library: &'a LibraryId,
        offset: u32,
        limit: u32,
        sort: Option<ItemSort>,
        filter: Option<&'a ItemFilter>,
    ) -> BoxFuture<'a, Result<(Vec<Item>, u32), BackendError>> {
        self.sorted_asked
            .lock()
            .unwrap()
            .push((library.clone(), sort, filter.cloned()));
        let answer = match &self.sorted {
            Some(sorted) => sorted(library, sort, filter).map(|whole| slice(whole, offset, limit)),
            None => Err(BackendError::Unsupported("items_sorted".into())),
        };
        Box::pin(async move { answer })
    }

    fn item_filter_values<'a>(
        &'a self,
        library: &'a LibraryId,
    ) -> BoxFuture<'a, Result<ItemFilterValues, BackendError>> {
        let answer = self
            .values
            .get(library)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(library.to_string()));
        Box::pin(async move { answer })
    }

    fn item<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<(Item, Vec<Item>), BackendError>> {
        let answer = self
            .reads
            .get(id)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(id.to_string()));
        Box::pin(async move { answer })
    }

    fn item_detail<'a>(
        &'a self,
        id: &'a MediaId,
    ) -> BoxFuture<'a, Result<ItemDetail, BackendError>> {
        let answer = self
            .details
            .get(id)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(id.to_string()));
        Box::pin(async move { answer })
    }

    fn search_hits<'a>(
        &'a self,
        request: &'a SearchRequest,
    ) -> BoxFuture<'a, Result<SearchResults<SearchHit>, BackendError>> {
        let answer = match &self.search {
            Some(search) => Ok(search(request).map(own_hit)),
            None => Err(BackendError::Unsupported("search_hits".into())),
        };
        Box::pin(async move { answer })
    }

    fn recently_played(
        &self,
        kinds: Option<Kinds>,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'_, Result<(Vec<Item>, u32), BackendError>> {
        self.played_asked.lock().unwrap().push(kinds);
        let answer = slice(self.played.clone(), offset, limit);
        Box::pin(async move { Ok(answer) })
    }
}

/// `item` as a hit of this server's own, on no other.
fn own_hit(item: Item) -> SearchHit {
    let play_from = item.id().clone();
    SearchHit::new(
        item,
        Copies {
            origin: None,
            also_on: Vec::new(),
            play_from,
        },
    )
}

/// The saved, pinned and verified server `name`, with key `key`.
pub fn server(key: &str, name: &str) -> Server {
    Server {
        key: ServerKey(key.into()),
        name: name.into(),
        addresses: vec!["10.10.0.5:7430".into()],
        pinned: true,
        fingerprint: "Q6D2-W5NB-3KX7-M4RA".into(),
        verified: true,
        on_network: false,
        verified_at: None,
        last_connected_at: None,
        server_id: None,
        advertised: Vec::new(),
        relays: Vec::new(),
        advertised_at: None,
        use_relays: None,
        relays_edited: false,
    }
}

/// `username`'s profile on `server`, unlocked, signed in with a password.
pub fn profile(server: &ServerKey, username: &str) -> Profile {
    Profile {
        server: server.clone(),
        username: username.into(),
        display_name: username.into(),
        locked: false,
        sign_in: Default::default(),
        trust: None,
    }
}

/// This server's own library `key` of `kind`, titled `title`.
pub fn library(key: &str, kind: LibraryKind, title: &str) -> Library {
    Library {
        id: LibraryId::new(Namespace::Plex, key).unwrap(),
        title: title.into(),
        kind,
        server: None,
    }
}
