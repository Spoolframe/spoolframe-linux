//! Passkeys on a security key or a phone (P3-19), laid out as the
//! design's LinuxPasskeyChoose, LinuxPasskeyKey and LinuxPasskeyPhone
//! boards.
//!
//! [`KeyPage`] is the Security key dialog's body, its states one at a
//! time: waiting for a key, touch it, its PIN, and why it did not work.
//! It runs a ceremony of [`spoolframe_linux_core::passkey`] on a thread,
//! for signing in here and for adding a passkey in Preferences.
//! [`PasskeySignIn`] is Sign in with a passkey: which kind, then the key.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{
    BackendError, Client, FinishPasskeyRegistration, FinishPasskeySignIn, PasskeyCreation,
    PasskeyRequest, Profile, Server,
};
use spoolframe_linux_core::passkey::{self, KeyError, Prompt, Prompter, Via};

/// What a key is asked to do.
pub enum Ceremony {
    SignIn(PasskeyRequest),
    /// Make a passkey, named so for the server.
    Register(PasskeyCreation, String),
}

/// What a key did.
pub enum Done {
    Signed(FinishPasskeySignIn),
    Made(FinishPasskeyRegistration),
}

/// What the dialog shows, one at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyState {
    Waiting,
    Touch,
    /// The phone flow's code to scan, a `FIDO:/…` URL.
    Scan(String),
    /// Reaching the phone over Bluetooth and the tunnel.
    Connecting,
    ConfirmOnPhone,
    Pin {
        wrong: bool,
    },
    /// The key is done; the server is checking.
    Working(&'static str),
    Failed(KeyError),
    /// The server refused what the key signed, in its words.
    Refused(String),
}

/// A state's words and buttons, as the boards have them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateText {
    pub title: String,
    pub body: String,
    /// Accent, warning or error, for the key's icon.
    pub tone: &'static str,
    pub spinner: Option<&'static str>,
    pub blink: bool,
    pub pin: bool,
    /// The main button's label, if any.
    pub button: Option<&'static str>,
    /// Another way, as a second button: "Use a security key".
    pub alternative: Option<&'static str>,
    /// Whether "Use your password instead" shows.
    pub password: bool,
}

