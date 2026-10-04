//! Signing in where the app cannot: on a phone (P3-05) or in the system
//! browser (P3-06). Both borrow the server's web sign-in through P3-07's
//! device sign-in: the screen begins a request bound to this device, the
//! user opens its web page (`web_url`), signs in there with anything the
//! web UI has (password, passkey, SSO once it lands) and approves "Sign in
//! on …?", and the wait brings back a session for this device; nothing of
//! it passes through the phone or the browser.
//!
//! - With your phone: laid out as the design's LinuxPhoneSignIn board, a QR
//!   code of the page. No Spoolframe app is needed on the phone; a
//!   signed-in one can take the code too (Preferences › Sign In a
//!   Device… here).
//! - With the browser: the page opens in the system browser (through the
//!   portal), and the screen shows the code and a Cancel. A server without
//!   a web address has no page: the password form takes over, saying why.
//!
//! A server holds at most three waiting requests per device, so one is
//! begun when the screen opens and again only when the user asks (a new
//! code, or trying again); closing the screen stops the wait.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, glib};
use spoolframe_linux_core::backend::{BackendError, Client, DeviceSignIn, Profile, Server};

/// Where the user approves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Phone,
    Browser,
}

/// What the screen hands back to the app. None is called once it closes.
pub struct Ends {
    /// The profile signed in, which goes on as a password sign-in does.
    pub done: Rc<dyn Fn(Profile)>,
    /// The browser has no page to open (the server has no web address):
    /// back to the password form, which says why.
    pub no_web: Rc<dyn Fn()>,
    /// Opens a page in the system browser.
    pub open: Rc<dyn Fn(&str)>,
}

pub struct RemoteSignIn {
    pub dialog: adw::Dialog,
    client: Arc<dyn Client>,
    server: Server,
    via: Via,
    ends: Ends,
    stack: gtk::Stack,
    qr: gtk::Picture,
    /// What to do on the phone: open the page, or, without one, use a
    /// signed-in app.
    lead: gtk::Label,
    page: gtk::Label,
    code: gtk::Label,
    countdown: gtk::Label,
    /// The expired code, struck through.
    expired_code: gtk::Label,
    expired_page: adw::StatusPage,
    new_code: gtk::Button,
    try_again: gtk::Button,
    problem: adw::StatusPage,
    /// The call with the server in flight, stopped when the screen closes.
    call: RefCell<Option<tokio::task::AbortHandle>>,
    /// When the code shown stops working.
    expires: Cell<Option<Instant>>,
    open: Cell<bool>,
}

impl RemoteSignIn {
    pub fn new(client: Arc<dyn Client>, server: &Server, via: Via, ends: Ends) -> Rc<Self> {
        let asking = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::Center)
            .margin_top(48)
            .margin_bottom(48)
            .build();
        asking.append(
            &gtk::Spinner::builder()
                .spinning(true)
                .width_request(32)
                .height_request(32)
                .build(),
        );
        asking.append(&gtk::Label::new(Some("Getting a code…")));

