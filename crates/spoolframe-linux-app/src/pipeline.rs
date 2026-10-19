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

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gst::prelude::*;
use gtk::glib;
use spoolframe_linux_core::reader::{self, ByteSource};

use crate::external_subtitle::ExternalSubtitle;
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
    /// Holds the external subtitle's buffers until it is shown (see
    /// [`crate::external_subtitle`]).
    external: ExternalSubtitle,
    analyzer: Option<crate::visualizer::Analyzer>,
    visualization_active: Cell<bool>,
}

impl Pipeline {
    pub fn new() -> anyhow::Result<Self> {
        let video = VideoSink::new()?;
        let paintable = video.gtk.property::<gtk::gdk::Paintable>("paintable");
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&video.outer);
        let play = gst_play::Play::new(Some(renderer));
        let analyzer = crate::visualizer::Analyzer::attach(&play.pipeline())
            .map_err(|error| tracing::warn!(%error, "audio analysis unavailable"))
            .ok();
        style_subtitles(&play.pipeline());
        let external = ExternalSubtitle::default();
        external.watch(&play.pipeline());
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
            external,
            analyzer,
            visualization_active: Cell::new(false),
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

    pub fn set_visualization(&self, name: Option<&str>) -> Result<(), glib::BoolError> {
        if let Some(name) = name {
            // Construct and validate the replacement before touching the active
            // visualizer, so an unavailable plugin cannot stop the music.
            let visualizer = crate::visualizer::gst_visualizer(name)?;
            self.play().set_visualization_enabled(false);
            self.play()
                .pipeline()
                .set_property("vis-plugin", &visualizer);
        }
        self.play().set_visualization_enabled(name.is_some());
        self.visualization_active.set(name.is_some());
        Ok(())
    }

    pub fn visualization_active(&self) -> bool {
        self.visualization_active.get()
    }

    /// Whether `message`, a GstPlay error, is the visualizer's, shown
    /// now over `music` or not (see [`crate::visualizer::caused`]).
    pub fn visualization_caused(&self, message: &str, music: bool) -> bool {
        self.visualization_active.get() && crate::visualizer::caused(message, music)
    }

    pub fn visualization_trails(&self, enabled: bool) {
        if let Some(plugin) = self
            .play()
            .pipeline()
            .property::<Option<gst::Element>>("vis-plugin")
        {
            let effect = plugin
                .downcast::<gst::Bin>()
                .ok()
                .and_then(|bin| bin.by_name("effect"));
            if let Some(effect) = effect
                && effect.find_property("shader").is_some()
            {
                effect.set_property_from_str("shader", if enabled { "fade" } else { "none" });
            }
        }
    }

