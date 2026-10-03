//! The Preferences dialog: Appearance (the app's theme, kept on this
//! device), Server (the fingerprint of the server in use, P1-21) and, when
//! signed in, Playback (the user's audio and subtitle preferences, kept on
//! the server for every device, P1-20) and Profile (the PIN and where it
//! is asked for, P2-08).

use std::rc::Rc;

use adw::prelude::*;
use spoolframe_linux_core::backend::Server;
use spoolframe_linux_core::format;
use spoolframe_linux_core::theme::Theme;
use spoolframe_types::{AudioRole, SdhPreference, SubtitleMode, TrackPreferences};

/// When subtitles show, in the order the dialog lists them.
const MODES: [(SubtitleMode, &str); 4] = [
    (
        SubtitleMode::ForcedOnly,
        "Only forced (signs, foreign speech)",
    ),
    (SubtitleMode::Always, "Always"),
    (
        SubtitleMode::ForeignOnly,
        "When the audio is in another language",
    ),
    (SubtitleMode::Off, "Never"),
];

const SDH: [(SdhPreference, &str); 3] = [
    (
        SdhPreference::Accept,
        "Plain first, SDH if that is all there is",
    ),
    (SdhPreference::Prefer, "Prefer SDH"),
    (SdhPreference::Avoid, "Never SDH"),
];

pub fn dialog(
    theme: Theme,
    tracks: Option<TrackPreferences>,
    server: Option<(Server, Rc<dyn Fn()>)>,
    set_theme: Rc<dyn Fn(Theme)>,
    set_tracks: Rc<dyn Fn(TrackPreferences)>,
    bitrate: (Option<u64>, SetBitrate),
    signed: Option<crate::profile_page::Signed>,
) -> adw::PreferencesDialog {
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&appearance(theme, set_theme));
    dialog.add(&streaming(bitrate.0, bitrate.1));
    if let Some((server, verify)) = server {
        dialog.add(&server_page(&server, verify));
    }
    if let Some(tracks) = tracks {
        dialog.add(&playback(tracks, set_tracks));
    }
    if let Some(signed) = signed {
        dialog.add(&crate::profile_page::page(signed, &dialog));
    }
    dialog
}

/// The server in use: its name, its fingerprint always (P1-21), so it can
/// be checked again at any time, and whether it was verified.
fn server_page(server: &Server, verify: Rc<dyn Fn()>) -> adw::PreferencesPage {
    let group = adw::PreferencesGroup::builder()
        .title(&server.name)
        .description("Compare the code with the server's log or `spoolframe-server fingerprint`.")
        .build();
    let fingerprint = adw::ActionRow::builder()
        .title("Fingerprint")
        .subtitle(&server.fingerprint)
        .subtitle_selectable(true)
        .css_classes(["property", "monospace"])
        .build();
    group.add(&fingerprint);
    let status = adw::ActionRow::builder()
        .title(if server.verified {
            "Verified"
        } else {
            "Not verified"
        })
        .subtitle(if server.verified {
            "You confirmed this code when you added the server"
        } else {
            "The code was not checked when the server was added"
        })
        .build();
    if !server.verified {
        let check = gtk::Button::builder()
            .label("Verify now")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        check.connect_clicked(move |button| {
            // Close preferences so the check shows over the window.
            if let Some(dialog) = button.ancestor(adw::Dialog::static_type()) {
                dialog.downcast::<adw::Dialog>().expect("a dialog").close();
            }
            verify();
        });
        status.add_suffix(&check);
    }
    group.add(&status);
    let page = adw::PreferencesPage::builder()
        .title("Server")
        .icon_name(crate::icons::SERVER)
        .build();
    page.add(&group);
    page
}

