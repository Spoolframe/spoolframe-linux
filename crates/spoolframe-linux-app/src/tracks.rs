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

/// Plays the `position`th audio stream of `info`.
pub fn select_audio(
    play: &gst_play::Play,
    info: &gst_play::PlayMediaInfo,
    position: usize,
) -> Result<(), glib::BoolError> {
    let stream = info
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

/// Shows the `position`th subtitle stream of `info`, or none.
pub fn select_subtitle(
    play: &gst_play::Play,
    info: &gst_play::PlayMediaInfo,
    position: Option<usize>,
) -> Result<(), glib::BoolError> {
    let Some(position) = position else {
        play.set_subtitle_track_enabled(false);
        return Ok(());
    };
    let stream = info
        .subtitle_streams()
        .get(position)
        .cloned()
        .ok_or_else(|| glib::bool_error!("no subtitle stream {position}"))?;
    let id = PlayStreamInfoExt::stream_id(&stream);
    if play
        .current_subtitle_track()
        .map(|t| PlayStreamInfoExt::stream_id(&t))
        != Some(id.clone())
    {
        play.set_subtitle_track_id(Some(&id))?;
    }
    play.set_subtitle_track_enabled(true);
    Ok(())
}

/// Subtitle lists longer than this get a search field.
pub const SEARCH_FROM: usize = 3;

/// The player's audio and subtitle menu: lists to pick one from, with a
/// check mark on what plays. `subtitles_on` is shared with the player,
/// which also turns subtitles on and off (the item page's choice, an
/// external subtitle); GstPlay has no call that says whether they are on.
pub fn menu(
    play: &gst_play::Play,
    info: &gst_play::PlayMediaInfo,
    subtitles_on: &Rc<Cell<bool>>,
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
                Some(s.channels()).filter(|c| *c > 0).map(channel_layout),
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

    {
        let (play, info, marks) = (play.clone(), info.clone(), audio.clone());
        audio.list.connect_row_activated(move |_, row| {
            let position = row.index() as usize;
            match select_audio(&play, &info, position) {
                Ok(()) => marks.mark(Some(position)),
                Err(e) => tracing::warn!(error = %e, position, "cannot switch audio"),
            }
        });
    }
    {
        let (play, info, marks, on) = (
            play.clone(),
            info.clone(),
            subtitles.clone(),
            subtitles_on.clone(),
        );
        subtitles.list.connect_row_activated(move |_, row| {
            // Row 0 is "Off"; the streams follow.
            let position = (row.index() as usize).checked_sub(1);
            match select_subtitle(&play, &info, position) {
                Ok(()) => {
                    on.set(position.is_some());
                    marks.mark(Some(row.index() as usize));
                }
                Err(e) => tracing::warn!(error = %e, ?position, "cannot switch subtitles"),
            }
        });
    }

    let popover = gtk::Popover::builder().child(&column).build();
    // What plays can change without the menu (the item page's choice, an
    // external subtitle arriving), so the marks are read on each opening.
    let (play, info, on) = (play.clone(), info.clone(), subtitles_on.clone());
    popover.connect_show(move |_| {
        audio.mark(current_audio(&play, &info));
        let subtitle = match on.get() {
            false => Some(0),
            true => current_subtitle(&play, &info).map(|p| p + 1),
        };
        subtitles.mark(subtitle);
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
}

impl PickList {
    fn new(names: &[String]) -> Self {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .css_classes(["pick-list"])
            .build();
        let mut checks = Vec::new();
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
            row.append(
                &gtk::Label::builder()
                    .label(name)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build(),
            );
            check.add_css_class("accent");
            row.append(&check);
            list.append(&row);
            checks.push(check);
        }
        Self {
            list,
            checks: Rc::new(checks),
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

pub fn channel_layout(channels: i32) -> String {
    match channels {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        6 => "5.1".to_owned(),
        8 => "7.1".to_owned(),
        n => format!("{n} ch"),
    }
}

#[cfg(test)]
mod tests {
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
        play.set_uri(Some(&gst::glib::filename_to_uri(path, None).unwrap()));
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
                if info.audio_streams().len() == 2 && info.subtitle_streams().len() == 1 {
                    return (play, info);
                }
            }
        }
        panic!("GstPlay never listed two audio streams and a subtitle");
    }

    fn clip_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "spoolframe-tracks-{}-{name}.mkv",
            std::process::id()
        ))
    }

    #[test]
    fn the_second_audio_stream_can_be_chosen() {
        gst::init().unwrap();
        let path = clip_path("audio");
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
        let path = clip_path("same-audio");
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
        let path = clip_path("subtitle");
        clip(&path);
        let (play, info) = play(&path);
        select_subtitle(&play, &info, Some(0)).unwrap();
        let showing = play
            .current_subtitle_track()
            .map(|t| PlayStreamInfoExt::stream_id(&t));
        play.stop();
        assert_eq!(
            showing,
            Some(PlayStreamInfoExt::stream_id(&info.subtitle_streams()[0]))
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
