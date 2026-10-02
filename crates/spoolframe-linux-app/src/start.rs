//! The screens before the library: choose a server, choose who is
//! watching, sign in. Laid out as in the UI mockups.

use std::rc::Rc;

use adw::prelude::*;
use spoolframe_linux_core::backend::{Profile, Server};

pub type Choose<T> = Rc<dyn Fn(T)>;

/// "Choose a server": servers on this network, then saved ones, and a way
/// to add one the network does not show.
pub fn servers(
    servers: &[Server],
    choose: &Choose<Server>,
    verify: &Choose<Server>,
    add: &Rc<dyn Fn()>,
) -> gtk::Widget {
    let (clamp, column) = column();
    column.append(&title("Choose a server"));
    column.append(&dim("Servers on this network appear here on their own."));

    let nearby = adw::PreferencesGroup::builder()
        .title("On this network")
        .build();
    let saved = adw::PreferencesGroup::builder().title("Saved").build();
    for server in servers {
        if server.on_network {
            nearby.add(&server_row(server, choose, verify, true));
        }
        if server.pinned {
            saved.add(&server_row(server, choose, verify, false));
        }
    }
    if servers.iter().any(|s| s.on_network) {
        column.append(&nearby);
    }
    if servers.iter().any(|s| s.pinned) {
        column.append(&saved);
    }
    let add_button = gtk::Button::builder()
        .label("Add a server by its key…")
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    let add = add.clone();
    add_button.connect_clicked(move |_| add());
    column.append(&add_button);
    column.append(&dim(
        "Before a server is saved you check its code against the server's own, \
         so a stranger on the network cannot pass for it.",
    ));
    page(&adw::HeaderBar::new(), &clamp)
}

fn server_row(
    server: &Server,
    choose: &Choose<Server>,
    verify: &Choose<Server>,
    nearby: bool,
) -> adw::ActionRow {
    let subtitle = if nearby {
        let address = server
            .addresses
            .first()
            .map_or("no address", String::as_str);
        format!("{address} · key {}", server.key.short())
    } else {
        server.fingerprint.clone()
    };
    let row = adw::ActionRow::builder()
        .title(&server.name)
        .subtitle(subtitle)
        .activatable(true)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(crate::icons::SERVER));
    if nearby {
        let connect = gtk::Button::builder()
            .label("Connect")
            .valign(gtk::Align::Center)
            .css_classes(if server.pinned {
                vec!["suggested-action", "pill"]
            } else {
                vec!["pill"]
            })
            .build();
        let (server_, choose_) = (server.clone(), choose.clone());
        connect.connect_clicked(move |_| choose_(server_.clone()));
        row.add_suffix(&connect);
        row.set_activatable_widget(Some(&connect));
    } else {
        if server.verified {
            row.add_suffix(&badge("Verified", "pinned-badge"));
        } else {
            // Skipped at the check: say so, and offer it again.
            row.add_suffix(&badge("Not verified", "unverified-badge"));
            let check = gtk::Button::builder()
                .label("Verify now")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let (server, verify) = (server.clone(), verify.clone());
            check.connect_clicked(move |_| verify(server.clone()));
            row.add_suffix(&check);
        }
        let (server, choose) = (server.clone(), choose.clone());
        row.connect_activated(move |_| choose(server.clone()));
    }
    row
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
        .css_classes(["title-2"])
        .build();
    let address = server.addresses.first().map(String::as_str);
    let who = gtk::Label::builder()
        .use_markup(true)
        .label(match address {
            Some(address) => format!(
                "<b>{}</b> · {}",
                glib_escape(&server.name),
                glib_escape(address)
            ),
            None => format!("<b>{}</b>", glib_escape(&server.name)),
        })
        .css_classes(["dim-label"])
        .build();
    let code = gtk::Label::builder()
        .label(&server.fingerprint)
        .selectable(true)
        .css_classes(["fingerprint", "monospace"])
        .build();
    let code_box = adw::Bin::builder()
        .child(&code)
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
        .css_classes(["dim-label"])
        .build();
    let full = full_keys(&[("", &server.key.0)]);
    let matches = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::CHECK)
                .label("It matches")
                .build(),
        )
        .hexpand(true)
        .css_classes(["action", "suggested-action"])
        .build();
    let skip = gtk::Button::builder()
        .label(if verifying { "Not now" } else { "Skip" })
        .hexpand(true)
        .css_classes(["action"])
        .build();
    let buttons = gtk::Box::builder().spacing(10).homogeneous(true).build();
    buttons.append(&skip);
    buttons.append(&matches);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(24)
        .margin_bottom(22)
        .margin_start(28)
        .margin_end(28)
        .build();
    for widget in [
        icon.upcast_ref::<gtk::Widget>(),
        heading.upcast_ref(),
        who.upcast_ref(),
        code_box.upcast_ref(),
        how.upcast_ref(),
        full.upcast_ref(),
        buttons.upcast_ref(),
    ] {
        body.append(widget);
    }
    if !verifying {
        body.append(&dim(
            "Skip saves the server as Not verified. You can verify it later.",
        ));
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body));
    let dialog = adw::Dialog::builder()
        .title(if verifying {
            "Verify server"
        } else {
            "Add server"
        })
        .content_width(440)
        .child(&toolbar)
        .build();
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

