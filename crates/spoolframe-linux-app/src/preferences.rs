//! The pages of Settings ([`crate::settings_window`]), as on the
//! LinuxSettingsDevice and LinuxSettingsServer boards: This computer's
//! Appearance, Playback, Connection, Shared computer and About, and each
//! saved server's page.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use spoolframe_linux_core::backend::TrackPreferencesInfo;
use spoolframe_linux_core::backend::{Client, Reachability, Server, ServerKey};
use spoolframe_linux_core::format;
use spoolframe_linux_core::theme::Theme;
use spoolframe_types::{
    AudioRole, BitmapPreference, SdhPreference, SubtitleMode, TrackPreferences,
};

/// When subtitles show, in the order the dialog lists them.
const MODES: [(SubtitleMode, &str); 4] = [
    (SubtitleMode::Off, "Off"),
    (SubtitleMode::ForcedOnly, "Forced only"),
    (
        SubtitleMode::ForeignOnly,
        "When the audio isn't in my language",
    ),
    (SubtitleMode::Always, "Always"),
];

const SDH: [(SdhPreference, &str); 3] = [
    (SdhPreference::Accept, "No preference"),
    (SdhPreference::Prefer, "Prefer SDH"),
    (SdhPreference::Avoid, "Avoid SDH"),
];

/// What the Servers page needs: the client, to list and change saved
/// servers, the one in use and who is signed in on it, and what the app
/// does to verify, use or add one. Those close Preferences first.
pub struct Servers {
    pub client: Arc<dyn Client>,
    pub current: Option<Server>,
    pub user: Option<String>,
    pub verify: Rc<dyn Fn(Server)>,
    pub use_server: Rc<dyn Fn(Server)>,
    pub add: Rc<dyn Fn()>,
    /// A server was forgotten: the app lets go of it if it was in use.
    pub forgotten: Rc<dyn Fn(ServerKey)>,
    /// The profile in use left a server others keep here: the app lets
    /// go of it, back to Who's watching.
    pub left: Rc<dyn Fn(ServerKey)>,
    /// The app's "Use relays", which a server follows unless it has its
    /// own choice.
    pub app_relays: bool,
}

/// A saved server's Relays row: its choice and list, and Relays… to change
/// them; the line is read again when the dialog closes.
fn relays_row(server: &Server, servers: &Rc<Servers>) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title("Relays")
        .build();
    row.set_subtitle(&crate::server_relays::summary(server, servers.app_relays));
    row.add_prefix(&gtk::Image::from_icon_name(crate::icons::BROWSER));
    let open = gtk::Button::builder()
        .label("Relays…")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "dialog-text-button"])
        .build();
    let (server, servers, weak) = (server.clone(), servers.clone(), row.downgrade());
    open.connect_clicked(move |button| open_relays(button, &server, &servers, weak.clone()));
    row.add_suffix(&open);
    row
}

/// The Relays dialog for `server`, over `anchor`; `line`, if given, is set
/// to the server's summary again once it closes.
fn open_relays(
    anchor: &impl IsA<gtk::Widget>,
    server: &Server,
    servers: &Rc<Servers>,
    line: gtk::glib::WeakRef<adw::ActionRow>,
) {
    let dialog = crate::server_relays::dialog(crate::server_relays::Needs {
        client: servers.client.clone(),
        server: server.clone(),
        app_relays: servers.app_relays,
        changed: Rc::new(|| {}),
    });
    let (client, key, app_relays) = (
        servers.client.clone(),
        server.key.clone(),
        servers.app_relays,
    );
    dialog.connect_closed(move |_| {
        let (client, key, line) = (client.clone(), key.clone(), line.clone());
        gtk::glib::spawn_future_local(async move {
            let Ok(saved) = client.servers().await else {
                return;
            };
            if let (Some(server), Some(line)) =
                (saved.into_iter().find(|s| s.key == key), line.upgrade())
            {
                line.set_subtitle(&crate::server_relays::summary(&server, app_relays));
            }
        });
    });
    dialog.present(Some(anchor));
}

/// This computer's own choices, for every server: whether people with
/// their own identity may sign in here (LinuxPrefsPortablePolicy).
fn this_computer(identities: spoolframe_linux_core::identity::Identities) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("This computer")
        .description("Applies to every server and every profile here.")
        .build();
    let guests = adw::SwitchRow::builder()
        .use_markup(false)
        .sensitive(false)
        .build();
    guests.set_title("Sign-ins with other identities");
    guests.set_subtitle(
        "People who watch with their own identity can sign in here with a code. Right for a \
         shared living-room PC; leave it off on your own laptop.",
    );
    guests.add_prefix(&gtk::Image::from_icon_name(crate::icons::IDENTITY));
    group.add(&guests);
    let note = gtk::Label::builder()
        .label(
            "Off by default. With it off, this computer shows no code for Sign in with your \
             identity, and refuses before contacting any server.",
        )
        .wrap(true)
        .xalign(0.0)
        .margin_top(12)
        .css_classes(["dim-label", "caption"])
        .build();
    group.add(&note);
    // Read first, then followed: the switch stays off until it is known.
    let weak = guests.downgrade();
    gtk::glib::spawn_future_local(async move {
        let allowed = identities
            .run(|ids| Box::pin(async move { ids.allow_guests().await }))
            .await;
        let Some(guests) = weak.upgrade() else { return };
        match allowed {
            Ok(allowed) => guests.set_active(allowed),
            Err(e) => return tracing::warn!(error = %e, "cannot read the guest sign-in setting"),
        }
        guests.set_sensitive(true);
        guests.connect_active_notify(move |row| {
            let (identities, allowed) = (identities.clone(), row.is_active());
            gtk::glib::spawn_future_local(async move {
                if let Err(e) = identities
                    .run(move |ids| Box::pin(async move { ids.set_allow_guests(allowed).await }))
                    .await
                {
                    tracing::warn!(error = %e, "cannot change the guest sign-in setting");
                }
            });
        });
    });
    group
}