    pub fn analysis(&self, enabled: bool) -> Option<crate::visualizer::Measurements> {
        self.analyzer.as_ref().map(|analyzer| {
            analyzer.enable(enabled);
            analyzer.levels.clone()
        })
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
        let uri = subtitle.map(file_uri);
        self.external.attach(uri.clone());
        // Setting even an empty suburi during source-setup can leave the
        // next item's pipeline at EOS without delivering its EOS message.
        // Touch it only to install a subtitle or clear a previous one.
        *self.loading_subtitle.lock().unwrap() =
            (subtitle.is_some() || self.subtitle.borrow().is_some()).then_some(uri);
        self.subtitle.replace(subtitle.map(Path::to_path_buf));
        // Shown once GstPlay lists it; see [`give_subtitle_uri`].
        if subtitle.is_some() {
            play.set_subtitle_track_enabled(false);
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
        give_subtitle_uri(self.play(), &self.external, path.map(file_uri).as_deref());
        self.subtitle.replace(path.map(Path::to_path_buf));
    }

    /// Shows the external subtitle once GstPlay lists it in `info`, as
    /// it does not pick it itself (see [`crate::external_subtitle`]).
    /// Whether it was shown now.
    pub fn show_external_subtitle(&self, info: &gst_play::PlayMediaInfo) -> bool {
        self.subtitle.borrow().is_some() && self.external.show(self.play(), info)
    }

    /// Whether GstPlay's error `message` is the external subtitle's, not
    /// the media's.
    pub fn subtitle_caused(&self, message: &str) -> bool {
        self.subtitle.borrow().is_some() && self.external.caused(message)
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
        tracks::select_subtitle(self.play(), info, position, &self.external)
    }

    /// The audio and subtitle menu for `info`; see [`tracks::menu`].
    pub fn track_menu(
        &self,
        info: &gst_play::PlayMediaInfo,
        subtitles_on: &std::rc::Rc<std::cell::Cell<bool>>,
        failed: std::rc::Rc<dyn Fn(&str)>,
        quality: Option<tracks::QualityMenu>,
        extras: tracks::SubtitleExtras,
    ) -> gtk::Popover {
        tracks::menu(
            self.play(),
            info,
            subtitles_on,
            failed,
            quality,
            self.external.clone(),
            extras,
        )
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

fn file_uri(path: &Path) -> String {
    gtk::prelude::FileExt::uri(&gtk::gio::File::for_path(path)).to_string()
}

/// Hands GstPlay `uri` as its external subtitle mid-play, on, or takes it
/// away. GstPlay restarts and seeks back.
///
/// Subtitles are off until [`ExternalSubtitle::show`] picks it: kept on,
/// GstPlay's restart for it shows the file's own subtitle first, and
/// decodebin3 moves its text output from a sparse stream to another only
/// on that stream's next buffer, which may not come for minutes.
fn give_subtitle_uri(play: &gst_play::Play, external: &ExternalSubtitle, uri: Option<&str>) {
    external.attach(uri.map(str::to_owned));
    play.set_subtitle_track_enabled(false);
    play.set_subtitle_uri(uri);
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

    /// Eight seconds of tiny raw video with a subtitle track of its own,
    /// as the episode that failed had eleven.
    fn file_with_its_own_subtitles(path: &Path) {
        let mux = gst::parse::launch(&format!(
            "videotestsrc num-buffers=80 ! video/x-raw,width=64,height=48,framerate=10/1 \
             ! matroskamux name=mux ! filesink location={} \
             appsrc name=text format=time caps=text/x-raw,format=utf8 ! mux.subtitle_%u",
            path.display()
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let text = mux.by_name("text").unwrap();
        mux.set_state(gst::State::Playing).unwrap();
        let mut cue = gst::Buffer::from_slice(b"the file's own cue".to_vec());
        {
            let cue = cue.get_mut().unwrap();
            cue.set_pts(gst::ClockTime::ZERO);
            cue.set_duration(gst::ClockTime::from_seconds(8));
        }
        let _ = text.emit_by_name::<gst::FlowReturn>("push-buffer", &[&cue]);
        let _ = text.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
        wait_for_eos(&mux);
    }

    /// A downloaded SRT's shape: a cue a second for ten minutes, with the
    /// ASS position tag the episode's had.
    fn external_srt(path: &Path) {
        let time = |s: u64| format!("{:02}:{:02}:{:02},417", s / 3600, s / 60 % 60, s % 60);
        let cues: String = (1..600)
            .map(|n| {
                format!(
                    "{n}\n{} --> {}\n{{\\an8}}[Vision] <i>external cue {n}</i>\n\n",
                    time(n - 1),
                    time(n)
                )
            })
            .collect();
        std::fs::write(path, cues).unwrap();
    }

    /// How a playback given an external subtitle went.
    struct Played {
        failed: Option<String>,
        /// Whether the failure was told to be the subtitle's.
        subtitles_fault: bool,
        /// The external subtitle's cues that reached the overlay.
        cues: usize,
        /// Whether the overlay drawing them was silenced at the end.
        silenced: bool,
    }

    /// Plays `uri` through GstPlay into fakesinks, and gives it `srt` as
    /// the app does once the item plays: mid-play, then picked when
    /// GstPlay lists it. With `off`, subtitles are then turned off from
    /// the menu.
    fn play_with_external_subtitle(uri: &str, srt: &Path, off: bool) -> Played {
        let play = gst_play::Play::new(None::<gst_play::PlayVideoRenderer>);
        let playbin = play.pipeline();
        let sink = |name| {
            gst::ElementFactory::make(name)
                .property("sync", true)
                .build()
                .unwrap()
        };
        playbin.set_property("video-sink", sink("fakesink"));
        playbin.set_property("audio-sink", sink("fakesink"));
        style_subtitles(&playbin);
        let external = ExternalSubtitle::default();
        external.watch(&playbin);
        let cues = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let overlays = Arc::new(Mutex::new(Vec::<gst::Element>::new()));
        let (counted, seen) = (cues.clone(), overlays.clone());
        playbin
            .downcast_ref::<gst::Bin>()
            .unwrap()
            .connect_deep_element_added(move |_, _, element| {
                let overlay = element
                    .factory()
                    .is_some_and(|f| f.name() == "subtitleoverlay");
                if overlay {
                    seen.lock().unwrap().push(element.clone());
                }
                if let (true, Some(pad)) = (overlay, element.static_pad("subtitle_sink")) {
                    let counted = counted.clone();
                    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                        if let Some(gst::PadProbeData::Buffer(buffer)) = &info.data
                            && let Ok(map) = buffer.map_readable()
                            && String::from_utf8_lossy(&map).contains("external cue")
                        {
                            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        gst::PadProbeReturn::Ok
                    });
                }
            });
        let bus = play.message_bus();
        play.set_uri(Some(uri));
        play.play();
        let srt = file_uri(srt);
        let started = std::time::Instant::now();
        let (mut given, mut shown, mut turned_off) = (None, None, false);
        let mut failed = None;
        while started.elapsed() < Duration::from_secs(20)
            && given.is_none_or(|at: std::time::Instant| at.elapsed() < Duration::from_secs(4))
        {
            let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) else {
                let one_second = gst::ClockTime::from_seconds(1);
                if given.is_none() && play.position().is_some_and(|p| p >= one_second) {
                    give_subtitle_uri(&play, &external, Some(&srt));
                    given = Some(std::time::Instant::now());
                }
                let showing = shown.is_some_and(|at: std::time::Instant| {
                    at.elapsed() > Duration::from_millis(1500)
                });
                if off && showing && !turned_off {
                    let info = play.media_info().unwrap();
                    crate::tracks::select_subtitle(&play, &info, None, &external).unwrap();
                    turned_off = true;
                }
                continue;
            };
            match gst_play::PlayMessage::parse(&message) {
                Ok(gst_play::PlayMessage::MediaInfoUpdated(info))
                    if given.is_some() && !turned_off =>
                {
                    if external.show(&play, info.media_info()) {
                        shown.get_or_insert_with(std::time::Instant::now);
                    }
                }
                Ok(gst_play::PlayMessage::Error(e)) => {
                    failed = Some(e.error().to_string());
                    break;
                }
                Ok(gst_play::PlayMessage::EndOfStream(_)) => break,
                _ => {}
            }
        }
        let silenced = overlays
            .lock()
            .unwrap()
            .last()
            .is_some_and(|o| o.property::<bool>("silent"));
        play.stop();
        let _ = playbin.set_state(gst::State::Null);
        Played {
            silenced,
            subtitles_fault: failed.as_deref().is_some_and(|e| external.caused(e)),
            failed,
            cues: cues.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    fn file_and_srt(name: &str) -> (spoolframe_linux_core::testing::TestDir, PathBuf, PathBuf) {
        gst::init().unwrap();
        let dir = spoolframe_linux_core::testing::TestDir::new_blocking(name);
        let (file, srt) = (dir.join("episode.mkv"), dir.join("external.srt"));
        file_with_its_own_subtitles(&file);
        external_srt(&srt);
        (dir, file, srt)
    }

    /// VisionQuest S01E02: picking an external subtitle stopped playback
    /// with "streaming stopped, reason not-linked" from the subtitle's
    /// typefind.
    #[test]
    #[ignore = "needs GStreamer's base and good plugins; run on its own"]
    fn an_external_subtitle_picked_mid_play_does_not_stop_playback() {
        let (_dir, file, srt) = file_and_srt("spoolframe-external-subtitle");
        crate::source::register().unwrap();
        let published = crate::source::tests::published(file);
        let played = play_with_external_subtitle(&published.uri(), &srt, false);
        assert_eq!(played.failed, None);
    }

    #[test]
    #[ignore = "needs GStreamer's base and good plugins; run on its own"]
    fn an_external_subtitle_picked_mid_play_is_shown() {
        let (_dir, file, srt) = file_and_srt("spoolframe-external-shown");
        let played = play_with_external_subtitle(&file_uri(&file), &srt, false);
        assert!(played.cues > 0, "no external cue reached the overlay");
    }

    #[test]
    #[ignore = "needs GStreamer's base and good plugins; run on its own"]
    fn a_broken_external_subtitle_is_told_from_the_media() {
        let (_dir, file, srt) = file_and_srt("spoolframe-external-broken");
        std::fs::write(
            &srt,
            (0..4096_u32)
                .map(|n| (n * 7919 % 251) as u8)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let played = play_with_external_subtitle(&file_uri(&file), &srt, false);
        assert!(played.subtitles_fault, "{:?}", played.failed);
    }

    #[test]
    #[ignore = "needs GStreamer's base and good plugins; run on its own"]
    fn turning_an_external_subtitle_off_silences_it() {
        let (_dir, file, srt) = file_and_srt("spoolframe-external-silenced");
        let played = play_with_external_subtitle(&file_uri(&file), &srt, true);
        assert!(played.silenced);
    }

    #[test]
    #[ignore = "needs GStreamer's base and good plugins; run on its own"]
    fn turning_an_external_subtitle_off_does_not_stop_playback() {
        let (_dir, file, srt) = file_and_srt("spoolframe-external-off");
        let played = play_with_external_subtitle(&file_uri(&file), &srt, true);
        assert_eq!(played.failed, None);
    }
}
