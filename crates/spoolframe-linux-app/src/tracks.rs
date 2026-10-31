//! Picking the audio and subtitle streams GstPlay plays, by their place in
//! the file's stream lists.
//!
//! GstPlay refuses to select the stream already selected ("stream
//! selection did not change") and reports it as a failure. GstPlay selects
//! the first subtitle stream by default with subtitles turned off, so
//! choosing that subtitle failed, and the subtitles were never turned on.
//! A stream already selected is therefore not selected again. Streams are
//! chosen by stream ID; the index calls are deprecated since 1.26.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gst::glib;
use gst_play::prelude::*;
use gtk::prelude::*;
use spoolframe_linux_core::format;
use spoolframe_protocol::{PlaybackPlan, SubtitleAction, SubtitleDelivery};
use spoolframe_types::MediaTracks;

use crate::external_subtitle::ExternalSubtitle;

/// Plays the `position`th audio stream of `info`.
pub fn select_audio(
    play: &gst_play::Play,
    info: &gst_play::PlayMediaInfo,
    position: usize,
) -> Result<(), glib::BoolError> {
    let stream = resolve(
        &info.audio_streams(),
        &now(play, info).audio_streams(),
        position,
    )
    .ok_or_else(|| glib::bool_error!("no audio stream {position}"))?;
    let id = PlayStreamInfoExt::stream_id(&stream);
    if play
        .current_audio_track()
        .map(|t| PlayStreamInfoExt::stream_id(&t))
        == Some(id.clone())
    {
        return Ok(());
    }
    play.set_audio_track_id(Some(&id))
}

/// What GstPlay plays now, else `info`. A pick is a position, and the
/// position is looked up here: every load is a new URI to GstPlay, so new
/// stream ids, and a list kept from before (the menu's, the first media
/// info's) names streams that are gone, which GstPlay refuses ("Failed to
/// set subtitle track", Vigil S2E05).
fn now(play: &gst_play::Play, info: &gst_play::PlayMediaInfo) -> gst_play::PlayMediaInfo {
    play.media_info().unwrap_or_else(|| info.clone())
}

/// The stream of `now` that the `position`th of `picked_from` is: the
/// same stream id, where GstPlay still has it, else the same position.
/// By id first: a subtitle file joining the media is a new stream
/// collection, whose order need not be the one the menu showed, and a
/// position alone then picked another subtitle than the one named
/// (Banshee S3E9).
fn resolve<T: IsA<gst_play::PlayStreamInfo>>(
    picked_from: &[T],
    now: &[T],
    position: usize,
) -> Option<T> {
    let id = picked_from
        .get(position)
        .map(|s| PlayStreamInfoExt::stream_id(s));
    id.and_then(|id| {
        now.iter()
            .find(|s| PlayStreamInfoExt::stream_id(*s) == id)
            .cloned()
    })
    .or_else(|| now.get(position).cloned())
}

/// Every stream `info` lists, by id: the menu is built again when this
/// changes, so its names and its picks are of the same streams.
pub fn stream_ids(info: &gst_play::PlayMediaInfo) -> Vec<String> {
    let audio = info.audio_streams();
    let subtitles = info.subtitle_streams();
    audio
        .iter()
        .map(|s| PlayStreamInfoExt::stream_id(s).to_string())
        .chain(
            subtitles
                .iter()
                .map(|s| PlayStreamInfoExt::stream_id(s).to_string()),
        )
        .collect()
}

