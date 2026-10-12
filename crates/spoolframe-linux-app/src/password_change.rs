//! "Choose a new password" (c86e6d3a), as on the LinuxPasswordChange
//! board: an admin reset the user's password to a one-time one, and the
//! session may do nothing but change it or sign out. Right after signing
//! in with the one-time password it is reused; otherwise it is asked for.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{BackendError, Session};

/// The least a new password may be, as the server checks.
pub const MIN_LEN: usize = 8;

/// Which field a message goes under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    OneTime,
    New,
    Again,
}

/// What is wrong with the fields before asking the server, if anything.
pub fn check(current: &str, new: &str, again: &str) -> Result<(), (Field, &'static str)> {
    if new.chars().count() < MIN_LEN {
        return Err((Field::New, "At least 8 characters."));
    }
    if new != again {
        return Err((Field::Again, "The two passwords don’t match."));
    }
    if new == current {
        return Err((Field::New, "That’s the one-time password: choose your own."));
    }
    Ok(())
}

/// What the server's refusal says, and under which field; `None` when the
/// one-time password has lapsed (the session ended), which has its own
/// card.
pub fn refusal(error: &BackendError) -> Option<(Field, String)> {
    match error {
        BackendError::WrongCredentials => Some((
            Field::OneTime,
            "That isn’t the one-time password.".to_owned(),
        )),
        BackendError::SignInRequired => None,
        BackendError::InvalidInput(why) => Some((Field::New, crate::app::sentence(why))),
        other => Some((Field::New, crate::app::sentence(&other.to_string()))),
    }
}

/// What the dialog needs.
pub struct Needs {
    pub session: Arc<dyn Session>,
    /// Who and where, for "An admin reset the password for Alice on Home
    /// Media."
    pub name: String,
    pub server: String,
    /// The one-time password just typed to sign in; `None` asks for it.
    pub one_time: Option<String>,
    pub done: Rc<dyn Fn()>,
    pub sign_out: Rc<dyn Fn()>,
}

/// A field with its label above and its hint or error below.
struct Entry {
    entry: gtk::PasswordEntry,
    below: gtk::Label,
    hint: &'static str,
    group: gtk::Box,
}

impl Entry {
    fn new(label: &str, hint: &'static str) -> Self {
        let group = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        group.append(
            &gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .css_classes(["field-label"])
                .build(),
        );
        let entry = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .activates_default(true)
            .css_classes(["field"])
            .build();
        group.append(&entry);
        let below = gtk::Label::builder()
            .label(hint)
            .xalign(0.0)
            .wrap(true)
            .visible(!hint.is_empty())
            .css_classes(["dim-label", "caption"])
            .build();
        group.append(&below);
        Self {
            entry,
            below,
            hint,
            group,
        }
    }

    fn calm(&self) {
        self.below.set_text(self.hint);
        self.below.set_visible(!self.hint.is_empty());
        self.below.remove_css_class("error");
        self.below.add_css_class("dim-label");
    }

    fn fault(&self, text: &str) {
        self.below.set_text(text);
        self.below.set_visible(true);
        self.below.remove_css_class("dim-label");
        self.below.add_css_class("error");
    }

    fn text(&self) -> String {
        self.entry.text().to_string()
    }
}

