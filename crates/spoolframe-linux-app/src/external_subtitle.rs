//! An external subtitle beside the media: GstPlay's suburi, and the one
//! guard it needs.
//!
//! playbin3 reads a suburi through a urisourcebin of its own, whose only
//! stream is the subtitle. decodebin3 answers a stream it has not
//! selected with not-linked, and the subtitle's parser has no other
//! stream to carry on with: the typefind in front of it stops with
//! "streaming stopped, reason not-linked" and GstPlay fails the whole
//! playback. And the subtitle is not selected when it arrives: on each
//! new stream collection GstPlay keeps the subtitle stream it had, or
//! takes the first text stream, and a file with its own subtitles has
//! one of those first. gst-launch's playbin3 does the same with a
//! suburi beside a file with subtitles in it.
//!
//! So the subtitle's buffers are held at its urisourcebin until it is
//! the stream GstPlay shows: [`ExternalSubtitle::show`] picks it once
//! GstPlay knows of it, then lets them through. Turned off from the
//! menu, it stays selected and the overlay is silenced. Picked away from
//! for another subtitle, its buffers are dropped from then on, not
//! refused; one already waiting in decodebin3's queue can still meet the
//! not-linked, which the player then recovers from (see
//! [`ExternalSubtitle::caused`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use gst::glib;
use gst::prelude::*;
use gst_play::prelude::*;
use spoolframe_types::SubtitleFormat;

/// What the player says when an external subtitle cannot be shown; the
/// playback goes on without it.
pub const NOT_SHOWN: &str = "Couldn't show this subtitle";

/// The external subtitle given to GstPlay, and what holds its buffers.
/// Cheap to clone: every clone is the same subtitle.
#[derive(Clone, Default)]
pub struct ExternalSubtitle(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    /// The subtitle's URI, as given to GstPlay.
    uri: Option<String>,
    /// Its stream, from the stream-start its urisourcebin sends.
    stream: Option<String>,
    /// The urisourcebins read it, by path: an error from inside one is
    /// the subtitle's.
    sources: Vec<String>,
    held: Vec<(gst::Pad, gst::PadProbeId)>,
    flow: Flow,
    /// Turned off from the menu while it showed.
    hidden: bool,
    /// The overlays that draw subtitles, to silence while it is hidden.
    overlays: Vec<glib::WeakRef<gst::Element>>,
}

/// What happens to the subtitle's buffers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Flow {
    /// Held until it is shown: not yet selected.
    #[default]
    Held,
    Passing,
    /// Picked away from: dropped, so its source does not fail.
    Dropping,
}

impl ExternalSubtitle {
    /// Watches `playbin` for the urisourcebins that read the subtitle.
    pub fn watch(&self, playbin: &gst::Element) {
        let Some(bin) = playbin.downcast_ref::<gst::Bin>() else {
            return;
        };
        let weak = Arc::downgrade(&self.0);
        bin.connect_deep_element_added(move |_, _, element| {
            let factory = element.factory().map(|f| f.name());
            if factory.as_deref() == Some("subtitleoverlay") {
                if let Some(inner) = weak.upgrade() {
                    let mut inner = inner.lock().unwrap();
                    element.set_property("silent", inner.hidden);
                    inner.overlays.push(element.downgrade());
                }
                return;
            }
            if factory.as_deref() != Some("urisourcebin") {
                return;
            }
            // Known by its URI, which may come after it is added. One
            // that cannot read the file fails before it has a pad.
            if let Some(inner) = weak.upgrade() {
                ExternalSubtitle(inner).source_bin(element);
            }
            let bin_weak = weak.clone();
            element.connect_notify(Some("uri"), move |element, _| {
                if let Some(inner) = bin_weak.upgrade() {
                    ExternalSubtitle(inner).source_bin(element);
                }
            });
            let weak = weak.clone();
            element.connect_pad_added(move |element, pad| {
                if let Some(inner) = weak.upgrade() {
                    ExternalSubtitle(inner).source_pad(element, pad);
                }
            });
        });
    }

    /// A urisourcebin, kept by its path if it reads the subtitle: an
    /// error from inside it is the subtitle's.
    fn source_bin(&self, element: &gst::Element) {
        let mut inner = self.0.lock().unwrap();
        let uri = element.property::<Option<String>>("uri");
        if inner.uri.is_none() || uri != inner.uri {
            return;
        }
        let path = element.path_string().to_string();
        if !inner.sources.contains(&path) {
            inner.sources.push(path);
        }
    }

    /// The subtitle GstPlay is given now, by URI; `None` for none. Its
    /// buffers are held again until [`Self::show`].
    pub fn attach(&self, uri: Option<String>) {
        let held = {
            let mut inner = self.0.lock().unwrap();
            inner.uri = uri;
            inner.stream = None;
            inner.sources.clear();
            inner.flow = Flow::Held;
            std::mem::take(&mut inner.held)
        };
        release(held);
        self.hide(false);
    }

