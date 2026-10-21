//! The screens before the library: choose a server, choose who is
//! watching, sign in. Laid out as in the UI mockups.

use std::rc::Rc;

use adw::prelude::*;
use spoolframe_linux_core::backend::{Profile, ProfileSignIn, Server};

use crate::markup::{Markup, markup};
use crate::remote_sign_in::Via;

pub type Choose<T> = Rc<dyn Fn(T)>;

/// "Choose a server": servers on this network, then saved ones, and a way
/// to add one the network does not show. Laid out as the design's server
/// list: a card of nearby servers, each saved one a card of its own.
/// A saved server the last try could not reach, for Choose a server's
/// banner (LinuxReachability): what was tried, Details… and Try again.
pub struct Unreachable {
    pub key: spoolframe_linux_core::backend::ServerKey,
    pub name: String,
    /// What was tried and when it last worked.
    pub line: String,
    pub details: Rc<dyn Fn()>,
    pub try_again: Rc<dyn Fn()>,
    /// The Downloads page with no server (PX-38), while this computer has
    /// any: they still play.
    pub downloads: Option<Rc<dyn Fn()>>,
}

/// The banner over the saved servers: which one can't be reached, what
/// was tried, and the way on.
fn unreachable_banner(unreachable: &Unreachable) -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["card", "unreachable-banner"])
        .build();
    let heading = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["heading"])
        .build();
    heading.set_text(&format!("{} can’t be reached", unreachable.name));
    let line = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    line.set_text(&unreachable.line);
    let details = gtk::Button::builder().label("Details…").build();
    let try_again = gtk::Button::builder()
        .label("Try again")
        .css_classes(["suggested-action"])
        .build();
    let open = unreachable.details.clone();
    details.connect_clicked(move |_| open());
    let again = unreachable.try_again.clone();
    try_again.connect_clicked(move |_| again());
    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    if let Some(open) = unreachable.downloads.clone() {
        let downloads = gtk::Button::builder().label("Downloads").build();
        downloads.connect_clicked(move |_| open());
        buttons.append(&downloads);
    }
    buttons.append(&details);
    buttons.append(&try_again);
    card.append(&heading);
    card.append(&line);
    card.append(&buttons);
    card
}

pub fn servers(
    servers: &[Server],
    choose: &Choose<Server>,
    verify: &Choose<Server>,
    add: &Rc<dyn Fn()>,
    (relays, set_relays): (bool, &Rc<dyn Fn(bool)>),
    unreachable: Option<&Unreachable>,
    downloads: Option<&Rc<dyn Fn()>>,
) -> gtk::Widget {
    let (clamp, column) = column();
    column.append(&crate::logo::mark(72));
    column.append(&title("Choose a server"));
    column.append(&dim("Servers on this network appear here on their own."));

    let nearby: Vec<&Server> = servers.iter().filter(|s| s.on_network).collect();
    if !nearby.is_empty() {
        let list = boxed_list();
        for server in nearby {
            list.append(&nearby_row(server, choose));
        }
        column.append(&section("On this network", &[list]));
    }
    if let Some(unreachable) = unreachable {
        column.append(&unreachable_banner(unreachable));
    }
    let saved: Vec<gtk::ListBox> = servers
        .iter()
        .filter(|s| s.pinned)
        .map(|server| {
            let list = boxed_list();
            let row = saved_row(server, choose, verify);
            if unreachable.is_some_and(|u| u.key == server.key) {
                row.add_suffix(&badge("Can’t be reached", "unreachable-pill"));
            }
            list.append(&row);
            list
        })
        .collect();
    if !saved.is_empty() {
        column.append(&section("Saved", &saved));
    }
    if let Some(open) = downloads {
        let list = boxed_list();
        list.append(&downloads_row(open));
        column.append(&section("On this computer", &[list]));
    }
    let add_button = gtk::Button::builder()
        .label("Add a server…")
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    let add = add.clone();
    add_button.connect_clicked(move |_| add());
    column.append(&add_button);
    // Here too, so it can be set before signing in; also in Preferences.
    let relays_row = adw::SwitchRow::builder()
        .use_markup(false)
        .title("Use relays")
        .subtitle(
            "Reach a server through its relays when it can't be reached directly. A \
             server's own setting in Settings, on its page, wins.",
        )
        .active(relays)
        .build();
    relays_row.add_prefix(&gtk::Image::from_icon_name(crate::icons::BROWSER));
    let set_relays = set_relays.clone();
    relays_row.connect_active_notify(move |row| set_relays(row.is_active()));
    let relays_list = boxed_list();
    relays_list.append(&relays_row);
    column.append(&section("Connection", &[relays_list]));
    let footer = dim(
        "Iroh proves each server's key when you connect. If a saved server's key \
         changes, Spoolframe asks before trusting the new one.",
    );
    footer.add_css_class("caption");
    column.append(&footer);
    let header = adw::HeaderBar::new();
    header.pack_end(&main_menu());
    page(&header, &clamp)
}

/// The main menu (LinuxSettingsEntry 1): Settings, with no server and no
/// sign-in.
fn main_menu() -> gtk::MenuButton {
    let menu = gtk::gio::Menu::new();
    menu.append(Some("Settings"), Some("win.settings"));
    gtk::MenuButton::builder()
        .icon_name(crate::icons::MENU)
        .tooltip_text("Main menu")
        .menu_model(&menu)
        .primary(true)
        .build()
}

/// The gear in the header of a screen before signing in
/// (LinuxSettingsEntry 2, 3): Settings.
fn settings_gear() -> gtk::Button {
    gtk::Button::builder()
        .icon_name(crate::icons::SET_UP)
        .tooltip_text("Settings")
        .action_name("win.settings")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build()
}

/// A small upper-case heading over its cards.
fn section(heading: &str, cards: &[gtk::ListBox]) -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    section.append(
        &gtk::Label::builder()
            .label(heading.to_uppercase())
            .halign(gtk::Align::Start)
            .css_classes(["caption-heading", "dim-label", "server-section"])
            .build(),
    );
    for card in cards {
        section.append(card);
    }
    section
}

fn boxed_list() -> gtk::ListBox {
    gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build()
}

