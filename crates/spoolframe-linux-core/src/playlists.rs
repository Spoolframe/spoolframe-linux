//! Playlists (PX-40) as the screens show them, without GTK: the lines
//! under a playlist's name and its entries, what a greyed "Add to
//! playlist" row says, and what an event asks to read again. The rules
//! (groups, Pinned, offers, a drag's edit, Continue, sharing) are the
//! SDK's [`spoolframe_client_sdk::playlists::view`]; this words them.
//! The server keeps everything; this only reads its answers.
//!
//! Playlist summaries carry no counts (only a playlist's own page does),
//! so lines from a summary say whose it is and what it holds, never how
//! many.

use std::ops::ControlFlow;
use std::time::Duration;

use spoolframe_types::{Item, MediaId};

use crate::backend::{
    BackendError, Builtin, EntryId, Kinds, PlaylistChange, PlaylistEdit, PlaylistEvent,
    PlaylistNext, PlaylistOwnerKind, PlaylistPage, PlaylistShare, PlaylistSummary, Session,
    ShareCandidate, UserKind, Visibility,
};

pub use spoolframe_client_sdk::PLAYLIST_PAGE as PAGE;
pub use spoolframe_client_sdk::playlists::view::{
    EditorControls, EntryNote, NewPlaylist, NewPlaylistError, Offer, PageCounts, PinNotesShown,
    ShareChange, add_targets, continue_entry, editor_controls, entry_note, kinds_for, move_edit,
    new_playlist, page_counts, pin_drop, share_change, with_pin_at,
};

