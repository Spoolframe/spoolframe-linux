//! Text the UI shows for media values.

use std::time::Duration;

/// How much of an item is left, as "1 h 20 min left" or "18 min left".
/// Rounds up, so an item is never shown as having "0 min" left before its
/// end.
pub fn remaining(length: Duration, position: Duration) -> String {
    let secs = length.saturating_sub(position).as_secs();
    let minutes = secs.div_ceil(60);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min left"),
        (h, 0) => format!("{h} h left"),
        (h, m) => format!("{h} h {m} min left"),
    }
}

/// An item's length, as "1 h 57 min", "2 h" or "50 min".
pub fn length(length: Duration) -> String {
    let minutes = length.as_secs().div_ceil(60);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A position as a clock reading, "37:12" or "1:02:03", as players show it.
pub fn timestamp(position: Duration) -> String {
    let secs = position.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The name of an ISO 639-2 language code, for the common ones; the code
/// itself, upper-cased, for the rest.
pub fn language(code: &str) -> String {
    let name = match code {
        "eng" => "English",
        "fre" | "fra" => "French",
        "ger" | "deu" => "German",
        "spa" => "Spanish",
        "ita" => "Italian",
        "por" => "Portuguese",
        "rus" => "Russian",
        "ukr" => "Ukrainian",
        "pol" => "Polish",
        "dut" | "nld" => "Dutch",
        "swe" => "Swedish",
        "nor" => "Norwegian",
        "dan" => "Danish",
        "fin" => "Finnish",
        "jpn" => "Japanese",
        "kor" => "Korean",
        "chi" | "zho" => "Chinese",
        "und" => "Unknown language",
        other => return other.to_uppercase(),
    };
    name.to_owned()
}

/// An audio layout: "stereo", "5.1", "3 channels".
pub fn channels(n: u32) -> String {
    match n {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        6 => "5.1".to_owned(),
        8 => "7.1".to_owned(),
        n => format!("{n} channels"),
    }
}

/// A bitrate in bits per second, in whole megabits: "12 Mbit/s".
pub fn bitrate(bits: u64) -> String {
    format!("{:.0} Mbit/s", bits as f64 / 1_000_000.0)
}

/// An audio track as a picker shows it: "English · Commentary · stereo".
/// The role is named unless it is the main sound; a title that only
/// repeats the role is left out.
pub fn audio_track(track: &spoolframe_types::AudioTrack) -> String {
    use spoolframe_types::AudioRole;
    let role = match track.role {
        AudioRole::Main => None,
        AudioRole::AudioDescription => Some("Audio description"),
        AudioRole::Commentary => Some("Commentary"),
        AudioRole::Other => None,
    };
    let parts: Vec<String> = [
        track.language.as_deref().map(language),
        role.map(str::to_owned),
        track
            .title
            .clone()
            .filter(|t| role.is_none_or(|r| !t.eq_ignore_ascii_case(r))),
        track.channels.map(channels),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "Audio".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// A subtitle track as a picker shows it: "English · SDH", "English ·
/// Forced · external file".
pub fn subtitle_track(track: &spoolframe_types::SubtitleTrack) -> String {
    use spoolframe_types::SubtitleRole;
    let role = match track.role {
        SubtitleRole::Sdh => Some("SDH"),
        SubtitleRole::Forced => Some("Forced"),
        SubtitleRole::Commentary => Some("Commentary"),
        SubtitleRole::Full => None,
    };
    let parts: Vec<String> = [
        track.language.as_deref().map(language),
        role.map(str::to_owned),
        track
            .title
            .clone()
            .filter(|t| role.is_none_or(|r| !t.eq_ignore_ascii_case(r))),
        track.external.then(|| "external file".to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "Subtitles".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// A preference's languages as typed in a field: "original, eng, ger".
pub fn language_list(languages: &[String]) -> String {
    languages.join(", ")
}

/// What was typed in a languages field: codes separated by commas or
/// spaces, lower-cased, each once, in the order given.
pub fn parse_language_list(text: &str) -> Vec<String> {
    let mut languages: Vec<String> = Vec::new();
    for code in text
        .split([',', ' '])
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        let code = code.to_lowercase();
        if !languages.contains(&code) {
            languages.push(code);
        }
    }
    languages
}

/// Why the server picked a track (or none), for a line under the pickers.
pub fn track_reason(reason: spoolframe_protocol::TrackReason) -> Option<&'static str> {
    use spoolframe_protocol::TrackReason as R;
    Some(match reason {
        R::Override => "your choice for this series or movie",
        R::Preferences => "your preferences",
        R::FileDefault => "the file's default",
        R::NotWanted => "not wanted by your preferences",
        R::NoneMatched => "nothing matched your preferences",
        R::NoTracks | R::Unknown => return None,
    })
}

/// Where playback is, as a fraction of the item, for progress bars.
pub fn progress(length: Duration, position: Duration) -> f64 {
    if length.is_zero() {
        return 0.0;
    }
    (position.as_secs_f64() / length.as_secs_f64()).clamp(0.0, 1.0)
}

/// A file as stored, "MKV · HEVC 2160p HDR10", from the server's probe;
/// `None` when it probed nothing. The picture's class goes by its width,
/// so a 2.40:1 4K film (3840 by 1600) is 2160p.
pub fn file(
    container: Option<spoolframe_types::Container>,
    video: Option<&spoolframe_types::VideoTrack>,
) -> Option<String> {
    use spoolframe_types::{Container, HdrFormat, VideoCodec};
    let container = container.and_then(|c| match c {
        Container::Matroska => Some("MKV"),
        Container::Mp4 => Some("MP4"),
        Container::MpegTs => Some("TS"),
        Container::Avi => Some("AVI"),
        Container::Flac => Some("FLAC"),
        Container::Mp3 => Some("MP3"),
        Container::Ogg => Some("OGG"),
        Container::Unknown => None,
    });
    let picture = video.map(|v| {
        let codec = match v.codec {
            VideoCodec::H264 => "H.264",
            VideoCodec::Hevc => "HEVC",
            VideoCodec::Av1 => "AV1",
            VideoCodec::Vp9 => "VP9",
            VideoCodec::Vp8 => "VP8",
            VideoCodec::Mpeg2 => "MPEG-2",
            VideoCodec::Mpeg4 => "MPEG-4",
            VideoCodec::Vc1 => "VC-1",
            VideoCodec::Unknown => "",
        };
        let class = match v.width {
            w if w >= 3200 => "2160p".to_owned(),
            w if w >= 1800 => "1080p".to_owned(),
            w if w >= 1200 => "720p".to_owned(),
            _ => format!("{}p", v.height),
        };
        let hdr = v.hdr.map(|h| match h {
            HdrFormat::Hdr10 => "HDR10",
            HdrFormat::Hlg => "HLG",
            HdrFormat::DolbyVision => "Dolby Vision",
            HdrFormat::Unknown => "HDR",
        });
        [Some(codec), Some(class.as_str()), hdr]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    });
    let parts: Vec<String> = [container.map(str::to_owned), picture]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Music's sound as an album shows it: "FLAC 24/96" for lossless with
/// its depth and rate, the codec alone otherwise ("AAC").
pub fn audio_format(
    codec: Option<spoolframe_types::AudioCodec>,
    bit_depth: Option<u32>,
    sample_rate: Option<u32>,
) -> Option<String> {
    use spoolframe_types::AudioCodec;
    let name = match codec? {
        AudioCodec::Aac => "AAC",
        AudioCodec::Ac3 => "AC-3",
        AudioCodec::Eac3 => "E-AC-3",
        AudioCodec::Dts => "DTS",
        AudioCodec::TrueHd => "TrueHD",
        AudioCodec::Flac => "FLAC",
        AudioCodec::Alac => "ALAC",
        AudioCodec::Opus => "Opus",
        AudioCodec::Vorbis => "Vorbis",
        AudioCodec::Mp3 => "MP3",
        AudioCodec::Pcm => "PCM",
        AudioCodec::Unknown => return None,
    };
    let lossless = matches!(
        codec?,
        AudioCodec::Flac | AudioCodec::Alac | AudioCodec::Pcm
    );
    Some(match (lossless, bit_depth, sample_rate) {
        (true, Some(depth), Some(rate)) => {
            let khz = rate as f64 / 1000.0;
            let khz = if rate % 1000 == 0 {
                format!("{}", rate / 1000)
            } else {
                format!("{khz:.1}")
            };
            format!("{name} {depth}/{khz}")
        }
        _ => name.to_owned(),
    })
}

/// "1 season", "2 seasons".
pub fn seasons(n: u32) -> String {
    match n {
        1 => "1 season".to_owned(),
        n => format!("{n} seasons"),
    }
}

/// How the device reaches a server, as the chip beside its name says it:
/// "Direct · LAN", "Direct · internet", "Relayed" (P6-06's word); `None`
/// while it is not connected.
pub fn path_chip(path: crate::backend::ConnectionPath) -> Option<&'static str> {
    use crate::backend::ConnectionPath;
    match path {
        ConnectionPath::Lan(_) => Some("Direct · LAN"),
        ConnectionPath::Direct(_) => Some("Direct · internet"),
        ConnectionPath::Relay => Some("Relayed"),
        ConnectionPath::Unknown => None,
    }
}

/// The path and how fast the link has carried media lately, for the
/// player (P6-06): "Direct · LAN · 48 Mbit/s", "Relayed · 6.2 Mbit/s";
/// the path alone before anything was measured, and `None` while not
/// connected.
pub fn link_line(path: crate::backend::ConnectionPath, rate: Option<u64>) -> Option<String> {
    let path = path_chip(path)?;
    Some(match rate {
        Some(bits) => format!("{path} · {}", link_rate(bits)),
        None => path.to_owned(),
    })
}

/// A link's rate: to a tenth under 10 Mbit/s, where a tenth matters.
fn link_rate(bits: u64) -> String {
    let mbits = bits as f64 / 1_000_000.0;
    if mbits < 10.0 {
        format!("{mbits:.1} Mbit/s")
    } else {
        format!("{mbits:.0} Mbit/s")
    }
}

/// How the device reaches the server, for diagnostics: "LAN ·
/// 192.168.1.20:7431", "direct · 203.0.113.9:7431", "relay".
pub fn connection_path(path: crate::backend::ConnectionPath) -> String {
    use crate::backend::ConnectionPath;
    match path {
        ConnectionPath::Lan(addr) => format!("LAN · {addr}"),
        ConnectionPath::Direct(addr) => format!("direct · {addr}"),
        ConnectionPath::Relay => "relayed".to_owned(),
        ConnectionPath::Unknown => "—".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relayed_link_says_so_and_how_fast_it_is() {
        use crate::backend::ConnectionPath;
        assert_eq!(
            link_line(ConnectionPath::Relay, Some(6_200_000)).as_deref(),
            Some("Relayed · 6.2 Mbit/s")
        );
    }

    #[test]
    fn a_fast_link_is_rounded_and_an_unmeasured_one_is_the_path_alone() {
        use crate::backend::ConnectionPath;
        let lan = ConnectionPath::Lan(([10, 10, 0, 6], 7431).into());
        assert_eq!(
            link_line(lan, Some(48_400_000)).as_deref(),
            Some("Direct · LAN · 48 Mbit/s")
        );
        assert_eq!(link_line(lan, None).as_deref(), Some("Direct · LAN"));
        assert_eq!(link_line(ConnectionPath::Unknown, Some(1)), None);
    }

    #[test]
    fn a_public_path_is_not_called_lan() {
        use crate::backend::ConnectionPath;
        assert_eq!(
            path_chip(ConnectionPath::Direct(([203, 0, 113, 9], 7431).into())),
            Some("Direct · internet")
        );
    }

    #[test]
    fn a_local_path_says_lan_and_where() {
        use crate::backend::ConnectionPath;
        assert_eq!(
            connection_path(ConnectionPath::Lan(([10, 10, 0, 6], 7431).into())),
            "LAN · 10.10.0.6:7431"
        );
    }

    fn picture(
        width: u32,
        height: u32,
        hdr: Option<spoolframe_types::HdrFormat>,
    ) -> spoolframe_types::VideoTrack {
        spoolframe_types::VideoTrack {
            codec: spoolframe_types::VideoCodec::Hevc,
            width,
            height,
            bit_depth: 10,
            hdr,
        }
    }

    #[test]
    fn a_4k_hdr_file_reads_as_the_board_shows_it() {
        assert_eq!(
            file(
                Some(spoolframe_types::Container::Matroska),
                Some(&picture(
                    3840,
                    2160,
                    Some(spoolframe_types::HdrFormat::Hdr10)
                ))
            )
            .as_deref(),
            Some("MKV · HEVC 2160p HDR10")
        );
    }

    #[test]
    fn a_wide_film_is_classed_by_its_width() {
        assert_eq!(
            file(None, Some(&picture(3840, 1600, None))).as_deref(),
            Some("HEVC 2160p")
        );
    }

    #[test]
    fn nothing_probed_is_no_file_line() {
        assert_eq!(file(None, None), None);
    }

    #[test]
    fn lossless_music_shows_depth_and_rate() {
        use spoolframe_types::AudioCodec;
        assert_eq!(
            (
                audio_format(Some(AudioCodec::Flac), Some(24), Some(96_000)),
                audio_format(Some(AudioCodec::Flac), Some(16), Some(44_100)),
            ),
            (Some("FLAC 24/96".into()), Some("FLAC 16/44.1".into()))
        );
    }

    #[test]
    fn lossy_music_shows_the_codec_alone() {
        use spoolframe_types::AudioCodec;
        assert_eq!(
            audio_format(Some(AudioCodec::Aac), Some(16), Some(44_100)).as_deref(),
            Some("AAC")
        );
    }

    fn mins(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    #[test]
    fn under_an_hour_is_minutes_only() {
        assert_eq!(remaining(mins(50), mins(32)), "18 min left");
    }

    #[test]
    fn over_an_hour_has_hours_and_minutes() {
        assert_eq!(remaining(mins(117), mins(37)), "1 h 20 min left");
    }

    #[test]
    fn whole_hours_drop_the_minutes() {
        assert_eq!(remaining(mins(150), mins(30)), "2 h left");
    }

    #[test]
    fn a_partial_minute_counts_as_a_minute() {
        assert_eq!(
            remaining(mins(10), Duration::from_secs(9 * 60 + 50)),
            "1 min left"
        );
    }

    #[test]
    fn a_length_over_an_hour_has_hours_and_minutes() {
        assert_eq!(length(mins(117)), "1 h 57 min");
    }

    #[test]
    fn a_length_under_an_hour_is_minutes_only() {
        assert_eq!(length(mins(50)), "50 min");
    }

    #[test]
    fn a_timestamp_under_an_hour_is_minutes_and_seconds() {
        assert_eq!(timestamp(Duration::from_secs(37 * 60 + 12)), "37:12");
    }

    #[test]
    fn a_timestamp_over_an_hour_pads_minutes_and_seconds() {
        assert_eq!(timestamp(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn a_known_language_code_reads_as_its_name() {
        assert_eq!(language("ger"), "German");
    }

    #[test]
    fn an_unknown_language_code_is_shown_as_is() {
        assert_eq!(language("tlh"), "TLH");
    }

    #[test]
    fn an_audio_track_names_language_title_and_channels() {
        let track = spoolframe_types::AudioTrack {
            language: Some("eng".into()),
            title: Some("Commentary".into()),
            channels: Some(2),
            default: false,
            role: spoolframe_types::AudioRole::Commentary,
        };
        assert_eq!(audio_track(&track), "English · Commentary · stereo");
    }

    #[test]
    fn a_forced_subtitle_says_so_once() {
        let track = spoolframe_types::SubtitleTrack {
            language: Some("eng".into()),
            title: Some("Forced".into()),
            forced: true,
            default: false,
            external: false,
            role: spoolframe_types::SubtitleRole::Forced,
        };
        assert_eq!(subtitle_track(&track), "English · Forced");
    }

    #[test]
    fn an_external_subtitle_says_where_it_comes_from() {
        let track = spoolframe_types::SubtitleTrack {
            language: Some("eng".into()),
            title: None,
            forced: false,
            default: false,
            external: true,
            role: spoolframe_types::SubtitleRole::Full,
        };
        assert_eq!(subtitle_track(&track), "English · external file");
    }

    #[test]
    fn an_sdh_track_is_named_by_its_role() {
        let track = spoolframe_types::SubtitleTrack {
            language: Some("eng".into()),
            title: Some("English SDH".into()),
            forced: false,
            default: false,
            external: false,
            role: spoolframe_types::SubtitleRole::Sdh,
        };
        assert_eq!(subtitle_track(&track), "English · SDH · English SDH");
    }

    #[test]
    fn an_audio_description_track_says_so() {
        let track = spoolframe_types::AudioTrack {
            language: Some("eng".into()),
            title: None,
            channels: Some(2),
            default: false,
            role: spoolframe_types::AudioRole::AudioDescription,
        };
        assert_eq!(audio_track(&track), "English · Audio description · stereo");
    }

    #[test]
    fn a_language_list_reads_back_as_typed() {
        let languages = vec!["original".to_owned(), "eng".to_owned()];
        assert_eq!(parse_language_list(&language_list(&languages)), languages);
    }

    #[test]
    fn typed_languages_are_trimmed_lowered_and_deduplicated() {
        assert_eq!(parse_language_list(" ENG,ger  eng,, "), ["eng", "ger"]);
    }

    #[test]
    fn progress_is_the_watched_fraction() {
        assert_eq!(progress(mins(100), mins(25)), 0.25);
    }

    #[test]
    fn progress_of_an_item_without_a_length_is_zero() {
        assert_eq!(progress(Duration::ZERO, mins(5)), 0.0);
    }
}
