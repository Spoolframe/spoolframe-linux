//! Audio-reactive views. Streaming threads only publish measurements; GTK
//! owns the artwork and paints on its frame clock.
use gst::prelude::*;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Keep the two waveform scopes; the other GStreamer effects are unreliable
/// during live switches and are deliberately no longer exposed.
pub fn plugins() -> Vec<(String, String)> {
    [("wavescope", "Oscilloscope"), ("spacescope", "Vectorscope")]
        .into_iter()
        .filter(|(name, _)| gst::ElementFactory::find(name).is_some())
        .map(|(name, label)| (name.to_owned(), label.to_owned()))
        .collect()
}

fn visualization_caps() -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .field("width", 1920i32)
        .field("height", 1080i32)
        .field("framerate", gst::Fraction::new(60, 1))
        .build()
}

/// Explicit output caps avoid playbin choosing tiny 320×240 visualization
/// frames and stretching them across the application.
pub fn gst_visualizer(name: &str) -> Result<gst::Bin, glib::BoolError> {
    if !matches!(name, "wavescope" | "spacescope") {
        return Err(glib::bool_error!("Unsupported visualizer"));
    }
    let visualizer = gst::ElementFactory::make(name).name("effect").build()?;
    visualizer.set_property_from_str("style", "lines");
    // Fade over roughly 16 frames (~0.27s at 60Hz), retaining continuity
    // without the default's long, bright accumulation of old waveforms.
    visualizer.set_property("shade-amount", 0x00101010u32);
    let caps = visualization_caps();
    let output = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()?;
    let bin = gst::Bin::new();
    bin.add_many([&visualizer, &output])?;
    visualizer.link(&output)?;
    let sink = gst::GhostPad::with_target(
        &visualizer
            .static_pad("sink")
            .ok_or_else(|| glib::bool_error!("Visualizer has no audio input"))?,
    )?;
    let src = gst::GhostPad::with_target(&output.static_pad("src").unwrap())?;
    bin.add_pad(&sink)?;
    bin.add_pad(&src)?;
    Ok(bin)
}

/// Whether `message`, a GstPlay error ("Error from element <path>: …"),
/// comes from the visualizer's branch: playsink's vis chain, which holds
/// the effect, or, for `music`, the video sink the effect draws into
/// (music has no other picture).
pub fn caused(message: &str, music: bool) -> bool {
    let Some(path) = crate::pipeline::error_element_path(message) else {
        return false;
    };
    let names: Vec<_> = path
        .split('/')
        .map(|segment| segment.rsplit_once(':').map_or(segment, |(_, name)| name))
        .collect();
    names.windows(2).any(|pair| match pair {
        [parent, chain] if parent.starts_with("playsink") => {
            *chain == "visbin" || (music && *chain == "vbin")
        }
        _ => false,
    })
}

pub const BANDS: usize = 64;
#[derive(Clone)]
pub struct Levels {
    pub bands: [f64; BANDS],
    pub rms: [f64; 2],
    pub updated: Instant,
    pub clock: Option<BeatClock>,
}
impl Default for Levels {
    fn default() -> Self {
        Self {
            bands: [0.; BANDS],
            rms: [0.; 2],
            updated: Instant::now(),
            clock: None,
        }
    }
}
pub type Measurements = Arc<Mutex<Levels>>;

/// Keep malformed/non-finite values out of the renderer, and silence at zero.
fn normalized(db: f64) -> f64 {
    if db.is_finite() {
        ((db + 60.) / 60.).clamp(0., 1.)
    } else {
        0.
    }
}

/// Tempo and beat placement are delegated to stratum-dsp. Only buffering and
/// projecting its most recent beat grid onto the display clock live here.
#[derive(Clone, Copy)]
pub struct BeatClock {
    anchor: Instant,
    period: f64,
    updated: Instant,
}

fn analyze_rhythm(frames: &[Vec<f32>], interval: f64) -> Option<(f64, f64)> {
    use stratum_dsp::features::{
        beat_tracking::generate_beat_grid, onset::spectral_flux::detect_spectral_flux_onsets,
        period::tempogram::estimate_bpm_tempogram,
    };
    if frames.len() < 180 || frames.iter().flatten().all(|v| *v < 0.001) {
        return None;
    }
    // The library uses sample_rate / hop_size as the novelty-frame clock.
    let rate = 48000;
    let hop = (interval * rate as f64).round().max(1.) as u32;
    let tempo = estimate_bpm_tempogram(frames, rate, hop, 60., 200., 0.5).ok()?;
    let onsets: Vec<_> = detect_spectral_flux_onsets(frames, 0.8)
        .ok()?
        .into_iter()
        .map(|i| i as f32 * interval as f32)
        .collect();
    if onsets.len() < 4 {
        return None;
    }
    let (grid, stability) = generate_beat_grid(tempo.bpm, tempo.confidence, &onsets, rate).ok()?;
    let last = *grid.beats.last()? as f64;
    if stability < 0.5 || !last.is_finite() || !tempo.bpm.is_finite() {
        return None;
    }
    Some((tempo.bpm as f64, last))
}

/// Where the analysis is: bumped by each reset (a new stream, segment
/// or format, and each turn on or off), and whether it is on. Rhythm
/// read in one epoch is stale in the next.
#[derive(Clone, Default)]
struct Epoch(Arc<std::sync::atomic::AtomicU64>);

/// An [`Epoch`] as read at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Stamp(u64);

impl Stamp {
    fn is_on(self) -> bool {
        self.0 & 1 == 1
    }
}

impl Epoch {
    fn now(&self) -> Stamp {
        Stamp(self.0.load(std::sync::atomic::Ordering::Acquire))
    }

    /// A reset, on or off as it was.
    fn bump(&self) {
        self.0.fetch_add(2, std::sync::atomic::Ordering::AcqRel);
    }

    /// A reset that turns it `on` or off.
    fn turn(&self, on: bool) {
        use std::sync::atomic::Ordering;
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some(((old + 2) & !1) | u64::from(on))
            });
    }
}

/// How many frames the rhythm is read from at most: twelve seconds.
const RHYTHM_FRAMES: usize = 360;

struct RhythmWindow {
    /// The frames end to end, `width` values each, oldest first: one
    /// copy, split on the worker's thread.
    frames: Vec<f32>,
    width: usize,
    interval: f64,
    observed: Instant,
    generation: Stamp,
}

impl RhythmWindow {
    /// The frames one by one, as the rhythm analysis takes them.
    fn split(&self) -> Vec<Vec<f32>> {
        self.frames
            .chunks(self.width.max(1))
            .map(<[f32]>::to_vec)
            .collect()
    }
}

struct RhythmInput {
    /// The last [`RHYTHM_FRAMES`] frames, `width` values each, end to
    /// end, oldest first.
    frames: std::collections::VecDeque<f32>,
    width: usize,
    count: usize,
    generation: Stamp,
    last_time: Option<u64>,
    interval: f64,
    sender: std::sync::mpsc::SyncSender<RhythmWindow>,
}
impl RhythmInput {
    fn new(sender: std::sync::mpsc::SyncSender<RhythmWindow>) -> Self {
        Self {
            frames: Default::default(),
            width: 0,
            count: 0,
            generation: Stamp::default(),
            last_time: None,
            interval: 0.,
            sender,
        }
    }

