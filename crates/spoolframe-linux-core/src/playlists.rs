//! Playlists (PX-40) as the screens show them, without GTK: the
//! Playlists section's groups, Pinned in its order, which playlists "Add
//! to playlist" offers for an item, the lines under a playlist's name and
//! its entries, the edit a drag makes, and what an event asks to read
//! again. The server keeps everything; this only reads its answers.
//!
//! Playlist summaries carry no counts (only a playlist's own page does),
//! so lines from a summary say whose it is and what it holds, never how
//! many.

use std::time::Duration;

use spoolframe_types::{Item, MediaId, MediaKind};

use crate::backend::{
    BackendError, Builtin, EntryId, Kinds, Liked, PlaylistChange, PlaylistEdit, PlaylistEntry,
    PlaylistEvent, PlaylistNext, PlaylistOwnerKind, PlaylistPage, PlaylistShare, PlaylistSummary,
    Session, Visibility,
};

/// The most entries a page of a playlist holds.
pub const PAGE: u32 = 200;

/// The Playlists section's groups, each in the server's order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sections<'a> {
    /// Liked and All (PX-43), first, under Music: the server lists them
    /// first, Liked before All.
    pub builtin: Vec<&'a PlaylistSummary>,
    pub mine: Vec<&'a PlaylistSummary>,
    pub shared_with_me: Vec<&'a PlaylistSummary>,
    pub server_wide: Vec<&'a PlaylistSummary>,
}

/// `list` apart by whose each is. A kind from a newer server goes with
/// the shared ones: someone else's, seen by this user.
pub fn sections(list: &[PlaylistSummary]) -> Sections<'_> {
    let mut sections = Sections::default();
    for playlist in list {
        if playlist.builtin.is_some() {
            sections.builtin.push(playlist);
            continue;
        }
        match playlist.owner_kind {
            PlaylistOwnerKind::Mine => sections.mine.push(playlist),
            PlaylistOwnerKind::ServerWide => sections.server_wide.push(playlist),
            PlaylistOwnerKind::SharedWithMe | PlaylistOwnerKind::Unknown => {
                sections.shared_with_me.push(playlist)
            }
        }
    }
    sections
}

/// The user's Pinned, in their order.
pub fn pinned(list: &[PlaylistSummary]) -> Vec<&PlaylistSummary> {
    let mut pinned: Vec<_> = list.iter().filter(|p| p.pinned.is_some()).collect();
    pinned.sort_by_key(|p| p.pinned);
    pinned
}

/// Where a pin newly made goes: last.
pub fn new_pin_position(list: &[PlaylistSummary]) -> u32 {
    list.iter().filter(|p| p.pinned.is_some()).count() as u32
}

/// Whose a playlist is, as its card and row say under its name.
pub fn owner_line(playlist: &PlaylistSummary) -> String {
    match playlist.builtin {
        Some(Builtin::All) => return "Built in · every track".to_owned(),
        Some(_) => return "Built in".to_owned(),
        None => {}
    }
    match (playlist.owner_kind, &playlist.visibility) {
        (PlaylistOwnerKind::Mine, Visibility::Shared { with }) if !with.is_empty() => {
            match with.len() {
                1 => "Yours · shared with 1 person".to_owned(),
                n => format!("Yours · shared with {n} people"),
            }
        }
        (PlaylistOwnerKind::Mine, _) => "Yours".to_owned(),
        (PlaylistOwnerKind::SharedWithMe, _) if playlist.can_edit => {
            "Shared with you · you can edit".to_owned()
        }
        (PlaylistOwnerKind::SharedWithMe, _) => "Shared with you · read only".to_owned(),
        (PlaylistOwnerKind::ServerWide, _) if playlist.can_manage => {
            "Server-wide · you can edit".to_owned()
        }
        (PlaylistOwnerKind::ServerWide, _) => "Server-wide".to_owned(),
        (PlaylistOwnerKind::Unknown, _) => "Shared with you".to_owned(),
    }
}

/// What a playlist holds: "Episodes", "Movies, episodes", "Music".
pub fn kinds_line(kinds: Kinds) -> String {
    let names: Vec<&str> = [
        (kinds.movies, "movies"),
        (kinds.episodes, "episodes"),
        (kinds.tracks, "music"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    let line = names.join(", ");
    let mut chars = line.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// How "Add to playlist" offers a playlist that takes the item's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// One click adds.
    Adds,
    /// Shared with the user to read: listed greyed.
    ReadOnly,
    /// Server-wide, and the user is no admin: listed greyed.
    AdminsEdit,
    /// Liked (PX-43): a track gets in by its heart, never by an add.
    UseHeart,
    /// All: it holds every track already.
    HasEverything,
}

impl Offer {
    /// Why a greyed one cannot take it; `None` for one that can.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Offer::Adds => None,
            Offer::ReadOnly => Some("Read only"),
            Offer::AdminsEdit => Some("Server-wide · admins edit"),
            Offer::UseHeart => Some("Use the heart instead"),
            Offer::HasEverything => Some("Built in · every track already"),
        }
    }
}

/// The playlists "Add to playlist" lists for an item of `kind`: only those
/// whose kinds take it (a film never sees a music playlist), the ones the
/// user can add to first, each group in the server's order.
pub fn add_targets(list: &[PlaylistSummary], kind: MediaKind) -> Vec<(&PlaylistSummary, Offer)> {
    let mut targets: Vec<_> = list
        .iter()
        .filter(|p| p.kinds.allows(kind))
        .map(|p| {
            let offer = match (p.builtin, p.can_edit, p.owner_kind) {
                (Some(Builtin::Liked), _, _) => Offer::UseHeart,
                (Some(_), _, _) => Offer::HasEverything,
                (None, true, _) => Offer::Adds,
                (None, false, PlaylistOwnerKind::ServerWide) => Offer::AdminsEdit,
                (None, false, _) => Offer::ReadOnly,
            };
            (p, offer)
        })
        .collect();
    targets.sort_by_key(|(_, offer)| *offer != Offer::Adds);
    targets
}

/// Whether a tap on a heart showing `liked` likes (true) or un-likes: a
/// full heart un-likes, a half one likes the rest, an empty one likes.
pub fn heart_likes(liked: Liked) -> bool {
    !matches!(liked, Liked::All)
}

/// An album's heart from which of its `tracks` are `liked`: full when
/// every one is, half when some are.
pub fn album_heart(tracks: &[MediaId], liked: &std::collections::HashSet<MediaId>) -> Liked {
    match tracks.iter().filter(|t| liked.contains(*t)).count() {
        0 => Liked::None,
        n if n == tracks.len() => Liked::All,
        _ => Liked::Some,
    }
}

