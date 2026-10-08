//! The only owner of the GStreamer objects a playback plays through:
//! GstPlay, its signal adapter, the video sink and the reader GstPlay
//! reads by its spoolframe:// URI. Nothing else holds them, so the one
//! rule they need holds here by construction:
//!
//! **The GTK main thread never blocks on a pipeline state change, nor on
//! GstPlay's dispose.** gtk4paintablesink does part of a state change on
//! the main thread and waits for it; a NULL that blocked the main thread
//! deadlocked against a streaming thread changing the sink's state
//! (stopping just after a converted restart), and GstPlay's dispose joins
//! its thread, which can wait on the same change. [`Pipeline::dismantle`]
//! takes the pipeline to NULL on a blocking worker, and [`Drop`] hands the
//! last reference to GstPlay to one too.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gst::prelude::*;
use gtk::glib;
use spoolframe_linux_core::reader::{self, ByteSource};

use crate::source::Published;
use crate::tracks;

pub struct Pipeline {
    /// `None` only once dropped: [`Drop`] moves it to a worker.
    play: Option<gst_play::Play>,
    state: tokio::sync::watch::Receiver<gst_play::PlayState>,
    /// Dropped before GstPlay on the main thread: its handlers are bound
    /// to it, and the messages it holds hold GstPlay.
    adapter: Option<gst_play::PlaySignalAdapter>,
    video: VideoSink,
    paintable: gtk::gdk::Paintable,
    /// The reader GstPlay reads, reachable by its URI while published.
    published: RefCell<Option<Published>>,
    /// The external subtitle GstPlay was given last.
    subtitle: RefCell<Option<PathBuf>>,
    loading_subtitle: std::sync::Arc<std::sync::Mutex<Option<Option<String>>>>,
}