/// "Server identity changed" (P1-13): a saved server answered with another
/// key. Both codes, the full keys on request, and the choice: Cancel
/// (nothing changes) or Trust New Identity (everyone signs in again).
pub fn identity_changed(
    name: &str,
    saved: &str,
    now: &str,
    trust: Rc<dyn Fn()>,
) -> adw::AlertDialog {
    let code =
        |key: &str| spoolframe_protocol::fingerprint::of_hex(key).unwrap_or_else(|| key.to_owned());
    let row = |label: &str, key: &str, class: &str| {
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
                .label(code(key))
                .xalign(0.0)
                .selectable(true)
                .css_classes(["monospace", class])
                .build(),
        );
        column
    };
    let extra = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    extra.append(&row("Previously", saved, "dim-label"));
    extra.append(&row("Now", now, "identity-new"));
    extra.append(&full_keys(&[("Previously ", saved), ("Now ", now)]));
    extra.append(
        &gtk::Label::builder()
            .use_markup(true)
            .label(
                "Check the new code against the server's log or \
                 <tt>spoolframe-server fingerprint</tt>. If you trust it, you sign \
                 in to this server again.",
            )
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    let dialog = adw::AlertDialog::builder()
        .heading("Server identity changed")
        .body_use_markup(true)
        .body(format!(
            "<b>{}</b> has a different key than the one saved on this computer. \
             This may mean the server was reinstalled, or that you're connecting \
             to a different server.",
            glib_escape(name)
        ))
        .extra_child(&extra)
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("trust", "Trust New Identity");
    dialog.set_response_appearance("trust", adw::ResponseAppearance::Destructive);
    dialog.connect_response(None, move |_, response| {
        if response == "trust" {
            trust();
        }
    });
    dialog
}

/// "Show full key(s)": the hex keys behind the codes, for whoever wants to
/// compare all of them.
fn full_keys(keys: &[(&str, &str)]) -> gtk::Expander {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    for (label, key) in keys {
        column.append(
            &gtk::Label::builder()
                .label(format!("{label}{key}"))
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::Char)
                .selectable(true)
                .xalign(0.0)
                .css_classes(["monospace", "caption", "dim-label"])
                .build(),
        );
    }
    gtk::Expander::builder()
        .label(if keys.len() > 1 {
            "Show full keys"
        } else {
            "Show full key"
        })
        .child(&column)
        .halign(gtk::Align::Center)
        .build()
}

fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