impl KeyState {
    pub fn text(&self, server: &str, via: Via) -> StateText {
        let base = StateText {
            title: String::new(),
            body: String::new(),
            tone: "accent",
            spinner: None,
            blink: false,
            pin: false,
            button: None,
            alternative: None,
            password: false,
        };
        let failed = |tone, title: &str, body: String, button| StateText {
            tone,
            title: title.to_owned(),
            body,
            button: Some(button),
            password: true,
            ..base.clone()
        };
        let phone = via == Via::Phone;
        match self {
            // Plugged in only: NFC readers are not read yet.
            KeyState::Waiting => StateText {
                title: "Insert your security key".into(),
                body: "Plug it into a USB port.".into(),
                spinner: Some("Waiting for a key…"),
                ..base
            },
            KeyState::Touch => StateText {
                title: "Touch your key".into(),
                body: "Its light is blinking. Touch the metal contact or button.".into(),
                blink: true,
                ..base
            },
            KeyState::Scan(_) => StateText {
                title: "Scan with your phone’s camera".into(),
                body: format!("Your phone offers its passkey for {server}."),
                spinner: Some("Bluetooth must be on, on both"),
                ..base
            },
            KeyState::Connecting => StateText {
                title: "Connecting to your phone…".into(),
                body: "Keep your phone near this computer.".into(),
                spinner: Some("Over Bluetooth"),
                ..base
            },
            KeyState::ConfirmOnPhone => StateText {
                title: "Confirm on your phone".into(),
                body: "Use Face ID, your fingerprint or your phone’s PIN there.".into(),
                blink: true,
                ..base
            },
            KeyState::Pin { wrong } => StateText {
                title: "Enter your key’s PIN".into(),
                body: if *wrong {
                    "That PIN isn’t right. Too many wrong ones lock the key.".into()
                } else {
                    "The PIN you set on the key itself, not your profile PIN.".into()
                },
                tone: if *wrong { "error" } else { "accent" },
                pin: true,
                button: Some("Continue"),
                ..base
            },
            KeyState::Working(what) => StateText {
                title: (*what).into(),
                spinner: Some("A moment…"),
                ..base
            },
            KeyState::Refused(why) => failed("error", "That didn’t work", why.clone(), "Try again"),
            KeyState::Failed(error) => match error {
                KeyError::Locked => failed(
                    "error",
                    "This key is locked",
                    "Too many wrong PINs. It has to be reset with its maker’s tool, which \
                     removes every passkey on it."
                        .into(),
                    "Try another key",
                ),
                KeyError::Replug => failed(
                    "error",
                    "Unplug your key",
                    "Too many wrong PINs in a row. Unplug the key, plug it back in, and try \
                     again."
                        .into(),
                    "Try again",
                ),
                KeyError::NoPasskey if phone => failed(
                    "warning",
                    &format!("No passkey for {server} on this phone"),
                    "Your phone has no passkey for this server. Sign in another way and add one \
                     in Preferences › Profile."
                        .into(),
                    "Try again",
                ),
                KeyError::NoPasskey => failed(
                    "warning",
                    &format!("No passkey for {server} on this key"),
                    "This key has no passkey for this server. Try another key, or sign in \
                     another way and add this one in Preferences › Profile."
                        .into(),
                    "Try another key",
                ),
                KeyError::CannotKeep { full: true } => failed(
                    "warning",
                    "This key is full",
                    "It has no room for another passkey. Remove one with its maker’s tool, or use \
                     another key."
                        .into(),
                    "Try another key",
                ),
                // A passkey it could not find again would never sign in:
                // signing in does not ask for a name.
                KeyError::CannotKeep { full: false } => failed(
                    "warning",
                    "This key can’t keep a passkey",
                    "It can only sign in when told which passkey to use, and Spoolframe signs in \
                     without asking your name. Use another key, or your phone."
                        .into(),
                    "Try another key",
                ),
                KeyError::AlreadyMade => failed(
                    "warning",
                    if phone {
                        "Your phone already has a passkey"
                    } else {
                        "This key already has a passkey"
                    },
                    format!("It already signs you in to {server}."),
                    "Try again",
                ),
                // A phone too far away also ends here: the library just
                // waits for its Bluetooth signal.
                KeyError::TimedOut if phone => failed(
                    "warning",
                    "Nothing happened",
                    "The code wasn’t scanned in time.".into(),
                    "Show a new code",
                ),
                KeyError::TimedOut => failed(
                    "warning",
                    "Nothing happened",
                    "No key was touched in time.".into(),
                    "Try again",
                ),
                KeyError::NoDevice => failed(
                    "warning",
                    "No security key found",
                    "Plug in a FIDO2 security key. If one is plugged in, this computer may not \
                     let Spoolframe open it."
                        .into(),
                    "Try again",
                ),
                KeyError::NoBluetooth => StateText {
                    button: None,
                    alternative: Some("Use a security key"),
                    ..failed(
                        "warning",
                        "Bluetooth is off",
                        "Your phone proves it’s nearby over Bluetooth. Turn it on in Settings, or \
                         use another way."
                            .into(),
                        "Try again",
                    )
                },
                KeyError::Cancelled if phone => failed(
                    "warning",
                    "Cancelled on your phone",
                    "Nothing was shared.".into(),
                    "Try again",
                ),
                KeyError::Cancelled => failed(
                    "warning",
                    "Cancelled",
                    "The key stopped before it was done.".into(),
                    "Try again",
                ),
                KeyError::Other(why) => {
                    failed("error", "That didn’t work", why.clone(), "Try again")
                }
            },
        }
    }
}

/// The Security key dialog's body.
pub struct KeyPage {
    pub widget: gtk::Box,
    server: String,
    icon: gtk::Image,
    blink: gtk::Box,
    title: gtk::Label,
    body: gtk::Label,
    spinner_row: gtk::Box,
    spinner_label: gtk::Label,
    pin_box: gtk::Box,
    pin: gtk::PasswordEntry,
    /// The phone flow's code.
    qr: gtk::Picture,
    button: gtk::Button,
    alternative: gtk::Button,
    password: gtk::Button,
    /// Where the run in flight goes: a key or a phone.
    via: Cell<Via>,
    alternative_action: RefCell<Option<Rc<dyn Fn()>>>,
    /// Whether "Use your password instead" is offered at all.
    offers_password: bool,
    state: RefCell<KeyState>,
    /// The PIN the ceremony waits for, when it asks.
    pin_reply: RefCell<Option<std::sync::mpsc::Sender<Option<String>>>>,
    /// Bumped by each run and by stopping: a stopped run's news is ignored.
    run: Cell<u64>,
    /// The main button outside the PIN state: try again, another key.
    retry: RefCell<Option<Rc<dyn Fn()>>>,
}