fn appearance(current: Theme, set_theme: Rc<dyn Fn(Theme)>) -> adw::PreferencesPage {
    let names = gtk::StringList::new(&Theme::ALL.map(Theme::name));
    let row = adw::ComboRow::builder()
        .title("Theme")
        .subtitle(if crate::theme::desktop_has_preference() {
            "Follow system uses your desktop's style and accent colour"
        } else {
            "Your desktop states no light or dark preference, so Follow system uses Mocha"
        })
        .model(&names)
        .selected(Theme::ALL.iter().position(|t| *t == current).unwrap_or(0) as u32)
        .build();
    row.connect_selected_notify(move |row| {
        if let Some(theme) = Theme::ALL.get(row.selected() as usize) {
            set_theme(*theme);
        }
    });
    let group = adw::PreferencesGroup::builder().title("Theme").build();
    group.add(&row);
    let page = adw::PreferencesPage::builder()
        .title("Appearance")
        .icon_name(crate::icons::APPEARANCE)
        .build();
    page.add(&group);
    page
}

/// Saves a new bitrate limit.
pub type SetBitrate = Rc<dyn Fn(Option<u64>)>;

/// The bitrate limits offered, in bits per second; `None` is no limit.
pub const BITRATE_LIMITS: [Option<u64>; 7] = [
    None,
    Some(40_000_000),
    Some(20_000_000),
    Some(12_000_000),
    Some(8_000_000),
    Some(4_000_000),
    Some(2_000_000),
];

/// "No limit", or "8 Mbit/s".
pub fn bitrate_name(limit: Option<u64>) -> String {
    match limit {
        None => "No limit".to_owned(),
        Some(bits) => format!("{} Mbit/s", bits / 1_000_000),
    }
}

/// The offered limit nearest `current` (a hand-edited setting may be
/// none of them).
fn nearest_limit(current: Option<u64>) -> usize {
    match current {
        None => 0,
        Some(bits) => BITRATE_LIMITS
            .iter()
            .enumerate()
            .skip(1)
            .min_by_key(|(_, limit)| limit.unwrap_or(0).abs_diff(bits))
            .map_or(0, |(i, _)| i),
    }
}

/// The most the server may send (P2-06): over it, it encodes down. Off by
/// default, as on a LAN.
fn streaming(current: Option<u64>, set: SetBitrate) -> adw::PreferencesPage {
    let names: Vec<String> = BITRATE_LIMITS.iter().map(|l| bitrate_name(*l)).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title("Bitrate limit")
        .subtitle("Over it, the server converts to fit. Applies when Spoolframe next starts.")
        .model(&gtk::StringList::new(&names))
        .selected(nearest_limit(current) as u32)
        .build();
    row.connect_selected_notify(move |row| {
        if let Some(limit) = BITRATE_LIMITS.get(row.selected() as usize) {
            set(*limit);
        }
    });
    let group = adw::PreferencesGroup::builder().title("Streaming").build();
    group.add(&row);
    let page = adw::PreferencesPage::builder()
        .title("Streaming")
        .icon_name(crate::icons::DIAGNOSTICS)
        .build();
    page.add(&group);
    page
}

/// The rows of the Playback page, read together whenever one changes.
#[derive(Clone)]
struct PlaybackRows {
    original: TrackPreferences,
    audio_languages: adw::EntryRow,
    avoid_description: adw::SwitchRow,
    avoid_commentary: adw::SwitchRow,
    mode: adw::ComboRow,
    subtitle_languages: adw::EntryRow,
    sdh: adw::ComboRow,
}

impl PlaybackRows {
    fn preferences(&self) -> TrackPreferences {
        let mut preferences = self.original.clone();
        preferences.audio.languages = format::parse_language_list(&self.audio_languages.text());
        // Roles this page has no switch for are kept as they were.
        preferences
            .audio
            .avoid
            .retain(|r| !matches!(r, AudioRole::AudioDescription | AudioRole::Commentary));
        if self.avoid_description.is_active() {
            preferences.audio.avoid.push(AudioRole::AudioDescription);
        }
        if self.avoid_commentary.is_active() {
            preferences.audio.avoid.push(AudioRole::Commentary);
        }
        if let Some((mode, _)) = MODES.get(self.mode.selected() as usize) {
            preferences.subtitles.mode = *mode;
        }
        preferences.subtitles.languages =
            format::parse_language_list(&self.subtitle_languages.text());
        if let Some((sdh, _)) = SDH.get(self.sdh.selected() as usize) {
            preferences.subtitles.sdh = *sdh;
        }
        preferences
    }
}

