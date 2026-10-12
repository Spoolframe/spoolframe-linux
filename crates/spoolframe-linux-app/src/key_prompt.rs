//! The key prompt (P7-08), as on the LinuxKeyPrompt board: one "Security
//! key" dialog for every root action. It names the action, asks for the
//! key's PIN when the key wants it, says when to touch, and says what
//! went wrong in the key's own terms, with Try again.
//!
//! The key works on a blocking thread (libfido2); the dialog lives on the
//! main loop. [`Prompt`] is what the key holds: it hands each PIN request
//! to the dialog on the main loop and waits for the answer.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use crate::dialog::{buttons, column};
use adw::prelude::*;
use gtk::glib;
use gtk::glib::thread_guard::ThreadGuard;
use spoolframe_client_sdk::{SdkError, SecurityKeyError};
use spoolframe_linux_core::identity::Identities;
use spoolframe_linux_core::security_key::{CANCELLED, NO_KEY, PinAsk, PinPrompt, Step};

/// The root action the prompt is for, in its words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Action {
    /// "Touch your key to pair Living-room TV".
    pub touch: String,
    /// "Any of your keys: Keyring key or Drawer key."
    pub keys: Option<String>,
    /// The identity's name, for "This key isn't one of Alice's".
    pub identity: Option<String>,
    /// Another key that can still act, for the locked key's card.
    pub other_key: Option<String>,
    /// What to say when what the action looked for is not there (the
    /// SDK's NotFound): recovery's server that knows no identity on the
    /// key.
    pub not_found: Option<String>,
}

/// "Keyring key or Drawer key", "Keyring key, Drawer key or Spare key".
pub fn or_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// "Wrong PIN. 5 tries left before the key locks."
pub fn wrong_pin_line(tries_left: Option<u32>) -> String {
    match tries_left {
        Some(1) => "Wrong PIN. 1 try left before the key locks.".to_owned(),
        Some(n) => format!("Wrong PIN. {n} tries left before the key locks."),
        None => "Wrong PIN.".to_owned(),
    }
}

/// What an error page says: its title, its words, whether Try again is
/// offered (Locked has only OK), and the other key's card for a locked
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trouble {
    pub title: String,
    pub body: String,
    pub retry: bool,
    /// The action ends here for good: OK, not Cancel (a locked key).
    pub for_good: bool,
    pub other_key: Option<String>,
}

/// The board's error pages, from what the key or the SDK said; `None`
/// for a cancel, which closes the prompt without a word.
pub fn trouble(error: &SdkError, action: &Action) -> Option<Trouble> {
    let page = |title: &str, body: String, retry: bool| Trouble {
        title: title.to_owned(),
        body,
        retry,
        for_good: false,
        other_key: None,
    };
    Some(match error {
        SdkError::SecurityKey(key) => match key {
            SecurityKeyError::WrongPin => page(
                "Wrong PIN",
                "The key took none of the PINs given. Try again.".to_owned(),
                true,
            ),
            SecurityKeyError::Replug => page(
                "Unplug the key and plug it back in",
                "Three wrong PINs in a row. The key won’t take another until it’s plugged in \
                 again."
                    .to_owned(),
                false,
            ),
            SecurityKeyError::Locked => Trouble {
                other_key: action.other_key.clone(),
                for_good: true,
                ..page(
                    "This key is locked",
                    "Too many wrong PINs. Resetting it with its maker’s tool erases it, \
                     including this identity’s root."
                        .to_owned(),
                    false,
                )
            },
            SecurityKeyError::NoPin => page(
                "This key has no PIN",
                "Set one with its maker’s tool, or fido2-token -S, then try again.".to_owned(),
                true,
            ),
            SecurityKeyError::Full => page(
                "No room on this key",
                "It can’t hold another passkey. Remove one with its maker’s tool, or use another \
                 key."
                    .to_owned(),
                true,
            ),
            SecurityKeyError::NotTouched => page(
                "The key wasn’t touched",
                "Try again, and touch it while it blinks.".to_owned(),
                true,
            ),
            SecurityKeyError::NoCredential => not_ours(action),
            SecurityKeyError::Other(why) if why == CANCELLED => return None,
            SecurityKeyError::Other(why) if why == NO_KEY => page(
                "Plug in your key",
                "No security key found. Plug one in: Spoolframe notices it, and carries on."
                    .to_owned(),
                false,
            ),
            other => page(
                "The key didn’t do it",
                crate::app::sentence(&other.to_string()),
                true,
            ),
        },
        SdkError::NotARoot => not_ours(action),
        SdkError::NotFound(_) if action.not_found.is_some() => page(
            "That didn’t work",
            action.not_found.clone().unwrap_or_default(),
            true,
        ),
        other => page(
            "That didn’t work",
            crate::app::sentence(&other.to_string()),
            true,
        ),
    })
}

