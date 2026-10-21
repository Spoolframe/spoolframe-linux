//! "Download Alien" (PX-38), as the LinuxDownloadSheet board has it: the
//! version (Original, then each optimized profile the admin set), the
//! tracks (every one, or a choice ticked from the user's languages first),
//! and the room left here. What the server says of the item
//! (`DownloadOptionsInfo`) is all it shows: each version's size and
//! whether the server must convert it first (`estimates`), but not the
//! board's codecs, which are not sent, so they are left out.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use spoolframe_linux_core::backend::{
    DownloadEstimate, DownloadOptionsInfo, DownloadReadiness, DownloadSource, DownloadSpace,
    DownloadTracks, OriginalDownload,
};
use spoolframe_linux_core::format;

/// What the dialog needs.
pub struct Picker {
    /// The item's name, for the heading.
    pub title: String,
    pub options: DownloadOptionsInfo,
    /// The room here, if it could be read.
    pub space: Option<DownloadSpace>,
    /// Starts the download chosen.
    pub start: Rc<dyn Fn(DownloadSource, DownloadTracks)>,
}

/// What is chosen: the version by its place (0 is Original, then the
/// profiles in order), and the tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub version: usize,
    /// Every track: the only choice that may send the file as it is.
    pub all: bool,
    pub audio: BTreeSet<u32>,
    pub subtitles: BTreeSet<u32>,
}

impl Choice {
    /// What the dialog opens on: Original, with the server's preferred
    /// tracks ticked and every track off. A subtitle the download would
    /// leave out is never ticked.
    pub fn first(options: &DownloadOptionsInfo) -> Self {
        let (audio, subtitles) = match &options.preferred {
            DownloadTracks::Chosen { audio, subtitles } => (audio.clone(), subtitles.clone()),
            _ => (Vec::new(), Vec::new()),
        };
        let left_out: BTreeSet<u32> = options.left_out.iter().map(|l| l.subtitle).collect();
        Choice {
            version: 0,
            all: false,
            audio: audio.into_iter().collect(),
            subtitles: subtitles
                .into_iter()
                .filter(|s| !left_out.contains(s))
                .collect(),
        }
    }

    pub fn source(&self, options: &DownloadOptionsInfo) -> DownloadSource {
        match self
            .version
            .checked_sub(1)
            .and_then(|i| options.profiles.get(i))
        {
            Some(name) => DownloadSource::Profile { name: name.clone() },
            None => DownloadSource::Original,
        }
    }

    pub fn tracks(&self) -> DownloadTracks {
        if self.all {
            return DownloadTracks::All;
        }
        DownloadTracks::Chosen {
            audio: self.audio.iter().copied().collect(),
            subtitles: self.subtitles.iter().copied().collect(),
        }
    }
}

/// What Original would be, under its name.
pub fn original_line(original: &OriginalDownload) -> String {
    let tracks = |n: usize| match n {
        1 => "1 audio track".to_owned(),
        n => format!("{n} audio tracks"),
    };
    match original {
        OriginalDownload::AsIs => {
            "This computer plays the source as it is: nothing converted".into()
        }
        OriginalDownload::Remux => "Repackaged for this computer: nothing converted".into(),
        OriginalDownload::Convert { video, audio } => match (video, audio.len()) {
            (true, 0) => "The server converts the video first".into(),
            (false, n) => format!("The server converts {} first", tracks(n)),
            (true, n) => format!("The server converts the video and {} first", tracks(n)),
        },
        OriginalDownload::Unknown => "The source, as close as this computer plays it".into(),
    }
}

/// What the server says of `source`, found by the source it names, not
/// by place: none when it sent no estimate (a server before 0.14.2, or
/// an item that is not a film or an episode).
pub fn estimate_for<'a>(
    options: &'a DownloadOptionsInfo,
    source: &DownloadSource,
) -> Option<&'a DownloadEstimate> {
    options.estimates.iter().find(|e| &e.source == source)
}

/// A version's size, as the board has it: Original's is the file's own
/// ("38.4 GB"), a profile's a guess ("about 3.6 GB"). None when the
/// server could not tell.
pub fn size_text(source: &DownloadSource, estimate: Option<&DownloadEstimate>) -> Option<String> {
    let bytes = estimate?.size_bytes?;
    let size = crate::downloads::size(bytes);
    Some(match source {
        DownloadSource::Original => size,
        _ => format!("about {size}"),
    })
}

