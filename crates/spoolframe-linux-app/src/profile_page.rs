//! Preferences → Profile: the user's PIN, whether it is asked for on this
//! device, and the same switch for each of their devices, so a TV with an
//! awkward keyboard is set from here (P2-08); and approving another
//! device's sign-in by the code it shows (P3-07). This computer: whether
//! it remembers the profile and opens it at start, and which devices are
//! trusted, another one forgotten from here (P4-02).
//!
//! The page talks to the session itself and shows what the server said as
//! toasts on the dialog. Being signed in is all any of it takes: the
//! session came from a sign-in with the password (later SSO or a passkey),
//! and a PIN only ever locks such a session, so an unlocked one is proof
//! enough.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{
    AddressNetwork, BackendError, BoxFuture, DeviceAddress, DeviceInfo, DeviceSignInRequest,
    PinProof, PinState, Platform, PlexLink, Session,
};
use spoolframe_protocol::device_sign_in;

/// "Today", "yesterday", "3 days ago": when a device was last used, from
/// its Unix time and now's.
pub fn last_used(seen: i64, now: i64) -> String {
    match (now - seen).max(0) / 86_400 {
        0 => "Used today".to_owned(),
        1 => "Used yesterday".to_owned(),
        days => format!("Used {days} days ago"),
    }
}

/// The signed-in profile the page is for.
pub struct Signed {
    pub session: Arc<dyn Session>,
    /// For the passkeys' dialogs, which name it.
    pub server: spoolframe_linux_core::backend::Server,
    /// The server's key, hex: a pasted link must name this server.
    pub server_key: String,
    pub server_name: String,
    pub display_name: String,
    pub username: String,
    /// This computer stopped remembering the profile, which ends its
    /// session here: the app goes back to Who's watching.
    pub left: Rc<dyn Fn()>,
}

/// The web UI's profile page, where a Plex account is linked.
fn profile_url(web: &str) -> String {
    format!("{}/profile", web.trim_end_matches('/'))
}

/// A code typed (any case, with or without the dash) or a pasted
/// `spoolframe://approve?…` link, as a code to look up; or what is wrong
/// with it.
pub fn code_from(text: &str, server_key: &str) -> Result<String, &'static str> {
    if let Some(link) = device_sign_in::parse_link(text) {
        if !link.server.eq_ignore_ascii_case(server_key) {
            return Err("That link is for another server");
        }
        return Ok(link.code);
    }
    device_sign_in::normalize(text).ok_or("That isn't a sign-in code")
}

/// "just now", "4 min ago": when the device asked.
pub fn asked(requested_at: i64, now: i64) -> String {
    match (now - requested_at).max(0) / 60 {
        0 => "just now".to_owned(),
        minutes => format!("{minutes} min ago"),
    }
}

/// What to tell the approver when the server said no (P3-07).
pub fn approval_refusal(error: &BackendError) -> String {
    match error {
        BackendError::NotFound(_) => "That code isn't valid or has expired".to_owned(),
        BackendError::RateLimited(wait) => format!(
            "Too many wrong codes. Try again in {} min",
            wait.as_secs().div_ceil(60).max(1)
        ),
        BackendError::InvalidInput(_) => "This device can't approve its own sign-in".to_owned(),
        BackendError::ProfileLocked => {
            "Your profile is locked. Enter your PIN, then try again".to_owned()
        }
        BackendError::TimedOut(_) | BackendError::Unreachable(_) => {
            "The server didn't answer. The device may be signed in already: look at it before \
             trying again"
                .to_owned()
        }
        other => refusal(other),
    }
}

/// What to tell the user when the server said no.
pub fn refusal(error: &BackendError) -> String {
    match error {
        BackendError::InvalidInput(why) => capitalized(why),
        // What it shows now is read again: the change may have gone
        // through before the answer was lost.
        BackendError::TimedOut(_) | BackendError::Unreachable(_) => {
            "The server didn't answer. What's shown now is what it has".to_owned()
        }
        other => capitalized(&other.to_string()),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The PIN row's line: "Set · 4 digits", "Set" (a PIN from before the
/// server kept its length), or "Not set".
pub fn pin_summary(pin: Option<&PinState>) -> String {
    match pin {
        Some(PinState {
            digits: Some(digits),
        }) => format!("Set · {digits} digits"),
        Some(_) => "Set".to_owned(),
        None => "Not set".to_owned(),
    }
}

/// The Plex row's line: who is linked, or that nobody is.
pub fn plex_summary(link: Option<&PlexLink>) -> String {
    match link {
        Some(link) if link.by_config => "Linked by the server's config".to_owned(),
        Some(link) => match link.email.as_ref().or(link.username.as_ref()) {
            Some(who) => format!("Linked as {who} · managed in the web app"),
            None => "Linked · managed in the web app".to_owned(),
        },
        None => "Not linked · link it in the web app".to_owned(),
    }
}

/// A device's row: "This computer" for this one, with its name; else its
/// name. The line under says what it runs and when it was last used.
pub fn device_line(device: &DeviceInfo, now: i64) -> (String, String) {
    let name = device
        .name
        .clone()
        .unwrap_or_else(|| "Unnamed device".into());
    let used = last_used(device.last_seen_at, now);
    let runs = device.platform.map(Platform::label);
    if device.this_device {
        let line = match runs {
            Some(runs) => format!("{name} · {runs}"),
            None => name,
        };
        ("This computer".to_owned(), line)
    } else {
        let line = match runs {
            Some(runs) => format!("{runs} · {used}"),
            None => used,
        };
        (name, line)
    }
}

/// An icon for what a device runs.
fn device_icon(platform: Option<Platform>) -> &'static str {
    match platform {
        Some(Platform::Tvos) => crate::icons::SHOWS,
        Some(Platform::Ios) => crate::icons::PHONE,
        Some(Platform::Linux | Platform::Macos) => crate::icons::LAPTOP,
        _ => crate::icons::MONITOR,
    }
}

/// Where a device asked to sign in from, as the approval says it (security
/// finding 27): a home network or the server's own machine plainly, the
/// internet as a warning (`true`), since a code someone sent you is theirs.
/// None when the server does not say.
fn asked_from(device: &str, from: Option<&DeviceAddress>) -> Option<(String, bool)> {
    let from = from?;
    Some(match from.network {
        AddressNetwork::Loopback => (format!("{device} asked from this computer"), false),
        // A device in the server's own /64 counts as local, though its
        // address is public: say so.
        AddressNetwork::Local if public_ipv6(&from.address) => (
            format!(
                "{device} asked from {}, on your network (public IPv6)",
                from.address
            ),
            false,
        ),
        AddressNetwork::Local => (format!("{device} asked from {}", from.address), false),
        // Unknown: a kind a newer server names; warned of as the internet.
        AddressNetwork::Internet | AddressNetwork::Unknown => (
            format!(
                "{device} asked from {}. This device is on the internet, not on your network. \
                 Approve only a device you can see. A code someone sent you is theirs.",
                from.address
            ),
            true,
        ),
    })
}

/// A global unicast IPv6 address: not unique-local (fc00::/7), not
/// link-local (fe80::/10), not IPv4-mapped, and not loopback, unspecified
/// or multicast.
fn public_ipv6(address: &str) -> bool {
    let Ok(ip) = address.parse::<std::net::Ipv6Addr>() else {
        return false;
    };
    let first = ip.segments()[0];
    !(first & 0xfe00 == 0xfc00
        || first & 0xffc0 == 0xfe80
        || ip.to_ipv4_mapped().is_some()
        || ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast())
}

/// A device's trust, as the pill beside it says.
fn trust_pill(trusted: bool) -> &'static str {
    if trusted { "Trusted" } else { "Not trusted" }
}

