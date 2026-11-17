//! Play history (PX-52) as the app shows it, GTK-free: Home's Recently
//! played row from the server's cards (SDK 0.17.5), or, from a server
//! without it, "Recently played music" from the tracks the server lists
//! by their latest play.

use spoolframe_types::{Album, Item};

use crate::backend::{PlaylistSummary, RecentCard};

/// A card of Home's Recently played row, as the app draws it: an item,
/// opened as any card is, or a playlist, opened on its page.
#[derive(Debug, Clone, PartialEq)]
pub enum RowCard {
    // Boxed: an Item is several times a playlist's size.
    Item(Box<Item>),
    Playlist(PlaylistSummary),
}

/// The row's cards, in the server's order; a kind this build does not
/// know is left out.
pub fn row_cards(cards: Vec<RecentCard>) -> Vec<RowCard> {
    cards
        .into_iter()
        .filter_map(|card| {
            let item = match card {
                RecentCard::Movie { movie } => Item::Movie(movie),
                RecentCard::Show { show } => Item::Show(show),
                RecentCard::Season { season } => Item::Season(season),
                RecentCard::Episode { episode } => Item::Episode(episode),
                RecentCard::Album { album } => Item::Album(album),
                RecentCard::Track { track } => Item::Track(track),
                RecentCard::Playlist { playlist } => return Some(RowCard::Playlist(playlist)),
                RecentCard::Unknown => return None,
            };
            Some(RowCard::Item(Box::new(item)))
        })
        .collect()
}

/// The albums of `played`, the tracks [`crate::backend::Session::recently_played`]
/// lists newest first: each album once, where its latest-played track
/// is, at most `limit`. Made from the tracks' own album fields, so the
/// row needs no read per album; opening one reads the album whole. A
/// track that does not name its album is left out: there is nothing to
/// call the card.
pub fn recent_albums(played: &[Item], limit: usize) -> Vec<Album> {
    let mut albums: Vec<Album> = Vec::new();
    for item in played {
        if albums.len() == limit {
            break;
        }
        let Item::Track(track) = item else { continue };
        let Some(title) = &track.album_title else {
            continue;
        };
        if albums.iter().any(|a| a.id == track.album_id) {
            continue;
        }
        albums.push(Album {
            id: track.album_id.clone(),
            artist_id: track.artist_id.clone(),
            artist_name: track.artist_name.clone(),
            title: title.clone(),
            year: None,
            track_count: track.album_track_count,
            // The server gives a track its album's cover.
            artwork: track.artwork.clone(),
            // A track carries none of its album's merge facts; opening the
            // album reads them.
            merge: Default::default(),
        });
    }
    albums
}

#[cfg(test)]
mod tests {
    use super::*;
    use spoolframe_types::{MediaId, MediaKind, Namespace, PlaybackState, Track};

    fn id(kind: MediaKind, key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, kind, key).expect("test keys are not empty")
    }

    #[test]
    fn the_row_keeps_the_server_s_order_and_leaves_out_unknown_cards() {
        let Item::Movie(movie) = crate::mock::movie("m", "Heat") else {
            unreachable!()
        };
        let Item::Track(song) = crate::mock::track("t", "Song") else {
            unreachable!()
        };
        let mix = crate::mock::playlist("p", "Mix", crate::backend::Kinds::default());
        let cards = vec![
            RecentCard::Track {
                track: song.clone(),
            },
            RecentCard::Unknown,
            RecentCard::Playlist {
                playlist: mix.clone(),
            },
            RecentCard::Movie {
                movie: movie.clone(),
            },
        ];
        assert_eq!(
            row_cards(cards),
            [
                RowCard::Item(Box::new(Item::Track(song))),
                RowCard::Playlist(mix),
                RowCard::Item(Box::new(Item::Movie(movie))),
            ]
        );
    }

    fn track(key: &str, album: &str, album_title: Option<&str>) -> Item {
        Item::Track(Track {
            id: id(MediaKind::Track, key),
            album_id: id(MediaKind::Album, album),
            artist_id: id(MediaKind::Artist, "1"),
            artist_name: Some("The Night Shift".into()),
            album_title: album_title.map(str::to_owned),
            title: format!("Track {key}"),
            track: None,
            disc: None,
            album_track_count: Some(9),
            codec: None,
            bit_depth: None,
            sample_rate: None,
            duration: None,
            artwork: Vec::new(),
            playback: PlaybackState::default(),
            merge: Default::default(),
        })
    }

    fn titles(albums: &[Album]) -> Vec<&str> {
        albums.iter().map(|a| a.title.as_str()).collect()
    }

    #[test]
    fn each_album_is_listed_once_where_its_latest_track_is() {
        let played = [
            track("1", "a", Some("Low Tide")),
            track("2", "b", Some("Glasshouse")),
            track("3", "a", Some("Low Tide")),
        ];
        assert_eq!(
            titles(&recent_albums(&played, 10)),
            ["Low Tide", "Glasshouse"]
        );
    }

    #[test]
    fn no_more_albums_than_asked_for_are_listed() {
        let played = [
            track("1", "a", Some("Low Tide")),
            track("2", "b", Some("Glasshouse")),
            track("3", "c", Some("North Line")),
        ];
        assert_eq!(
            titles(&recent_albums(&played, 2)),
            ["Low Tide", "Glasshouse"]
        );
    }

    #[test]
    fn a_track_that_names_no_album_is_left_out() {
        let played = [track("1", "a", None), track("2", "b", Some("Glasshouse"))];
        assert_eq!(titles(&recent_albums(&played, 10)), ["Glasshouse"]);
    }

    #[test]
    fn the_album_is_the_one_the_track_is_on() {
        let albums = recent_albums(&[track("1", "a", Some("Low Tide"))], 10);
        assert_eq!(
            (albums[0].id.clone(), albums[0].artist_name.as_deref()),
            (id(MediaKind::Album, "a"), Some("The Night Shift"))
        );
    }
}
