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

pub const BANDS: usize = 64;
#[derive(Clone)]
pub struct Levels {
    pub bands: [f64; BANDS],
    pub rms: [f64; 2],
    pub updated: Instant,
    pub clock: Option<BeatClock>,
    pub bpm: Option<f64>,
}
impl Default for Levels {
    fn default() -> Self {
        Self {
            bands: [0.; BANDS],
            rms: [0.; 2],
            updated: Instant::now(),
            clock: None,
            bpm: None,
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

struct RhythmWindow {
    frames: Vec<Vec<f32>>,
    interval: f64,
    observed: Instant,
    generation: u64,
}
struct RhythmInput {
    frames: std::collections::VecDeque<Vec<f32>>,
    count: usize,
    generation: u64,
    last_time: Option<u64>,
    interval: f64,
    sender: std::sync::mpsc::SyncSender<RhythmWindow>,
}
impl RhythmInput {
    fn push(
        &mut self,
        raw: &[f64],
        timestamp: u64,
        duration: u64,
        generation: u64,
        observed: Instant,
    ) {
        let interval = duration as f64 / 1e9;
        if duration == 0 || generation & 1 == 0 {
            return;
        }
        if generation != self.generation
            || (interval - self.interval).abs() > 0.001
            || self.last_time.is_some_and(|last| {
                timestamp.abs_diff(last.saturating_add(duration)) > duration / 2
            })
        {
            self.frames.clear();
            self.count = 0;
        }
        self.generation = generation;
        self.last_time = Some(timestamp);
        self.interval = interval;
        // Pool linear magnitudes for rhythm analysis; spectrum rendering retains
        // the full FFT. This bounds window memory and background CPU use.
        let frame = raw
            .chunks(raw.len().div_ceil(256).max(1))
            .map(|chunk| {
                (chunk
                    .iter()
                    .map(|db| {
                        if db.is_finite() && *db > -60. {
                            10f64.powf(db.min(0.) / 10.)
                        } else {
                            0.
                        }
                    })
                    .sum::<f64>()
                    / chunk.len() as f64)
                    .sqrt() as f32
            })
            .collect();
        self.frames.push_back(frame);
        while self.frames.len() > 360 {
            self.frames.pop_front();
        }
        self.count += 1;
        if self.frames.len() >= 180 && self.count.is_multiple_of(60) {
            let _ = self.sender.try_send(RhythmWindow {
                frames: self.frames.iter().cloned().collect(),
                interval,
                observed,
                generation,
            });
        }
    }
}
fn rhythm_worker(output: Measurements, epoch: Arc<std::sync::atomic::AtomicU64>) -> RhythmInput {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<RhythmWindow>(1);
    std::thread::spawn(move || {
        use std::sync::atomic::Ordering;
        while let Ok(window) = receiver.recv() {
            if epoch.load(Ordering::Acquire) != window.generation {
                continue;
            }
            let result = analyze_rhythm(&window.frames, window.interval);
            let mut levels = output.lock().unwrap();
            if epoch.load(Ordering::Acquire) != window.generation {
                continue;
            }
            levels.clock = result.and_then(|(bpm, last)| {
                let age = (window.frames.len() - 1) as f64 * window.interval - last;
                if age < 0. || age > window.frames.len() as f64 * window.interval {
                    return None;
                }
                Some(BeatClock {
                    anchor: window.observed
                        - std::time::Duration::from_secs_f64(age.rem_euclid(60. / bpm)),
                    period: 60. / bpm,
                    updated: window.observed,
                })
            });
            levels.bpm = result.map(|(bpm, _)| bpm);
        }
    });
    RhythmInput {
        frames: Default::default(),
        count: 0,
        generation: 0,
        last_time: None,
        interval: 0.,
        sender,
    }
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

pub struct Analyzer {
    pub levels: Measurements,
    spectrum: gst::Element,
    level: gst::Element,
    beats: Arc<std::sync::atomic::AtomicU64>,
}
impl Analyzer {
    pub fn attach(playbin: &gst::Element) -> anyhow::Result<Self> {
        let filter = gst::parse::bin_from_description(
            "audioconvert ! spectrum name=visual-spectrum bands=8193 threshold=-60 interval=33333333 post-messages=false ! level name=visual-level interval=33333333 post-messages=false",
            true,
        )?;
        let spectrum = filter.by_name("visual-spectrum").unwrap();
        let level = filter.by_name("visual-level").unwrap();
        let levels = Measurements::default();
        let output = levels.clone();
        let beats = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let rhythm = Mutex::new(rhythm_worker(levels.clone(), beats.clone()));
        let epoch = beats.clone();
        let reset_output = levels.clone();
        spectrum.static_pad("sink").unwrap().add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                if info.event().is_some_and(|event| {
                    matches!(
                        event.view(),
                        gst::EventView::Segment(_)
                            | gst::EventView::StreamStart(_)
                            | gst::EventView::Caps(_)
                    )
                }) {
                    epoch.fetch_add(2, std::sync::atomic::Ordering::AcqRel);
                    *reset_output.lock().unwrap() = Levels::default();
                }
                gst::PadProbeReturn::Ok
            },
        );
        let epoch = beats.clone();
        let bus = playbin.bus().unwrap();
        let spectrum_source = spectrum.clone();
        let level_source = level.clone();
        bus.enable_sync_message_emission();
        bus.connect_sync_message(Some("element"), move |_, message| {
            let Some(s) = message.structure() else {
                return;
            };
            let mut values = output.lock().unwrap();
            if message.src() == Some(spectrum_source.upcast_ref()) {
                if let Ok(bands) = s.get::<gst::List>("magnitude") {
                    let raw: Vec<f64> = bands
                        .iter()
                        .map(|v| v.get::<f32>().unwrap_or(-60.) as f64)
                        .collect();
                    values.updated = Instant::now();
                    let rate = spectrum_source
                        .static_pad("sink")
                        .and_then(|p| p.current_caps())
                        .and_then(|caps| caps.structure(0)?.get::<i32>("rate").ok())
                        .unwrap_or(44100) as f64;
                    values.bands = logarithmic_bands(&raw, rate);
                    if let (Ok(timestamp), Ok(duration)) =
                        (s.get::<u64>("timestamp"), s.get::<u64>("duration"))
                    {
                        rhythm.lock().unwrap().push(
                            &raw,
                            timestamp,
                            duration,
                            epoch.load(std::sync::atomic::Ordering::Acquire),
                            spectrum_time(&spectrum_source, s, duration),
                        );
                    }
                }
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
                values.rms = [normalized(first), normalized(second)];
                values.updated = Instant::now();
            }
        });
        playbin.set_property("audio-filter", &filter);
        Ok(Self {
            levels,
            spectrum,
            level,
            beats,
        })
    }
    pub fn enable(&self, enabled: bool) {
        use std::sync::atomic::Ordering;
        let _ = self
            .beats
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some(((old + 2) & !1) | u64::from(enabled))
            });
        self.spectrum.set_property("post-messages", enabled);
        self.level.set_property("post-messages", enabled);
        if !enabled {
            *self.levels.lock().unwrap() = Levels::default();
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Mode {
    #[default]
    Radial,
    Vu,
    Ambient,
    Spectrum,
    Spectrogram,
    Rings,
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
        pub history: RefCell<std::collections::VecDeque<[f64; BANDS]>>,
        pub history_time: Cell<f64>,
        pub history_offset: Cell<f64>,
        pub history_texture: RefCell<Option<gdk::MemoryTexture>>,
        pub color: Cell<(f64, f64, f64)>,
        pub punch: Cell<f64>,
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
            let bass = values.bands[..16].iter().sum::<f64>() / 16.;
            let energy = (values.rms[0] + values.rms[1]) / 2.;
            let size = w.min(h) * 0.38;
            let texture = self.texture.borrow();
            if self.mode.get() == Mode::Ambient
                && let Some(texture) = texture.as_ref()
            {
                let drift = self.phase.get();
                let zoom = 1.25 + bass as f32 * 0.16;
                let width = w * zoom;
                let height = h * zoom;
                snapshot.push_clip(&bounds);
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
                    &bounds,
                );
            }
            if matches!(self.mode.get(), Mode::Radial | Mode::Ambient | Mode::Rings) {
                let rect = graphene::Rect::new((w - size) / 2., (h - size) / 2., size, size);
                snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(rect, 18.));
                if let Some(texture) = texture.as_ref() {
                    snapshot.append_texture(texture, &rect);
                } else {
                    snapshot.append_color(&gdk::RGBA::new(0.18, 0.22, 0.32, 1.), &rect);
                }
                snapshot.pop();
            }
            if self.mode.get() == Mode::Spectrogram {
                if self.history_texture.borrow().is_none() {
                    *self.history_texture.borrow_mut() =
                        Some(waterfall_texture(&self.history.borrow()));
                }
                let step = w / 180.;
                snapshot.push_clip(&bounds);
                snapshot.append_texture(
                    self.history_texture.borrow().as_ref().unwrap(),
                    &graphene::Rect::new(
                        -step * (1. + self.history_offset.get() as f32),
                        0.,
                        step * 182.,
                        h,
                    ),
                );
                snapshot.pop();
                return;
            }
            let cr = snapshot.append_cairo(&bounds);
            if self.mode.get() == Mode::Radial {
                let radius = size as f64 * 0.76;
                cr.set_line_width((w.min(h) as f64 * 0.006).max(2.));
                cr.set_line_cap(gtk::cairo::LineCap::Round);
                for (i, value) in values.bands.iter().enumerate() {
                    let angle = i as f64 / BANDS as f64 * std::f64::consts::TAU
                        - std::f64::consts::FRAC_PI_2;
                    let length = 4. + value * w.min(h) as f64 * 0.14;
                    let (r, g, b) = band_color(self.color.get(), i as f64 / BANDS as f64, *value);
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
            } else if self.mode.get() == Mode::Rings {
                let unit = w.min(h) as f64;
                let phase = self.phase.get();
                let punch = self.punch.get();
                cr.set_line_width((unit * 0.0035).max(1.5));
                for ring in 0..5 {
                    let first = ring * 12;
                    let energy = values.bands[first..first + 12].iter().sum::<f64>() / 12.;
                    let radius = size as f64 * 0.76
                        + ring as f64 * unit * 0.025
                        + punch * unit * 0.008 * (1. + ring as f64 * 0.12);
                    let (r, g, b) = band_color(self.color.get(), ring as f64 / 5., energy);
                    cr.set_source_rgba(r, g, b, 0.65 + energy * 0.35);
                    for point in 0..=256 {
                        let angle = point as f64 / 256. * std::f64::consts::TAU;
                        let rotation = phase
                            * (1. + ring as f64 * 0.15)
                            * if ring % 2 == 0 { 1. } else { -1. };
                        let rotated = angle + rotation;
                        let wave = (rotated * (3. + ring as f64 * 2.)).sin()
                            + 0.3 * (rotated * (7. + ring as f64)).sin();
                        let distance =
                            radius + wave * unit * (0.012 + energy * 0.004 + punch * 0.006);
                        let x = w as f64 / 2. + angle.cos() * distance;
                        let y = h as f64 / 2. + angle.sin() * distance;
                        if point == 0 {
                            cr.move_to(x, y);
                        } else {
                            cr.line_to(x, y);
                        }
                    }
                    cr.close_path();
                    let _ = cr.stroke();
                }
            } else if self.mode.get() == Mode::Spectrum {
                let margin = (w as f64 * 0.06).max(16.);
                let bottom = h as f64 * 0.78;
                let height = h as f64 * 0.55;
                let step = (w as f64 - 2. * margin) / BANDS as f64;
                let (r, g, b) = self.color.get();
                let peaks = self.peaks.borrow();
                for (i, value) in values.bands.iter().enumerate() {
                    let (r, g, b) = band_color((r, g, b), i as f64 / BANDS as f64, *value);
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
                    cr.rectangle(
                        margin + i as f64 * step,
                        bottom - value * height,
                        (step - 3.).max(1.),
                        (value * height).max(1.),
                    );
                    let _ = cr.fill();
                    if self.show_peaks.get()
                        && let Some(peak) = peaks.get(i)
                    {
                        cr.set_source_rgba(0.95, 0.97, 1., 0.8);
                        cr.rectangle(
                            margin + i as f64 * step,
                            bottom - peak * height - 3.,
                            (step - 3.).max(1.),
                            2.,
                        );
                        let _ = cr.fill();
                    }
                }
            } else if self.mode.get() == Mode::Vu {
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
                for (i, label) in ["−60", "−40", "−20", "0 dBFS"].iter().enumerate() {
                    let label_width = cr.text_extents(label).map(|e| e.width()).unwrap_or(0.);
                    cr.move_to(
                        x + 32. + i as f64 * (width - 32. - label_width) / 3.,
                        y + 115.,
                    );
                    let _ = cr.show_text(label);
                }
            }
        }
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
        obj.imp().color.set((0.4, 0.65, 0.95));
        obj.imp().show_peaks.set(true);
        obj.set_hexpand(true);
        obj.set_vexpand(true);
        obj.add_tick_callback(|obj, clock| {
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
                let previous = imp.history_time.get();
                let elapsed = if previous == 0. {
                    1. / 30.
                } else {
                    (now - previous).max(0.)
                };
                let steps = (elapsed * 30.).floor() as usize;
                if steps > 0 {
                    // Preserve the fractional remainder; snapping to `now` drifts
                    // against the display clock and produces uneven scrolling.
                    imp.history_time.set(now - (elapsed * 30.).fract() / 30.);
                    let mut history = imp.history.borrow_mut();
                    for _ in 0..steps.min(181) {
                        history.push_back(frame.bands);
                        if history.len() > 181 {
                            history.pop_front();
                        }
                    }
                    imp.history_texture.borrow_mut().take();
                }
                imp.history_offset.set((elapsed * 30.).fract());
            }
            obj.queue_draw();
            glib::ControlFlow::Continue
        });
        obj
    }
    pub fn set_mode(&self, mode: Mode) {
        self.imp().mode.set(mode);
        self.imp().history_time.set(0.);
        self.imp().history_offset.set(0.);
        self.queue_draw();
    }
    pub fn set_peaks(&self, enabled: bool) {
        self.imp().show_peaks.set(enabled);
    }
    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        self.imp()
            .color
            .set(texture.as_ref().map(art_color).unwrap_or((0.4, 0.65, 0.95)));
        self.imp().history.borrow_mut().clear();
        self.imp().history_texture.borrow_mut().take();
        self.imp().history_time.set(0.);
        self.imp().history_offset.set(0.);
        *self.imp().texture.borrow_mut() = texture;
        self.queue_draw();
    }
}
/// Integrate linearly interpolated FFT power over each logarithmic band.
/// This includes partial bins, so bands narrower than the FFT resolution do
/// not become artificial black stripes. It does not invent extra resolution.
fn logarithmic_bands(raw: &[f64], rate: f64) -> [f64; BANDS] {
    if raw.len() < 2 || !rate.is_finite() || rate <= 40. {
        return [0.; BANDS];
    }
    let bin_hz = rate / (2. * (raw.len() - 1) as f64);
    let power: Vec<_> = raw
        .iter()
        .enumerate()
        .map(|(i, db)| {
            if i > 0 && db.is_finite() && *db > -60. {
                10f64.powf(db.min(0.) / 10.)
            } else {
                0.
            }
        })
        .collect();
    std::array::from_fn(|i| {
        let low = 20. * 1000f64.powf(i as f64 / BANDS as f64);
        let high = (20. * 1000f64.powf((i + 1) as f64 / BANDS as f64)).min(rate / 2.);
        if high <= low {
            return 0.;
        }
        let start = low / bin_hz;
        let end = high / bin_hz;
        let mut integral = 0.;
        for bin in start.floor() as usize..(end.ceil() as usize).min(raw.len() - 1) {
            let left = (start - bin as f64).max(0.);
            let right = (end - bin as f64).min(1.);
            let slope = power[bin + 1] - power[bin];
            integral += (right - left) * (power[bin] + slope * (left + right) / 2.);
        }
        let average = integral / (end - start);
        if average > 0. {
            normalized(10. * average.log10())
        } else {
            0.
        }
    })
}