/// What turning on Open at start says, before it is (LinuxTrustStates 5).
fn auto_login_body(name: &str) -> String {
    format!(
        "Spoolframe opens {name}’s profile when it starts, without Who’s watching. {name}’s PIN \
         is turned off on this computer, and any other profile stops opening here at start."
    )
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The page, for `session`, showing toasts on `dialog`.
pub fn page(signed: Signed, dialog: &adw::ToastOverlay) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Profile")
        .icon_name(crate::icons::PROFILE)
        .build();
    let passkeys = crate::passkeys::group(
        crate::passkeys::Needs {
            server: signed.server,
            session: signed.session.clone(),
        },
        dialog,
    );
    let view = Rc::new(View {
        session: signed.session,
        server_key: signed.server_key,
        server_name: signed.server_name,
        display_name: signed.display_name,
        dialog: dialog.downgrade(),
        devices: adw::PreferencesGroup::builder()
            .title("Devices")
            // The legend (canvas v74), in markup: the terms bold.
            .description(
                "<b>Trusted</b>: remembered on this device: picking your profile on Who’s \
                 watching opens it without your password. Ends when you sign out there, change \
                 your password, or don’t use it for 180 days.\n\
                 <b>Open at start</b>: opens your profile as soon as the app starts, instead of \
                 showing Who’s watching. Needs Trusted; one profile per device.\n\
                 <b>PIN</b>: asks for your PIN before opening your profile here, even when the \
                 device is trusted.",
            )
            .build(),
        remember_row: adw::SwitchRow::builder()
            .use_markup(false)
            .title("Remember this device")
            .subtitle(
                "Reopen your profile here without a password, until you sign out. Forgotten \
                 after 180 days unused.",
            )
            .build(),
        auto_row: adw::SwitchRow::builder()
            .use_markup(false)
            .title("Open this profile when Spoolframe starts")
            .subtitle(
                "Skips Who’s watching. Turns off your PIN on this computer, and stops any other \
                 profile opening here at start.",
            )
            .sensitive(false)
            .build(),
        here: std::cell::Cell::new(Here::default()),
        syncing: std::cell::Cell::new(false),
        left: signed.left,
        rows: std::cell::RefCell::new(Vec::new()),
        pin_row: adw::ActionRow::builder()
            .use_markup(false)
            .title("PIN")
            .subtitle("…")
            .build(),
        set_button: gtk::Button::builder()
            .label("Set PIN…")
            .valign(gtk::Align::Center)
            .css_classes(["row-button"])
            .build(),
        remove_button: gtk::Button::builder()
            .label("Remove PIN…")
            .valign(gtk::Align::Center)
            .visible(false)
            .css_classes(["row-button", "destructive-tint"])
            .build(),
        has_pin: std::cell::Cell::new(false),
        plex_row: adw::ActionRow::builder()
            .use_markup(false)
            .title("Plex account")
            .activatable(true)
            .build(),
    });

    // Who: the avatar, the name, the username and the server.
    let avatar = adw::Avatar::new(48, Some(&view.display_name), true);
    avatar.add_css_class("profile-avatar");
    avatar.add_css_class("tint-purple");
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Center)
        .build();
    names.append(
        &gtk::Label::builder()
            .label(&view.display_name)
            .xalign(0.0)
            .css_classes(["title-4", "profile-header-name"])
            .build(),
    );
    names.append(
        &gtk::Label::builder()
            .label(format!("{} · {}", signed.username, view.server_name))
            .xalign(0.0)
            .css_classes(["dim-label", "small-text"])
            .build(),
    );
    let who = gtk::Box::builder().spacing(14).build();
    who.append(&avatar);
    who.append(&names);
    let header = adw::PreferencesGroup::new();
    header.add(&who);
    page.add(&header);
    page.add(&passkeys);

    let pin = adw::PreferencesGroup::builder()
        .title("Profile PIN")
        .description("Asked for when someone opens your profile.")
        .build();
    view.pin_row
        .add_prefix(&gtk::Image::from_icon_name(crate::icons::LOCK));
    {
        let weak = Rc::downgrade(&view);
        view.set_button.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.set_pin();
            }
        });
        let weak = Rc::downgrade(&view);
        view.remove_button.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.remove_pin();
            }
        });
    }
    view.pin_row.add_suffix(&view.set_button);
    view.pin_row.add_suffix(&view.remove_button);
    pin.add(&view.pin_row);
    page.add(&pin);
    let this_computer = adw::PreferencesGroup::builder()
        .title("This computer")
        .build();
    this_computer.add(&view.remember_row);
    this_computer.add(&view.auto_row);
    {
        let weak = Rc::downgrade(&view);
        view.remember_row.connect_active_notify(move |row| {
            if let Some(view) = weak.upgrade().filter(|v| !v.syncing.get()) {
                view.remember(row.is_active());
            }
        });
        let weak = Rc::downgrade(&view);
        view.auto_row.connect_active_notify(move |row| {
            if let Some(view) = weak.upgrade().filter(|v| !v.syncing.get()) {
                view.open_at_start(row.is_active());
            }
        });
    }
    page.add(&this_computer);
    page.add(&view.devices);
    let others = adw::PreferencesGroup::builder()
        .title("Sign in another device")
        .build();
    // The name the web page and the other clients use for it.
    let approve = adw::ActionRow::builder()
        .use_markup(false)
        .title("Sign In a Device…")
        .subtitle("Type the code a TV or phone shows, and approve it here")
        .activatable(true)
        .build();
    approve.add_prefix(&gtk::Image::from_icon_name(crate::icons::PHONE));
    approve.add_suffix(&gtk::Image::from_icon_name(crate::icons::RIGHT));
    {
        let view = Rc::downgrade(&view);
        approve.connect_activated(move |_| {
            if let Some(view) = view.upgrade() {
                view.ask_code();
            }
        });
    }
    others.add(&approve);
    page.add(&others);
    // Linking a Plex account is the web UI's (P3-12): the row opens it,
    // when the server has a web address a browser can reach.
    if let Some(web) = view.session.web_url().filter(|u| crate::browser::is_web(u)) {
        let plex = adw::PreferencesGroup::builder().title("Plex").build();
        let row = view.plex_row.clone();
        row.set_subtitle("…");
        row.add_prefix(&gtk::Image::from_icon_name(crate::icons::LINK));
        row.add_suffix(&gtk::Image::from_icon_name(crate::icons::EXTERNAL));
        let url = profile_url(&web);
        row.connect_activated(move |row| open_in_browser(row, &url));
        plex.add(&row);
        page.add(&plex);
    }
    view.load();
    // The page owns its view.
    page.connect_destroy(move |_| {
        let _ = &view;
    });
    page
}

struct View {
    session: Arc<dyn Session>,
    server_key: String,
    server_name: String,
    display_name: String,
    /// Preferences' toasts, and the parent its own dialogs show over.
    dialog: glib::WeakRef<adw::ToastOverlay>,
    devices: adw::PreferencesGroup,
    rows: std::cell::RefCell<Vec<adw::ActionRow>>,
    remember_row: adw::SwitchRow,
    auto_row: adw::SwitchRow,
    /// This computer as the server last listed it.
    here: std::cell::Cell<Here>,
    /// Set while the switches are put to what the server said, so that is
    /// not taken for the user turning them.
    syncing: std::cell::Cell<bool>,
    left: Rc<dyn Fn()>,
    pin_row: adw::ActionRow,
    set_button: gtk::Button,
    remove_button: gtk::Button,
    /// Whether the user has a PIN, as the server last said.
    has_pin: std::cell::Cell<bool>,
    plex_row: adw::ActionRow,
}

/// This computer's trust for the profile, from the device list.
#[derive(Debug, Clone, Copy, Default)]
struct Here {
    trusted: bool,
    auto_login: bool,
}