/// The icon in its rounded tile, at the start of a row.
fn tile(icon: &str, tint: &str) -> gtk::Image {
    gtk::Image::builder()
        .icon_name(icon)
        .width_request(36)
        .height_request(36)
        .valign(gtk::Align::Center)
        .css_classes(["server-tile", tint])
        .build()
}

/// A server on this network: its address and fingerprint, and Connect,
/// filled for one whose fingerprint the user has checked.
fn nearby_row(server: &Server, choose: &Choose<Server>) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .css_classes(["server-row", "server-code"])
        .build();
    row.set_title(&server.name);
    row.set_subtitle(&nearby_subtitle(server));
    row.add_prefix(&tile(
        crate::icons::SERVER,
        if server.verified {
            "tint-accent"
        } else {
            "tint-other"
        },
    ));
    let connect = gtk::Button::builder()
        .label("Connect")
        .valign(gtk::Align::Center)
        .css_classes(if server.verified {
            vec!["server-connect", "suggested-action"]
        } else {
            vec!["server-connect", "outlined"]
        })
        .build();
    // Which of two servers with one name is the saved one.
    if server.pinned {
        row.add_suffix(&badge("Saved", "saved-badge"));
    }
    let (server, choose) = (server.clone(), choose.clone());
    connect.connect_clicked(move |_| choose(server.clone()));
    row.add_suffix(&connect);
    row.set_activatable_widget(Some(&connect));
    row
}

/// This computer's downloads (PX-38), which play with no server: the way
/// to them before any server is chosen, or when none can be reached.
fn downloads_row(open: &Rc<dyn Fn()>) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title("Downloads")
        .subtitle("Films and episodes downloaded here play with no server.")
        .activatable(true)
        .css_classes(["server-row"])
        .build();
    row.add_prefix(&tile(crate::icons::DOWNLOADS, "tint-accent"));
    let button = gtk::Button::builder()
        .label("Open")
        .valign(gtk::Align::Center)
        .css_classes(["server-connect", "outlined"])
        .build();
    let (on_button, on_row) = (open.clone(), open.clone());
    button.connect_clicked(move |_| on_button());
    row.connect_activated(move |_| on_row());
    row.add_suffix(&button);
    row
}

/// A saved server: whether its fingerprint was checked, and if not, a way
/// to check it now.
fn saved_row(server: &Server, choose: &Choose<Server>, verify: &Choose<Server>) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .activatable(true)
        .css_classes(["server-row", "server-code"])
        .build();
    row.set_title(&server.name);
    // Its fingerprint, as the nearby rows show it: with two servers of
    // one name, that is how to tell which was saved. Whether it was
    // checked is the badge's to say.
    row.set_subtitle(&match server.last_connected_at {
        Some(at) => format!("{}  ·  connected {}", nearby_subtitle(server), day(at)),
        None => nearby_subtitle(server),
    });
    if server.verified {
        row.add_prefix(&tile(crate::icons::VERIFIED, "tint-success"));
        let verified = badge("Verified", "pinned-badge");
        if let Some(at) = server.verified_at {
            verified.set_tooltip_text(Some(&format!("Fingerprint checked {}", day(at))));
        }
        row.add_suffix(&verified);
    } else {
        row.add_prefix(&tile(crate::icons::UNVERIFIED, "tint-warning"));
        let check = gtk::Button::builder()
            .label("Verify now")
            .valign(gtk::Align::Center)
            .css_classes(["server-connect", "outlined"])
            .build();
        let (server, verify) = (server.clone(), verify.clone());
        check.connect_clicked(move |_| verify(server.clone()));
        row.add_suffix(&check);
    }
    let (server, choose) = (server.clone(), choose.clone());
    row.connect_activated(move |_| choose(server.clone()));
    row
}

/// A nearby server is told apart by its fingerprint, the code its log
/// prints at start (`server identity … fingerprint=…`), never its raw key,
/// after the host it answered from.
fn nearby_subtitle(server: &Server) -> String {
    match server.shown_address() {
        Some(address) => format!("{}  ·  {}", host(address), server.fingerprint),
        None => server.fingerprint.clone(),
    }
}

/// `10.10.0.6:7440` as `10.10.0.6`, `[fe80::1]:7430` as `fe80::1`.
pub(crate) fn host(address: &str) -> &str {
    match address.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
            host.trim_start_matches('[').trim_end_matches(']')
        }
        _ => address,
    }
}

/// When something happened, as a person says it: "today", "yesterday",
/// "12 Sep", or "12 Sep 2025" in another year, in local time.
pub fn day(at: std::time::SystemTime) -> String {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    match (
        gtk::glib::DateTime::from_unix_local(secs),
        gtk::glib::DateTime::now_local(),
    ) {
        (Ok(at), Ok(now)) => day_between(&at, &now),
        _ => String::new(),
    }
}

