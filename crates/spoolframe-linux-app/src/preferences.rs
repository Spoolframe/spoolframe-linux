//! The Preferences dialog, as on the LinuxPreferences and LinuxPrefs*
//! boards: Appearance (the theme and the player's behaviour, on this
//! device), Playback (the bitrate limit and skipping intros on this
//! device, P2-06 and P2-10, and, when signed in, the user's audio and
//! subtitle preferences, kept on the server for every device, P1-20),
//! Servers (the fingerprint of the server in use, P1-21) and Profile (the
//! PIN and where it is asked for, P2-08).

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use spoolframe_linux_core::backend::TrackPreferencesInfo;
use spoolframe_linux_core::backend::{Client, ConnectionPath, Server};
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

pub fn dialog(
    theme: Theme,
    tracks: Option<TrackPreferencesInfo>,
    servers: Servers,
    set_theme: Rc<dyn Fn(Theme)>,
    set_tracks: SetTracks,
    device: Device,
    signed: Option<crate::profile_page::Signed>,
) -> adw::Dialog {
    // The pages under the board's own tab strip, not libadwaita's
    // switcher: four 120×52 tabs, icon over label, the picked one on the
    // surface. Toasts (the profile page's answers) show over the pages.
    let toasts = adw::ToastOverlay::new();
    let mut pages = vec![
        appearance(
            theme,
            set_theme,
            device.show_diagnostics,
            device.set_show_diagnostics.clone(),
        ),
        playback(tracks.map(|tracks| (tracks, set_tracks)), &device),
        servers_page(servers),
    ];
    if let Some(signed) = signed {
        pages.push(crate::profile_page::page(signed, &toasts));
    }
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vexpand(true)
        .build();
    let tabs = gtk::Box::builder()
        .spacing(4)
        .halign(gtk::Align::Center)
        .css_classes(["pref-tabs"])
        .build();
    let mut first: Option<gtk::ToggleButton> = None;
    for page in &pages {
        stack.add_child(page);
        let face = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .valign(gtk::Align::Center)
            .build();
        face.append(&gtk::Image::from_icon_name(
            page.icon_name().as_deref().unwrap_or_default(),
        ));
        face.append(&gtk::Label::new(Some(&page.title())));
        let tab = gtk::ToggleButton::builder()
            .child(&face)
            .width_request(120)
            .hexpand(false)
            .css_classes(["pref-tab"])
            .build();
        match &first {
            Some(first) => tab.set_group(Some(first)),
            None => {
                tab.set_active(true);
                first = Some(tab.clone());
            }
        }
        let (stack, page) = (stack.clone(), page.clone());
        tab.connect_toggled(move |tab| {
            if tab.is_active() {
                stack.set_visible_child(&page);
            }
        });
        tabs.append(&tab);
    }
    toasts.set_child(Some(&stack));
    // The strip's colour runs the dialog's width; the tabs sit centred.
    let tab_bar = gtk::Box::builder()
        .hexpand(true)
        .css_classes(["pref-tabs-bar"])
        .build();
    tabs.set_hexpand(true);
    tab_bar.append(&tabs);
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .title_widget(
            &gtk::Label::builder()
                .label("Preferences")
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["title", "pref-title"])
                .build(),
        )
        .css_classes(["pref-header"])
        .build();
    let toolbar = crate::views::toolbar(&header, &[tab_bar.upcast_ref()], &toasts);
    adw::Dialog::builder()
        .title("Preferences")
        .content_width(720)
        .content_height(680)
        .child(&toolbar)
        .build()
}

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
}

/// The server in use, its fingerprint always shown (P1-21) so it can be
/// checked again at any time, and every other saved server, as on the
/// LinuxPrefsServers board.
fn servers_page(servers: Servers) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Servers")
        .icon_name(crate::icons::SERVER)
        .build();
    let servers = Rc::new(servers);
    if let Some(current) = &servers.current {
        page.add(&in_use(current, servers.user.as_deref(), &servers));
    }
    let others = adw::PreferencesGroup::builder()
        .title("Other servers")
        .description(
            "Saved on this computer. Forgetting one signs you out of it and drops its fingerprint.",
        )
        .build();
    let add = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::ADD)
                .label("Add a server…")
                .build(),
        )
        .valign(gtk::Align::End)
        .css_classes(["row-button"])
        .build();
    {
        let servers = servers.clone();
        add.connect_clicked(move |button| {
            close_dialog(button);
            (servers.add)();
        });
    }
    others.set_header_suffix(Some(&add));
    page.add(&others);
    // The list comes from the client, and who is signed in from each.
    {
        let (servers, others) = (servers.clone(), others.clone());
        gtk::glib::spawn_future_local(async move {
            let current = servers.current.as_ref().map(|s| s.key.clone());
            let saved = match servers.client.servers().await {
                Ok(list) => list,
                Err(e) => return tracing::warn!(error = %e, "cannot list servers"),
            };
            for server in saved
                .into_iter()
                .filter(|s| s.pinned && Some(&s.key) != current.as_ref())
            {
                let user = match servers.client.profiles(&server.key).await {
                    Ok(profiles) => profiles.first().map(|p| p.display_name.clone()),
                    Err(_) => None,
                };
                others.add(&other_row(&server, user, &servers, &others));
            }
        });
    }
    page
}

