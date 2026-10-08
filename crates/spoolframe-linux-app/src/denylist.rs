//! Demuxers and decoders the app never needs, taken out of GStreamer's
//! registry before any pipeline is built (C-15).
//!
//! The server sends Matroska or MP4 (fragmented, when it encodes), with
//! H.264, HEVC, AV1, VP9 and the older video codecs the client says it
//! plays, the usual audio codecs and text or picture subtitles. A crafted
//! file from a hostile or swapped server should reach no other parser:
//! playbin plugs in whatever element claims the bytes, and the formats
//! below, from GStreamer's ugly and bad sets and from FFmpeg's long tail,
//! are where the memory-safety bugs have been.
//!
//! An element taken out cannot be made by name or autoplugged, and the
//! client stops saying it plays the format (see [`crate::capabilities`],
//! which reads the registry after this): the server then converts such a
//! file, as it does any other the client cannot play.
//!
//! A denylist, not an allowlist: a new plugin that this list does not name
//! still loads. Encoders and muxers are left alone; playback never plugs
//! them in.

use std::collections::BTreeSet;

use gst::prelude::*;

/// Elements taken out: a name, or a prefix ending in `*`.
const DENIED: &[&str] = &[
    // FFmpeg's demuxers: they repeat GStreamer's own, which the server's
    // formats use, and cover hundreds of formats it never sends.
    "avdemux_*",
    // FFmpeg's decoders, but those in LIBAV_KEPT: the long tail of
    // FFmpeg's codecs, from game formats to screen recorders.
    "avdec_*",
    // ASF / WMV.
    "asfdemux",
    // RealMedia and RealAudio, and their decoders when installed.
    "rmdemux",
    "rademux",
    "realvideodec",
    "realaudiodec",
    // Commodore 64 tunes.
    "siddec",
    // AVI, and the subtitles found in it: the server remuxes AVI.
    "avidemux",
    "avisubtitle",
    // Flash video.
    "flvdemux",
    // MPEG program streams (VOB) and MXF: never sent.
    "mpegpsdemux",
    "mxfdemux",
    // DV.
    "dvdemux",
    "dvdec",
    // Ogg's carriers for AVI-era streams (OGM); Ogg itself is kept.
    "oggaviparse",
    "ogmaudioparse",
    "ogmvideoparse",
    "ogmtextparse",
    // Theora: Ogg's video codec, which the client does not say it plays.
    "theoradec",
    // Uncompressed video in a stream, VMware's screen codec, FLIC.
    "y4mdec",
    "vmncdec",
    "flxdec",
    // IVF, raw VP8/VP9/AV1 frames: the server sends them in Matroska.
    "ivfparse",
    // A second decoder for codecs another kept one plays: HEVC through
    // libde265, H.264 through OpenH264, MPEG-2 through libmpeg2, AC-3
    // through liba52, AAC through FAAD and FDK.
    "libde265dec",
    "openh264dec",
    "mpeg2dec",
    "a52dec",
    "faad",
    "fdkaacdec",
    // Tracker and module music.
    "modplug",
    "openmptdec",
    // MIDI.
    "midiparse",
    "fluiddec",
    "wildmididec",
    // Game console music.
    "gmedec",
    // Speech and telephony codecs.
    "gsmdec",
    "isacdec",
    "sirendec",
    "speexdec",
    "amrnbdec",
    "amrwbdec",
    "amrparse",
    "alawdec",
    "mulawdec",
    "adpcmdec",
    // Bluetooth audio.
    "sbcdec",
    // Audio containers the client does not say it plays: WAV, AIFF, Sun
    // AU, WavPack, DVD LPCM, and libsndfile's many.
    "wavparse",
    "aiffparse",
    "auparse",
    "wavpackparse",
    "wavpackdec",
    "dvdlpcmdec",
    "sfdec",
    // Images: artwork is decoded by GTK, never by a pipeline.
    "jpegdec",
    "jpegparse",
    "pngdec",
    "pngparse",
    "webpdec",
    "pnmdec",
    "rsvgdec",
    "openexrdec",
    "openjpegdec",
    "jpeg2000parse",
    "gdkpixbufdec",
    // Adaptive streaming: the client reads one file over the server's
    // connection. These fetch the URLs a playlist names with their own
    // HTTP client.
    "hlsdemux",
    "hlsdemux2",
    "dashdemux",
    "dashdemux2",
    "mssdemux",
    "mssdemux2",
    // Internet radio metadata, and multipart MIME.
    "icydemux",
    "multipartdemux",
];