fn day_between(at: &gtk::glib::DateTime, now: &gtk::glib::DateTime) -> String {
    let date = |t: &gtk::glib::DateTime| (t.year(), t.day_of_year());
    let yesterday = now.add_days(-1).ok();
    if date(at) == date(now) {
        "today".to_owned()
    } else if yesterday.as_ref().map(date) == Some(date(at)) {
        "yesterday".to_owned()
    } else {
        let pattern = if at.year() == now.year() {
            "%-d %b"
        } else {
            "%-d %b %Y"
        };
        at.format(pattern)
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

fn badge(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .css_classes([class, "caption-heading"])
        .build()
}

/// What the user decides at the fingerprint check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// "It matches": save the server, verified.
    Matches,
    /// "Skip": save it, not verified. When verifying a saved server,
    /// "Not now": leave it as it is.
    Skip,
}

/// "Is this your server?" (P1-21): the server's name and address, its
/// fingerprint large, and how to compare it with the server's own. Closing
/// it (Cancel) decides nothing and saves nothing. `verifying` is for a
/// server saved before as not verified.
pub fn fingerprint_check(
    server: &Server,
    verifying: bool,
    decide: Rc<dyn Fn(Check)>,
) -> adw::Dialog {
    let icon = gtk::Image::builder()
        .icon_name(crate::icons::VERIFIED)
        .pixel_size(30)
        .width_request(56)
        .height_request(56)
        .halign(gtk::Align::Center)
        .css_classes(["check-icon"])
        .build();
    let heading = gtk::Label::builder()
        .label("Is this your server?")
        .css_classes(["title-2", "dialog-title"])
        .build();
    let address = server.shown_address();
    let who = match address {
        Some(address) => markup!("<b>{}</b> · {}", server.name, address),
        None => markup!("<b>{}</b>", server.name),
    };
    let who = gtk::Label::builder()
        .use_markup(true)
        .label(who.escaped())
        .css_classes(["dim-label"])
        .build();
    let code = gtk::Label::builder()
        .label(&server.fingerprint)
        .selectable(true)
        .css_classes(["fingerprint", "monospace"])
        .build();
    let code_column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    code_column.append(&code);
    if let Some(site) = server.passkey_site() {
        code_column.append(&passkey_site(&site));
    }
    let code_box = adw::Bin::builder()
        .child(&code_column)
        .halign(gtk::Align::Center)
        .css_classes(["fingerprint-box"])
        .build();
    let how = gtk::Label::builder()
        .use_markup(true)
        .label(
            "Check that your server shows the same code, in its log or with \
             <tt>spoolframe-server fingerprint</tt>.",
        )
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label", "small-text"])
        .build();
    let full = full_keys(&[&server.key.0]);
    let matches = action(
        "It matches",
        Some(crate::icons::CHECK),
        &["suggested-action"],
    );
    let skip = action(
        if verifying { "Not now" } else { "Skip" },
        None,
        &["secondary"],
    );
    let buttons = gtk::Box::builder()
        .spacing(10)
        .homogeneous(true)
        .margin_start(24)
        .margin_end(24)
        .margin_bottom(if verifying { 22 } else { 0 })
        .build();
    buttons.append(&skip);
    buttons.append(&matches);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(26)
        .margin_bottom(24)
        .margin_start(30)
        .margin_end(30)
        .build();
    for widget in [
        icon.upcast_ref::<gtk::Widget>(),
        heading.upcast_ref(),
        who.upcast_ref(),
        code_box.upcast_ref(),
        how.upcast_ref(),
        full.upcast_ref(),
    ] {
        content.append(widget);
    }
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    body.append(&content);
    body.append(&buttons);
    if !verifying {
        let note = dim("Skip saves the server as Not verified. You can verify it later.");
        note.add_css_class("caption");
        note.set_margin_top(12);
        note.set_margin_bottom(18);
        body.append(&note);
    }
    let dialog = adw::Dialog::builder()
        .title(if verifying {
            "Verify server"
        } else {
            "Add server"
        })
        .content_width(440)
        .build();
    with_cancel(&dialog, &body);
    for (button, choice) in [(&matches, Check::Matches), (&skip, Check::Skip)] {
        let (decide, dialog) = (decide.clone(), dialog.downgrade());
        button.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
            decide(choice);
        });
    }
    dialog
}

/// "Passkeys · <server id>.spoolframe.app" (PX-14): where the server's
/// passkeys are kept, under its fingerprint wherever a server shows.
pub fn passkey_site(site: &str) -> gtk::Label {
    gtk::Label::builder()
        .use_markup(true)
        .label(markup!(
            "<span font_family=\"sans\" weight=\"600\">Passkeys</span> · {}",
            site
        ))
        .xalign(0.0)
        .css_classes(["monospace", "passkey-site", "dim-label"])
        .build()
}

/// A labelled code in an alert: "Previously" over a fingerprint, "Saved
/// server ID" over an id.
fn code_row(label: &str, code: &str, class: &str) -> gtk::Box {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .css_classes(["identity-row"])
        .build();
    column.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .css_classes(["caption-heading", "dim-label"])
            .build(),
    );
    column.append(
        &gtk::Label::builder()
            .label(code)
            .xalign(0.0)
            .selectable(true)
            .css_classes(["monospace", "identity-code", class])
            .build(),
    );
    column
}

/// An alert's shape with the warning mark above the heading, which an
/// AlertDialog cannot carry: the body, `extra` under it, then two
/// responses, the second destructive and running `act`.
fn alert(
    heading: &str,
    body: &Markup,
    extra: &gtk::Box,
    responses: (&str, &str),
    act: Rc<dyn Fn()>,
) -> adw::Dialog {
    let icon = gtk::Image::builder()
        .icon_name(crate::icons::WARNING)
        .pixel_size(40)
        .css_classes(["identity-warning"])
        .build();
    let title = gtk::Label::builder()
        .label(heading)
        .css_classes(["title-3", "dialog-title"])
        .build();
    let text = gtk::Label::builder()
        .use_markup(true)
        .label(body.escaped())
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(28)
        .margin_bottom(22)
        .margin_start(28)
        .margin_end(28)
        .build();
    content.append(&icon);
    content.append(&title);
    content.append(&text);
    content.append(extra);
    let close = gtk::Button::builder()
        .label(responses.0)
        .hexpand(true)
        .css_classes(["flat", "alert-response"])
        .build();
    let action = gtk::Button::builder()
        .label(responses.1)
        .hexpand(true)
        .css_classes(["flat", "alert-response", "destructive"])
        .build();
    let row = gtk::Box::builder()
        .homogeneous(true)
        .css_classes(["alert-responses"])
        .build();
    row.append(&close);
    row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    row.append(&action);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&content);
    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    column.append(&row);
    let dialog = adw::Dialog::builder()
        .title(heading)
        .content_width(460)
        .child(&column)
        .build();
    {
        let dialog = dialog.downgrade();
        close.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
    }
    {
        let dialog = dialog.downgrade();
        action.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
            act();
        });
    }
    dialog.set_focus(Some(&close));
    dialog
}