/// The item's subtitle track each of the player's subtitle `streams` is,
/// in their order, as `plan` sends them; `None` for a stream that is none
/// of them. A direct play's file holds the item's embedded tracks in
/// their order, and the external file, whose stream is `external`, is
/// the plan's subtitle. A converted output holds one subtitle stream, the
/// one it was planned with, unless it is burned in or read as a file.
pub fn stream_tracks(
    tracks: &MediaTracks,
    plan: &PlaybackPlan,
    streams: &[String],
    external: Option<&str>,
) -> Vec<Option<u32>> {
    let (planned, mut own): (Option<u32>, Box<dyn Iterator<Item = u32>>) = match plan {
        PlaybackPlan::Direct { subtitle, .. } => (
            subtitle
                .filter(|s| s.delivery == SubtitleDelivery::External)
                .map(|s| s.index),
            Box::new(
                (0..tracks.subtitles.len() as u32)
                    .filter(|&n| !tracks.subtitles[n as usize].external),
            ),
        ),
        PlaybackPlan::Converted {
            subtitle: Some(subtitle),
            ..
        } => match subtitle.action {
            SubtitleAction::Copy { .. } | SubtitleAction::Convert { .. } => {
                (None, Box::new(std::iter::once(subtitle.index)))
            }
            SubtitleAction::Ocr { .. } => (Some(subtitle.index), Box::new(std::iter::empty())),
            SubtitleAction::BurnIn { .. } | SubtitleAction::Unknown => {
                (None, Box::new(std::iter::empty()))
            }
        },
        _ => (None, Box::new(std::iter::empty())),
    };
    streams
        .iter()
        .map(|id| match Some(id.as_str()) == external {
            true => planned,
            false => own.next(),
        })
        .collect()
}

/// Shows the `position`th subtitle stream of `info`, or none.
/// `external` is told first, so an external subtitle picked away from is
/// not failed by it (see [`crate::external_subtitle`]).
pub fn select_subtitle(
    play: &gst_play::Play,
    info: &gst_play::PlayMediaInfo,
    position: Option<usize>,
    external: &ExternalSubtitle,
) -> Result<(), glib::BoolError> {
    let Some(position) = position else {
        if !external.picking(None) {
            play.set_subtitle_track_enabled(false);
        }
        return Ok(());
    };
    let stream = resolve(
        &info.subtitle_streams(),
        &now(play, info).subtitle_streams(),
        position,
    )
    .ok_or_else(|| glib::bool_error!("no subtitle stream {position}"))?;
    let id = PlayStreamInfoExt::stream_id(&stream);
    external.picking(Some(&id));
    // On before the track is set: with subtitles off, as after an encode
    // that had none, GstPlay refuses the track ("Failed to set subtitle
    // track"), and a subtitle chosen mid-play did not show.
    play.set_subtitle_track_enabled(true);
    if play
        .current_subtitle_track()
        .map(|t| PlayStreamInfoExt::stream_id(&t))
        != Some(id.clone())
    {
        play.set_subtitle_track_id(Some(&id))?;
    }
    // Its cues were dropped while it was not shown: they are read again
    // from where playback is.
    if external.picked(Some(&id))
        && now(play, info).is_seekable()
        && let Some(at) = play.position()
    {
        play.seek(at);
    }
    Ok(())
}

/// Subtitle lists longer than this get a search field.
pub const SEARCH_FROM: usize = 3;

/// The menu's width: its rows wrap within it.
const MENU_WIDTH: i32 = 320;

/// The player's audio and subtitle menu: lists to pick one from, with a
/// check mark on what plays. `subtitles_on` is shared with the player,
/// which also turns subtitles on and off (the item page's choice, an
/// external subtitle); GstPlay has no call that says whether they are on.
/// It holds GstPlay weakly: a menu outliving its playback must not hold
/// the last reference (see [`crate::pipeline`]).
/// The quality section (P6-06): what is chosen now, with the height the
/// server encodes to, and how to choose.
/// Opens Search for subtitles over the widget the menu hangs from.
pub type FindSubtitles = Rc<dyn Fn(&gtk::Widget)>;

/// What the subtitle section offers past the file's own streams, where
/// the server has it.
#[derive(Default)]
pub struct SubtitleExtras {
    /// Search for subtitles… (PX-02), under the subtitles, as the board
    /// LinuxPlayer has it; it opens over what the menu hangs from.
    pub find: Option<FindSubtitles>,
    /// The chosen external text subtitle's timing (PX-03), under it.
    pub timing: Option<crate::subtitle_timing::TimingMenu>,
    /// The server's tracks the stream has none of, after its own.
    pub elsewhere: Option<ServerSubtitles>,
    /// The item's track each subtitle stream of the menu's media info is,
    /// with the server's name for it, where known ([`stream_tracks`]); a
    /// stream without one is named by its language and format.
    pub streams: Vec<Option<(u32, String)>>,
    /// Told the item's subtitle track a pick from the menu shows: `None`
    /// for Off, or a stream that is no track of the item.
    pub picked: Option<Rc<dyn Fn(Option<u32>)>>,
}