impl KeyPage {
    pub fn new(server: &str, use_password: Option<Rc<dyn Fn()>>) -> Rc<Self> {
        let icon = gtk::Image::builder()
            .icon_name(crate::icons::SECURITY_KEY)
            .pixel_size(32)
            .build();
        let blink = gtk::Box::builder()
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .visible(false)
            .css_classes(["key-blink"])
            .build();
        let tile = gtk::Overlay::builder()
            .child(
                &gtk::Box::builder()
                    .width_request(64)
                    .height_request(64)
                    .halign(gtk::Align::Center)
                    .css_classes(["key-tile"])
                    .build(),
            )
            .halign(gtk::Align::Center)
            .build();
        icon.set_halign(gtk::Align::Center);
        icon.set_valign(gtk::Align::Center);
        tile.add_overlay(&icon);
        tile.add_overlay(&blink);
        let title = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(1)
            .hexpand(true)
            .css_classes(["key-title"])
            .build();
        let body = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(1)
            .hexpand(true)
            .css_classes(["dim-label", "key-body"])
            .build();
        let spinner_label = gtk::Label::new(None);
        let spinner_row = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .css_classes(["dim-label"])
            .build();
        spinner_row.append(&gtk::Spinner::builder().spinning(true).build());
        spinner_row.append(&spinner_label);
        let pin = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .css_classes(["field"])
            .build();
        let pin_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        pin_box.append(
            &gtk::Label::builder()
                .label("Security key PIN")
                .xalign(0.0)
                .css_classes(["field-label"])
                .build(),
        );
        pin_box.append(&pin);
        let button = gtk::Button::builder()
            .css_classes(["action", "suggested-action"])
            .build();
        let alternative = gtk::Button::builder()
            .css_classes(["action", "secondary"])
            .build();
        let qr = gtk::Picture::builder()
            .can_shrink(false)
            .halign(gtk::Align::Center)
            .visible(false)
            .css_classes(["qr-code"])
            .build();
        let password = gtk::Button::builder()
            .label("Use your password instead")
            .halign(gtk::Align::Center)
            .css_classes(["flat", "text-link"])
            .build();
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .margin_top(24)
            .margin_bottom(26)
            .margin_start(26)
            .margin_end(26)
            .build();
        for child in [
            qr.upcast_ref::<gtk::Widget>(),
            tile.upcast_ref(),
            title.upcast_ref(),
            pin_box.upcast_ref(),
            body.upcast_ref(),
            spinner_row.upcast_ref(),
            button.upcast_ref(),
            alternative.upcast_ref(),
            password.upcast_ref(),
        ] {
            widget.append(child);
        }
        let offers_password = use_password.is_some();
        let page = Rc::new(Self {
            widget,
            server: server.to_owned(),
            icon,
            blink,
            title,
            body,
            spinner_row,
            spinner_label,
            pin_box,
            pin,
            qr,
            button,
            alternative,
            password,
            via: Cell::new(Via::Key),
            alternative_action: RefCell::new(None),
            offers_password,
            state: RefCell::new(KeyState::Waiting),
            pin_reply: RefCell::new(None),
            run: Cell::new(0),
            retry: RefCell::new(None),
        });
        {
            let weak = Rc::downgrade(&page);
            page.button.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.press();
                }
            });
            let weak = Rc::downgrade(&page);
            page.alternative.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    let action = page.alternative_action.borrow().clone();
                    if let Some(action) = action {
                        action();
                    }
                }
            });
            let weak = Rc::downgrade(&page);
            page.pin.connect_activate(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.press();
                }
            });
        }
        if let Some(use_password) = use_password {
            page.password.connect_clicked(move |_| use_password());
        }
        page.show(KeyState::Waiting);
        page
    }

    /// What the main button does after a failure.
    pub fn set_retry(&self, retry: Rc<dyn Fn()>) {
        self.retry.replace(Some(retry));
    }

    /// What "Use a security key" does, when the phone has no Bluetooth.
    pub fn set_alternative(&self, action: Rc<dyn Fn()>) {
        self.alternative_action.replace(Some(action));
    }

    /// Where the run in flight goes.
    pub fn via(&self) -> Via {
        self.via.get()
    }

    pub fn show(&self, state: KeyState) {
        let text = restart_hint(
            &state,
            &self.server,
            self.via.get(),
            crate::sandbox::keys_at_start(),
        );
        // The phone's code instead of the key's tile.
        match &state {
            KeyState::Scan(url) => {
                match crate::qr::texture(url, crate::qr::MODULE) {
                    Ok(texture) => self.qr.set_paintable(Some(&texture)),
                    Err(e) => tracing::warn!(error = %e, "cannot draw the passkey code"),
                }
                self.qr.set_visible(true);
            }
            _ => self.qr.set_visible(false),
        }
        self.icon.set_icon_name(Some(match self.via.get() {
            Via::Key => crate::icons::SECURITY_KEY,
            Via::Phone => crate::icons::PHONE,
        }));
        if let Some(tile) = self.icon.parent() {
            tile.set_visible(!matches!(state, KeyState::Scan(_)));
        }
        for tone in ["accent", "warning", "error"] {
            self.icon.remove_css_class(tone);
        }
        self.icon.add_css_class(text.tone);
        self.blink.set_visible(text.blink);
        self.title.set_label(&text.title);
        self.body.set_label(&text.body);
        self.body.set_visible(!text.body.is_empty());
        if text.tone == "error" && text.pin {
            self.body.add_css_class("error");
        } else {
            self.body.remove_css_class("error");
        }
        self.spinner_row.set_visible(text.spinner.is_some());
        self.spinner_label
            .set_label(text.spinner.unwrap_or_default());
        self.pin_box.set_visible(text.pin);
        if text.pin {
            self.pin.set_text("");
            self.pin.grab_focus();
        }
        self.button.set_visible(text.button.is_some());
        self.button.set_label(text.button.unwrap_or_default());
        self.alternative.set_visible(text.alternative.is_some());
        self.alternative
            .set_label(text.alternative.unwrap_or_default());
        // A failure's button is the secondary one, as on the boards,
        // except Try again after a timeout.
        let primary = text.pin
            || matches!(
                state,
                KeyState::Failed(KeyError::TimedOut | KeyError::Cancelled)
            );
        if primary {
            self.button.add_css_class("suggested-action");
            self.button.remove_css_class("secondary");
        } else {
            self.button.remove_css_class("suggested-action");
            self.button.add_css_class("secondary");
        }
        self.password
            .set_visible(text.password && self.offers_password);
        self.state.replace(state);
    }

    fn press(&self) {
        let pinning = matches!(*self.state.borrow(), KeyState::Pin { .. });
        if pinning {
            let reply = self.pin_reply.take();
            let pin = self.pin.text().to_string();
            if let Some(reply) = reply {
                let _ = reply.send(Some(pin));
            }
            self.show(KeyState::Touch);
            return;
        }
        let retry = self.retry.borrow().clone();
        if let Some(retry) = retry {
            retry();
        }
    }

    /// Stops listening to the run in flight, and gives up a PIN it waits
    /// for, which ends it. The key may still blink until it times out.
    pub fn stop(&self) {
        self.run.set(self.run.get() + 1);
        if let Some(reply) = self.pin_reply.take() {
            let _ = reply.send(None);
        }
    }

    /// Runs `ceremony` on a key, showing its states; `done` gets what the
    /// key did. Failures show here, with the retry button.
    pub fn run(self: &Rc<Self>, ceremony: Ceremony, via: Via, done: Rc<dyn Fn(Done)>) {
        self.stop();
        self.via.set(via);
        let run = self.run.get();
        self.show(match via {
            Via::Key => KeyState::Waiting,
            Via::Phone => KeyState::Connecting,
        });
        let (events, mut heard) = tokio::sync::mpsc::unbounded_channel();
        let prompter: Arc<dyn Prompter> = Arc::new(ToDialog(events.clone()));
        std::thread::spawn(move || {
            let result = match ceremony {
                Ceremony::SignIn(request) => {
                    passkey::sign_in(&request, via, prompter).map(Done::Signed)
                }
                Ceremony::Register(creation, name) => {
                    passkey::register(&creation, &name, via, prompter).map(Done::Made)
                }
            };
            let _ = events.send(Event::Finished(result));
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Some(event) = heard.recv().await {
                let Some(page) = weak.upgrade().filter(|p| p.run.get() == run) else {
                    return;
                };
                match event {
                    Event::Prompt(Prompt::WaitingForKey) => page.show(KeyState::Waiting),
                    Event::Prompt(Prompt::Touch) => page.show(KeyState::Touch),
                    // Processing: the key is working on its touch, or the
                    // phone on its confirmation; neither needs a new look.
                    Event::Prompt(Prompt::Processing) => {}
                    Event::Prompt(Prompt::Scan(url)) => page.show(KeyState::Scan(url)),
                    Event::Prompt(Prompt::Connecting) => page.show(KeyState::Connecting),
                    Event::Prompt(Prompt::ConfirmOnPhone) => page.show(KeyState::ConfirmOnPhone),
                    Event::Pin { wrong, reply } => {
                        page.pin_reply.replace(Some(reply));
                        page.show(KeyState::Pin { wrong });
                    }
                    Event::Finished(Ok(result)) => {
                        page.show(KeyState::Working(match result {
                            Done::Signed(_) => "Signing in…",
                            Done::Made(_) => "Saving the passkey…",
                        }));
                        done(result);
                        return;
                    }
                    Event::Finished(Err(error)) => {
                        page.show(KeyState::Failed(error));
                        return;
                    }
                }
            }
        });
    }
}