/// Closes Settings, from `widget` in it, so what comes next shows over the
/// app's window.
fn close_dialog(widget: &impl IsA<gtk::Widget>) {
    crate::settings_window::close(widget);
}

/// Appearance: the theme, and the player, which stays dark.
pub(crate) fn appearance(current: Theme, set_theme: Rc<dyn Fn(Theme)>) -> adw::PreferencesPage {
    // The themes as swatches, five across, the picked one ringed.
    let grid = gtk::Box::builder().spacing(12).homogeneous(true).build();
    let hint = gtk::Label::builder()
        .label(theme_hint(current))
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "small-text"])
        .build();
    let mut first: Option<gtk::ToggleButton> = None;
    for theme in Theme::ALL {
        let button = swatch(theme);
        button.set_active(theme == current);
        match &first {
            Some(first) => button.set_group(Some(first)),
            None => first = Some(button.clone()),
        }
        let (set_theme, hint) = (set_theme.clone(), hint.clone());
        button.connect_toggled(move |button| {
            if button.is_active() {
                hint.set_label(&theme_hint(theme));
                set_theme(theme);
            }
        });
        grid.append(&button);
    }
    let themes = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    themes.append(&grid);
    themes.append(&hint);
    let group = adw::PreferencesGroup::builder().title("Theme").build();
    group.add(&themes);

    let dark_player = adw::ActionRow::builder()
        .use_markup(false)
        .title("Dark player")
        .subtitle("The player stays dark with every theme")
        .build();
    dark_player.add_suffix(
        &gtk::Label::builder()
            .label("Always on")
            .valign(gtk::Align::Center)
            .css_classes(["fixed-chip"])
            .build(),
    );
    let player = adw::PreferencesGroup::builder().title("Player").build();
    player.add(&dark_player);

    let page = adw::PreferencesPage::builder()
        .title("Appearance")
        .description("On this computer.")
        .icon_name(crate::icons::APPEARANCE)
        .build();
    page.add(&group);
    page.add(&player);
    page
}

/// What the picked theme does, under the swatches.
fn theme_hint(theme: Theme) -> String {
    match theme {
        Theme::System if crate::theme::desktop_has_preference() => {
            "Uses your desktop's light or dark style and accent color.".to_owned()
        }
        Theme::System => {
            "Your desktop states no light or dark preference, so Follow system uses Mocha."
                .to_owned()
        }
        theme => format!(
            "Catppuccin {} replaces the libadwaita colors. Your desktop's light or dark \
             setting is ignored.",
            theme.name()
        ),
    }
}

/// A theme drawn in miniature, its name under it: a sidebar strip, a line
/// of text and accent bars. "Follow system" is drawn in the colours in use.
fn swatch(theme: Theme) -> gtk::ToggleButton {
    let preview = gtk::DrawingArea::builder()
        .content_height(86)
        .height_request(86)
        .vexpand(false)
        .valign(gtk::Align::Start)
        .hexpand(true)
        .css_classes(["swatch-preview"])
        .build();
    preview.set_draw_func(move |area, cr, width, height| {
        let parse = |hex: &str| gtk::gdk::RGBA::parse(hex).unwrap_or(gtk::gdk::RGBA::BLACK);
        #[allow(deprecated)]
        let style = |name: &str| area.style_context().lookup_color(name);
        let (base, mantle, surface, text, accent) = match theme.swatch() {
            Some(s) => (
                parse(s.base),
                parse(s.mantle),
                parse(s.surface),
                parse(s.text),
                parse(s.accent),
            ),
            None => (
                style("window_bg_color").unwrap_or(gtk::gdk::RGBA::BLACK),
                style("sidebar_bg_color").unwrap_or(gtk::gdk::RGBA::BLACK),
                style("borders").unwrap_or(gtk::gdk::RGBA::BLACK),
                area.color(),
                adw::StyleManager::default().accent_color_rgba(),
            ),
        };
        let _ = paint_swatch(
            cr,
            f64::from(width),
            f64::from(height),
            [base, mantle, surface, text, accent],
        );
    });
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    body.append(&preview);
    body.append(
        &gtk::Label::builder()
            .label(theme.name())
            .css_classes(["swatch-name"])
            .build(),
    );
    gtk::ToggleButton::builder()
        .child(&body)
        .tooltip_text(theme.name())
        .valign(gtk::Align::Start)
        .css_classes(["flat", "swatch"])
        .build()
}