impl View {
    fn toast(&self, text: &str) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.add_toast(crate::markup::toast(text));
        }
    }

    /// Runs `call` on the session off the main loop, then `then` with its
    /// answer back on it.
    fn run<T: Send + 'static>(
        self: &Rc<Self>,
        call: impl FnOnce(Arc<dyn Session>) -> BoxFuture<'static, T>,
        then: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        let handle = relm4::spawn(call(self.session.clone()));
        let this = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = handle.await else { return };
            if let Some(this) = this.upgrade() {
                then(&this, answer);
            }
        });
    }

    /// "Confirm it's you" on this session, then `again`.
    fn confirm(&self, title: &str, again: Rc<dyn Fn()>) {
        if let Some(parent) = self.dialog.upgrade() {
            crate::confirm::ask(
                &parent,
                self.session.clone(),
                &self.server_name,
                title,
                again,
            );
        }
    }

    /// Lists the devices again, each with its switch.
    fn load(self: &Rc<Self>) {
        self.run(
            |s| Box::pin(async move { s.devices().await }),
            |this, devices| match devices {
                Ok(devices) => this.show(devices),
                Err(e) => this.toast(&refusal(&e)),
            },
        );
        self.load_pin();
        self.run(
            |s| Box::pin(async move { s.plex_link().await }),
            |this, link| match link {
                Ok(link) => this.plex_row.set_subtitle(&plex_summary(link.as_ref())),
                Err(_) => this.plex_row.set_subtitle("Managed in the web app"),
            },
        );
    }

    /// Whether there is a PIN: the row's line and which buttons it shows.
    fn load_pin(self: &Rc<Self>) {
        self.run(
            |s| Box::pin(async move { s.pin().await }),
            |this, pin| match pin {
                Ok(pin) => {
                    this.has_pin.set(pin.is_some());
                    this.pin_row.set_subtitle(&pin_summary(pin.as_ref()));
                    this.set_button.set_label(if pin.is_some() {
                        "Change PIN…"
                    } else {
                        "Set PIN…"
                    });
                    this.remove_button.set_visible(pin.is_some());
                }
                Err(e) => this.toast(&refusal(&e)),
            },
        );
    }

    fn show(self: &Rc<Self>, devices: Vec<DeviceInfo>) {
        for row in self.rows.borrow_mut().drain(..) {
            self.devices.remove(&row);
        }
        let here = devices
            .iter()
            .find(|d| d.this_device)
            .map_or_else(Here::default, |d| Here {
                trusted: d.trusted,
                auto_login: d.auto_login,
            });
        self.here.set(here);
        self.syncing.set(true);
        self.remember_row.set_active(here.trusted);
        self.auto_row.set_active(here.auto_login);
        // Opening at start needs this computer remembered.
        self.auto_row.set_sensitive(here.trusted);
        self.syncing.set(false);
        let now = now();
        let mut rows = Vec::new();
        for device in devices {
            let row = self.device_row(&device, now);
            self.devices.add(&row);
            rows.push(row);
        }
        self.rows.replace(rows);
    }

    /// A device: what it is, its PIN switch, whether it is trusted, and
    /// Forget for another device that is.
    fn device_row(self: &Rc<Self>, device: &DeviceInfo, now: i64) -> adw::ActionRow {
        let (title, subtitle) = device_line(device, now);
        let row = adw::ActionRow::builder().use_markup(false).build();
        row.set_title(&title);
        row.set_subtitle(&subtitle);
        row.add_prefix(&gtk::Image::from_icon_name(device_icon(device.platform)));
        row.add_suffix(
            &gtk::Label::builder()
                .label("PIN")
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        let pin = gtk::Switch::builder()
            .active(device.pin_required)
            .valign(gtk::Align::Center)
            .build();
        {
            let this = Rc::downgrade(self);
            let id = (!device.this_device).then(|| device.endpoint_id.clone());
            pin.connect_active_notify(move |pin| {
                if let Some(this) = this.upgrade() {
                    this.pin_switched(id.clone(), pin.is_active());
                }
            });
        }
        row.add_suffix(&pin);
        row.add_suffix(
            &gtk::Label::builder()
                .label(trust_pill(device.trusted))
                .valign(gtk::Align::Center)
                .css_classes(if device.trusted {
                    vec!["status-chip", "trusted"]
                } else {
                    vec!["status-chip", "untrusted"]
                })
                .build(),
        );
        if device.trusted && !device.this_device {
            let forget = gtk::Button::builder()
                .label("Forget")
                .valign(gtk::Align::Center)
                .css_classes(["row-button", "destructive-tint"])
                .build();
            let (this, id, name) = (Rc::downgrade(self), device.endpoint_id.clone(), title);
            forget.connect_clicked(move |_| {
                if let Some(this) = this.upgrade() {
                    this.ask_forget(id.clone(), name.clone());
                }
            });
            row.add_suffix(&forget);
        }
        // Delete (canvas v75): any other device, gone from the list until
        // it signs in again. This computer signs out instead.
        if !device.this_device {
            let delete = gtk::Button::builder()
                .label("Delete")
                .valign(gtk::Align::Center)
                .css_classes(["row-button", "destructive-tint"])
                .build();
            let (this, id, name) = (
                Rc::downgrade(self),
                device.endpoint_id.clone(),
                row.title().to_string(),
            );
            delete.connect_clicked(move |_| {
                if let Some(this) = this.upgrade() {
                    this.ask_delete(id.clone(), name.clone());
                }
            });
            row.add_suffix(&delete);
        }
        row
    }

    /// "Delete <device>?" (LinuxTrustStates): it is signed out of this
    /// profile and forgotten, and shows up as new if it signs in again.
    fn ask_delete(self: &Rc<Self>, id: String, name: String) {
        let weak = Rc::downgrade(self);
        self.ask(
            &format!("Delete {name}?"),
            "It is signed out and forgotten. If it signs in again it shows up as new.",
            "Delete",
            true,
            Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.delete(id.clone(), name.clone());
                }
            }),
            Rc::new(|| {}),
        );
    }

    /// Deletes the device; the server always wants a recent sign-in for
    /// it, so a refusal confirms it is them and deletes again.
    fn delete(self: &Rc<Self>, id: String, name: String) {
        let (again_id, again_name) = (id.clone(), name.clone());
        self.run(
            move |s| Box::pin(async move { s.forget_device(&id).await }),
            move |this, result| match result {
                Ok(()) => {
                    this.toast(&format!("Deleted {name}"));
                    this.load();
                }
                Err(BackendError::RecentSignInRequired) => {
                    let weak = Rc::downgrade(this);
                    this.confirm(
                        "Delete a device",
                        Rc::new(move || {
                            if let Some(this) = weak.upgrade() {
                                this.delete(again_id.clone(), again_name.clone());
                            }
                        }),
                    );
                }
                Err(e) => this.toast(&refusal(&e)),
            },
        );
    }

    /// A question over Preferences: `yes` on `action`, `no` on Cancel.
    fn ask(
        &self,
        heading: &str,
        body: &str,
        action: &str,
        destructive: bool,
        yes: Rc<dyn Fn()>,
        no: Rc<dyn Fn()>,
    ) {
        let Some(parent) = self.dialog.upgrade() else {
            return no();
        };
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_responses(&[("cancel", "Cancel"), ("yes", action)]);
        dialog.set_response_appearance(
            "yes",
            if destructive {
                adw::ResponseAppearance::Destructive
            } else {
                adw::ResponseAppearance::Suggested
            },
        );
        dialog.set_close_response("cancel");
        dialog.connect_response(
            None,
            move |_, response| {
                if response == "yes" { yes() } else { no() }
            },
        );
        dialog.present(Some(&parent));
    }

    /// Puts the switches back to what the server has.
    fn undo(self: &Rc<Self>) -> Rc<dyn Fn()> {
        let weak = Rc::downgrade(self);
        Rc::new(move || {
            if let Some(this) = weak.upgrade() {
                this.load();
            }
        })
    }

    /// The PIN switch on a device. Turned on here while the profile opens
    /// at start, it says that stops first (LinuxTrustStates 6).
    fn pin_switched(self: &Rc<Self>, device: Option<String>, on: bool) {
        if !(on && device.is_none() && self.here.get().auto_login) {
            return self.require(device, on);
        }
        let weak = Rc::downgrade(self);
        self.ask(
            "Ask for the PIN here?",
            &format!(
                "{} stops opening at start: Spoolframe shows Who’s watching and asks for the PIN.",
                self.display_name
            ),
            "Turn PIN on",
            false,
            Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.require(None, true);
                }
            }),
            self.undo(),
        );
    }

    /// Remember this device. On needs a recent sign-in; off ends the
    /// session here, so it asks first (LinuxTrustStates 8).
    fn remember(self: &Rc<Self>, on: bool) {
        if on {
            return self.set_trust(true, false);
        }
        let weak = Rc::downgrade(self);
        self.ask(
            "Stop remembering this computer?",
            "You’re signed out here now, and next time you need your password or a passkey.",
            "Stop remembering",
            true,
            Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.set_trust(false, false);
                }
            }),
            self.undo(),
        );
    }

    /// Open this profile when Spoolframe starts: on says what it turns off
    /// first (LinuxTrustStates 5).
    fn open_at_start(self: &Rc<Self>, on: bool) {
        if !on {
            return self.set_trust(true, false);
        }
        let weak = Rc::downgrade(self);
        self.ask(
            &format!("Open {} at start?", self.display_name),
            &auto_login_body(&self.display_name),
            "Turn on",
            false,
            Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.set_trust(true, true);
                }
            }),
            self.undo(),
        );
    }

    /// Sets this computer's trust and auto-login; a refusal for want of a
    /// recent sign-in confirms it is them, then sets it again.
    fn set_trust(self: &Rc<Self>, trusted: bool, auto_login: bool) {
        self.run(
            move |s| Box::pin(async move { s.set_device_trust(None, trusted, auto_login).await }),
            move |this, result| match result {
                // Not remembered, the session here is over.
                Ok(()) if !trusted => {
                    if let Some(dialog) = this
                        .dialog
                        .upgrade()
                        .and_then(|d| d.ancestor(adw::Dialog::static_type()))
                        .and_downcast::<adw::Dialog>()
                    {
                        dialog.close();
                    }
                    (this.left)();
                }
                Ok(()) => this.load(),
                Err(BackendError::RecentSignInRequired) => {
                    this.load();
                    let weak = Rc::downgrade(this);
                    this.confirm(
                        "Remember this device",
                        Rc::new(move || {
                            if let Some(this) = weak.upgrade() {
                                this.set_trust(trusted, auto_login);
                            }
                        }),
                    );
                }
                Err(e) => {
                    this.toast(&refusal(&e));
                    this.load();
                }
            },
        );
    }

    /// Forget another device (LinuxTrustStates 7): it is signed out of the
    /// profile; trusting it again is the web app's.
    fn ask_forget(self: &Rc<Self>, id: String, name: String) {
        let weak = Rc::downgrade(self);
        self.ask(
            &format!("Forget {name}?"),
            &format!(
                "It’s signed out of {}’s profile now, and needs a sign-in next time. Trusting it \
                 again is done in the web app.",
                self.display_name
            ),
            "Forget",
            true,
            Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.forget(id.clone(), name.clone());
                }
            }),
            Rc::new(|| {}),
        );
    }

    fn forget(self: &Rc<Self>, id: String, name: String) {
        let (again_id, again_name) = (id.clone(), name.clone());
        self.run(
            move |s| Box::pin(async move { s.set_device_trust(Some(&id), false, false).await }),
            move |this, result| match result {
                Ok(()) => {
                    this.toast(&format!("{name} was forgotten"));
                    this.load();
                }
                Err(BackendError::RecentSignInRequired) => {
                    let weak = Rc::downgrade(this);
                    this.confirm(
                        "Forget a device",
                        Rc::new(move || {
                            if let Some(this) = weak.upgrade() {
                                this.forget(again_id.clone(), again_name.clone());
                            }
                        }),
                    );
                }
                Err(e) => this.toast(&refusal(&e)),
            },
        );
    }

    /// Asks for the PIN on `device` (this one for `None`), or stops
    /// asking. A refusal puts the switch back to what the server has.
    fn require(self: &Rc<Self>, device: Option<String>, on: bool) {
        let again = device.clone();
        self.run(
            move |s| Box::pin(async move { s.set_pin_required(device.as_deref(), on).await }),
            move |this, result| match result {
                Ok(()) if on => this.toast("Your PIN will be asked for there"),
                Ok(()) => this.toast("Your PIN will not be asked for there"),
                // No longer asking needs a recent sign-in (P3-18): the
                // switch goes back until it is confirmed, then again.
                Err(BackendError::RecentSignInRequired) => {
                    this.load();
                    let weak = Rc::downgrade(this);
                    this.confirm(
                        "Stop asking for the PIN",
                        Rc::new(move || {
                            if let Some(this) = weak.upgrade() {
                                this.require(again.clone(), on);
                            }
                        }),
                    );
                }
                Err(e) => {
                    this.toast(&refusal(&e));
                    this.load();
                }
            },
        );
    }

    /// "Set a PIN" or "Change PIN": changing proves who it is first, with
    /// the current PIN or the account password; then a new PIN, then the
    /// same again, a mismatch starting it over. The dialog stays until
    /// the server answers, so a wrong proof can say so and ask again.
    fn set_pin(self: &Rc<Self>) {
        let Some(parent) = self.dialog.upgrade() else {
            return;
        };
        let changing = self.has_pin.get();
        let dialog = adw::Dialog::builder()
            .title(if changing { "Change PIN" } else { "Set a PIN" })
            .content_width(400)
            .build();
        let heading = gtk::Label::builder().css_classes(["title-3"]).build();
        let hint = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label", "small-text"])
            .build();
        let entry = pin_entry();
        let proof = ProofInput::new(self.session.as_ref());
        let fields = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        fields.append(&proof.root);
        fields.append(&entry);
        let next = gtk::Button::builder()
            .sensitive(false)
            .css_classes(["action", "suggested-action"])
            .build();
        let body = pin_body(&heading, &hint, &fields, &next);
        crate::start::with_cancel(&dialog, &body);
        let flow = Rc::new(PinFlow {
            heading,
            hint,
            entry: entry.clone(),
            proof: proof.clone(),
            next: next.clone(),
            changing,
            step: std::cell::Cell::new(if changing {
                PinStep::Prove
            } else {
                PinStep::Choose
            }),
            given: std::cell::RefCell::new(None),
            chosen: std::cell::RefCell::new(None),
        });
        flow.show(None);
        {
            let flow_ = flow.clone();
            entry.connect_changed(move |_| flow_.update_next());
            let flow_ = flow.clone();
            proof.on_change(Rc::new(move || flow_.update_next()));
        }
        let go = {
            let (flow, this, dialog) = (flow.clone(), Rc::downgrade(self), dialog.downgrade());
            Rc::new(move || match flow.step.get() {
                PinStep::Prove => {
                    let Some(given) = flow.proof.proof() else {
                        return;
                    };
                    flow.given.replace(Some(given));
                    flow.step.set(PinStep::Choose);
                    flow.show(None);
                }
                PinStep::Choose => {
                    let typed = flow.entry.text().to_string();
                    if !pin_ok(&typed) {
                        return;
                    }
                    flow.chosen.replace(Some(typed));
                    flow.step.set(PinStep::Again);
                    flow.show(None);
                }
                PinStep::Again => {
                    let typed = flow.entry.text().to_string();
                    let chosen = flow.chosen.borrow().clone().unwrap_or_default();
                    // A typo would lock the user out of their profile.
                    if chosen != typed {
                        flow.step.set(PinStep::Choose);
                        flow.show(Some("Those don't match. Start again."));
                        return;
                    }
                    let Some(this) = this.upgrade() else { return };
                    let given = flow.given.borrow().clone();
                    flow.next.set_sensitive(false);
                    let (flow, dialog) = (flow.clone(), dialog.clone());
                    this.run(
                        move |s| Box::pin(async move { s.set_pin(&chosen, given).await }),
                        move |this, result| match result {
                            Ok(()) => {
                                if let Some(dialog) = dialog.upgrade() {
                                    dialog.close();
                                }
                                this.toast("PIN saved");
                                this.load_pin();
                            }
                            // The proof was refused: back to it, saying why.
                            Err(
                                e @ (BackendError::WrongCredentials
                                | BackendError::NoPassword
                                | BackendError::RateLimited(_)),
                            ) => {
                                flow.step.set(PinStep::Prove);
                                flow.show(None);
                                flow.proof.refused(&e);
                            }
                            // A first PIN, with nothing to prove it by,
                            // wants a recent sign-in: confirm it is them,
                            // then save the PIN typed, as it still is.
                            Err(BackendError::RecentSignInRequired) => {
                                flow.next.set_sensitive(true);
                                let again = flow.next.downgrade();
                                this.confirm(
                                    "Set a PIN",
                                    Rc::new(move || {
                                        if let Some(next) = again.upgrade() {
                                            next.emit_clicked();
                                        }
                                    }),
                                );
                            }
                            Err(e) => {
                                if let Some(dialog) = dialog.upgrade() {
                                    dialog.close();
                                }
                                this.toast(&refusal(&e));
                                this.load_pin();
                            }
                        },
                    );
                }
            })
        };
        {
            let go = go.clone();
            next.connect_clicked(move |_| go());
        }
        {
            let go = go.clone();
            entry.connect_activate(move |_| go());
        }
        proof.on_activate(go);
        dialog.set_focus(Some(if changing {
            proof.pin.upcast_ref::<gtk::Widget>()
        } else {
            entry.upcast_ref()
        }));
        dialog.present(Some(&parent));
    }

    /// "Remove your PIN?": what that means, and the current PIN or the
    /// account password to say so.
    fn remove_pin(self: &Rc<Self>) {
        let Some(parent) = self.dialog.upgrade() else {
            return;
        };
        let dialog = adw::Dialog::builder()
            .title("Remove PIN")
            .content_width(400)
            .build();
        let heading = gtk::Label::builder()
            .label("Remove your PIN?")
            .css_classes(["title-3"])
            .build();
        let hint = gtk::Label::builder()
            .label(
                "Anyone using a device signed in to your profile can open it without \
                 asking. Type your current PIN to confirm.",
            )
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label", "small-text"])
            .build();
        let proof = ProofInput::new(self.session.as_ref());
        let remove = gtk::Button::builder()
            .label("Remove PIN")
            .sensitive(false)
            .css_classes(["action", "destructive-action"])
            .build();
        let body = pin_body(&heading, &hint, &proof.root, &remove);
        crate::start::with_cancel(&dialog, &body);
        {
            let (remove, proof_) = (remove.clone(), proof.clone());
            proof.on_change(Rc::new(move || {
                remove.set_sensitive(proof_.proof().is_some())
            }));
        }
        let go = {
            let (proof, remove, this, dialog) = (
                proof.clone(),
                remove.clone(),
                Rc::downgrade(self),
                dialog.downgrade(),
            );
            Rc::new(move || {
                let Some(given) = proof.proof() else { return };
                let Some(this) = this.upgrade() else { return };
                remove.set_sensitive(false);
                let (proof, dialog) = (proof.clone(), dialog.clone());
                this.run(
                    move |s| Box::pin(async move { s.clear_pin(given).await }),
                    move |this, result| match result {
                        Ok(()) => {
                            if let Some(dialog) = dialog.upgrade() {
                                dialog.close();
                            }
                            this.toast("PIN removed");
                            this.load();
                        }
                        Err(
                            e @ (BackendError::WrongCredentials
                            | BackendError::NoPassword
                            | BackendError::RateLimited(_)),
                        ) => proof.refused(&e),
                        Err(e) => {
                            if let Some(dialog) = dialog.upgrade() {
                                dialog.close();
                            }
                            this.toast(&refusal(&e));
                            this.load();
                        }
                    },
                );
            })
        };
        {
            let go = go.clone();
            remove.connect_clicked(move |_| go());
        }
        proof.on_activate(go);
        dialog.set_focus(Some(&proof.pin));
        dialog.present(Some(&parent));
    }
}