/// "Who's watching?": the users signed in on this device, and a way to sign
/// in another. Switching between them keeps the connection (§15).
pub fn profiles(
    server: &Server,
    profiles: &[Profile],
    choose: &Choose<Profile>,
    sign_in: &Rc<dyn Fn()>,
    back: &Rc<dyn Fn()>,
) -> gtk::Widget {
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(&server.name, "Direct · LAN"))
        .build();
    let back_button = gtk::Button::builder()
        .icon_name(crate::icons::BACK)
        .tooltip_text("Choose another server")
        .build();
    let back = back.clone();
    back_button.connect_clicked(move |_| back());
    header.pack_start(&back_button);

    let tiles = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .halign(gtk::Align::Center)
        .column_spacing(40)
        .row_spacing(32)
        // As many columns as tiles, up to six: a flow box is as wide as its
        // most columns, and centring a six-column box leaves two tiles on
        // its left.
        .max_children_per_line((profiles.len() as u32 + 1).min(6))
        .homogeneous(true)
        .build();
    for profile in profiles {
        let avatar = adw::Avatar::new(128, Some(&profile.display_name), true);
        let tile = profile_tile(&avatar, &profile.display_name, "Signed in");
        let (profile, choose) = (profile.clone(), choose.clone());
        tile.connect_clicked(move |_| choose(profile.clone()));
        tiles.append(&tile);
    }
    // A plain circle, not an avatar: libadwaita colours every avatar from
    // its name, which no stylesheet rule reliably undoes.
    let add = gtk::Image::builder()
        .icon_name(crate::icons::ADD)
        .pixel_size(48)
        .width_request(128)
        .height_request(128)
        .halign(gtk::Align::Center)
        .css_classes(["add-profile"])
        .build();
    let add_tile = profile_tile(&add, "Sign in", "Another account");
    let sign_in = sign_in.clone();
    add_tile.connect_clicked(move |_| sign_in());
    tiles.append(&add_tile);

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(48)
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
    let note = dim("Each profile has its own session on this device.");
    note.set_halign(gtk::Align::Center);
    column.append(&note);
    page(&header, &column)
}

fn profile_tile(avatar: &impl IsA<gtk::Widget>, name: &str, detail: &str) -> gtk::Button {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    body.append(avatar);
    body.append(
        &gtk::Label::builder()
            .label(name)
            .css_classes(["title-4"])
            .margin_top(8)
            .build(),
    );
    body.append(
        &gtk::Label::builder()
            .label(detail)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    gtk::Button::builder()
        .child(&body)
        .css_classes(["flat", "profile-tile"])
        .build()
}

/// The sign-in dialog: a username and password for one server (§22). The
/// app keeps it to show a refusal without closing it.
pub struct SignIn {
    pub dialog: adw::Dialog,
    error: gtk::Label,
    button: gtk::Button,
}

impl SignIn {
    pub fn new(server: &Server, hint: Option<&str>, submit: Rc<dyn Fn(String, String)>) -> Self {
        let username = adw::EntryRow::builder().title("Username").build();
        let password = adw::PasswordEntryRow::builder().title("Password").build();
        let fields = adw::PreferencesGroup::new();
        fields.add(&username);
        fields.add(&password);

        let error = gtk::Label::builder()
            .visible(false)
            .wrap(true)
            .css_classes(["error"])
            .build();
        let button = gtk::Button::builder()
            .label("Sign in")
            .halign(gtk::Align::Center)
            .css_classes(["suggested-action", "pill"])
            .build();
        let send = {
            let (username, password) = (username.clone(), password.clone());
            move || submit(username.text().to_string(), password.text().to_string())
        };
        let send_ = send.clone();
        button.connect_clicked(move |_| send_());
        password.connect_entry_activated(move |_| send());

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
        if let Some(hint) = hint {
            let hint = dim(hint);
            hint.set_halign(gtk::Align::Center);
            hint.add_css_class("caption");
            body.append(&hint);
        }

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&body));
        let dialog = adw::Dialog::builder()
            .title(format!("Sign in to {}", server.name))
            .content_width(420)
            .child(&toolbar)
            .focus_widget(&username)
            .build();
        Self {
            dialog,
            error,
            button,
        }
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
        let key = adw::EntryRow::builder().title("Server key").build();
        let address = adw::EntryRow::builder()
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

fn dim(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build()
}
