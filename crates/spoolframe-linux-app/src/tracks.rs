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

use crate::external_subtitle::ExternalSubtitle;

/// Plays the `position`th audio stream of `info`.
pub fn select_audio(
    play: &gst_play::Play,
    info: &gst_play::PlayMediaInfo,
    position: usize,
) -> Result<(), glib::BoolError> {
    let stream = now(play, info)
        .audio_streams()
        .get(position)
        .cloned()
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
    let stream = now(play, info)
        .subtitle_streams()
        .get(position)
        .cloned()
        .ok_or_else(|| glib::bool_error!("no subtitle stream {position}"))?;
    let id = PlayStreamInfoExt::stream_id(&stream);
    external.picking(Some(&id));
    if play
        .current_subtitle_track()
        .map(|t| PlayStreamInfoExt::stream_id(&t))
        != Some(id.clone())
    {
        play.set_subtitle_track_id(Some(&id))?;
    }
    play.set_subtitle_track_enabled(true);
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
    SubtitleExtras { find, timing }: SubtitleExtras,
) -> gtk::Popover {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .width_request(280)
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

    let subtitle_names: Vec<String> = std::iter::once("Off".to_owned())
        .chain(
            info.subtitle_streams()
                .iter()
                .map(|s| stream_label(s.language().as_deref(), s.codec().as_deref(), None)),
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
        subtitles.list.connect_row_activated(move |_, row| {
            let Some(play) = play.upgrade() else { return };
            // Row 0 is "Off"; the streams follow.
            let position = (row.index() as usize).checked_sub(1);
            match select_subtitle(&play, &info, position, &external) {
                Ok(()) => {
                    on.set(position.is_some());
                    marks.mark(Some(row.index() as usize));
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
        let choices = crate::quality::choices();
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

    let popover = gtk::Popover::builder().child(&column).build();
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
            picks.mark(Some(crate::quality::index(chosen)));
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

    #[test]
    fn the_second_audio_stream_can_be_chosen() {
        gst::init().unwrap();
        let (_dir, path) = clip_path("audio");
        clip(&path);
        let (play, info) = play(&path);
        select_audio(&play, &info, 1).unwrap();
        let playing = play
            .current_audio_track()
            .map(|t| PlayStreamInfoExt::stream_id(&t));
        play.stop();
        assert_eq!(
            playing,
            Some(PlayStreamInfoExt::stream_id(&info.audio_streams()[1]))
        );
    }

    #[test]
    fn choosing_the_audio_already_playing_succeeds() {
        gst::init().unwrap();
        let (_dir, path) = clip_path("same-audio");
        clip(&path);
        let (play, info) = play(&path);
        let chosen = select_audio(&play, &info, 0);
        play.stop();
        assert!(chosen.is_ok(), "{chosen:?}");
    }

    /// GstPlay selects the first subtitle by default, with subtitles off.
    #[test]
    fn a_subtitle_stream_can_be_chosen() {
        gst::init().unwrap();
        let (_dir, path) = clip_path("subtitle");
        clip(&path);
        let (play, info) = play(&path);
        select_subtitle(&play, &info, Some(0), &Default::default()).unwrap();
        let showing = play
            .current_subtitle_track()
            .map(|t| PlayStreamInfoExt::stream_id(&t));
        play.stop();
        assert_eq!(
            showing,
            Some(PlayStreamInfoExt::stream_id(&info.subtitle_streams()[0]))
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
}
