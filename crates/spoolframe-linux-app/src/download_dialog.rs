//! "Download Alien" (PX-38), as the LinuxDownloadSheet board has it: the
//! version (Original, then each optimized profile the admin set), the
//! tracks (every one, or a choice ticked from the user's languages first),
//! and the room left here. What the server says of the item
//! (`DownloadOptionsInfo`) is all it shows: the board's per-profile sizes,
//! codecs and "ready on the server" are not sent, so they are left out.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use spoolframe_linux_core::backend::{
    DownloadOptionsInfo, DownloadSource, DownloadSpace, DownloadTracks, OriginalDownload,
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

/// The footer: how many tracks go with the video, and the room here.
pub fn footer(choice: &Choice, space: Option<DownloadSpace>) -> String {
    let tracks = match choice.all {
        true => "Every track".to_owned(),
        false => match choice.audio.len() + choice.subtitles.len() {
            1 => "With 1 track".to_owned(),
            n => format!("With {n} tracks"),
        },
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
        let names = std::iter::once((
            "Original".to_owned(),
            Some(original_line(&options.original)),
        ))
        .chain(options.profiles.iter().map(|p| (p.clone(), None)));
        for (index, (name, line)) in names.enumerate() {
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
            .label(footer(&choice, picker.space))
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
        self.footer
            .set_label(&footer(&self.choice.borrow(), self.picker.space));
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
            footer(&choice, Some(space)),
            "With 3 tracks · 182.0 GB free here"
        );
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