/// Where the PIN dialog is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinStep {
    /// Changing: the current PIN or the password.
    Prove,
    Choose,
    Again,
}

/// The PIN dialog's widgets and what has been typed so far.
struct PinFlow {
    heading: gtk::Label,
    hint: gtk::Label,
    /// The new PIN, twice.
    entry: gtk::PasswordEntry,
    proof: Rc<ProofInput>,
    next: gtk::Button,
    changing: bool,
    step: std::cell::Cell<PinStep>,
    given: std::cell::RefCell<Option<PinProof>>,
    chosen: std::cell::RefCell<Option<String>>,
}

impl PinFlow {
    /// Shows the step it is on, with `problem` in place of the hint and
    /// the field marked when something went wrong.
    fn show(&self, problem: Option<&str>) {
        let step = self.step.get();
        let (heading, hint, button) = match (step, self.changing) {
            (PinStep::Prove, _) => (
                "Confirm it's you",
                "Step 1 of 3. Your current PIN.",
                "Continue",
            ),
            (PinStep::Choose, true) => ("New PIN", "Step 2 of 3. 4 to 8 digits.", "Continue"),
            (PinStep::Choose, false) => ("Choose a PIN", "Step 1 of 2. 4 to 8 digits.", "Continue"),
            (PinStep::Again, true) => ("Type it again", "Step 3 of 3.", "Save PIN"),
            (PinStep::Again, false) => ("Type it again", "Step 2 of 2.", "Save PIN"),
        };
        self.heading.set_label(heading);
        self.hint.set_label(problem.unwrap_or(hint));
        self.next.set_label(button);
        self.proof.root.set_visible(step == PinStep::Prove);
        self.entry.set_visible(step != PinStep::Prove);
        self.entry.set_text("");
        if problem.is_some() {
            self.entry.add_css_class("error");
        } else {
            self.entry.remove_css_class("error");
        }
        self.update_next();
    }

