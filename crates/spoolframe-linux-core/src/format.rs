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

fn channels(n: u32) -> String {
    match n {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        6 => "5.1".to_owned(),
        8 => "7.1".to_owned(),
        n => format!("{n} channels"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
