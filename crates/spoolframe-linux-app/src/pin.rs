//! The profile PIN (P2-08), asked for on "Who's watching" before anything
//! of a locked profile shows, as on the LinuxPin board: the avatar and
//! name, "Enter your PIN", a dot per digit, a number pad, and a way back
//! to the password.
//!
//! A PIN is 4 to 8 digits and the pad cannot know which, so it does not
//! try on its own: every wrong try counts towards the ten that end the
//! session. Enter, or the Unlock button once there are four digits,
//! sends it.
//!
//! The same pad, without the header and the password, stands in for a
//! locked profile's downloads (PX-60, [`inline`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::Profile;

/// The fewest and most digits a PIN has.
pub const MIN: usize = 4;
pub const MAX: usize = 8;

/// The digits typed so far.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Digits(String);

impl Digits {
    /// Adds a digit; anything else, or a ninth digit, is ignored.
    pub fn push(&mut self, c: char) -> bool {
        if c.is_ascii_digit() && self.0.len() < MAX {
            self.0.push(c);
            true
        } else {
            false
        }
    }

    pub fn pop(&mut self) {
        self.0.pop();
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Long enough to send.
    pub fn ready(&self) -> bool {
        self.0.len() >= MIN
    }

    /// How many dots to draw: one per digit, and at least the shortest
    /// PIN's four.
    pub fn slots(&self) -> usize {
        self.0.len().max(MIN)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The pad's live parts, to show what the server said.
pub struct Pad {
    pub widget: gtk::Widget,
    digits: Rc<RefCell<Digits>>,
    dots: gtk::Box,
    message: gtk::Label,
    unlock: gtk::Button,
    keys: gtk::Grid,
    /// Types a digit, and sends what was typed, as the keys do: for tests.
    #[cfg(test)]
    press: Rc<dyn Fn(char)>,
    #[cfg(test)]
    send: Rc<dyn Fn()>,
    /// Counts a wait down. It holds the pad weakly: a pad dropped stops
    /// it at the next second.
    countdown: RefCell<Option<glib::SourceId>>,
}

impl Pad {
    /// A wrong PIN: the digits go, and the pad says so.
    pub fn wrong(&self) {
        self.reject("Wrong PIN. Try again.");
    }

    /// The server refused for another reason (too many tries, a
    /// connection lost), in its words.
    pub fn reject(&self, why: &str) {
        self.stop_countdown();
        self.digits.borrow_mut().clear();
        self.say(why);
        self.busy(false);
        self.redraw();
    }

    /// Too many tries: the keys rest for `wait`, saying how long is left
    /// each second, and wake when it is over (PX-60).
    pub fn wait(self: &Rc<Self>, wait: Duration) {
        self.stop_countdown();
        self.digits.borrow_mut().clear();
        self.redraw();
        let left = Rc::new(std::cell::Cell::new(wait.as_secs().max(1)));
        self.say(&wait_line(left.get()));
        self.busy(true);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local(1, move || {
            let Some(pad) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            left.set(left.get().saturating_sub(1));
            if left.get() == 0 {
                pad.countdown.take();
                pad.message.set_visible(false);
                pad.busy(false);
                return glib::ControlFlow::Break;
            }
            pad.say(&wait_line(left.get()));
            glib::ControlFlow::Continue
        });
        self.countdown.replace(Some(id));
    }

    fn stop_countdown(&self) {
        if let Some(id) = self.countdown.take() {
            id.remove();
        }
    }

    fn say(&self, what: &str) {
        self.message.set_label(what);
        self.message.add_css_class("error");
        self.message.set_visible(true);
    }

    /// Waiting for the server: the keys rest.
    fn busy(&self, busy: bool) {
        self.keys.set_sensitive(!busy);
        self.unlock
            .set_sensitive(!busy && self.digits.borrow().ready());
    }

    fn redraw(&self) {
        redraw(&self.digits.borrow(), &self.dots, &self.unlock);
    }

    /// Types `pin` and sends it, as the keys would.
    #[cfg(test)]
    pub fn enter(&self, pin: &str) {
        for c in pin.chars() {
            (self.press)(c);
        }
        (self.send)();
    }

    /// Whether the keys take digits now, and what the pad says.
    #[cfg(test)]
    pub fn shown(&self) -> (bool, Option<String>) {
        (
            self.keys.is_sensitive(),
            self.message
                .is_visible()
                .then(|| self.message.label().to_string()),
        )
    }
}

/// "Try again in 4 s".
pub fn wait_line(secs: u64) -> String {
    format!("Try again in {secs} s")
}

fn redraw(digits: &Digits, dots: &gtk::Box, unlock: &gtk::Button) {
    while let Some(dot) = dots.first_child() {
        dots.remove(&dot);
    }
    for i in 0..digits.slots() {
        let dot = gtk::Box::builder()
            .width_request(14)
            .height_request(14)
            .valign(gtk::Align::Center)
            .css_classes(["pin-dot"])
            .build();
        if i < digits.len() {
            dot.add_css_class("filled");
        }
        dots.append(&dot);
    }
    dots.update_property(&[gtk::accessible::Property::Label(&format!(
        "{} digits entered",
        digits.len()
    ))]);
    unlock.set_visible(digits.ready());
    unlock.set_sensitive(digits.ready());
}

/// The pad for `profile` on server `server_name`. `unlock` gets the PIN;
/// `forgot` signs in with the password instead; `back` returns to the
/// profiles.
pub fn pad(
    profile: &Profile,
    server_name: &str,
    unlock: Rc<dyn Fn(String)>,
    forgot: Rc<dyn Fn()>,
    back: Rc<dyn Fn()>,
) -> Rc<Pad> {
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(server_name, ""))
        .build();
    let back_button = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::BACK)
                .label("Profiles")
                .build(),
        )
        .tooltip_text("Back to Who's watching (Esc)")
        .css_classes(["flat"])
        .build();
    {
        let back = back.clone();
        back_button.connect_clicked(move |_| back());
    }
    header.pack_start(&back_button);
    let pad = keypad(&profile.display_name, unlock, Some(forgot), Some(back));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&pad.widget));
    let pad = Rc::new(Pad {
        widget: toolbar.upcast(),
        ..pad
    });
    pad.redraw();
    pad
}