/// "address · signed in as Alice", or "signed out".
fn whereabouts(server: &Server, user: Option<&str>) -> String {
    let signed = match user {
        Some(user) => format!("signed in as {user}"),
        None => "signed out".to_owned(),
    };
    match server.shown_address() {
        Some(address) => format!("{} · {signed}", crate::start::host(address)),
        None => signed,
    }
}

fn in_use(server: &Server, user: Option<&str>, servers: &Rc<Servers>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("In use").build();
    let row = adw::ActionRow::builder().use_markup(false).build();
    row.set_title(&server.name);
    row.set_subtitle(&whereabouts(server, user));
    row.add_prefix(
        &gtk::Image::builder()
            .icon_name(crate::icons::SERVER)
            .width_request(40)
            .height_request(40)
            .css_classes(["server-tile", "tint-accent"])
            .build(),
    );
    row.add_suffix(
        &gtk::Label::builder()
            .label("In use")
            .valign(gtk::Align::Center)
            .css_classes(["in-use-chip"])
            .build(),
    );
    group.add(&row);

    let (icon, tint, state) = if server.verified {
        (crate::icons::VERIFIED, "verified", "Verified")
    } else {
        (crate::icons::UNVERIFIED, "unverified", "Not verified")
    };
    let fingerprint = adw::ActionRow::builder()
        .use_markup(false)
        .subtitle_selectable(true)
        .css_classes(["property", "fingerprint-row", tint])
        .build();
    fingerprint.set_title(&match server.verified_at.filter(|_| server.verified) {
        Some(at) => format!("Fingerprint · {state} {}", crate::start::day(at)),
        None => format!("Fingerprint · {state}"),
    });
    fingerprint.set_subtitle(&server.fingerprint);
    fingerprint.add_prefix(&gtk::Image::from_icon_name(icon));
    let check = gtk::Button::builder()
        .label(if server.verified {
            "Verify again"
        } else {
            "Verify now"
        })
        .valign(gtk::Align::Center)
        .css_classes(["flat", "dialog-text-button"])
        .build();
    {
        let (server, verify) = (server.clone(), servers.verify.clone());
        check.connect_clicked(move |button| {
            close_dialog(button);
            verify(server.clone());
        });
    }
    fingerprint.add_suffix(&check);
    group.add(&fingerprint);
    let keys = crate::start::full_keys(&[&server.key.0]);
    keys.set_halign(gtk::Align::Start);
    keys.set_margin_top(8);
    group.add(&keys);
    if let Some(site) = server.passkey_site() {
        let line = crate::start::passkey_site(&site);
        line.set_margin_top(4);
        group.add(&line);
    }
    // What the server says about where it can be reached (protocol 7),
    // the address in use marked; nothing until it has said.
    if !server.advertised.is_empty() {
        let in_use = match servers.client.connection_path(&server.key) {
            ConnectionPath::Lan(at) | ConnectionPath::Direct(at) => Some(at),
            ConnectionPath::Relay | ConnectionPath::Unknown => None,
        };
        let mut lines = vec![
            format!("Addresses: {}", address_list(&server.advertised, in_use)),
            format!("Relays: {}", relay_list(&server.relays)),
        ];
        if let Some(at) = server.advertised_at {
            lines.push(format!("as of {}", crate::start::day(at)));
        }
        group.add(
            &gtk::Label::builder()
                .label(lines.join("\n"))
                .selectable(true)
                .wrap(true)
                .xalign(0.0)
                .margin_top(4)
                .css_classes(["monospace", "dim-label", "caption"])
                .build(),
        );
    }
    group
}