/// "Server identity changed" (P1-13): a saved server answered with another
/// key. Both codes, the server's passkey site (kept: its id is the same),
/// the full keys on request, and the choice: Cancel (nothing changes) or
/// Trust New Identity (everyone signs in again).
pub fn identity_changed(
    name: &str,
    saved: &str,
    now: &str,
    site: Option<&str>,
    trust: Rc<dyn Fn()>,
) -> adw::Dialog {
    let code =
        |key: &str| spoolframe_protocol::fingerprint::of_hex(key).unwrap_or_else(|| key.to_owned());
    let extra = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    extra.append(&code_row("Previously", &code(saved), "dim-label"));
    extra.append(&code_row("Now", &code(now), "identity-new"));
    if let Some(site) = site {
        extra.append(&passkey_site(site));
    }
    extra.append(&full_keys(&[saved, now]));
    extra.append(
        &gtk::Label::builder()
            .use_markup(true)
            .label(
                "Check the new code against the server's log or \
                 <tt>spoolframe-server fingerprint</tt>. If you trust it, you sign \
                 in to this server again.",
            )
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    alert(
        "Server identity changed",
        &markup!(
            "<b>{}</b> has a different key than the one saved on this computer. \
             This may mean the server was reinstalled, or that you're connecting \
             to a different server.",
            name
        ),
        &extra,
        ("Cancel", "Trust New Identity"),
        trust,
    )
}

/// "Server ID changed" (PX-14): a saved server answered with its key but
/// another server id. It is not trusted as the same server, and there is
/// no accepting it: Close, or Remove server to add it again.
pub fn server_id_changed(name: &str, saved: &str, now: &str, remove: Rc<dyn Fn()>) -> adw::Dialog {
    let extra = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    extra.append(&code_row("Saved server ID", saved, "dim-label"));
    extra.append(&code_row("This server says", now, "identity-new"));
    alert(
        "Server ID changed",
        &markup!(
            "<b>{}</b> has the same key as before, but a different server ID. It can’t be \
             trusted as the same server. Remove it and add it again if this is expected.",
            name
        ),
        &extra,
        ("Close", "Remove server"),
        remove,
    )
}

/// "Show full key(s)": a link that shows the hex keys behind the codes,
/// for whoever wants to compare all of them, in the codes' order.
pub(crate) fn full_keys(keys: &[&str]) -> gtk::Box {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    for key in keys {
        column.append(
            &gtk::Label::builder()
                .label(*key)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::Char)
                .selectable(true)
                .xalign(0.0)
                .css_classes(["monospace", "full-key", "dim-label"])
                .build(),
        );
    }
    let revealer = gtk::Revealer::builder().child(&column).build();
    let (show, hide) = if keys.len() > 1 {
        ("Show full keys", "Hide full keys")
    } else {
        ("Show full key", "Hide full key")
    };
    let link = gtk::Button::builder()
        .label(show)
        .halign(gtk::Align::Center)
        .css_classes(["flat", "text-link"])
        .build();
    {
        let revealer = revealer.clone();
        link.connect_clicked(move |link| {
            let open = !revealer.reveals_child();
            revealer.set_reveal_child(open);
            link.set_label(if open { hide } else { show });
        });
    }
    let block = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    block.append(&link);
    block.append(&revealer);
    block
}

/// Puts `body` in `dialog` under the boards' dialog header: "Cancel" at
/// the start, the title in the middle, no close button.
pub fn with_cancel(dialog: &adw::Dialog, body: &impl IsA<gtk::Widget>) -> adw::HeaderBar {
    let cancel = gtk::Button::builder()
        .label("Cancel")
        .css_classes(["flat", "dialog-cancel"])
        .build();
    let weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });
    let header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .css_classes(["dialog-header"])
        .build();
    header.pack_start(&cancel);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(body));
    dialog.set_child(Some(&toolbar));
    header
}

/// A button as the boards draw them: 44 px, 10 px corners, with an icon
/// when given one.
pub(crate) fn action(label: &str, icon: Option<&str>, classes: &[&str]) -> gtk::Button {
    let button = gtk::Button::builder().hexpand(true).build();
    match icon {
        Some(icon) => button.set_child(Some(
            &adw::ButtonContent::builder()
                .icon_name(icon)
                .label(label)
                .build(),
        )),
        None => button.set_label(label),
    }
    button.add_css_class("action");
    for class in classes {
        button.add_css_class(class);
    }
    button
}

/// What a profile's tile says under its name, and its icon.
pub fn profile_caption(profile: &Profile) -> (Option<&'static str>, &'static str) {
    match (&profile.trust, profile.sign_in) {
        (Some(trust), _) if trust.auto_login => (Some(crate::icons::PLAY), "Opens at start"),
        (_, ProfileSignIn::Password) => (None, "Password"),
        (_, ProfileSignIn::Passkey) => (Some(crate::icons::PASSKEY), "Passkey"),
        (_, ProfileSignIn::Portable(_)) => (Some(crate::icons::IDENTITY), "Your identity"),
        (Some(trust), _) if trust.pin_required || profile.locked => {
            (Some(crate::icons::LOCK), "Remembered · PIN")
        }
        (Some(trust), _) if trust.by_passkey => (None, "Remembered · passkey"),
        (Some(_), _) => (None, "Remembered"),
        (None, _) if profile.locked => (Some(crate::icons::LOCK), "PIN"),
        (None, _) => (None, "Signed in"),
    }
}

/// What a profile's tile does: open it, and its menu's forget and sign out.
pub struct ProfileActions {
    pub choose: Choose<Profile>,
    pub forget: Choose<Profile>,
    pub sign_out: Choose<Profile>,
    /// The password form, for a profile that needs a sign-in again.
    pub sign_in_as: Choose<Profile>,
}

/// The profile that opens when Spoolframe starts, if any (P4-01: one per
/// device at most).
pub fn opens_at_start(profiles: &[Profile]) -> Option<&Profile> {
    profiles
        .iter()
        .find(|p| p.trust.as_ref().is_some_and(|t| t.auto_login))
}

