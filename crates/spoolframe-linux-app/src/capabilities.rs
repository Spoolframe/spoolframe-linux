//! What this machine can play, from the GStreamer registry (P1-07, P1-16).
//!
//! The server plans playback from these, so they must list what is
//! actually installed, not what GStreamer could do with every plugin: a
//! codec is in only if some decoder (or demuxer, for containers) takes its
//! caps. Mapped to the shared codec vocabulary, which the server maps
//! ffprobe's names to.

use spoolframe_types::{
    AudioCodec, ClientCapabilities, Container, Delivery, SubtitleFormat, VideoCodec,
};

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
    // Text subtitles are decoded or parsed (subparse). Bitmap ones (PGS,
    // VobSub, DVB) are drawn by overlays, dvdspu and dvbsuboverlay, which
    // playbin's subtitleoverlay plugs in: without them the client never
    // said it plays PGS, and the server refused files that had it.
    let mut subtitle_handlers = elements(
        gst::ElementFactoryType::DECODER
            | gst::ElementFactoryType::PARSER
            | gst::ElementFactoryType::DEMUXER,
    );
    subtitle_handlers.extend(overlays());
    let mut caps = ClientCapabilities {
        video: supported(VIDEO, &decoders),
        // What the GPU decodes (PGPU-06), so the server encodes to a codec
        // that plays without the CPU: AV1 decoded in software is slow.
        hardware_video: Some(supported(VIDEO, &hardware(&decoders))),
        audio: supported(AUDIO, &decoders),
        containers: supported(CONTAINERS, &demuxers),
        subtitles: supported(SUBTITLES, &subtitle_handlers),
        // The file itself first; a progressive encode when it will not
        // play (P2-06). Not HLS: GStreamer's HLS demuxer fetches with its
        // own HTTP client, which cannot reach the server over Iroh.
        delivery: vec![Delivery::Range, Delivery::Progressive],
        // No limits on resolution or channels; the bitrate limit is the
        // user's setting, added by the caller.
        ..ClientCapabilities::default()
    };
    // Raw PCM needs no decoder; every pipeline plays it.
    caps.audio.insert(AudioCodec::Pcm);
    caps
}

/// `caps` as the user set HDR: shown as HDR10 and HLG (the frames carry
/// their colour state to GTK, and the compositor maps them per monitor),
/// or none of it, so the server tone-maps every HDR film to SDR. Never
/// Dolby Vision: since PGPU-06 claiming it (or no limit) has a profile 5
/// film copied as it is, in Dolby's own colours until its RPU is applied,
/// which GTK does not do; the server converts it to HDR10 instead.
pub fn with_hdr(mut caps: ClientCapabilities, show: bool) -> ClientCapabilities {
    use spoolframe_types::HdrFormat;
    let limits = caps.max_video.get_or_insert_with(Default::default);
    limits.hdr = Some(if show {
        [HdrFormat::Hdr10, HdrFormat::Hlg].into()
    } else {
        std::collections::BTreeSet::new()
    });
    caps
}

/// The decoders that say they are hardware ("Hardware" in their class):
/// VA-API's and NVDEC's. Their plugins register them only for a device
/// that works, so a listed one decodes here.
fn hardware(decoders: &[gst::ElementFactory]) -> Vec<gst::ElementFactory> {
    decoders
        .iter()
        .filter(|f| {
            f.metadata(gst::ELEMENT_METADATA_KLASS)
                .is_some_and(|klass| klass.split('/').any(|part| part == "Hardware"))
        })
        .cloned()
        .collect()
}

fn elements(kind: gst::ElementFactoryType) -> Vec<gst::ElementFactory> {
    gst::ElementFactory::factories_with_type(kind, gst::Rank::MARGINAL)
        .into_iter()
        .collect()
}

/// Elements that draw onto video (their class has "Overlay"), at a rank
/// playbin plugs in. From the whole registry: dvdspu's class starts with
/// "Mixer", which no element-type filter covers.
fn overlays() -> Vec<gst::ElementFactory> {
    use gst::prelude::*;
    gst::Registry::get()
        .features(gst::ElementFactory::static_type())
        .into_iter()
        .filter_map(|feature| feature.downcast::<gst::ElementFactory>().ok())
        .filter(|f| f.rank() >= gst::Rank::MARGINAL)
        .filter(|f| {
            f.metadata(gst::ELEMENT_METADATA_KLASS)
                .is_some_and(|klass| klass.split('/').any(|part| part == "Overlay"))
        })
        .collect()
}

