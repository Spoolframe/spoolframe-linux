//! A playback's quality, as the viewer chooses it (P6-06): Auto, for the
//! link; a bitrate; or Original, the file as it is. The choices, their
//! order and the settings form are the SDK's (`PlaybackQuality::choices`,
//! `choice_index`, `Display`/`FromStr`), one list for every app; the words
//! are this app's.

use spoolframe_client_sdk::PlaybackQuality;

/// A choice as the picker names it. Auto says what it picked when the
/// server converts ("Auto (720p)"); plain "Auto" when it plays the file.
pub fn label(quality: PlaybackQuality, auto_height: Option<u32>) -> String {
    match quality {
        PlaybackQuality::Auto => match auto_height {
            Some(height) => format!("Auto ({height}p)"),
            None => "Auto".to_owned(),
        },
        PlaybackQuality::Bitrate(bits) => format!("{} Mbit/s", bits / 1_000_000),
        PlaybackQuality::Original => "Original".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_run_from_auto_through_the_bitrates_to_original() {
        let labels: Vec<String> = PlaybackQuality::choices()
            .iter()
            .map(|q| label(*q, None))
            .collect();
        assert_eq!(
            labels,
            [
                "Auto",
                "20 Mbit/s",
                "8 Mbit/s",
                "4 Mbit/s",
                "2 Mbit/s",
                "1 Mbit/s",
                "Original"
            ]
        );
    }

    #[test]
    fn auto_names_the_height_the_server_encodes_to() {
        assert_eq!(label(PlaybackQuality::Auto, Some(720)), "Auto (720p)");
    }
}