/// "This key isn't one of Alice's".
fn not_ours(action: &Action) -> Trouble {
    Trouble {
        title: match &action.identity {
            Some(name) => format!("This key isn’t one of {name}’s"),
            None => "This key isn’t one of yours".to_owned(),
        },
        body: match &action.keys {
            Some(keys) => format!("It doesn’t hold a root of this identity. Try {keys}."),
            None => "It doesn’t hold a root of this identity.".to_owned(),
        },
        retry: true,
        for_good: false,
        other_key: None,
    }
}

/// The plug-in page, while no key is plugged in.
const PLUG_IN: &str = "Plug in your key";
const PLUG_IN_LINE: &str =
    "Plug in a security key. With a backup, plug in the one not used yet, or both.";

/// A root action to run with a key: given the prompt, the work.
pub type Op<T> =
    Rc<dyn Fn(Arc<dyn PinPrompt>) -> Pin<Box<dyn Future<Output = Result<T, SdkError>>>>>;

/// A root action whose work runs on the SDK's runtime, as every call of
/// the SDK's client does: `work` gets `identities` and the prompt, here,
/// and returns the future that runs there.
pub fn op_on<T: Send + 'static>(
    identities: &Identities,
    work: impl Fn(
        Identities,
        Arc<dyn PinPrompt>,
    ) -> spoolframe_linux_core::backend::BoxFuture<'static, Result<T, SdkError>>
    + 'static,
) -> Op<T> {
    let identities = identities.clone();
    Rc::new(move |prompt| {
        let task = work(identities.clone(), prompt);
        let identities = identities.clone();
        Box::pin(async move { identities.run(move |_| task).await })
    })
}

/// Runs `op` behind the prompt over `parent`, offering Try again when it
/// fails in a way trying again can fix; `done` with what it made. The
/// prompt closes on success, cancel and OK.
pub fn act<T: 'static>(
    parent: &impl IsA<gtk::Widget>,
    action: Action,
    op: Op<T>,
    done: impl Fn(T) + 'static,
) {
    let view = View::new(action);
    view.dialog.present(Some(parent));
    view.start(op, Rc::new(done));
}

/// The dialog's pages.
struct View {
    dialog: adw::Dialog,
    stack: gtk::Stack,
    action: Action,
    touch_title: gtk::Label,
    /// "Step 1 of 2", for an action of several keys.
    touch_step: gtk::Label,
    touch_line: gtk::Label,
    plug_title: gtk::Label,
    plug_line: gtk::Label,
    pin_title: gtk::Label,
    pin: gtk::PasswordEntry,
    pin_error: gtk::Label,
    trouble_title: gtk::Label,
    trouble_body: gtk::Label,
    other_key: gtk::Box,
    other_key_line: gtk::Label,
    retry: gtk::Button,
    cancel_trouble: gtk::Button,
    /// Where the PIN typed goes: the key's thread waits on it.
    answer: RefCell<Option<mpsc::Sender<Option<String>>>>,
    /// Run the action again (Try again).
    again: RefCell<Option<Rc<dyn Fn()>>>,
    /// The person cancelled: a key waiting to be plugged in stops waiting.
    cancelled: Arc<AtomicBool>,
}

fn label(classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(classes)
        .build()
}