fn supported<T: Ord + Copy>(
    table: &[(&str, T)],
    factories: &[gst::ElementFactory],
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
    use super::with_hdr;

    #[test]
    fn hdr_off_asks_the_server_to_tone_map_everything() {
        let caps = with_hdr(Default::default(), false);
        assert_eq!(caps.max_video.and_then(|v| v.hdr), Some(Default::default()));
    }

    /// Shown as HDR: HDR10 and HLG, never Dolby Vision, whose profile 5
    /// is in Dolby's own colours until its RPU is applied, which GTK does
    /// not do: green and purple. The server converts that to HDR10.
    #[test]
    fn hdr_on_takes_hdr10_and_hlg_but_not_dolby_vision() {
        let caps = with_hdr(Default::default(), true);
        assert_eq!(
            caps.max_video.and_then(|v| v.hdr),
            Some(
                [
                    spoolframe_types::HdrFormat::Hdr10,
                    spoolframe_types::HdrFormat::Hlg
                ]
                .into()
            )
        );
    }

    use super::*;
    use spoolframe_linux_core::testing::TestDir;

    fn caps() -> ClientCapabilities {
        gst::init().unwrap();
        capabilities()
    }

    /// libav decodes H.264 on the CPU: it counts for what plays, not for
    /// what the GPU decodes (PGPU-06), so the server does not pick a codec
    /// for a GPU this computer does not have.
    #[test]
    fn a_software_decoder_is_not_hardware() {
        gst::init().unwrap();
        let avdec = gst::ElementFactory::find("avdec_h264").expect("the dev shell has gst-libav");
        let decoders = [avdec];
        assert_eq!(
            (
                supported(VIDEO, &decoders).contains(&VideoCodec::H264),
                supported(VIDEO, &hardware(&decoders)).contains(&VideoCodec::H264)
            ),
            (true, false)
        );
    }

    /// GStreamer's VA-API decoders say they are hardware; checked where
    /// this computer has one.
    #[test]
    fn a_va_decoder_is_hardware() {
        gst::init().unwrap();
        let Some(va) = gst::ElementFactory::find("vah264dec") else {
            eprintln!("no VA-API H.264 decoder here; nothing to check");
            return;
        };
        assert!(supported(VIDEO, &hardware(&[va])).contains(&VideoCodec::H264));
    }

    #[test]
    fn hardware_is_said_and_within_what_plays() {
        let caps = caps();
        let hardware = caps.hardware_video.clone().expect("the registry tells");
        assert!(hardware.is_subset(&caps.video), "{caps:?}");
    }

    #[test]
    fn direct_play_comes_first_then_a_progressive_encode() {
        assert_eq!(caps().delivery, [Delivery::Range, Delivery::Progressive]);
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

    /// Bitmap subtitles are drawn by overlays (dvdspu, dvbsuboverlay),
    /// which playbin's subtitleoverlay plugs in; -bad has them.
    #[test]
    fn the_dev_shell_draws_bitmap_subtitles() {
        let caps = caps();
        for format in [
            SubtitleFormat::Pgs,
            SubtitleFormat::VobSub,
            SubtitleFormat::DvbSub,
        ] {
            assert!(caps.subtitles.contains(&format), "{format:?} in {caps:?}");
        }
    }

    #[test]
    fn unknown_is_never_claimed() {
        let caps = caps();
        assert!(!caps.video.contains(&VideoCodec::Unknown));
        assert!(!caps.audio.contains(&AudioCodec::Unknown));
    }

    /// PGS as a `.sup` stream: one white `w`×`h` box at (`x`, `y`) on a
    /// 640×360 frame, shown from `from` to `to` (90 kHz ticks). The
    /// smallest display sets the format allows: PCS, WDS, PDS, ODS, END.
    fn pgs_box(x: u16, y: u16, w: u16, h: u16, from: u32, to: u32) -> Vec<u8> {
        fn segment(out: &mut Vec<u8>, pts: u32, kind: u8, data: &[u8]) {
            out.extend_from_slice(b"PG");
            out.extend_from_slice(&pts.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.push(kind);
            out.extend_from_slice(&(data.len() as u16).to_be_bytes());
            out.extend_from_slice(data);
        }
        let be = |v: u16| v.to_be_bytes();
        let window = [&[1u8, 0][..], &be(x), &be(y), &be(w), &be(h)].concat();
        let mut out = Vec::new();
        // Shown: an epoch start with one object.
        let pcs = [
            &be(640)[..],
            &be(360),
            &[0x10],
            &be(0),
            &[0x80, 0, 0, 1],
            &be(0),
            &[0, 0],
            &be(x),
            &be(y),
        ]
        .concat();
        segment(&mut out, from, 0x16, &pcs);
        segment(&mut out, from, 0x17, &window);
        // Palette entry 1: white (Y, Cr, Cb, alpha).
        segment(&mut out, from, 0x14, &[0, 0, 1, 235, 128, 128, 255]);
        let mut rle = Vec::new();
        for _ in 0..h {
            // A run of colour 1, then the end of the line.
            rle.extend_from_slice(&[0x00, 0xC0 | (w >> 8) as u8, (w & 0xff) as u8, 0x01, 0, 0]);
        }
        let length = (rle.len() + 4) as u32;
        let ods = [
            &be(0)[..],
            &[0, 0xC0],
            &length.to_be_bytes()[1..],
            &be(w),
            &be(h),
            &rle,
        ]
        .concat();
        segment(&mut out, from, 0x15, &ods);
        segment(&mut out, from, 0x80, &[]);
        // Cleared: no objects.
        let clear = [&be(640)[..], &be(360), &[0x10], &be(1), &[0, 0, 0, 0]].concat();
        segment(&mut out, to, 0x16, &clear);
        segment(&mut out, to, 0x17, &window);
        segment(&mut out, to, 0x80, &[]);
        out
    }

    /// Plays `path` through playbin3 to the end, with its default streams
    /// (subtitles included), and says for each point whether any frame was
    /// white (`true`) or ever anything but dark (`false` = always dark) there.
    fn frames_at(path: &std::path::Path, points: &[(usize, usize)]) -> Vec<(bool, bool)> {
        use gst::prelude::*;
        use std::sync::{Arc, Mutex};
        let sink = gst::parse::bin_from_description(
            "videoconvert ! video/x-raw,format=RGBA,width=640 ! \
             fakesink name=frames sync=false signal-handoffs=true",
            true,
        )
        .unwrap();
        let seen = Arc::new(Mutex::new(vec![(false, true); points.len()]));
        {
            let (seen, points) = (seen.clone(), points.to_vec());
            sink.by_name("frames")
                .unwrap()
                .connect("handoff", false, move |values| {
                    let buffer = values[1].get::<gst::Buffer>().unwrap();
                    let map = buffer.map_readable().unwrap();
                    let mut seen = seen.lock().unwrap();
                    for (i, (x, y)) in points.iter().enumerate() {
                        let p = (y * 640 + x) * 4;
                        let rgb = &map[p..p + 3];
                        seen[i].0 |= rgb.iter().all(|c| *c > 200);
                        seen[i].1 &= rgb.iter().all(|c| *c < 40);
                    }
                    None
                });
        }
        let playbin = gst::ElementFactory::make("playbin3")
            .property("uri", gst::glib::filename_to_uri(path, None).unwrap())
            .property("video-sink", &sink)
            .property(
                "audio-sink",
                gst::ElementFactory::make("fakesink").build().unwrap(),
            )
            .build()
            .unwrap();
        let bus = playbin.bus().unwrap();
        playbin.set_state(gst::State::Playing).unwrap();
        bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("the clip plays to its end");
        playbin.set_state(gst::State::Null).unwrap();
        seen.lock().unwrap().clone()
    }

    /// A PGS subtitle in Matroska is drawn into the picture by the pipeline
    /// the player uses, now that the client says it plays PGS.
    #[test]
    #[ignore = "needs ffmpeg to mux the PGS stream into Matroska"]
    fn a_pgs_subtitle_is_drawn() {
        use gst::prelude::*;
        gst::init().unwrap();
        let dir = TestDir::new_blocking("spoolframe-pgs");
        let (video, sup, muxed) = (dir.join("v.mkv"), dir.join("s.sup"), dir.join("m.mkv"));
        // Three seconds of black.
        let pipeline = gst::parse::launch(&format!(
            "videotestsrc pattern=black num-buffers=90 ! video/x-raw,width=640,height=360,framerate=30/1 \
             ! videoconvert ! x264enc ! h264parse ! matroskamux ! filesink location={}",
            video.display()
        ))
        .unwrap();
        let bus = pipeline.bus().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        bus.timed_pop_filtered(gst::ClockTime::from_seconds(30), &[gst::MessageType::Eos])
            .unwrap();
        pipeline.set_state(gst::State::Null).unwrap();
        // A white box from 1 s to 2.5 s.
        std::fs::write(&sup, pgs_box(200, 260, 240, 40, 90_000, 225_000)).unwrap();
        let status = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-i"])
            .arg(&video)
            .arg("-i")
            .arg(&sup)
            .args(["-map", "0", "-map", "1", "-c", "copy"])
            .arg(&muxed)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success());

        let seen = frames_at(&muxed, &[(320, 280), (320, 100)]);
        assert!(seen[0].0, "the box was never drawn");
        assert!(seen[1].1, "something was drawn outside the box");
    }
}