fn paint_swatch(
    cr: &gtk::cairo::Context,
    width: f64,
    height: f64,
    [base, mantle, surface, text, accent]: [gtk::gdk::RGBA; 5],
) -> Result<(), gtk::cairo::Error> {
    let set = |colour: &gtk::gdk::RGBA| {
        cr.set_source_rgba(
            f64::from(colour.red()),
            f64::from(colour.green()),
            f64::from(colour.blue()),
            1.0,
        )
    };
    let rounded = |x: f64, y: f64, w: f64, h: f64, r: f64| {
        use std::f64::consts::{FRAC_PI_2, PI};
        cr.new_sub_path();
        cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
        cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
        cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
        cr.arc(x + r, y + r, r, PI, PI + FRAC_PI_2);
        cr.close_path();
    };
    // The card, clipped to its corners, with a hairline in its border colour.
    rounded(0.5, 0.5, width - 1.0, height - 1.0, 12.0);
    cr.save()?;
    cr.clip_preserve();
    set(&base);
    cr.fill_preserve()?;
    set(&mantle);
    cr.rectangle(0.0, 0.0, width * 0.3, height);
    cr.fill()?;
    let left = width * 0.3 + 10.0;
    let inner = width - left - 10.0;
    set(&text);
    rounded(left, 10.0, inner * 0.7, 8.0, 4.0);
    cr.fill()?;
    set(&accent);
    rounded(left, 24.0, inner * 0.45, 8.0, 4.0);
    cr.fill()?;
    rounded(left, height - 24.0, inner * 0.6, 14.0, 7.0);
    cr.fill()?;
    cr.restore()?;
    rounded(0.5, 0.5, width - 1.0, height - 1.0, 12.0);
    set(&surface);
    cr.set_line_width(1.0);
    cr.stroke()
}

/// The picture-subtitle choices, in the row's order.
const BITMAP: [(BitmapPreference, &str); 2] = [
    (BitmapPreference::Text, "Read as text (OCR)"),
    (BitmapPreference::Pictures, "Show as pictures"),
];

/// The picture-subtitle row as it shows: which choice, and whether it can
/// change. Without OCR on the server only pictures can be.
fn bitmap_row(preference: BitmapPreference, ocr_offered: bool) -> (u32, bool) {
    let shown = if ocr_offered {
        preference
    } else {
        BitmapPreference::Pictures
    };
    let index = BITMAP.iter().position(|(b, _)| *b == shown).unwrap_or(0);
    (index as u32, ocr_offered)
}

/// The picture-subtitle preference to save: the row's, unless the server
/// has no OCR and the row only shows what it does instead; then the
/// user's own choice stays, for their other servers.
fn bitmap_saved(original: BitmapPreference, selected: u32, ocr_offered: bool) -> BitmapPreference {
    match BITMAP.get(selected as usize) {
        Some((choice, _)) if ocr_offered => *choice,
        _ => original,
    }
}

/// Saves a new bitrate limit.
pub type SetBitrate = Rc<dyn Fn(Option<u64>)>;

/// Saves the user's track preferences on the server.
pub type SetTracks = Rc<dyn Fn(TrackPreferences)>;

/// Turns skipping intros on or off.
pub type SetSkipIntros = Rc<dyn Fn(bool)>;

/// The bitrate limits offered, in bits per second; `None` is no limit.
pub const BITRATE_LIMITS: [Option<u64>; 6] = [
    None,
    Some(40_000_000),
    Some(20_000_000),
    Some(8_000_000),
    Some(4_000_000),
    Some(2_000_000),
];

/// "Original quality", or a limit with what it carries: "8 Mbps · 1080p".
pub fn bitrate_name(limit: Option<u64>) -> String {
    match limit {
        None => "Original quality".to_owned(),
        Some(bits) => {
            let fits = match bits {
                b if b >= 40_000_000 => " · 4K",
                b if b >= 8_000_000 => " · 1080p",
                b if b >= 4_000_000 => " · 720p",
                _ => " · 480p",
            };
            format!("{} Mbps{fits}", bits / 1_000_000)
        }
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
fn streaming(current: Option<u64>, set: SetBitrate) -> adw::PreferencesGroup {
    let names: Vec<String> = BITRATE_LIMITS.iter().map(|l| bitrate_name(*l)).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .use_markup(false)
        .title("Quality limit")
        .subtitle(
            "This computer only. The server converts anything above it. Applies when Spoolframe \
             next starts.",
        )
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
    group
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
    bitmap: adw::ComboRow,
    ocr_offered: bool,
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
        preferences.subtitles.bitmap = bitmap_saved(
            self.original.subtitles.bitmap,
            self.bitmap.selected(),
            self.ocr_offered,
        );
        preferences
    }
}

/// Settings kept on this device, as they are now and how to change them.
pub struct Device {
    pub bitrate: Option<u64>,
    pub set_bitrate: SetBitrate,
    pub show_hdr: bool,
    pub set_show_hdr: SetSkipIntros,
    pub use_relays: bool,
    pub set_use_relays: SetSkipIntros,
    pub skip_intros: bool,
    pub set_skip_intros: SetSkipIntros,
    pub show_diagnostics: bool,
    pub set_show_diagnostics: SetSkipIntros,
}

/// Playback, on this computer for every server and profile: the quality
/// limit and HDR, then what happens while playing. Audio and subtitle
/// languages are the profile's, on each server's page.
pub(crate) fn playback(device: &Device) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Playback")
        .description("On this computer, for every server and profile.")
        .icon_name(crate::icons::PLAYBACK_PAGE)
        .build();
    let streaming = streaming(device.bitrate, device.set_bitrate.clone());
    let hdr = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Show HDR")
        .subtitle("Off: the server converts HDR to SDR, even for an HDR monitor.")
        .active(device.show_hdr)
        .build();
    let set_show_hdr = device.set_show_hdr.clone();
    hdr.connect_active_notify(move |row| set_show_hdr(row.is_active()));
    streaming.add(&hdr);
    page.add(&streaming);

    let skip = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Skip intros automatically")
        .subtitle("When the server knows where the intro is.")
        .active(device.skip_intros)
        .build();
    let set_skip_intros = device.set_skip_intros.clone();
    skip.connect_active_notify(move |row| set_skip_intros(row.is_active()));
    let diagnostics = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Show playback diagnostics")
        .subtitle("Decoder, buffer and network over the video.")
        .active(device.show_diagnostics)
        .build();
    let set_show_diagnostics = device.set_show_diagnostics.clone();
    diagnostics.connect_active_notify(move |row| set_show_diagnostics(row.is_active()));
    let playing = adw::PreferencesGroup::builder()
        .title("While playing")
        .description(
            "Audio and subtitle languages belong to your profile: they’re on each server’s page.",
        )
        .build();
    playing.add(&skip);
    playing.add(&diagnostics);
    page.add(&playing);
    page
}