impl View {
    fn new(action: Action) -> Rc<Self> {
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vhomogeneous(false)
            .build();

        // Touch.
        let touch = column();
        touch.append(
            &gtk::Image::builder()
                .icon_name(crate::icons::SECURITY_KEY)
                .pixel_size(48)
                .css_classes(["key-touch"])
                .build(),
        );
        let touch_step = label(&["caption", "dim-label"]);
        touch_step.set_visible(false);
        touch.append(&touch_step);
        let touch_title = label(&["title-3"]);
        touch.append(&touch_title);
        let touch_line = label(&["dim-label"]);
        touch_line.set_text(action.keys.as_deref().unwrap_or_default());
        touch_line.set_visible(action.keys.is_some());
        touch.append(&touch_line);
        let cancel = gtk::Button::builder()
            .label("Cancel")
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        touch.append(&cancel);
        stack.add_named(&touch, Some("touch"));

        // Plug in: no key the action can use is plugged in yet.
        let plug = column();
        plug.append(
            &gtk::Image::builder()
                .icon_name(crate::icons::SECURITY_KEY)
                .pixel_size(48)
                .css_classes(["dim-label"])
                .build(),
        );
        let plug_title = label(&["title-3"]);
        plug.append(&plug_title);
        let plug_line = label(&["dim-label"]);
        plug.append(&plug_line);
        let plug_cancel = gtk::Button::builder()
            .label("Cancel")
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        plug.append(&plug_cancel);
        stack.add_named(&plug, Some("plug"));

        // PIN.
        let pin_page = column();
        let pin_title = label(&["title-3"]);
        pin_page.append(&pin_title);
        let pin = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .activates_default(true)
            .css_classes(["field"])
            .build();
        pin_page.append(&pin);
        let pin_error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error"])
            .build();
        pin_page.append(&pin_error);
        let pin_note = label(&["dim-label", "caption"]);
        pin_note.set_text("The key’s own PIN, set when you got it. Spoolframe doesn’t keep it.");
        pin_page.append(&pin_note);
        let pin_buttons = buttons();
        let pin_cancel = gtk::Button::builder().label("Cancel").build();
        let pin_go = gtk::Button::builder()
            .label("Continue")
            .css_classes(["suggested-action"])
            .build();
        pin_buttons.append(&pin_cancel);
        pin_buttons.append(&pin_go);
        pin_page.append(&pin_buttons);
        stack.add_named(&pin_page, Some("pin"));

        // Trouble.
        let trouble_page = column();
        let trouble_title = label(&["title-3"]);
        let trouble_body = label(&["dim-label"]);
        trouble_page.append(&trouble_title);
        trouble_page.append(&trouble_body);
        let other_key = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .visible(false)
            .css_classes(["card"])
            .build();
        other_key.append(
            &gtk::Label::builder()
                .label("Use your other key")
                .xalign(0.0)
                .css_classes(["heading"])
                .margin_top(10)
                .margin_start(12)
                .build(),
        );
        let other_key_line = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(10)
            .css_classes(["dim-label"])
            .build();
        other_key.append(&other_key_line);
        trouble_page.append(&other_key);
        let trouble_buttons = buttons();
        let cancel_trouble = gtk::Button::builder().label("Cancel").build();
        let retry = gtk::Button::builder()
            .label("Try again")
            .css_classes(["suggested-action"])
            .build();
        trouble_buttons.append(&cancel_trouble);
        trouble_buttons.append(&retry);
        trouble_page.append(&trouble_buttons);
        stack.add_named(&trouble_page, Some("trouble"));

        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let dialog = adw::Dialog::builder()
            .title("Security key")
            .content_width(420)
            .can_close(false)
            .child(&toolbar)
            .build();
        dialog.set_default_widget(Some(&pin_go));

        let view = Rc::new(View {
            dialog,
            stack,
            action,
            touch_title,
            touch_step,
            touch_line,
            plug_title,
            plug_line,
            pin_title,
            pin,
            pin_error,
            trouble_title,
            trouble_body,
            other_key,
            other_key_line,
            retry,
            cancel_trouble,
            answer: RefCell::new(None),
            again: RefCell::new(None),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        view.touch_title.set_text(&view.action.touch);

        let weak = Rc::downgrade(&view);
        let give_up = move || {
            if let Some(view) = weak.upgrade() {
                view.cancelled.store(true, Ordering::SeqCst);
                view.answer(None);
                view.close();
            }
        };
        let give_up = Rc::new(give_up);
        for button in [&cancel, &pin_cancel, &plug_cancel, &view.cancel_trouble] {
            let give_up = give_up.clone();
            button.connect_clicked(move |_| give_up());
        }
        let weak = Rc::downgrade(&view);
        pin_go.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                let typed = view.pin.text().to_string();
                view.pin.set_text("");
                view.stack.set_visible_child_name("touch");
                view.answer(Some(typed));
            }
        });
        let weak = Rc::downgrade(&view);
        view.retry.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade()
                && let Some(again) = view.again.borrow().clone()
            {
                again();
            }
        });
        crate::dialog::keep_until_closed(&view.dialog, view.clone());
        view
    }

    fn close(&self) {
        self.dialog.set_can_close(true);
        self.dialog.close();
    }

    /// Hands the key's thread its answer.
    fn answer(&self, pin: Option<String>) {
        if let Some(answer) = self.answer.borrow_mut().take() {
            let _ = answer.send(pin);
        }
    }

    /// The key wants its PIN: the PIN page, the answer sent to `answer`.
    fn ask(&self, ask: &PinAsk, answer: mpsc::Sender<Option<String>>) {
        self.pin_title
            .set_text(&format!("Enter the PIN for {}", ask.key));
        self.pin_error.set_text(&wrong_pin_line(ask.tries_left));
        self.pin_error.set_visible(ask.wrong);
        *self.answer.borrow_mut() = Some(answer);
        self.stack.set_visible_child_name("pin");
        self.pin.grab_focus();
    }

    fn start<T: 'static>(self: &Rc<Self>, op: Op<T>, done: Rc<dyn Fn(T)>) {
        let weak = Rc::downgrade(self);
        let (op_again, done_again) = (op.clone(), done.clone());
        *self.again.borrow_mut() = Some(Rc::new(move || {
            if let Some(view) = weak.upgrade() {
                view.start(op_again.clone(), done_again.clone());
            }
        }));
        self.stack.set_visible_child_name("touch");
        self.plug_title.set_text(PLUG_IN);
        self.plug_line.set_text(PLUG_IN_LINE);
        self.cancelled.store(false, Ordering::SeqCst);
        let prompt: Arc<dyn PinPrompt> = Arc::new(Prompt {
            view: Arc::new(ThreadGuard::new(Rc::downgrade(self))),
            cancelled: self.cancelled.clone(),
        });
        let work = op(prompt);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = work.await;
            let Some(view) = weak.upgrade() else { return };
            match result {
                Ok(made) => {
                    view.close();
                    done(made);
                }
                Err(e) => match trouble(&e, &view.action) {
                    Some(trouble) => view.show_trouble(&trouble),
                    None => view.close(),
                },
            }
        });
    }

    fn show_trouble(&self, trouble: &Trouble) {
        self.trouble_title.set_text(&trouble.title);
        self.trouble_body.set_text(&trouble.body);
        self.retry.set_visible(trouble.retry);
        self.cancel_trouble
            .set_label(if trouble.for_good { "OK" } else { "Cancel" });
        match &trouble.other_key {
            Some(key) => {
                self.other_key_line.set_text(&format!(
                    "{key} can still act for you, and can add a new key in place of this one."
                ));
                self.other_key.set_visible(true);
            }
            None => self.other_key.set_visible(false),
        }
        self.stack.set_visible_child_name("trouble");
    }
}

