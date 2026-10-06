//! "Confirm it's you" (P3-18), as the LinuxPasskeyManage board has it:
//! the actions that need a sign-in in the last 10 minutes (approving a
//! device, adding or removing a passkey, no longer requiring the PIN) are
//! refused with RecentSignInRequired after that. The password, or one of
//! the user's app passkeys on a key or a phone, proves it is them; the
//! session's sign-in time is refreshed, and the action is made again.
//! The PIN does not prove it.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{BackendError, Session};
use spoolframe_linux_core::passkey::Via;

use crate::passkey_dialog::{self, Ceremony, Done, KeyPage, KeyState};

/// What to say when the server refuses a proof.
pub fn refusal(error: &BackendError) -> String {
    match error {
        BackendError::WrongCredentials => "That isn’t right.".to_owned(),
        BackendError::RateLimited(wait) => format!(
            "Too many tries. Try again in {} min.",
            wait.as_secs().div_ceil(60).max(1)
        ),
        BackendError::ProfileLocked => "Your profile is locked. Enter your PIN first.".to_owned(),
        BackendError::NotFound(_) => {
            "You have no passkey made in the app. Use your password.".to_owned()
        }
        other => other.to_string(),
    }
}

/// Asks the user to confirm it is them on `session`, over `parent`, then
/// calls `again`. `title` is the action's, as the dialog's header.
pub fn ask(
    parent: &impl IsA<gtk::Widget>,
    session: Arc<dyn Session>,
    server: &str,
    title: &str,
    again: Rc<dyn Fn()>,
) {
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(380)
        .build();
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .vhomogeneous(false)
        .build();
    let error = gtk::Label::builder()
        .visible(false)
        .wrap(true)
        .max_width_chars(1)
        .css_classes(["error"])
        .build();
    let password = gtk::PasswordEntry::builder()
        .show_peek_icon(true)
        .css_classes(["field"])
        .build();
    let go = gtk::Button::builder()
        .label("Continue")
        .css_classes(["action", "suggested-action"])
        .build();
    let with_passkey = gtk::Button::builder()
        .label("Use a passkey instead")
        .halign(gtk::Align::Center)
        .css_classes(["flat", "text-link"])
        .build();
    let body = column();
    body.append(
        &gtk::Label::builder()
            .label("Confirm it’s you")
            .css_classes(["title-3"])
            .build(),
    );
    body.append(
        &gtk::Label::builder()
            .label("You signed in over 10 minutes ago.")
            .css_classes(["dim-label", "small-text"])
            .build(),
    );
    let field = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    field.append(
        &gtk::Label::builder()
            .label("Password")
            .xalign(0.0)
            .css_classes(["field-label"])
            .build(),
    );
    field.append(&password);
    body.append(&field);
    body.append(&error);
    body.append(&go);
    body.append(&with_passkey);
    stack.add_named(&body, Some("password"));

    // Which passkey, then the key or phone, as signing in has it.
    let choose = column();
    let key_option = passkey_dialog::option(
        crate::icons::SECURITY_KEY,
        "Security key",
        "A key such as a YubiKey, by USB",
    );
    let phone_option = passkey_dialog::option(
        crate::icons::PHONE,
        "Phone or tablet",
        "A passkey saved on your phone: scan a code, Bluetooth on",
    );
    choose.append(&key_option);
    choose.append(&phone_option);
    let back_to_password = gtk::Button::builder()
        .label("Use your password instead")
        .halign(gtk::Align::Center)
        .css_classes(["flat", "text-link"])
        .build();
    choose.append(&back_to_password);
    stack.add_named(&choose, Some("choose"));
    let to_password: Rc<dyn Fn()> = {
        let stack = stack.clone();
        Rc::new(move || stack.set_visible_child_name("password"))
    };
    let key = KeyPage::new(server, Some(to_password.clone()));
    stack.add_named(&key.widget, Some("key"));
    crate::start::with_cancel(&dialog, &stack);

    let confirmed: Rc<dyn Fn()> = {
        let (dialog, again) = (dialog.downgrade(), again.clone());
        Rc::new(move || {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
            again();
        })
    };
    {
        let to_password = to_password.clone();
        back_to_password.connect_clicked(move |_| to_password());
    }
    {
        let stack = stack.clone();
        with_passkey.connect_clicked(move |_| stack.set_visible_child_name("choose"));
    }
    // The password.
    {
        let (session, password_, error, confirmed, stack) = (
            session.clone(),
            password.clone(),
            error.clone(),
            confirmed.clone(),
            stack.clone(),
        );
        go.connect_clicked(move |go| {
            go.set_sensitive(false);
            let (session, secret) = (session.clone(), password_.text().to_string());
            let call = relm4::spawn(async move { session.confirm_password(&secret).await });
            let (go, error, confirmed, stack) =
                (go.clone(), error.clone(), confirmed.clone(), stack.clone());
            glib::spawn_future_local(async move {
                let answer = call.await;
                go.set_sensitive(true);
                match answer {
                    Ok(Ok(())) => confirmed(),
                    // No password to give: a passkey it is.
                    Ok(Err(BackendError::NoPassword)) => stack.set_visible_child_name("choose"),
                    Ok(Err(e)) => {
                        error.set_label(&refusal(&e));
                        error.set_visible(true);
                    }
                    Err(e) => {
                        error.set_label(&e.to_string());
                        error.set_visible(true);
                    }
                }
            });
        });
        let go_ = go.clone();
        password.connect_activate(move |_| go_.emit_clicked());
    }
    // A passkey: ask the server what to sign, have the key or phone sign
    // it, and hand it back.
    let with: Rc<dyn Fn(Via)> = {
        let (session, key, stack, confirmed) = (
            session.clone(),
            key.clone(),
            stack.clone(),
            confirmed.clone(),
        );
        Rc::new(move |via| {
            stack.set_visible_child_name("key");
            key.show(KeyState::Working("Asking the server…"));
            let asking = session.clone();
            let begun = relm4::spawn(async move { asking.begin_confirm_passkey().await });
            let (session, key, confirmed) = (session.clone(), key.clone(), confirmed.clone());
            glib::spawn_future_local(async move {
                let request = match begun.await {
                    Ok(Ok(request)) => request,
                    Ok(Err(e)) => return key.show(KeyState::Refused(refusal(&e))),
                    Err(e) => return key.show(KeyState::Refused(e.to_string())),
                };
                let done: Rc<dyn Fn(Done)> = {
                    let (session, key, confirmed) =
                        (session.clone(), key.clone(), confirmed.clone());
                    Rc::new(move |done| {
                        let Done::Signed(signed) = done else { return };
                        let session = session.clone();
                        let finished =
                            relm4::spawn(
                                async move { session.finish_confirm_passkey(signed).await },
                            );
                        let (key, confirmed) = (key.clone(), confirmed.clone());
                        glib::spawn_future_local(async move {
                            match finished.await {
                                Ok(Ok(())) => confirmed(),
                                Ok(Err(e)) => key.show(KeyState::Refused(refusal(&e))),
                                Err(e) => key.show(KeyState::Refused(e.to_string())),
                            }
                        });
                    })
                };
                key.run(Ceremony::SignIn(request), via, done);
            });
        })
    };
    {
        let with = with.clone();
        key_option.connect_clicked(move |_| with(Via::Key));
    }
    {
        let with = with.clone();
        phone_option.connect_clicked(move |_| with(Via::Phone));
    }
    {
        let (with, key_) = (with.clone(), Rc::downgrade(&key));
        key.set_retry(Rc::new(move || {
            if let Some(key) = key_.upgrade() {
                with(key.via());
            }
        }));
    }
    {
        let with = with.clone();
        key.set_alternative(Rc::new(move || with(Via::Key)));
    }
    {
        // The dialog holds its pieces while it shows; a key left waiting
        // gives up when it closes.
        let held = (key.clone(), with.clone());
        dialog.connect_closed(move |_| {
            held.0.stop();
            let _ = &held.1;
        });
    }
    dialog.present(Some(parent));
}

fn column() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(24)
        .margin_bottom(26)
        .margin_start(26)
        .margin_end(26)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_app_passkey_points_back_to_the_password() {
        assert!(refusal(&BackendError::NotFound("app passkey".into())).contains("password"));
    }
}
