//! Create an identity with a security key (P7-08), as on the
//! LinuxIdentityCreate board: its name, whether to add a backup key, then
//! the key prompt makes it (each key's touch and PIN; with a backup, the
//! keys plugged in together or one after another), and this computer
//! pairs itself. Then the fingerprint, to compare when friends invite.
//!
//! The backup is asked about before any key is touched, not after the
//! first as the board has it: the SDK makes the identity from all its
//! keys at once (each key signs a genesis that names them all).

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use spoolframe_linux_core::identity::{Identities, KeyLabel};
use spoolframe_linux_core::security_key::PinPrompt;

use crate::key_prompt::{self, Action, Op};

/// The names the keys get, first and backup.
pub const KEY_NAMES: [&str; 2] = ["Main key", "Backup key"];

/// "Rooted in 2 keys: Main key, Backup key. This computer is paired with
/// it."
pub fn rooted_line(keys: &[&str]) -> String {
    let count = match keys.len() {
        1 => "1 key".to_owned(),
        n => format!("{n} keys"),
    };
    format!(
        "Rooted in {count}: {}. This computer is paired with it.",
        keys.join(", ")
    )
}

/// The labels the keys get, made now.
pub fn labels(backup: bool, now: i64) -> Vec<KeyLabel> {
    KEY_NAMES[..if backup { 2 } else { 1 }]
        .iter()
        .map(|name| KeyLabel {
            name: (*name).to_owned(),
            maker: None,
            added_at: now,
        })
        .collect()
}

/// The dialog; `done` once the identity is made.
pub fn open(parent: &impl IsA<gtk::Widget>, identities: Identities, done: Rc<dyn Fn()>) {
    let dialog = adw::Dialog::builder()
        .title("Create your identity")
        .content_width(460)
        .build();
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeft)
        .vhomogeneous(false)
        .build();

    // 1 · Name.
    let name_page = column();
    name_page.append(&step("Step 1 of 3"));
    let name = gtk::Entry::builder()
        .placeholder_text("Alice")
        .activates_default(true)
        .css_classes(["field"])
        .build();
    name_page.append(&name);
    name_page.append(&dim(
        "The name friends see when you visit their servers. You can change it later.",
    ));
    let next = gtk::Button::builder()
        .label("Continue")
        .halign(gtk::Align::End)
        .sensitive(false)
        .css_classes(["suggested-action"])
        .build();
    name_page.append(&next);
    stack.add_named(&name_page, Some("name"));

    // 2 · A backup key?
    let backup_page = column();
    backup_page.append(&step("Step 2 of 3"));
    backup_page.append(&title("Add a backup key?"));
    backup_page.append(&dim(
        "If you lose your only key, nobody can act for this identity again: not you, not us. A \
         second key in a drawer fixes that. It needs its own PIN and touch. You can also add one \
         later.",
    ));
    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let just_one = gtk::Button::builder().label("Just one key").build();
    let with_backup = gtk::Button::builder()
        .label("Use a backup key")
        .css_classes(["suggested-action"])
        .build();
    buttons.append(&just_one);
    buttons.append(&with_backup);
    backup_page.append(&buttons);
    stack.add_named(&backup_page, Some("backup"));

    // 3 · Ready.
    let ready_page = column();
    ready_page.append(&title("Your identity is ready"));
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .css_classes(["card", "identity-card"])
        .build();
    card.append(
        &gtk::Label::builder()
            .label("Identity fingerprint")
            .css_classes(["caption", "dim-label"])
            .margin_top(12)
            .build(),
    );
    let fingerprint = gtk::Label::builder()
        .selectable(true)
        .css_classes(["title-2", "monospace", "invite-identity"])
        .build();
    card.append(&fingerprint);
    card.append(
        &gtk::Label::builder()
            .label("Friends can compare this when they invite you.")
            .css_classes(["caption", "dim-label"])
            .margin_bottom(12)
            .build(),
    );
    ready_page.append(&card);
    let rooted = dim("");
    ready_page.append(&rooted);
    let done_button = gtk::Button::builder()
        .label("Done")
        .halign(gtk::Align::End)
        .css_classes(["suggested-action"])
        .build();
    ready_page.append(&done_button);
    stack.add_named(&ready_page, Some("ready"));

    crate::start::with_cancel(&dialog, &stack);
    dialog.set_default_widget(Some(&next));

    {
        let next = next.clone();
        name.connect_changed(move |entry| next.set_sensitive(!entry.text().trim().is_empty()));
    }
    {
        let stack = stack.clone();
        next.connect_clicked(move |_| stack.set_visible_child_name("backup"));
    }
    let make = {
        let (dialog, stack, name, fingerprint, rooted) = (
            dialog.downgrade(),
            stack.clone(),
            name.clone(),
            fingerprint.clone(),
            rooted.clone(),
        );
        Rc::new(move |backup: bool| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let identity_name = name.text().trim().to_owned();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            let labels = labels(backup, now);
            let ids = identities.clone();
            let op: Op<(String, Vec<String>)> = Rc::new(move |prompt: Arc<dyn PinPrompt>| {
                // One key for the action: it puts each root on a key the
                // action has not used yet.
                let key = ids.key(prompt);
                let keys: Vec<_> = labels.iter().map(|l| (key.clone(), l.clone())).collect();
                let (name, ids) = (identity_name.clone(), ids.clone());
                Box::pin(async move {
                    ids.run(move |ids| {
                        Box::pin(async move {
                            let id = ids.create(&name, &keys).await?;
                            let names = keys.iter().map(|(_, l)| l.name.clone()).collect();
                            Ok((id.fingerprint(), names))
                        })
                    })
                    .await
                })
            });
            let action = Action {
                touch: if backup {
                    "Touch each key when it blinks to make your identity".into()
                } else {
                    "Touch your key when it blinks to make your identity".into()
                },
                keys: Some(if backup {
                    "Plug in both keys, or one and then the other when asked.".into()
                } else {
                    "The key makes your identity’s root and keeps it. Nothing secret is stored \
                     on this computer."
                        .into()
                }),
                identity: None,
                other_key: None,
            };
            let (stack, fingerprint, rooted) = (stack.clone(), fingerprint.clone(), rooted.clone());
            key_prompt::act(&dialog, action, op, move |(made, names)| {
                fingerprint.set_text(&made);
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                rooted.set_text(&rooted_line(&names));
                stack.set_visible_child_name("ready");
            });
        })
    };
    {
        let make = make.clone();
        just_one.connect_clicked(move |_| make(false));
    }
    with_backup.connect_clicked(move |_| make(true));
    {
        let dialog = dialog.downgrade();
        done_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
            done();
        });
    }
    dialog.present(Some(parent));
}

fn column() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build()
}

fn step(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["caption", "dim-label"])
        .build()
}

fn title(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["title-3"])
        .build()
}

fn dim(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ready_line_names_each_key() {
        assert_eq!(
            rooted_line(&["Main key", "Backup key"]),
            "Rooted in 2 keys: Main key, Backup key. This computer is paired with it."
        );
    }

    #[test]
    fn without_a_backup_there_is_one_key() {
        assert_eq!(labels(false, 0).len(), 1);
        assert_eq!(labels(true, 0)[1].name, "Backup key");
    }
}
