//! Preferences › Identity › Pair a device (P7-08), as on the
//! LinuxIdentityPair board (3–6): the code the device shows, the device's
//! name and key fetched under it to check, one touch, then how long it
//! signs in as you. The code is held on whichever server the device
//! used, so each saved server is asked in turn.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use adw::prelude::*;
use gtk::glib;
use spoolframe_client_sdk::{RelayOffer, SdkError};
use spoolframe_linux_core::backend::Client;
use spoolframe_linux_core::identity::Identities;
use spoolframe_protocol::pairing::RelayCode;
use spoolframe_protocol::portable::PortableUserId;

use crate::dialog::{buttons, closes, column, dim};
use crate::key_prompt::{self, Action};

/// How long a paired device signs in as you.
const SIGN_IN_FOR: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// Under the code when no server holds it (6).
pub const NOT_WAITING: &str =
    "No device is waiting with that code. Codes last 5 minutes: ask the device for a new one.";

/// Whether `text` is a whole code, as the device shows it or typed loosely.
pub fn is_code(text: &str) -> bool {
    RelayCode::parse(text).is_some()
}

/// "Living-room TV signs in as you until Jan 8."
pub fn paired_line(name: &str, now: SystemTime) -> String {
    format!(
        "{name} signs in as you until {}.",
        crate::invite::month_day(now + SIGN_IN_FOR)
    )
}

/// What the dialog needs.
pub struct Needs {
    pub identities: Identities,
    pub client: Arc<dyn Client>,
    /// The identity the device pairs with.
    pub id: PortableUserId,
    /// The key prompt's words for this identity.
    pub action: Action,
    pub done: Rc<dyn Fn()>,
}

/// The dialog, over `parent`.
pub fn open(parent: &impl IsA<gtk::Widget>, needs: Needs) {
    let dialog = adw::Dialog::builder()
        .title("Pair a device")
        .content_width(460)
        .build();
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeft)
        .vhomogeneous(false)
        .build();

    // 3 · The code.
    let code_page = column();
    code_page.append(&dim(
        "On the device you’re adding, choose Sign in with your identity. It shows a code. Type \
         it here.",
    ));
    let code = gtk::Entry::builder()
        .placeholder_text("ABCDE-FGHJK")
        .activates_default(true)
        .css_classes(["field", "monospace"])
        .build();
    code_page.append(&code);
    let code_error = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["error", "caption"])
        .build();
    code_page.append(&code_error);
    let go = gtk::Button::builder()
        .label("Continue")
        .halign(gtk::Align::End)
        .sensitive(false)
        .css_classes(["suggested-action"])
        .build();
    code_page.append(&go);
    stack.add_named(&code_page, Some("code"));

    // 4 · Check it.
    let check = column();
    let fetched = dim("");
    check.append(&fetched);
    let rows = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let device = adw::ActionRow::builder().use_markup(false).build();
    device.add_prefix(&gtk::Image::from_icon_name(crate::icons::MONITOR));
    rows.append(&device);
    check.append(&rows);
    check.append(&dim(
        "It gets its own sign-in as you for 90 days. You can revoke it here.",
    ));
    let check_buttons = buttons();
    let check_cancel = gtk::Button::builder().label("Cancel").build();
    let pair = gtk::Button::builder()
        .label("Pair")
        .css_classes(["suggested-action"])
        .build();
    check_buttons.append(&check_cancel);
    check_buttons.append(&pair);
    check.append(&check_buttons);
    stack.add_named(&check, Some("check"));

    // 5 · Paired.
    let paired = column();
    paired.append(
        &gtk::Label::builder()
            .label("Paired")
            .css_classes(["title-3"])
            .build(),
    );
    let paired_until = dim("");
    paired.append(&paired_until);
    let done = gtk::Button::builder()
        .label("Done")
        .halign(gtk::Align::End)
        .css_classes(["suggested-action"])
        .build();
    paired.append(&done);
    stack.add_named(&paired, Some("paired"));

    crate::start::with_cancel(&dialog, &stack);
    dialog.set_default_widget(Some(&go));

    {
        let (go, code_error) = (go.clone(), code_error.clone());
        code.connect_changed(move |entry| {
            code_error.set_visible(false);
            go.set_sensitive(is_code(&entry.text()));
        });
    }
    let needs = Rc::new(needs);
    let offer: Rc<std::cell::RefCell<Option<RelayOffer>>> = Rc::default();
    {
        let (needs, offer, stack, fetched, device, code) = (
            needs.clone(),
            offer.clone(),
            stack.clone(),
            fetched.clone(),
            device.clone(),
            code.clone(),
        );
        go.connect_clicked(move |go| {
            let typed = code.text().to_string();
            go.set_sensitive(false);
            let (needs, offer, stack, fetched, device, code_error, go) = (
                needs.clone(),
                offer.clone(),
                stack.clone(),
                fetched.clone(),
                device.clone(),
                code_error.clone(),
                go.downgrade(),
            );
            glib::spawn_future_local(async move {
                let found = find(&needs, &typed).await;
                if let Some(go) = go.upgrade() {
                    go.set_sensitive(true);
                }
                match found {
                    Ok((server, found)) => {
                        fetched.set_text(&format!(
                            "Fetched from {server}. Check these match what the device shows."
                        ));
                        device.set_title(found.name());
                        device.set_subtitle(&format!("Device key {}", found.fingerprint()));
                        *offer.borrow_mut() = Some(found);
                        stack.set_visible_child_name("check");
                    }
                    Err(why) => {
                        code_error.set_text(&why);
                        code_error.set_visible(true);
                    }
                }
            });
        });
    }
    closes(&check_cancel, &dialog);
    {
        let (needs, dialog, stack) = (needs.clone(), dialog.downgrade(), stack.clone());
        pair.connect_clicked(move |_| {
            let (Some(dialog), Some(found)) = (dialog.upgrade(), offer.borrow().clone()) else {
                return;
            };
            let name = found.name().to_owned();
            let id = needs.id;
            let op = key_prompt::op_on(&needs.identities, move |ids, prompt| {
                let (key, found) = (ids.key(prompt), found.clone());
                Box::pin(async move { ids.answer_pairing(&found, id, key.as_ref()).await })
            });
            let action = Action {
                touch: format!("Touch your key to pair {name}"),
                ..needs.action.clone()
            };
            let (stack, paired_until, needs) = (stack.clone(), paired_until.clone(), needs.clone());
            key_prompt::act(&dialog, action, op, move |()| {
                paired_until.set_text(&paired_line(&name, SystemTime::now()));
                stack.set_visible_child_name("paired");
                // The Devices list behind shows it now, however this
                // dialog then closes.
                (needs.done)();
            });
        });
    }
    closes(&done, &dialog);
    dialog.present(Some(parent));
}