    /// One spectrum's linear `power` (see [`power_spectrum`]), at
    /// `timestamp` for `duration`.
    fn push(
        &mut self,
        power: &[f64],
        timestamp: u64,
        duration: u64,
        generation: Stamp,
        observed: Instant,
    ) {
        let interval = duration as f64 / 1e9;
        if duration == 0 || !generation.is_on() {
            return;
        }
        // Pool linear magnitudes for rhythm analysis; spectrum rendering retains
        // the full FFT. This bounds window memory and background CPU use.
        let chunks = power.chunks(power.len().div_ceil(256).max(1));
        let width = chunks.len();
        if generation != self.generation
            || width != self.width
            || (interval - self.interval).abs() > 0.001
            || self.last_time.is_some_and(|last| {
                timestamp.abs_diff(last.saturating_add(duration)) > duration / 2
            })
        {
            self.frames.clear();
            self.count = 0;
        }
        self.generation = generation;
        self.width = width;
        self.last_time = Some(timestamp);
        self.interval = interval;
        self.frames.extend(
            chunks.map(|chunk| (chunk.iter().sum::<f64>() / chunk.len() as f64).sqrt() as f32),
        );
        let full = RHYTHM_FRAMES * width;
        if self.frames.len() > full {
            self.frames.drain(..self.frames.len() - full);
        }
        self.count += 1;
        if self.frames.len() >= RHYTHM_FRAMES / 2 * width && self.count.is_multiple_of(60) {
            let _ = self.sender.try_send(RhythmWindow {
                frames: self.frames.iter().copied().collect(),
                width,
                interval,
                observed,
                generation,
            });
        }
    }
}
fn rhythm_worker(output: Measurements, epoch: Epoch) -> RhythmInput {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<RhythmWindow>(1);
    std::thread::spawn(move || {
        while let Ok(window) = receiver.recv() {
            if epoch.now() != window.generation {
                continue;
            }
            let frames = window.split();
            let result = analyze_rhythm(&frames, window.interval);
            let mut levels = output.lock().unwrap();
            if epoch.now() != window.generation {
                continue;
            }
            levels.clock = result.and_then(|(bpm, last)| {
                let age = (frames.len() - 1) as f64 * window.interval - last;
                if age < 0. || age > frames.len() as f64 * window.interval {
                    return None;
                }
                Some(BeatClock {
                    anchor: window.observed
                        - std::time::Duration::from_secs_f64(age.rem_euclid(60. / bpm)),
                    period: 60. / bpm,
                    updated: window.observed,
                })
            });
        }
    });
    RhythmInput::new(sender)
}

/// Spectrum messages arrive before the sink presents their audio. Use the
/// pipeline clock to anchor the estimated grid to audible time, not bus arrival.
fn spectrum_time(source: &gst::Element, structure: &gst::StructureRef, duration: u64) -> Instant {
    let now = Instant::now();
    let Some(clock) = source.clock() else {
        return now;
    };
    let Some(base) = source.base_time() else {
        return now;
    };
    let Ok(running) = structure.get::<u64>("running-time") else {
        return now;
    };
    let target = base
        .nseconds()
        .saturating_add(running)
        .saturating_add(duration);
    let current = clock.time().nseconds();
    // Reject invalid/uninitialized timestamps rather than overflowing Instant.
    if target.abs_diff(current) > 5_000_000_000 {
        return now;
    }
    let delta = std::time::Duration::from_nanos(target.abs_diff(current));
    if target >= current {
        now + delta
    } else {
        now - delta
    }
}

/// The rhythm worker's input, while a visualizer is shown: the worker
/// thread lives exactly as long as it.
type Rhythm = Arc<Mutex<Option<RhythmInput>>>;

pub struct Analyzer {
    pub levels: Measurements,
    /// Closed (dropping) while no visualizer is shown: the music passes
    /// the analysis by, and nothing is converted or measured.
    valve: gst::Element,
    spectrum: gst::Element,
    level: gst::Element,
    epoch: Epoch,
    rhythm: Rhythm,
}
impl Analyzer {
    /// Puts the analysis in `playbin`'s audio filter, closed: the audio
    /// goes through a tee to the sink as it is, and a second branch, a
    /// valve that drops everything until [`Self::enable`], leads to the
    /// measurements. The filter stays in place for good, so turning a
    /// visualizer on or off never touches the audio's own path, and
    /// video plays past a closed valve.
    ///
    /// The branch ends unlinked, not in a sink: a sink's EOS is waited
    /// for, and a closed valve drops it, so the item would never end.
    /// The tee takes the branch's "not linked" for fine.
    pub fn attach(playbin: &gst::Element) -> anyhow::Result<Self> {
        let filter = gst::Bin::with_name("visual-analysis");
        let split = gst::ElementFactory::make("tee")
            .property("allow-not-linked", true)
            .build()?;
        let valve = gst::ElementFactory::make("valve")
            .property("drop", true)
            .build()?;
        let analysis = gst::parse::bin_from_description(
            "audioconvert ! spectrum name=visual-spectrum bands=8193 threshold=-60 interval=33333333 post-messages=false ! level name=visual-level interval=33333333 post-messages=false",
            true,
        )?;
        filter.add_many([&split, &valve, analysis.upcast_ref()])?;
        // The audio's pad first: the tee pushes to it before the analysis.
        let pad = || {
            split
                .request_pad_simple("src_%u")
                .ok_or_else(|| anyhow::anyhow!("the tee gives no pad"))
        };
        let (audio, analysed) = (pad()?, pad()?);
        analysed.link(&valve.static_pad("sink").unwrap())?;
        valve.link(&analysis)?;
        // Named as playsink looks for them.
        let sink = split.static_pad("sink").unwrap();
        filter.add_pad(
            &gst::GhostPad::builder_with_target(&sink)?
                .name("sink")
                .build(),
        )?;
        filter.add_pad(
            &gst::GhostPad::builder_with_target(&audio)?
                .name("src")
                .build(),
        )?;
        let spectrum = analysis.by_name("visual-spectrum").unwrap();
        let level = analysis.by_name("visual-level").unwrap();
        let levels = Measurements::default();
        let output = levels.clone();
        let epoch = Epoch::default();
        let rhythm: Rhythm = Arc::default();
        let input = rhythm.clone();
        let reset = epoch.clone();
        let reset_output = levels.clone();
        // The audio's sample rate, from its caps as they come; 0 until
        // known.
        let rate = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let caps_rate = rate.clone();
        spectrum.static_pad("sink").unwrap().add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                let Some(event) = info.event() else {
                    return gst::PadProbeReturn::Ok;
                };
                let reset_here = match event.view() {
                    gst::EventView::Caps(caps) => {
                        let known = caps
                            .caps()
                            .structure(0)
                            .and_then(|s| s.get::<i32>("rate").ok());
                        caps_rate.store(
                            known.unwrap_or(0).max(0) as u32,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                        true
                    }
                    gst::EventView::Segment(_) | gst::EventView::StreamStart(_) => true,
                    _ => false,
                };
                if reset_here {
                    reset.bump();
                    *reset_output.lock().unwrap() = Levels::default();
                }
                gst::PadProbeReturn::Ok
            },
        );
        let stamp = epoch.clone();
        let bus = playbin.bus().unwrap();
        let spectrum_source = spectrum.clone();
        let level_source = level.clone();
        bus.enable_sync_message_emission();
        // The spectrum's power, kept from message to message.
        let scratch = Mutex::new(Vec::new());
        // What a message measured goes out under the lock alone, and only
        // in the epoch it was measured in: a reset or a turn meanwhile,
        // which clears the levels, is not undone by it.
        let store = move |since: Stamp, write: &dyn Fn(&mut Levels)| {
            let mut values = output.lock().unwrap();
            if stamp.now() == since {
                write(&mut values);
                values.updated = Instant::now();
            }
        };
        let now = epoch.clone();
        bus.connect_sync_message(Some("element"), move |_, message| {
            let Some(s) = message.structure() else {
                return;
            };
            let since = now.now();
            if message.src() == Some(spectrum_source.upcast_ref()) {
                let Ok(magnitudes) = s.get::<gst::List>("magnitude") else {
                    return;
                };
                let mut power = scratch.lock().unwrap();
                power_spectrum(
                    magnitudes
                        .iter()
                        .map(|v| v.get::<f32>().unwrap_or(-60.) as f64),
                    &mut power,
                );
                let rate = match rate.load(std::sync::atomic::Ordering::Relaxed) {
                    0 => 44100.,
                    known => known as f64,
                };
                let bands = logarithmic_bands(&power, rate);
                if let (Ok(timestamp), Ok(duration)) =
                    (s.get::<u64>("timestamp"), s.get::<u64>("duration"))
                    && let Some(rhythm) = input.lock().unwrap().as_mut()
                {
                    rhythm.push(
                        &power,
                        timestamp,
                        duration,
                        since,
                        spectrum_time(&spectrum_source, s, duration),
                    );
                }
                store(since, &|values| values.bands = bands);
            } else if message.src() == Some(level_source.upcast_ref())
                && let Ok(channels) = s.get::<glib::ValueArray>("rms")
            {
                let first = channels
                    .first()
                    .and_then(|v| v.get::<f64>().ok())
                    .unwrap_or(-60.);
                let second = channels
                    .get(1)
                    .and_then(|v| v.get::<f64>().ok())
                    .unwrap_or(first);
                let rms = [normalized(first), normalized(second)];
                store(since, &|values| values.rms = rms);
            }
        });
        playbin.set_property("audio-filter", &filter);
        Ok(Self {
            levels,
            valve,
            spectrum,
            level,
            epoch,
            rhythm,
        })
    }
    /// Analyses the audio while a visualizer is shown, and starts the
    /// rhythm worker for it; off, the valve drops the audio before any
    /// analysis and the worker's thread ends.
    pub fn enable(&self, enabled: bool) {
        self.epoch.turn(enabled);
        {
            let mut rhythm = self.rhythm.lock().unwrap();
            if !enabled {
                // Its sender gone, the worker's thread ends.
                *rhythm = None;
            } else if rhythm.is_none() {
                *rhythm = Some(rhythm_worker(self.levels.clone(), self.epoch.clone()));
            }
        }
        self.spectrum.set_property("post-messages", enabled);
        self.level.set_property("post-messages", enabled);
        self.valve.set_property("drop", !enabled);
        if !enabled {
            *self.levels.lock().unwrap() = Levels::default();
        }
    }

    /// Whether the rhythm worker runs.
    #[cfg(test)]
    fn rhythm_running(&self) -> bool {
        self.rhythm.lock().unwrap().is_some()
    }
}

