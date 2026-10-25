//! A playback's quality, as the viewer chooses it (P6-06): Auto, for the
//! link; a bitrate; or Original, the file as it is. The same choices as
//! the Apple apps (agreed with sf-ios), so one server offers one list.

use spoolframe_client_sdk::PlaybackQuality;
use spoolframe_protocol::{ConvertedVideo, PlaybackPlan};

/// The bitrates offered between Auto and Original, in Mbit/s.
const LADDER: [u64; 5] = [20, 8, 4, 2, 1];

/// Every choice, in the order shown.
pub fn choices() -> Vec<PlaybackQuality> {
    std::iter::once(PlaybackQuality::Auto)
        .chain(
            LADDER
                .iter()
                .map(|m| PlaybackQuality::Bitrate(m * 1_000_000)),
        )
        .chain(std::iter::once(PlaybackQuality::Original))
        .collect()
}

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

/// Where `quality` is among [`choices`]; a bitrate not offered (from a
/// newer app) shows as Auto.
pub fn index(quality: PlaybackQuality) -> usize {
    choices().iter().position(|c| *c == quality).unwrap_or(0)
}

/// The lines the server encodes to, when it encodes the video.
pub fn encoded_height(plan: Option<&PlaybackPlan>) -> Option<u32> {
    match plan? {
        PlaybackPlan::Converted {
            video: Some(ConvertedVideo::Encode { height, .. }),
            ..
        } => Some(*height),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_run_from_auto_through_the_bitrates_to_original() {
        let labels: Vec<String> = choices().into_iter().map(|q| label(q, None)).collect();
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

    #[test]
    fn a_bitrate_not_offered_shows_as_auto() {
        assert_eq!(index(PlaybackQuality::Bitrate(8_000_000)), 2);
        assert_eq!(index(PlaybackQuality::Bitrate(12_000_000)), 0);
        assert_eq!(index(PlaybackQuality::Original), 6);
    }

    #[test]
    fn only_an_encode_has_a_height_of_its_own() {
        let encode = PlaybackPlan::Converted {
            delivery: spoolframe_types::Delivery::Progressive,
            container: spoolframe_types::Container::Matroska,
            video: Some(ConvertedVideo::Encode {
                codec: spoolframe_types::VideoCodec::H264,
                height: 540,
                max_bitrate: Some(2_000_000),
                tone_map: false,
            }),
            audio: None,
            subtitle: None,
            reasons: Vec::new(),
            ladder: Vec::new(),
            source_codec: None,
        };
        assert_eq!(encoded_height(Some(&encode)), Some(540));
        assert_eq!(encoded_height(None), None);
    }
}
