//! What this machine can play, from the GStreamer registry (P1-07, P1-16).
//!
//! The server plans playback from these, so they must list what is
//! actually installed, not what GStreamer could do with every plugin: a
//! codec is in only if some decoder (or demuxer, for containers) takes its
//! caps. Mapped to the shared codec vocabulary, which the server maps
//! ffprobe's names to.

use gst::glib;
use spoolframe_types::{AudioCodec, ClientCapabilities, Container, SubtitleFormat, VideoCodec};

const VIDEO: &[(&str, VideoCodec)] = &[
    ("video/x-h264", VideoCodec::H264),
    ("video/x-h265", VideoCodec::Hevc),
    ("video/x-av1", VideoCodec::Av1),
    ("video/x-vp9", VideoCodec::Vp9),
    ("video/x-vp8", VideoCodec::Vp8),
    (
        "video/mpeg, mpegversion=(int)2, systemstream=(boolean)false",
        VideoCodec::Mpeg2,
    ),
    (
        "video/mpeg, mpegversion=(int)4, systemstream=(boolean)false",
        VideoCodec::Mpeg4,
    ),
    (
        "video/x-wmv, wmvversion=(int)3, format=(string)WVC1",
        VideoCodec::Vc1,
    ),
];

const AUDIO: &[(&str, AudioCodec)] = &[
    ("audio/mpeg, mpegversion=(int)4", AudioCodec::Aac),
    (
        "audio/mpeg, mpegversion=(int)1, layer=(int)3",
        AudioCodec::Mp3,
    ),
    ("audio/x-ac3", AudioCodec::Ac3),
    ("audio/x-eac3", AudioCodec::Eac3),
    ("audio/x-dts", AudioCodec::Dts),
    ("audio/x-true-hd", AudioCodec::TrueHd),
    ("audio/x-flac", AudioCodec::Flac),
    ("audio/x-alac", AudioCodec::Alac),
    ("audio/x-opus", AudioCodec::Opus),
    ("audio/x-vorbis", AudioCodec::Vorbis),
];

const CONTAINERS: &[(&str, Container)] = &[
    ("video/x-matroska", Container::Matroska),
    ("video/quicktime", Container::Mp4),
    (
        "video/mpegts, systemstream=(boolean)true",
        Container::MpegTs,
    ),
    ("video/x-msvideo", Container::Avi),
    ("audio/x-flac", Container::Flac),
    (
        "audio/mpeg, mpegversion=(int)1, layer=(int)3",
        Container::Mp3,
    ),
    ("application/ogg", Container::Ogg),
];

const SUBTITLES: &[(&str, SubtitleFormat)] = &[
    ("application/x-subtitle", SubtitleFormat::Srt),
    ("application/x-ssa", SubtitleFormat::Ass),
    ("application/x-ass", SubtitleFormat::Ass),
    ("application/x-subtitle-vtt", SubtitleFormat::WebVtt),
    ("text/x-raw, format=(string)utf8", SubtitleFormat::MovText),
    ("subpicture/x-pgs", SubtitleFormat::Pgs),
    ("subpicture/x-dvd", SubtitleFormat::VobSub),
    ("subpicture/x-dvb", SubtitleFormat::DvbSub),
];

/// Builds the set from the registry. Call after gst::init.
pub fn capabilities() -> ClientCapabilities {
    // Elements below "marginal" rank are never autoplugged, so they do not
    // count, however installed.
    let decoders = elements(gst::ElementFactoryType::DECODER);
    let demuxers = elements(gst::ElementFactoryType::DEMUXER | gst::ElementFactoryType::PARSER);
    // Subtitles are decoded, parsed (subparse) or overlaid as raw text.
    let subtitle_handlers = elements(
        gst::ElementFactoryType::DECODER
            | gst::ElementFactoryType::PARSER
            | gst::ElementFactoryType::DEMUXER,
    );
    let mut caps = ClientCapabilities {
        video: supported(VIDEO, &decoders),
        audio: supported(AUDIO, &decoders),
        containers: supported(CONTAINERS, &demuxers),
        subtitles: supported(SUBTITLES, &subtitle_handlers),
        // Range reads only, the default, until the player can follow a
        // growing stream; no limits on resolution, channels or bitrate.
        ..ClientCapabilities::default()
    };
    // Raw PCM needs no decoder; every pipeline plays it.
    caps.audio.insert(AudioCodec::Pcm);
    caps
}

fn elements(kind: gst::ElementFactoryType) -> glib::List<gst::ElementFactory> {
    gst::ElementFactory::factories_with_type(kind, gst::Rank::MARGINAL)
}

fn supported<T: Ord + Copy>(
    table: &[(&str, T)],
    factories: &glib::List<gst::ElementFactory>,
) -> std::collections::BTreeSet<T> {
    table
        .iter()
        .filter(|(caps, _)| {
            let Ok(caps) = caps.parse::<gst::Caps>() else {
                tracing::warn!(caps, "unparsable caps in the capability table");
                return false;
            };
            factories.iter().any(|f| f.can_sink_any_caps(&caps))
        })
        .map(|(_, codec)| *codec)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> ClientCapabilities {
        gst::init().unwrap();
        capabilities()
    }

    #[test]
    fn every_table_entry_is_valid_caps() {
        gst::init().unwrap();
        for caps in VIDEO
            .iter()
            .map(|(c, _)| c)
            .chain(AUDIO.iter().map(|(c, _)| c))
            .chain(CONTAINERS.iter().map(|(c, _)| c))
            .chain(SUBTITLES.iter().map(|(c, _)| c))
        {
            assert!(caps.parse::<gst::Caps>().is_ok(), "{caps}");
        }
    }

    /// The dev shell has gst-libav and -good; a machine without them would
    /// rightly report less.
    #[test]
    fn the_dev_shell_plays_h264_aac_in_matroska() {
        let caps = caps();
        assert!(caps.video.contains(&VideoCodec::H264), "{caps:?}");
        assert!(caps.audio.contains(&AudioCodec::Aac), "{caps:?}");
        assert!(caps.containers.contains(&Container::Matroska), "{caps:?}");
    }

    #[test]
    fn unknown_is_never_claimed() {
        let caps = caps();
        assert!(!caps.video.contains(&VideoCodec::Unknown));
        assert!(!caps.audio.contains(&AudioCodec::Unknown));
    }
}