/// What Now Playing shows over the music.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// No visualizer: the cover and the queue.
    Cover,
    /// One of the canvas's own modes, drawn from the measurements.
    Canvas(Mode),
    /// A GStreamer effect from [`plugins`], by its element's name.
    Effect(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Mode {
    #[default]
    Radial,
    Vu,
    Ambient,
    Spectrum,
    Spectrogram,
    Rings,
}

impl Mode {
    /// Whether it draws the album's cover.
    pub fn draws_cover(self) -> bool {
        matches!(self, Mode::Radial | Mode::Ambient | Mode::Rings)
    }

    /// Whether its colours are tinted by the album's cover.
    pub fn tinted(self) -> bool {
        matches!(self, Mode::Radial | Mode::Rings | Mode::Spectrum)
    }
}

mod imp {
    use super::*;
    #[derive(Default)]
    pub struct Canvas {
        pub levels: RefCell<Measurements>,
        pub frame: RefCell<Levels>,
        pub texture: RefCell<Option<gdk::Texture>>,
        pub mode: Cell<Mode>,
        pub time: Cell<f64>,
        pub phase: Cell<f64>,
        pub peaks: RefCell<Vec<f64>>,
        pub show_peaks: Cell<bool>,
        pub waterfall: RefCell<Waterfall>,
        /// The VU deck's scale labels' widths, measured on its first draw:
        /// their font does not change.
        pub vu_labels: Cell<Option<[f64; 4]>>,
        pub color: Cell<(f64, f64, f64)>,
        pub punch: Cell<f64>,
        /// The frame clock's callback, while it runs (see
        /// [`super::Canvas::set_playing`]).
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub playing: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "SpoolframeVisualizer";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Canvas {}
    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let w = obj.width() as f32;
            let h = obj.height() as f32;
            if w <= 0. || h <= 0. {
                return;
            }
            let bounds = graphene::Rect::new(0., 0., w, h);
            snapshot.append_color(&gdk::RGBA::new(0.025, 0.03, 0.05, 1.), &bounds);
            let values = self.frame.borrow();
            let texture = self.texture.borrow();
            let mode = self.mode.get();
            if mode == Mode::Ambient
                && let Some(texture) = texture.as_ref()
            {
                draw_ambient(snapshot, &bounds, texture, &values, self.phase.get());
            }
            if mode.draws_cover() {
                draw_cover(snapshot, w, h, texture.as_ref());
            }
            let color = self.color.get();
            match mode {
                Mode::Ambient => {}
                Mode::Spectrogram => self.waterfall.borrow_mut().draw(snapshot, &bounds),
                Mode::Radial => draw_radial(&snapshot.append_cairo(&bounds), w, h, &values, color),
                Mode::Rings => draw_rings(
                    &snapshot.append_cairo(&bounds),
                    w,
                    h,
                    &values,
                    color,
                    (self.phase.get(), self.punch.get()),
                ),
                Mode::Spectrum => {
                    let peaks = self.peaks.borrow();
                    draw_spectrum(
                        &snapshot.append_cairo(&bounds),
                        w,
                        h,
                        &values,
                        color,
                        self.show_peaks.get().then_some(peaks.as_slice()),
                    );
                }
                Mode::Vu => draw_vu(
                    &snapshot.append_cairo(&bounds),
                    w,
                    h,
                    &values,
                    &self.vu_labels,
                ),
            }
        }
    }
}

/// The cover's side, in the middle of a `w`×`h` canvas.
fn cover_size(w: f32, h: f32) -> f32 {
    w.min(h) * 0.38
}

/// The album's cover, blurred, filling the canvas: it swells with the
/// bass and drifts with `drift`, darkened less as the music is louder.
fn draw_ambient(
    snapshot: &gtk::Snapshot,
    bounds: &graphene::Rect,
    texture: &gdk::Texture,
    values: &Levels,
    drift: f64,
) {
    let (w, h) = (bounds.width(), bounds.height());
    let bass = values.bands[..16].iter().sum::<f64>() / 16.;
    let energy = (values.rms[0] + values.rms[1]) / 2.;
    let zoom = 1.25 + bass as f32 * 0.16;
    let width = w * zoom;
    let height = h * zoom;
    snapshot.push_clip(bounds);
    snapshot.push_blur(48.);
    snapshot.append_texture(
        texture,
        &graphene::Rect::new(
            (w - width) / 2. + drift.sin() as f32 * w * 0.04,
            (h - height) / 2. + (drift * 0.7).cos() as f32 * h * 0.04,
            width,
            height,
        ),
    );
    snapshot.pop();
    snapshot.pop();
    snapshot.append_color(
        &gdk::RGBA::new(0., 0., 0., (0.42 - energy * 0.15) as f32),
        bounds,
    );
}

/// The cover in the middle, rounded; a plain tile until it loads.
fn draw_cover(snapshot: &gtk::Snapshot, w: f32, h: f32, texture: Option<&gdk::Texture>) {
    let size = cover_size(w, h);
    let rect = graphene::Rect::new((w - size) / 2., (h - size) / 2., size, size);
    snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(rect, 18.));
    if let Some(texture) = texture {
        snapshot.append_texture(texture, &rect);
    } else {
        snapshot.append_color(&gdk::RGBA::new(0.18, 0.22, 0.32, 1.), &rect);
    }
    snapshot.pop();
}

/// A spoke per band around the cover, as long as the band is loud.
fn draw_radial(cr: &gtk::cairo::Context, w: f32, h: f32, values: &Levels, color: (f64, f64, f64)) {
    let radius = cover_size(w, h) as f64 * 0.76;
    cr.set_line_width((w.min(h) as f64 * 0.006).max(2.));
    cr.set_line_cap(gtk::cairo::LineCap::Round);
    for (i, value) in values.bands.iter().enumerate() {
        let angle = i as f64 / BANDS as f64 * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
        let length = 4. + value * w.min(h) as f64 * 0.14;
        let (r, g, b) = band_color(color, i as f64 / BANDS as f64, *value);
        cr.set_source_rgb(r, g, b);
        cr.move_to(
            w as f64 / 2. + angle.cos() * radius,
            h as f64 / 2. + angle.sin() * radius,
        );
        cr.line_to(
            w as f64 / 2. + angle.cos() * (radius + length),
            h as f64 / 2. + angle.sin() * (radius + length),
        );
        let _ = cr.stroke();
    }
}

/// How many points each of the rings is drawn through.
const RING_POINTS: usize = 256;

/// The cosine and sine of each of a ring's points, once.
fn ring_points() -> &'static [(f64, f64); RING_POINTS + 1] {
    static POINTS: std::sync::OnceLock<[(f64, f64); RING_POINTS + 1]> = std::sync::OnceLock::new();
    POINTS.get_or_init(|| {
        std::array::from_fn(|point| {
            let angle = point as f64 / RING_POINTS as f64 * std::f64::consts::TAU;
            (angle.cos(), angle.sin())
        })
    })
}