        // 1 · Waiting: the QR code, where to go, the code, the fingerprint.
        let qr = gtk::Picture::builder()
            .can_shrink(false)
            .halign(gtk::Align::Center)
            .css_classes(["qr-code"])
            .build();
        let lead = wrapped("", &[]);
        let page = gtk::Label::builder()
            .selectable(true)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["heading"])
            .build();
        let code = code_label(&["sign-in-code"]);
        let fingerprint = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        fingerprint.append(&wrapped("Server fingerprint", &["dim-label", "caption"]));
        fingerprint.append(
            &gtk::Label::builder()
                .label(&server.fingerprint)
                .selectable(true)
                .css_classes(["monospace", "caption"])
                .build(),
        );
        let countdown = gtk::Label::builder().css_classes(["dim-label"]).build();
        let waiting_row = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        waiting_row.append(&gtk::Spinner::builder().spinning(true).build());
        waiting_row.append(&countdown);
        let use_password = gtk::Button::builder()
            .label(match via {
                Via::Phone => "Use a password instead",
                Via::Browser => "Cancel",
            })
            .halign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let waiting = column();
        for widget in [
            qr.upcast_ref::<gtk::Widget>(),
            lead.upcast_ref(),
            page.upcast_ref(),
            code.upcast_ref(),
            fingerprint.upcast_ref(),
            waiting_row.upcast_ref(),
            use_password.upcast_ref(),
        ] {
            waiting.append(widget);
        }

        // 2 · Expired: the old code struck through, and a new one.
        let expired_code = code_label(&["sign-in-code", "expired"]);
        let new_code = pill("Get a new code", true);
        let expired = column();
        expired.append(&expired_code);
        expired.append(&new_code);
        let expired_page = adw::StatusPage::builder()
            .icon_name(crate::icons::BACK_10)
            .title("This code expired")
            .child(&expired)
            .build();

        // 3 · Denied on the phone.
        let try_again = pill("Try again", true);
        let password_after_denial = pill("Use a password instead", false);
        let denied_actions = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        denied_actions.append(&password_after_denial);
        denied_actions.append(&try_again);
        let denied = adw::StatusPage::builder()
            .icon_name(crate::icons::REMOVE)
            .title("Sign-in was denied")
            .description("Someone chose Deny on the approval page.")
            .child(&denied_actions)
            .build();

        // No code to be had: the server's answer, and another try.
        let problem_retry = pill("Try again", true);
        let problem = adw::StatusPage::builder()
            .icon_name(crate::icons::ERROR)
            .title("No code")
            .child(&problem_retry)
            .build();

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vhomogeneous(false)
            .build();
        stack.add_named(&asking, Some("asking"));
        stack.add_named(&waiting, Some("waiting"));
        stack.add_named(&expired_page, Some("expired"));
        stack.add_named(&denied, Some("denied"));
        stack.add_named(&problem, Some("problem"));

        let body = adw::Clamp::builder()
            .maximum_size(380)
            .margin_top(6)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .child(&stack)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&body));
        let dialog = adw::Dialog::builder()
            .title(match via {
                Via::Phone => "Sign in with your phone",
                Via::Browser => "Sign in with browser",
            })
            .content_width(440)
            .child(&toolbar)
            .build();

        let this = Rc::new(Self {
            dialog,
            client,
            server: server.clone(),
            via,
            ends,
            stack,
            qr,
            lead,
            page,
            code,
            countdown,
            expired_code,
            expired_page,
            new_code,
            try_again,
            problem,
            call: RefCell::new(None),
            expires: Cell::new(None),
            open: Cell::new(true),
        });
        let weak = Rc::downgrade(&this);
        this.dialog.connect_closed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.open.set(false);
                this.stop();
            }
        });
        for button in [&this.new_code, &this.try_again, &problem_retry] {
            let weak = Rc::downgrade(&this);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.begin();
                }
            });
        }
        for button in [&use_password, &password_after_denial] {
            let dialog = this.dialog.downgrade();
            button.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
        }
        this.begin();
        this
    }

    /// Asks the server for a code.
    fn begin(self: &Rc<Self>) {
        self.stop();
        self.expires.set(None);
        self.stack.set_visible_child_name("asking");
        let (client, key) = (self.client.clone(), self.server.key.clone());
        let begun = relm4::spawn(async move { client.begin_device_sign_in(&key).await });
        self.call.replace(Some(begun.abort_handle()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = begun.await else { return };
            let Some(this) = weak.upgrade().filter(|t| t.open.get()) else {
                return;
            };
            match answer {
                Ok(sign_in) => {
                    this.show(&sign_in);
                    if this.open.get() {
                        this.wait(sign_in);
                    }
                }
                Err(e) => this.trouble(&e.to_string()),
            }
        });
    }

    /// Shows the code: as a QR code of the server's page when it has a web
    /// address, and always as text, with the server's fingerprint to match
    /// on the phone's page.
    fn show(self: &Rc<Self>, sign_in: &DeviceSignIn) {
        if self.via == Via::Browser {
            return self.show_in_browser(sign_in);
        }
        let texture = sign_in.web_url.as_deref().and_then(|url| {
            crate::qr::texture(url, crate::qr::MODULE, &accent(&self.dialog))
                .inspect_err(|e| tracing::warn!(error = %e, "cannot draw the QR code"))
                .ok()
        });
        self.qr.set_paintable(texture.as_ref());
        self.qr.set_visible(texture.is_some());
        match sign_in.web_url.as_deref().map(page_address) {
            Some(page) => {
                self.dialog.set_title("Sign in with your phone");
                self.lead.set_label("Scan with your phone, or open");
                self.page.set_label(&format!("{page}  and type"));
            }
            None => {
                self.dialog.set_title("Sign in with another device");
                self.lead.set_label(
                    "This server has no web address, so there's no QR. On a phone or \
                     computer that's already signed in, open",
                );
                self.page
                    .set_label("Preferences › Sign In a Device…  and type:");
            }
        }
        self.showing(sign_in);
    }

    /// Opens the page in the system browser, and shows the code (the page
    /// shows it too) and where the page is, should the browser not open.
    /// Without a page, the password form takes over.
    fn show_in_browser(self: &Rc<Self>, sign_in: &DeviceSignIn) {
        let Some(url) = sign_in.web_url.as_deref() else {
            // Closed now, not when the dialog says so: nothing waits.
            self.open.set(false);
            self.dialog.close();
            return (self.ends.no_web)();
        };
        (self.ends.open)(url);
        self.qr.set_visible(false);
        self.lead.set_label(
            "Sign in in your browser, then approve this computer. If the browser \
             did not open, go to",
        );
        self.page.set_label(url);
        self.showing(sign_in);
    }

    /// The code, the countdown, and the expired page's words.
    fn showing(self: &Rc<Self>, sign_in: &DeviceSignIn) {
        self.code.set_label(&sign_in.display_code);
        self.expired_code.set_label(&sign_in.display_code);
        self.expired_page.set_description(Some(&format!(
            "Codes last {} minutes.",
            sign_in.expires_in.as_secs().div_ceil(60)
        )));
        self.expires.set(Some(Instant::now() + sign_in.expires_in));
        self.tick();
        self.stack.set_visible_child_name("waiting");
        let weak: Weak<Self> = Rc::downgrade(self);
        glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(this) if this.open.get() && this.expires.get().is_some() => {
                this.tick();
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
    }

    fn tick(&self) {
        let left = self.expires.get().map_or(Duration::ZERO, |at| {
            at.saturating_duration_since(Instant::now())
        });
        self.countdown.set_label(&format!(
            "Waiting for approval · code expires in {}:{:02}",
            left.as_secs() / 60,
            left.as_secs() % 60
        ));
    }

    /// Waits for the phone.
    fn wait(self: &Rc<Self>, sign_in: DeviceSignIn) {
        let client = self.client.clone();
        let waited = relm4::spawn(async move { client.await_device_sign_in(&sign_in).await });
        self.call.replace(Some(waited.abort_handle()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = waited.await else { return };
            let Some(this) = weak.upgrade().filter(|t| t.open.get()) else {
                return;
            };
            this.expires.set(None);
            this.call.take();
            match answer {
                Ok(profile) => (this.ends.done)(profile),
                Err(BackendError::SignInExpired) => this.stack.set_visible_child_name("expired"),
                Err(BackendError::SignInDenied) => this.stack.set_visible_child_name("denied"),
                Err(e) => this.trouble(&e.to_string()),
            }
        });
    }

    fn trouble(&self, why: &str) {
        self.expires.set(None);
        self.problem.set_description(Some(why));
        self.stack.set_visible_child_name("problem");
    }

    /// Stops the call in flight: a begin no longer wanted, or a wait.
    fn stop(&self) {
        if let Some(call) = self.call.take() {
            call.abort();
        }
    }
}