/// "Who's watching?": the users signed in on this device, the ones it
/// remembers (P4-01), and a way to sign in another. Switching between them
/// keeps the connection (§15).
pub fn profiles(
    server: &Server,
    profiles: &[Profile],
    actions: &ProfileActions,
    needs_sign_in: Option<&Profile>,
    sign_in: &Rc<dyn Fn()>,
    back: &Rc<dyn Fn()>,
    path: Rc<dyn Fn() -> Option<&'static str>>,
) -> gtk::Widget {
    // The server, and how it is reached beside it.
    let where_ = gtk::Box::builder().spacing(8).build();
    where_.append(
        &gtk::Label::builder()
            .label(&server.name)
            .css_classes(["title"])
            .build(),
    );
    let chip = gtk::Label::builder()
        .valign(gtk::Align::Center)
        .css_classes(["status-chip"])
        .build();
    let show_path = move |chip: &gtk::Label| {
        let text = path();
        chip.set_visible(text.is_some());
        chip.set_label(text.unwrap_or_default());
    };
    show_path(&chip);
    // The connection comes up, or moves, while the screen shows; asking
    // dials nothing, so it is asked again each second until it goes.
    {
        let weak = chip.downgrade();
        gtk::glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(chip) => {
                show_path(&chip);
                gtk::glib::ControlFlow::Continue
            }
            None => gtk::glib::ControlFlow::Break,
        });
    }
    where_.append(&chip);
    let header = adw::HeaderBar::builder().title_widget(&where_).build();
    header.pack_end(&settings_gear());
    let back_button = gtk::Button::builder()
        .icon_name(crate::icons::BACK)
        .tooltip_text("Back to servers")
        .build();
    let back = back.clone();
    back_button.connect_clicked(move |_| back());
    header.pack_start(&back_button);

    let tiles = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .halign(gtk::Align::Center)
        .column_spacing(32)
        .row_spacing(32)
        // As many columns as tiles, up to six: a flow box is as wide as its
        // most columns, and centring a six-column box leaves two tiles on
        // its left.
        .max_children_per_line((profiles.len() as u32 + 1).min(6))
        .homogeneous(true)
        .build();
    for (i, profile) in profiles.iter().enumerate() {
        let avatar = adw::Avatar::new(148, Some(&profile.display_name), true);
        avatar.add_css_class("profile-avatar");
        avatar.add_css_class(PROFILE_TINTS[i % PROFILE_TINTS.len()]);
        // A locked profile asks for its PIN when chosen (P2-08).
        let detail = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Center)
            .css_classes(["dim-label", "profile-detail"])
            .build();
        let (icon, caption) = profile_caption(profile);
        if let Some(icon) = icon {
            detail.append(&gtk::Image::from_icon_name(icon));
        }
        detail.append(&gtk::Label::new(Some(caption)));
        let tile = profile_tile(&avatar, &profile.display_name, &detail);
        let ended = matches!(
            profile.sign_in,
            ProfileSignIn::Password | ProfileSignIn::Passkey | ProfileSignIn::Portable(_)
        );
        // One whose session ended is only a name here: it can go. One
        // signed in or remembered signs out, which forgets this computer
        // for it too.
        let menu = if ended {
            let (profile, forget) = (profile.clone(), actions.forget.clone());
            vec![vec![crate::views::MenuEntry::new(
                "Forget on this device",
                crate::icons::FORGET,
                move || forget(profile.clone()),
            )]]
        } else {
            let (opened, choose) = (profile.clone(), actions.choose.clone());
            let (leaving, sign_out) = (profile.clone(), actions.sign_out.clone());
            vec![vec![
                crate::views::MenuEntry::new("Open", crate::icons::PLAY, move || {
                    choose(opened.clone())
                }),
                crate::views::MenuEntry {
                    destructive: true,
                    ..crate::views::MenuEntry::new("Sign out", crate::icons::SIGN_OUT, move || {
                        sign_out(leaving.clone())
                    })
                },
            ]]
        };
        crate::views::context_menu(tile.upcast_ref(), &crate::views::menu_popover(menu));
        let (profile, choose) = (profile.clone(), actions.choose.clone());
        tile.connect_clicked(move |_| choose(profile.clone()));
        tiles.append(&tile);
    }
    // A plain circle, not an avatar: libadwaita colours every avatar from
    // its name, which no stylesheet rule reliably undoes.
    let add = gtk::Image::builder()
        .icon_name(crate::icons::ADD)
        .pixel_size(40)
        .width_request(148)
        .height_request(148)
        .halign(gtk::Align::Center)
        .css_classes(["add-profile"])
        .build();
    let add_tile = profile_tile(
        &add,
        "Sign in",
        &gtk::Label::builder()
            .label("Another account")
            .css_classes(["dim-label", "profile-detail"])
            .build(),
    );
    let sign_in = sign_in.clone();
    add_tile.connect_clicked(move |_| sign_in());
    tiles.append(&add_tile);

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(56)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .margin_top(48)
        .margin_bottom(48)
        .build();
    let heading = title("Who's watching?");
    heading.set_halign(gtk::Align::Center);
    heading.add_css_class("profiles-title");
    column.append(&heading);
    column.append(&tiles);
    if let Some(profile) = needs_sign_in {
        column.append(&needs_sign_in_card(profile, &actions.sign_in_as));
    }
    let note = dim(
        "Profiles signed in here, and the ones this computer remembers. Right-click one \
         to sign out, which also forgets this computer for it.",
    );
    note.set_halign(gtk::Align::Center);
    note.set_max_width_chars(70);
    column.append(&note);
    page(&header, &column)
}

/// "Alice needs to sign in again on this computer.", and Sign in: a
/// remembered profile the server would not resume (LinuxTrustStates 1).
fn needs_sign_in_card(profile: &Profile, sign_in_as: &Choose<Profile>) -> gtk::Box {
    let card = gtk::Box::builder()
        .spacing(16)
        .halign(gtk::Align::Center)
        .css_classes(["card", "needs-sign-in"])
        .build();
    card.append(
        &gtk::Image::builder()
            .icon_name(crate::icons::LOCK)
            .css_classes(["warning"])
            .build(),
    );
    card.append(
        &gtk::Label::builder()
            .label(format!(
                "{} needs to sign in again on this computer.",
                profile.display_name
            ))
            .wrap(true)
            .xalign(0.0)
            .build(),
    );
    let button = gtk::Button::builder()
        .label("Sign in")
        .valign(gtk::Align::Center)
        .css_classes(["action", "compact", "suggested-action"])
        .build();
    let (profile, sign_in_as) = (profile.clone(), sign_in_as.clone());
    button.connect_clicked(move |_| sign_in_as(profile.clone()));
    card.append(&button);
    card
}

/// "Sign out Alice?", as the LinuxTrustStates board has it; `sign_out`
/// runs on Sign out.
pub fn sign_out_confirm(name: &str, sign_out: Rc<dyn Fn()>) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Sign out {name}?")),
        Some(&format!(
            "This also forgets this computer: next time, {name} signs in with a password or \
             passkey."
        )),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("sign-out", "Sign out")]);
    dialog.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");
    dialog.connect_response(Some("sign-out"), move |_, _| sign_out());
    dialog
}