/// A playlist's header line from its page: "7 episodes", "5 films, 2
/// episodes", "18 episodes · 17 playable". By kind only when the page has
/// every entry; a longer playlist says how many items.
pub fn count_line(page: &PlaylistPage) -> String {
    if page.total == 0 {
        return "Empty".to_owned();
    }
    let counts = if page.offset == 0 && page.entries.len() as u32 == page.total {
        let count = |kind: MediaKind| {
            page.entries
                .iter()
                .filter(|e| e.item.id().kind() == kind)
                .count()
        };
        [
            (count(MediaKind::Movie), "film", "films"),
            (count(MediaKind::Episode), "episode", "episodes"),
            (count(MediaKind::Track), "track", "tracks"),
        ]
        .into_iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, one, many)| plural(n, one, many))
        .collect::<Vec<_>>()
        .join(", ")
    } else {
        plural(page.total as usize, "item", "items")
    };
    match page.playable < page.total {
        true => format!("{counts} · {} playable", page.playable),
        false => counts,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// The entry Continue starts at, of those on the page: the user's place,
/// or the next playable one once the place's item is watched or cannot be
/// read. `None` without a place, when Continue is Play from the top.
pub fn up_next(page: &PlaylistPage) -> Option<EntryId> {
    let place = page.place?;
    let at = page.entries.iter().position(|e| e.entry == place)?;
    let here = &page.entries[at];
    if !here.unavailable && !watched(&here.item) {
        return Some(place);
    }
    page.entries[at + 1..]
        .iter()
        .find(|e| !e.unavailable)
        .map(|e| e.entry)
}

/// What Continue says it plays: "S4 · E6 Halloween IV", or a title.
pub fn continue_label(page: &PlaylistPage) -> Option<String> {
    let next = up_next(page)?;
    let entry = page.entries.iter().find(|e| e.entry == next)?;
    Some(match &entry.item {
        Item::Episode(e) => format!("S{} · E{} {}", e.season, e.episode, e.title),
        item => title(item),
    })
}

/// The line under an entry: Unavailable, Up next, Watched, or its length.
pub fn entry_note(entry: &PlaylistEntry, up_next: Option<EntryId>) -> Option<String> {
    if entry.unavailable {
        return Some("Unavailable · the file can’t be read; skipped".to_owned());
    }
    if Some(entry.entry) == up_next {
        return Some("Up next · Continue starts here".to_owned());
    }
    if watched(&entry.item) {
        return Some("Watched".to_owned());
    }
    length(&entry.item).map(crate::format::length)
}

/// Where an entry's item is from: "Andor · S1 · E3", "The Night Shift ·
/// Low Tide", or a film's year.
pub fn entry_context(item: &Item) -> String {
    match item {
        Item::Episode(e) => {
            let show = e.show_title.clone().unwrap_or_default();
            format!("{show} · S{} · E{}", e.season, e.episode)
        }
        Item::Track(t) => [t.artist_name.clone(), t.album_title.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · "),
        Item::Movie(m) => m.year.map(|y| y.to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// An item's own name: a film's, an episode's or a track's title.
pub fn title(item: &Item) -> String {
    match item {
        Item::Movie(m) => m.title.clone(),
        Item::Episode(e) => e.title.clone(),
        Item::Track(t) => t.title.clone(),
        Item::Show(s) => s.title.clone(),
        Item::Season(s) => s.title.clone(),
        Item::Album(a) => a.title.clone(),
        Item::Artist(a) => a.name.clone(),
    }
}

fn watched(item: &Item) -> bool {
    match item {
        Item::Movie(m) => m.playback.watched,
        Item::Episode(e) => e.playback.watched,
        Item::Track(t) => t.playback.watched,
        _ => false,
    }
}

fn length(item: &Item) -> Option<Duration> {
    match item {
        Item::Movie(m) => m.duration,
        Item::Episode(e) => e.duration,
        Item::Track(t) => t.duration,
        _ => None,
    }
}

/// The edit that moves the entry at `from` in `entries` (as shown) so it
/// ends at `to`: before whatever is then after it, or to the end. `None`
/// when it stays where it is.
pub fn move_edit(entries: &[EntryId], from: usize, to: usize) -> Option<PlaylistEdit> {
    if from >= entries.len() {
        return None;
    }
    let mut order = entries.to_vec();
    let entry = order.remove(from);
    let to = to.min(order.len());
    if to == from {
        return None;
    }
    order.insert(to, entry);
    Some(PlaylistEdit::Move {
        entry,
        before: order.get(to + 1).copied(),
    })
}

/// `edits` on the revision shown; when someone changed the same entries
/// since, once more on the revision the refusal names, so an edit the
/// other one did not undo still lands. A second refusal is the user's to
/// see ("Someone changed this playlist; reload").
pub async fn edit_retrying(
    session: &dyn Session,
    id: &str,
    revision: u64,
    edits: Vec<PlaylistEdit>,
) -> Result<u64, BackendError> {
    match session.edit_playlist(id, revision, edits.clone()).await {
        Err(BackendError::PlaylistConflict { revision }) => {
            session.edit_playlist(id, revision, edits).await
        }
        done => done,
    }
}

/// What an event asks to read again, for the server `server` and the
/// playlist page shown, if one is.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Refresh {
    /// The playlists list, Pinned with it.
    pub list: bool,
    /// The page shown.
    pub page: bool,
    /// The page shown is no longer the user's to see: leave it.
    pub page_gone: bool,
}

pub fn refresh_for(event: &PlaylistEvent, server: &str, shown: Option<&str>) -> Refresh {
    if event.server != server {
        return Refresh::default();
    }
    match &event.change {
        PlaylistChange::Changed { id, .. } => Refresh {
            list: true,
            page: shown == Some(id.as_str()),
            page_gone: false,
        },
        PlaylistChange::Gone { id } => Refresh {
            list: true,
            page: false,
            page_gone: shown == Some(id.as_str()),
        },
        PlaylistChange::PinsChanged => Refresh {
            list: true,
            ..Refresh::default()
        },
    }
}

/// The one-time note for pins that left Pinned: "Christmas films is no
/// longer shared with you, so it left Pinned."
pub fn pin_note_text(notes: &[crate::backend::PinNote]) -> Option<String> {
    use crate::backend::PinNoteReason;
    let lines: Vec<String> = notes
        .iter()
        .map(|note| match note.reason {
            PinNoteReason::Deleted => format!("{} was deleted, so it left Pinned.", note.name),
            PinNoteReason::Unshared => format!(
                "{} is no longer shared with you, so it left Pinned.",
                note.name
            ),
            PinNoteReason::Unknown => format!("{} left Pinned.", note.name),
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The pin notes shown this session: the server keeps a note until some
/// device dismisses it, so a list read again before the dismissal lands
/// carries it once more, and it is not shown twice.
#[derive(Debug, Default)]
pub struct PinNotesShown(std::collections::HashSet<String>);

impl PinNotesShown {
    /// The text of the notes in `notes` not shown yet, now counted as
    /// shown; `None` when there are none.
    pub fn fresh(&mut self, notes: &[crate::backend::PinNote]) -> Option<String> {
        let fresh: Vec<_> = notes
            .iter()
            .filter(|note| self.0.insert(note.playlist_id.clone()))
            .cloned()
            .collect();
        pin_note_text(&fresh)
    }
}

/// The pin a drag within Pinned makes: the playlist at `from` (in
/// [`pinned`]'s order) dropped on the row at `to` takes that row's place.
/// Its id and the position to pin it at; `None` when it stays.
pub fn pin_drop(pinned: &[&PlaylistSummary], from: usize, to: usize) -> Option<(String, u32)> {
    let playlist = pinned.get(from)?;
    let to = to.min(pinned.len() - 1);
    (to != from).then(|| (playlist.id.clone(), to as u32))
}

/// `list` with playlist `id` pinned at `position`, as the server will
/// have it: shown at once, before its answer. Positions are made 0, 1, …
/// in the new order.
pub fn with_pin_at(list: &mut [PlaylistSummary], id: &str, position: u32) {
    let mut order: Vec<String> = pinned(list)
        .into_iter()
        .filter(|p| p.id != id)
        .map(|p| p.id.clone())
        .collect();
    let at = (position as usize).min(order.len());
    order.insert(at, id.to_owned());
    for playlist in list.iter_mut() {
        playlist.pinned = order
            .iter()
            .position(|pinned| *pinned == playlist.id)
            .map(|at| at as u32);
    }
}

/// What "New playlist" sends, from what its dialog has: a trimmed name,
/// at least one kind, and who sees it.
pub fn new_playlist(
    name: &str,
    kinds: Kinds,
    server_wide: bool,
) -> Result<(String, Kinds, Visibility), &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the playlist a name.");
    }
    if !(kinds.movies || kinds.episodes || kinds.tracks) {
        return Err("Tick at least one kind it holds.");
    }
    let visibility = match server_wide {
        true => Visibility::ServerWide,
        false => Visibility::Personal,
    };
    Ok((name.to_owned(), kinds, visibility))
}

/// Adds `item` to the end of playlist `id` (a season its episodes, an
/// album its tracks), retrying once on a conflict: the entries the add
/// made, for Undo. They are told apart from the ones before by id, so an
/// item already in the playlist, or one added twice, is counted right.
pub async fn add_item(
    session: &dyn Session,
    id: &str,
    item: &MediaId,
) -> Result<Vec<EntryId>, BackendError> {
    let head = session.playlist(id, 0, 1).await?;
    // The last page before, and everything from it on after: an add
    // appends, so its entries are past what was there.
    let from = head.total.saturating_sub(PAGE);
    let before = match head.total {
        0 => Vec::new(),
        _ => entry_ids(&session.playlist(id, from, PAGE).await?),
    };
    let add = vec![PlaylistEdit::Add {
        id: item.clone(),
        before: None,
    }];
    edit_retrying(session, id, head.playlist.revision, add).await?;
    let mut after = Vec::new();
    let mut offset = from;
    loop {
        let page = session.playlist(id, offset, PAGE).await?;
        after.extend(entry_ids(&page));
        offset += page.entries.len() as u32;
        if page.entries.is_empty() || offset >= page.total {
            break;
        }
    }
    Ok(added_entries(&before, &after))
}

fn entry_ids(page: &PlaylistPage) -> Vec<EntryId> {
    page.entries.iter().map(|e| e.entry).collect()
}

/// The entries in `after` that were not in `before`, in order.
fn added_entries(before: &[EntryId], after: &[EntryId]) -> Vec<EntryId> {
    after
        .iter()
        .filter(|e| !before.contains(e))
        .copied()
        .collect()
}

/// Takes `entries` out of playlist `id`: Undo of an add.
pub async fn remove_entries(
    session: &dyn Session,
    id: &str,
    entries: &[EntryId],
) -> Result<u64, BackendError> {
    let revision = session.playlist(id, 0, 1).await?.playlist.revision;
    let edits = entries
        .iter()
        .map(|&entry| PlaylistEdit::Remove { entry })
        .collect();
    edit_retrying(session, id, revision, edits).await
}

/// What a refused add says, for `item` and the playlist `playlist`.
pub fn add_refusal(error: &BackendError, item: &Item, playlist: &str) -> String {
    match error {
        BackendError::KindNotAllowed(_) => format!("{} can’t go in {playlist}", title(item)),
        BackendError::PlaylistReadOnly => "You can’t change this playlist".to_owned(),
        other => other.to_string(),
    }
}

/// The kinds a playlist made from an item's "New playlist…" starts with:
/// those that take the item.
pub fn kinds_for(kind: MediaKind) -> Kinds {
    Kinds {
        movies: kind == MediaKind::Movie,
        episodes: matches!(
            kind,
            MediaKind::Episode | MediaKind::Season | MediaKind::Show
        ),
        tracks: matches!(
            kind,
            MediaKind::Track | MediaKind::Album | MediaKind::Artist
        ),
    }
}

/// What the editor shows of a playlist: Edit itself, the entries' handles
/// and Remove, and the side panel (name, kinds, sharing, Delete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorControls {
    pub edit: bool,
    pub panel: bool,
    /// Server-wide among the sharing choices: an admin's.
    pub server_wide: bool,
}

pub fn editor_controls(playlist: &PlaylistSummary, admin: bool) -> EditorControls {
    EditorControls {
        edit: playlist.can_edit || playlist.can_manage,
        panel: playlist.can_manage,
        server_wide: playlist.can_manage && admin,
    }
}

/// A change made in the editor's Sharing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareChange {
    Personal,
    ServerWide,
    /// Whether this user may edit the entries.
    CanEdit(String, bool),
    StopSharing(String),
}

/// Who sees the playlist after `change`, from `now`. Shared with nobody
/// is Personal.
pub fn share_change(now: &Visibility, change: &ShareChange) -> Visibility {
    let shares = |now: &Visibility| match now {
        Visibility::Shared { with } => with.clone(),
        _ => Vec::new(),
    };
    let with: Vec<PlaylistShare> = match change {
        ShareChange::Personal => return Visibility::Personal,
        ShareChange::ServerWide => return Visibility::ServerWide,
        ShareChange::CanEdit(user, can_edit) => shares(now)
            .into_iter()
            .map(|share| match share.user_id == *user {
                true => PlaylistShare {
                    can_edit: *can_edit,
                    ..share
                },
                false => share,
            })
            .collect(),
        ShareChange::StopSharing(user) => shares(now)
            .into_iter()
            .filter(|share| share.user_id != *user)
            .collect(),
    };
    match with.is_empty() {
        true => Visibility::Personal,
        false => Visibility::Shared { with },
    }
}

/// A playlist being played: which, and the entry playing. The server
/// keeps the user's place and picks each next entry (shuffle, repeat and
/// skip watched are its); this only follows its answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playing {
    pub id: String,
    pub name: String,
    pub entry: EntryId,
}

/// What follows when the entry playing ends, or Next is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advance {
    /// The server's next entry: its item, from where the user stopped in
    /// it.
    Play {
        item: Box<Item>,
        from: Option<Duration>,
    },
    /// The end, with repeat off: the server has cleared the place, so the
    /// next play starts at the top.
    Finished,
}

impl Playing {
    /// `next_in_playlist`'s answer, taken: the entry playing moves on.
    pub fn advance(&mut self, next: Option<PlaylistNext>) -> Advance {
        let Some(next) = next else {
            return Advance::Finished;
        };
        self.entry = next.entry;
        Advance::Play {
            from: resume_from(&next.item),
            item: Box::new(next.item),
        }
    }

    /// Under what plays: "Playing from the Heists".
    pub fn line(&self) -> String {
        format!("Playing from {}", self.name)
    }

    /// When it has played to its end.
    pub fn finished_text(&self) -> String {
        format!("End of {}", self.name)
    }
}

/// Where the user stopped in `item`, unless they watched it to the end.
pub fn resume_from(item: &Item) -> Option<Duration> {
    let state = match item {
        Item::Movie(m) => &m.playback,
        Item::Episode(e) => &e.playback,
        Item::Track(t) => &t.playback,
        _ => return None,
    };
    state.position.filter(|_| !state.watched)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use spoolframe_types::{MediaId, MediaKind, Namespace};

    use super::*;
    use crate::backend::{Client, PinNoteReason, PlaySettings, PlaylistUpdate, Repeat};
    use crate::fake::FakeClient;

    fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
        futures::executor::block_on(future)
    }

    fn media(kind: MediaKind, key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, kind, key).unwrap()
    }

    /// `username`'s session; Alice's opens locked, so it is unlocked.
    fn session(client: &FakeClient, username: &str) -> Arc<dyn Session> {
        let session = client.session(&FakeClient::profile(username)).unwrap();
        if username == "alice" {
            ready(session.unlock(crate::fake::FAKE_PIN)).unwrap();
        }
        session
    }

    fn named<'a>(list: &'a [PlaylistSummary], name: &str) -> &'a PlaylistSummary {
        list.iter().find(|p| p.name == name).unwrap()
    }

    fn names(list: &[&PlaylistSummary]) -> Vec<String> {
        list.iter().map(|p| p.name.clone()).collect()
    }

    fn episodes() -> Kinds {
        Kinds {
            episodes: true,
            ..Kinds::default()
        }
    }

    fn page(session: &dyn Session, id: &str) -> PlaylistPage {
        ready(session.playlist(id, 0, 200)).unwrap()
    }

    #[test]
    fn the_section_groups_playlists_by_whose_they_are() {
        let client = FakeClient::new(None);
        let list = ready(session(&client, "andrey").playlists())
            .unwrap()
            .playlists;
        let groups = sections(&list);
        assert_eq!(
            (
                names(&groups.mine),
                names(&groups.shared_with_me),
                names(&groups.server_wide)
            ),
            (
                vec!["Andor: the heists".to_owned(), "Movie night".to_owned()],
                vec!["Road trip".to_owned()],
                vec!["New this month".to_owned()],
            )
        );
    }

    #[test]
    fn a_third_user_sees_only_the_server_wide_one() {
        let client = FakeClient::new(None);
        let list = ready(session(&client, "mum").playlists())
            .unwrap()
            .playlists;
        // Of others' playlists; Liked and All are her own.
        assert_eq!(
            list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Liked", "All", "New this month"]
        );
    }

    #[test]
    fn whose_it_is_reads_from_the_summary() {
        let client = FakeClient::new(None);
        let andrey = ready(session(&client, "andrey").playlists())
            .unwrap()
            .playlists;
        let alice = ready(session(&client, "alice").playlists())
            .unwrap()
            .playlists;
        assert_eq!(
            [
                owner_line(named(&andrey, "Andor: the heists")),
                owner_line(named(&andrey, "Movie night")),
                owner_line(named(&andrey, "Road trip")),
                owner_line(named(&alice, "Andor: the heists")),
                owner_line(named(&alice, "New this month")),
                owner_line(named(&andrey, "New this month")),
            ],
            [
                "Yours · shared with 1 person",
                "Yours",
                "Shared with you · you can edit",
                "Shared with you · read only",
                "Server-wide",
                "Server-wide · you can edit",
            ]
        );
    }

    #[test]
    fn kinds_are_listed_in_order() {
        assert_eq!(
            [
                kinds_line(episodes()),
                kinds_line(Kinds {
                    movies: true,
                    episodes: true,
                    tracks: false
                }),
                kinds_line(Kinds {
                    tracks: true,
                    ..Kinds::default()
                }),
            ],
            ["Episodes", "Movies, episodes", "Music"]
        );
    }

    #[test]
    fn pinned_follows_the_users_order_and_a_new_pin_goes_last() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let night = named(&list, "Movie night").id.clone();
        ready(andrey.pin_playlist(&night, new_pin_position(&list))).unwrap();
        let list = ready(andrey.playlists()).unwrap().playlists;
        assert_eq!(names(&pinned(&list)), ["Andor: the heists", "Movie night"]);
        // Dragged to the top.
        ready(andrey.pin_playlist(&night, 0)).unwrap();
        let list = ready(andrey.playlists()).unwrap().playlists;
        assert_eq!(names(&pinned(&list)), ["Movie night", "Andor: the heists"]);
    }

    #[test]
    fn pins_are_the_users_on_every_device() {
        let client = FakeClient::new(None);
        let night = {
            let list = ready(session(&client, "andrey").playlists())
                .unwrap()
                .playlists;
            named(&list, "Movie night").id.clone()
        };
        ready(session(&client, "andrey").pin_playlist(&night, 0)).unwrap();
        // Another device asks the same server for the same user.
        let elsewhere = FakeClient::profile("andrey");
        let list = ready(client.session(&elsewhere).unwrap().playlists())
            .unwrap()
            .playlists;
        assert_eq!(named(&list, "Movie night").pinned, Some(0));
    }

    #[test]
    fn a_film_is_offered_no_episode_or_music_playlist() {
        let client = FakeClient::new(None);
        let list = ready(session(&client, "andrey").playlists())
            .unwrap()
            .playlists;
        let offered: Vec<_> = add_targets(&list, MediaKind::Movie)
            .into_iter()
            .map(|(p, offer)| (p.name.as_str(), offer))
            .collect();
        assert_eq!(
            offered,
            [
                ("Movie night", Offer::Adds),
                ("New this month", Offer::Adds)
            ]
        );
    }

    #[test]
    fn read_only_and_server_wide_ones_are_offered_greyed_last() {
        let client = FakeClient::new(None);
        let list = ready(session(&client, "alice").playlists())
            .unwrap()
            .playlists;
        let offered: Vec<_> = add_targets(&list, MediaKind::Season)
            .into_iter()
            .map(|(p, offer)| (p.name.as_str(), offer.reason()))
            .collect();
        assert_eq!(offered, [("Andor: the heists", Some("Read only"))]);
        let films: Vec<_> = add_targets(&list, MediaKind::Movie)
            .into_iter()
            .map(|(p, offer)| (p.name.as_str(), offer.reason()))
            .collect();
        assert_eq!(
            films,
            [("New this month", Some("Server-wide · admins edit"))]
        );
    }

    #[test]
    fn the_header_counts_by_kind_and_says_what_is_playable() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let night = page(andrey.as_ref(), &named(&list, "Movie night").id);
        let heists = named(&list, "Andor: the heists").id.clone();
        client.make_unavailable(&media(MediaKind::Episode, "65"));
        assert_eq!(
            (
                count_line(&night),
                count_line(&page(andrey.as_ref(), &heists))
            ),
            (
                "2 films, 1 episode".to_owned(),
                "3 episodes · 2 playable".to_owned()
            )
        );
    }

    #[test]
    fn an_unavailable_entry_is_listed_and_skipped() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        client.make_unavailable(&media(MediaKind::Episode, "64"));
        let shown = page(andrey.as_ref(), &heists);
        assert_eq!(
            entry_note(&shown.entries[0], up_next(&shown)).as_deref(),
            Some("Unavailable · the file can’t be read; skipped")
        );
        let first = ready(andrey.play_playlist(&heists, None, PlaySettings::default()))
            .unwrap()
            .unwrap();
        assert_eq!(first.item.id(), &media(MediaKind::Episode, "65"));
    }

    #[test]
    fn continue_starts_at_the_place_and_passes_it_once_watched() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        assert_eq!(continue_label(&page(andrey.as_ref(), &heists)), None);
        let first = ready(andrey.play_playlist(&heists, None, PlaySettings::default()))
            .unwrap()
            .unwrap();
        assert_eq!(
            continue_label(&page(andrey.as_ref(), &heists)).as_deref(),
            Some("S1 · E3 Reckoning")
        );
        ready(andrey.mark_watched(first.item.id())).unwrap();
        assert_eq!(
            continue_label(&page(andrey.as_ref(), &heists)).as_deref(),
            Some("S1 · E4 Aldhani")
        );
    }

    #[test]
    fn a_playlist_plays_in_order_to_its_end_then_starts_from_the_top() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        let mut played = Vec::new();
        let mut next = ready(andrey.play_playlist(&heists, None, PlaySettings::default())).unwrap();
        while let Some(now) = next {
            played.push(now.item.id().clone());
            next = ready(andrey.next_in_playlist(&heists, now.entry)).unwrap();
        }
        assert_eq!(
            played,
            ["64", "65", "72"].map(|k| media(MediaKind::Episode, k))
        );
        assert_eq!(page(andrey.as_ref(), &heists).place, None);
    }

    /// Andrey playing the heists from the top with `settings`.
    fn playing_heists(andrey: &dyn Session, settings: PlaySettings) -> (Playing, Item) {
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").clone();
        let first = ready(andrey.play_playlist(&heists.id, None, settings))
            .unwrap()
            .unwrap();
        let playing = Playing {
            id: heists.id,
            name: heists.name,
            entry: first.entry,
        };
        (playing, first.item)
    }

    fn after(andrey: &dyn Session, playing: &mut Playing) -> Advance {
        let next = ready(andrey.next_in_playlist(&playing.id, playing.entry)).unwrap();
        playing.advance(next)
    }

    #[test]
    fn an_entry_that_ends_plays_the_next_and_follows_it() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let (mut playing, _) = playing_heists(andrey.as_ref(), PlaySettings::default());
        let first = playing.entry;
        let advance = after(andrey.as_ref(), &mut playing);
        let Advance::Play { item, .. } = advance else {
            panic!("the heists ended after one: {advance:?}")
        };
        assert_eq!(
            (item.id().clone(), playing.entry != first),
            (media(MediaKind::Episode, "65"), true)
        );
    }

    #[test]
    fn the_last_entry_ends_the_playlist_and_continue_starts_at_the_top() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let (mut playing, _) = playing_heists(andrey.as_ref(), PlaySettings::default());
        let mut advances = Vec::new();
        for _ in 0..3 {
            advances.push(matches!(
                after(andrey.as_ref(), &mut playing),
                Advance::Finished
            ));
        }
        assert_eq!(
            (advances, page(andrey.as_ref(), &playing.id).place),
            (vec![false, false, true], None)
        );
    }

    #[test]
    fn repeat_one_plays_the_same_entry_again() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let settings = PlaySettings {
            repeat: Repeat::One,
            ..PlaySettings::default()
        };
        let (mut playing, first) = playing_heists(andrey.as_ref(), settings);
        let entry = playing.entry;
        let advance = after(andrey.as_ref(), &mut playing);
        assert_eq!(
            (advance, playing.entry),
            (
                Advance::Play {
                    from: resume_from(&first),
                    item: Box::new(first)
                },
                entry
            )
        );
    }

    #[test]
    fn an_item_part_watched_resumes_and_a_watched_one_starts_over() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let (_, mut item) = playing_heists(andrey.as_ref(), PlaySettings::default());
        let Item::Episode(e) = &mut item else {
            panic!("the heists hold episodes")
        };
        e.playback.position = Some(Duration::from_secs(90));
        let part = resume_from(&item);
        if let Item::Episode(e) = &mut item {
            e.playback.watched = true;
        }
        assert_eq!(
            (part, resume_from(&item)),
            (Some(Duration::from_secs(90)), None)
        );
    }

    #[test]
    fn the_lines_name_the_playlist() {
        let playing = Playing {
            id: "1".into(),
            name: "the Heists".into(),
            entry: 1,
        };
        assert_eq!(
            (playing.line(), playing.finished_text()),
            (
                "Playing from the Heists".to_owned(),
                "End of the Heists".to_owned()
            )
        );
    }

    #[test]
    fn skip_watched_passes_a_watched_entry() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        ready(andrey.mark_watched(&media(MediaKind::Episode, "64"))).unwrap();
        let settings = PlaySettings {
            skip_watched: true,
            ..PlaySettings::default()
        };
        let first = ready(andrey.play_playlist(&heists, None, settings))
            .unwrap()
            .unwrap();
        assert_eq!(first.item.id(), &media(MediaKind::Episode, "65"));
        assert_eq!(page(andrey.as_ref(), &heists).settings, settings);
    }

    #[test]
    fn shuffle_with_repeat_all_plays_every_entry_once_per_round() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        let settings = PlaySettings {
            shuffle: true,
            repeat: Repeat::All,
            skip_watched: false,
        };
        let mut now = ready(andrey.play_playlist(&heists, None, settings))
            .unwrap()
            .unwrap();
        let mut played = vec![now.entry];
        for _ in 0..5 {
            now = ready(andrey.next_in_playlist(&heists, now.entry))
                .unwrap()
                .unwrap();
            played.push(now.entry);
        }
        let (mut one, mut two) = (played[..3].to_vec(), played[3..].to_vec());
        one.sort_unstable();
        two.sort_unstable();
        assert_eq!((one.len(), one.windows(2).all(|w| w[0] != w[1])), (3, true));
        assert_eq!(one, two);
    }

    #[test]
    fn a_film_is_refused_by_an_episodes_playlist_until_movies_are_ticked() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let made =
            ready(andrey.create_playlist("Heists", episodes(), Visibility::Personal)).unwrap();
        let alien = media(MediaKind::Movie, "1");
        let add = vec![PlaylistEdit::Add {
            id: alien.clone(),
            before: None,
        }];
        assert_eq!(
            ready(andrey.edit_playlist(&made.id, made.revision, add.clone())),
            Err(BackendError::KindNotAllowed(
                crate::backend::ItemKind::Movie
            ))
        );
        let widened = ready(andrey.update_playlist(
            &made.id,
            PlaylistUpdate {
                kinds: Some(Kinds {
                    movies: true,
                    ..episodes()
                }),
                ..PlaylistUpdate::default()
            },
        ))
        .unwrap();
        ready(andrey.edit_playlist(&made.id, widened.revision, add)).unwrap();
        assert_eq!(page(andrey.as_ref(), &made.id).entries[0].item.id(), &alien);
    }

    #[test]
    fn a_season_adds_each_of_its_episodes_in_order() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let made =
            ready(andrey.create_playlist("Andor", episodes(), Visibility::Personal)).unwrap();
        let add = vec![PlaylistEdit::Add {
            id: media(MediaKind::Season, "70"),
            before: None,
        }];
        ready(andrey.edit_playlist(&made.id, made.revision, add)).unwrap();
        let ids: Vec<_> = page(andrey.as_ref(), &made.id)
            .entries
            .iter()
            .map(|e| e.item.id().clone())
            .collect();
        assert_eq!(
            ids,
            ["71", "72", "73"].map(|k| media(MediaKind::Episode, k))
        );
    }

    #[test]
    fn a_drag_moves_the_entry_before_the_one_then_after_it() {
        assert_eq!(
            [
                move_edit(&[1, 2, 3, 4], 0, 2),
                move_edit(&[1, 2, 3, 4], 3, 0),
                move_edit(&[1, 2, 3, 4], 1, 3),
                move_edit(&[1, 2, 3, 4], 2, 2),
            ],
            [
                Some(PlaylistEdit::Move {
                    entry: 1,
                    before: Some(4)
                }),
                Some(PlaylistEdit::Move {
                    entry: 4,
                    before: Some(1)
                }),
                Some(PlaylistEdit::Move {
                    entry: 2,
                    before: None
                }),
                None,
            ]
        );
    }

    #[test]
    fn a_second_editors_add_is_redone_on_the_new_revision() {
        let client = FakeClient::new(None);
        let (andrey, alice) = (session(&client, "andrey"), session(&client, "alice"));
        let list = ready(alice.playlists()).unwrap().playlists;
        let road = named(&list, "Road trip").clone();
        let shown = page(alice.as_ref(), &road.id);
        // Alice removes the first; Andrey, on the same revision, adds one.
        ready(alice.edit_playlist(
            &road.id,
            road.revision,
            vec![PlaylistEdit::Remove {
                entry: shown.entries[0].entry,
            }],
        ))
        .unwrap();
        let add = vec![PlaylistEdit::Add {
            id: media(MediaKind::Track, "48"),
            before: None,
        }];
        ready(edit_retrying(andrey.as_ref(), &road.id, road.revision, add)).unwrap();
        assert_eq!(page(andrey.as_ref(), &road.id).total, 3);
    }

    #[test]
    fn a_move_of_an_entry_the_other_removed_is_refused_after_one_retry() {
        let client = FakeClient::new(None);
        let (andrey, alice) = (session(&client, "andrey"), session(&client, "alice"));
        let list = ready(alice.playlists()).unwrap().playlists;
        let road = named(&list, "Road trip").clone();
        let first = page(alice.as_ref(), &road.id).entries[0].entry;
        ready(alice.edit_playlist(
            &road.id,
            road.revision,
            vec![PlaylistEdit::Remove { entry: first }],
        ))
        .unwrap();
        let moved = vec![PlaylistEdit::Move {
            entry: first,
            before: None,
        }];
        assert_eq!(
            ready(andrey.edit_playlist(&road.id, road.revision, moved.clone())),
            Err(BackendError::PlaylistConflict {
                revision: road.revision + 1
            })
        );
        // Redone on the new revision, the entry is gone.
        assert!(matches!(
            ready(edit_retrying(
                andrey.as_ref(),
                &road.id,
                road.revision,
                moved
            )),
            Err(BackendError::NotFound(_))
        ));
    }

    #[test]
    fn a_read_only_user_cannot_edit_and_an_editor_cannot_rename() {
        let client = FakeClient::new(None);
        let (andrey, alice) = (session(&client, "andrey"), session(&client, "alice"));
        let list = ready(alice.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").clone();
        let road = ready(andrey.playlists()).unwrap().playlists;
        let road = named(&road, "Road trip").clone();
        assert_eq!(
            (
                ready(alice.edit_playlist(
                    &heists.id,
                    heists.revision,
                    vec![PlaylistEdit::Add {
                        id: media(MediaKind::Episode, "62"),
                        before: None
                    }]
                )),
                ready(andrey.update_playlist(
                    &road.id,
                    PlaylistUpdate {
                        name: Some("Mine now".into()),
                        ..PlaylistUpdate::default()
                    }
                ))
                .map(|_| ()),
            ),
            (
                Err(BackendError::PlaylistReadOnly),
                Err(BackendError::PlaylistReadOnly)
            )
        );
    }

    #[test]
    fn an_unshared_pin_leaves_pinned_with_a_note_once() {
        let client = FakeClient::new(None);
        let (andrey, alice) = (session(&client, "andrey"), session(&client, "alice"));
        let heists = {
            let list = ready(alice.playlists()).unwrap().playlists;
            named(&list, "Andor: the heists").id.clone()
        };
        ready(alice.pin_playlist(&heists, 0)).unwrap();
        ready(andrey.update_playlist(
            &heists,
            PlaylistUpdate {
                visibility: Some(Visibility::Personal),
                ..PlaylistUpdate::default()
            },
        ))
        .unwrap();
        let seen = ready(alice.playlists()).unwrap();
        assert_eq!(
            (
                pinned(&seen.playlists).len(),
                pin_note_text(&seen.pin_notes)
            ),
            (
                0,
                Some("Andor: the heists is no longer shared with you, so it left Pinned.".into())
            )
        );
        assert_eq!(seen.pin_notes[0].reason, PinNoteReason::Unshared);
        ready(alice.dismiss_pin_notes()).unwrap();
        assert!(ready(alice.playlists()).unwrap().pin_notes.is_empty());
    }

    #[test]
    fn an_items_details_list_the_playlists_holding_it_or_its_episodes() {
        let client = FakeClient::new(None);
        let refs = |username: &str, id: MediaId| -> Vec<String> {
            ready(session(&client, username).item_detail(&id))
                .unwrap()
                .playlists
                .into_iter()
                .map(|p| p.name)
                .collect()
        };
        let andor = media(MediaKind::Show, "60");
        assert_eq!(
            (
                refs("andrey", andor.clone()),
                refs("alice", andor.clone()),
                refs("mum", andor),
                refs("mum", media(MediaKind::Movie, "2")),
                refs("andrey", media(MediaKind::Album, "31")),
            ),
            (
                vec!["Andor: the heists".to_owned()],
                vec!["Andor: the heists".to_owned()],
                vec![],
                vec!["New this month".to_owned()],
                vec!["Road trip".to_owned()],
            )
        );
    }

    #[test]
    fn a_server_without_the_feature_shows_no_playlists() {
        let client = FakeClient::new(None).without_playlists();
        let andrey = session(&client, "andrey");
        assert_eq!(
            (
                andrey.has_playlists(),
                ready(andrey.playlists()).map(|_| ()),
                ready(andrey.item_detail(&media(MediaKind::Show, "60")))
                    .unwrap()
                    .playlists,
            ),
            (
                false,
                Err(BackendError::Unsupported("spoolframe:playlists".into())),
                vec![],
            )
        );
    }

    #[test]
    fn a_change_elsewhere_asks_to_read_the_list_and_the_page_shown_again() {
        let client = FakeClient::new(None);
        let mut events = client.playlist_events();
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let night = named(&list, "Movie night").clone();
        ready(andrey.update_playlist(
            &night.id,
            PlaylistUpdate {
                name: Some("Film night".into()),
                ..PlaylistUpdate::default()
            },
        ))
        .unwrap();
        let event = events.try_recv().unwrap();
        let server = event.server.clone();
        assert_eq!(
            (
                refresh_for(&event, &server, Some(&night.id)),
                refresh_for(&event, &server, Some("another")),
                refresh_for(&event, "another server", Some(&night.id)),
            ),
            (
                Refresh {
                    list: true,
                    page: true,
                    page_gone: false
                },
                Refresh {
                    list: true,
                    page: false,
                    page_gone: false
                },
                Refresh::default(),
            )
        );
        ready(andrey.delete_playlist(&night.id)).unwrap();
        let gone = events.try_recv().unwrap();
        assert!(refresh_for(&gone, &server, Some(&night.id)).page_gone);
    }

    /// Andrey with Movie night pinned after the heists, and the list.
    fn two_pinned(client: &FakeClient) -> (Arc<dyn Session>, Vec<PlaylistSummary>) {
        let andrey = session(client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let night = named(&list, "Movie night").id.clone();
        ready(andrey.pin_playlist(&night, new_pin_position(&list))).unwrap();
        let list = ready(andrey.playlists()).unwrap().playlists;
        (andrey, list)
    }

    #[test]
    fn a_pin_dropped_on_the_first_row_goes_first_here_and_on_the_server() {
        let client = FakeClient::new(None);
        let (andrey, mut list) = two_pinned(&client);
        let (id, position) = pin_drop(&pinned(&list), 1, 0).unwrap();
        with_pin_at(&mut list, &id, position);
        ready(andrey.pin_playlist(&id, position)).unwrap();
        let server = ready(andrey.playlists()).unwrap().playlists;
        assert_eq!(
            (names(&pinned(&list)), names(&pinned(&server))),
            (
                vec!["Movie night".to_owned(), "Andor: the heists".to_owned()],
                vec!["Movie night".to_owned(), "Andor: the heists".to_owned()],
            )
        );
    }

    #[test]
    fn a_pin_dropped_on_itself_sends_nothing() {
        let client = FakeClient::new(None);
        let (_, list) = two_pinned(&client);
        assert_eq!(pin_drop(&pinned(&list), 1, 1), None);
    }

    #[test]
    fn a_pin_dropped_past_the_last_row_goes_last() {
        let client = FakeClient::new(None);
        let (_, list) = two_pinned(&client);
        let heists = named(&list, "Andor: the heists").id.clone();
        assert_eq!(pin_drop(&pinned(&list), 0, 7), Some((heists, 1)));
    }

    /// Alice pinned the heists, and Andrey stopped sharing it.
    fn alice_lost_a_pin(client: &FakeClient) -> Arc<dyn Session> {
        let (andrey, alice) = (session(client, "andrey"), session(client, "alice"));
        let list = ready(alice.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        ready(alice.pin_playlist(&heists, 0)).unwrap();
        ready(andrey.update_playlist(
            &heists,
            PlaylistUpdate {
                visibility: Some(Visibility::Personal),
                ..PlaylistUpdate::default()
            },
        ))
        .unwrap();
        alice
    }

    #[test]
    fn a_pin_note_is_shown_once_though_the_list_is_read_again() {
        let client = FakeClient::new(None);
        let alice = alice_lost_a_pin(&client);
        let mut shown = PinNotesShown::default();
        let first = ready(alice.playlists()).unwrap().pin_notes;
        let again = ready(alice.playlists()).unwrap().pin_notes;
        assert_eq!(
            (shown.fresh(&first), shown.fresh(&again)),
            (
                Some("Andor: the heists is no longer shared with you, so it left Pinned.".into()),
                None
            )
        );
    }

    #[test]
    fn a_dismissed_pin_note_is_gone_from_the_next_list() {
        let client = FakeClient::new(None);
        let alice = alice_lost_a_pin(&client);
        let mut shown = PinNotesShown::default();
        let notes = ready(alice.playlists()).unwrap().pin_notes;
        if shown.fresh(&notes).is_some() {
            ready(alice.dismiss_pin_notes()).unwrap();
        }
        assert!(ready(alice.playlists()).unwrap().pin_notes.is_empty());
    }

    fn entries_of(session: &dyn Session, id: &str) -> Vec<MediaId> {
        page(session, id)
            .entries
            .iter()
            .map(|e| e.item.id().clone())
            .collect()
    }

    #[test]
    fn a_season_added_reports_each_entry_it_made_even_one_already_there() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        let added = ready(add_item(
            andrey.as_ref(),
            &heists,
            &media(MediaKind::Season, "70"),
        ))
        .unwrap();
        let shown = page(andrey.as_ref(), &heists);
        let items: Vec<_> = shown
            .entries
            .iter()
            .filter(|e| added.contains(&e.entry))
            .map(|e| e.item.id().clone())
            .collect();
        assert_eq!(
            items,
            ["71", "72", "73"].map(|k| media(MediaKind::Episode, k))
        );
    }

    #[test]
    fn undo_of_an_add_leaves_the_playlist_as_it_was() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        let before = entries_of(andrey.as_ref(), &heists);
        let added = ready(add_item(
            andrey.as_ref(),
            &heists,
            &media(MediaKind::Season, "70"),
        ))
        .unwrap();
        ready(remove_entries(andrey.as_ref(), &heists, &added)).unwrap();
        assert_eq!(entries_of(andrey.as_ref(), &heists), before);
    }

    #[test]
    fn a_refused_add_says_why_in_the_boards_words() {
        let client = FakeClient::new(None);
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        let heists = named(&list, "Andor: the heists").id.clone();
        let film = ready(andrey.item(&media(MediaKind::Movie, "1"))).unwrap().0;
        let refused = ready(add_item(andrey.as_ref(), &heists, film.id())).unwrap_err();
        assert_eq!(
            add_refusal(&refused, &film, "the Heists"),
            format!("{} can’t go in the Heists", title(&film))
        );
    }

    #[test]
    fn a_new_playlist_from_an_item_takes_its_kind() {
        assert_eq!(
            [
                kinds_for(MediaKind::Season),
                kinds_for(MediaKind::Album),
                kinds_for(MediaKind::Movie)
            ],
            [
                episodes(),
                Kinds {
                    tracks: true,
                    ..Kinds::default()
                },
                Kinds {
                    movies: true,
                    ..Kinds::default()
                },
            ]
        );
    }

    #[test]
    fn the_editor_shows_what_each_user_may_change() {
        let client = FakeClient::new(None);
        let andrey = ready(session(&client, "andrey").playlists())
            .unwrap()
            .playlists;
        let alice = ready(session(&client, "alice").playlists())
            .unwrap()
            .playlists;
        let controls = |p: &PlaylistSummary, admin| {
            let c = editor_controls(p, admin);
            (c.edit, c.panel, c.server_wide)
        };
        assert_eq!(
            [
                // The owner, an admin.
                controls(named(&andrey, "Movie night"), true),
                // An editor of Alice's.
                controls(named(&andrey, "Road trip"), true),
                // Read only.
                controls(named(&alice, "Andor: the heists"), false),
                // Server-wide, seen by a user.
                controls(named(&alice, "New this month"), false),
                // Server-wide, for its admin.
                controls(named(&andrey, "New this month"), true),
            ],
            [
                (true, true, true),
                (true, false, false),
                (false, false, false),
                (false, false, false),
                (true, true, true),
            ]
        );
    }

    fn shares(users: &[(&str, bool)]) -> Visibility {
        Visibility::Shared {
            with: users
                .iter()
                .map(|(user, can_edit)| PlaylistShare::new(*user, *can_edit))
                .collect(),
        }
    }

    #[test]
    fn a_share_can_be_made_an_editor_or_stopped() {
        let now = shares(&[("local:a", false), ("local:b", true)]);
        assert_eq!(
            (
                share_change(&now, &ShareChange::CanEdit("local:a".into(), true)),
                share_change(&now, &ShareChange::StopSharing("local:b".into())),
            ),
            (
                shares(&[("local:a", true), ("local:b", true)]),
                shares(&[("local:a", false)]),
            )
        );
    }

    #[test]
    fn stopping_the_last_share_makes_it_personal() {
        let now = shares(&[("local:a", false)]);
        assert_eq!(
            share_change(&now, &ShareChange::StopSharing("local:a".into())),
            Visibility::Personal
        );
    }

    #[test]
    fn personal_and_server_wide_drop_the_shares() {
        let now = shares(&[("local:a", false)]);
        assert_eq!(
            (
                share_change(&now, &ShareChange::Personal),
                share_change(&now, &ShareChange::ServerWide)
            ),
            (Visibility::Personal, Visibility::ServerWide)
        );
    }

    #[test]
    fn a_new_playlist_needs_a_name_and_a_kind() {
        let kinds = episodes();
        assert_eq!(
            (
                new_playlist("  ", kinds, false),
                new_playlist("Heists", Kinds::default(), false),
                new_playlist(" Heists ", kinds, true),
            ),
            (
                Err("Give the playlist a name."),
                Err("Tick at least one kind it holds."),
                Ok(("Heists".to_owned(), kinds, Visibility::ServerWide)),
            )
        );
    }

    fn album() -> MediaId {
        media(MediaKind::Album, "31")
    }

    fn track(n: u32) -> MediaId {
        media(MediaKind::Track, &format!("4{n}"))
    }

    fn heart(session: &dyn Session, id: &MediaId) -> Option<Liked> {
        ready(session.item_detail(id)).unwrap().liked
    }

    fn builtin(session: &dyn Session, which: Builtin) -> PlaylistSummary {
        ready(session.playlists())
            .unwrap()
            .playlists
            .into_iter()
            .find(|p| p.builtin == Some(which))
            .unwrap()
    }

    #[test]
    fn liked_and_all_are_listed_first_apart_from_the_users_own() {
        let client = FakeClient::new(None);
        let list = ready(session(&client, "andrey").playlists())
            .unwrap()
            .playlists;
        let groups = sections(&list);
        assert_eq!(
            (names(&groups.builtin), names(&groups.mine)),
            (
                vec!["Liked".to_owned(), "All".to_owned()],
                vec!["Andor: the heists".to_owned(), "Movie night".to_owned()]
            )
        );
    }

    #[test]
    fn liked_and_all_say_they_are_built_in() {
        let andrey = session(&FakeClient::new(None), "andrey");
        assert_eq!(
            [
                owner_line(&builtin(andrey.as_ref(), Builtin::Liked)),
                owner_line(&builtin(andrey.as_ref(), Builtin::All)),
            ],
            ["Built in", "Built in · every track"]
        );
    }

    #[test]
    fn an_album_is_offered_liked_and_all_greyed_saying_why() {
        let client = FakeClient::new(None);
        let list = ready(session(&client, "andrey").playlists())
            .unwrap()
            .playlists;
        let offered: Vec<_> = add_targets(&list, MediaKind::Album)
            .into_iter()
            .map(|(p, offer)| (p.name.as_str(), offer.reason()))
            .collect();
        assert_eq!(
            offered,
            [
                ("Road trip", None),
                ("Liked", Some("Use the heart instead")),
                ("All", Some("Built in · every track already")),
            ]
        );
    }

    #[test]
    fn a_liked_track_fills_its_heart_and_half_fills_its_albums() {
        let andrey = session(&FakeClient::new(None), "andrey");
        ready(andrey.like(&[track(2)])).unwrap();
        assert_eq!(
            (
                heart(andrey.as_ref(), &track(2)),
                heart(andrey.as_ref(), &album())
            ),
            (Some(Liked::All), Some(Liked::Some))
        );
    }

    #[test]
    fn liking_an_album_likes_every_track_and_unliking_takes_them_out() {
        let andrey = session(&FakeClient::new(None), "andrey");
        ready(andrey.like(&[track(2)])).unwrap();
        ready(andrey.like(&[album()])).unwrap();
        let full = heart(andrey.as_ref(), &album());
        ready(andrey.unlike(&[album()])).unwrap();
        assert_eq!(
            (full, heart(andrey.as_ref(), &album())),
            (Some(Liked::All), Some(Liked::None))
        );
    }

    #[test]
    fn an_albums_details_name_its_liked_tracks() {
        let andrey = session(&FakeClient::new(None), "andrey");
        ready(andrey.like(&[track(4), track(1)])).unwrap();
        let detail = ready(andrey.item_detail(&album())).unwrap();
        assert_eq!(detail.liked_children, [track(1), track(4)]);
    }

    #[test]
    fn the_newest_liked_is_first_in_liked() {
        let andrey = session(&FakeClient::new(None), "andrey");
        ready(andrey.like(&[track(1)])).unwrap();
        let liked = ready(andrey.like(&[track(5)])).unwrap();
        let entries: Vec<MediaId> = page(andrey.as_ref(), &liked.id)
            .entries
            .into_iter()
            .map(|e| e.item.id().clone())
            .collect();
        assert_eq!(entries, [track(5), track(1)]);
    }

    #[test]
    fn liked_entries_carry_their_hearts() {
        let andrey = session(&FakeClient::new(None), "andrey");
        let liked = ready(andrey.like(&[track(1)])).unwrap();
        let hearts: Vec<bool> = page(andrey.as_ref(), &liked.id)
            .entries
            .iter()
            .map(|e| e.liked)
            .collect();
        assert_eq!(hearts, [true]);
    }

    #[test]
    fn only_tracks_and_albums_are_liked() {
        let andrey = session(&FakeClient::new(None), "andrey");
        assert_eq!(
            ready(andrey.like(&[media(MediaKind::Artist, "30")])),
            Err(BackendError::KindNotAllowed(
                crate::backend::ItemKind::Artist
            ))
        );
    }

    #[test]
    fn liked_and_all_are_never_renamed_or_deleted_and_all_never_edited() {
        let andrey = session(&FakeClient::new(None), "andrey");
        let rename = PlaylistUpdate {
            name: Some("Mine".into()),
            ..PlaylistUpdate::default()
        };
        let (liked, all) = (
            builtin(andrey.as_ref(), Builtin::Liked),
            builtin(andrey.as_ref(), Builtin::All),
        );
        let first = page(andrey.as_ref(), &all.id).entries[0].entry;
        assert_eq!(
            [
                ready(andrey.update_playlist(&liked.id, rename.clone())).err(),
                ready(andrey.delete_playlist(&all.id)).err(),
                ready(andrey.edit_playlist(
                    &all.id,
                    all.revision,
                    vec![PlaylistEdit::Remove { entry: first }]
                ))
                .err(),
            ],
            [
                Some(BackendError::PlaylistReadOnly),
                Some(BackendError::PlaylistReadOnly),
                Some(BackendError::PlaylistReadOnly),
            ]
        );
    }

    #[test]
    fn a_track_gets_into_liked_by_its_heart_not_an_add() {
        let andrey = session(&FakeClient::new(None), "andrey");
        let liked = builtin(andrey.as_ref(), Builtin::Liked);
        let added = ready(andrey.edit_playlist(
            &liked.id,
            liked.revision,
            vec![PlaylistEdit::Add {
                id: track(1),
                before: None,
            }],
        ));
        assert!(
            matches!(added, Err(BackendError::InvalidInput(_))),
            "{added:?}"
        );
    }

    #[test]
    fn liked_is_edited_but_has_no_panel_and_all_is_neither() {
        let andrey = session(&FakeClient::new(None), "andrey");
        let controls = |which| editor_controls(&builtin(andrey.as_ref(), which), true);
        assert_eq!(
            [controls(Builtin::Liked), controls(Builtin::All)],
            [
                EditorControls {
                    edit: true,
                    panel: false,
                    server_wide: false
                },
                EditorControls {
                    edit: false,
                    panel: false,
                    server_wide: false
                },
            ]
        );
    }

    #[test]
    fn an_album_with_a_liked_track_lists_liked_and_never_all() {
        let andrey = session(&FakeClient::new(None), "andrey");
        ready(andrey.like(&[track(3)])).unwrap();
        let refs: Vec<_> = ready(andrey.item_detail(&album()))
            .unwrap()
            .playlists
            .into_iter()
            .map(|r| (r.name, r.builtin))
            .collect();
        assert_eq!(
            refs,
            [
                ("Liked".to_owned(), Some(Builtin::Liked)),
                ("Road trip".to_owned(), None)
            ]
        );
    }

    #[test]
    fn without_liked_there_are_no_builtins_and_no_hearts() {
        let client = FakeClient::new(None).with_features(crate::backend::Features {
            playlists: true,
            ..crate::backend::Features::default()
        });
        let andrey = session(&client, "andrey");
        let list = ready(andrey.playlists()).unwrap().playlists;
        assert_eq!(
            (
                sections(&list).builtin.len(),
                heart(andrey.as_ref(), &album()),
                ready(andrey.like(&[track(1)])).err()
            ),
            (
                0,
                None,
                Some(BackendError::Unsupported("spoolframe:liked".into()))
            )
        );
    }

    #[test]
    fn a_full_heart_unlikes_and_any_other_likes() {
        assert_eq!(
            [Liked::All, Liked::Some, Liked::None].map(heart_likes),
            [false, true, true]
        );
    }

    #[test]
    fn an_albums_heart_is_full_half_or_empty_by_its_tracks() {
        let tracks = [track(1), track(2)];
        let liked = |ids: &[MediaId]| ids.iter().cloned().collect();
        assert_eq!(
            [
                album_heart(&tracks, &liked(&[track(1), track(2)])),
                album_heart(&tracks, &liked(&[track(2)])),
                album_heart(&tracks, &liked(&[])),
            ],
            [Liked::All, Liked::Some, Liked::None]
        );
    }
}
