//! Text the UI shows for media values: the SDK's parts
//! (`spoolframe_client_sdk::describe`), in this app's words.

use std::time::Duration;

use spoolframe_client_sdk::describe::{
    self, Channels, HoursMinutes, TrackLabel, TrackOrigin, TrackRole,
};

/// How much of an item is left, as "1 h 20 min left" or "18 min left".
/// Rounds up, so an item is never shown as having "0 min" left before its
/// end.
pub fn remaining(length: Duration, position: Duration) -> String {
    format!(
        "{} left",
        hours_minutes(describe::remaining(length, position))
    )
}

/// An item's length, as "1 h 57 min", "2 h" or "50 min".
pub fn length(length: Duration) -> String {
    hours_minutes(HoursMinutes::of(length))
}

fn hours_minutes(span: HoursMinutes) -> String {
    match (span.hours, span.minutes) {
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

/// The name of an ISO 639-2 language code, or of a two-letter one with
/// or without its region as subtitle sites name them (`en`, `pt-BR`,
/// PX-02), for the common ones; the code itself, upper-cased, for the
/// rest.
pub fn language(code: &str) -> String {
    language_name(&describe::language(code))
}

fn language_name(language: &describe::Language) -> String {
    if language.undetermined {
        return "Unknown language".to_owned();
    }
    let Some(name) = &language.english else {
        return language.code.to_uppercase();
    };
    match (language.iso639_1.as_deref(), language.region.as_deref()) {
        (Some("pt"), Some("BR")) => format!("{name} (Brazil)"),
        (Some("zh"), Some("TW")) => format!("{name} (Taiwan)"),
        _ => name.clone(),
    }
}

/// An audio layout: "stereo", "5.1", "3 channels".
pub fn channels(n: u32) -> String {
    match Channels::of(n) {
        Channels::Mono => "mono".to_owned(),
        Channels::Stereo => "stereo".to_owned(),
        Channels::Surround51 => "5.1".to_owned(),
        Channels::Surround71 => "7.1".to_owned(),
        Channels::Other(n) => format!("{n} channels"),
    }
}

/// A bitrate in bits per second, in whole megabits: "12 Mbit/s".
pub fn bitrate(bits: u64) -> String {
    format!("{:.0} Mbit/s", bits as f64 / 1_000_000.0)
}

/// An audio track as a picker shows it: "English · Commentary · stereo".
/// The role is named unless it is the main sound; a title that only
/// repeats the language or the role is left out.
pub fn audio_track(track: &spoolframe_types::AudioTrack) -> String {
    track_label(&describe::audio_track(track), "Audio")
}

/// A subtitle track as a picker shows it: "English · SDH", "English ·
/// Forced · external file".
pub fn subtitle_track(track: &spoolframe_types::SubtitleTrack) -> String {
    track_label(&describe::subtitle_track(track), "Subtitles")
}

/// `label`'s parts joined, or `none` when it has none.
fn track_label(label: &TrackLabel, none: &str) -> String {
    let role = label.role.map(|role| match role {
        TrackRole::AudioDescription => "Audio description",
        TrackRole::Commentary => "Commentary",
        TrackRole::Sdh => "SDH",
        TrackRole::Forced => "Forced",
    });
    // Forced by its flag alone, it says so once all the same.
    let forced = (label.forced && label.role != Some(TrackRole::Forced)).then_some("Forced");
    let origin = match &label.origin {
        TrackOrigin::InFile => None,
        TrackOrigin::ExternalFile => Some("external file".to_owned()),
        TrackOrigin::Searched { site } => Some(format!("Searched ({site})")),
    };
    let parts: Vec<String> = [
        label.language.as_ref().map(language_name),
        role.map(str::to_owned),
        forced.map(str::to_owned),
        label.title.clone(),
        label.channels.map(|c| channels(channel_count(c))),
        origin,
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        none.to_owned()
    } else {
        parts.join(" · ")
    }
}

fn channel_count(channels: Channels) -> u32 {
    match channels {
        Channels::Mono => 1,
        Channels::Stereo => 2,
        Channels::Surround51 => 6,
        Channels::Surround71 => 8,
        Channels::Other(n) => n,
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
/// `None` when it probed nothing. The picture's class goes by its width
/// or its height, whichever says more, so a 2.40:1 4K film (3840 by 1600)
/// is 2160p.
pub fn file(
    container: Option<spoolframe_types::Container>,
    video: Option<&spoolframe_types::VideoTrack>,
) -> Option<String> {
    use spoolframe_types::{Container, HdrFormat, Resolution, VideoCodec};
    let class = describe::picture(container, video)?;
    let container = class.container.and_then(|c| match c {
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
        let codec = class.codec.and_then(|c| match c {
            VideoCodec::H264 => Some("H.264"),
            VideoCodec::Hevc => Some("HEVC"),
            VideoCodec::Av1 => Some("AV1"),
            VideoCodec::Vp9 => Some("VP9"),
            VideoCodec::Vp8 => Some("VP8"),
            VideoCodec::Mpeg2 => Some("MPEG-2"),
            VideoCodec::Mpeg4 => Some("MPEG-4"),
            VideoCodec::Vc1 => Some("VC-1"),
            VideoCodec::Unknown => None,
        });
        // Below 720p the lines themselves say more than "SD".
        let resolution = match class.resolution {
            Some(Resolution::Uhd) => "2160p".to_owned(),
            Some(Resolution::FullHd) => "1080p".to_owned(),
            Some(Resolution::Hd) => "720p".to_owned(),
            _ => format!("{}p", v.height),
        };
        let hdr = class.hdr.map(|h| match h {
            HdrFormat::Hdr10 => "HDR10",
            HdrFormat::Hlg => "HLG",
            HdrFormat::DolbyVision => "Dolby Vision",
            HdrFormat::Unknown => "HDR",
        });
        [codec, Some(resolution.as_str()), hdr]
            .into_iter()
            .flatten()
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
    let format = describe::audio_format(codec, bit_depth, sample_rate)?;
    let name = match format.codec {
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
    Some(match format.bit_depth.zip(format.sample_rate) {
        Some((depth, rate)) => {
            let khz = if rate % 1000 == 0 {
                format!("{}", rate / 1000)
            } else {
                format!("{:.1}", rate as f64 / 1000.0)
            };
            format!("{name} {depth}/{khz}")
        }
        None => name.to_owned(),
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
    use describe::PathChip;
    Some(match describe::path_chip(path)? {
        PathChip::Lan => "Direct · LAN",
        PathChip::Internet => "Direct · internet",
        PathChip::Relayed => "Relayed",
    })
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
        ConnectionPath::Lan { addr } => format!("LAN · {addr}"),
        ConnectionPath::Direct { addr } => format!("direct · {addr}"),
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
        let lan = ConnectionPath::Lan {
            addr: ([10, 10, 0, 6], 7431).into(),
        };
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
            path_chip(ConnectionPath::Direct {
                addr: ([203, 0, 113, 9], 7431).into(),
            }),
            Some("Direct · internet")
        );
    }

    #[test]
    fn a_local_path_says_lan_and_where() {
        use crate::backend::ConnectionPath;
        assert_eq!(
            connection_path(ConnectionPath::Lan {
                addr: ([10, 10, 0, 6], 7431).into(),
            }),
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
            bit_rate: None,
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
    fn a_narrow_film_is_classed_by_its_lines() {
        assert_eq!(
            file(None, Some(&picture(1440, 1080, None))).as_deref(),
            Some("HEVC 1080p")
        );
    }

    #[test]
    fn an_sd_file_says_its_lines() {
        assert_eq!(
            file(None, Some(&picture(720, 576, None))).as_deref(),
            Some("HEVC 576p")
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
    fn the_time_left_is_hours_and_minutes() {
        // Under an hour is minutes only, whole hours drop the minutes, and a
        // partial minute counts as a minute.
        assert_eq!(
            [
                remaining(mins(50), mins(32)),
                remaining(mins(117), mins(37)),
                remaining(mins(150), mins(30)),
                remaining(mins(10), Duration::from_secs(9 * 60 + 50)),
            ],
            ["18 min left", "1 h 20 min left", "2 h left", "1 min left"]
        );
    }

    #[test]
    fn a_length_is_hours_and_minutes_or_minutes_only() {
        assert_eq!(
            [length(mins(117)), length(mins(50))],
            ["1 h 57 min", "50 min"]
        );
    }

    #[test]
    fn a_timestamp_pads_minutes_and_seconds_over_an_hour() {
        assert_eq!(
            [
                timestamp(Duration::from_secs(37 * 60 + 12)),
                timestamp(Duration::from_secs(3723)),
            ],
            ["37:12", "1:02:03"]
        );
    }

    #[test]
    fn a_subtitle_sites_language_codes_are_named() {
        // "ger" is a known code, "tlh" and "xx" are unknown and shown as is.
        assert_eq!(
            ["ger", "tlh", "en", "pt-BR", "pt-br", "sv", "xx"].map(language),
            [
                "German",
                "TLH",
                "English",
                "Portuguese (Brazil)",
                "Portuguese (Brazil)",
                "Swedish",
                "XX"
            ]
        );
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
    fn a_subtitle_forced_by_its_flag_alone_says_so() {
        let track = spoolframe_types::SubtitleTrack {
            language: Some("eng".into()),
            title: None,
            forced: true,
            default: false,
            external: false,
            role: spoolframe_types::SubtitleRole::Full,
            timing: None,
            download: None,
        };
        assert_eq!(subtitle_track(&track), "English · Forced");
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
            timing: None,
            download: None,
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
            timing: None,
            download: None,
        };
        assert_eq!(subtitle_track(&track), "English · external file");
    }

    #[test]
    fn a_downloaded_subtitle_says_which_site_it_was_searched_on() {
        let track = spoolframe_types::SubtitleTrack {
            language: Some("en".into()),
            title: Some("SubDL".into()),
            forced: false,
            default: false,
            external: true,
            role: spoolframe_types::SubtitleRole::Full,
            timing: None,
            download: Some(spoolframe_types::SubtitleDownload {
                id: "d1".into(),
                source: "subdl".into(),
                release: "Alien.1979.1080p".into(),
                by: "Alice".into(),
                at: 0,
                hash_match: false,
                origin: Default::default(),
                can_remove: true,
            }),
        };
        assert_eq!(subtitle_track(&track), "English · Searched (SubDL)");
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
            timing: None,
            download: None,
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