enum Event {
    Prompt(Prompt),
    Pin {
        wrong: bool,
        reply: std::sync::mpsc::Sender<Option<String>>,
    },
    Finished(Result<Done, KeyError>),
}

/// The ceremony's questions, onto the dialog's main loop.
struct ToDialog(tokio::sync::mpsc::UnboundedSender<Event>);

impl Prompter for ToDialog {
    fn show(&self, prompt: Prompt) {
        let _ = self.0.send(Event::Prompt(prompt));
    }

    fn pin(&self, wrong: bool) -> Option<String> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.0.send(Event::Pin { wrong, reply }).ok()?;
        answer.recv().ok().flatten()
    }
}

/// What Sign in with a passkey hands back to the app.
pub struct Ends {
    /// Signed in: goes on as a password sign-in does.
    pub done: Rc<dyn Fn(Profile)>,
}

/// Sign in with a passkey: which kind, then the key.
pub struct PasskeySignIn {
    pub dialog: adw::Dialog,
    client: Arc<dyn Client>,
    server: Server,
    username: Option<String>,
    ends: Ends,
    stack: gtk::Stack,
    header_title: adw::WindowTitle,
    back: gtk::Button,
    key: Rc<KeyPage>,
    remember: gtk::CheckButton,
}

impl PasskeySignIn {
    /// `username`, when typed in the sign-in form, narrows the passkeys
    /// to that user's. `remember` is the sign-in form's box, shown again
    /// here (P4-02).
    pub fn new(
        client: Arc<dyn Client>,
        server: &Server,
        username: Option<String>,
        remember: bool,
        ends: Ends,
    ) -> Rc<Self> {
        let dialog = adw::Dialog::builder()
            .title("Sign in with a passkey")
            .content_width(360)
            .build();
        let use_password: Rc<dyn Fn()> = {
            let weak = dialog.downgrade();
            Rc::new(move || {
                if let Some(dialog) = weak.upgrade() {
                    dialog.close();
                }
            })
        };
        let key = KeyPage::new(&server.name, Some(use_password.clone()));
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .vhomogeneous(false)
            .build();
        let header_title = adw::WindowTitle::new("Sign in with a passkey", "");
        let back = gtk::Button::builder()
            .label("Cancel")
            .css_classes(["flat", "dialog-cancel"])
            .build();
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .title_widget(&header_title)
            .css_classes(["dialog-header"])
            .build();
        header.pack_start(&back);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        dialog.set_child(Some(&toolbar));
        let screen = Rc::new(Self {
            dialog,
            client,
            server: server.clone(),
            username: username.filter(|u| !u.trim().is_empty()),
            ends,
            stack,
            header_title,
            back,
            key,
            remember: crate::start::remember_box(),
        });
        screen.remember.set_active(remember);
        screen
            .stack
            .add_named(&screen.choose_page(use_password), Some("choose"));
        screen.stack.add_named(&screen.key.widget, Some("key"));
        {
            let weak = Rc::downgrade(&screen);
            screen.back.connect_clicked(move |_| {
                if let Some(screen) = weak.upgrade() {
                    screen.go_back();
                }
            });
            let weak = Rc::downgrade(&screen);
            screen.key.set_retry(Rc::new(move || {
                if let Some(screen) = weak.upgrade() {
                    screen.with(screen.key.via());
                }
            }));
            let weak = Rc::downgrade(&screen);
            screen.key.set_alternative(Rc::new(move || {
                if let Some(screen) = weak.upgrade() {
                    screen.with(Via::Key);
                }
            }));
            let weak = Rc::downgrade(&screen);
            screen.dialog.connect_closed(move |_| {
                if let Some(screen) = weak.upgrade() {
                    screen.key.stop();
                }
            });
        }
        screen
    }