/// Whether the server must convert a version first. A kind this build
/// does not know says nothing.
pub fn readiness_line(readiness: DownloadReadiness) -> Option<&'static str> {
    match readiness {
        DownloadReadiness::Ready => Some("Ready on the server"),
        DownloadReadiness::ConvertsFirst => Some("The server converts it first"),
        DownloadReadiness::Unknown => None,
    }
}

/// The line under a version's name. Original keeps what it would be on
/// this computer, which says more than "ready" (as is, repackaged, or
/// what is converted); only when that is not known does the server's
/// readiness stand in.
pub fn version_line(
    source: &DownloadSource,
    original: &OriginalDownload,
    estimate: Option<&DownloadEstimate>,
) -> Option<String> {
    let readiness = estimate.and_then(|e| readiness_line(e.readiness));
    match source {
        DownloadSource::Original => match original {
            OriginalDownload::Unknown => readiness.map(str::to_owned),
            _ => Some(original_line(original)),
        },
        _ => readiness.map(str::to_owned),
    }
}

/// A track's row: its place in the item's tracks, its name, and why it
/// is left out, if it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRow {
    pub place: u32,
    pub label: String,
    pub left_out: Option<String>,
}

pub fn audio_rows(options: &DownloadOptionsInfo) -> Vec<TrackRow> {
    (0u32..)
        .zip(&options.tracks.audio)
        .map(|(place, track)| TrackRow {
            place,
            label: format::audio_track(track),
            left_out: None,
        })
        .collect()
}

/// The subtitles: those the download keeps, then those it leaves out,
/// with why.
pub fn subtitle_rows(options: &DownloadOptionsInfo) -> Vec<TrackRow> {
    let why = |place: u32| {
        options
            .left_out
            .iter()
            .find(|l| l.subtitle == place)
            .map(|l| l.reason.clone())
    };
    let (kept, left_out): (Vec<_>, Vec<_>) = (0u32..)
        .zip(&options.tracks.subtitles)
        .map(|(place, track)| TrackRow {
            place,
            label: format::subtitle_track(track),
            left_out: why(place),
        })
        .partition(|row| row.left_out.is_none());
    kept.into_iter().chain(left_out).collect()
}

/// The footer: the chosen version's size, when it is known for what is
/// asked, how many tracks go with the video, and the room here. The
/// server sizes a version with every track (the request does not carry
/// the choice), so the size shows only while All tracks is on; a
/// smaller choice has no number the server gave, and none is made up.
pub fn footer(
    choice: &Choice,
    options: &DownloadOptionsInfo,
    space: Option<DownloadSpace>,
) -> String {
    let source = choice.source(options);
    let about = match choice.all {
        true => estimate_for(options, &source)
            .and_then(|e| e.size_bytes)
            .map(|bytes| format!("About {}", crate::downloads::size(bytes))),
        false => None,
    };
    let tracks = match choice.all {
        true => "Every track".to_owned(),
        false => match choice.audio.len() + choice.subtitles.len() {
            1 => "With 1 track".to_owned(),
            n => format!("With {n} tracks"),
        },
    };
    let tracks = match about {
        Some(about) => format!("{about} · {}", tracks.to_lowercase()),
        None => tracks,
    };
    match space {
        Some(space) => format!(
            "{tracks} · {} free here",
            crate::downloads::size(space.available)
        ),
        None => tracks,
    }
}

/// The dialog's state and the widgets it changes.
pub struct View {
    pub dialog: adw::Dialog,
    picker: Picker,
    choice: RefCell<Choice>,
    /// The tracks' columns, insensitive while every track is chosen.
    tracks: gtk::Box,
    footer: gtk::Label,
    /// Each subtitle's tick, by its place.
    #[cfg(test)]
    subtitles: Vec<(u32, gtk::CheckButton)>,
}