    /// Next only once what the step asks for is there.
    fn update_next(&self) {
        let ready = match self.step.get() {
            PinStep::Prove => self.proof.proof().is_some(),
            _ => pin_ok(&self.entry.text()),
        };
        self.next.set_sensitive(ready);
    }
}

/// Proving it is the user, to change or remove the PIN: the current PIN,
/// or the account password instead (which covers a forgotten PIN). A
/// refusal is said under the field; an account with no password is sent
/// to My profile in the web UI, which proves it there.
struct ProofInput {
    root: gtk::Box,
    pin: gtk::PasswordEntry,
    password: gtk::PasswordEntry,
    switch: gtk::Button,
    problem: gtk::Label,
    web: Option<gtk::Button>,
    using_password: std::cell::Cell<bool>,
    changed: std::cell::RefCell<Option<Rc<dyn Fn()>>>,
}

impl ProofInput {
    fn new(session: &dyn Session) -> Rc<Self> {
        let pin = pin_entry();
        let password = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .placeholder_text("Account password")
            .visible(false)
            .css_classes(["field"])
            .build();
        let switch = gtk::Button::builder()
            .label("Use your password instead")
            .halign(gtk::Align::Center)
            .css_classes(["flat", "text-link"])
            .build();
        let problem = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .visible(false)
            .css_classes(["error", "small-text"])
            .build();
        let web = web_profile_link(session);
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        root.append(&pin);
        root.append(&password);
        root.append(&problem);
        root.append(&switch);
        if let Some(web) = &web {
            web.set_visible(false);
            root.append(web);
        }
        let this = Rc::new(Self {
            root,
            pin,
            password,
            switch,
            problem,
            web,
            using_password: std::cell::Cell::new(false),
            changed: std::cell::RefCell::new(None),
        });
        {
            let weak = Rc::downgrade(&this);
            this.switch.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.use_password(!this.using_password.get());
                }
            });
        }
        for entry in [&this.pin, &this.password] {
            let weak = Rc::downgrade(&this);
            entry.connect_changed(move |entry| {
                entry.remove_css_class("error");
                if let Some(this) = weak.upgrade() {
                    this.problem.set_visible(false);
                    this.notify();
                }
            });
        }
        this
    }

    /// The PIN field or the password field.
    fn use_password(&self, on: bool) {
        self.using_password.set(on);
        self.pin.set_visible(!on);
        self.password.set_visible(on);
        self.switch.set_label(if on {
            "Use your PIN instead"
        } else {
            "Use your password instead"
        });
        self.problem.set_visible(false);
        if let Some(web) = &self.web {
            web.set_visible(false);
        }
        if on {
            self.password.grab_focus();
        } else {
            self.pin.grab_focus();
        }
        self.notify();
    }

    /// What was typed, once it could be a proof.
    fn proof(&self) -> Option<PinProof> {
        if self.using_password.get() {
            let password = self.password.text().to_string();
            (!password.is_empty()).then_some(PinProof::Password { password })
        } else {
            let pin = self.pin.text().to_string();
            pin_ok(&pin).then_some(PinProof::Pin { pin })
        }
    }

    fn on_change(&self, run: Rc<dyn Fn()>) {
        self.changed.replace(Some(run));
    }

    fn on_activate(&self, run: Rc<dyn Fn()>) {
        for entry in [&self.pin, &self.password] {
            let run = run.clone();
            entry.connect_activate(move |_| run());
        }
    }

    fn notify(&self) {
        let changed = self.changed.borrow().clone();
        if let Some(changed) = changed {
            changed();
        }
    }

    /// Says under the field why the server refused the proof.
    fn refused(&self, error: &BackendError) {
        let field = if self.using_password.get() {
            &self.password
        } else {
            &self.pin
        };
        let why = match error {
            BackendError::NoPassword => {
                if let Some(web) = &self.web {
                    web.set_visible(true);
                }
                "Your account signs in with a passkey or a sign-in provider, so it has no \
                 password. Use your PIN, or set it in My profile in the web app."
                    .to_owned()
            }
            BackendError::RateLimited(wait) => format!(
                "Too many tries. Try again in {} min.",
                wait.as_secs().div_ceil(60).max(1)
            ),
            _ if self.using_password.get() => "That password isn't right.".to_owned(),
            _ => "That PIN isn't right.".to_owned(),
        };
        field.set_text("");
        field.add_css_class("error");
        self.problem.set_label(&why);
        self.problem.set_visible(true);
    }
}

