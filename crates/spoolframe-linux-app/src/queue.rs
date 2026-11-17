//! What plays next (P1-23): the SDK's play queue, of [`Item`]s. It is
//! this app's queue moved there, and loads the queues this app saved.
//! No GStreamer here: [`crate::playback::Playback`] asks this what to play.

use spoolframe_linux_core::backend::Repeat;
use spoolframe_types::Item;

pub type Queue = spoolframe_client_sdk::queue::PlayQueue<Item>;
pub type Queued = spoolframe_client_sdk::queue::Queued<Item>;

/// Where what plays was started, told with each server start
/// (`PlaybackOptions::source`) for the server's Recently played row: a
/// track added by hand was queued; else the server playlist playing, if
/// any (`playlist`); else the album. The playback keeps the playlist, not
/// the queue, so it is named to the queue here, before the queue's own
/// rule is asked.
pub fn play_source(
    queue: &mut Queue,
    playlist: Option<String>,
) -> Option<spoolframe_client_sdk::PlaySource> {
    queue.set_context_playlist(playlist);
    queue.current_source()
}

/// The Repeat button's next state on a click: off, the whole context, the
/// one item, off again. One this build does not know goes on from off.
/// For a server playlist's repeat, which the queue does not hold.
pub fn next_repeat(repeat: Repeat) -> Repeat {
    match repeat {
        Repeat::Off | Repeat::Unknown => Repeat::All,
        Repeat::All => Repeat::One,
        Repeat::One => Repeat::Off,
    }
}

/// The Repeat button's icon.
pub fn repeat_icon(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::One => crate::icons::REPEAT_ONE,
        _ => crate::icons::REPEAT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_goes_round_off_all_one() {
        let mut seen = vec![Repeat::Off];
        for _ in 0..3 {
            seen.push(next_repeat(*seen.last().unwrap()));
        }
        assert_eq!(seen, [Repeat::Off, Repeat::All, Repeat::One, Repeat::Off]);
    }

    #[test]
    fn an_unknown_repeat_goes_on_from_off() {
        assert_eq!(next_repeat(Repeat::Unknown), Repeat::All);
    }

    use spoolframe_client_sdk::PlaySource;
    use spoolframe_linux_core::mock::{media, track};
    use spoolframe_types::{Album, MediaKind};

    fn album() -> Album {
        Album {
            id: media(MediaKind::Album, "a"),
            artist_id: media(MediaKind::Artist, "r"),
            artist_name: None,
            title: "Album".into(),
            year: None,
            track_count: Some(2),
            artwork: Vec::new(),
            merge: Default::default(),
        }
    }

    #[test]
    fn an_album_s_track_was_started_from_the_album() {
        let mut queue = Queue::new(vec![track("1", "One")], 0, Some(album()), Vec::new());
        assert_eq!(
            play_source(&mut queue, None),
            Some(PlaySource::Album {
                id: media(MediaKind::Album, "a")
            })
        );
    }

    #[test]
    fn a_playlist_playing_names_the_playlist_over_the_album() {
        let mut queue = Queue::new(vec![track("1", "One")], 0, Some(album()), Vec::new());
        assert_eq!(
            play_source(&mut queue, Some("pl".into())),
            Some(PlaySource::Playlist { id: "pl".into() })
        );
    }

    #[test]
    fn a_track_added_by_hand_was_queued_even_in_a_playlist() {
        let mut queue = Queue::new(Vec::new(), 0, None, Vec::new());
        queue.play_next(vec![track("2", "Two")], Some(album()));
        assert_eq!(
            play_source(&mut queue, Some("pl".into())),
            Some(PlaySource::Queued)
        );
    }

    #[test]
    fn a_film_with_no_album_or_playlist_has_no_source() {
        let mut queue = Queue::new(vec![track("1", "One")], 0, None, Vec::new());
        assert_eq!(play_source(&mut queue, None), None);
    }
}
