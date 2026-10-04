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
    BackendError, BoxFuture, DeviceInfo, DeviceSignInRequest, Session,
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
    /// The server's key, hex: a pasted link must name this server.
    pub server_key: String,
    pub server_name: String,
    pub display_name: String,
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
        other => refusal(other),
    }
}

/// What to tell the user when the server said no.
pub fn refusal(error: &BackendError) -> String {
    match error {
        BackendError::InvalidInput(why) => capitalized(why),
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

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The page, for `session`, showing toasts on `dialog`.
pub fn page(signed: Signed, dialog: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Profile")
        .icon_name(crate::icons::LOCK)
        .build();
    let view = Rc::new(View {
        session: signed.session,
        server_key: signed.server_key,
        server_name: signed.server_name,
        display_name: signed.display_name,
        dialog: dialog.downgrade(),
        devices: adw::PreferencesGroup::builder()
            .title("Asked for on")
            .description("Where your PIN is asked for before your profile opens.")
            .build(),
        rows: std::cell::RefCell::new(Vec::new()),
    });

    let pin = adw::PreferencesGroup::builder()
        .title("PIN")
        .description("4 to 8 digits, asked for before your profile opens.")
        .build();
    let set = adw::ButtonRow::builder()
        .title("Set or Change PIN…")
        .build();
    {
        let view = Rc::downgrade(&view);
        set.connect_activated(move |_| {
            if let Some(view) = view.upgrade() {
                view.set_pin();
            }
        });
    }
    let remove = adw::ButtonRow::builder()
        .title("Remove PIN…")
        .css_classes(["destructive-action"])
        .build();
    {
        let view = Rc::downgrade(&view);
        remove.connect_activated(move |_| {
            if let Some(view) = view.upgrade() {
                view.remove_pin();
            }
        });
    }
    pin.add(&set);
    pin.add(&remove);
    page.add(&pin);
    page.add(&view.devices);
    let others = adw::PreferencesGroup::builder()
        .title("Sign in another device")
        .description("A TV or another device shows a code when it asks to sign in as you.")
        .build();
    // The name the web page and the other clients use for it.
    let approve = adw::ButtonRow::builder().title("Sign In a Device…").build();
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
        let plex = adw::PreferencesGroup::builder()
            .title("Plex")
            .description("Sign in with Plex, and choose which libraries keep watch state in sync.")
            .build();
        let row = adw::ActionRow::builder()
            .title("Plex Account")
            .subtitle("Opens your profile in the web UI")
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name(crate::icons::RIGHT));
        let url = profile_url(&web);
        let page_ = page.downgrade();
        row.connect_activated(move |_| {
            let window = page_
                .upgrade()
                .and_then(|p| p.root())
                .and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&url).launch(
                window.as_ref(),
                None::<&gtk::gio::Cancellable>,
                |result| {
                    if let Err(e) = result {
                        tracing::warn!(error = %e, "cannot open the browser");
                    }
                },
            );
        });
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
    dialog: glib::WeakRef<adw::PreferencesDialog>,
    devices: adw::PreferencesGroup,
    rows: std::cell::RefCell<Vec<adw::SwitchRow>>,
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
    }

    fn show(self: &Rc<Self>, devices: Vec<DeviceInfo>) {
        for row in self.rows.borrow_mut().drain(..) {
            self.devices.remove(&row);
        }
        let now = now();
        let mut rows = Vec::new();
        for device in devices {
            let name = device
                .name
                .clone()
                .unwrap_or_else(|| "Unnamed device".into());
            let mut subtitle = last_used(device.last_seen_at, now);
            if device.this_device {
                subtitle = format!("This device · {subtitle}");
            }
            let row = adw::SwitchRow::builder()
                .title(&name)
                .subtitle(&subtitle)
                .active(device.pin_required)
                .build();
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

    fn set_pin(self: &Rc<Self>) {
        let Some(dialog) = self.dialog.upgrade() else {
            return;
        };
        let new = pin_row("New PIN, 4 to 8 digits");
        let again = pin_row("The same again");
        let alert = alert(
            "Set PIN",
            "It is asked for on the devices you choose.",
            "Save",
        );
        alert.set_extra_child(Some(&boxed(&[&new, &again])));
        let this = Rc::downgrade(self);
        alert.connect_response(None, move |_, response| {
            let Some(this) = this.upgrade() else { return };
            if response != "confirm" {
                return;
            }
            let pin = new.text().to_string();
            // A typo would lock the user out of their own profile.
            if pin != again.text().as_str() {
                return this.toast("The two PINs differ; nothing was changed");
            }
            this.run(
                move |s| Box::pin(async move { s.set_pin(&pin).await }),
                |this, result| match result {
                    Ok(()) => this.toast("PIN saved"),
                    Err(e) => this.toast(&refusal(&e)),
                },
            );
        });
        alert.present(Some(&dialog));
    }

    fn remove_pin(self: &Rc<Self>) {
        let Some(dialog) = self.dialog.upgrade() else {
            return;
        };
        let alert = alert(
            "Remove Your PIN?",
            "It will no longer be asked for on any device.",
            "Remove",
        );
        alert.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        let this = Rc::downgrade(self);
        alert.connect_response(None, move |_, response| {
            let Some(this) = this.upgrade() else { return };
            if response != "confirm" {
                return;
            }
            this.run(
                |s| Box::pin(async move { s.clear_pin().await }),
                |this, result| match result {
                    Ok(()) => {
                        this.toast("PIN removed");
                        this.load();
                    }
                    Err(e) => this.toast(&refusal(&e)),
                },
            );
        });
        alert.present(Some(&dialog));
    }
}

impl View {
    /// Asks for the code the other device shows, or its link.
    fn ask_code(self: &Rc<Self>) {
        let Some(dialog) = self.dialog.upgrade() else {
            return;
        };
        let entry = adw::EntryRow::builder().title("Code or link").build();
        let alert = alert(
            "Sign in a device",
            "Enter the code the device shows, such as BCDF-GHJK, or paste its link.",
            "Continue",
        );
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        list.append(&entry);
        alert.set_extra_child(Some(&list));
        let this = Rc::downgrade(self);
        alert.connect_response(None, move |_, response| {
            let Some(this) = this.upgrade() else { return };
            if response != "confirm" {
                return;
            }
            match code_from(&entry.text(), &this.server_key) {
                Ok(code) => this.look_up(code),
                Err(why) => this.toast(why),
            }
        });
        alert.present(Some(&dialog));
    }

    fn look_up(self: &Rc<Self>, code: String) {
        let asked_for = code.clone();
        self.run(
            move |s| Box::pin(async move { s.lookup_device_sign_in(&asked_for).await }),
            move |this, result| match result {
                Ok(request) => this.confirm(code, request),
                Err(e) => this.toast(&approval_refusal(&e)),
            },
        );
    }

    /// "Sign in on Living Room Apple TV?", with the code to compare.
    fn confirm(self: &Rc<Self>, code: String, request: DeviceSignInRequest) {
        let Some(dialog) = self.dialog.upgrade() else {
            return;
        };
        let device = request
            .device_name
            .clone()
            .unwrap_or_else(|| "a device".to_owned());
        let alert = adw::AlertDialog::new(
            Some(&format!("Sign in on {device}?")),
            Some(&format!(
                "It will sign in to {} as {}. It asked {}, from {}. Check that the code matches the one on the device.",
                self.server_name,
                self.display_name,
                asked(request.requested_at, now()),
                request.client_name,
            )),
        );
        let shown = gtk::Label::builder()
            .label(device_sign_in::display(&code))
            .css_classes(["title-1", "monospace", "accent"])
            .build();
        alert.set_extra_child(Some(&shown));
        alert.add_responses(&[
            ("cancel", "Cancel"),
            ("deny", "Not Me"),
            ("approve", "Approve"),
        ]);
        alert.set_response_appearance("deny", adw::ResponseAppearance::Destructive);
        alert.set_response_appearance("approve", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("approve"));
        alert.set_close_response("cancel");
        let this = Rc::downgrade(self);
        alert.connect_response(None, move |_, response| {
            let Some(this) = this.upgrade() else { return };
            let approve = match response {
                "approve" => true,
                "deny" => false,
                // Left pending: it expires on its own.
                _ => return,
            };
            let (code, device) = (code.clone(), device.clone());
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
                    Ok(()) if approve => this.toast(&format!("{device} is signing in")),
                    Ok(()) => this.toast(&format!("{device} was not signed in")),
                    Err(e) => this.toast(&approval_refusal(&e)),
                },
            );
        });
        alert.present(Some(&dialog));
    }
}

fn pin_row(title: &str) -> adw::PasswordEntryRow {
    adw::PasswordEntryRow::builder()
        .title(title)
        .input_purpose(gtk::InputPurpose::Pin)
        .build()
}

fn alert(heading: &str, body: &str, confirm: &str) -> adw::AlertDialog {
    let alert = adw::AlertDialog::new(Some(heading), Some(body));
    alert.add_responses(&[("cancel", "Cancel"), ("confirm", confirm)]);
    alert.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("confirm"));
    alert.set_close_response("cancel");
    alert
}

fn boxed(rows: &[&adw::PasswordEntryRow]) -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for row in rows {
        list.append(*row);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_servers_reason_is_shown_capitalized() {
        assert_eq!(
            refusal(&BackendError::InvalidInput("set a PIN first".into())),
            "Set a PIN first"
        );
    }
}