/// The pad alone, to sit in a page (PX-60's locked downloads): the
/// avatar and `name`, the dots and the keys. `unlock` gets the PIN.
pub fn inline(name: &str, unlock: Rc<dyn Fn(String)>) -> Rc<Pad> {
    let pad = Rc::new(keypad(name, unlock, None, None));
    pad.redraw();
    pad
}

/// The avatar, the dots, the keys and the keyboard, with "Forgot your
/// PIN?" when there is a `forgot`, and Esc for `back`.
fn keypad(
    name: &str,
    unlock: Rc<dyn Fn(String)>,
    forgot: Option<Rc<dyn Fn()>>,
    back: Option<Rc<dyn Fn()>>,
) -> Pad {
    let avatar = adw::Avatar::new(88, Some(name), true);
    avatar.add_css_class("profile-avatar");
    avatar.add_css_class("tint-purple");
    let name = gtk::Label::builder()
        .label(name)
        .css_classes(["pin-name"])
        .margin_top(4)
        .build();
    let prompt = gtk::Box::builder()
        .spacing(6)
        .halign(gtk::Align::Center)
        .css_classes(["dim-label"])
        .build();
    prompt.append(&gtk::Image::from_icon_name(crate::icons::LOCK));
    prompt.append(&gtk::Label::new(Some("Enter your PIN")));
    let dots = gtk::Box::builder()
        .spacing(18)
        .halign(gtk::Align::Center)
        .margin_top(10)
        .build();
    // Empty until a try fails, so the pad sits 28 px under the dots.
    let message = gtk::Label::builder()
        .css_classes(["caption"])
        .visible(false)
        .build();

    let keys = gtk::Grid::builder()
        .row_spacing(12)
        .column_spacing(12)
        .halign(gtk::Align::Center)
        .build();
    let digits: Rc<RefCell<Digits>> = Rc::default();
    let unlock_button = gtk::Button::builder()
        .label("Unlock")
        .halign(gtk::Align::Center)
        .visible(false)
        .css_classes(["pill", "suggested-action"])
        .build();

    // Every way of typing goes through these.
    let press: Rc<dyn Fn(char)> = {
        let (digits, dots, message, unlock_button, keys) = (
            digits.clone(),
            dots.clone(),
            message.clone(),
            unlock_button.clone(),
            keys.clone(),
        );
        Rc::new(move |c| {
            // Resting keys take nothing: a try is with the server, or a
            // wait runs.
            if keys.is_sensitive() && digits.borrow_mut().push(c) {
                message.set_label("");
                message.remove_css_class("error");
                message.set_visible(false);
                redraw(&digits.borrow(), &dots, &unlock_button);
            }
        })
    };
    let delete: Rc<dyn Fn()> = {
        let (digits, dots, unlock_button) = (digits.clone(), dots.clone(), unlock_button.clone());
        Rc::new(move || {
            digits.borrow_mut().pop();
            redraw(&digits.borrow(), &dots, &unlock_button);
        })
    };
    let send: Rc<dyn Fn()> = {
        let (digits, keys, unlock_button) = (digits.clone(), keys.clone(), unlock_button.clone());
        Rc::new(move || {
            let pin = digits.borrow().clone();
            // Resting keys mean a try is with the server: Enter (held,
            // it repeats) sends no second.
            if pin.ready() && keys.is_sensitive() {
                // Resting until the server answers; see `Pad::reject`.
                keys.set_sensitive(false);
                unlock_button.set_sensitive(false);
                unlock(pin.as_str().to_owned());
            }
        })
    };

    for (i, c) in "123456789".chars().enumerate() {
        let key = digit_key(c, &press);
        keys.attach(&key, (i % 3) as i32, (i / 3) as i32, 1, 1);
    }
    keys.attach(&digit_key('0', &press), 1, 3, 1, 1);
    let delete_key = gtk::Button::builder()
        .icon_name(crate::icons::DELETE)
        .tooltip_text("Delete")
        .width_request(64)
        .height_request(64)
        .css_classes(["flat", "circular", "pin-key", "pin-delete"])
        .build();
    {
        let delete = delete.clone();
        delete_key.connect_clicked(move |_| delete());
    }
    keys.attach(&delete_key, 2, 3, 1, 1);
    {
        let send = send.clone();
        unlock_button.connect_clicked(move |_| send());
    }

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build();
    for widget in [
        avatar.upcast_ref::<gtk::Widget>(),
        name.upcast_ref(),
        prompt.upcast_ref(),
        dots.upcast_ref(),
        message.upcast_ref(),
        keys.upcast_ref(),
        unlock_button.upcast_ref(),
    ] {
        column.append(widget);
    }
    if let Some(forgot) = forgot {
        let forgot_link = gtk::Button::builder()
            .label("Forgot your PIN? Sign in with your password")
            .halign(gtk::Align::Center)
            .margin_top(10)
            .css_classes(["flat", "text-link"])
            .build();
        forgot_link.connect_clicked(move |_| forgot());
        column.append(&forgot_link);
    }

    // The keyboard: digits, Backspace, Enter, Esc.
    let typing = gtk::EventControllerKey::new();
    {
        let (press, send) = (press.clone(), send.clone());
        typing.connect_key_pressed(move |_, key, _, _| {
            match key {
                gtk::gdk::Key::BackSpace => delete(),
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => send(),
                gtk::gdk::Key::Escape => match &back {
                    Some(back) => back(),
                    None => return glib::Propagation::Proceed,
                },
                _ => match key.to_unicode() {
                    Some(c) if c.is_ascii_digit() => press(c),
                    _ => return glib::Propagation::Proceed,
                },
            }
            glib::Propagation::Stop
        });
    }
    column.add_controller(typing);
    column.set_focusable(true);
    column.connect_map(|column| {
        column.grab_focus();
    });

    Pad {
        widget: column.upcast(),
        digits,
        dots,
        message,
        unlock: unlock_button,
        keys,
        #[cfg(test)]
        press,
        #[cfg(test)]
        send,
        countdown: RefCell::default(),
    }
}

fn digit_key(c: char, press: &Rc<dyn Fn(char)>) -> gtk::Button {
    let key = gtk::Button::builder()
        .label(c.to_string())
        .width_request(64)
        .height_request(64)
        .focusable(false)
        .css_classes(["circular", "pin-key"])
        .build();
    let press = press.clone();
    key.connect_clicked(move |_| press(c));
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Digits {
        let mut digits = Digits::default();
        for c in text.chars() {
            digits.push(c);
        }
        digits
    }

    #[test]
    fn only_digits_are_taken() {
        assert_eq!(typed("1a2 3").as_str(), "123");
    }

    #[test]
    fn a_ninth_digit_is_ignored() {
        assert_eq!(typed("123456789").as_str(), "12345678");
    }

    #[test]
    fn four_digits_are_ready_three_are_not() {
        assert_eq!((typed("123").ready(), typed("1234").ready()), (false, true));
    }

    #[test]
    fn there_are_always_at_least_four_dots() {
        assert_eq!((typed("").slots(), typed("123456").slots()), (4, 6));
    }

    #[test]
    fn delete_takes_the_last_digit() {
        let mut digits = typed("1234");
        digits.pop();
        assert_eq!(digits.as_str(), "123");
    }
}