    /// Which kind: a security key, or a phone.
    fn choose_page(self: &Rc<Self>, use_password: Rc<dyn Fn()>) -> gtk::Box {
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .margin_top(24)
            .margin_bottom(26)
            .margin_start(26)
            .margin_end(26)
            .build();
        let options = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .build();
        let key = option(
            crate::icons::SECURITY_KEY,
            "Security key",
            "A key such as a YubiKey, by USB",
        );
        {
            let weak: Weak<Self> = Rc::downgrade(self);
            key.connect_clicked(move |_| {
                if let Some(screen) = weak.upgrade() {
                    screen.with(Via::Key);
                }
            });
        }
        options.append(&key);
        let phone = option(
            crate::icons::PHONE,
            "Phone or tablet",
            "A passkey saved on your phone: scan a code, Bluetooth on",
        );
        {
            let weak: Weak<Self> = Rc::downgrade(self);
            phone.connect_clicked(move |_| {
                if let Some(screen) = weak.upgrade() {
                    screen.with(Via::Phone);
                }
            });
        }
        options.append(&phone);
        page.append(&options);
        page.append(&crate::start::remember_row(&self.remember));
        page.append(
            &gtk::Label::builder()
                .label(
                    "This computer can’t keep passkeys itself, so the passkey comes from a key \
                     or your phone.",
                )
                .wrap(true)
                .max_width_chars(1)
                .hexpand(true)
                .justify(gtk::Justification::Center)
                .css_classes(["dim-label", "key-body"])
                .build(),
        );
        let password = gtk::Button::builder()
            .label("Use your password instead")
            .halign(gtk::Align::Center)
            .css_classes(["flat", "text-link"])
            .build();
        password.connect_clicked(move |_| use_password());
        page.append(&password);
        page
    }