/// What the key holds: asks the dialog on the main loop, and waits.
struct Prompt {
    view: Arc<ThreadGuard<Weak<View>>>,
    cancelled: Arc<AtomicBool>,
}

impl PinPrompt for Prompt {
    fn pin(&self, ask: &PinAsk) -> Option<String> {
        let (tx, rx) = mpsc::channel();
        let (view, ask) = (self.view.clone(), ask.clone());
        glib::MainContext::default().invoke(move || match view.get_ref().upgrade() {
            Some(view) => view.ask(&ask, tx),
            None => {
                let _ = tx.send(None);
            }
        });
        rx.recv().ok().flatten()
    }

    fn touch(&self, _key: &str) {
        let view = self.view.clone();
        glib::MainContext::default().invoke(move || {
            if let Some(view) = view.get_ref().upgrade() {
                view.stack.set_visible_child_name("touch");
            }
        });
    }

    fn step(&self, step: &Step) {
        let (view, step) = (self.view.clone(), step.clone());
        glib::MainContext::default().invoke(move || {
            if let Some(view) = view.get_ref().upgrade() {
                view.touch_step.set_text(&step.caption);
                view.touch_step.set_visible(true);
                view.touch_title.set_text(&step.title);
                view.touch_line.set_text(&step.line);
                view.touch_line.set_visible(!step.line.is_empty());
            }
        });
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn not_ours(&self) {
        let view = self.view.clone();
        glib::MainContext::default().invoke(move || {
            if let Some(view) = view.get_ref().upgrade() {
                let page = not_ours(&view.action);
                view.plug_title.set_text(&page.title);
                view.plug_line.set_text(&page.body);
            }
        });
    }

    fn plug_in(&self) -> bool {
        let view = self.view.clone();
        glib::MainContext::default().invoke(move || {
            if let Some(view) = view.get_ref().upgrade() {
                view.stack.set_visible_child_name("plug");
            }
        });
        !self.cancelled.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> Action {
        Action {
            touch: "Touch your key to pair Living-room TV".into(),
            keys: Some(or_list(&["Keyring key".into(), "Drawer key".into()])),
            identity: Some("Alice".into()),
            other_key: Some("Drawer key".into()),
            not_found: None,
        }
    }

    /// The prompt keeps its view while it shows: an action that succeeds
    /// reaches `done`, though `act` has long returned.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn an_action_that_succeeds_reaches_done() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let window = adw::Window::new();
        window.present();
        let reached = Rc::new(std::cell::Cell::new(false));
        let op: Op<()> = Rc::new(|_prompt| Box::pin(async { Ok(()) }));
        let seen = reached.clone();
        act(&window, alice(), op, move |()| seen.set(true));
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(2),
            || reached.get()
        ));
    }

    #[test]
    fn an_action_says_what_it_did_not_find_in_its_own_words() {
        let action = Action {
            not_found: Some("Home Media doesn’t know an identity this key holds.".into()),
            ..alice()
        };
        let trouble = trouble(&SdkError::NotFound("portable identity".into()), &action).unwrap();
        assert_eq!(
            trouble.body,
            "Home Media doesn’t know an identity this key holds."
        );
    }

    #[test]
    fn a_locked_key_ends_the_action_for_good() {
        assert!(
            trouble(&SecurityKeyError::Locked.into(), &alice())
                .unwrap()
                .for_good
        );
    }

    #[test]
    fn keys_are_listed_with_or() {
        assert_eq!(or_list(&["A".into(), "B".into(), "C".into()]), "A, B or C");
    }

    #[test]
    fn a_wrong_pin_says_how_many_tries_are_left() {
        assert_eq!(
            wrong_pin_line(Some(5)),
            "Wrong PIN. 5 tries left before the key locks."
        );
    }

    #[test]
    fn a_locked_key_has_only_ok_and_points_to_the_other_key() {
        let trouble = trouble(&SecurityKeyError::Locked.into(), &alice()).unwrap();
        assert_eq!(
            (
                trouble.title.as_str(),
                trouble.retry,
                trouble.other_key.as_deref()
            ),
            ("This key is locked", false, Some("Drawer key"))
        );
    }

    #[test]
    fn a_key_that_is_not_a_root_names_the_identity_and_its_keys() {
        let trouble = trouble(&SdkError::NotARoot, &alice()).unwrap();
        assert_eq!(
            (trouble.title.as_str(), trouble.body.as_str()),
            (
                "This key isn’t one of Alice’s",
                "It doesn’t hold a root of this identity. Try Keyring key or Drawer key."
            )
        );
    }

    #[test]
    fn no_key_says_to_plug_one_in_with_cancel_only() {
        let error = SecurityKeyError::Other(NO_KEY.into());
        let trouble = trouble(&error.into(), &alice()).unwrap();
        assert_eq!(
            (trouble.title.as_str(), trouble.retry),
            ("Plug in your key", false)
        );
    }

    #[test]
    fn three_wrong_pins_in_a_row_offer_cancel_only() {
        assert!(
            !trouble(&SecurityKeyError::Replug.into(), &alice())
                .unwrap()
                .retry
        );
    }

    #[test]
    fn cancelling_closes_without_a_word() {
        assert_eq!(
            trouble(&SecurityKeyError::Other(CANCELLED.into()).into(), &alice()),
            None
        );
    }
}