impl Pipeline {
    pub fn new() -> anyhow::Result<Self> {
        let video = VideoSink::new()?;
        let paintable = video.gtk.property::<gtk::gdk::Paintable>("paintable");
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&video.outer);
        let play = gst_play::Play::new(Some(renderer));
        style_subtitles(&play.pipeline());
        // GstPlay's subtitle setter restores the previous position, including
        // for a non-seekable replacement stream. Set playbin's startup subtitle
        // when its source is created, after GstPlay has reset the old URI.
        let loading_subtitle = std::sync::Arc::new(std::sync::Mutex::new(None::<Option<String>>));
        let pending = loading_subtitle.clone();
        play.pipeline()
            .connect("source-setup", false, move |values| {
                if let Some(subtitle) = pending.lock().unwrap().take() {
                    let playbin = values[0].get::<gst::Element>().unwrap();
                    playbin.set_property("suburi", subtitle.as_deref());
                }
                None
            });
        let mut config = play.config();
        config.set_position_update_interval(250);
        play.set_config(config)?;
        let adapter = gst_play::PlaySignalAdapter::new(&play);
        let (state_send, state) = tokio::sync::watch::channel(gst_play::PlayState::Stopped);
        adapter.connect_state_changed(move |_, state| {
            let _ = state_send.send(state);
        });
        Ok(Self {
            play: Some(play),
            state,
            adapter: Some(adapter),
            video,
            paintable,
            published: RefCell::new(None),
            subtitle: RefCell::new(None),
            loading_subtitle,
        })
    }

    fn play(&self) -> &gst_play::Play {
        self.play
            .as_ref()
            .expect("GstPlay is held until the pipeline drops")
    }

    /// GstPlay's signals, delivered on the main loop it was made on.
    pub fn adapter(&self) -> &gst_play::PlaySignalAdapter {
        self.adapter
            .as_ref()
            .expect("the adapter is held until the pipeline drops")
    }

    /// The video frames, for any number of pictures.
    pub fn paintable(&self) -> &gtk::gdk::Paintable {
        &self.paintable
    }

    /// For the diagnostics: read only.
    pub fn video(&self) -> &VideoSink {
        &self.video
    }

    /// For the diagnostics: read only.
    pub fn element(&self) -> gst::Element {
        self.play().pipeline()
    }

    /// Stops what plays and opens `reader` in its place, with `subtitle`
    /// beside it. The subtitle is handed over while stopped: any subtitle
    /// change on a running pipeline makes GstPlay restart and seek back,
    /// which a converted stream cannot do, so even clearing an unset one
    /// mid-play stalled it. One already given is given again, as a new
    /// URI may not keep it.
    pub fn load(&self, reader: Box<dyn ByteSource>, subtitle: Option<&Path>) {
        self.quiesce();
        let play = self.play();
        let published = Published::boxed(reader);
        let uri = subtitle
            .map(|path| gtk::prelude::FileExt::uri(&gtk::gio::File::for_path(path)).to_string());
        *self.loading_subtitle.lock().unwrap() = Some(uri);
        self.subtitle.replace(subtitle.map(Path::to_path_buf));
        if subtitle.is_some() {
            play.set_subtitle_track_enabled(true);
        }
        play.set_uri(Some(&published.uri()));
        self.published.replace(Some(published));
    }

    /// Hands GstPlay an external subtitle mid-play, on, or takes it away.
    /// Only for a stream that can seek: GstPlay restarts and seeks back.
    pub fn set_subtitle(&self, path: Option<&Path>) {
        if self.subtitle.borrow().as_deref() != path {
            self.give_subtitle(path);
        }
    }

    fn give_subtitle(&self, path: Option<&Path>) {
        let play = self.play();
        let uri = path.map(|p| gtk::prelude::FileExt::uri(&gtk::gio::File::for_path(p)));
        play.set_subtitle_uri(uri.as_deref());
        if path.is_some() {
            play.set_subtitle_track_enabled(true);
        }
        self.subtitle.replace(path.map(Path::to_path_buf));
    }

    /// Stops, keeping the pipeline for the next item.
    pub fn stop(&self) {
        self.quiesce();
    }

    pub fn quiesce(&self) {
        if let Some(published) = self.published.borrow().as_ref() {
            published.retire();
        }
        self.play().stop();
    }

    // Retire reads immediately, then wait without blocking GTK for GstPlay
    // to finish stopping before the server changes the stream's epoch.
    pub fn stop_and_wait(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        let mut state = self.state.clone();
        self.quiesce();
        async move {
            let _ = state
                .wait_for(|state| *state == gst_play::PlayState::Stopped)
                .await;
        }
    }

    pub fn message_bus(&self) -> gst::Bus {
        self.play().message_bus()
    }

    #[cfg(test)]
    pub fn playback_uri(&self) -> Option<String> {
        self.published.borrow().as_ref().map(Published::uri)
    }

    pub fn accepts_error(&self, uri: &str) -> bool {
        self.published
            .borrow()
            .as_ref()
            .is_some_and(|p| !p.retired() && p.uri() == uri)
    }

    pub fn play_now(&self) {
        self.play().play();
    }

    pub fn pause(&self) {
        self.play().pause();
    }

    pub fn seek(&self, to: Duration) {
        self.play()
            .seek(gst::ClockTime::from_nseconds(to.as_nanos() as u64));
    }

    pub fn position(&self) -> Option<Duration> {
        self.play()
            .position()
            .map(|p| Duration::from_nanos(p.nseconds()))
    }

    pub fn duration(&self) -> Option<Duration> {
        self.play()
            .duration()
            .map(|d| Duration::from_nanos(d.nseconds()))
    }

    pub fn volume(&self) -> f64 {
        self.play().volume()
    }

    pub fn set_volume(&self, volume: f64) {
        self.play().set_volume(volume);
    }

    pub fn is_muted(&self) -> bool {
        self.play().is_muted()
    }

    pub fn set_muted(&self, muted: bool) {
        self.play().set_mute(muted);
    }

    pub fn select_audio(
        &self,
        info: &gst_play::PlayMediaInfo,
        position: usize,
    ) -> Result<(), glib::BoolError> {
        tracks::select_audio(self.play(), info, position)
    }

    pub fn select_subtitle(
        &self,
        info: &gst_play::PlayMediaInfo,
        position: Option<usize>,
    ) -> Result<(), glib::BoolError> {
        tracks::select_subtitle(self.play(), info, position)
    }

    /// The audio and subtitle menu for `info`; see [`tracks::menu`].
    pub fn track_menu(
        &self,
        info: &gst_play::PlayMediaInfo,
        subtitles_on: &std::rc::Rc<std::cell::Cell<bool>>,
        failed: std::rc::Rc<dyn Fn(&str)>,
    ) -> gtk::Popover {
        tracks::menu(self.play(), info, subtitles_on, failed)
    }

    pub fn reader_status(&self) -> Option<reader::Status> {
        self.published
            .borrow()
            .as_ref()
            .map(|p| p.reader().status())
    }

    /// Down to NULL, not just stopped, so the sink lets go of its GL
    /// context and the pipeline of its threads now rather than whenever
    /// the last reference goes; on a blocking worker (see the module
    /// docs). The handle is for quitting, which waits for it.
    pub fn dismantle(&self) -> tokio::task::JoinHandle<()> {
        let play = self.play();
        play.stop();
        let (play, sink) = (play.clone(), self.video.gtk.clone());
        relm4::spawn_blocking(move || {
            let _ = play.pipeline().set_state(gst::State::Null);
            let _ = sink.set_state(gst::State::Null);
        })
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // The adapter first, here: its handlers are bound to this thread.
        // Its queued messages hold GstPlay, which then is not the last
        // reference yet.
        self.adapter.take();
        if let Some(play) = self.play.take() {
            // Whoever runs GstPlay's dispose joins its thread: never the
            // main thread (see the module docs).
            relm4::spawn_blocking(move || {
                let _ = play.pipeline().set_state(gst::State::Null);
                drop(play);
            });
        }
    }
}