    fn go_back(&self) {
        if self.stack.visible_child_name().as_deref() == Some("key") {
            self.key.stop();
            self.stack.set_visible_child_name("choose");
            self.header_title.set_title("Sign in with a passkey");
            self.back.set_label("Cancel");
        } else {
            self.dialog.close();
        }
    }

    /// Asks the server what to sign, then the key or phone to sign it.
    fn with(self: &Rc<Self>, via: Via) {
        self.stack.set_visible_child_name("key");
        self.header_title.set_title(match via {
            Via::Key => "Security key",
            Via::Phone => "Phone or tablet",
        });
        self.back.set_label("Back");
        self.key.show(KeyState::Working("Asking the server…"));
        let (client, key, username) = (
            self.client.clone(),
            self.server.key.clone(),
            self.username.clone(),
        );
        let begun = relm4::spawn(async move {
            client
                .begin_passkey_sign_in(&key, username.as_deref())
                .await
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let begun = begun.await;
            let Some(screen) = weak.upgrade() else { return };
            match begun {
                Ok(Ok(request)) => {
                    let done: Rc<dyn Fn(Done)> = {
                        let weak = Rc::downgrade(&screen);
                        Rc::new(move |done| {
                            if let (Some(screen), Done::Signed(signed)) = (weak.upgrade(), done) {
                                screen.finish(signed);
                            }
                        })
                    };
                    screen.key.run(Ceremony::SignIn(request), via, done);
                }
                Ok(Err(e)) => screen.key.show(KeyState::Refused(e.to_string())),
                Err(e) => screen.key.show(KeyState::Refused(e.to_string())),
            }
        });
    }

    /// Hands the server what the key signed.
    fn finish(self: &Rc<Self>, signed: FinishPasskeySignIn) {
        let (client, key, remember) = (
            self.client.clone(),
            self.server.key.clone(),
            self.remember.is_active(),
        );
        let finished =
            relm4::spawn(
                async move { client.finish_passkey_sign_in(&key, signed, remember).await },
            );
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let finished = finished.await;
            let Some(screen) = weak.upgrade() else { return };
            match finished {
                Ok(Ok(profile)) => (screen.ends.done)(profile),
                Ok(Err(BackendError::WrongCredentials)) => {
                    screen.key.show(KeyState::Refused(format!(
                        "That passkey doesn’t sign in to {}. It may have been removed.",
                        screen.server.name
                    )))
                }
                Ok(Err(e)) => screen.key.show(KeyState::Refused(e.to_string())),
                Err(e) => screen.key.show(KeyState::Refused(e.to_string())),
            }
        });
    }
}