/// The avatars' fills, in turn, as on the boards: the accent, mauve, peach.
pub const PROFILE_TINTS: [&str; 3] = ["tint-accent", "tint-purple", "tint-orange"];

fn profile_tile(
    avatar: &impl IsA<gtk::Widget>,
    name: &str,
    detail: &impl IsA<gtk::Widget>,
) -> gtk::Button {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .build();
    body.append(avatar);
    body.append(
        &gtk::Label::builder()
            .label(name)
            .css_classes(["profile-name"])
            .build(),
    );
    body.append(detail);
    gtk::Button::builder()
        .child(&body)
        .css_classes(["flat", "profile-tile"])
        .build()
}

/// The sign-in dialog: a username and password for one server (§22), or
/// the web sign-in, on a phone (P3-05) or in the browser (P3-06). The app keeps it to show a refusal
/// without closing it.
pub struct SignIn {
    pub dialog: adw::Dialog,
    username: gtk::Entry,
    password: gtk::PasswordEntry,
    error: gtk::Label,
    button: gtk::Button,
}

impl SignIn {
    /// `passkey` opens Sign in with a passkey, with the username typed,
    /// if any. Each way in is handed whether to remember this device
    /// (P4-02).
    pub fn new(
        server: &Server,
        submit: Rc<dyn Fn(String, String, bool)>,
        elsewhere: Rc<dyn Fn(Via, bool)>,
        passkey: Rc<dyn Fn(Option<String>, bool)>,
    ) -> Self {
        let username = gtk::Entry::builder().css_classes(["field"]).build();
        let password = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .css_classes(["field"])
            .build();
        {
            let password = password.clone();
            username.connect_activate(move |_| {
                password.grab_focus();
            });
        }

        let error = gtk::Label::builder()
            .visible(false)
            .wrap(true)
            .css_classes(["error"])
            .build();
        let button = action("Sign in", None, &["suggested-action"]);
        // Above every way in, and off unless ticked: a shared computer
        // should not keep a profile open by default.
        let remember = remember_box();
        let send = {
            let (username, password, remember) =
                (username.clone(), password.clone(), remember.clone());
            move || {
                submit(
                    username.text().to_string(),
                    password.text().to_string(),
                    remember.is_active(),
                )
            }
        };
        let send_ = send.clone();
        button.connect_clicked(move |_| send_());
        password.connect_activate(move |_| send());

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_top(24)
            .margin_bottom(28)
            .margin_start(28)
            .margin_end(28)
            .build();
        body.append(&field("Username", &username));
        body.append(&field("Password", &password));
        body.append(&error);
        body.append(&remember_row(&remember));
        body.append(&button);
        // A passkey on a security key or a phone signs in here (P3-19); a
        // sign-in provider cannot, and the web sign-in does it for the app,
        // in the browser or on a phone.
        body.append(&or_rule());
        let with_passkey = action(
            "Sign in with a passkey",
            Some(crate::icons::PASSKEY),
            &["secondary"],
        );
        {
            let (username, remember) = (username.clone(), remember.clone());
            with_passkey.connect_clicked(move |_| {
                let typed = username.text().trim().to_owned();
                passkey((!typed.is_empty()).then_some(typed), remember.is_active());
            });
        }
        body.append(&with_passkey);
        for (label, icon, via) in [
            ("Sign in with browser", crate::icons::BROWSER, Via::Browser),
            // P3-05: another device approves this one; a passkey saved on a
            // phone is under Sign in with a passkey.
            (
                "Approve from another device",
                crate::icons::PHONE,
                Via::Phone,
            ),
        ] {
            let button = action(label, Some(icon), &["secondary"]);
            let (elsewhere, remember) = (elsewhere.clone(), remember.clone());
            button.connect_clicked(move |_| elsewhere(via, remember.is_active()));
            body.append(&button);
        }

        let dialog = adw::Dialog::builder()
            .title(format!("Sign in to {}", server.name))
            .content_width(420)
            .focus_widget(&username)
            .build();
        with_cancel(&dialog, &body).pack_end(&settings_gear());
        Self {
            dialog,
            username,
            password,
            error,
            button,
        }
    }

    /// Signing in again as someone whose session ended: their name is
    /// kept, so only the password is asked.
    pub fn fill_username(&self, name: &str) {
        self.username.set_text(name);
        self.dialog.set_property("focus-widget", &self.password);
    }

    /// While a sign-in is in flight, the button cannot be pressed again.
    pub fn set_busy(&self, busy: bool) {
        self.button.set_sensitive(!busy);
        if busy {
            self.error.set_visible(false);
        }
    }

    pub fn refuse(&self, reason: &str) {
        self.set_busy(false);
        self.error.set_label(reason);
        self.error.set_visible(true);
    }
}

/// "Remember this device", unticked (P4-02).
pub fn remember_box() -> gtk::CheckButton {
    gtk::CheckButton::builder()
        .label("Remember this device")
        .css_classes(["remember-device"])
        .build()
}

/// The box with its helper line under it.
pub fn remember_row(remember: &gtk::CheckButton) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    row.append(remember);
    row.append(
        &gtk::Label::builder()
            .label("You can open your profile here without a password, until you sign out.")
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(1)
            .margin_start(28)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    row
}

/// A label over its field, as the boards lay out a form.
pub(crate) fn field(label: &str, entry: &impl IsA<gtk::Widget>) -> gtk::Box {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    column.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .css_classes(["field-label"])
            .build(),
    );
    column.append(entry);
    column
}

/// "or" between two rules.
fn or_rule() -> gtk::Box {
    let row = gtk::Box::builder().spacing(12).build();
    let rule = || {
        gtk::Separator::builder()
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build()
    };
    row.append(&rule());
    row.append(
        &gtk::Label::builder()
            .label("or")
            .css_classes(["or-label"])
            .build(),
    );
    row.append(&rule());
    row
}

/// The "Add a server" dialog: the server's key, which the first connect
/// pins, and an address to dial it at. For servers mDNS does not show: on
/// another subnet, or behind a firewall that drops multicast.
pub struct AddServer {
    pub dialog: adw::Dialog,
    error: gtk::Label,
    button: gtk::Button,
}