fn playback(
    preferences: TrackPreferences,
    set_tracks: Rc<dyn Fn(TrackPreferences)>,
) -> adw::PreferencesPage {
    let audio_languages = adw::EntryRow::builder()
        .title("Preferred languages")
        .text(format::language_list(&preferences.audio.languages))
        .show_apply_button(true)
        .build();
    let avoid_description = adw::SwitchRow::builder()
        .title("Avoid audio description")
        .subtitle("Narration of what is on screen")
        .active(
            preferences
                .audio
                .avoid
                .contains(&AudioRole::AudioDescription),
        )
        .build();
    let avoid_commentary = adw::SwitchRow::builder()
        .title("Avoid commentary")
        .active(preferences.audio.avoid.contains(&AudioRole::Commentary))
        .build();
    let audio = adw::PreferencesGroup::builder()
        .title("Audio")
        .description(
            "Language codes in order, such as “original, eng”. Empty plays the file's default.",
        )
        .build();
    audio.add(&audio_languages);
    audio.add(&avoid_description);
    audio.add(&avoid_commentary);

    let mode = adw::ComboRow::builder()
        .title("Show subtitles")
        .model(&gtk::StringList::new(&MODES.map(|(_, name)| name)))
        .selected(
            MODES
                .iter()
                .position(|(m, _)| *m == preferences.subtitles.mode)
                .unwrap_or(0) as u32,
        )
        .build();
    let subtitle_languages = adw::EntryRow::builder()
        .title("Preferred languages")
        .text(format::language_list(&preferences.subtitles.languages))
        .show_apply_button(true)
        .build();
    let sdh = adw::ComboRow::builder()
        .title("SDH (for deaf and hard of hearing)")
        .model(&gtk::StringList::new(&SDH.map(|(_, name)| name)))
        .selected(
            SDH.iter()
                .position(|(s, _)| *s == preferences.subtitles.sdh)
                .unwrap_or(0) as u32,
        )
        .build();
    let subtitles = adw::PreferencesGroup::builder()
        .title("Subtitles")
        .description("Empty languages: the audio's language.")
        .build();
    subtitles.add(&mode);
    subtitles.add(&subtitle_languages);
    subtitles.add(&sdh);

    let rows = PlaybackRows {
        original: preferences,
        audio_languages,
        avoid_description,
        avoid_commentary,
        mode,
        subtitle_languages,
        sdh,
    };
    let save = {
        let rows = rows.clone();
        Rc::new(move || set_tracks(rows.preferences()))
    };
    for entry in [&rows.audio_languages, &rows.subtitle_languages] {
        let save = save.clone();
        entry.connect_apply(move |_| save());
    }
    for switch in [&rows.avoid_description, &rows.avoid_commentary] {
        let save = save.clone();
        switch.connect_active_notify(move |_| save());
    }
    for combo in [&rows.mode, &rows.sdh] {
        let save = save.clone();
        combo.connect_selected_notify(move |_| save());
    }

    let page = adw::PreferencesPage::builder()
        .title("Playback")
        .icon_name(crate::icons::PLAY)
        .description("Kept on the server, for all your devices.")
        .build();
    page.add(&audio);
    page.add(&subtitles);
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_setting_is_no_limit() {
        assert_eq!(BITRATE_LIMITS[nearest_limit(None)], None);
    }

    #[test]
    fn a_hand_edited_limit_shows_as_the_nearest_offered() {
        assert_eq!(
            BITRATE_LIMITS[nearest_limit(Some(9_000_000))],
            Some(8_000_000)
        );
    }

    #[test]
    fn a_limit_is_named_in_megabits() {
        assert_eq!(bitrate_name(Some(12_000_000)), "12 Mbit/s");
    }
}