/// The server's advertised addresses, in its order, the one this
/// connection uses marked.
fn address_list(advertised: &[String], in_use: Option<std::net::SocketAddr>) -> String {
    advertised
        .iter()
        .map(|address| {
            let used = in_use.is_some_and(|at| address.parse() == Ok(at));
            if used {
                format!("{address} (in use)")
            } else {
                address.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The relays the server says it can be reached through, or none.
fn relay_list(relays: &[String]) -> String {
    if relays.is_empty() {
        "none".to_owned()
    } else {
        relays.join(", ")
    }
}

/// Another saved server's line: where it is, who is signed in, where its
/// passkeys are kept (PX-14), and whether its fingerprint was checked.
fn other_line(server: &Server, user: Option<&str>) -> String {
    let state = if server.verified {
        "Verified"
    } else {
        "Not verified"
    };
    match server.passkey_site() {
        Some(site) => format!("{} · passkeys {site} · {state}", whereabouts(server, user)),
        None => format!("{} · {state}", whereabouts(server, user)),
    }
}

/// A saved server not in use: where it is and who is signed in, whether
/// its fingerprint was checked, Use, and a menu to verify, sign out or
/// forget it.
fn other_row(
    server: &Server,
    user: Option<String>,
    servers: &Rc<Servers>,
    group: &adw::PreferencesGroup,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().use_markup(false).build();
    row.set_title(&server.name);
    row.set_subtitle(&other_line(server, user.as_deref()));
    row.add_prefix(
        &gtk::Image::builder()
            .icon_name(if server.verified {
                crate::icons::VERIFIED
            } else {
                crate::icons::UNVERIFIED
            })
            .css_classes(["dim-label"])
            .build(),
    );
    let use_button = gtk::Button::builder()
        .label("Use")
        .valign(gtk::Align::Center)
        .css_classes(["row-button"])
        .build();
    {
        let (server, servers) = (server.clone(), servers.clone());
        use_button.connect_clicked(move |button| {
            close_dialog(button);
            (servers.use_server)(server.clone());
        });
    }
    row.add_suffix(&use_button);

    let verify = {
        let (server, servers, anchor) = (server.clone(), servers.clone(), row.downgrade());
        crate::views::MenuEntry::new("Verify now", crate::icons::VERIFIED, move || {
            if let Some(anchor) = anchor.upgrade() {
                close_dialog(&anchor);
            }
            (servers.verify)(server.clone());
        })
    };
    let sign_out = {
        let (server, servers, row) = (server.clone(), servers.clone(), row.downgrade());
        crate::views::MenuEntry::new("Sign out", crate::icons::SIGN_OUT, move || {
            let (server, client, row) = (server.clone(), servers.client.clone(), row.clone());
            gtk::glib::spawn_future_local(async move {
                match client.sign_out(&server.key).await {
                    Ok(()) => {
                        if let Some(row) = row.upgrade() {
                            row.set_subtitle(&format!(
                                "{} · {}",
                                whereabouts(&server, None),
                                if server.verified {
                                    "Verified"
                                } else {
                                    "Not verified"
                                }
                            ));
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "cannot sign out"),
                }
            });
        })
    };
    let forget = {
        let (server, servers, row, group) = (
            server.clone(),
            servers.clone(),
            row.downgrade(),
            group.downgrade(),
        );
        crate::views::MenuEntry {
            destructive: true,
            ..crate::views::MenuEntry::new("Forget server…", crate::icons::FORGET, move || {
                let Some(anchor) = row.upgrade() else { return };
                let confirm = adw::AlertDialog::builder()
                    .heading(format!("Forget {}?", server.name))
                    .body(
                        "You are signed out of it and its fingerprint is dropped. To use it \
                         again, add it and check its code.",
                    )
                    .default_response("cancel")
                    .close_response("cancel")
                    .build();
                confirm.add_response("cancel", "Cancel");
                confirm.add_response("forget", "Forget");
                confirm.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
                let (server, client, row, group) = (
                    server.clone(),
                    servers.client.clone(),
                    row.clone(),
                    group.clone(),
                );
                confirm.connect_response(Some("forget"), move |_, _| {
                    let (server, client, row, group) =
                        (server.clone(), client.clone(), row.clone(), group.clone());
                    gtk::glib::spawn_future_local(async move {
                        match client.forget(&server.key).await {
                            Ok(()) => {
                                if let (Some(row), Some(group)) = (row.upgrade(), group.upgrade()) {
                                    group.remove(&row);
                                }
                            }
                            Err(e) => tracing::warn!(error = %e, "cannot forget the server"),
                        }
                    });
                });
                confirm.present(Some(&anchor));
            })
        }
    };
    let mut first = vec![verify];
    if user.is_some() {
        first.push(sign_out);
    }
    let menu = crate::views::menu_popover(vec![first, vec![forget]]);
    row.add_suffix(&crate::views::menu_button(
        &menu,
        &format!("More for {}", server.name),
    ));
    row
}

/// Closes the Preferences dialog `widget` is in, so what comes next shows
/// over the window.
fn close_dialog(widget: &impl IsA<gtk::Widget>) {
    if let Some(dialog) = widget.ancestor(adw::Dialog::static_type()) {
        dialog.downcast::<adw::Dialog>().expect("a dialog").close();
    }
}

fn appearance(
    current: Theme,
    set_theme: Rc<dyn Fn(Theme)>,
    show_diagnostics: bool,
    set_show_diagnostics: SetSkipIntros,
) -> adw::PreferencesPage {
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
    let diagnostics = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Show playback diagnostics")
        .subtitle("Decoder, buffer and network details over the video")
        .active(show_diagnostics)
        .build();
    diagnostics.connect_active_notify(move |row| set_show_diagnostics(row.is_active()));
    let behavior = adw::PreferencesGroup::builder().title("Behavior").build();
    behavior.add(&dark_player);
    behavior.add(&diagnostics);

    let page = adw::PreferencesPage::builder()
        .title("Appearance")
        .icon_name(crate::icons::APPEARANCE)
        .build();
    page.add(&group);
    page.add(&behavior);
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
        .title("Bitrate limit")
        .subtitle(
            "Above it, the server converts the video to fit. Applies when Spoolframe next starts.",
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
    pub skip_intros: bool,
    pub set_skip_intros: SetSkipIntros,
    pub show_diagnostics: bool,
    pub set_show_diagnostics: SetSkipIntros,
}

/// The bitrate limit and skipping intros, on this device, and the user's
/// track preferences when the server gave them.
fn playback(
    tracks: Option<(TrackPreferencesInfo, SetTracks)>,
    device: &Device,
) -> adw::PreferencesPage {
    let skip = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Skip intros automatically")
        .subtitle("On this computer only. Otherwise a Skip intro button shows.")
        .active(device.skip_intros)
        .build();
    let set_skip_intros = device.set_skip_intros.clone();
    skip.connect_active_notify(move |row| set_skip_intros(row.is_active()));
    let markers = adw::PreferencesGroup::builder().title("Intros").build();
    markers.add(&skip);

    let page = adw::PreferencesPage::builder()
        .title("Playback")
        .icon_name(crate::icons::PLAYBACK_PAGE)
        .build();
    let streaming = streaming(device.bitrate, device.set_bitrate.clone());
    let hdr = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Show HDR")
        .subtitle(
            "Off: the server converts HDR films to SDR, even for an HDR monitor. \
             Applies when Spoolframe next starts.",
        )
        .active(device.show_hdr)
        .build();
    let set_show_hdr = device.set_show_hdr.clone();
    hdr.connect_active_notify(move |row| set_show_hdr(row.is_active()));
    streaming.add(&hdr);
    page.add(&streaming);
    page.add(&markers);
    if let Some((info, set_tracks)) = tracks {
        let (audio, subtitles) = track_groups(info, set_tracks);
        page.add(&audio);
        page.add(&subtitles);
    }
    page
}

/// Audio and Subtitles, kept on the server for all the user's devices.
fn track_groups(
    info: TrackPreferencesInfo,
    set_tracks: SetTracks,
) -> (adw::PreferencesGroup, adw::PreferencesGroup) {
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
            "Kept on the server, for all your devices. Preferred languages in order, such as “original, eng”: the first one a file has is picked.",
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
    (audio, subtitles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_in_use_is_marked_among_the_advertised() {
        assert_eq!(
            address_list(
                &["10.10.0.6:7430".into(), "192.168.86.10:7430".into()],
                Some("192.168.86.10:7430".parse().unwrap())
            ),
            "10.10.0.6:7430, 192.168.86.10:7430 (in use)"
        );
    }

    #[test]
    fn no_relays_reads_none() {
        assert_eq!(relay_list(&[]), "none");
    }

    #[test]
    fn another_servers_line_names_its_passkey_site() {
        let server = Server {
            key: spoolframe_linux_core::backend::ServerKey("k".into()),
            name: "Cabin".into(),
            addresses: vec!["10.10.0.6:7430".into()],
            pinned: true,
            fingerprint: String::new(),
            verified: true,
            on_network: false,
            verified_at: None,
            last_connected_at: None,
            server_id: Some("q2d2w5nb3kx7m4ra".into()),
            advertised: Vec::new(),
            relays: Vec::new(),
            advertised_at: None,
        };
        assert_eq!(
            other_line(&server, Some("Alice")),
            "10.10.0.6 · signed in as Alice · passkeys q2d2w5nb3kx7m4ra.spoolframe.app · Verified"
        );
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