/// Opens `url` in the browser, over the window `widget` is in.
fn open_in_browser(widget: &impl IsA<gtk::Widget>, url: &str) {
    let window = widget.root().and_downcast::<gtk::Window>();
    crate::browser::open(url, window.as_ref());
}

/// "Open My profile in the web app", for an account with no password:
/// the web UI proves who it is with a passkey or provider, and sets or
/// removes the PIN there. None when the server has no web address.
fn web_profile_link(session: &dyn Session) -> Option<gtk::Button> {
    let url = profile_url(&session.web_url().filter(|u| crate::browser::is_web(u))?);
    let body = gtk::Box::builder().spacing(4).build();
    body.append(&gtk::Label::new(Some("Open My profile in the web app")));
    body.append(&gtk::Image::from_icon_name(crate::icons::EXTERNAL));
    let link = gtk::Button::builder()
        .child(&body)
        .halign(gtk::Align::Center)
        .css_classes(["flat", "text-link"])
        .build();
    link.connect_clicked(move |link| open_in_browser(link, &url));
    Some(link)
}

/// A PIN as the server takes it: 4 to 8 digits.
fn pin_ok(pin: &str) -> bool {
    spoolframe_types::pin::pin_digits(pin).is_some()
}

/// The PIN typed large and spaced, digits only.
fn pin_entry() -> gtk::PasswordEntry {
    gtk::PasswordEntry::builder()
        .halign(gtk::Align::Center)
        .width_chars(8)
        .css_classes(["field", "pin-field"])
        .build()
}

/// A dialog's body: the lock, a heading, a hint, the field and its button.
fn pin_body(
    heading: &gtk::Label,
    hint: &gtk::Label,
    entry: &impl IsA<gtk::Widget>,
    button: &gtk::Button,
) -> gtk::Box {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(26)
        .margin_bottom(24)
        .margin_start(28)
        .margin_end(28)
        .build();
    body.append(
        &gtk::Image::builder()
            .icon_name(crate::icons::LOCK)
            .pixel_size(30)
            .width_request(56)
            .height_request(56)
            .halign(gtk::Align::Center)
            .css_classes(["check-icon"])
            .build(),
    );
    body.append(heading);
    body.append(hint);
    body.append(entry);
    body.append(button);
    body
}

impl View {
    /// "Sign In a Device": the code the other device shows (or its link),
    /// then who it signs in as, then that it is signed in; a wrong code or
    /// too many says so under the field.
    fn ask_code(self: &Rc<Self>) {
        let Some(parent) = self.dialog.upgrade() else {
            return;
        };
        let dialog = adw::Dialog::builder()
            .title("Sign In a Device")
            .content_width(420)
            .build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .vhomogeneous(false)
            .build();

        // 1 · The code.
        let entry = gtk::Entry::builder()
            .placeholder_text("BCDF-GHJK")
            .css_classes(["field", "code-field"])
            .build();
        let problem = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .css_classes(["error", "small-text"])
            .build();
        let next = gtk::Button::builder()
            .label("Continue")
            .css_classes(["action", "suggested-action"])
            .build();
        let code_page = step_body(
            crate::icons::PHONE,
            "Enter the code",
            "Type the code the TV or phone shows, or paste its link.",
        );
        code_page.append(&entry);
        code_page.append(&problem);
        code_page.append(&next);
        stack.add_named(&code_page, Some("code"));

        // 2 · Who it signs in as, and 3 · done, filled in as they come.
        let confirm_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stack.add_named(&confirm_page, Some("confirm"));
        let done_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stack.add_named(&done_page, Some("done"));
        crate::start::with_cancel(&dialog, &stack);

        let refuse = {
            let (entry, problem) = (entry.clone(), problem.clone());
            Rc::new(move |why: &str| {
                entry.add_css_class("error");
                problem.set_label(why);
                problem.set_visible(true);
            })
        };
        let submit = {
            let (this, entry, refuse, stack, confirm_page, done_page, dialog) = (
                Rc::downgrade(self),
                entry.clone(),
                refuse.clone(),
                stack.clone(),
                confirm_page.clone(),
                done_page.clone(),
                dialog.downgrade(),
            );
            Rc::new(move || {
                let Some(this) = this.upgrade() else { return };
                let code = match code_from(&entry.text(), &this.server_key) {
                    Ok(code) => code,
                    Err(why) => return refuse(why),
                };
                let asked_for = code.clone();
                let (refuse, stack, confirm_page, done_page, dialog) = (
                    refuse.clone(),
                    stack.clone(),
                    confirm_page.clone(),
                    done_page.clone(),
                    dialog.clone(),
                );
                this.run(
                    move |s| Box::pin(async move { s.lookup_device_sign_in(&asked_for).await }),
                    move |this, result| match result {
                        Ok(request) => {
                            this.fill_confirm(
                                &confirm_page,
                                &done_page,
                                &stack,
                                &dialog,
                                code,
                                request,
                            );
                            stack.set_visible_child_name("confirm");
                        }
                        Err(e) => refuse(&approval_refusal(&e)),
                    },
                );
            })
        };
        {
            let submit = submit.clone();
            next.connect_clicked(move |_| submit());
        }
        entry.connect_activate(move |_| submit());
        {
            let problem = problem.clone();
            entry.connect_changed(move |entry| {
                entry.remove_css_class("error");
                problem.set_visible(false);
            });
        }
        dialog.set_focus(Some(&entry));
        dialog.present(Some(&parent));
    }