impl AddServer {
    pub fn new(submit: Rc<dyn Fn(String, String)>) -> Self {
        let key = adw::EntryRow::builder()
            .use_markup(false)
            .title("Server key")
            .build();
        let address = adw::EntryRow::builder()
            .use_markup(false)
            .title("Address, such as 192.168.1.20:4433")
            .build();
        let fields = adw::PreferencesGroup::builder()
            .description("The key is what `spoolframe-server id` prints on the server.")
            .build();
        fields.add(&key);
        fields.add(&address);
        let error = gtk::Label::builder()
            .visible(false)
            .wrap(true)
            .css_classes(["error"])
            .build();
        let button = gtk::Button::builder()
            .label("Add")
            .halign(gtk::Align::Center)
            .css_classes(["suggested-action", "pill"])
            .build();
        let send = {
            let (key, address) = (key.clone(), address.clone());
            move || {
                submit(
                    key.text().trim().to_owned(),
                    address.text().trim().to_owned(),
                )
            }
        };
        let send_ = send.clone();
        button.connect_clicked(move |_| send_());
        address.connect_entry_activated(move |_| send());

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        body.append(&fields);
        body.append(&error);
        body.append(&button);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&body));
        let dialog = adw::Dialog::builder()
            .title("Add a server")
            .content_width(460)
            .child(&toolbar)
            .focus_widget(&key)
            .build();
        Self {
            dialog,
            error,
            button,
        }
    }

    pub fn set_busy(&self, busy: bool) {
        self.button.set_sensitive(!busy);
        if busy {
            self.error.set_visible(false);
        }
    }

    pub fn refuse(&self, reason: &str) {
        self.set_busy(false);
        self.error.set_label(reason);
        self.error.set_visible(true);
    }
}

fn page(header: &adw::HeaderBar, body: &impl IsA<gtk::Widget>) -> gtk::Widget {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(header);
    toolbar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .child(body)
            .vexpand(true)
            .build(),
    ));
    toolbar.upcast()
}

/// A centred column at most 560 px wide, and the clamp to put on the page.
fn column() -> (adw::Clamp, gtk::Box) {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(48)
        .margin_bottom(48)
        .margin_start(16)
        .margin_end(16)
        .build();
    let clamp = adw::Clamp::builder()
        .maximum_size(560)
        .child(&column)
        .build();
    (clamp, column)
}

fn title(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["title-1"])
        .build()
}

pub(crate) fn dim(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build()
}

/// "<server> isn't set up yet" (P3-10), where the library would be: the
/// server runs and signs people in, but its admin has not finished setup,
/// so it has no catalog. Laid out as the design's LinuxNotSetUp board.
pub struct NotSetUp {
    pub page: gtk::Widget,
    title: gtk::Label,
    status: adw::StatusPage,
    hint: gtk::Label,
}

/// The admin listener's port when the server's config leaves it alone.
const ADMIN_PORT: u16 = 7421;

impl NotSetUp {
    pub fn new(try_again: Rc<dyn Fn()>, choose_server: Rc<dyn Fn()>) -> Self {
        let title = gtk::Label::builder().css_classes(["heading"]).build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let hint = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .selectable(true)
            .css_classes(["small-text"])
            .build();
        let other = action("Choose another server", None, &["secondary", "compact"]);
        other.set_hexpand(false);
        other.connect_clicked(move |_| choose_server());
        let again = action(
            "Try again",
            Some(crate::icons::BACK_10),
            &["suggested-action", "compact"],
        );
        again.set_hexpand(false);
        again.connect_clicked(move |_| try_again());
        let buttons = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .margin_top(8)
            .build();
        buttons.append(&other);
        buttons.append(&again);
        let child = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .build();
        child.append(&hint);
        child.append(&buttons);
        let status = adw::StatusPage::builder()
            .icon_name(crate::icons::SET_UP)
            .css_classes(["small-icon", "warning-icon", "not-set-up"])
            .description(
                "The server is running, but its admin hasn't finished setup in the web UI. \
                 Once they do, you can sign in here.",
            )
            .child(&child)
            .build();
        Self {
            page: page(&header, &status),
            title,
            status,
            hint,
        }
    }

    /// Names `server`, and where its setup is likely to be.
    pub fn show(&self, server: &Server) {
        self.title.set_label(&server.name);
        self.status
            .set_title(&format!("{} isn't set up yet", server.name));
        // The words dimmed, the address not: it is what to type. Where
        // the server says it is, else where it was reached.
        let hint = "If that's you: open the address in the server's log";
        let hint = match admin_example(if server.advertised.is_empty() {
            &server.addresses
        } else {
            &server.advertised
        }) {
            Some(address) => markup!(
                "<span fgalpha=\"55%\">{}, for example</span> \
                 <span font_family=\"JetBrains Mono\">{}</span>",
                hint,
                address
            ),
            None => markup!("<span fgalpha=\"55%\">{}.</span>", hint),
        };
        self.hint.set_markup(hint.escaped());
    }
}

/// Where the server's web UI likely is, as an example: the host it was
/// seen at, on the admin listener's default port.
fn admin_example(addresses: &[String]) -> Option<String> {
    let address = addresses.first()?;
    let host = match address.strip_prefix('[') {
        // [v6]:port or [v6]
        Some(rest) => format!("[{}]", rest.split(']').next()?),
        None if address.matches(':').count() == 1 => address.split(':').next()?.to_owned(),
        // A bare IPv6 address.
        None if address.contains(':') => format!("[{address}]"),
        None => address.clone(),
    };
    Some(format!("http://{host}:{ADMIN_PORT}"))
}

#[cfg(test)]
mod not_set_up_tests {
    use super::*;

    #[test]
    fn the_example_is_the_host_on_the_admin_port() {
        assert_eq!(
            admin_example(&["192.168.1.20:4433".into()]).as_deref(),
            Some("http://192.168.1.20:7421")
        );
    }

    #[test]
    fn an_ipv6_host_is_bracketed() {
        assert_eq!(
            admin_example(&["[fd00::14]:4433".into()]).as_deref(),
            Some("http://[fd00::14]:7421")
        );
    }