/// The request held under `code` on a saved server, and that server's
/// name; else what to say under the code.
async fn find(needs: &Needs, code: &str) -> Result<(String, RelayOffer), String> {
    let servers = needs
        .client
        .servers()
        .await
        .map_err(|e| crate::app::sentence(&e.to_string()))?;
    let servers: Vec<_> = servers.into_iter().filter(|s| s.pinned).collect();
    let (keys, code): (Vec<String>, String) = (
        servers.iter().map(|s| s.key.0.clone()).collect(),
        code.to_owned(),
    );
    // Every server at once: one that cannot be reached holds up only
    // itself.
    let found = needs
        .identities
        .run(move |ids| Box::pin(async move { Ok(ids.find_pairing(&keys, &code).await) }))
        .await
        .map_err(|e| crate::app::sentence(&e.to_string()))?;
    let errors = match found {
        Ok((at, offer)) => return Ok((servers[at].name.clone(), offer)),
        Err(errors) => errors,
    };
    // A server that could not be asked may be the one: said only when
    // none had the code; the last of them, as servers are listed.
    let trouble = servers
        .iter()
        .zip(&errors)
        .rev()
        .find(|(_, e)| !matches!(e, SdkError::NotFound(_)))
        .map(|(server, e)| format!("{}: {}", server.name, crate::app::sentence(&e.to_string())));
    Err(match trouble {
        Some(why) => format!("{NOT_WAITING} {why}"),
        None => NOT_WAITING.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_typed_loosely_is_still_a_code() {
        assert!(is_code("abcde fghjk"));
    }

    #[test]
    fn a_short_code_is_not_one() {
        assert!(!is_code("ABCDE-FGH"));
    }
}