    /// Step 2: the device, who it signs in as and the code to compare, and
    /// Sign In; approving moves to step 3.
    fn fill_confirm(
        self: &Rc<Self>,
        page: &gtk::Box,
        done: &gtk::Box,
        stack: &gtk::Stack,
        dialog: &glib::WeakRef<adw::Dialog>,
        code: String,
        request: DeviceSignInRequest,
    ) {
        while let Some(child) = page.first_child() {
            page.remove(&child);
        }
        let device = request
            .device_name
            .clone()
            .unwrap_or_else(|| "A device".to_owned());
        // Which app asks, as the server says it: "Apple TV app".
        let platform = request.platform.label();
        let body = step_body(
            device_icon(Some(request.platform)),
            "Sign in this device?",
            &format!(
                "It asked {}, from the {platform}.",
                asked(request.requested_at, now()),
            ),
        );
        let facts = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        for (label, value, mono) in [
            ("Device", format!("{device} · {platform}"), false),
            ("Signs in as", self.display_name.clone(), false),
            ("Code", device_sign_in::display(&code), true),
        ] {
            let row = adw::ActionRow::builder().use_markup(false).build();
            row.set_title(label);
            let value = gtk::Label::builder()
                .label(value)
                .css_classes(if mono {
                    vec!["monospace", "sign-in-fingerprint"]
                } else {
                    vec!["heading"]
                })
                .build();
            row.add_suffix(&value);
            facts.append(&row);
        }
        body.append(&facts);
        // It asked to be remembered (P4-02): ticked, and the approver may
        // untick it. A device that did not ask has no box.
        let remember = gtk::CheckButton::builder()
            .label("This device will be remembered")
            .active(request.remember)
            .css_classes(["remember-device"])
            .build();
        if request.remember {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .build();
            row.append(&remember);
            row.append(
                &gtk::Label::builder()
                    .label("It asked to be. Untick to sign it in just this once.")
                    .xalign(0.0)
                    .wrap(true)
                    .max_width_chars(1)
                    .margin_start(28)
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
            body.append(&row);
        }
        if let Some((line, warn)) = asked_from(&device, request.requested_from.as_ref()) {
            body.append(
                &gtk::Label::builder()
                    .label(line)
                    .wrap(true)
                    .justify(gtk::Justification::Center)
                    .css_classes(if warn {
                        vec!["warning", "asked-from"]
                    } else {
                        vec!["dim-label", "small-text", "asked-from"]
                    })
                    .build(),
            );
        }
        let warning = gtk::Label::builder()
            .label("Only continue if you started this on a device in front of you.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label", "small-text"])
            .build();
        body.append(&warning);
        let sign_in = gtk::Button::builder()
            .label("Sign In")
            .css_classes(["action", "suggested-action"])
            .build();
        let not_me = gtk::Button::builder()
            .label("Not me")
            .halign(gtk::Align::Center)
            .css_classes(["flat", "dialog-text-button"])
            .build();
        body.append(&sign_in);
        body.append(&not_me);
        page.append(&body);
        for (button, approve) in [(&sign_in, true), (&not_me, false)] {
            let (this, code, device, stack, done, dialog, remember) = (
                Rc::downgrade(self),
                code.clone(),
                device.clone(),
                stack.clone(),
                done.clone(),
                dialog.clone(),
                remember.clone(),
            );
            button.connect_clicked(move |button| {
                let Some(this) = this.upgrade() else { return };
                // Pressed again once "Confirm it's you" is done.
                let again = button.downgrade();
                // Unshown, it stays unticked.
                let remembered = remember.is_active();
                let (code, device, stack, done, dialog) = (
                    code.clone(),
                    device.clone(),
                    stack.clone(),
                    done.clone(),
                    dialog.clone(),
                );
                this.run(
                    move |s| {
                        Box::pin(async move {
                            if approve {
                                s.approve_device_sign_in(&code, remembered).await
                            } else {
                                s.deny_device_sign_in(&code).await
                            }
                        })
                    },
                    move |this, result| match result {
                        Ok(()) if approve => {
                            fill_done(&done, &device, remembered, &dialog);
                            stack.set_visible_child_name("done");
                        }
                        Ok(()) => {
                            if let Some(dialog) = dialog.upgrade() {
                                dialog.close();
                            }
                            this.toast(&format!("{device} was not signed in"));
                        }
                        Err(BackendError::RecentSignInRequired) => this.confirm(
                            "Sign in a device",
                            Rc::new(move || {
                                if let Some(button) = again.upgrade() {
                                    button.emit_clicked();
                                }
                            }),
                        ),
                        Err(e) => this.toast(&approval_refusal(&e)),
                    },
                );
            });
        }
    }
}

/// What step 3 says under its heading.
fn done_line(remembered: bool) -> &'static str {
    if remembered {
        "It’s remembered until it signs out. It shows up under Devices in your profile."
    } else {
        "It shows up under Devices in your profile on the web."
    }
}