/// Connection: this computer's Use relays, the default every server
/// follows unless it has its own choice.
pub(crate) fn connection(device: &Device) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Connection")
        .description("On this computer.")
        .icon_name(crate::icons::CONNECTION)
        .build();
    let relays = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Use relays")
        .subtitle("Reach a server through its relays when it can’t be reached directly.")
        .active(device.use_relays)
        .build();
    let set_use_relays = device.set_use_relays.clone();
    relays.connect_active_notify(move |row| set_use_relays(row.is_active()));
    let group = adw::PreferencesGroup::builder()
        .title("Relays")
        .description(
            "This computer’s default. Each server can choose its own in its page. Applies when \
             Spoolframe next starts.",
        )
        .build();
    group.add(&relays);
    page.add(&group);
    page
}

/// Shared computer: whether people with their own identity may sign in
/// here.
pub(crate) fn shared(
    identities: spoolframe_linux_core::identity::Identities,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Shared computer")
        .description("On this computer.")
        .icon_name(crate::icons::SHARED)
        .build();
    page.add(&this_computer(identities.clone()));
    let signed_in = adw::PreferencesGroup::builder()
        .title("Signed in here with their identity")
        .description("Signing one out changes nothing on the server.")
        .build();
    page.add(&signed_in);
    list_guests(identities, &signed_in, Rc::default());
    page
}

/// A guest's name: what this computer calls them, else their fingerprint.
pub fn guest_title(guest: &spoolframe_linux_core::identity::Guest) -> String {
    guest
        .name
        .clone()
        .unwrap_or_else(|| format!("Identity {}", guest.id.fingerprint()))
}

/// What a guest's row says under their name: "Signed in 8 Oct with their
/// identity · Home Media", each part left out when it is not known.
/// `day` is when they were paired here, as [`crate::start::day`] says it.
pub fn guest_line(day: Option<&str>, server: Option<&str>) -> String {
    let signed_in = match day {
        Some(day) => format!("Signed in {day} with their identity"),
        None => "Signed in with their identity".to_owned(),
    };
    match server {
        Some(server) => format!("{signed_in} · {server}"),
        None => signed_in,
    }
}

/// The rows Shared computer lists its guests in.
type GuestRows = Rc<std::cell::RefCell<Vec<adw::ActionRow>>>;

/// Fills `group` with who is signed in here with their identity, each
/// with Sign out, in place of `rows`; listed again after one goes.
fn list_guests(
    identities: spoolframe_linux_core::identity::Identities,
    group: &adw::PreferencesGroup,
    rows: GuestRows,
) {
    let group = group.downgrade();
    gtk::glib::spawn_future_local(async move {
        let listed = identities
            .run(|ids| Box::pin(async move { ids.guests().await }))
            .await;
        let Some(group) = group.upgrade() else { return };
        for row in rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        let guests = match listed {
            Ok(guests) => guests,
            Err(e) => return tracing::warn!(error = %e, "cannot list the guests here"),
        };
        if guests.is_empty() {
            let row = adw::ActionRow::builder().use_markup(false).build();
            row.set_title("Nobody");
            row.set_subtitle("No one is signed in here with their own identity.");
            group.add(&row);
            rows.borrow_mut().push(row);
        }
        for guest in guests {
            let row = adw::ActionRow::builder().use_markup(false).build();
            row.set_title(&guest_title(&guest));
            let day = guest.paired_at.map(|secs| {
                crate::start::day(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            });
            row.set_subtitle(&guest_line(day.as_deref(), guest.server.as_deref()));
            // As Your identity marks a portable one.
            row.add_suffix(
                &gtk::Label::builder()
                    .label("Portable")
                    .valign(gtk::Align::Center)
                    .css_classes(["chip", "invite-chip", "invite-identity"])
                    .build(),
            );
            let sign_out = gtk::Button::builder()
                .label("Sign out")
                .valign(gtk::Align::Center)
                .css_classes(["row-button"])
                .build();
            let (identities, id, weak, again) = (
                identities.clone(),
                guest.id,
                group.downgrade(),
                rows.clone(),
            );
            sign_out.connect_clicked(move |button| {
                button.set_sensitive(false);
                let (identities, weak, again) = (identities.clone(), weak.clone(), again.clone());
                gtk::glib::spawn_future_local(async move {
                    let forgot = identities
                        .run(move |ids| Box::pin(async move { ids.forget_guest(id).await }))
                        .await;
                    if let Err(e) = forgot {
                        tracing::warn!(error = %e, "cannot sign the guest out");
                    }
                    if let Some(group) = weak.upgrade() {
                        list_guests(identities, &group, again);
                    }
                });
            });
            row.add_suffix(&sign_out);
            group.add(&row);
            rows.borrow_mut().push(row);
        }
    });
}

/// "Version 0.6.1 · protocol 14".
pub fn version_line() -> String {
    format!(
        "Version {} · protocol {}",
        env!("CARGO_PKG_VERSION"),
        spoolframe_protocol::PROTOCOL_VERSION
    )
}

/// About: the version, to copy into a bug report.
pub(crate) fn about() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("About")
        .icon_name(crate::icons::ABOUT)
        .build();
    let row = adw::ActionRow::builder().use_markup(false).build();
    row.set_title("Spoolframe");
    row.set_subtitle(&version_line());
    let copy = gtk::Button::builder()
        .label("Copy")
        .valign(gtk::Align::Center)
        .css_classes(["row-button"])
        .build();
    copy.connect_clicked(|button| button.clipboard().set_text(&version_line()));
    row.add_suffix(&copy);
    let group = adw::PreferencesGroup::new();
    group.add(&row);
    page.add(&group);
    page
}