/// Five waving rings around the cover, each from a fifth of the bands,
/// turning by `phase` and pushed out by the beat's `punch`.
fn draw_rings(
    cr: &gtk::cairo::Context,
    w: f32,
    h: f32,
    values: &Levels,
    color: (f64, f64, f64),
    (phase, punch): (f64, f64),
) {
    let unit = w.min(h) as f64;
    let size = cover_size(w, h) as f64;
    cr.set_line_width((unit * 0.0035).max(1.5));
    for ring in 0..5 {
        let first = ring * 12;
        let energy = values.bands[first..first + 12].iter().sum::<f64>() / 12.;
        let radius = size * 0.76
            + ring as f64 * unit * 0.025
            + punch * unit * 0.008 * (1. + ring as f64 * 0.12);
        let (r, g, b) = band_color(color, ring as f64 / 5., energy);
        cr.set_source_rgba(r, g, b, 0.65 + energy * 0.35);
        let rotation = phase * (1. + ring as f64 * 0.15) * if ring % 2 == 0 { 1. } else { -1. };
        for (point, (cos, sin)) in ring_points().iter().enumerate() {
            let angle = point as f64 / RING_POINTS as f64 * std::f64::consts::TAU;
            let rotated = angle + rotation;
            let wave = (rotated * (3. + ring as f64 * 2.)).sin()
                + 0.3 * (rotated * (7. + ring as f64)).sin();
            let distance = radius + wave * unit * (0.012 + energy * 0.004 + punch * 0.006);
            let x = w as f64 / 2. + cos * distance;
            let y = h as f64 / 2. + sin * distance;
            if point == 0 {
                cr.move_to(x, y);
            } else {
                cr.line_to(x, y);
            }
        }
        cr.close_path();
        let _ = cr.stroke();
    }
}

/// A bar per band, lit from below, with each band's falling peak over
/// it when `peaks` are shown.
fn draw_spectrum(
    cr: &gtk::cairo::Context,
    w: f32,
    h: f32,
    values: &Levels,
    color: (f64, f64, f64),
    peaks: Option<&[f64]>,
) {
    let margin = (w as f64 * 0.06).max(16.);
    let bottom = h as f64 * 0.78;
    let height = h as f64 * 0.55;
    let step = (w as f64 - 2. * margin) / BANDS as f64;
    let bar = (step - 3.).max(1.);
    for (i, value) in values.bands.iter().enumerate() {
        let x = margin + i as f64 * step;
        let (r, g, b) = band_color(color, i as f64 / BANDS as f64, *value);
        let gradient = gtk::cairo::LinearGradient::new(0., bottom, 0., bottom - height);
        gradient.add_color_stop_rgb(0., r * 0.4, g * 0.4, b * 0.4);
        gradient.add_color_stop_rgb(0.55, r, g, b);
        gradient.add_color_stop_rgb(
            1.,
            (r + 0.35).min(1.),
            (g + 0.35).min(1.),
            (b + 0.35).min(1.),
        );
        let _ = cr.set_source(&gradient);
        cr.rectangle(x, bottom - value * height, bar, (value * height).max(1.));
        let _ = cr.fill();
        if let Some(peak) = peaks.and_then(|peaks| peaks.get(i)) {
            cr.set_source_rgba(0.95, 0.97, 1., 0.8);
            cr.rectangle(x, bottom - peak * height - 3., bar, 2.);
            let _ = cr.fill();
        }
    }
}

/// The VU deck's scale, under its meters.
const VU_LABELS: [&str; 4] = ["−60", "−40", "−20", "0 dBFS"];

/// Two segmented meters, left and right, over a dBFS scale. `labels`
/// keeps the scale's widths once measured.
fn draw_vu(
    cr: &gtk::cairo::Context,
    w: f32,
    h: f32,
    values: &Levels,
    labels: &Cell<Option<[f64; 4]>>,
) {
    let width = (w as f64 * 0.8).min(950.);
    let x = (w as f64 - width) / 2.;
    let y = h as f64 / 2.;
    cr.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Bold,
    );
    cr.set_font_size(18.);
    for (channel, level) in values.rms.iter().enumerate() {
        let cy = y + (channel as f64 - 0.5) * 90.;
        cr.set_source_rgb(0.75, 0.8, 0.85);
        cr.move_to(x, cy + 22.);
        let _ = cr.show_text(if channel == 0 { "L" } else { "R" });
        for segment in 0..40 {
            let amount = segment as f64 / 40.;
            let lit = amount < *level;
            let color = if segment > 35 {
                (1., 0.2, 0.25)
            } else if segment > 28 {
                (1., 0.75, 0.2)
            } else {
                (0.2, 0.9, 0.6)
            };
            cr.set_source_rgba(color.0, color.1, color.2, if lit { 1. } else { 0.12 });
            cr.rectangle(
                x + 32. + segment as f64 * (width - 32.) / 40.,
                cy,
                (width - 32.) / 40. - 4.,
                32.,
            );
            let _ = cr.fill();
        }
    }
    cr.set_source_rgb(0.55, 0.6, 0.65);
    cr.set_font_size(14.);
    let widths = labels.get().unwrap_or_else(|| {
        let widths = VU_LABELS.map(|label| cr.text_extents(label).map(|e| e.width()).unwrap_or(0.));
        labels.set(Some(widths));
        widths
    });
    for (i, (label, label_width)) in VU_LABELS.iter().zip(widths).enumerate() {
        cr.move_to(
            x + 32. + i as f64 * (width - 32. - label_width) / 3.,
            y + 115.,
        );
        let _ = cr.show_text(label);
    }
}

/// How many columns the spectrogram's texture has: 181 thirtieths of a
/// second, and the newest again, for scrolling between them.
const WATERFALL_COLUMNS: usize = 182;

/// The spectrogram: the bands over the last six seconds, the newest on
/// the right, kept as the pixels of a small texture. A new column is
/// coloured once, as it comes in; the rest move over.
pub struct Waterfall {
    /// RGBA, [`WATERFALL_COLUMNS`] wide and [`BANDS`] high, the highest
    /// band on top.
    pixels: Vec<u8>,
    /// Made from `pixels` once drawn after a change.
    texture: Option<gdk::MemoryTexture>,
    /// When, on the frame clock, the last column came in; 0 before one.
    time: f64,
    /// How far towards the next column it has scrolled, 0 to 1.
    offset: f64,
}

impl Default for Waterfall {
    fn default() -> Self {
        let [r, g, b] = spectrogram_rgb(0.);
        Self {
            pixels: [r, g, b, 255].repeat(WATERFALL_COLUMNS * BANDS),
            texture: None,
            time: 0.,
            offset: 0.,
        }
    }
}

impl Waterfall {
    /// Empty, as for a new song.
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Scrolls on from the next frame, as for a mode just shown.
    fn rewind(&mut self) {
        self.time = 0.;
        self.offset = 0.;
    }

    /// Moves on to `now`, `frame` coming in for each thirtieth of a
    /// second since the last column. The fraction left over is kept:
    /// snapping to `now` drifts against the display clock and scrolls
    /// unevenly.
    fn advance(&mut self, now: f64, frame: &[f64; BANDS]) {
        let elapsed = if self.time == 0. {
            1. / 30.
        } else {
            (now - self.time).max(0.)
        };
        let steps = (elapsed * 30.).floor() as usize;
        if steps > 0 {
            self.time = now - (elapsed * 30.).fract() / 30.;
            for _ in 0..steps.min(WATERFALL_COLUMNS - 1) {
                self.push(frame);
            }
        }
        self.offset = (elapsed * 30.).fract();
    }

    /// `frame` as the newest column, the others one to the left.
    fn push(&mut self, frame: &[f64; BANDS]) {
        let row = WATERFALL_COLUMNS * 4;
        for (line, value) in self.pixels.chunks_exact_mut(row).zip(frame.iter().rev()) {
            line.copy_within(4.., 0);
            let [r, g, b] = spectrogram_rgb(*value);
            for pixel in line[row - 8..].as_chunks_mut::<4>().0 {
                *pixel = [r, g, b, 255];
            }
        }
        self.texture = None;
    }

    fn draw(&mut self, snapshot: &gtk::Snapshot, bounds: &graphene::Rect) {
        let texture = self.texture.get_or_insert_with(|| {
            gdk::MemoryTexture::new(
                WATERFALL_COLUMNS as i32,
                BANDS as i32,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from(self.pixels.as_slice()),
                WATERFALL_COLUMNS * 4,
            )
        });
        let step = bounds.width() / 180.;
        snapshot.push_clip(bounds);
        snapshot.append_texture(
            &*texture,
            &graphene::Rect::new(
                -step * (1. + self.offset as f32),
                0.,
                step * WATERFALL_COLUMNS as f32,
                bounds.height(),
            ),
        );
        snapshot.pop();
    }
}
glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}
impl Canvas {
    pub fn new(levels: Measurements) -> Self {
        let obj: Self = glib::Object::builder().build();
        *obj.imp().levels.borrow_mut() = levels;
        obj.imp().color.set(DEFAULT_TINT);
        obj.imp().show_peaks.set(true);
        obj.set_hexpand(true);
        obj.set_vexpand(true);
        // Shown only while a visualizer mode is: the stacks unmap it
        // for the cover or a GStreamer effect.
        obj.connect_map(|obj| obj.run());
        obj.connect_unmap(|obj| obj.halt());
        obj
    }

