//! An item's history as its page says it: how often it was played and
//! when last (PX-52), else when it was last viewed. A state the server
//! sent without them says nothing.

use std::time::SystemTime;

use spoolframe_types::PlaybackState;

use crate::reachability::ago;

/// "Played 3 times · last 2 days ago" (or "Played once · 3 hours ago"),
/// else "Viewed 2 days ago" for an item viewed but never played through;
/// `None` when the server said neither.
pub fn played_line(state: &PlaybackState, now: SystemTime) -> Option<String> {
    let when = |at: SystemTime| ago(at, now);
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn state(
        play_count: u32,
        last_played: Option<SystemTime>,
        last_viewed: Option<SystemTime>,
    ) -> PlaybackState {
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
            played_line(&state(3, Some(now() - 2 * DAY), None), now()).as_deref(),
            Some("Played 3 times · last 2 days ago")
        );
    }

    #[test]
    fn one_play_says_once() {
        assert_eq!(
            played_line(
                &state(1, Some(now() - Duration::from_secs(3 * 3600)), None),
                now()
            )
            .as_deref(),
            Some("Played once · 3 hours ago")
        );
    }

    #[test]
    fn plays_win_over_a_later_view() {
        assert_eq!(
            played_line(&state(2, Some(now() - 5 * DAY), Some(now())), now()).as_deref(),
            Some("Played 2 times · last 5 days ago")
        );
    }

    #[test]
    fn an_item_viewed_and_never_played_through_says_when_it_was_viewed() {
        assert_eq!(
            played_line(&state(0, None, Some(now() - DAY)), now()).as_deref(),
            Some("Viewed 1 day ago")
        );
    }

    #[test]
    fn an_item_the_server_said_nothing_of_has_no_line() {
        assert_eq!(played_line(&PlaybackState::default(), now()), None);
    }
}