/// What a server's page shows of the profile signed in to it, when it is
/// the server in use.
pub(crate) struct Signed {
    pub profile: crate::profile_page::Signed,
    pub tracks: Option<(TrackPreferencesInfo, SetTracks)>,
}

/// How a server can be reached now, from its reachability report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A connection is open.
    Connected,
    /// The last dial failed.
    Unreachable,
    /// Not tried in this run, or tried and closed since.
    Unknown,
}

/// What the report says of reaching it.
pub fn reach(report: &Reachability) -> Reach {
    if !report.live.is_empty() {
        Reach::Connected
    } else if report
        .last_dial
        .as_ref()
        .is_some_and(|dial| !dial.connected)
    {
        Reach::Unreachable
    } else {
        Reach::Unknown
    }
}

/// The line under a server's name: where it is and how it is reached
/// ("192.168.1.20 · Direct · LAN"), or when it last answered.
pub fn reach_line(
    server: &Server,
    reach: Reach,
    path: Option<&str>,
    report: Option<&Reachability>,
    now: u64,
) -> String {
    let mut parts: Vec<String> = server
        .shown_address()
        .map(|address| crate::start::host(address).to_owned())
        .into_iter()
        .collect();
    match reach {
        Reach::Connected => parts.extend(path.map(str::to_owned)),
        Reach::Unreachable | Reach::Unknown => {
            if let Some(at) = report.and_then(|r| r.last_connected_at) {
                parts.push(format!(
                    "last connected {}",
                    crate::reachability::ago(at, now)
                ));
            }
        }
    }
    parts.join(" · ")
}

/// "K6EQ-ZXKC-HT6H-AJ4F · Verified Sep 12", or "· Not verified".
pub fn fingerprint_line(server: &Server) -> String {
    match server.verified_at.filter(|_| server.verified) {
        Some(at) => format!(
            "{} · Verified {}",
            server.fingerprint,
            crate::start::day(at)
        ),
        None if server.verified => format!("{} · Verified", server.fingerprint),
        None => format!("{} · Not verified", server.fingerprint),
    }
}

/// What a server's page needs besides the server.
pub(crate) struct ServerPage {
    pub servers: Rc<Servers>,
    /// The profile signed in to it, when it is the server in use.
    pub signed: Option<Signed>,
    /// Forgotten: Settings drops its page and row.
    pub dropped: Rc<dyn Fn()>,
    /// Left by its profile, kept for others: Settings shows it signed out.
    pub signed_out: Rc<dyn Fn()>,
    /// Opens This computer › Your identity, to make one.
    pub open_identity: Rc<dyn Fn()>,
}