/// One kind of passkey to choose: its icon, name and what it is.
pub(crate) fn option(icon: &str, name: &str, detail: &str) -> gtk::Button {
    let row = gtk::Box::builder().spacing(14).build();
    row.append(
        &gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(20)
            .width_request(40)
            .height_request(40)
            .css_classes(["passkey-option-icon"])
            .build(),
    );
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    text.append(
        &gtk::Label::builder()
            .label(detail)
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(1)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    row.append(&text);
    row.append(&gtk::Image::from_icon_name(crate::icons::RIGHT));
    gtk::Button::builder()
        .child(&row)
        .css_classes(["passkey-option"])
        .build()
}

/// `state`'s words, unless the app cannot reach the key. Confined (C-15),
/// it opens only the security keys plugged in when it started
/// (`keys_at_start`, `None` when not confined): Landlock refuses one
/// plugged in later, which the key library reports as no key, as no key in
/// time, or as no permission (which [`KeyError::NoBluetooth`] stands for).
/// Then the dialog says to reopen the app with the key plugged in.
fn restart_hint(
    state: &KeyState,
    server: &str,
    via: Via,
    keys_at_start: Option<usize>,
) -> StateText {
    const RESTART: &str = "Spoolframe can only use a security key that was plugged in when it \
                           started. Plug in your key, then quit and reopen Spoolframe.";
    let text = state.text(server, via);
    let Some(keys) = keys_at_start.filter(|_| via == Via::Key) else {
        return text;
    };
    match state {
        KeyState::Waiting if keys == 0 => StateText {
            title: "Plug in your key, then reopen Spoolframe".into(),
            body: RESTART.into(),
            spinner: None,
            ..text
        },
        KeyState::Failed(KeyError::TimedOut) if keys == 0 => StateText {
            body: RESTART.into(),
            ..text
        },
        KeyState::Failed(KeyError::NoDevice | KeyError::NoBluetooth) => StateText {
            body: RESTART.into(),
            ..KeyState::Failed(KeyError::NoDevice).text(server, via)
        },
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confined_without_a_key_at_start_the_dialog_says_to_reopen_the_app() {
        for state in [
            KeyState::Waiting,
            KeyState::Failed(KeyError::TimedOut),
            KeyState::Failed(KeyError::NoDevice),
            // A refused hidraw node reads as no permission.
            KeyState::Failed(KeyError::NoBluetooth),
        ] {
            let text = restart_hint(&state, "Home Media", Via::Key, Some(0));
            assert!(text.body.contains("quit and reopen"), "{state:?}: {text:?}");
        }
    }

    #[test]
    fn unconfined_or_by_phone_the_dialog_is_as_before() {
        for (via, keys) in [(Via::Key, None), (Via::Phone, Some(0))] {
            let state = KeyState::Failed(KeyError::NoBluetooth);
            assert_eq!(
                restart_hint(&state, "Home Media", via, keys),
                state.text("Home Media", via)
            );
        }
        let waiting = restart_hint(&KeyState::Waiting, "Home Media", Via::Key, Some(1));
        assert_eq!(waiting, KeyState::Waiting.text("Home Media", Via::Key));
    }

    #[test]
    fn a_wrong_pin_says_so_and_keeps_the_field() {
        let text = KeyState::Pin { wrong: true }.text("Home Media", Via::Key);
        assert_eq!(
            (
                text.pin,
                text.tone,
                text.body.starts_with("That PIN isn’t right")
            ),
            (true, "error", true)
        );
    }

    #[test]
    fn a_key_without_this_servers_passkey_names_the_server() {
        let text = KeyState::Failed(KeyError::NoPasskey).text("Home Media", Via::Key);
        assert_eq!(
            (text.title.as_str(), text.button, text.password),
            (
                "No passkey for Home Media on this key",
                Some("Try another key"),
                true
            )
        );
    }

    /// One test: GTK binds itself to the first thread that starts it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn passkey_dialogs_on_a_display() {
        gtk::init().unwrap();
        adw::init().unwrap();
        every_key_state_shows();
        sign_in_opens_on_which_kind();
        touch_your_key_lays_out_with_the_apps_stylesheet();
    }

    /// GTK's warnings that a widget measured below zero, from here on.
    fn negative_sizes() -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = seen.clone();
        glib::log_set_writer_func(move |level, fields| {
            let message = fields
                .iter()
                .find(|f| f.key() == "MESSAGE")
                .and_then(|f| f.value_str())
                .unwrap_or_default();
            if message.contains("sizes must be >= 0") {
                kept.lock().unwrap().push(message.to_owned());
            }
            glib::log_writer_default(level, fields)
        });
        seen
    }

    fn touch_your_key_lays_out_with_the_apps_stylesheet() {
        let css = gtk::CssProvider::new();
        css.load_from_string(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let seen = negative_sizes();
        let page = KeyPage::new("Home Media", None);
        page.show(KeyState::Touch);
        let window = gtk::Window::builder().child(&page.widget).build();
        window.present();
        let context = glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < until {
            context.iteration(false);
        }
        window.destroy();
        assert_eq!(*seen.lock().unwrap(), Vec::<String>::new());
    }

    fn every_key_state_shows() {
        let page = KeyPage::new("Home Media", Some(Rc::new(|| {})));
        for state in [
            KeyState::Waiting,
            KeyState::Touch,
            KeyState::Pin { wrong: false },
            KeyState::Pin { wrong: true },
            KeyState::Working("Signing in…"),
            KeyState::Refused("no".into()),
            KeyState::Failed(KeyError::Locked),
            KeyState::Failed(KeyError::Replug),
            KeyState::Failed(KeyError::NoPasskey),
            KeyState::Failed(KeyError::AlreadyMade),
            KeyState::Failed(KeyError::CannotKeep { full: true }),
            KeyState::Failed(KeyError::CannotKeep { full: false }),
            KeyState::Failed(KeyError::TimedOut),
            KeyState::Failed(KeyError::NoDevice),
            KeyState::Failed(KeyError::Cancelled),
            KeyState::Failed(KeyError::Other("odd".into())),
            KeyState::Failed(KeyError::NoBluetooth),
            KeyState::Scan("FIDO:/0123456789012345678901234567890123456789".into()),
            KeyState::Connecting,
            KeyState::ConfirmOnPhone,
        ] {
            let pin = matches!(state, KeyState::Pin { .. });
            page.show(state);
            assert_eq!(page.pin_box.is_visible(), pin);
        }
    }

    fn sign_in_opens_on_which_kind() {
        use spoolframe_linux_core::fake::FakeClient;
        let client: Arc<dyn Client> = Arc::new(FakeClient::new(None));
        let server = first_server(&client);
        let screen = PasskeySignIn::new(
            client,
            &server,
            None,
            true,
            Ends {
                done: Rc::new(|_| {}),
            },
        );
        assert_eq!(screen.stack.visible_child_name().as_deref(), Some("choose"));
        assert!(
            screen.remember.is_active(),
            "the sign-in form's Remember this device was not carried over"
        );
    }

    /// The fake's first server, without a runtime: its list is ready.
    fn first_server(client: &Arc<dyn Client>) -> Server {
        let mut servers = client.servers();
        let waker = std::task::Waker::noop();
        match servers
            .as_mut()
            .poll(&mut std::task::Context::from_waker(waker))
        {
            std::task::Poll::Ready(Ok(servers)) => servers.into_iter().next().unwrap(),
            _ => panic!("the fake lists its servers at once"),
        }
    }

    #[test]
    fn a_phone_that_timed_out_offers_a_new_code() {
        assert_eq!(
            KeyState::Failed(KeyError::TimedOut)
                .text("Home Media", Via::Phone)
                .button,
            Some("Show a new code")
        );
    }

    #[test]
    fn without_bluetooth_a_security_key_is_offered() {
        let text = KeyState::Failed(KeyError::NoBluetooth).text("Home Media", Via::Phone);
        assert_eq!(
            (text.alternative, text.button, text.password),
            (Some("Use a security key"), None, true)
        );
    }

    #[test]
    fn a_locked_key_offers_another_key() {
        assert_eq!(
            KeyState::Failed(KeyError::Locked)
                .text("Home Media", Via::Key)
                .button,
            Some("Try another key")
        );
    }
}