    /// A urisourcebin's pad: held, if it reads the subtitle. On a
    /// streaming thread.
    fn source_pad(&self, element: &gst::Element, pad: &gst::Pad) {
        {
            let mut inner = self.0.lock().unwrap();
            let uri = element.property::<Option<String>>("uri");
            if inner.uri.is_none() || uri != inner.uri {
                return;
            }
            // A new urisourcebin is a new pipeline (GstPlay restarts for
            // a subtitle): nothing is selected in it yet.
            inner.flow = Flow::Held;
            if let Some(start) = pad.sticky_event::<gst::event::StreamStart>(0) {
                inner.stream = Some(start.stream_id().to_owned());
            }
        }
        let weak = Arc::downgrade(&self.0);
        pad.add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM
                | gst::PadProbeType::BUFFER
                | gst::PadProbeType::BUFFER_LIST,
            move |_, info| on_data(&weak, info),
        );
        let hold = pad.add_probe(
            gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
            |_, _| gst::PadProbeReturn::Ok,
        );
        let Some(hold) = hold else { return };
        let mut inner = self.0.lock().unwrap();
        if inner.flow == Flow::Held {
            inner.held.push((pad.clone(), hold));
        } else {
            // Shown while the hold went on.
            drop(inner);
            pad.remove_probe(hold);
        }
    }

    /// Shows the subtitle once GstPlay knows of it, `info` being what it
    /// knows, and lets its buffers through: once for each pipeline that
    /// reads it, so a pick from the menu since stands. Whether it was
    /// shown now.
    pub fn show(&self, play: &gst_play::Play, info: &gst_play::PlayMediaInfo) -> bool {
        let (id, flow) = {
            let inner = self.0.lock().unwrap();
            (inner.stream.clone(), inner.flow)
        };
        if flow != Flow::Held {
            return false;
        }
        let Some(id) = id else {
            return false;
        };
        if !info
            .subtitle_streams()
            .iter()
            .any(|s| PlayStreamInfoExt::stream_id(s) == id)
        {
            return false;
        }
        let current = || {
            play.current_subtitle_track()
                .map(|t| PlayStreamInfoExt::stream_id(&t).to_string())
        };
        if current().as_deref() != Some(id.as_str()) {
            // Picked while subtitles are off, GstPlay's selection does not
            // change yet and it reports that as a failure: what it shows
            // once on is what counts.
            let _ = play.set_subtitle_track_id(Some(&id));
            play.set_subtitle_track_enabled(true);
            if current().as_deref() != Some(id.as_str()) {
                tracing::warn!(stream = %id, "GstPlay would not pick the external subtitle");
                return false;
            }
        }
        self.passing();
        true
    }

    /// Before GstPlay is told to show `stream` (`None`: none). Turned
    /// off while it shows, it is hidden instead, and true says GstPlay is
    /// to be left as it is. Picked away from, its buffers are dropped
    /// from now on.
    pub fn picking(&self, stream: Option<&str>) -> bool {
        let showing = {
            let mut inner = self.0.lock().unwrap();
            // Passing only while GstPlay shows it.
            let showing = inner.uri.is_some() && inner.flow == Flow::Passing;
            if showing && stream.is_some() && inner.stream.as_deref() != stream {
                inner.flow = Flow::Dropping;
            }
            showing
        };
        let hiding = showing && stream.is_none();
        self.hide(hiding);
        hiding
    }

    fn hide(&self, hidden: bool) {
        let overlays: Vec<_> = {
            let mut inner = self.0.lock().unwrap();
            inner.hidden = hidden;
            inner.overlays.retain(|o| o.upgrade().is_some());
            inner.overlays.iter().filter_map(|o| o.upgrade()).collect()
        };
        for overlay in overlays {
            overlay.set_property("silent", hidden);
        }
    }

    /// After GstPlay was told to show `stream`. Whether it is the
    /// subtitle coming back after its buffers were dropped: then only a
    /// seek reads them again.
    pub fn picked(&self, stream: Option<&str>) -> bool {
        let flow = {
            let inner = self.0.lock().unwrap();
            if stream.is_none() || inner.stream.as_deref() != stream {
                return false;
            }
            inner.flow
        };
        self.passing();
        flow == Flow::Dropping
    }

    /// Whether `message`, a GstPlay error, comes from inside the
    /// subtitle's source rather than the media's.
    pub fn caused(&self, message: &str) -> bool {
        let inner = self.0.lock().unwrap();
        inner.sources.iter().any(|path| from_element(message, path))
    }

    fn passing(&self) {
        let held = {
            let mut inner = self.0.lock().unwrap();
            inner.flow = Flow::Passing;
            std::mem::take(&mut inner.held)
        };
        release(held);
    }
}

