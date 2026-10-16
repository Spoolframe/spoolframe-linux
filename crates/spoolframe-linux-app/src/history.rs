//! An item's history as its page says it: how often it was played and
//! when last (PX-52, `spoolframe:play-history`), else when it was last
//! viewed (`spoolframe:last-viewed`). A session without the features gets
//! neither field, so nothing is said.

use spoolframe_types::PlaybackState;

use crate::reachability::ago;

/// "Played 3 times · last 2 days ago" (or "Played once · 3 hours ago"),
/// else "Viewed 2 days ago" for an item viewed but never played through;
/// `None` when the server said neither. `now` is Unix seconds.
pub fn played_line(state: &PlaybackState, now: u64) -> Option<String> {
    let when = |at: i64| ago(at.max(0) as u64, now);
    match (state.play_count, state.last_played_at) {
        (0, _) => state
            .last_viewed_at
            .map(|at| format!("Viewed {}", when(at))),
        (1, Some(at)) => Some(format!("Played once · {}", when(at))),
        (1, None) => Some("Played once".to_owned()),
        (n, Some(at)) => Some(format!("Played {n} times · last {}", when(at))),
        (n, None) => Some(format!("Played {n} times")),
    }
}

/// Unix seconds now, for [`played_line`].
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn state(play_count: u32, last_played: Option<i64>, last_viewed: Option<i64>) -> PlaybackState {
        PlaybackState {
            play_count,
            last_played_at: last_played,
            last_viewed_at: last_viewed,
            ..PlaybackState::default()
        }
    }

    #[test]
    fn plays_are_counted_with_when_the_last_was() {
        assert_eq!(
            played_line(&state(3, Some(NOW as i64 - 2 * DAY), None), NOW).as_deref(),
            Some("Played 3 times · last 2 days ago")
        );
    }

    #[test]
    fn one_play_says_once() {
        assert_eq!(
            played_line(&state(1, Some(NOW as i64 - 3 * 3600), None), NOW).as_deref(),
            Some("Played once · 3 hours ago")
        );
    }

    #[test]
    fn plays_win_over_a_later_view() {
        assert_eq!(
            played_line(&state(2, Some(NOW as i64 - 5 * DAY), Some(NOW as i64)), NOW).as_deref(),
            Some("Played 2 times · last 5 days ago")
        );
    }

    #[test]
    fn an_item_viewed_and_never_played_through_says_when_it_was_viewed() {
        assert_eq!(
            played_line(&state(0, None, Some(NOW as i64 - DAY)), NOW).as_deref(),
            Some("Viewed 1 day ago")
        );
    }

    #[test]
    fn an_item_the_server_said_nothing_of_has_no_line() {
        assert_eq!(played_line(&PlaybackState::default(), NOW), None);
    }
}