/// Step 3: "Apple TV is signed in", and Done.
fn fill_done(page: &gtk::Box, device: &str, remembered: bool, dialog: &glib::WeakRef<adw::Dialog>) {
    while let Some(child) = page.first_child() {
        page.remove(&child);
    }
    let body = step_body(
        crate::icons::CHECK,
        &format!("{device} is signed in"),
        done_line(remembered),
    );
    let done = gtk::Button::builder()
        .label("Done")
        .css_classes(["action", "suggested-action"])
        .build();
    let dialog = dialog.clone();
    done.connect_clicked(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
    body.append(&done);
    page.append(&body);
}

/// A step's top: its icon in a tile, a heading, a line under it.
fn step_body(icon: &str, heading: &str, line: &str) -> gtk::Box {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(26)
        .margin_bottom(24)
        .margin_start(28)
        .margin_end(28)
        .build();
    body.append(
        &gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(30)
            .width_request(56)
            .height_request(56)
            .halign(gtk::Align::Center)
            .css_classes(["check-icon"])
            .build(),
    );
    body.append(
        &gtk::Label::builder()
            .label(heading)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["title-3"])
            .build(),
    );
    body.append(
        &gtk::Label::builder()
            .label(line)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label", "small-text"])
            .build(),
    );
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approval_without_an_answer_says_to_look_at_the_device() {
        assert!(
            approval_refusal(&BackendError::TimedOut("Home Media".into()))
                .contains("may be signed in already")
        );
    }

    fn run_until(limit: std::time::Duration, done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + limit;
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        done()
    }

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
    fn profile_page_on_the_fake_server() {
        gtk::init().unwrap();
        adw::init().unwrap();
        remembering_this_computer_lets_it_open_at_start();
        only_another_device_can_be_deleted();
    }

    fn only_another_device_can_be_deleted() {
        use spoolframe_linux_core::backend::{Client, Server};
        use spoolframe_linux_core::fake::FakeClient;
        let client: Arc<dyn Client> = Arc::new(FakeClient::new(None));
        let profile = FakeClient::profile("andrey");
        let server = Server {
            key: profile.server.clone(),
            name: "Home Media".into(),
            addresses: Vec::new(),
            pinned: true,
            fingerprint: String::new(),
            verified: true,
            on_network: true,
            verified_at: None,
            last_connected_at: None,
            server_id: None,
            advertised: Vec::new(),
            relays: Vec::new(),
            advertised_at: None,
        };
        let page = page(
            Signed {
                session: client.session(&profile).unwrap(),
                server_key: server.key.0.clone(),
                server_name: server.name.clone(),
                server,
                display_name: "Andrey".into(),
                username: "andrey".into(),
                left: Rc::new(|| {}),
            },
            &adw::ToastOverlay::new(),
        );
        let deletable = |title: &str| {
            all::<adw::ActionRow>(page.upcast_ref())
                .into_iter()
                .find(|r| r.title() == title)
                .map(|row| {
                    all::<gtk::Button>(row.upcast_ref())
                        .iter()
                        .any(|b| b.label().as_deref() == Some("Delete"))
                })
        };
        assert!(
            run_until(std::time::Duration::from_secs(5), || deletable(
                "Living room TV"
            )
            .is_some()),
            "the devices never listed"
        );
        assert_eq!(
            (deletable("Living room TV"), deletable("This computer")),
            (Some(true), Some(false))
        );
    }

    fn remembering_this_computer_lets_it_open_at_start() {
        use spoolframe_linux_core::backend::{Client, Server};
        use spoolframe_linux_core::fake::FakeClient;
        use spoolframe_linux_core::testing::ready;
        let fake = Arc::new(FakeClient::new(None));
        let client: Arc<dyn Client> = fake.clone();
        let profile = FakeClient::profile("andrey");
        let server = Server {
            key: profile.server.clone(),
            name: "Home Media".into(),
            addresses: Vec::new(),
            pinned: true,
            fingerprint: String::new(),
            verified: true,
            on_network: true,
            verified_at: None,
            last_connected_at: None,
            server_id: None,
            advertised: Vec::new(),
            relays: Vec::new(),
            advertised_at: None,
        };
        let toasts = adw::ToastOverlay::new();
        let page = page(
            Signed {
                session: client.session(&profile).unwrap(),
                server_key: server.key.0.clone(),
                server_name: server.name.clone(),
                server,
                display_name: "Andrey".into(),
                username: "andrey".into(),
                left: Rc::new(|| {}),
            },
            &toasts,
        );
        let switch = |title: &str| {
            all::<adw::SwitchRow>(page.upcast_ref())
                .into_iter()
                .find(|r| r.title() == title)
                .unwrap()
        };
        let remember = switch("Remember this device");
        let at_start = switch("Open this profile when Spoolframe starts");
        assert!(!remember.is_active() && !at_start.is_sensitive());
        remember.set_active(true);
        assert!(
            run_until(std::time::Duration::from_secs(5), || ready(
                client.trusted_profiles(&profile.server)
            )
            .unwrap()
            .iter()
            .any(|t| t.username == "andrey")),
            "the switch did not make this computer remember the profile"
        );
        assert!(
            run_until(std::time::Duration::from_secs(5), || at_start
                .is_sensitive()),
            "Open at start stayed off-limits once remembered"
        );
    }

    fn from(address: &str, network: AddressNetwork) -> DeviceAddress {
        DeviceAddress {
            address: address.into(),
            network,
        }
    }

    #[test]
    fn a_device_on_the_home_network_says_where_it_asked_from() {
        assert_eq!(
            asked_from(
                "The TV",
                Some(&from("192.168.86.20", AddressNetwork::Local))
            ),
            Some(("The TV asked from 192.168.86.20".to_owned(), false))
        );
    }

    #[test]
    fn a_device_on_the_home_network_with_a_public_ipv6_address_says_so() {
        assert_eq!(
            asked_from(
                "The TV",
                Some(&from("2603:8000:c33e:6ac6::20", AddressNetwork::Local))
            ),
            Some((
                "The TV asked from 2603:8000:c33e:6ac6::20, on your network (public IPv6)"
                    .to_owned(),
                false
            ))
        );
    }

    #[test]
    fn a_unique_local_or_link_local_ipv6_address_reads_plainly() {
        let line = |address| {
            asked_from("The TV", Some(&from(address, AddressNetwork::Local))).map(|(l, _)| l)
        };
        assert_eq!(
            (line("fd7a:115c:a1e0::1"), line("fe80::1")),
            (
                Some("The TV asked from fd7a:115c:a1e0::1".to_owned()),
                Some("The TV asked from fe80::1".to_owned())
            )
        );
    }

    #[test]
    fn a_device_on_the_servers_own_machine_is_this_computer() {
        assert_eq!(
            asked_from("The TV", Some(&from("127.0.0.1", AddressNetwork::Loopback))),
            Some(("The TV asked from this computer".to_owned(), false))
        );
    }

    #[test]
    fn a_device_on_the_internet_is_a_warning_with_its_address() {
        let (line, warn) = asked_from(
            "The TV",
            Some(&from("203.0.113.5", AddressNetwork::Internet)),
        )
        .unwrap();
        assert!(warn && line.contains("203.0.113.5") && line.contains("not on your network"));
    }

    #[test]
    fn a_network_this_build_does_not_know_warns_too() {
        assert!(
            asked_from(
                "The TV",
                Some(&from("2001:db8::1", AddressNetwork::Unknown))
            )
            .is_some_and(|(_, warn)| warn)
        );
    }

    #[test]
    fn without_an_address_nothing_is_said() {
        assert_eq!(asked_from("The TV", None), None);
    }

    #[test]
    fn a_device_not_trusted_says_so() {
        assert_eq!(trust_pill(false), "Not trusted");
    }

    #[test]
    fn opening_at_start_says_the_pin_goes_off_here() {
        assert!(auto_login_body("Alice").contains("Alice’s PIN is turned off on this computer"));
    }

    #[test]
    fn a_remembered_device_is_said_to_stay_until_it_signs_out() {
        assert!(done_line(true).contains("until it signs out"));
    }

    #[test]
    fn the_plex_account_is_on_the_web_profile_page() {
        assert_eq!(
            [
                profile_url("https://media.example.org"),
                profile_url("https://media.example.org/")
            ],
            [
                "https://media.example.org/profile",
                "https://media.example.org/profile"
            ]
        );
    }

    #[test]
    fn a_device_used_hours_ago_was_used_today() {
        assert_eq!(last_used(1_000_000, 1_000_000 + 3_600), "Used today");
    }

    #[test]
    fn a_device_used_three_days_ago_says_so() {
        assert_eq!(last_used(0, 3 * 86_400 + 10), "Used 3 days ago");
    }

    const KEY: &str = "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f";

    #[test]
    fn a_code_is_taken_in_any_case_with_or_without_the_dash() {
        assert_eq!(
            (code_from("bcdf-ghjk", KEY), code_from("BCDFGHJK", KEY)),
            (Ok("BCDFGHJK".into()), Ok("BCDFGHJK".into()))
        );
    }

    #[test]
    fn a_pasted_link_gives_its_code() {
        let link = device_sign_in::link(KEY, "BCDFGHJK");
        assert_eq!(code_from(&link, KEY), Ok("BCDFGHJK".into()));
    }

    #[test]
    fn a_link_for_another_server_is_refused() {
        let other = "a".repeat(64);
        let link = device_sign_in::link(&other, "BCDFGHJK");
        assert_eq!(
            code_from(&link, KEY),
            Err("That link is for another server")
        );
    }

    #[test]
    fn text_that_is_no_code_is_refused() {
        assert_eq!(code_from("hello", KEY), Err("That isn't a sign-in code"));
    }

    #[test]
    fn an_unknown_code_is_said_plainly() {
        assert_eq!(
            approval_refusal(&BackendError::NotFound("x".into())),
            "That code isn't valid or has expired"
        );
    }

    #[test]
    fn a_request_minutes_old_says_so() {
        assert_eq!(
            (asked(100, 130), asked(100, 100 + 4 * 60 + 5)),
            ("just now".to_owned(), "4 min ago".to_owned())
        );
    }

    #[test]
    fn the_pin_row_says_how_many_digits_or_just_set() {
        assert_eq!(
            [
                pin_summary(Some(&PinState { digits: Some(4) })),
                pin_summary(Some(&PinState { digits: None })),
                pin_summary(None)
            ],
            ["Set · 4 digits", "Set", "Not set"]
        );
    }

    #[test]
    fn a_plex_link_names_the_account() {
        let link = PlexLink {
            username: Some("alice".into()),
            email: Some("alice@example.com".into()),
            by_config: false,
            linked_at: None,
            last_sync: None,
            syncing: false,
        };
        assert_eq!(
            plex_summary(Some(&link)),
            "Linked as alice@example.com · managed in the web app"
        );
    }

    #[test]
    fn this_device_is_this_computer_with_what_it_runs() {
        let device = DeviceInfo {
            endpoint_id: "x".into(),
            name: Some("andoriyu-desktop".into()),
            platform: Some(Platform::Linux),
            last_seen_at: 0,
            pin_required: false,
            this_device: true,
            trusted: false,
            auto_login: false,
        };
        assert_eq!(
            device_line(&device, 0),
            (
                "This computer".to_owned(),
                "andoriyu-desktop · Linux app".to_owned()
            )
        );
    }

    #[test]
    fn a_pin_is_four_to_eight_digits() {
        assert_eq!(
            [
                pin_ok("123"),
                pin_ok("1234"),
                pin_ok("12345678"),
                pin_ok("123456789"),
                pin_ok("12a4")
            ],
            [false, true, true, false, false]
        );
    }

    #[test]
    fn the_servers_reason_is_shown_capitalized() {
        assert_eq!(
            refusal(&BackendError::InvalidInput("set a PIN first".into())),
            "Set a PIN first"
        );
    }
}