/// A saved server's page, as on LinuxSettingsServer: the server, which
/// needs no sign-in, then the profile signed in to it: who, its identity
/// here, audio and subtitles, sign-in and security, Plex, Admin for its
/// admin, and Leave.
pub(crate) fn server_page(
    server: &Server,
    needs: ServerPage,
    toasts: &adw::ToastOverlay,
) -> adw::PreferencesPage {
    // Untitled: Settings heads it with the server's name, as plain text.
    let page = adw::PreferencesPage::new();
    let servers = needs.servers;
    // The name, where it is, and whether it answers.
    let head = adw::PreferencesGroup::new();
    let name = adw::ActionRow::builder().use_markup(false).build();
    name.set_title(&server.name);
    name.add_prefix(
        &gtk::Image::builder()
            .icon_name(crate::icons::SERVER)
            .width_request(40)
            .height_request(40)
            .css_classes(["server-tile", "tint-accent"])
            .build(),
    );
    let chip = gtk::Label::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .css_classes(["status-chip"])
        .build();
    name.add_suffix(&chip);
    head.add(&name);
    page.add(&head);

    let part = adw::PreferencesGroup::builder()
        .title("Server")
        .description("On this computer; no sign-in needed.")
        .build();
    // Can't be reached: what was tried, Details… and Try again.
    let unreachable = adw::ActionRow::builder()
        .use_markup(false)
        .title("Can’t be reached")
        .visible(false)
        .css_classes(["warning"])
        .build();
    let details = gtk::Button::builder()
        .label("Details…")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "dialog-text-button"])
        .build();
    let again = gtk::Button::builder()
        .label("Try again")
        .valign(gtk::Align::Center)
        .css_classes(["row-button"])
        .build();
    unreachable.add_suffix(&details);
    unreachable.add_suffix(&again);
    {
        let (server, servers) = (server.clone(), servers.clone());
        again.connect_clicked(move |button| {
            close_dialog(button);
            (servers.use_server)(server.clone());
        });
    }
    part.add(&unreachable);

    let fingerprint = adw::ActionRow::builder()
        .use_markup(false)
        .title("Fingerprint")
        .subtitle_selectable(true)
        .build();
    fingerprint.set_subtitle(&fingerprint_line(server));
    let verify = gtk::Button::builder()
        .label(if server.verified {
            "Verify again"
        } else {
            "Verify now"
        })
        .valign(gtk::Align::Center)
        .css_classes(["flat", "dialog-text-button"])
        .build();
    {
        let (server, verify_with) = (server.clone(), servers.verify.clone());
        verify.connect_clicked(move |button| {
            close_dialog(button);
            verify_with(server.clone());
        });
    }
    fingerprint.add_suffix(&verify);
    part.add(&fingerprint);
    part.add(&relays_row(server, &servers));
    part.add(&forget_row(server, &servers, needs.dropped.clone()));
    page.add(&part);

    // How it is reached, from its report: asked, never dialled.
    {
        let (client, key, server_c) = (servers.client.clone(), server.key.clone(), server.clone());
        let (name, chip, unreachable, details) = (
            name.downgrade(),
            chip.downgrade(),
            unreachable.downgrade(),
            details.downgrade(),
        );
        let servers = servers.clone();
        gtk::glib::spawn_future_local(async move {
            let report = client.reachability(&key).await.ok();
            let path = format::path_chip(client.connection_path(&key));
            let (Some(name), Some(chip), Some(unreachable), Some(details)) = (
                name.upgrade(),
                chip.upgrade(),
                unreachable.upgrade(),
                details.upgrade(),
            ) else {
                return;
            };
            let state = report.as_ref().map_or(Reach::Unknown, reach);
            let now = crate::invite::unix_now().max(0) as u64;
            name.set_subtitle(&reach_line(&server_c, state, path, report.as_ref(), now));
            match state {
                Reach::Connected => {
                    chip.set_label("Connected");
                    chip.set_visible(true);
                }
                Reach::Unreachable => {
                    chip.set_label("Can’t be reached");
                    chip.set_visible(true);
                    if let Some(report) = report {
                        unreachable.set_subtitle(&crate::reachability::tried_line(&report, now));
                        unreachable.set_visible(true);
                        let server = server_c.clone();
                        details.connect_clicked(move |button| {
                            let relays: Rc<dyn Fn()> = {
                                let (button, server, servers) =
                                    (button.downgrade(), server.clone(), servers.clone());
                                Rc::new(move || {
                                    if let Some(button) = button.upgrade() {
                                        open_relays(
                                            &button,
                                            &server,
                                            &servers,
                                            gtk::glib::WeakRef::new(),
                                        );
                                    }
                                })
                            };
                            let try_again: Rc<dyn Fn()> = {
                                let (button, server, servers) =
                                    (button.downgrade(), server.clone(), servers.clone());
                                Rc::new(move || {
                                    if let Some(button) = button.upgrade() {
                                        close_dialog(&button);
                                    }
                                    (servers.use_server)(server.clone());
                                })
                            };
                            crate::reachability::dialog(
                                &report,
                                servers.app_relays,
                                crate::reachability::Actions { relays, try_again },
                            )
                            .present(Some(button));
                        });
                    }
                }
                Reach::Unknown => {}
            }
        });
    }

    let in_use = servers.current.as_ref().map(|s| &s.key) == Some(&server.key);
    match needs.signed.filter(|_| in_use) {
        Some(signed) => {
            let admin = signed
                .profile
                .session
                .is_admin()
                .then(|| crate::admin_invites::Admin {
                    session: signed.profile.session.clone(),
                    server: signed.profile.server.clone(),
                    username: signed.profile.username.clone(),
                });
            // In the board's order: who, your identity here, audio and
            // subtitles, sign-in and security, Plex, admin, then leave.
            let identity = crate::identity_here::group(
                crate::identity_here::Needs {
                    session: signed.profile.session.clone(),
                    identities: servers.client.identities(),
                    server_name: signed.profile.server_name.clone(),
                    username: signed.profile.username.clone(),
                    open_identity: needs.open_identity,
                },
                toasts,
            );
            let leave = crate::leave_server::group(
                crate::leave_server::Needs {
                    session: signed.profile.session.clone(),
                    client: servers.client.clone(),
                    server: server.key.clone(),
                    server_name: signed.profile.server_name.clone(),
                    username: signed.profile.username.clone(),
                    dropped: needs.dropped.clone(),
                    forgotten: servers.forgotten.clone(),
                    signed_out: needs.signed_out.clone(),
                    left: servers.left.clone(),
                },
                toasts,
            );
            let parts = crate::profile_page::parts(signed.profile, toasts);
            page.add(&parts.header);
            page.add(&identity);
            if let Some((info, set_tracks)) = signed.tracks {
                for group in track_groups(info, set_tracks) {
                    page.add(&group);
                }
            }
            for group in parts.security.iter().chain(&parts.plex) {
                page.add(group);
            }
            if let Some(admin) = admin {
                crate::admin_invites::fill(&page, admin, toasts);
            }
            page.add(&leave);
        }
        None => page.add(&profiles_here(server, &servers)),
    }
    page
}