    /// Whether the music plays: paused, the canvas stops drawing once
    /// what it shows has settled.
    pub fn set_playing(&self, playing: bool) {
        self.imp().playing.set(playing);
        self.run();
    }

    /// Draws on the frame clock, if shown and not running already.
    fn run(&self) {
        let imp = self.imp();
        if !self.is_mapped() || imp.tick.borrow().is_some() {
            return;
        }
        let tick = self.add_tick_callback(|obj, clock| obj.step(clock));
        imp.tick.replace(Some(tick));
    }

    fn halt(&self) {
        if let Some(tick) = self.imp().tick.take() {
            tick.remove();
        }
    }

    /// One frame: the measurements eased in, and a draw. Paused and
    /// settled, it ends (the id is only let go: Break removes it).
    fn step(&self, clock: &gdk::FrameClock) -> glib::ControlFlow {
        let obj = self;
        {
            let imp = obj.imp();
            let target = imp.levels.borrow().lock().unwrap().clone();
            let now = clock.frame_time() as f64 / 1_000_000.;
            let dt = (now - imp.time.replace(now)).clamp(0., 0.1);
            let fresh = target.updated.elapsed().as_millis() < 250;
            let pulse = target
                .clock
                .filter(|clock| fresh && clock.updated.elapsed().as_secs_f64() < 4.)
                .map(|clock| {
                    let phase = clock
                        .anchor
                        .elapsed()
                        .as_secs_f64()
                        .rem_euclid(clock.period);
                    // Smooth attack, then decay; no instantaneous phase jumps.
                    (1. - (-phase / 0.045).exp()) * (-phase / (clock.period * 0.25)).exp() * 1.8
                })
                .unwrap_or(0.);
            imp.punch.set(pulse);
            if fresh {
                imp.phase
                    .set(imp.phase.get() + dt * (0.12 + target.rms[0] * 0.15));
            }
            let mut frame = imp.frame.borrow_mut();
            for (out, input) in frame.bands.iter_mut().zip(target.bands) {
                smooth(out, if fresh { input } else { 0. }, dt);
            }
            for (out, input) in frame.rms.iter_mut().zip(target.rms) {
                smooth(out, if fresh { input } else { 0. }, dt);
            }
            {
                let mut peaks = imp.peaks.borrow_mut();
                peaks.resize(BANDS, 0.);
                for (peak, band) in peaks.iter_mut().zip(frame.bands) {
                    *peak = band.max(*peak - dt * 0.18);
                }
            }
            if imp.mode.get() == Mode::Spectrogram {
                imp.waterfall.borrow_mut().advance(now, &frame.bands);
            }
        }
        obj.queue_draw();
        let imp = obj.imp();
        if !imp.playing.get() && settled(&imp.frame.borrow(), imp.punch.get()) {
            imp.tick.take();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    }
    pub fn set_mode(&self, mode: Mode) {
        self.imp().mode.set(mode);
        self.imp().waterfall.borrow_mut().rewind();
        self.queue_draw();
    }
    pub fn set_peaks(&self, enabled: bool) {
        self.imp().show_peaks.set(enabled);
    }
    pub fn mode(&self) -> Mode {
        self.imp().mode.get()
    }

    /// A new song: the spectrogram starts empty.
    pub fn new_song(&self) {
        self.imp().waterfall.borrow_mut().reset();
        self.queue_draw();
    }

    /// The album's cover, for the modes that draw it; a plain tile
    /// without.
    pub fn set_cover(&self, texture: Option<gdk::Texture>) {
        *self.imp().texture.borrow_mut() = texture;
        self.queue_draw();
    }

    /// The colours' tint, from the album's cover (a small one does), or
    /// the canvas's own blue without.
    pub fn set_tint(&self, cover: Option<&gdk::Texture>) {
        self.imp()
            .color
            .set(cover.map(art_color).unwrap_or(DEFAULT_TINT));
        self.queue_draw();
    }
}
/// The linear power of each of the spectrum's `levels` (dB, with -60
/// its floor), into `out`: what the bands and the rhythm are read from.
fn power_spectrum(levels: impl IntoIterator<Item = f64>, out: &mut Vec<f64>) {
    out.clear();
    out.extend(levels.into_iter().map(|db| {
        if db.is_finite() && db > -60. {
            10f64.powf(db.min(0.) / 10.)
        } else {
            0.
        }
    }));
}

/// Integrate linearly interpolated FFT power over each logarithmic band.
/// This includes partial bins, so bands narrower than the FFT resolution do
/// not become artificial black stripes. It does not invent extra resolution.
/// The first bin, the DC offset, counts for nothing.
fn logarithmic_bands(power: &[f64], rate: f64) -> [f64; BANDS] {
    if power.len() < 2 || !rate.is_finite() || rate <= 40. {
        return [0.; BANDS];
    }
    let bin_hz = rate / (2. * (power.len() - 1) as f64);
    let at = |bin: usize| if bin == 0 { 0. } else { power[bin] };
    std::array::from_fn(|i| {
        let low = 20. * 1000f64.powf(i as f64 / BANDS as f64);
        let high = (20. * 1000f64.powf((i + 1) as f64 / BANDS as f64)).min(rate / 2.);
        if high <= low {
            return 0.;
        }
        let start = low / bin_hz;
        let end = high / bin_hz;
        let mut integral = 0.;
        for bin in start.floor() as usize..(end.ceil() as usize).min(power.len() - 1) {
            let left = (start - bin as f64).max(0.);
            let right = (end - bin as f64).min(1.);
            let slope = at(bin + 1) - at(bin);
            integral += (right - left) * (at(bin) + slope * (left + right) / 2.);
        }
        let average = integral / (end - start);
        if average > 0. {
            normalized(10. * average.log10())
        } else {
            0.
        }
    })
}

/// The canvas's colours without a cover to tint them.
const DEFAULT_TINT: (f64, f64, f64) = (0.4, 0.65, 0.95);

/// The cover's colour: its pixels on a 32×32 grid, the more saturated
/// counting more.
fn art_color(texture: &gdk::Texture) -> (f64, f64, f64) {
    let (bytes, stride) = crate::artwork::pixels(texture, gdk::MemoryFormat::R8g8b8a8);
    let mut sum = [0.; 3];
    let mut count = 0.;
    for y in (0..texture.height() as usize).step_by((texture.height() as usize / 32).max(1)) {
        for x in (0..texture.width() as usize).step_by((texture.width() as usize / 32).max(1)) {
            let offset = y * stride + x * 4;
            if let Some(pixel) = bytes.get(offset..offset + 4) {
                let rgb = [
                    pixel[0] as f64 / 255.,
                    pixel[1] as f64 / 255.,
                    pixel[2] as f64 / 255.,
                ];
                let max = rgb.iter().copied().fold(0., f64::max);
                let min = rgb.iter().copied().fold(1., f64::min);
                let weight = (max - min).max(0.05);
                for i in 0..3 {
                    sum[i] += rgb[i] * weight;
                }
                count += weight;
            }
        }
    }
    let max = sum.iter().copied().fold(0., f64::max);
    if count == 0. || max == 0. {
        return DEFAULT_TINT;
    }
    // Keep even dark album colors legible against the dark canvas.
    let scale = (0.85 / (max / count)).max(1.);
    (
        (sum[0] / count * scale).min(1.),
        (sum[1] / count * scale).min(1.),
        (sum[2] / count * scale).min(1.),
    )
}

/// Album tint anchors a saturated spectral palette instead of reducing the
/// entire display to one averaged color. Energy lifts the highlights.
fn band_color(album: (f64, f64, f64), band: f64, energy: f64) -> (f64, f64, f64) {
    let hue = band * 0.82;
    let channel = |offset: f64, tint: f64| {
        let wave = ((hue + offset) * std::f64::consts::TAU).cos() * 0.5 + 0.5;
        (0.12 + wave * 0.72 + tint * 0.18 + energy * 0.12).min(1.)
    };
    (
        channel(0., album.0),
        channel(2. / 3., album.1),
        channel(1. / 3., album.2),
    )
}

/// How finely [`spectrogram_rgb`] tells levels apart.
const SPECTROGRAM_SHADES: usize = 1024;

/// [`spectrogram_color`] as a pixel's bytes, from a table made once: a
/// column of the spectrogram is coloured without a `powf` per pixel.
fn spectrogram_rgb(value: f64) -> [u8; 3] {
    static SHADES: std::sync::OnceLock<Vec<[u8; 3]>> = std::sync::OnceLock::new();
    let shades = SHADES.get_or_init(|| {
        (0..=SPECTROGRAM_SHADES)
            .map(|i| {
                let (r, g, b) = spectrogram_color(i as f64 / SPECTROGRAM_SHADES as f64);
                [(r * 255.) as u8, (g * 255.) as u8, (b * 255.) as u8]
            })
            .collect()
    });
    let value = if value.is_finite() {
        value.clamp(0., 1.)
    } else {
        0.
    };
    shades[(value * SPECTROGRAM_SHADES as f64).round() as usize]
}

/// A brighter heat map with a gentle lift for quiet detail. Silence remains
/// dark; color encodes energy consistently across all frequency bands.
fn spectrogram_color(value: f64) -> (f64, f64, f64) {
    const COLORS: [(f64, f64, f64); 7] = [
        (0.025, 0.03, 0.05),
        (0.35, 0.08, 0.65),
        (0.12, 0.35, 1.),
        (0.05, 0.9, 0.95),
        (0.95, 0.9, 0.2),
        (1., 0.4, 0.1),
        (1., 0.98, 0.85),
    ];
    let position = value.clamp(0., 1.).powf(0.7) * (COLORS.len() - 1) as f64;
    let index = (position as usize).min(COLORS.len() - 2);
    let mix = position - index as f64;
    let (r, g, b) = COLORS[index];
    let (nr, ng, nb) = COLORS[index + 1];
    (r + (nr - r) * mix, g + (ng - g) * mix, b + (nb - b) * mix)
}

/// Whether nothing on the canvas moves any more: every level at rest.
fn settled(frame: &Levels, punch: f64) -> bool {
    frame
        .bands
        .iter()
        .chain(&frame.rms)
        .chain([&punch])
        .all(|v| *v < 0.001)
}

fn smooth(out: &mut f64, target: f64, dt: f64) {
    let speed = if target > *out { 22. } else { 7. };
    *out += (target - *out) * (1. - (-dt * speed).exp());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gst_play_publishes_real_audio_measurements() {
        gst::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // Use a supported playback format: other tests apply the process-wide
        // decoder denylist, which intentionally removes wavparse.
        let path = dir.path().join("tone.flac");
        let generator = gst::parse::launch(&format!(
            "audiotestsrc num-buffers=600 freq=440 volume=0.5 ! audioconvert ! flacenc ! filesink location={}", path.display()
        )).unwrap().downcast::<gst::Pipeline>().unwrap();
        generator.set_state(gst::State::Playing).unwrap();
        let done = generator.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        generator.set_state(gst::State::Null).unwrap();
        assert!(matches!(
            done.as_ref().map(|m| m.view()),
            Some(gst::MessageView::Eos(_))
        ));
        let video = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .property("signal-handoffs", true)
            .build()
            .unwrap();
        let frames = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let received_frames = frames.clone();
        video.connect("handoff", false, move |_| {
            received_frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        });
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&video);
        let play = gst_play::Play::new(Some(renderer));
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .build()
            .unwrap();
        play.pipeline().set_property("audio-sink", &sink);
        let analyzer = Analyzer::attach(&play.pipeline()).unwrap();
        analyzer.enable(true);
        play.set_uri(Some(gtk::gio::File::for_path(&path).uri().as_str()));
        play.play();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut received = false;
        while Instant::now() < deadline {
            let values = analyzer.levels.lock().unwrap().clone();
            if values.rms[0] > 0.5 && values.bands.iter().any(|v| *v > 0.5) {
                received = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            received,
            "GstPlay should deliver spectrum and RMS measurements"
        );
        for (name, _) in plugins() {
            let before = frames.load(std::sync::atomic::Ordering::Relaxed);
            let effect = gst_visualizer(&name).unwrap();
            play.pipeline().set_property("vis-plugin", &effect);
            play.set_visualization_enabled(true);
            let deadline = Instant::now() + std::time::Duration::from_secs(2);
            while frames.load(std::sync::atomic::Ordering::Relaxed) == before
                && Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(
                frames.load(std::sync::atomic::Ordering::Relaxed) > before,
                "{name} should render video while audio continues"
            );
            let caps = video.static_pad("sink").unwrap().current_caps().unwrap();
            assert_eq!(
                caps.structure(0).unwrap().get::<i32>("width").unwrap(),
                1920
            );
            assert_eq!(
                caps.structure(0).unwrap().get::<i32>("height").unwrap(),
                1080
            );
            assert_eq!(
                caps.structure(0)
                    .unwrap()
                    .get::<gst::Fraction>("framerate")
                    .unwrap(),
                gst::Fraction::new(60, 1)
            );
            assert_eq!(
                effect
                    .by_name("effect")
                    .unwrap()
                    .property::<u32>("shade-amount"),
                0x00101010
            );
            play.set_visualization_enabled(false);
        }
        analyzer.enable(false);
        assert_eq!(analyzer.levels.lock().unwrap().rms, [0., 0.]);
        play.stop();
        play.pipeline().set_state(gst::State::Null).unwrap();
        assert!(
            received,
            "GstPlay should deliver spectrum and RMS measurements"
        );
    }

    /// With no visualizer shown, the music goes by the analysis: nothing
    /// reaches the spectrum and no rhythm thread runs, while the audio
    /// sink plays on through turning it on and off again.
    #[test]
    fn audio_is_analysed_only_while_a_visualizer_is_shown() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        gst::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.flac");
        let generator = gst::parse::launch(&format!(
            "audiotestsrc num-buffers=900 freq=440 volume=0.5 ! audioconvert ! flacenc ! filesink location={}",
            path.display()
        ))
        .unwrap();
        generator.set_state(gst::State::Playing).unwrap();
        generator
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Eos]);
        generator.set_state(gst::State::Null).unwrap();
        let video = gst::ElementFactory::make("fakesink").build().unwrap();
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&video);
        let play = gst_play::Play::new(Some(renderer));
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .property("signal-handoffs", true)
            .build()
            .unwrap();
        let played = Arc::new(AtomicUsize::new(0));
        {
            let played = played.clone();
            sink.connect("handoff", false, move |_| {
                played.fetch_add(1, Ordering::Relaxed);
                None
            });
        }
        play.pipeline().set_property("audio-sink", &sink);
        let analyzer = Analyzer::attach(&play.pipeline()).unwrap();
        let analysed = Arc::new(AtomicUsize::new(0));
        {
            let analysed = analysed.clone();
            analyzer.spectrum.static_pad("sink").unwrap().add_probe(
                gst::PadProbeType::BUFFER,
                move |_, _| {
                    analysed.fetch_add(1, Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                },
            );
        }
        play.set_uri(Some(gtk::gio::File::for_path(&path).uri().as_str()));
        play.play();
        let plays_on = |within: u64| {
            let before = played.load(Ordering::Relaxed);
            let deadline = Instant::now() + std::time::Duration::from_secs(within);
            while played.load(Ordering::Relaxed) < before + 10 && Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            played.load(Ordering::Relaxed) >= before + 10
        };
        assert!(plays_on(5), "the music does not play");
        assert_eq!(analysed.load(Ordering::Relaxed), 0);
        assert!(!analyzer.rhythm_running());
        analyzer.enable(true);
        assert!(analyzer.rhythm_running());
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while analyzer.levels.lock().unwrap().rms[0] < 0.5 && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(analyzer.levels.lock().unwrap().rms[0] >= 0.5);
        assert!(plays_on(2), "turning the analysis on stopped the music");
        analyzer.enable(false);
        assert!(!analyzer.rhythm_running());
        assert!(plays_on(2), "turning the analysis off stopped the music");
        let after = analysed.load(Ordering::Relaxed);
        assert!(plays_on(2));
        assert_eq!(analysed.load(Ordering::Relaxed), after);
        play.stop();
        play.pipeline().set_state(gst::State::Null).unwrap();
    }

    /// GstPlay's error from an element at `path`.
    fn error_from(path: &str) -> String {
        format!("Error from element {path}: Internal data stream error.")
    }

    const PLAYSINK: &str = "/GstPlayBin3:playbin3/GstPlaySink:playsink";

    #[test]
    fn an_error_inside_the_vis_chain_is_the_visualizers() {
        for element in [
            "/GstBin:visbin/GstQueue:visqueue",
            "/GstBin:visbin/GstBin:bin0/GstWaveScope:effect",
        ] {
            assert!(caused(&error_from(&format!("{PLAYSINK}{element}")), false));
        }
    }

    #[test]
    fn an_error_from_the_audio_sink_is_not_the_visualizers() {
        let message = error_from(&format!("{PLAYSINK}/GstBin:abin/GstPulseSink:pulsesink0"));
        assert!(!caused(&message, true));
    }

    #[test]
    fn an_error_from_the_media_is_not_the_visualizers() {
        let message = error_from(
            "/GstPlayBin3:playbin3/GstURIDecodeBin3:uridecodebin3/GstDecodebin3:decodebin3-0",
        );
        assert!(!caused(&message, true));
    }

    #[test]
    fn the_video_sink_is_the_visualizers_only_for_music() {
        let message = error_from(&format!("{PLAYSINK}/GstBin:vbin/GstVideoConvert:vconv"));
        assert_eq!(
            (caused(&message, true), caused(&message, false)),
            (true, false)
        );
    }

    #[test]
    fn a_message_without_an_element_is_not_the_visualizers() {
        assert!(!caused("Error from element : visbin", true));
        assert!(!caused("the server went away", true));
    }

    /// Where playbin really puts a visualizer, and the audio sink.
    #[test]
    fn playbins_own_paths_are_told_apart() {
        gst::init().unwrap();
        let video = gst::ElementFactory::make("fakesink").build().unwrap();
        let renderer = gst_play::PlayVideoOverlayVideoRenderer::with_sink(&video);
        let play = gst_play::Play::new(Some(renderer));
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .build()
            .unwrap();
        play.pipeline().set_property("audio-sink", &sink);
        let effect = gst_visualizer("wavescope").unwrap();
        play.pipeline().set_property("vis-plugin", &effect);
        play.set_visualization_enabled(true);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.flac");
        let generator = gst::parse::launch(&format!(
            "audiotestsrc num-buffers=300 ! audioconvert ! flacenc ! filesink location={}",
            path.display()
        ))
        .unwrap();
        generator.set_state(gst::State::Playing).unwrap();
        generator
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Eos]);
        generator.set_state(gst::State::Null).unwrap();
        play.set_uri(Some(gtk::gio::File::for_path(&path).uri().as_str()));
        play.pause();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while effect.parent().is_none() && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let inner = effect.by_name("effect").unwrap();
        assert!(
            caused(&error_from(&inner.path_string()), false),
            "{}",
            inner.path_string()
        );
        assert!(
            !caused(&error_from(&sink.path_string()), true),
            "{}",
            sink.path_string()
        );
        play.stop();
        play.pipeline().set_state(gst::State::Null).unwrap();
    }

    #[test]
    #[ignore = "needs a GTK display; writes the visualizer contact sheet to /tmp"]
    fn render_visualizer_modes() {
        gtk::init().unwrap();
        let canvas = Canvas::new(Measurements::default());
        canvas.measure(gtk::Orientation::Horizontal, -1);
        canvas.measure(gtk::Orientation::Vertical, 600);
        canvas.allocate(600, 400, -1, None);
        let texture = gdk::MemoryTexture::new(
            2,
            2,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(vec![
                160u8, 60, 100, 255, 30, 120, 200, 255, 20, 50, 80, 255, 220, 150, 90, 255,
            ]),
            8,
        );
        let texture: gdk::Texture = texture.upcast();
        canvas.set_tint(Some(&texture));
        canvas.set_cover(Some(texture));
        let bands = std::array::from_fn(|i| ((i as f64 * 0.3).sin() * 0.35 + 0.5).max(0.));
        canvas.imp().frame.borrow_mut().bands = bands;
        canvas.imp().frame.borrow_mut().rms = [0.68, 0.82];
        *canvas.imp().peaks.borrow_mut() = bands.iter().map(|x| x + 0.04).collect();
        for i in 0..180 {
            canvas
                .imp()
                .waterfall
                .borrow_mut()
                .push(&std::array::from_fn(|j| {
                    ((i as f64 * 0.1 + j as f64 * 0.15).sin() * 0.4 + 0.5).max(0.)
                }));
        }
        let snapshot = gtk::Snapshot::new();
        for (i, mode) in [
            Mode::Radial,
            Mode::Vu,
            Mode::Ambient,
            Mode::Spectrum,
            Mode::Spectrogram,
            Mode::Rings,
        ]
        .into_iter()
        .enumerate()
        {
            canvas.set_mode(mode);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                (i % 2) as f32 * 600.,
                (i / 2) as f32 * 400.,
            ));
            canvas.imp().snapshot(&snapshot);
            snapshot.restore();
        }
        let renderer = gtk::gsk::CairoRenderer::new();
        renderer.realize(None::<&gdk::Surface>).unwrap();
        let texture = renderer.render_texture(snapshot.to_node().unwrap(), None);
        renderer.unrealize();
        texture
            .save_to_png("/tmp/spoolframe-visualizers.png")
            .unwrap();
    }

    #[test]
    fn only_audio_visualizers_are_offered_and_both_scopes_are_named() {
        gst::init().unwrap();
        let modes = plugins();
        assert!(!modes.iter().any(|(name, _)| name == "goom"));
        assert!(
            modes
                .iter()
                .all(|(name, _)| matches!(name.as_str(), "wavescope" | "spacescope"))
        );
        assert!(gst_visualizer("goom2k1").is_err());
        assert!(gst_visualizer("goom").is_err());
        assert!(
            modes
                .iter()
                .any(|(name, label)| name == "wavescope" && label == "Oscilloscope")
        );
        assert!(
            modes
                .iter()
                .any(|(name, label)| name == "spacescope" && label == "Vectorscope")
        );
        assert!(
            !modes
                .iter()
                .any(|(name, _)| name.contains("detection") || name.contains("segmentation"))
        );
        assert!(
            !modes.iter().any(|(name, _)| name == "monoscope"),
            "fixed 256×128 visualizers must not be offered as HD modes"
        );
    }

    #[test]
    fn stratum_tracks_click_spectra_and_rejects_silence() {
        let silent = vec![vec![0.; 256]; 360];
        assert!(analyze_rhythm(&silent, 1. / 30.).is_none());
        assert!(analyze_rhythm(&vec![vec![0.2; 256]; 360], 1. / 30.).is_none());
        for bpm in [90., 120., 150.] {
            let frames: Vec<_> = (0..360)
                .map(|i| {
                    let phase = (i as f64 / 30.).rem_euclid(60. / bpm);
                    let amplitude = if phase < 0.05 { 0.8 } else { 0. };
                    vec![amplitude; 256]
                })
                .collect();
            let start = Instant::now();
            let result = analyze_rhythm(&frames, 1. / 30.);
            eprintln!("{bpm} BPM -> {result:?}, {:?}", start.elapsed());
            let (detected, _) = result.expect("rhythm");
            assert!(
                (detected - bpm).abs() < 5.,
                "expected {bpm}, got {detected}"
            );
        }
    }

    #[test]
    fn stratum_accepts_gstreamer_spectra_of_pcm_clicks() {
        gst::init().unwrap();
        let pipeline = gst::parse::launch("appsrc name=input format=time caps=audio/x-raw,format=F32LE,layout=interleaved,rate=44100,channels=1 ! spectrum bands=8193 threshold=-60 interval=33333333 ! fakesink sync=false")
            .unwrap().downcast::<gst::Pipeline>().unwrap();
        let input = pipeline.by_name("input").unwrap();
        let samples: Vec<u8> = (0..44100 * 12)
            .flat_map(|i| {
                let phase = (i % 22050) as f32 / 44100.;
                let value = if phase < 0.08 {
                    (phase * 600. * std::f32::consts::TAU).sin() * (-phase * 60.).exp()
                } else {
                    0.
                };
                value.to_le_bytes()
            })
            .collect();
        pipeline.set_state(gst::State::Playing).unwrap();
        let _ = input.emit_by_name::<gst::FlowReturn>(
            "push-buffer",
            &[&gst::Buffer::from_mut_slice(samples)],
        );
        let _ = input.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
        let (sender, receiver) = std::sync::mpsc::sync_channel(10);
        let mut collector = RhythmInput::new(sender);
        let mut power = Vec::new();
        let bus = pipeline.bus().unwrap();
        loop {
            let message = bus
                .timed_pop(gst::ClockTime::from_seconds(10))
                .expect("pipeline timeout");
            match message.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(e) => panic!("{}", e.error()),
                gst::MessageView::Element(_) => {
                    let structure = message.structure().unwrap();
                    if let Ok(magnitudes) = structure.get::<gst::List>("magnitude") {
                        power_spectrum(
                            magnitudes.iter().map(|v| v.get::<f32>().unwrap() as f64),
                            &mut power,
                        );
                        collector.push(
                            &power,
                            structure.get("timestamp").unwrap(),
                            structure.get("duration").unwrap(),
                            Stamp(1),
                            Instant::now(),
                        );
                    }
                }
                _ => (),
            }
        }
        pipeline.set_state(gst::State::Null).unwrap();
        let window = receiver.try_iter().last().expect("analysis window");
        let result = analyze_rhythm(&window.split(), window.interval);
        eprintln!("PCM clicks through GStreamer: {result:?}");
        let (bpm, _) = result.expect("beat grid");
        assert!((bpm - 120.).abs() < 5.);
    }

    #[test]
    fn frequency_bands_are_sample_rate_aware_without_resolution_gaps() {
        for rate in [44100., 48000., 96000.] {
            for hz in [60., 1000., 12000.] {
                let mut raw = vec![f64::NEG_INFINITY; 8192];
                let bin = (hz * 2. * raw.len() as f64 / rate).round() as usize;
                raw[bin] = 0.;
                let bands = logarithmic_bands(&power_of(&raw), rate);
                let active: Vec<_> = bands.iter().enumerate().filter(|(_, v)| **v > 0.).collect();
                assert!(!active.is_empty());
                assert!(active.len() <= 3, "one tone should remain localized");
                let expected = ((hz / 20.).ln() / 1000f64.ln() * BANDS as f64) as usize;
                let peak = active.iter().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
                assert!(peak.abs_diff(expected) <= 1);
            }
        }
        assert_eq!(logarithmic_bands(&[], 44100.), [0.; BANDS]);
        let mut dc = vec![f64::NEG_INFINITY; 8192];
        dc[0] = 0.;
        assert_eq!(logarithmic_bands(&power_of(&dc), 44100.), [0.; BANDS]);
        for rate in [44100., 48000., 96000., 192000.] {
            let flat = logarithmic_bands(&power_of(&vec![-30.; 8193]), rate);
            assert!(
                flat.iter().all(|v| (*v - 0.5).abs() < 1e-9),
                "blank row at {rate} Hz: {flat:?}"
            );
        }
    }

    fn power_of(levels: &[f64]) -> Vec<f64> {
        let mut power = Vec::new();
        power_spectrum(levels.iter().copied(), &mut power);
        power
    }

    /// The rhythm gets the last twelve seconds of frames, oldest first,
    /// in one piece, and starts again on a spectrum of another size.
    #[test]
    fn the_rhythm_window_is_the_last_frames_in_order() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(100);
        let mut input = RhythmInput::new(sender);
        let frame = 33_333_333;
        let push = |input: &mut RhythmInput, n: u64, size: usize| {
            let power = vec![(n as f64).powi(2); size];
            input.push(&power, n * frame, frame, Stamp(1), Instant::now());
        };
        for n in 0..420 {
            push(&mut input, n, 512);
        }
        let window = receiver.try_iter().last().expect("a window");
        let frames = window.split();
        assert_eq!(
            (
                frames.len(),
                frames[0].len(),
                frames[0][0],
                frames[359][255]
            ),
            (RHYTHM_FRAMES, 256, 60., 419.)
        );
        push(&mut input, 420, 8193);
        assert_eq!((input.width, input.frames.len()), (249, 249));
    }

    /// Paused, the canvas draws until the bars have fallen, then stops.
    #[test]
    fn a_paused_canvas_settles_once_its_bars_have_fallen() {
        let mut frame = Levels {
            bands: [1.; BANDS],
            rms: [1.; 2],
            ..Levels::default()
        };
        let mut frames = 0;
        while !settled(&frame, 0.) {
            for out in frame.bands.iter_mut().chain(&mut frame.rms) {
                smooth(out, 0., 1. / 60.);
            }
            frames += 1;
            assert!(frames < 10 * 60, "never settles");
        }
        assert!(frames > 1, "settled while still lit");
        assert!(
            !settled(&Levels::default(), 0.2),
            "a beat's punch still moves"
        );
    }

    /// The spectrogram's colours from the table are those worked out
    /// for each pixel, to a few shades in the darkest quiet.
    #[test]
    fn the_spectrograms_shades_are_its_colours() {
        let exact = |value: f64| {
            let (r, g, b) = spectrogram_color(value);
            [r, g, b].map(|c| (c * 255.) as u8 as i32)
        };
        for i in 0..=10_000 {
            let value = i as f64 / 10_000.;
            let shade = spectrogram_rgb(value).map(i32::from);
            let off = shade
                .iter()
                .zip(exact(value))
                .map(|(a, b)| (a - b).abs())
                .max()
                .unwrap();
            let allowed = if value < 0.01 { 5 } else { 1 };
            assert!(off <= allowed, "{value}: {shade:?} for {:?}", exact(value));
        }
        assert_eq!(spectrogram_rgb(f64::NAN), spectrogram_rgb(0.));
        assert_eq!(spectrogram_rgb(2.), spectrogram_rgb(1.));
    }

    /// The colour of each column of the spectrogram's top row.
    fn top_row(waterfall: &Waterfall) -> Vec<[u8; 3]> {
        waterfall.pixels[..WATERFALL_COLUMNS * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| [p[0], p[1], p[2]])
            .collect()
    }

    #[test]
    fn a_new_column_comes_in_on_the_right_and_the_rest_move_left() {
        let mut waterfall = Waterfall::default();
        let silence = spectrogram_rgb(0.);
        let (mut loud, mut soft) = ([0.; BANDS], [0.; BANDS]);
        loud[BANDS - 1] = 1.;
        soft[BANDS - 1] = 0.5;
        waterfall.push(&loud);
        waterfall.push(&soft);
        let row = top_row(&waterfall);
        let last = WATERFALL_COLUMNS - 1;
        assert_eq!(
            (row[last], row[last - 1], row[last - 2], row[last - 3]),
            (
                spectrogram_rgb(0.5),
                spectrogram_rgb(0.5),
                spectrogram_rgb(1.),
                silence
            )
        );
        // The lowest band is the bottom row.
        let bottom = &waterfall.pixels[(BANDS - 1) * WATERFALL_COLUMNS * 4..];
        assert_eq!(bottom[(last) * 4..last * 4 + 3], silence);
    }

    #[test]
    fn a_reset_spectrogram_is_silent_and_scrolls_from_its_next_frame() {
        let mut waterfall = Waterfall::default();
        waterfall.advance(10., &[1.; BANDS]);
        waterfall.advance(10.05, &[1.; BANDS]);
        assert_eq!(top_row(&waterfall).last(), Some(&spectrogram_rgb(1.)));
        assert!(waterfall.offset > 0.);
        waterfall.reset();
        assert!(
            top_row(&waterfall)
                .iter()
                .all(|c| *c == spectrogram_rgb(0.))
        );
        assert_eq!((waterfall.time, waterfall.offset), (0., 0.));
    }

    /// A stall of seconds fills the spectrogram once over, no more.
    #[test]
    fn a_long_gap_adds_no_more_columns_than_there_are() {
        let mut waterfall = Waterfall::default();
        waterfall.advance(1., &[0.; BANDS]);
        waterfall.advance(100., &[1.; BANDS]);
        assert!(
            top_row(&waterfall)
                .iter()
                .all(|c| *c == spectrogram_rgb(1.))
        );
    }

    #[test]
    fn each_reset_starts_a_new_epoch_and_turning_keeps_whether_it_is_on() {
        let epoch = Epoch::default();
        let off = epoch.now();
        epoch.turn(true);
        let on = epoch.now();
        epoch.bump();
        let reset = epoch.now();
        epoch.turn(false);
        assert_eq!(
            [off.is_on(), on.is_on(), reset.is_on(), epoch.now().is_on()],
            [false, true, true, false]
        );
        assert!(off != on && on != reset && reset != epoch.now());
    }

    #[test]
    fn levels_handle_silence_clipping_and_non_finite_input() {
        assert_eq!(normalized(f64::NEG_INFINITY), 0.);
        assert_eq!(normalized(f64::NAN), 0.);
        assert_eq!(normalized(-90.), 0.);
        assert_eq!(normalized(6.), 1.);
        assert_eq!(normalized(-30.), 0.5);
        let mut level = 1.;
        for _ in 0..60 {
            smooth(&mut level, 0., 1. / 60.);
        }
        assert!(level < 0.001);
    }
}