/// A playlist's name, or what is typed to search them, as a search
/// compares it: lowercase, without the spaces around it.
pub fn search_text(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Whether a playlist whose name is `name` shows for a search of `query`,
/// both as [`search_text`] gives them: its name holds what is typed. An
/// empty search shows every one.
pub fn matches_query(name: &str, query: &str) -> bool {
    name.contains(query)
}

/// Whether a playlist of `kinds` holds music and nothing else: it plays
/// as music does, with no place kept and no watched to skip.
pub fn music_only(kinds: Kinds) -> bool {
    kinds.tracks && !kinds.movies && !kinds.episodes
}

/// Watch-state skipping is a video option. Clear a previously saved value
/// for music-only playlists as well as hiding its control in the UI.
pub fn play_settings(
    kinds: Kinds,
    mut settings: crate::backend::PlaySettings,
) -> crate::backend::PlaySettings {
    if music_only(kinds) {
        settings.skip_watched = false;
    }
    settings
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
        // Whose it is, by name, where the server says (sharing by name).
        (PlaylistOwnerKind::SharedWithMe, _) => {
            let whose = match &playlist.owner_name {
                Some(owner) => format!("{owner}’s"),
                None => "Shared with you".to_owned(),
            };
            match playlist.can_edit {
                true => format!("{whose} · you can edit"),
                false => format!("{whose} · read only"),
            }
        }
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

/// Why a greyed "Add to playlist" row cannot take the item; `None` for
/// one that can.
pub fn offer_reason(offer: Offer) -> Option<&'static str> {
    match offer {
        Offer::Adds => None,
        Offer::ReadOnly => Some("Read only"),
        Offer::AdminsEdit => Some("Server-wide · admins edit"),
        Offer::UseHeart => Some("Use the heart instead"),
        Offer::HasEverything => Some("Built in · every track already"),
    }
}

/// A playlist's header line from its page: "7 episodes", "5 films, 2
/// episodes", "18 episodes · 17 playable". By kind only when the page has
/// every entry; a longer playlist says how many items.
pub fn count_line(page: &PlaylistPage) -> String {
    let (counts, playable) = match page_counts(page) {
        PageCounts::Empty => return "Empty".to_owned(),
        PageCounts::ByKind {
            films,
            episodes,
            tracks,
            playable,
        } => (
            [
                (films, "film", "films"),
                (episodes, "episode", "episodes"),
                (tracks, "track", "tracks"),
            ]
            .into_iter()
            .filter(|(n, _, _)| *n > 0)
            .map(|(n, one, many)| plural(n, one, many))
            .collect::<Vec<_>>()
            .join(", "),
            playable,
        ),
        PageCounts::Items { total, playable } => {
            (plural(total as usize, "item", "items"), playable)
        }
    };
    match playable {
        Some(playable) => format!("{counts} · {playable} playable"),
        None => counts,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// What Continue says it plays: "S4 · E6 Halloween IV", or a title.
pub fn continue_label(page: &PlaylistPage) -> Option<String> {
    let next = continue_entry(page)?;
    let entry = page.entries.iter().find(|e| e.entry == next)?;
    Some(match &entry.item {
        Item::Episode(e) => format!("S{} · E{} {}", e.season, e.episode, e.title),
        item => title(item),
    })
}

/// The line under an entry, from [`entry_note`].
pub fn note_text(note: EntryNote) -> String {
    match note {
        EntryNote::Unavailable => "Unavailable · the file can’t be read; skipped".to_owned(),
        EntryNote::UpNext => "Up next · Continue starts here".to_owned(),
        EntryNote::Watched => "Watched".to_owned(),
        EntryNote::Length(length) => crate::format::length(length),
    }
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

/// Whether the user has watched (or played to its end) `item`.
pub fn watched(item: &Item) -> bool {
    match item {
        Item::Movie(m) => m.playback.watched,
        Item::Episode(e) => e.playback.watched,
        Item::Track(t) => t.playback.watched,
        _ => false,
    }
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

/// What "New playlist" says it still needs.
pub fn new_playlist_refusal(error: NewPlaylistError) -> &'static str {
    match error {
        NewPlaylistError::NoName => "Give the playlist a name.",
        NewPlaylistError::NoKind => "Tick at least one kind it holds.",
    }
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
    for_each_page(session, id, from, PAGE, |page| {
        after.extend(entry_ids(&page));
        ControlFlow::<()>::Continue(())
    })
    .await?;
    Ok(added_entries(&before, &after))
}

/// Reads playlist `id` from entry `from` to its end, `size` entries a
/// page, handing each page to `each` until it breaks: what it broke
/// with, or `None` at the end.
pub async fn for_each_page<B>(
    session: &dyn Session,
    id: &str,
    from: u32,
    size: u32,
    mut each: impl FnMut(PlaylistPage) -> ControlFlow<B>,
) -> Result<Option<B>, BackendError> {
    let mut offset = from;
    loop {
        let page = session.playlist(id, offset, size).await?;
        let read = page.offset + page.entries.len() as u32;
        let end = page.entries.is_empty() || read >= page.total;
        if let ControlFlow::Break(done) = each(page) {
            return Ok(Some(done));
        }
        if end {
            return Ok(None);
        }
        offset = read;
    }
}

/// How many entries from a random place Play random looks through for
/// one that can play, before it reads the whole list for one.
const RANDOM_LOOK: u32 = 20;

/// An entry of playlist `id` that can play, at random (Play random);
/// `random(n)` is a number below `n`. It reads one page from a random
/// place, not the whole list: an entry that cannot play gives the next
/// that can. Only past [`RANDOM_LOOK`] of them in a row is every entry
/// read, to choose among those that can play. `None` when none can.
pub async fn random_entry(
    session: &dyn Session,
    id: &str,
    mut random: impl FnMut(u32) -> u32,
) -> Result<Option<EntryId>, BackendError> {
    let head = session.playlist(id, 0, 1).await?;
    if head.total == 0 {
        return Ok(None);
    }
    let at = random(head.total).min(head.total - 1);
    let near = session.playlist(id, at, RANDOM_LOOK).await?;
    if let Some(entry) = near.entries.iter().find(|e| !e.unavailable) {
        return Ok(Some(entry.entry));
    }
    let mut playable = Vec::new();
    for_each_page(session, id, 0, PAGE, |page| {
        playable.extend(
            page.entries
                .iter()
                .filter(|e| !e.unavailable)
                .map(|e| e.entry),
        );
        ControlFlow::<()>::Continue(())
    })
    .await?;
    if playable.is_empty() {
        return Ok(None);
    }
    let pick = (random(playable.len() as u32) as usize).min(playable.len() - 1);
    Ok(Some(playable[pick]))
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

/// Who someone a playlist is shared with is shown as: their name where
/// the server says it (sharing by name), else their user id.
pub fn share_name(share: &PlaylistShare) -> &str {
    share.display_name.as_deref().unwrap_or(&share.user_id)
}

/// Whose account someone is, as a badge: an account here or a portable
/// user; nothing for a kind this build does not know.
pub fn user_kind_badge(kind: UserKind) -> Option<&'static str> {
    match kind {
        UserKind::Local => Some("Local"),
        UserKind::Portable => Some("Portable"),
        UserKind::Unknown => None,
    }
}

/// The line under a share candidate's name: "@alice", or for a portable
/// user, who has no username here, what they are.
pub fn candidate_line(candidate: &ShareCandidate) -> String {
    match &candidate.username {
        Some(username) => format!("@{username}"),
        None => "Portable identity".to_owned(),
    }
}

/// A playlist being played: which, and the entry playing. The server
/// keeps the user's place and picks each next entry (shuffle, repeat and
/// skip watched are its); this only follows its answers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    use crate::testing::ready;

    use spoolframe_types::MediaKind;

    use super::*;
    use crate::backend::{PinNote, PinNoteReason, PlaySettings, PlaylistList};
    use crate::mock::{self, Edit, MockSession, media};

    #[test]
    fn a_search_finds_a_name_holding_it_whatever_its_case() {
        let name = search_text("Movie Night ");
        assert_eq!(
            [
                matches_query(&name, &search_text("  NIGHT")),
                matches_query(&name, &search_text("movie n")),
                matches_query(&name, &search_text("")),
                matches_query(&name, &search_text("nights")),
            ],
            [true, true, true, false]
        );
    }

    #[test]
    fn only_a_playlist_of_tracks_alone_is_music_only() {
        let kinds = |movies, episodes, tracks| Kinds {
            movies,
            episodes,
            tracks,
        };
        assert_eq!(
            [
                music_only(kinds(false, false, true)),
                music_only(kinds(true, false, true)),
                music_only(kinds(false, true, true)),
                music_only(kinds(false, true, false)),
                music_only(kinds(false, false, false)),
            ],
            [true, false, false, false, false]
        );
    }

    fn names(list: &[PlaylistSummary]) -> Vec<String> {
        list.iter().map(|p| p.name.clone()).collect()
    }

    fn episodes() -> Kinds {
        Kinds {
            episodes: true,
            ..Kinds::default()
        }
    }

    fn movies_and_episodes() -> Kinds {
        Kinds {
            movies: true,
            ..episodes()
        }
    }

    fn music() -> Kinds {
        Kinds {
            tracks: true,
            ..Kinds::default()
        }
    }

    /// The user's, shared with Alice to read.
    fn heists() -> PlaylistSummary {
        let alice = PlaylistShare {
            display_name: Some("Alice".into()),
            ..PlaylistShare::new("local:alice", false)
        };
        PlaylistSummary {
            visibility: Visibility::Shared { with: vec![alice] },
            ..mock::playlist("1", "Andor: the heists", episodes())
        }
    }

    /// The heists as Alice sees them: Andrey's, read only.
    fn heists_read_only() -> PlaylistSummary {
        PlaylistSummary {
            owner_kind: PlaylistOwnerKind::SharedWithMe,
            owner_name: Some("Andrey".into()),
            visibility: Visibility::Shared { with: Vec::new() },
            can_edit: false,
            can_manage: false,
            ..heists()
        }
    }

    fn movie_night() -> PlaylistSummary {
        mock::playlist("2", "Movie night", movies_and_episodes())
    }

    /// Alice's, shared with the user to edit.
    fn road_trip() -> PlaylistSummary {
        PlaylistSummary {
            owner_kind: PlaylistOwnerKind::SharedWithMe,
            owner_name: Some("Alice".into()),
            can_manage: false,
            ..mock::playlist("3", "Road trip", music())
        }
    }

    /// Server-wide films, as an admin or anyone else sees them.
    fn new_this_month(admin: bool) -> PlaylistSummary {
        PlaylistSummary {
            owner_kind: PlaylistOwnerKind::ServerWide,
            visibility: Visibility::ServerWide,
            can_edit: admin,
            can_manage: admin,
            ..mock::playlist(
                "4",
                "New this month",
                Kinds {
                    movies: true,
                    ..Kinds::default()
                },
            )
        }
    }

    fn liked() -> PlaylistSummary {
        PlaylistSummary {
            builtin: Some(Builtin::Liked),
            can_manage: false,
            ..mock::playlist("5", "Liked", music())
        }
    }

    fn all() -> PlaylistSummary {
        PlaylistSummary {
            builtin: Some(Builtin::All),
            can_edit: false,
            can_manage: false,
            ..mock::playlist("6", "All", music())
        }
    }

    /// The heists' three episodes, none watched.
    fn heists_page() -> PlaylistPage {
        mock::page(
            heists(),
            vec![
                mock::episode("64", "Andor", 1, 3, "Reckoning"),
                mock::episode("65", "Andor", 1, 4, "Aldhani"),
                mock::episode("72", "Andor", 2, 2, "Sagrona Teema"),
            ],
        )
    }

    /// `page` with `entries` unavailable, as the server says it.
    fn unavailable(mut page: PlaylistPage, entries: &[EntryId]) -> PlaylistPage {
        for entry in &mut page.entries {
            entry.unavailable = entries.contains(&entry.entry);
        }
        page.playable = page.total - entries.len() as u32;
        page
    }

    fn watch(item: &mut Item) {
        match item {
            Item::Movie(m) => m.playback.watched = true,
            Item::Episode(e) => e.playback.watched = true,
            Item::Track(t) => t.playback.watched = true,
            _ => {}
        }
    }

    fn add(id: MediaId) -> PlaylistEdit {
        PlaylistEdit::Add { id, before: None }
    }

    #[test]
    fn whose_it_is_reads_from_the_summary() {
        assert_eq!(
            [
                owner_line(&heists()),
                owner_line(&movie_night()),
                owner_line(&road_trip()),
                owner_line(&heists_read_only()),
                owner_line(&new_this_month(false)),
                owner_line(&new_this_month(true)),
            ],
            [
                "Yours · shared with 1 person",
                "Yours",
                "Alice’s · you can edit",
                "Andrey’s · read only",
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
    fn read_only_and_server_wide_ones_are_offered_greyed_last() {
        let list = [heists_read_only(), new_this_month(false), movie_night()];
        let offered = |kind| -> Vec<_> {
            add_targets(&list, kind)
                .into_iter()
                .map(|t| (t.playlist.name, offer_reason(t.offer)))
                .collect()
        };
        assert_eq!(
            (offered(MediaKind::Season), offered(MediaKind::Movie)),
            (
                vec![
                    ("Movie night".to_owned(), None),
                    ("Andor: the heists".to_owned(), Some("Read only")),
                ],
                vec![
                    ("Movie night".to_owned(), None),
                    (
                        "New this month".to_owned(),
                        Some("Server-wide · admins edit")
                    ),
                ]
            )
        );
    }

    #[test]
    fn the_header_counts_by_kind_and_says_what_is_playable() {
        let night = mock::page(
            movie_night(),
            vec![
                mock::movie("1", "Alien"),
                mock::movie("2", "Heat"),
                mock::episode("61", "Andor", 1, 1, "Kassa"),
            ],
        );
        assert_eq!(
            (
                count_line(&night),
                count_line(&unavailable(heists_page(), &[2]))
            ),
            (
                "2 films, 1 episode".to_owned(),
                "3 episodes · 2 playable".to_owned()
            )
        );
    }

    #[test]
    fn a_random_pick_is_the_entry_at_a_random_place() {
        let session = MockSession::new().with_playlist(heists_page());
        let picked = ready(random_entry(&session, "1", |n| n - 2)).unwrap();
        assert_eq!(picked, Some(2));
    }

    #[test]
    fn a_random_pick_of_an_entry_that_cannot_play_is_the_next_that_can() {
        let session = MockSession::new().with_playlist(unavailable(heists_page(), &[1]));
        let picked = ready(random_entry(&session, "1", |_| 0)).unwrap();
        assert_eq!(picked, Some(2));
    }

    #[test]
    fn a_random_pick_past_the_last_that_can_play_chooses_among_them_all() {
        let session = MockSession::new().with_playlist(unavailable(heists_page(), &[3]));
        // The last place first; then the second of the two that play.
        let mut asked = Vec::new();
        let picked = ready(random_entry(&session, "1", |n| {
            asked.push(n);
            n - 1
        }))
        .unwrap();
        assert_eq!((picked, asked), (Some(2), vec![3, 2]));
    }

    #[test]
    fn a_random_pick_where_nothing_can_play_is_none() {
        let session = MockSession::new().with_playlist(unavailable(heists_page(), &[1, 2, 3]));
        let picked = ready(random_entry(&session, "1", |_| 0)).unwrap();
        assert_eq!(picked, None);
    }

    #[test]
    fn every_page_is_read_from_where_asked_until_told_to_stop() {
        let session = MockSession::new().with_playlist(heists_page());
        let mut seen = Vec::new();
        let stopped = ready(for_each_page(&session, "1", 1, 1, |page| {
            seen.extend(page.entries.iter().map(|e| e.entry));
            ControlFlow::<()>::Continue(())
        }))
        .unwrap();
        assert_eq!((stopped, seen), (None, vec![2, 3]));
        let first = ready(for_each_page(&session, "1", 0, 1, |page| {
            ControlFlow::Break(page.entries[0].entry)
        }))
        .unwrap();
        assert_eq!(first, Some(1));
    }

    #[test]
    fn an_unavailable_entry_says_it_is_skipped() {
        let shown = unavailable(heists_page(), &[1]);
        assert_eq!(
            entry_note(&shown.entries[0], continue_entry(&shown)).map(note_text),
            Some("Unavailable · the file can’t be read; skipped".to_owned())
        );
    }

    #[test]
    fn continue_starts_at_the_place_and_passes_it_once_watched() {
        let mut page = heists_page();
        let never_played = continue_label(&page);
        page.place = Some(1);
        let at_place = continue_label(&page);
        watch(&mut page.entries[0].item);
        assert_eq!(
            (
                never_played,
                at_place.as_deref(),
                continue_label(&page).as_deref()
            ),
            (None, Some("S1 · E3 Reckoning"), Some("S1 · E4 Aldhani"))
        );
    }

    #[test]
    fn continue_passes_entries_watched_after_the_place_too() {
        let mut page = heists_page();
        page.place = Some(1);
        watch(&mut page.entries[0].item);
        watch(&mut page.entries[1].item);
        assert_eq!(
            continue_label(&page).as_deref(),
            Some("S2 · E2 Sagrona Teema")
        );
    }

    fn playing_heists() -> Playing {
        Playing {
            id: "1".into(),
            name: "Andor: the heists".into(),
            entry: 1,
        }
    }

    #[test]
    fn an_entry_that_ends_plays_the_next_and_follows_it() {
        let mut playing = playing_heists();
        let aldhani = mock::episode("65", "Andor", 1, 4, "Aldhani");
        let advance = playing.advance(Some(PlaylistNext {
            entry: 2,
            item: aldhani.clone(),
            markers: Default::default(),
            liked: false,
        }));
        assert_eq!(
            (advance, playing.entry),
            (
                Advance::Play {
                    item: Box::new(aldhani),
                    from: None
                },
                2
            )
        );
    }

    #[test]
    fn music_does_not_skip_previously_played_tracks_with_saved_video_settings() {
        let saved = PlaySettings {
            skip_watched: true,
            ..PlaySettings::default()
        };
        assert_eq!(
            (
                play_settings(music(), saved).skip_watched,
                play_settings(movies_and_episodes(), saved).skip_watched
            ),
            (false, true)
        );
    }

    #[test]
    fn no_next_entry_ends_the_playlist() {
        let mut playing = playing_heists();
        assert_eq!(
            (playing.advance(None), playing.entry),
            (Advance::Finished, 1)
        );
    }

    #[test]
    fn an_item_part_watched_resumes_and_a_watched_one_starts_over() {
        let mut item = mock::episode("64", "Andor", 1, 3, "Reckoning");
        let Item::Episode(e) = &mut item else {
            unreachable!()
        };
        e.playback.position = Some(Duration::from_secs(90));
        let part = resume_from(&item);
        watch(&mut item);
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
    fn a_second_editors_add_is_redone_on_the_new_revision() {
        // Someone else changed it since revision 7: refused, then taken.
        let session = MockSession::new()
            .with_edit_answers([Err(BackendError::PlaylistConflict { revision: 8 }), Ok(9)]);
        let edits = vec![add(media(MediaKind::Track, "48"))];
        let done = ready(edit_retrying(&session, "3", 7, edits.clone()));
        let sent = |revision| Edit {
            id: "3".into(),
            revision,
            edits: edits.clone(),
        };
        assert_eq!((done, session.edits()), (Ok(9), vec![sent(7), sent(8)]));
    }

    #[test]
    fn a_move_of_an_entry_the_other_removed_is_refused_after_one_retry() {
        // A third try would be taken: there must not be one.
        let session = MockSession::new().with_edit_answers([
            Err(BackendError::PlaylistConflict { revision: 8 }),
            Err(BackendError::NotFound("entry 1".into())),
            Ok(9),
        ]);
        let moved = vec![PlaylistEdit::Move {
            entry: 1,
            before: None,
        }];
        let done = ready(edit_retrying(&session, "3", 7, moved));
        assert_eq!(
            (done, session.edits().len()),
            (Err(BackendError::NotFound("entry 1".into())), 2)
        );
    }

    #[test]
    fn a_pin_note_says_why_the_playlist_left_pinned() {
        let note = |name: &str, reason| PinNote {
            playlist_id: "1".into(),
            name: name.into(),
            reason,
        };
        assert_eq!(
            (
                pin_note_text(&[
                    note("Andor: the heists", PinNoteReason::Unshared),
                    note("Movie night", PinNoteReason::Deleted),
                ]),
                pin_note_text(&[])
            ),
            (
                Some(
                    "Andor: the heists is no longer shared with you, so it left Pinned.\n\
                     Movie night was deleted, so it left Pinned."
                        .into()
                ),
                None
            )
        );
    }

    #[test]
    fn a_change_elsewhere_asks_to_read_the_list_and_the_page_shown_again() {
        let event = |change| PlaylistEvent {
            server: "server".into(),
            change,
        };
        let changed = event(PlaylistChange::Changed {
            id: "2".into(),
            revision: 2,
        });
        let gone = event(PlaylistChange::Gone { id: "2".into() });
        assert_eq!(
            (
                refresh_for(&changed, "server", Some("2")),
                refresh_for(&changed, "server", Some("another")),
                refresh_for(&changed, "another server", Some("2")),
                refresh_for(&gone, "server", Some("2")).page_gone,
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
                true,
            )
        );
    }

    #[test]
    fn a_pin_dropped_on_the_first_row_goes_first() {
        let mut list = PlaylistList {
            playlists: vec![
                PlaylistSummary {
                    pinned: Some(0),
                    ..heists()
                },
                PlaylistSummary {
                    pinned: Some(1),
                    ..movie_night()
                },
            ],
            pin_notes: Vec::new(),
        };
        let (id, position) = pin_drop(&list.pinned(), 1, 0).unwrap();
        with_pin_at(&mut list.playlists, &id, position);
        assert_eq!(
            ((id.as_str(), position), names(&list.pinned())),
            (
                ("2", 0),
                vec!["Movie night".to_owned(), "Andor: the heists".to_owned()]
            )
        );
    }

    #[test]
    fn a_season_added_reports_each_entry_it_made_even_one_already_there() {
        // Season 2 is 71, 72 and 73; 72 is in the heists already.
        let before = heists_page();
        let mut after = before.entries.clone();
        for (entry, key, number) in [(4, "71", 1), (5, "72", 2), (6, "73", 3)] {
            after.push(mock::entry_of(
                entry,
                mock::episode(key, "Andor", 2, number, key),
            ));
        }
        let session = MockSession::new()
            .with_playlist(before)
            .with_edit_answers([Ok(2)])
            .with_entries_after_edit("1", after);
        let season = media(MediaKind::Season, "70");
        let added = ready(add_item(&session, "1", &season)).unwrap();
        assert_eq!(
            (added, session.edits()),
            (
                vec![4, 5, 6],
                vec![Edit {
                    id: "1".into(),
                    revision: 1,
                    edits: vec![add(season)],
                }]
            )
        );
    }

    #[test]
    fn undo_of_an_add_removes_the_entries_it_made() {
        let mut page = heists_page();
        page.playlist.revision = 5;
        let session = MockSession::new()
            .with_playlist(page)
            .with_edit_answers([Ok(6)]);
        let done = ready(remove_entries(&session, "1", &[2, 3]));
        assert_eq!(
            (done, session.edits()),
            (
                Ok(6),
                vec![Edit {
                    id: "1".into(),
                    revision: 5,
                    edits: vec![
                        PlaylistEdit::Remove { entry: 2 },
                        PlaylistEdit::Remove { entry: 3 }
                    ],
                }]
            )
        );
    }

    #[test]
    fn a_refused_add_says_why_in_the_boards_words() {
        let session = MockSession::new()
            .with_playlist(heists_page())
            .with_edit_answers([Err(BackendError::KindNotAllowed(
                crate::backend::ItemKind::Movie,
            ))]);
        let film = mock::movie("1", "Alien");
        let refused = ready(add_item(&session, "1", film.id())).unwrap_err();
        assert_eq!(
            add_refusal(&refused, &film, "the Heists"),
            "Alien can’t go in the Heists"
        );
    }

    #[test]
    fn the_editor_shows_what_each_user_may_change() {
        let controls = |p: &PlaylistSummary, admin| {
            let c = editor_controls(p, admin);
            (c.edit, c.panel, c.server_wide)
        };
        assert_eq!(
            [
                // The owner, an admin.
                controls(&movie_night(), true),
                // An editor of Alice's.
                controls(&road_trip(), true),
                // Read only.
                controls(&heists_read_only(), false),
                // Server-wide, seen by a user.
                controls(&new_this_month(false), false),
                // Server-wide, for its admin.
                controls(&new_this_month(true), true),
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
                new_playlist("  ", kinds, false).map_err(new_playlist_refusal),
                new_playlist("Heists", Kinds::default(), false).map_err(new_playlist_refusal),
                new_playlist(" Heists ", kinds, true).map_err(new_playlist_refusal),
            ),
            (
                Err("Give the playlist a name."),
                Err("Tick at least one kind it holds."),
                Ok(NewPlaylist {
                    name: "Heists".to_owned(),
                    kinds,
                    visibility: Visibility::ServerWide
                }),
            )
        );
    }

    #[test]
    fn liked_and_all_are_listed_first_apart_from_the_users_own() {
        let list = PlaylistList {
            playlists: vec![
                heists(),
                liked(),
                road_trip(),
                movie_night(),
                all(),
                new_this_month(true),
            ],
            pin_notes: Vec::new(),
        };
        let groups = list.sections();
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
        assert_eq!(
            [owner_line(&liked()), owner_line(&all())],
            ["Built in", "Built in · every track"]
        );
    }

    #[test]
    fn an_album_is_offered_liked_and_all_greyed_saying_why() {
        let list = [liked(), all(), heists(), movie_night(), road_trip()];
        let offered: Vec<_> = add_targets(&list, MediaKind::Album)
            .into_iter()
            .map(|t| (t.playlist.name, offer_reason(t.offer)))
            .collect();
        assert_eq!(
            offered,
            [
                ("Road trip".to_owned(), None),
                ("Liked".to_owned(), Some("Use the heart instead")),
                ("All".to_owned(), Some("Built in · every track already")),
            ]
        );
    }

    #[test]
    fn liked_is_edited_but_has_no_panel_and_all_is_neither() {
        assert_eq!(
            [
                editor_controls(&liked(), true),
                editor_controls(&all(), true)
            ],
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
    fn whom_a_playlist_is_shared_with_is_named() {
        let Visibility::Shared { with } = heists().visibility else {
            unreachable!()
        };
        assert_eq!(share_name(&with[0]), "Alice");
    }

    #[test]
    fn someone_picked_is_shared_with_at_the_access_chosen() {
        let picked = ShareChange::Add {
            user: "local:mum".into(),
            can_edit: true,
        };
        let Visibility::Shared { with } = share_change(&heists().visibility, &picked) else {
            panic!("no longer shared")
        };
        let access: Vec<_> = with
            .iter()
            .map(|s| (s.user_id.as_str(), s.can_edit))
            .collect();
        assert_eq!(access, [("local:alice", false), ("local:mum", true)]);
    }

    #[test]
    fn a_share_with_no_name_shows_its_user_id() {
        assert_eq!(
            share_name(&PlaylistShare::new("local:gone", false)),
            "local:gone"
        );
    }

    #[test]
    fn a_candidate_says_their_username_or_that_they_are_portable() {
        let candidate = |username: Option<&str>, kind| ShareCandidate {
            user_id: "local:alice".into(),
            display_name: "Alice".into(),
            username: username.map(str::to_owned),
            kind,
        };
        assert_eq!(
            [
                (
                    candidate_line(&candidate(Some("alice"), UserKind::Local)),
                    user_kind_badge(UserKind::Local)
                ),
                (
                    candidate_line(&candidate(None, UserKind::Portable)),
                    user_kind_badge(UserKind::Portable)
                ),
            ],
            [
                ("@alice".to_owned(), Some("Local")),
                ("Portable identity".to_owned(), Some("Portable"))
            ]
        );
    }
}