impl View {
    pub fn new(picker: Picker) -> Rc<Self> {
        let options = &picker.options;
        let choice = Choice::first(options);
        let dialog = adw::Dialog::builder()
            .title(format!("Download {}", picker.title))
            .content_width(620)
            .content_height(720)
            .build();
        let page = adw::PreferencesPage::new();

        let versions = adw::PreferencesGroup::builder()
            .title("Version")
            .description(
                "Original is the source as it is, or as close to it as this computer can play: \
                 only what it can’t play is converted, at the source’s resolution and range.",
            )
            .build();
        let mut first: Option<gtk::CheckButton> = None;
        let mut radios = Vec::new();
        let names = std::iter::once(("Original".to_owned(), DownloadSource::Original)).chain(
            options
                .profiles
                .iter()
                .map(|p| (p.clone(), DownloadSource::Profile { name: p.clone() })),
        );
        for (index, (name, source)) in names.enumerate() {
            let estimate = estimate_for(options, &source);
            let line = version_line(&source, &options.original, estimate);
            let size = size_text(&source, estimate);
            let radio = gtk::CheckButton::builder()
                .active(index == choice.version)
                .valign(gtk::Align::Center)
                .build();
            if let Some(first) = &first {
                radio.set_group(Some(first));
            } else {
                first = Some(radio.clone());
            }
            let row = adw::ActionRow::builder().use_markup(false).build();
            row.set_title(&name);
            if let Some(line) = line {
                row.set_subtitle(&line);
            }
            if let Some(size) = size {
                row.add_suffix(
                    &gtk::Label::builder()
                        .label(size)
                        .css_classes(["dim-label"])
                        .build(),
                );
            }
            row.add_prefix(&radio);
            row.set_activatable_widget(Some(&radio));
            versions.add(&row);
            radios.push((index, radio));
        }
        page.add(&versions);

        let all_group = adw::PreferencesGroup::new();
        let all = adw::SwitchRow::builder()
            .use_markup(false)
            .title("All tracks")
            .subtitle("Off: the tracks in your languages, as your profile prefers them")
            .active(choice.all)
            .build();
        all_group.add(&all);
        page.add(&all_group);

        let tracks = gtk::Box::builder().spacing(18).homogeneous(true).build();
        let audio_group = adw::PreferencesGroup::builder().title("Audio").build();
        let mut audio_checks = Vec::new();
        for row in audio_rows(options) {
            let (widget, check) = track_row(&row, choice.audio.contains(&row.place));
            audio_group.add(&widget);
            audio_checks.push((row.place, check));
        }
        let subtitle_group = adw::PreferencesGroup::builder().title("Subtitles").build();
        let mut subtitle_checks = Vec::new();
        for row in subtitle_rows(options) {
            let (widget, check) = track_row(&row, choice.subtitles.contains(&row.place));
            subtitle_group.add(&widget);
            subtitle_checks.push((row.place, check));
        }
        tracks.append(&audio_group);
        tracks.append(&subtitle_group);
        tracks.set_sensitive(!choice.all);
        // Side by side, as the board's two columns, in the page's scroll.
        let columns = adw::PreferencesGroup::new();
        columns.add(&tracks);
        page.add(&columns);

        let foot = gtk::Label::builder()
            .label(footer(&choice, options, picker.space))
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let cancel = gtk::Button::builder().label("Cancel").build();
        let download = gtk::Button::builder()
            .label("Download")
            .css_classes(["suggested-action"])
            .build();
        let bar = gtk::Box::builder()
            .spacing(8)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(18)
            .margin_end(18)
            .build();
        bar.append(&foot);
        bar.append(&cancel);
        bar.append(&download);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        toolbar.add_bottom_bar(&bar);
        dialog.set_child(Some(&toolbar));
        crate::dialog::closes(&cancel, &dialog);

        let view = Rc::new(View {
            dialog,
            picker,
            choice: RefCell::new(choice),
            tracks,
            footer: foot,
            #[cfg(test)]
            subtitles: subtitle_checks.clone(),
        });
        for (index, radio) in radios {
            let weak = Rc::downgrade(&view);
            radio.connect_toggled(move |radio| {
                if let (true, Some(view)) = (radio.is_active(), weak.upgrade()) {
                    view.choice.borrow_mut().version = index;
                    view.refresh();
                }
            });
        }
        {
            let weak = Rc::downgrade(&view);
            all.connect_active_notify(move |row| {
                if let Some(view) = weak.upgrade() {
                    view.set_all(row.is_active());
                }
            });
        }
        for (place, check) in audio_checks {
            let weak = Rc::downgrade(&view);
            check.connect_toggled(move |check| {
                if let Some(view) = weak.upgrade() {
                    view.tick(true, place, check.is_active());
                }
            });
        }
        for (place, check) in subtitle_checks {
            let weak = Rc::downgrade(&view);
            check.connect_toggled(move |check| {
                if let Some(view) = weak.upgrade() {
                    view.tick(false, place, check.is_active());
                }
            });
        }
        {
            let weak = Rc::downgrade(&view);
            download.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.start();
                }
            });
        }
        crate::dialog::keep_until_closed(&view.dialog, view.clone());
        view
    }

    fn set_all(&self, all: bool) {
        self.choice.borrow_mut().all = all;
        self.tracks.set_sensitive(!all);
        self.refresh();
    }

    fn tick(&self, audio: bool, place: u32, on: bool) {
        {
            let mut choice = self.choice.borrow_mut();
            let set = if audio {
                &mut choice.audio
            } else {
                &mut choice.subtitles
            };
            if on {
                set.insert(place);
            } else {
                set.remove(&place);
            }
        }
        self.refresh();
    }

    fn refresh(&self) {
        self.footer.set_label(&footer(
            &self.choice.borrow(),
            &self.picker.options,
            self.picker.space,
        ));
    }

    /// What Download would ask for now.
    pub fn chosen(&self) -> (DownloadSource, DownloadTracks) {
        let choice = self.choice.borrow();
        (choice.source(&self.picker.options), choice.tracks())
    }

    fn start(&self) {
        let (source, tracks) = self.chosen();
        (self.picker.start)(source, tracks);
        self.dialog.close();
    }

    /// The subtitle at `place`'s tick, for tests.
    #[cfg(test)]
    fn subtitle(&self, place: u32) -> Option<&gtk::CheckButton> {
        self.subtitles
            .iter()
            .find(|(p, _)| *p == place)
            .map(|(_, check)| check)
    }
}

