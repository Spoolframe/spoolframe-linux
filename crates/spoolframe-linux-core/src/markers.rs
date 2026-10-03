//! What the player offers at a point of an item from its markers (P2-10):
//! Skip Intro inside an intro, Skip Credits inside credits that end before
//! the item does, Next Episode inside credits that run to the end when
//! something follows.
//!
//! Markers are in the item's own time, which is also a converted output's
//! time (the server encodes with -copyts), so positions compare directly.

use std::time::Duration;

use spoolframe_types::{Marker, MarkerKind};

/// A button over the video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Seek to the intro's end.
    SkipIntro { to: Duration },
    /// Seek to the end of credits that a scene follows.
    SkipCredits { to: Duration },
    /// Play what comes next: the credits run to the end.
    NextItem,
}

/// The offer at `at`, if any. `has_next` is whether something follows
/// the item (the queue, or the show's next episode). Credits that run to
/// the end with nothing after them offer nothing. A kind this build does
/// not know is passed over, so it cannot hide an intro it overlaps.
pub fn offer(markers: &[Marker], at: Duration, has_next: bool) -> Option<Offer> {
    let ms = at.as_millis() as u64;
    let marker = markers.iter().find(|m| {
        m.kind != MarkerKind::Unknown && m.start_ms <= ms && (ms < m.end_ms || m.to_end)
    })?;
    let to = Duration::from_millis(marker.end_ms);
    match marker.kind {
        MarkerKind::Intro => Some(Offer::SkipIntro { to }),
        MarkerKind::Credits if marker.to_end => has_next.then_some(Offer::NextItem),
        MarkerKind::Credits => Some(Offer::SkipCredits { to }),
        MarkerKind::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(kind: MarkerKind, start_ms: u64, end_ms: u64, to_end: bool) -> Marker {
        Marker {
            kind,
            start_ms,
            end_ms,
            to_end,
        }
    }

    fn episode() -> Vec<Marker> {
        vec![
            marker(MarkerKind::Intro, 60_000, 90_000, false),
            marker(MarkerKind::Credits, 1_200_000, 1_260_000, false),
            marker(MarkerKind::Credits, 2_600_000, 2_700_001, true),
        ]
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn inside_the_intro_it_offers_to_skip_to_its_end() {
        assert_eq!(
            offer(&episode(), secs(70), true),
            Some(Offer::SkipIntro { to: secs(90) })
        );
    }

    #[test]
    fn outside_every_marker_there_is_nothing() {
        assert_eq!(offer(&episode(), secs(100), true), None);
    }

    #[test]
    fn the_intros_end_is_already_past_it() {
        assert_eq!(offer(&episode(), secs(90), true), None);
    }

    #[test]
    fn credits_with_a_scene_after_offer_to_skip_them() {
        assert_eq!(
            offer(&episode(), secs(1_210), true),
            Some(Offer::SkipCredits { to: secs(1_260) })
        );
    }

    #[test]
    fn credits_to_the_end_offer_the_next_item() {
        assert_eq!(offer(&episode(), secs(2_650), true), Some(Offer::NextItem));
    }

    #[test]
    fn credits_to_the_end_with_nothing_after_offer_nothing() {
        assert_eq!(offer(&episode(), secs(2_650), false), None);
    }

    #[test]
    fn to_end_holds_past_a_marker_end_short_of_the_duration() {
        // Plex's end can be off the duration either way: go by to_end.
        let markers = vec![marker(MarkerKind::Credits, 100_000, 150_000, true)];
        assert_eq!(offer(&markers, secs(151), true), Some(Offer::NextItem));
    }

    #[test]
    fn an_unknown_kind_does_not_hide_an_intro_inside_it() {
        let markers = vec![
            marker(MarkerKind::Unknown, 0, 120_000, false),
            marker(MarkerKind::Intro, 60_000, 90_000, false),
        ];
        assert_eq!(
            offer(&markers, secs(70), true),
            Some(Offer::SkipIntro { to: secs(90) })
        );
    }

    #[test]
    fn an_unknown_kind_shows_nothing() {
        let markers = vec![marker(MarkerKind::Unknown, 0, 10_000, false)];
        assert_eq!(offer(&markers, secs(5), true), None);
    }
}