/// The item's subtitle tracks that only the server has for this playback:
/// a downloaded one, or one a conversion did not carry. Picking one starts
/// the item again with it, as Search for subtitles does.
pub struct ServerSubtitles {
    /// Each track's place in the item's subtitles, and its name.
    pub tracks: Vec<(u32, String)>,
    pub pick: Rc<dyn Fn(u32)>,
}

pub struct QualityMenu {
    pub current: Rc<dyn Fn() -> (spoolframe_client_sdk::PlaybackQuality, Option<u32>)>,
    pub choose: Rc<dyn Fn(spoolframe_client_sdk::PlaybackQuality)>,
}

pub fn menu(
    play: &gst_play::Play,
    info: &gst_play::PlayMediaInfo,
    subtitles_on: &Rc<Cell<bool>>,
    failed: Rc<dyn Fn(&str)>,
    quality: Option<QualityMenu>,
    external: ExternalSubtitle,
    SubtitleExtras {
        find,
        timing,
        elsewhere,
        streams: stream_names,
        picked,
    }: SubtitleExtras,
) -> gtk::Popover {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .width_request(MENU_WIDTH)
        .build();

    let audio_names: Vec<String> = info
        .audio_streams()
        .iter()
        .map(|s| {
            stream_label(
                s.language().as_deref(),
                s.codec().as_deref(),
                Some(s.channels())
                    .filter(|c| *c > 0)
                    .map(|c| format::channels(c as u32)),
            )
        })
        .collect();
    let audio = PickList::new(&audio_names);
    column.append(&heading("Audio"));
    column.append(&audio.list);

    // Off, the streams, then the server's tracks the streams lack.
    let streams = info.subtitle_streams().len();
    let subtitle_names: Vec<String> = std::iter::once("Off".to_owned())
        .chain(
            // The server's name where the stream is one of its tracks: a
            // subtitle file has no language tag, and its format alone
            // ("SubRip") said nothing of which it was.
            info.subtitle_streams()
                .iter()
                .enumerate()
                .map(|(n, s)| match stream_names.get(n) {
                    Some(Some((_, name))) => name.clone(),
                    _ => stream_label(s.language().as_deref(), s.codec().as_deref(), None),
                }),
        )
        .chain(
            elsewhere
                .iter()
                .flat_map(|e| e.tracks.iter().map(|(_, name)| name.clone())),
        )
        .collect();
    let subtitles = PickList::new(&subtitle_names);
    let subtitles_heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = heading("Subtitles");
    title.set_hexpand(true);
    subtitles_heading.append(&title);
    column.append(&subtitles_heading);
    if subtitle_names.len() > SEARCH_FROM {
        // As designed: how many tracks match, beside the heading.
        let total = subtitle_names.len() - 1;
        let count = gtk::Label::builder()
            .label(count_label(total, total))
            .css_classes(["dim-label", "caption"])
            .build();
        subtitles_heading.append(&count);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search subtitles")
            .css_classes(["track-search"])
            .build();
        let names = Rc::new(subtitle_names.clone());
        let query = Rc::new(RefCell::new(String::new()));
        {
            let (names, query) = (names.clone(), query.clone());
            subtitles.list.set_filter_func(move |row| {
                let index = row.index() as usize;
                // "Off" always stays.
                index == 0 || matches(&names[index], &query.borrow())
            });
        }
        let list = subtitles.list.clone();
        search.connect_search_changed(move |entry| {
            let text = entry.text().to_string();
            // "Off" is not a track.
            let shown = names[1..].iter().filter(|n| matches(n, &text)).count();
            count.set_label(&count_label(shown, total));
            query.replace(text);
            list.invalidate_filter();
        });
        column.append(&search);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .child(&subtitles.list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(320)
        .propagate_natural_height(true)
        .build();
    column.append(&scroller);
    let find_button = find.map(|find| {
        let button = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name(crate::icons::SEARCH)
                    .label("Search for subtitles…")
                    .halign(gtk::Align::Start)
                    .build(),
            )
            .css_classes(["flat", "subtitle-find"])
            .build();
        column.append(&button);
        (button, find)
    });
    let timing = timing.map(|menu| {
        let (section, draw) = crate::subtitle_timing::section(menu);
        column.append(&section);
        draw
    });

    let weak = play.downgrade();
    {
        let (play, info, marks, failed) =
            (weak.clone(), info.clone(), audio.clone(), failed.clone());
        audio.list.connect_row_activated(move |_, row| {
            let Some(play) = play.upgrade() else { return };
            let position = row.index() as usize;
            match select_audio(&play, &info, position) {
                Ok(()) => marks.mark(Some(position)),
                Err(e) => {
                    tracing::warn!(error = %e, position, "cannot switch audio");
                    failed(crate::playback::AUDIO_FAILED);
                }
            }
        });
    }
    {
        let (play, info, marks, on, failed) = (
            weak.clone(),
            info.clone(),
            subtitles.clone(),
            subtitles_on.clone(),
            failed.clone(),
        );
        subtitles.list.connect_row_activated(move |list, row| {
            let Some(play) = play.upgrade() else { return };
            // Row 0 is "Off"; the streams follow, then the server's tracks.
            let position = (row.index() as usize).checked_sub(1);
            if let Some(elsewhere) = &elsewhere
                && let Some(&(track, _)) = position
                    .and_then(|p| p.checked_sub(streams))
                    .and_then(|n| elsewhere.tracks.get(n))
            {
                marks.mark(Some(row.index() as usize));
                // The item starts again, with a new menu.
                if let Some(popover) = list.ancestor(gtk::Popover::static_type()) {
                    popover.downcast_ref::<gtk::Popover>().unwrap().popdown();
                }
                (elsewhere.pick)(track);
                return;
            }
            match select_subtitle(&play, &info, position, &external) {
                Ok(()) => {
                    on.set(position.is_some());
                    marks.mark(Some(row.index() as usize));
                    if let Some(picked) = &picked {
                        picked(
                            position.and_then(|p| {
                                stream_names.get(p).cloned().flatten().map(|(n, _)| n)
                            }),
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, ?position, "cannot switch subtitles");
                    failed(crate::playback::SUBTITLES_FAILED);
                }
            }
        });
    }

    // Below Subtitles, as sf-design placed it: Auto, the bitrates, and
    // Original. Choosing plans the playback again.
    let quality = quality.map(|menu| {
        let choices = spoolframe_client_sdk::PlaybackQuality::choices();
        let names: Vec<String> = choices
            .iter()
            .map(|q| crate::quality::label(*q, None))
            .collect();
        let picks = PickList::new(&names);
        column.append(&heading("Quality"));
        column.append(&picks.list);
        let (marks, choose) = (picks.clone(), menu.choose.clone());
        picks.list.connect_row_activated(move |_, row| {
            let index = row.index() as usize;
            if let Some(quality) = choices.get(index) {
                marks.mark(Some(index));
                choose(*quality);
            }
        });
        (picks, menu.current)
    });

    // One width whatever it holds, its end at the button's: centred on
    // the button, it was as wide as its widest row (Timing shown or not,
    // the server's track names), and moved sideways between openings.
    let clamp = adw::Clamp::builder()
        .maximum_size(MENU_WIDTH)
        .tightening_threshold(MENU_WIDTH)
        .child(&column)
        .build();
    let popover = gtk::Popover::builder()
        .child(&clamp)
        .halign(gtk::Align::End)
        .build();
    if let Some((button, find)) = find_button {
        let popover = popover.downgrade();
        button.connect_clicked(move |_| {
            let Some(popover) = popover.upgrade() else {
                return;
            };
            popover.popdown();
            if let Some(anchor) = popover.parent() {
                find(&anchor);
            }
        });
    }
    // What plays can change without the menu (the item page's choice, an
    // external subtitle arriving), so the marks are read on each opening.
    let (play, info, on) = (weak, info.clone(), subtitles_on.clone());
    popover.connect_show(move |_| {
        let Some(play) = play.upgrade() else { return };
        audio.mark(current_audio(&play, &info));
        let subtitle = match on.get() {
            false => Some(0),
            true => current_subtitle(&play, &info).map(|p| p + 1),
        };
        subtitles.mark(subtitle);
        if let Some(draw) = &timing {
            draw();
        }
        if let Some((picks, current)) = &quality {
            let (chosen, height) = current();
            picks.rename(
                0,
                &crate::quality::label(spoolframe_client_sdk::PlaybackQuality::Auto, height),
            );
            picks.mark(Some(chosen.choice_index()));
        }
    });
    popover
}

/// Where the playing audio stream is in `info`.
fn current_audio(play: &gst_play::Play, info: &gst_play::PlayMediaInfo) -> Option<usize> {
    let id = PlayStreamInfoExt::stream_id(&play.current_audio_track()?);
    info.audio_streams()
        .iter()
        .position(|s| PlayStreamInfoExt::stream_id(s) == id)
}

/// Where the selected subtitle stream is in `info`.
fn current_subtitle(play: &gst_play::Play, info: &gst_play::PlayMediaInfo) -> Option<usize> {
    let id = PlayStreamInfoExt::stream_id(&play.current_subtitle_track()?);
    info.subtitle_streams()
        .iter()
        .position(|s| PlayStreamInfoExt::stream_id(s) == id)
}

/// Whether a track's label matches what was typed: every word, anywhere,
/// ignoring case.
pub fn matches(label: &str, query: &str) -> bool {
    let label = label.to_lowercase();
    query
        .split_whitespace()
        .all(|word| label.contains(&word.to_lowercase()))
}

/// A list where activating a row picks it; the picked row has a check
/// mark. Nothing can be unpicked, only another row picked.
#[derive(Clone)]
struct PickList {
    list: gtk::ListBox,
    checks: Rc<Vec<gtk::Image>>,
    names: Rc<Vec<gtk::Label>>,
}

impl PickList {
    fn new(names: &[String]) -> Self {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .css_classes(["pick-list"])
            .build();
        let mut checks = Vec::new();
        let mut labels = Vec::new();
        for name in names {
            let check = gtk::Image::builder()
                .icon_name(crate::icons::CHECK)
                .opacity(0.0)
                .build();
            let row = gtk::Box::builder()
                .spacing(10)
                .margin_top(8)
                .margin_bottom(8)
                .margin_start(10)
                .margin_end(10)
                .build();
            // As designed: the name, and the check at the right.
            let label = gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            row.append(&label);
            labels.push(label);
            check.add_css_class("accent");
            row.append(&check);
            list.append(&row);
            checks.push(check);
        }
        Self {
            list,
            checks: Rc::new(checks),
            names: Rc::new(labels),
        }
    }

    /// Names row `index` anew.
    fn rename(&self, index: usize, name: &str) {
        if let Some(label) = self.names.get(index) {
            label.set_label(name);
        }
    }

    /// Checks and fills the picked row.
    fn mark(&self, picked: Option<usize>) {
        for (i, check) in self.checks.iter().enumerate() {
            let on = Some(i) == picked;
            check.set_opacity(if on { 1.0 } else { 0.0 });
            if let Some(row) = self.list.row_at_index(i as i32) {
                if on {
                    row.add_css_class("picked");
                } else {
                    row.remove_css_class("picked");
                }
            }
        }
    }
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text.to_uppercase())
        .xalign(0.0)
        .margin_top(6)
        .css_classes(["caption-heading", "dim-label"])
        .build()
}

/// "3 of 41": the subtitle tracks a search shows, of all of them.
pub fn count_label(shown: usize, total: usize) -> String {
    format!("{shown} of {total}")
}

/// A stream's name in the menu: language, codec and channels, whichever
/// the file says.
pub fn stream_label(language: Option<&str>, codec: Option<&str>, extra: Option<String>) -> String {
    let parts: Vec<String> = [language.map(str::to_owned), codec.map(str::to_owned), extra]
        .into_iter()
        .flatten()
        .collect();
    if parts.is_empty() {
        "Unnamed track".to_owned()
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::testing::TestDir;
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    /// Two seconds of H.264 with two FLAC tracks and an SRT subtitle, in
    /// Matroska, as many real files carry several of each.
    fn clip(path: &Path) {
        let srt = path.with_extension("srt");
        std::fs::write(
            &srt,
            "1\n00:00:00,000 --> 00:00:01,500\nHello\n\n2\n00:00:01,500 --> 00:00:02,000\nAgain\n",
        )
        .unwrap();
        let pipeline = gst::parse::launch(&format!(
            "videotestsrc num-buffers=60 ! video/x-raw,width=320,height=240,framerate=30/1 \
             ! videoconvert ! x264enc ! h264parse ! matroskamux name=m ! filesink location={} \
             audiotestsrc num-buffers=90 ! audioconvert ! flacenc ! flacparse ! m. \
             audiotestsrc num-buffers=90 freq=880 ! audioconvert ! flacenc ! flacparse ! m. \
             filesrc location={} ! subparse ! m.",
            path.display(),
            srt.display()
        ))
        .unwrap();
        let bus = pipeline.bus().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let message = bus
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(30),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .expect("the clip is written in time");
        pipeline.set_state(gst::State::Null).unwrap();
        if let gst::MessageView::Error(e) = message.view() {
            panic!("{} ({:?})", e.error(), e.debug());
        }
    }

    /// GstPlay on `path` with fakesinks, paused once its streams are known.
    fn play(path: &Path) -> (gst_play::Play, gst_play::PlayMediaInfo) {
        let play = gst_play::Play::new(None::<gst_play::PlayVideoRenderer>);
        let pipeline = play.pipeline();
        for sink in ["video-sink", "audio-sink"] {
            pipeline.set_property(sink, gst::ElementFactory::make("fakesink").build().unwrap());
        }
        play_on(play, path)
    }

    /// `play` opened on `path`, paused once its streams are known.
    fn play_on(play: gst_play::Play, path: &Path) -> (gst_play::Play, gst_play::PlayMediaInfo) {
        let uri = gst::glib::filename_to_uri(path, None).unwrap();
        play.set_uri(Some(&uri));
        play.pause();
        let bus = play.message_bus();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
                continue;
            };
            if let Ok(gst_play::PlayMessage::MediaInfoUpdated(m)) =
                gst_play::PlayMessage::parse(&message)
            {
                let info = m.media_info().clone();
                if info.uri() == uri
                    && info.audio_streams().len() == 2
                    && info.subtitle_streams().len() == 1
                {
                    return (play, info);
                }
            }
        }
        panic!("GstPlay never listed two audio streams and a subtitle");
    }

    /// Where a clip goes, in a directory of the test's own.
    fn clip_path(name: &str) -> (TestDir, std::path::PathBuf) {
        let dir = TestDir::new_blocking(&format!("spoolframe-tracks-{name}"));
        let path = dir.join("clip.mkv");
        (dir, path)
    }

    /// One clip on one Play, each pick made after the one before.
    #[test]
    fn the_streams_of_a_clip_can_be_chosen() {
        gst::init().unwrap();
        let (_dir, path) = clip_path("streams");
        clip(&path);
        let (play, info) = play(&path);
        let audio = || {
            play.current_audio_track()
                .map(|t| PlayStreamInfoExt::stream_id(&t))
        };
        let subtitle = || {
            play.current_subtitle_track()
                .map(|t| PlayStreamInfoExt::stream_id(&t))
        };
        // The audio already playing.
        let same = select_audio(&play, &info, 0);
        // The second audio stream.
        let second = select_audio(&play, &info, 1);
        let playing_audio = audio();
        // GstPlay selects the first subtitle by default, with subtitles off.
        let shown = select_subtitle(&play, &info, Some(0), &Default::default());
        let showing = subtitle();
        play.stop();
        assert!(same.is_ok(), "the audio already playing: {same:?}");
        assert!(second.is_ok(), "the second audio: {second:?}");
        assert_eq!(
            playing_audio,
            Some(PlayStreamInfoExt::stream_id(&info.audio_streams()[1])),
            "the second audio plays"
        );
        assert!(shown.is_ok(), "the subtitle: {shown:?}");
        assert_eq!(
            showing,
            Some(PlayStreamInfoExt::stream_id(&info.subtitle_streams()[0])),
            "the subtitle shows"
        );
    }

    /// Each load is a new URI to GstPlay, so new stream ids: the app gives
    /// every playback its own spoolframe:// URI. A pick made with what the
    /// last load listed still lands (Vigil S2E05's subtitles did not).
    #[test]
    fn a_pick_made_with_an_earlier_loads_list_still_lands() {
        gst::init().unwrap();
        let (_dir, path) = clip_path("reloaded");
        clip(&path);
        let (play, earlier) = play(&path);
        play.stop();
        let again = path.with_file_name("again.mkv");
        std::fs::copy(&path, &again).unwrap();
        let (play, now) = play_on(play, &again);
        let picked = (
            select_audio(&play, &earlier, 1),
            select_subtitle(&play, &earlier, Some(0), &Default::default()),
        );
        let playing = (
            play.current_audio_track()
                .map(|t| PlayStreamInfoExt::stream_id(&t)),
            play.current_subtitle_track()
                .map(|t| PlayStreamInfoExt::stream_id(&t)),
        );
        play.stop();
        assert!(picked.0.is_ok() && picked.1.is_ok(), "{picked:?}");
        assert_eq!(
            playing,
            (
                Some(PlayStreamInfoExt::stream_id(&now.audio_streams()[1])),
                Some(PlayStreamInfoExt::stream_id(&now.subtitle_streams()[0]))
            )
        );
    }

    #[test]
    fn a_search_matches_every_word_in_any_case() {
        assert!(matches("English · SDH · SubRip", "sdh english"));
    }

    #[test]
    fn a_search_with_a_missing_word_does_not_match() {
        assert!(!matches("English · SubRip", "english sdh"));
    }

    #[test]
    fn the_count_says_how_many_tracks_a_search_shows() {
        assert_eq!(count_label(3, 41), "3 of 41");
    }

    /// An item's subtitles, each embedded or external.
    fn item_subtitles(external: &[bool]) -> MediaTracks {
        MediaTracks {
            subtitles: external
                .iter()
                .map(|&external| spoolframe_types::SubtitleTrack {
                    language: Some("eng".into()),
                    title: None,
                    forced: false,
                    default: false,
                    external,
                    role: spoolframe_types::SubtitleRole::default(),
                    timing: None,
                    download: None,
                })
                .collect(),
            ..MediaTracks::default()
        }
    }

    fn direct(subtitle: Option<(u32, SubtitleDelivery)>) -> PlaybackPlan {
        PlaybackPlan::Direct {
            container: spoolframe_types::Container::Matroska,
            video: None,
            audio: None,
            subtitle: subtitle.map(|(index, delivery)| spoolframe_protocol::PlannedSubtitle {
                index,
                format: spoolframe_types::SubtitleFormat::Srt,
                delivery,
            }),
            source_codec: None,
        }
    }

    fn converted(index: u32, action: SubtitleAction) -> PlaybackPlan {
        PlaybackPlan::Converted {
            delivery: spoolframe_types::Delivery::Progressive,
            container: spoolframe_types::Container::Matroska,
            video: None,
            audio: None,
            subtitle: Some(spoolframe_protocol::ConvertedSubtitle { index, action }),
            reasons: Vec::new(),
            ladder: Vec::new(),
            source_codec: None,
        }
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// Banshee S3E9: a searched subtitle (the item's third track, after
    /// two embedded ones) joined GstPlay's streams between the embedded
    /// ones. It is the plan's track wherever it lands, and the embedded
    /// streams keep their order around it.
    #[test]
    fn a_direct_plays_file_and_its_subtitle_file_are_the_items_tracks() {
        let tracks = item_subtitles(&[false, false, true]);
        let plan = direct(Some((2, SubtitleDelivery::External)));
        assert_eq!(
            stream_tracks(&tracks, &plan, &ids(&["e0", "file", "e1"]), Some("file")),
            [Some(0), Some(2), Some(1)]
        );
    }

    /// An external track between embedded ones is not in the file: the
    /// file's streams are the embedded tracks around it.
    #[test]
    fn an_external_track_takes_no_place_among_the_files_streams() {
        let tracks = item_subtitles(&[false, true, false]);
        let plan = direct(Some((0, SubtitleDelivery::Embedded)));
        assert_eq!(
            stream_tracks(&tracks, &plan, &ids(&["e0", "e1"]), None),
            [Some(0), Some(2)]
        );
    }

    #[test]
    fn a_converted_outputs_one_subtitle_is_the_planned_track_unless_burned_in() {
        let tracks = item_subtitles(&[false, false]);
        let copied = converted(
            1,
            SubtitleAction::Copy {
                format: spoolframe_types::SubtitleFormat::Srt,
            },
        );
        let burned = converted(
            1,
            SubtitleAction::BurnIn {
                format: spoolframe_types::SubtitleFormat::Pgs,
            },
        );
        assert_eq!(
            (
                stream_tracks(&tracks, &copied, &ids(&["out"]), None),
                stream_tracks(&tracks, &burned, &ids(&["out"]), None),
            ),
            (vec![Some(1)], vec![None])
        );
    }
}