/// FFmpeg's decoders kept from "avdec_*": the formats the server sends
/// that no GStreamer decoder installed here plays as well.
const LIBAV_KEPT: &[&str] = &[
    // Video: the software decoders behind VA-API.
    "avdec_h264",
    "avdec_h265",
    "avdec_mpeg2video",
    "avdec_mpeg4",
    "avdec_vc1",
    // Audio.
    "avdec_aac",
    "avdec_ac3",
    "avdec_eac3",
    // DTS.
    "avdec_dca",
    "avdec_truehd",
    "avdec_alac",
];

/// Whether the element factory `name` is taken out.
fn denied(name: &str) -> bool {
    !LIBAV_KEPT.contains(&name)
        && DENIED.iter().any(|entry| match entry.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == *entry,
        })
}

/// Takes the denied elements out of the registry. Call after `gst::init`,
/// before any pipeline is built or the capabilities are read. Returns
/// their names.
pub fn apply() -> Vec<String> {
    let registry = gst::Registry::get();
    let doomed: Vec<gst::PluginFeature> = registry
        .features(gst::ElementFactory::static_type())
        .into_iter()
        .filter(|feature| denied(&feature.name()))
        .collect();
    // Their plugins are loaded first. A plugin loaded later, for a kept
    // element beside them (h264parse beside jpeg2000parse, avdec_h264
    // beside every avdec_), would register all its elements again.
    let plugins: BTreeSet<String> = doomed
        .iter()
        .filter_map(|feature| feature.plugin_name())
        .map(String::from)
        .collect();
    for name in &plugins {
        if let Some(plugin) = registry.find_plugin(name)
            && let Err(error) = plugin.load()
        {
            // It cannot be loaded later either; its elements go anyway.
            tracing::debug!(plugin = %name, %error, "a plugin with denied elements does not load");
        }
    }
    let mut removed: Vec<String> = doomed
        .iter()
        .filter_map(|feature| {
            // Loading replaced the registry's entry with the plugin's own.
            let current = registry.lookup_feature(&feature.name())?;
            registry.remove_feature(&current);
            Some(current.name().to_string())
        })
        .collect();
    removed.sort();
    tracing::debug!(
        count = removed.len(),
        elements = ?removed,
        "GStreamer demuxers and decoders denied"
    );
    removed
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::OnceLock;

    use spoolframe_linux_core::testing::TestDir;
    use spoolframe_types::{AudioCodec, Container, VideoCodec};

    use super::*;

    /// GStreamer, with the denylist applied once for every test here.
    fn removed() -> &'static [String] {
        static REMOVED: OnceLock<Vec<String>> = OnceLock::new();
        REMOVED.get_or_init(|| {
            gst::init().unwrap();
            apply()
        })
    }

    #[test]
    fn a_name_or_a_prefix_is_denied_and_the_kept_are_not() {
        assert!(denied("asfdemux"));
        assert!(denied("avdemux_ape"));
        assert!(denied("avdec_cinepak"));
        assert!(!denied("avdec_h264"));
        assert!(!denied("matroskademux"));
        assert!(!denied("qtdemux"));
        assert!(!denied("avenc_aac"));
    }

    #[test]
    fn a_denied_element_cannot_be_made_after_startup() {
        let removed = removed();
        assert!(removed.iter().any(|name| name == "asfdemux"), "{removed:?}");
        for name in removed {
            assert!(gst::ElementFactory::find(name).is_none(), "{name}");
            assert!(gst::ElementFactory::make(name).build().is_err(), "{name}");
        }
    }

    #[test]
    fn a_denied_element_stays_out_when_its_plugin_is_loaded() {
        removed();
        // h264parse shares its plugin with jpeg2000parse, and avdec_h264
        // with every avdec_ element: loading a plugin registers all of its
        // elements again.
        for kept in ["h264parse", "avdec_h264"] {
            assert!(gst::ElementFactory::make(kept).build().is_ok(), "{kept}");
        }
        for name in ["jpeg2000parse", "avdec_cinepak"] {
            assert!(gst::ElementFactory::find(name).is_none(), "{name}");
        }
    }

    #[test]
    fn the_formats_the_server_sends_still_play() {
        removed();
        let caps = crate::capabilities::capabilities();
        for codec in [
            VideoCodec::H264,
            VideoCodec::Hevc,
            VideoCodec::Av1,
            VideoCodec::Vp9,
        ] {
            assert!(caps.video.contains(&codec), "{codec:?}");
        }
        for codec in [
            AudioCodec::Aac,
            AudioCodec::Flac,
            AudioCodec::Opus,
            AudioCodec::Ac3,
            AudioCodec::Eac3,
            AudioCodec::Dts,
            AudioCodec::Mp3,
            AudioCodec::Vorbis,
        ] {
            assert!(caps.audio.contains(&codec), "{codec:?}");
        }
        for container in [Container::Matroska, Container::Mp4] {
            assert!(caps.containers.contains(&container), "{container:?}");
        }
        assert!(!caps.containers.contains(&Container::Avi));
    }

    /// Encodes `encode` (a pipeline ending in a muxer) into `path`.
    fn encode(encode: &str, path: &Path) {
        let pipeline =
            gst::parse::launch(&format!("{encode} ! filesink location={}", path.display()))
                .unwrap();
        let bus = pipeline.bus().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let message = bus
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(60),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .expect("the clip is encoded");
        pipeline.set_state(gst::State::Null).unwrap();
        assert!(
            matches!(message.view(), gst::MessageView::Eos(_)),
            "{message:?}"
        );
    }

    /// Plays `path` to its end through playbin3, as the player does, and
    /// returns the elements it plugged in.
    fn play(path: &Path) -> Vec<String> {
        let sink = |name| {
            gst::ElementFactory::make("fakesink")
                .property("sync", false)
                .name(name)
                .build()
                .unwrap()
        };
        let playbin = gst::ElementFactory::make("playbin3")
            .property("uri", format!("file://{}", path.display()))
            .property("video-sink", sink("video"))
            .property("audio-sink", sink("audio"))
            .build()
            .unwrap();
        let plugged = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = plugged.clone();
        playbin
            .downcast_ref::<gst::Bin>()
            .unwrap()
            .connect_deep_element_added(move |_, _, element| {
                if let Some(factory) = element.factory() {
                    seen.lock().unwrap().push(factory.name().to_string());
                }
            });
        let bus = playbin.bus().unwrap();
        playbin.set_state(gst::State::Playing).unwrap();
        let message = bus
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(60),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .expect("the clip plays");
        playbin.set_state(gst::State::Null).unwrap();
        assert!(
            matches!(message.view(), gst::MessageView::Eos(_)),
            "{}: {message:?}",
            path.display()
        );
        plugged.lock().unwrap().clone()
    }

    #[test]
    fn flac_hevc_and_aac_clips_play_after_startup() {
        removed();
        let dir = TestDir::new_blocking("spoolframe-denylist");
        let audio = "audiotestsrc num-buffers=20 ! audioconvert";
        let clips = [
            (
                "flac.mkv",
                format!("{audio} ! flacenc ! flacparse ! matroskamux"),
                "flacdec",
            ),
            (
                "aac.mp4",
                format!("{audio} ! avenc_aac ! aacparse ! mp4mux"),
                "avdec_aac",
            ),
            (
                "hevc.mkv",
                "videotestsrc num-buffers=10 ! video/x-raw,width=320,height=240 \
                 ! videoconvert ! x265enc ! h265parse ! matroskamux"
                    .to_owned(),
                "h265",
            ),
        ];
        for (name, pipeline, decoder) in clips {
            let path = dir.join(name);
            encode(&pipeline, &path);
            let plugged = play(&path);
            assert!(
                plugged.iter().any(|element| element.contains(decoder)),
                "{name}: {plugged:?}"
            );
            assert!(
                !plugged.iter().any(|element| denied(element)),
                "{name}: {plugged:?}"
            );
        }
    }
}