/// One small texture per analysis update, instead of thousands of Cairo draws
/// per display frame. Extra columns permit seamless fractional scrolling.
fn waterfall_texture(history: &std::collections::VecDeque<[f64; BANDS]>) -> gdk::MemoryTexture {
    let mut pixels = Vec::with_capacity(182 * BANDS * 4);
    for band in (0..BANDS).rev() {
        for column in 0..182usize {
            let index = column
                .min(180)
                .checked_sub(181usize.saturating_sub(history.len()));
            let value = index
                .and_then(|i| history.get(i))
                .map(|frame| frame[band])
                .unwrap_or(0.);
            let (r, g, b) = spectrogram_color(value);
            pixels.extend([(r * 255.) as u8, (g * 255.) as u8, (b * 255.) as u8, 255]);
        }
    }
    gdk::MemoryTexture::new(
        182,
        BANDS as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(pixels),
        182 * 4,
    )
}
fn art_color(texture: &gdk::Texture) -> (f64, f64, f64) {
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
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
        return (0.4, 0.65, 0.95);
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
        canvas.set_texture(Some(texture.upcast()));
        let bands = std::array::from_fn(|i| ((i as f64 * 0.3).sin() * 0.35 + 0.5).max(0.));
        canvas.imp().frame.borrow_mut().bands = bands;
        canvas.imp().frame.borrow_mut().rms = [0.68, 0.82];
        *canvas.imp().peaks.borrow_mut() = bands.iter().map(|x| x + 0.04).collect();
        for i in 0..180 {
            canvas
                .imp()
                .history
                .borrow_mut()
                .push_back(std::array::from_fn(|j| {
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
        let mut collector = RhythmInput {
            frames: Default::default(),
            count: 0,
            generation: 0,
            last_time: None,
            interval: 0.,
            sender,
        };
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
                        let raw: Vec<f64> = magnitudes
                            .iter()
                            .map(|v| v.get::<f32>().unwrap() as f64)
                            .collect();
                        collector.push(
                            &raw,
                            structure.get("timestamp").unwrap(),
                            structure.get("duration").unwrap(),
                            1,
                            Instant::now(),
                        );
                    }
                }
                _ => (),
            }
        }
        pipeline.set_state(gst::State::Null).unwrap();
        let window = receiver.try_iter().last().expect("analysis window");
        let result = analyze_rhythm(&window.frames, window.interval);
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
                let bands = logarithmic_bands(&raw, rate);
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
        assert_eq!(logarithmic_bands(&dc, 44100.), [0.; BANDS]);
        for rate in [44100., 48000., 96000., 192000.] {
            let flat = logarithmic_bands(&vec![-30.; 8193], rate);
            assert!(
                flat.iter().all(|v| (*v - 0.5).abs() < 1e-9),
                "blank row at {rate} Hz: {flat:?}"
            );
        }
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