/// "Forget this server", with its confirmation; `dropped` once it is.
fn forget_row(server: &Server, servers: &Rc<Servers>, dropped: Rc<dyn Fn()>) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title("Forget this server")
        .subtitle("Signs every profile out of it here and drops its fingerprint.")
        .build();
    let forget = gtk::Button::builder()
        .label("Forget…")
        .valign(gtk::Align::Center)
        .css_classes(["row-button", "destructive-action"])
        .build();
    let (server, servers) = (server.clone(), servers.clone());
    forget.connect_clicked(move |button| {
        let confirm = adw::AlertDialog::builder()
            .heading(format!("Forget {}?", server.name))
            .body(
                "Every profile here is signed out of it and its fingerprint is dropped. To use it \
                 again, add it and check its code.",
            )
            .default_response("cancel")
            .close_response("cancel")
            .build();
        confirm.add_response("cancel", "Cancel");
        confirm.add_response("forget", "Forget");
        confirm.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
        let (server, servers, dropped) = (server.clone(), servers.clone(), dropped.clone());
        confirm.connect_response(Some("forget"), move |_, _| {
            let (server, servers, dropped) = (server.clone(), servers.clone(), dropped.clone());
            gtk::glib::spawn_future_local(async move {
                match servers.client.forget(&server.key).await {
                    Ok(()) => {
                        dropped();
                        (servers.forgotten)(server.key.clone());
                    }
                    Err(e) => tracing::warn!(error = %e, "cannot forget the server"),
                }
            });
        });
        confirm.present(Some(button));
    });
    row.add_suffix(&forget);
    row
}

/// The profile part of a server this computer is not using: the profiles
/// it remembers or is signed in as there, and Sign in.
fn profiles_here(server: &Server, servers: &Rc<Servers>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Profile")
        .description("Not in use on this computer.")
        .build();
    let sign_in = adw::ActionRow::builder().use_markup(false).build();
    sign_in.set_title(&format!("Sign in to {}…", server.name));
    let open = gtk::Button::builder()
        .label("Sign in…")
        .valign(gtk::Align::Center)
        .css_classes(["row-button", "suggested-action"])
        .build();
    {
        let (server, servers) = (server.clone(), servers.clone());
        open.connect_clicked(move |button| {
            close_dialog(button);
            (servers.use_server)(server.clone());
        });
    }
    sign_in.add_suffix(&open);
    group.add(&sign_in);
    // Who: signed in there, or remembered on this computer.
    let (client, key, weak) = (
        servers.client.clone(),
        server.key.clone(),
        group.downgrade(),
    );
    gtk::glib::spawn_future_local(async move {
        let profiles = client.profiles(&key).await.unwrap_or_default();
        let Some(group) = weak.upgrade() else { return };
        if profiles.is_empty() {
            group.set_description(Some("Not signed in."));
        } else {
            group.set_description(Some("Profiles on this computer:"));
        }
        for profile in profiles {
            let row = adw::ActionRow::builder().use_markup(false).build();
            row.set_title(&profile.display_name);
            row.set_subtitle(crate::start::profile_caption(&profile).1);
            row.add_prefix(&adw::Avatar::new(32, Some(&profile.display_name), true));
            group.add(&row);
        }
    });
    group
}

