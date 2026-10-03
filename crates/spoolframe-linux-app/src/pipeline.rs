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
    /// Dropped before GstPlay on the main thread: its handlers are bound
    /// to it, and the messages it holds hold GstPlay.
    adapter: Option<gst_play::PlaySignalAdapter>,
    video: VideoSink,
    paintable: gtk::gdk::Paintable,
    /// The reader GstPlay reads, reachable by its URI while published.
    published: RefCell<Option<Published>>,
    /// The external subtitle GstPlay was given last.
    subtitle: RefCell<Option<PathBuf>>,
}

impl Pipeline {
    pub fn new() -> anyhow::Result<Self> {
        let video = VideoSink::new()?;
        let paintable = video.gtk.property::<gtk::gdk::Paintable>("paintable");
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&video.outer);
        let play = gst_play::Play::new(Some(renderer));
        let mut config = play.config();
        config.set_position_update_interval(250);
        play.set_config(config)?;
        let adapter = gst_play::PlaySignalAdapter::new(&play);
        Ok(Self {
            play: Some(play),
            adapter: Some(adapter),
            video,
            paintable,
            published: RefCell::new(None),
            subtitle: RefCell::new(None),
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
        let play = self.play();
        play.stop();
        let published = Published::boxed(reader);
        play.set_uri(Some(&published.uri()));
        self.published.replace(Some(published));
        match subtitle {
            Some(path) => self.give_subtitle(Some(path)),
            None if self.subtitle.borrow().is_some() => self.give_subtitle(None),
            None => {}
        }
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
        self.play().stop();
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
    ) -> gtk::Popover {
        tracks::menu(self.play(), info, subtitles_on)
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