    #[test]
    fn with_no_address_there_is_no_example() {
        assert_eq!(admin_example(&[]), None);
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::{ServerKey, Trust};

    use super::*;

    /// Every widget of type `T` under `widget`.
    fn all<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Vec<T> {
        let mut found = Vec::new();
        if let Some(it) = widget.downcast_ref::<T>() {
            found.push(it.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            found.extend(all::<T>(&c));
            child = c.next_sibling();
        }
        found
    }

    /// One test: GTK binds itself to the first thread that starts it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn start_dialogs_on_a_display() {
        gtk::init().unwrap();
        adw::init().unwrap();
        a_changed_server_id_shows_both_and_offers_removing_it();
    }

    fn a_changed_server_id_shows_both_and_offers_removing_it() {
        let removed = Rc::new(std::cell::Cell::new(false));
        let dialog = {
            let removed = removed.clone();
            server_id_changed(
                "Home Media",
                "k7f3q2m4x5pq6z2a",
                "w4n2b2r6t3kq7x5m",
                Rc::new(move || removed.set(true)),
            )
        };
        let window = adw::Window::builder()
            .default_width(800)
            .default_height(600)
            .build();
        window.present();
        dialog.present(Some(&window));
        let root = dialog.child().expect("the alert's content");
        let texts: Vec<String> = all::<gtk::Label>(&root)
            .iter()
            .map(|l| l.text().to_string())
            .collect();
        assert!(
            texts.iter().any(|t| t == "k7f3q2m4x5pq6z2a")
                && texts.iter().any(|t| t == "w4n2b2r6t3kq7x5m"),
            "{texts:?}"
        );
        all::<gtk::Button>(&root)
            .into_iter()
            .find(|b| b.label().as_deref() == Some("Remove server"))
            .expect("a Remove server button")
            .emit_clicked();
        assert!(removed.get(), "Remove server did not remove it");
        window.destroy();
    }

    #[test]
    fn a_profile_last_in_as_an_identity_says_so() {
        let id = spoolframe_protocol::portable::PortableUserId::from_bytes([7; 32]).unwrap();
        let profile = Profile {
            trust: None,
            ..remembered(ProfileSignIn::Portable(id), trust())
        };
        assert_eq!(profile_caption(&profile).1, "Your identity");
    }

    fn remembered(sign_in: ProfileSignIn, trust: Trust) -> Profile {
        Profile {
            server: ServerKey("home".into()),
            username: "alice".into(),
            display_name: "Alice".into(),
            locked: false,
            sign_in,
            trust: Some(trust),
        }
    }

    fn trust() -> Trust {
        Trust {
            user_id: "u-alice".into(),
            pin_required: false,
            auto_login: false,
            by_passkey: false,
        }
    }

    #[test]
    fn only_the_auto_login_profile_opens_at_start() {
        let signed_in = Profile {
            username: "steve".into(),
            trust: None,
            ..remembered(ProfileSignIn::Automatic, trust())
        };
        let opens = remembered(
            ProfileSignIn::Resume,
            Trust {
                auto_login: true,
                ..trust()
            },
        );
        let profiles = [signed_in, opens];
        assert_eq!(
            opens_at_start(&profiles).map(|p| p.username.as_str()),
            Some("alice")
        );
    }

    #[test]
    fn the_profile_that_opens_at_start_says_so() {
        let profile = remembered(
            ProfileSignIn::Automatic,
            Trust {
                auto_login: true,
                ..trust()
            },
        );
        assert_eq!(profile_caption(&profile).1, "Opens at start");
    }

    #[test]
    fn a_remembered_profile_with_a_pin_shows_the_lock() {
        let profile = remembered(
            ProfileSignIn::Resume,
            Trust {
                pin_required: true,
                ..trust()
            },
        );
        assert_eq!(
            profile_caption(&profile),
            (Some(crate::icons::LOCK), "Remembered · PIN")
        );
    }

    #[test]
    fn a_remembered_profile_last_in_by_passkey_says_passkey() {
        let profile = remembered(
            ProfileSignIn::Resume,
            Trust {
                by_passkey: true,
                ..trust()
            },
        );
        assert_eq!(profile_caption(&profile).1, "Remembered · passkey");
    }

    #[test]
    fn a_profile_signed_in_but_not_remembered_says_signed_in() {
        let profile = Profile {
            trust: None,
            ..remembered(ProfileSignIn::Automatic, trust())
        };
        assert_eq!(profile_caption(&profile).1, "Signed in");
    }

    fn nimble(address: &str) -> Server {
        Server {
            key: ServerKey("f5e5a1c0d800".into()),
            name: "Nimble".into(),
            addresses: vec![address.into()],
            pinned: false,
            fingerprint: "VVWT-BWMQ-SZ8C-6ZDC".into(),
            verified: false,
            on_network: true,
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
    fn a_nearby_server_shows_its_host_and_fingerprint_not_its_key() {
        assert_eq!(
            nearby_subtitle(&nimble("10.10.0.6:7440")),
            "10.10.0.6  ·  VVWT-BWMQ-SZ8C-6ZDC"
        );
    }

    fn local(y: i32, m: i32, d: i32) -> gtk::glib::DateTime {
        gtk::glib::DateTime::new(&gtk::glib::TimeZone::utc(), y, m, d, 12, 0, 0.0).unwrap()
    }

    #[test]
    fn a_day_is_today_yesterday_or_its_date() {
        let now = local(2026, 10, 4);
        assert_eq!(
            [
                day_between(&local(2026, 10, 4), &now),
                day_between(&local(2026, 10, 3), &now),
                day_between(&local(2026, 9, 12), &now),
                day_between(&local(2025, 9, 12), &now),
            ],
            ["today", "yesterday", "12 Sep", "12 Sep 2025"]
        );
    }

    #[test]
    fn an_ipv6_host_loses_its_brackets_and_port() {
        assert_eq!(host("[fe80::1]:7430"), "fe80::1");
    }

    #[test]
    fn a_saved_server_without_an_address_shows_its_fingerprint() {
        let mut server = nimble("10.10.0.6:7440");
        server.addresses.clear();
        assert_eq!(nearby_subtitle(&server), "VVWT-BWMQ-SZ8C-6ZDC");
    }
}