fn release(held: Vec<(gst::Pad, gst::PadProbeId)>) {
    for (pad, probe) in held {
        pad.remove_probe(probe);
    }
}

/// The subtitle's stream id as it starts, and its buffers dropped once it
/// is picked away from.
fn on_data(inner: &Weak<Mutex<Inner>>, info: &mut gst::PadProbeInfo) -> gst::PadProbeReturn {
    let Some(inner) = inner.upgrade() else {
        return gst::PadProbeReturn::Remove;
    };
    match &info.data {
        Some(gst::PadProbeData::Event(event)) => {
            if let gst::EventView::StreamStart(start) = event.view() {
                inner.lock().unwrap().stream = Some(start.stream_id().to_owned());
            }
            gst::PadProbeReturn::Ok
        }
        _ if inner.lock().unwrap().flow == Flow::Dropping => gst::PadProbeReturn::Drop,
        _ => gst::PadProbeReturn::Ok,
    }
}

/// Whether `message` is GstPlay's "Error from element <path>: …" for the
/// element at `path` or one inside it.
fn from_element(message: &str, path: &str) -> bool {
    crate::pipeline::error_element_path(message)
        .and_then(|element| element.strip_prefix(path))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Why `bytes` cannot be a subtitle: every external subtitle the server
/// sends is text.
pub fn check(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err("the subtitle is empty");
    }
    if std::str::from_utf8(bytes).is_err() {
        return Err("the subtitle is not UTF-8 text");
    }
    Ok(())
}

/// The file's extension: GStreamer's typefind takes it as a hint.
fn extension(format: SubtitleFormat) -> &'static str {
    match format {
        SubtitleFormat::Ass => "ass",
        SubtitleFormat::WebVtt => "vtt",
        _ => "srt",
    }
}

/// Writes the subtitle to a file of its own in `dir`, whole before it
/// has its name: GStreamer never opens one half written, nor one being
/// rewritten for the next subtitle.
pub fn write(dir: &Path, format: SubtitleFormat, bytes: &[u8]) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "spoolframe-subtitle-{}-{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        extension(format)
    );
    let path = dir.join(&name);
    let part = dir.join(format!(".{name}.part"));
    std::fs::write(&part, bytes)?;
    if let Err(e) = std::fs::rename(&part, &path) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str =
        "/GstPlayBin3:playbin3/GstURIDecodeBin3:uridecodebin3/GstURISourceBin:urisourcebin2";

    #[test]
    fn an_error_inside_the_subtitles_source_is_the_subtitles() {
        let message = format!(
            "Error from element {SOURCE}/GstParseBin:parsebin2/GstTypeFindElement:typefind: \
             GStreamer encountered a general stream error."
        );
        assert!(from_element(&message, SOURCE));
    }

    #[test]
    fn an_error_from_the_subtitles_source_itself_is_the_subtitles() {
        let message = format!("Error from element {SOURCE}: Resource not found.");
        assert!(from_element(&message, SOURCE));
    }

    #[test]
    fn an_error_from_a_source_with_a_longer_name_is_not_the_subtitles() {
        let message = format!("Error from element {SOURCE}0/GstParseBin:parsebin0: Failed.");
        assert!(!from_element(&message, SOURCE));
    }

    #[test]
    fn an_error_from_the_media_is_not_the_subtitles() {
        let message = "Error from element /GstPlayBin3:playbin3/GstPlaySink:playsink: \
                       The stream is in the wrong format.";
        assert!(!from_element(message, SOURCE));
    }

    #[test]
    fn a_srt_with_an_ass_tag_is_a_subtitle() {
        let srt = "1\n00:00:01,417 --> 00:00:04,917\n{\\an8}[Vision] <i>It's been…</i>\n";
        assert_eq!(check(srt.as_bytes()), Ok(()));
    }

    #[test]
    fn nothing_is_not_a_subtitle() {
        assert!(check(b" \n\r\n").is_err());
    }

    #[test]
    fn bytes_that_are_not_utf8_are_not_a_subtitle() {
        assert!(check(b"1\n00:00:01,000 --> 00:00:02,000\n\xff\xfe\n").is_err());
    }

    #[test]
    fn each_subtitle_gets_a_file_of_its_own() {
        let dir =
            spoolframe_linux_core::testing::TestDir::new_blocking("spoolframe-subtitle-files");
        let first = write(&dir, SubtitleFormat::Srt, b"one").unwrap();
        let second = write(&dir, SubtitleFormat::Srt, b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
    }

    #[test]
    fn a_written_subtitle_leaves_no_part_file_behind() {
        let dir = spoolframe_linux_core::testing::TestDir::new_blocking("spoolframe-subtitle-part");
        let path = write(&dir, SubtitleFormat::Ass, b"[Script Info]").unwrap();
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(names, vec![path]);
    }
}
