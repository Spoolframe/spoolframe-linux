//! What plays next (P1-23): the SDK's play queue, of [`Item`]s. It is
//! this app's queue moved there, and loads the queues this app saved.
//! No GStreamer here: [`crate::playback::Playback`] asks this what to play.

use spoolframe_linux_core::backend::Repeat;
use spoolframe_types::Item;

pub type Queue = spoolframe_client_sdk::queue::PlayQueue<Item>;
pub type Queued = spoolframe_client_sdk::queue::Queued<Item>;

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
}