/// The dialog, over `parent`.
pub fn open(parent: &impl IsA<gtk::Widget>, needs: Needs) {
    let dialog = adw::Dialog::builder()
        .title("Choose a new password")
        .content_width(420)
        .can_close(false)
        .build();
    let stack = gtk::Stack::builder().vhomogeneous(false).build();

    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    form.append(
        &gtk::Label::builder()
            .label("Choose a new password")
            .xalign(0.0)
            .css_classes(["title-3"])
            .build(),
    );
    let intro = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .use_markup(true)
        .css_classes(["dim-label"])
        .build();
    intro.set_markup(
        crate::markup::markup!(
            "An admin reset the password for <b>{}</b> on {}. Choose a new one to go on.",
            needs.name,
            needs.server
        )
        .escaped(),
    );
    form.append(&intro);
    let one_time = Entry::new("One-time password", "The one your admin gave you.");
    one_time.group.set_visible(needs.one_time.is_none());
    let new = Entry::new("New password", "At least 8 characters.");
    let again = Entry::new("New password again", "");
    form.append(&one_time.group);
    form.append(&new.group);
    form.append(&again.group);
    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let sign_out = gtk::Button::builder().label("Sign out").build();
    let go = gtk::Button::builder()
        .label("Continue")
        .css_classes(["suggested-action"])
        .build();
    buttons.append(&sign_out);
    buttons.append(&go);
    form.append(&buttons);
    stack.add_named(&form, Some("form"));

    // Lapsed: the one-time password no longer works.
    let lapsed = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["card", "erase-all"])
        .build();
    card.append(
        &gtk::Label::builder()
            .label("The one-time password no longer works. Sign out and sign in again.")
            .wrap(true)
            .xalign(0.0)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build(),
    );
    lapsed.append(&card);
    let lapsed_sign_out = gtk::Button::builder()
        .label("Sign out")
        .halign(gtk::Align::End)
        .css_classes(["suggested-action"])
        .build();
    lapsed.append(&lapsed_sign_out);
    stack.add_named(&lapsed, Some("lapsed"));

    let header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    dialog.set_child(Some(&toolbar));
    dialog.set_default_widget(Some(&go));

    let fields = Rc::new((one_time, new, again));
    let needs = Rc::new(needs);
    let leave = {
        let (dialog, needs) = (dialog.downgrade(), needs.clone());
        Rc::new(move || {
            if let Some(dialog) = dialog.upgrade() {
                dialog.set_can_close(true);
                dialog.close();
            }
            (needs.sign_out)();
        })
    };
    {
        let leave = leave.clone();
        sign_out.connect_clicked(move |_| leave());
    }
    lapsed_sign_out.connect_clicked(move |_| leave());
    let (dialog_weak, stack_c) = (dialog.downgrade(), stack.clone());
    go.connect_clicked(move |go| {
        let (one_time, new, again) = &*fields;
        one_time.calm();
        new.calm();
        again.calm();
        let current = needs.one_time.clone().unwrap_or_else(|| one_time.text());
        let (new_text, again_text) = (new.text(), again.text());
        if let Err((field, why)) = check(&current, &new_text, &again_text) {
            match field {
                Field::OneTime => one_time.fault(why),
                Field::New => new.fault(why),
                Field::Again => again.fault(why),
            }
            return;
        }
        go.set_sensitive(false);
        let session = needs.session.clone();
        let changing =
            relm4::spawn(async move { session.change_password(&current, &new_text).await });
        let (fields, needs, dialog, stack, go) = (
            fields.clone(),
            needs.clone(),
            dialog_weak.clone(),
            stack_c.clone(),
            go.downgrade(),
        );
        glib::spawn_future_local(async move {
            let Ok(changed) = changing.await else { return };
            if let Some(go) = go.upgrade() {
                go.set_sensitive(true);
            }
            let (one_time, new, _) = &*fields;
            match changed {
                Ok(()) => {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.set_can_close(true);
                        dialog.close();
                    }
                    (needs.done)();
                }
                Err(e) => match refusal(&e) {
                    None => stack.set_visible_child_name("lapsed"),
                    Some((Field::OneTime, why)) => {
                        // The one reused from the sign-in was wrong: ask.
                        one_time.group.set_visible(true);
                        one_time.fault(&why);
                    }
                    Some((_, why)) => new.fault(&why),
                },
            }
        });
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_password_is_caught_before_asking() {
        assert_eq!(
            check("one-time", "short", "short"),
            Err((Field::New, "At least 8 characters."))
        );
    }

    #[test]
    fn two_different_passwords_are_caught_under_again() {
        assert_eq!(
            check("one-time", "long enough 1", "long enough 2"),
            Err((Field::Again, "The two passwords don’t match."))
        );
    }

    #[test]
    fn the_one_time_password_again_is_refused() {
        assert_eq!(
            check("one-time-7KQ2", "one-time-7KQ2", "one-time-7KQ2"),
            Err((Field::New, "That’s the one-time password: choose your own."))
        );
    }

    #[test]
    fn a_wrong_one_time_password_goes_under_its_field() {
        assert_eq!(
            refusal(&BackendError::WrongCredentials),
            Some((
                Field::OneTime,
                "That isn’t the one-time password.".to_owned()
            ))
        );
    }

    #[test]
    fn a_lapsed_one_time_password_has_its_own_card() {
        assert_eq!(refusal(&BackendError::SignInRequired), None);
    }
}
