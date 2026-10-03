//! The profile PIN (P2-08), asked for on "Who's watching" before anything
//! of a locked profile shows, as on the LinuxPin board: the avatar and
//! name, "Enter your PIN", a dot per digit, a number pad, and a way back
//! to the password.
//!
//! A PIN is 4 to 8 digits and the pad cannot know which, so it does not
//! try on its own: every wrong try counts towards the ten that end the
//! session. Enter, or the Unlock button once there are four digits,
//! sends it.

use std::cell::RefCell;
use std::rc::Rc;

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
}

impl Pad {
    /// A wrong PIN: the digits go, and the pad says so.
    pub fn wrong(&self) {
        self.reject("Wrong PIN. Try again.");
    }

    /// The server refused for another reason (too many tries, a
    /// connection lost), in its words.
    pub fn reject(&self, why: &str) {
        self.digits.borrow_mut().clear();
        self.message.set_label(why);
        self.message.add_css_class("error");
        self.busy(false);
        self.redraw();
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

    let avatar = adw::Avatar::new(88, Some(&profile.display_name), true);
    let name = gtk::Label::builder()
        .label(&profile.display_name)
        .css_classes(["title-2"])
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
        .margin_bottom(4)
        .build();
    let message = gtk::Label::builder()
        .css_classes(["caption"])
        .height_request(20)
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
        let (digits, dots, message, unlock_button) = (
            digits.clone(),
            dots.clone(),
            message.clone(),
            unlock_button.clone(),
        );
        Rc::new(move |c| {
            if digits.borrow_mut().push(c) {
                message.set_label("");
                message.remove_css_class("error");
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
        .css_classes(["flat", "circular", "pin-key"])
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
    let forgot_link = gtk::Button::builder()
        .label("Forgot your PIN? Sign in with your password")
        .halign(gtk::Align::Center)
        .margin_top(10)
        .css_classes(["link", "caption"])
        .build();
    forgot_link.connect_clicked(move |_| forgot());

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
        forgot_link.upcast_ref(),
    ] {
        column.append(widget);
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&column));
    // The keyboard: digits, Backspace, Enter, Esc.
    let typing = gtk::EventControllerKey::new();
    typing.connect_key_pressed(move |_, key, _, _| {
        match key {
            gtk::gdk::Key::BackSpace => delete(),
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => send(),
            gtk::gdk::Key::Escape => back(),
            _ => match key.to_unicode() {
                Some(c) if c.is_ascii_digit() => press(c),
                _ => return glib::Propagation::Proceed,
            },
        }
        glib::Propagation::Stop
    });
    toolbar.add_controller(typing);
    toolbar.set_focusable(true);
    toolbar.connect_map(|toolbar| {
        toolbar.grab_focus();
    });

    let pad = Rc::new(Pad {
        widget: toolbar.upcast(),
        digits,
        dots,
        message,
        unlock: unlock_button,
        keys,
    });
    pad.redraw();
    pad
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