/// The page's address as people type it: no scheme, no code (the code is
/// shown beside it).
fn page_address(url: &str) -> String {
    let bare = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    bare.split(['?', '#']).next().unwrap_or(bare).to_owned()
}

/// The theme's accent, for the reel in the QR code; libadwaita names it
/// `accent_color`.
#[allow(deprecated)]
fn accent(widget: &impl IsA<gtk::Widget>) -> gdk::RGBA {
    widget
        .style_context()
        .lookup_color("accent_color")
        .unwrap_or(gdk::RGBA::new(0.58, 0.886, 0.835, 1.0))
}

fn column() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .build()
}

fn wrapped(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(classes)
        .build()
}

fn code_label(classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .selectable(true)
        .css_classes(classes)
        .build()
}

fn pill(label: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    if suggested {
        button.add_css_class("suggested-action");
    }
    button
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::fake::{FakeClient, PhoneAnswer};
    use spoolframe_linux_core::testing::ready;

    use super::*;

    #[test]
    fn the_page_is_shown_as_people_type_it() {
        assert_eq!(
            page_address("https://home-media.lan/device?code=BCDFGHJK"),
            "home-media.lan/device"
        );
    }

    /// Runs the main loop until `done` or `limit`.
    fn run_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    /// What the screen handed back: the profile, whether it gave up for
    /// want of a web address, and the pages it opened in the browser.
    #[derive(Default)]
    struct Ended {
        profile: RefCell<Option<Profile>>,
        no_web: Cell<bool>,
        opened: RefCell<Vec<String>>,
    }

    /// The screen on the fake's Home Media, shown in a window, and what
    /// it handed back.
    fn open(client: FakeClient) -> (Rc<RemoteSignIn>, Rc<Ended>) {
        open_via(client, Via::Phone)
    }

    fn open_via(client: FakeClient, via: Via) -> (Rc<RemoteSignIn>, Rc<Ended>) {
        let client: Arc<dyn Client> = Arc::new(client);
        let server = ready(client.servers())
            .unwrap()
            .into_iter()
            .find(|s| s.pinned)
            .unwrap();
        let ended = Rc::new(Ended::default());
        let ends = Ends {
            done: {
                let ended = ended.clone();
                Rc::new(move |profile| {
                    ended.profile.replace(Some(profile));
                })
            },
            no_web: {
                let ended = ended.clone();
                Rc::new(move || ended.no_web.set(true))
            },
            open: {
                let ended = ended.clone();
                Rc::new(move |url| ended.opened.borrow_mut().push(url.to_owned()))
            },
        };
        let screen = RemoteSignIn::new(client, &server, via, ends);
        let window = adw::Window::builder()
            .default_width(800)
            .default_height(760)
            .build();
        window.present();
        run_until(Duration::from_secs(5), || window.is_mapped());
        screen.dialog.present(Some(&window));
        run_until(Duration::from_secs(5), || screen.dialog.is_mapped());
        (screen, ended)
    }

    fn showing_page(screen: &RemoteSignIn) -> Option<String> {
        screen.stack.visible_child_name().map(|n| n.to_string())
    }

    fn reaches(screen: &RemoteSignIn, page: &str) -> bool {
        run_until(Duration::from_secs(5), || {
            showing_page(screen).as_deref() == Some(page)
        })
    }

    /// One test: GTK binds itself to the first thread that starts it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn remote_sign_in_on_the_fake_server() {
        gtk::init().unwrap();
        adw::init().unwrap();
        an_approval_ends_as_a_password_sign_in();
        an_expired_code_is_replaced_on_asking();
        a_denial_offers_another_try();
        without_a_web_address_there_is_only_the_code();
        closing_the_screen_stops_waiting();
        the_browser_opens_the_page_and_the_screen_waits();
        without_a_web_address_the_browser_gives_way_to_the_password();
    }

    fn the_browser_opens_the_page_and_the_screen_waits() {
        let (screen, ended) = open_via(
            FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Wait),
            Via::Browser,
        );
        assert!(reaches(&screen, "waiting"));
        let url = "https://media.example.org/auth/device?code=BCDFGHJK";
        assert_eq!(*ended.opened.borrow(), [url]);
        assert!(!screen.qr.is_visible() && screen.code.label() == "BCDF-GHJK");
        screen.dialog.force_close();
    }

    fn without_a_web_address_the_browser_gives_way_to_the_password() {
        let (screen, ended) = open_via(
            FakeClient::new(None)
                .without_web_address()
                .answering_phone_sign_in(PhoneAnswer::Wait),
            Via::Browser,
        );
        assert!(run_until(Duration::from_secs(5), || ended.no_web.get()));
        assert!(
            ended.opened.borrow().is_empty() && screen.call.borrow().is_none(),
            "it opened a page, or waits"
        );
    }

    fn an_approval_ends_as_a_password_sign_in() {
        let (_screen, ended) = open(FakeClient::new(None));
        assert!(run_until(Duration::from_secs(5), || ended
            .profile
            .borrow()
            .is_some()));
        assert_eq!(ended.profile.borrow().as_ref().unwrap().username, "andrey");
    }

    fn an_expired_code_is_replaced_on_asking() {
        let (screen, ended) =
            open(FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Expire));
        assert!(reaches(&screen, "expired"), "an expired code was not said");
        assert_eq!(screen.expired_code.label(), "BCDF-GHJK");
        screen.new_code.emit_clicked();
        // A new code is asked for, and expires the same way.
        assert_eq!(showing_page(&screen).as_deref(), Some("asking"));
        assert!(reaches(&screen, "expired"));
        assert!(ended.profile.borrow().is_none());
    }

    fn a_denial_offers_another_try() {
        let (screen, ended) =
            open(FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Deny));
        assert!(reaches(&screen, "denied"), "the denial was not said");
        screen.try_again.emit_clicked();
        assert_eq!(showing_page(&screen).as_deref(), Some("asking"));
        assert!(reaches(&screen, "denied"));
        assert!(ended.profile.borrow().is_none());
    }

    fn without_a_web_address_there_is_only_the_code() {
        let (screen, _) = open(
            FakeClient::new(None)
                .without_web_address()
                .answering_phone_sign_in(PhoneAnswer::Wait),
        );
        assert!(reaches(&screen, "waiting"));
        assert!(!screen.qr.is_visible() && screen.code.label() == "BCDF-GHJK");
        screen.dialog.force_close();
    }

    fn closing_the_screen_stops_waiting() {
        let (screen, ended) =
            open(FakeClient::new(None).answering_phone_sign_in(PhoneAnswer::Wait));
        assert!(reaches(&screen, "waiting"), "the code was never shown");
        assert!(screen.qr.is_visible() && screen.page.label().starts_with("media.example.org"));
        assert!(screen.call.borrow().is_some(), "not waiting");
        screen.dialog.force_close();
        assert!(run_until(Duration::from_secs(2), || screen
            .call
            .borrow()
            .is_none()));
        assert!(ended.profile.borrow().is_none());
    }
}