/// The face text subtitles are drawn in: the app's own, not whatever
/// fontconfig falls back to (a serif, here). The overlay scales the size
/// with the picture.
const SUBTITLE_FONT: &str = "Figtree SemiBold 18";

/// Text subtitles in the app's face, and SRT's tags in the file's own
/// text streams drawn as styling, not as letters (see [`srt_markup`]).
///
/// [`srt_markup`]: crate::srt_markup
fn style_subtitles(pipeline: &gst::Element) {
    pipeline.set_property("subtitle-font-desc", SUBTITLE_FONT);
    let Some(bin) = pipeline.downcast_ref::<gst::Bin>() else {
        return;
    };
    // playsink makes its subtitleoverlay when a text stream is linked.
    bin.connect_deep_element_added(|_, _, element| {
        let overlay = element
            .factory()
            .is_some_and(|f| f.name() == "subtitleoverlay");
        if overlay && let Some(pad) = element.static_pad("subtitle_sink") {
            mark_up_srt(&pad);
        }
    });
}

/// Text into the overlay with SRT's tags in it, as styling: plain `utf8`
/// becomes Pango markup (its caps say so, and each cue is converted), and
/// markup with escaped SRT tags in it is converted back (see
/// [`crate::srt_markup`]). Other markup, an external SRT's through
/// subparse among it, passes as it is.
fn mark_up_srt(pad: &gst::Pad) {
    use std::sync::atomic::{AtomicU8, Ordering};
    const OTHER: u8 = 0;
    const PLAIN: u8 = 1;
    const MARKUP: u8 = 2;
    let text = std::sync::Arc::new(AtomicU8::new(OTHER));
    pad.add_probe(
        gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
        move |_, info| {
            match &mut info.data {
                Some(gst::PadProbeData::Event(event)) => {
                    let gst::EventView::Caps(caps) = event.view() else {
                        return gst::PadProbeReturn::Ok;
                    };
                    let format = caps
                        .caps()
                        .structure(0)
                        .filter(|s| s.name() == "text/x-raw")
                        .and_then(|s| s.get::<String>("format").ok());
                    let kind = match format.as_deref() {
                        Some("utf8") => PLAIN,
                        Some("pango-markup") => MARKUP,
                        _ => OTHER,
                    };
                    text.store(kind, Ordering::Relaxed);
                    if kind == PLAIN {
                        let mut markup = caps.caps_owned();
                        markup.make_mut().set("format", "pango-markup");
                        *event = gst::event::Caps::new(&markup);
                    }
                }
                Some(gst::PadProbeData::Buffer(buffer)) => {
                    let kind = text.load(Ordering::Relaxed);
                    if kind == OTHER {
                        return gst::PadProbeReturn::Ok;
                    }
                    let Ok(map) = buffer.map_readable() else {
                        return gst::PadProbeReturn::Ok;
                    };
                    let cue = String::from_utf8_lossy(&map);
                    let marked = match kind {
                        PLAIN => Some(crate::srt_markup::to_pango(&cue)),
                        _ => crate::srt_markup::repair_escaped(&cue),
                    };
                    drop(map);
                    if let Some(marked) = marked {
                        let mut out = gst::Buffer::from_mut_slice(marked.into_bytes());
                        {
                            let out = out.get_mut().expect("a new buffer is writable");
                            out.set_pts(buffer.pts());
                            out.set_dts(buffer.dts());
                            out.set_duration(buffer.duration());
                        }
                        *buffer = out;
                    }
                }
                _ => {}
            }
            gst::PadProbeReturn::Ok
        },
    );
}

/// gtk4paintablesink itself, and what GstPlay renders into (the same, or a
/// glsinkbin around it); the diagnostics read both.
#[derive(Clone)]
pub struct VideoSink {
    pub gtk: gst::Element,
    pub outer: gst::Element,
}