/// A track's tick: a left-out subtitle's is insensitive, with why in small
/// dim text under its name.
fn track_row(row: &TrackRow, ticked: bool) -> (adw::ActionRow, gtk::CheckButton) {
    let check = gtk::CheckButton::builder()
        .active(ticked && row.left_out.is_none())
        .valign(gtk::Align::Center)
        .build();
    let widget = adw::ActionRow::builder().use_markup(false).build();
    widget.set_title(&row.label);
    if let Some(why) = &row.left_out {
        widget.set_subtitle(why);
        widget.add_css_class("dim-label");
        widget.set_sensitive(false);
        check.set_sensitive(false);
    }
    widget.add_prefix(&check);
    widget.set_activatable_widget(Some(&check));
    (widget, check)
}

/// Opens the dialog over `parent`.
pub fn present(parent: &impl IsA<gtk::Widget>, picker: Picker) {
    let view = View::new(picker);
    view.dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::{Client, LeftOutSubtitle};
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::testing::ready;
    use spoolframe_types::{MediaId, MediaKind, Namespace};

    use super::*;

    fn options() -> DownloadOptionsInfo {
        let client = FakeClient::new(None);
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        let alien = MediaId::new(Namespace::Plex, MediaKind::Movie, "1").unwrap();
        ready(session.download_options(&alien)).unwrap()
    }

    /// Alien's options with its second subtitle left out, as an Apple
    /// device leaves out picture subtitles.
    fn with_one_left_out() -> DownloadOptionsInfo {
        DownloadOptionsInfo {
            left_out: vec![LeftOutSubtitle {
                subtitle: 0,
                reason: "Image subtitles (PGS) can’t be kept with this profile".into(),
            }],
            ..options()
        }
    }

    #[test]
    fn the_dialog_opens_on_the_preferred_tracks() {
        let options = options();
        assert_eq!(Choice::first(&options).tracks(), options.preferred);
    }

    #[test]
    fn all_tracks_asks_for_every_track() {
        let choice = Choice {
            all: true,
            ..Choice::first(&options())
        };
        assert_eq!(choice.tracks(), DownloadTracks::All);
    }

    #[test]
    fn a_left_out_subtitle_is_never_ticked() {
        let choice = Choice::first(&with_one_left_out());
        assert!(!choice.subtitles.contains(&0));
    }

    #[test]
    fn left_out_subtitles_come_last_with_why() {
        let options = with_one_left_out();
        let rows = subtitle_rows(&options);
        assert_eq!(
            rows.last().map(|r| (r.place, r.left_out.clone())),
            Some((0, Some(options.left_out[0].reason.clone())))
        );
    }

    #[test]
    fn a_profile_is_asked_for_by_name() {
        let options = options();
        let choice = Choice {
            version: 1,
            ..Choice::first(&options)
        };
        assert_eq!(
            (
                choice.source(&options),
                Choice::first(&options).source(&options)
            ),
            (
                DownloadSource::Profile {
                    name: "Mobile".into()
                },
                DownloadSource::Original
            )
        );
    }

    #[test]
    fn original_says_what_the_server_converts() {
        assert_eq!(
            [
                OriginalDownload::AsIs,
                OriginalDownload::Convert {
                    video: true,
                    audio: vec![1],
                },
            ]
            .map(|o| original_line(&o)),
            [
                "This computer plays the source as it is: nothing converted",
                "The server converts the video and 1 audio track first"
            ]
        );
    }

    #[test]
    fn the_footer_counts_the_tracks_and_the_room() {
        let choice = Choice {
            all: false,
            audio: [0].into(),
            subtitles: [0, 2].into(),
            version: 0,
        };
        let space = DownloadSpace {
            available: 182_000_000_000,
            used: 0,
        };
        assert_eq!(
            footer(&choice, &options(), Some(space)),
            "With 3 tracks · 182.0 GB free here"
        );
    }

    fn profile(name: &str) -> DownloadSource {
        DownloadSource::Profile { name: name.into() }
    }

    #[test]
    fn an_estimate_is_found_by_its_source_not_its_place() {
        let mut options = options();
        options.estimates.reverse();
        assert_eq!(
            estimate_for(&options, &profile("Mobile")).map(|e| e.size_bytes),
            Some(Some(3_600_000_000))
        );
    }

    #[test]
    fn a_version_without_an_estimate_has_no_size_and_no_line() {
        let mut options = options();
        options.estimates.clear();
        let source = profile("Mobile");
        let estimate = estimate_for(&options, &source);
        assert_eq!(
            (
                size_text(&source, estimate),
                version_line(&source, &options.original, estimate)
            ),
            (None, None)
        );
    }

    #[test]
    fn original_has_its_size_and_a_profile_about_its_size() {
        let options = options();
        let sizes = [
            DownloadSource::Original,
            profile("Mobile"),
            profile("Mobile (H.264)"),
        ]
        .map(|s| size_text(&s, estimate_for(&options, &s)));
        assert_eq!(
            sizes,
            [
                Some("38.4 GB".to_owned()),
                Some("about 3.6 GB".to_owned()),
                None
            ]
        );
    }

    #[test]
    fn readiness_says_whether_the_server_converts_first() {
        assert_eq!(
            [
                DownloadReadiness::Ready,
                DownloadReadiness::ConvertsFirst,
                DownloadReadiness::Unknown
            ]
            .map(readiness_line),
            [
                Some("Ready on the server"),
                Some("The server converts it first"),
                None
            ]
        );
    }

    #[test]
    fn original_keeps_its_own_line_unless_that_is_unknown() {
        let options = options();
        let estimate = estimate_for(&options, &DownloadSource::Original);
        let lines = [OriginalDownload::AsIs, OriginalDownload::Unknown]
            .map(|o| version_line(&DownloadSource::Original, &o, estimate));
        assert_eq!(
            lines,
            [
                Some("This computer plays the source as it is: nothing converted".to_owned()),
                Some("Ready on the server".to_owned())
            ]
        );
    }

    #[test]
    fn the_footer_shows_the_size_only_for_every_track() {
        let options = options();
        let every = Choice {
            all: true,
            version: 1,
            ..Choice::first(&options)
        };
        let some = Choice {
            all: false,
            audio: [0].into(),
            subtitles: [1].into(),
            ..every.clone()
        };
        assert_eq!(
            [
                footer(&every, &options, None),
                footer(&some, &options, None)
            ],
            ["About 3.6 GB · every track", "With 2 tracks"].map(str::to_owned)
        );
    }

    #[test]
    fn the_footer_has_no_size_when_the_server_gave_none() {
        let options = options();
        let choice = Choice {
            all: true,
            version: 2,
            ..Choice::first(&options)
        };
        assert_eq!(footer(&choice, &options, None), "Every track");
    }

    /// The dialog itself: a left-out subtitle's tick cannot be used, and
    /// All tracks is what Download asks for once it is on.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_dialog_greys_left_out_subtitles_and_asks_for_all() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let view = View::new(Picker {
            title: "Alien".into(),
            options: with_one_left_out(),
            space: None,
            start: Rc::new(|_, _| {}),
        });
        let left_out = view.subtitle(0).unwrap();
        let kept = view.subtitle(1).unwrap();
        assert_eq!(
            (
                left_out.is_sensitive(),
                left_out.is_active(),
                kept.is_sensitive()
            ),
            (false, false, true)
        );
        view.set_all(true);
        assert_eq!(view.chosen().1, DownloadTracks::All);
    }
}