/// "Audio & subtitles", kept on the server for all the user's devices,
/// then its Audio and Subtitles groups.
fn track_groups(info: TrackPreferencesInfo, set_tracks: SetTracks) -> [adw::PreferencesGroup; 3] {
    let TrackPreferencesInfo {
        preferences,
        ocr_offered,
    } = info;
    let audio_languages = adw::EntryRow::builder()
        .use_markup(false)
        .title("Preferred languages")
        .text(format::language_list(&preferences.audio.languages))
        .show_apply_button(true)
        .build();
    let avoid_description = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Avoid audio description")
        .subtitle("Unless it is the only track")
        .active(
            preferences
                .audio
                .avoid
                .contains(&AudioRole::AudioDescription),
        )
        .build();
    let avoid_commentary = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Avoid commentary")
        .subtitle("Unless it is the only track")
        .active(preferences.audio.avoid.contains(&AudioRole::Commentary))
        .build();
    let audio = adw::PreferencesGroup::builder()
        .title("Audio")
        .description(
            "Preferred languages in order, such as “original, eng”: the first one a file has is picked.",
        )
        .build();
    audio.add(&audio_languages);
    audio.add(&avoid_description);
    audio.add(&avoid_commentary);

    let mode = adw::ComboRow::builder()
        .use_markup(false)
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
        .use_markup(false)
        .title("Preferred languages")
        .text(format::language_list(&preferences.subtitles.languages))
        .show_apply_button(true)
        .build();
    let sdh = adw::ComboRow::builder()
        .use_markup(false)
        .title("Subtitles for the deaf (SDH)")
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
    // How picture subtitles (PGS, VobSub, DVB) show (PX-13). Without OCR
    // on the server they can only be pictures: the row says so and keeps
    // the user's choice for a server that has it.
    let (selected, sensitive) = bitmap_row(preferences.subtitles.bitmap, ocr_offered);
    let bitmap = adw::ComboRow::builder()
        .use_markup(false)
        .title("Picture subtitles")
        .model(&gtk::StringList::new(&BITMAP.map(|(_, name)| name)))
        .selected(selected)
        .sensitive(sensitive)
        .build();
    bitmap.set_subtitle(if ocr_offered {
        "Subtitles stored as pictures: read as text, or shown as they are"
    } else {
        "This server doesn't read picture subtitles as text"
    });
    subtitles.add(&mode);
    subtitles.add(&subtitle_languages);
    subtitles.add(&sdh);
    subtitles.add(&bitmap);

    let rows = PlaybackRows {
        original: preferences,
        audio_languages,
        avoid_description,
        avoid_commentary,
        mode,
        subtitle_languages,
        sdh,
        bitmap,
        ocr_offered,
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
    for combo in [&rows.mode, &rows.sdh, &rows.bitmap] {
        let save = save.clone();
        combo.connect_selected_notify(move |_| save());
    }
    // A heading over both: a group with no rows shows only its title.
    let section = adw::PreferencesGroup::builder()
        .title("Audio &amp; subtitles")
        .description("Kept on the server for all your devices.")
        .build();
    [section, audio, subtitles]
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::reachability::tests::{NOW, cabin};

    fn cabin_server() -> Server {
        Server {
            key: ServerKey("c4b1".repeat(16)),
            name: "Cabin".into(),
            addresses: vec!["10.10.0.6:7430".into()],
            pinned: true,
            fingerprint: "Q6D2-W5NB-3KX7-M4RA".into(),
            verified: true,
            on_network: false,
            verified_at: None,
            last_connected_at: None,
            server_id: None,
            advertised: Vec::new(),
            relays: Vec::new(),
            advertised_at: None,
            use_relays: None,
            relays_edited: false,
        }
    }

    #[test]
    fn a_server_whose_last_dial_failed_cannot_be_reached() {
        assert_eq!(reach(&cabin()), Reach::Unreachable);
    }

    #[test]
    fn a_server_not_dialled_yet_is_not_called_unreachable() {
        let report = Reachability {
            last_dial: None,
            ..cabin()
        };
        assert_eq!(reach(&report), Reach::Unknown);
    }

    #[test]
    fn an_unreachable_servers_line_says_when_it_last_answered() {
        let report = cabin();
        let server = cabin_server();
        assert!(
            reach_line(&server, Reach::Unreachable, None, Some(&report), NOW)
                .ends_with("last connected 3 days ago"),
            "{}",
            reach_line(&server, Reach::Unreachable, None, Some(&report), NOW)
        );
    }

    #[test]
    fn a_fingerprint_not_checked_says_so() {
        let server = Server {
            verified: false,
            ..cabin_server()
        };
        assert!(fingerprint_line(&server).ends_with(" · Not verified"));
    }

    #[test]
    fn a_guest_with_no_name_here_goes_by_their_fingerprint() {
        let id = spoolframe_protocol::portable::PortableUserId::from_bytes([7; 32]).unwrap();
        let guest = spoolframe_linux_core::identity::Guest {
            id,
            name: None,
            server: None,
            paired_at: None,
        };
        assert_eq!(
            guest_title(&guest),
            format!("Identity {}", id.fingerprint())
        );
    }

    #[test]
    fn a_guest_line_says_when_and_for_which_server() {
        assert_eq!(
            guest_line(Some("8 Oct"), Some("Home Media")),
            "Signed in 8 Oct with their identity · Home Media"
        );
    }

    #[test]
    fn a_guest_line_with_no_server_ends_at_the_identity() {
        assert_eq!(
            guest_line(Some("8 Oct"), None),
            "Signed in 8 Oct with their identity"
        );
    }

    #[test]
    fn a_guest_line_with_no_date_leaves_it_out() {
        assert_eq!(
            guest_line(None, Some("Home Media")),
            "Signed in with their identity · Home Media"
        );
    }

    #[test]
    fn a_guest_line_with_neither_says_only_how() {
        assert_eq!(guest_line(None, None), "Signed in with their identity");
    }

    #[test]
    fn without_ocr_the_choice_shows_pictures_and_cannot_change() {
        assert_eq!(bitmap_row(BitmapPreference::Text, false), (1, false));
    }

    #[test]
    fn with_ocr_the_users_choice_shows() {
        assert_eq!(bitmap_row(BitmapPreference::Pictures, true), (1, true));
    }

    #[test]
    fn saving_without_ocr_keeps_the_users_choice() {
        assert_eq!(
            bitmap_saved(BitmapPreference::Text, 1, false),
            BitmapPreference::Text
        );
    }

    #[test]
    fn saving_with_ocr_takes_the_row() {
        assert_eq!(
            bitmap_saved(BitmapPreference::Text, 1, true),
            BitmapPreference::Pictures
        );
    }

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
    fn a_limit_is_named_with_what_it_carries() {
        assert_eq!(bitrate_name(Some(8_000_000)), "8 Mbps · 1080p");
    }
}