impl VideoSink {
    /// gtk4paintablesink, in a glsinkbin when it has a GL context, as
    /// gst-plugin-gtk4's own example does. Frames then stay on the GPU:
    /// dmabufs from the decoder go to GTK directly or through GL, and a
    /// 10-bit frame is converted on the GPU. Without it every frame is
    /// copied out and converted on the CPU, which 4K outruns.
    fn new() -> anyhow::Result<Self> {
        let gtk = gst::ElementFactory::make("gtk4paintablesink").build()?;
        // The sink finds its GL context on going to Ready. Safe here: the
        // sink is in no pipeline yet, so no streaming thread can hold it.
        gtk.set_state(gst::State::Ready)?;
        let paintable = gtk.property::<gtk::gdk::Paintable>("paintable");
        let gl = paintable
            .property::<Option<gtk::gdk::GLContext>>("gl-context")
            .is_some();
        let outer = match gl {
            true => gst::ElementFactory::make("glsinkbin")
                .property("sink", &gtk)
                .build()?,
            false => gtk.clone(),
        };
        Ok(Self { gtk, outer })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A second of tiny raw video with one SRT cue muxed in as Matroska's
    /// S_TEXT/UTF8, as a ripped file carries it.
    fn file_with_a_tagged_cue(path: &Path) {
        let mux = gst::parse::launch(&format!(
            "videotestsrc num-buffers=10 ! video/x-raw,width=64,height=48,framerate=10/1 \
             ! matroskamux name=mux ! filesink location={} \
             appsrc name=text format=time caps=text/x-raw,format=utf8 ! mux.subtitle_%u",
            path.display()
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        // appsrc by its action signals: no gstreamer-app crate for a test.
        let text = mux.by_name("text").unwrap();
        mux.set_state(gst::State::Playing).unwrap();
        let mut cue =
            gst::Buffer::from_slice(b"<font color=\"white\">PB&J under the bridge</font>".to_vec());
        {
            let cue = cue.get_mut().unwrap();
            cue.set_pts(gst::ClockTime::ZERO);
            cue.set_duration(gst::ClockTime::from_mseconds(800));
        }
        let _ = text.emit_by_name::<gst::FlowReturn>("push-buffer", &[&cue]);
        let _ = text.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
        wait_for_eos(&mux);
    }

    fn wait_for_eos(pipeline: &impl IsA<gst::Element>) {
        let bus = pipeline.bus().unwrap();
        let mut failed = None;
        for message in bus.iter_timed(gst::ClockTime::from_seconds(20)) {
            match message.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(e) => {
                    failed = Some(format!("{e:?}"));
                    break;
                }
                _ => {}
            }
        }
        // Down to NULL before any panic: a pipeline dropped while running
        // takes the test process down with it.
        pipeline.set_state(gst::State::Null).unwrap();
        if let Some(failed) = failed {
            panic!("{failed}");
        }
    }

    #[test]
    #[ignore = "needs GStreamer's base and good plugins; run on its own"]
    fn a_cue_in_the_file_reaches_the_overlay_as_styling() {
        gst::init().unwrap();
        let dir = tempfile_dir();
        let path = dir.join("cue.mkv");
        file_with_a_tagged_cue(&path);
        let playbin = gst::ElementFactory::make("playbin3")
            .property("uri", format!("file://{}", path.display()))
            .property(
                "video-sink",
                gst::ElementFactory::make("fakesink")
                    .property("sync", false)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        style_subtitles(&playbin);
        // What the overlay's text renderer is given.
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_ = seen.clone();
        playbin
            .downcast_ref::<gst::Bin>()
            .unwrap()
            .connect_deep_element_added(move |_, _, element| {
                let renderer = element.factory().is_some_and(|f| f.name() == "textoverlay");
                if let (true, Some(pad)) = (renderer, element.static_pad("text_sink")) {
                    let seen = seen_.clone();
                    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                        if let Some(gst::PadProbeData::Buffer(buffer)) = &info.data {
                            let map = buffer.map_readable().unwrap();
                            seen.lock()
                                .unwrap()
                                .push(String::from_utf8_lossy(&map).into_owned());
                        }
                        gst::PadProbeReturn::Ok
                    });
                }
            });
        playbin.set_state(gst::State::Playing).unwrap();
        wait_for_eos(&playbin);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            seen.lock().unwrap().first().map(String::as_str),
            Some("<span foreground=\"white\">PB&amp;J under the bridge</span>")
        );
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spoolframe-srt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
