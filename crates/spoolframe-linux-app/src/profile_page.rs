//! Preferences → Profile: the user's PIN, whether it is asked for on this
//! device, and the same switch for each of their devices, so a TV with an
//! awkward keyboard is set from here (P2-08); and approving another
//! device's sign-in by the code it shows (P3-07).
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
    BackendError, BoxFuture, DeviceInfo, DeviceSignInRequest, PinProof, PinState, Platform,
    PlexLink, Session,
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
    /// For the passkeys, whose "Confirm it's you" signs in again.
    pub client: Arc<dyn spoolframe_linux_core::backend::Client>,
    pub server: spoolframe_linux_core::backend::Server,
    pub profile: spoolframe_linux_core::backend::Profile,
    /// The server's key, hex: a pasted link must name this server.
    pub server_key: String,
    pub server_name: String,
    pub display_name: String,
    pub username: String,
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
            client: signed.client,
            server: signed.server,
            profile: signed.profile,
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
            .title("Asked for on")
            .description("Your devices that are signed in to this profile.")
            .build(),
        rows: std::cell::RefCell::new(Vec::new()),
        pin_row: adw::ActionRow::builder().title("PIN").subtitle("…").build(),
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
    page.add(&view.devices);
    let others = adw::PreferencesGroup::builder()
        .title("Sign in another device")
        .build();
    // The name the web page and the other clients use for it.
    let approve = adw::ActionRow::builder()
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
    if let Some(web) = view.session.web_url() {
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
    rows: std::cell::RefCell<Vec<adw::SwitchRow>>,
    pin_row: adw::ActionRow,
    set_button: gtk::Button,
    remove_button: gtk::Button,
    /// Whether the user has a PIN, as the server last said.
    has_pin: std::cell::Cell<bool>,
    plex_row: adw::ActionRow,
}

impl View {
    fn toast(&self, text: &str) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.add_toast(adw::Toast::new(text));
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
        let now = now();
        let mut rows = Vec::new();
        for device in devices {
            let (title, subtitle) = device_line(&device, now);
            let row = adw::SwitchRow::builder()
                .use_markup(false)
                .active(device.pin_required)
                .build();
            row.set_title(&title);
            row.set_subtitle(&subtitle);
            row.add_prefix(&gtk::Image::from_icon_name(device_icon(device.platform)));
            let this = Rc::downgrade(self);
            let id = (!device.this_device).then(|| device.endpoint_id.clone());
            row.connect_active_notify(move |row| {
                if let Some(this) = this.upgrade() {
                    this.require(id.clone(), row.is_active());
                }
            });
            self.devices.add(&row);
            rows.push(row);
        }
        self.rows.replace(rows);
    }

    /// Asks for the PIN on `device` (this one for `None`), or stops
    /// asking. A refusal puts the switch back to what the server has.
    fn require(self: &Rc<Self>, device: Option<String>, on: bool) {
        self.run(
            move |s| Box::pin(async move { s.set_pin_required(device.as_deref(), on).await }),
            move |this, result| match result {
                Ok(()) if on => this.toast("Your PIN will be asked for there"),
                Ok(()) => this.toast("Your PIN will not be asked for there"),
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
    gtk::UriLauncher::new(url).launch(window.as_ref(), None::<&gtk::gio::Cancellable>, |result| {
        if let Err(e) = result {
            tracing::warn!(error = %e, "cannot open the browser");
        }
    });
}

/// "Open My profile in the web app", for an account with no password:
/// the web UI proves who it is with a passkey or provider, and sets or
/// removes the PIN there. None when the server has no web address.
fn web_profile_link(session: &dyn Session) -> Option<gtk::Button> {
    let url = profile_url(&session.web_url()?);
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
        let body = step_body(
            crate::icons::PHONE,
            "Sign in this device?",
            &format!(
                "It asked {}, from {}.",
                asked(request.requested_at, now()),
                request.client_name
            ),
        );
        let facts = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        for (label, value, mono) in [
            ("Device", device.clone(), false),
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
            let (this, code, device, stack, done, dialog) = (
                Rc::downgrade(self),
                code.clone(),
                device.clone(),
                stack.clone(),
                done.clone(),
                dialog.clone(),
            );
            button.connect_clicked(move |_| {
                let Some(this) = this.upgrade() else { return };
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
                                s.approve_device_sign_in(&code).await
                            } else {
                                s.deny_device_sign_in(&code).await
                            }
                        })
                    },
                    move |this, result| match result {
                        Ok(()) if approve => {
                            fill_done(&done, &device, &dialog);
                            stack.set_visible_child_name("done");
                        }
                        Ok(()) => {
                            if let Some(dialog) = dialog.upgrade() {
                                dialog.close();
                            }
                            this.toast(&format!("{device} was not signed in"));
                        }
                        Err(e) => this.toast(&approval_refusal(&e)),
                    },
                );
            });
        }
    }
}

/// Step 3: "Apple TV is signed in", and Done.
fn fill_done(page: &gtk::Box, device: &str, dialog: &glib::WeakRef<adw::Dialog>) {
    while let Some(child) = page.first_child() {
        page.remove(&child);
    }
    let body = step_body(
        crate::icons::CHECK,
        &format!("{device} is signed in"),
        "It shows up under Devices in your profile on the web.",
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
