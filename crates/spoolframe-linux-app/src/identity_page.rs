//! Settings › Your identity (P7-08), as on the LinuxIdentity board: this
//! computer's portable identity rooted in security keys. Its fingerprint
//! and keys, the devices it paired, a Renew banner in this computer's
//! last 14 days; or, with none yet, where to start. Every root action
//! goes through the key prompt ([`crate::key_prompt`]).

use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use adw::prelude::*;
use gtk::{gio, glib};
use spoolframe_client_sdk::{RootKey, SdkError, SecurityKey};
use spoolframe_linux_core::identity::KeyLabel;
use spoolframe_linux_core::identity::{Device, Identities, IdentityView, KeyRoot};
use spoolframe_linux_core::security_key::{Step, stepped};

use crate::key_prompt::{self, Action};

/// "Oct 9", from unix seconds.
fn day(at: i64) -> String {
    crate::invite::month_day(
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(at.max(0) as u64),
    )
}

/// "Identity R2VN-8KQT-M4XA-H7PD · rooted in 2 security keys".
pub fn header_line(view: &IdentityView) -> String {
    let keys = match view.keys.len() {
        1 => "rooted in 1 security key".to_owned(),
        n => format!("rooted in {n} security keys"),
    };
    format!("Identity {} · {keys}", view.fingerprint)
}

/// A key's name, or its place when this computer has none for it.
pub fn key_name(key: &KeyRoot, index: usize) -> String {
    key.label
        .as_ref()
        .map_or_else(|| format!("Key {}", index + 1), |l| l.name.clone())
}

/// Each key's name, in order.
pub fn key_names(view: &IdentityView) -> Vec<String> {
    view.keys
        .iter()
        .enumerate()
        .map(|(i, k)| key_name(k, i))
        .collect()
}

/// "Feitian ePass · added Oct 9".
pub fn key_line(key: &KeyRoot) -> String {
    match &key.label {
        Some(label) => {
            let added = format!("added {}", day(label.added_at));
            match &label.maker {
                Some(maker) => format!("{maker} · {added}"),
                None => added,
            }
        }
        None => "Added on another device".to_owned(),
    }
}

/// A device's name: this computer, or what it was called when paired.
pub fn device_name(device: &Device) -> String {
    if device.this_computer {
        "This computer".to_owned()
    } else {
        device
            .credential
            .label
            .clone()
            .unwrap_or_else(|| "A device".to_owned())
    }
}

/// "Paired Oct 9 · ends Jan 8 · key 4HCX-Q9MB-2KWE-7DNR"; this computer's
/// without its key.
pub fn device_line(device: &Device) -> String {
    let c = &device.credential;
    let mut line = format!("Paired {} · ends {}", day(c.issued_at), day(c.expires_at));
    if !device.this_computer {
        line.push_str(&format!(" · key {}", c.device_key.fingerprint()));
    }
    line
}

/// The Renew banner's words.
pub fn renew_line(ends: i64) -> String {
    format!(
        "This computer’s sign-in with your identity ends on {}. Renewing takes one touch of a key.",
        day(ends)
    )
}

/// The name a key added here gets: Backup key, unless one has it.
pub fn added_key_name(view: &IdentityView) -> String {
    let names = key_names(view);
    let backup = crate::create_identity::KEY_NAMES[1];
    if !names.iter().any(|n| n == backup) {
        return backup.to_owned();
    }
    (names.len() + 1..)
        .map(|n| format!("Key {n}"))
        .find(|name| !names.contains(name))
        .expect("an unused name")
}

/// Add a key's two steps: the new key, then one already a root approves.
pub fn add_key_steps(view: &IdentityView) -> [Step; 2] {
    let names = key_names(view);
    [
        Step {
            caption: "Step 1 of 2".into(),
            title: "Plug in the new key and touch it".into(),
            line: "Then its PIN. It becomes a root: it can do anything your other keys can.".into(),
        },
        Step {
            caption: "Step 2 of 2".into(),
            title: format!("Now touch {}", key_prompt::or_list(&names)),
            line: "An existing key approves the new one.".into(),
        },
    ]
}

/// An exported record's file extension.
pub use crate::file_helper::RECORD_EXTENSION;

/// "alice.spoolframe-identity": the identity's name, lower case, with
/// anything a file name should not hold as a dash.
pub fn record_file_name(name: &str) -> String {
    let stem: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let stem = stem.trim_matches('-');
    let stem = if stem.is_empty() { "identity" } else { stem };
    format!("{stem}.{RECORD_EXTENSION}")
}

/// What erasing from the keys plugged in would leave, under the Remove
/// alert's checkbox (sf-design's two cases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EraseNote {
    /// Some keys stay out: a quiet line.
    Some(String),
    /// Every key is plugged in: the red card's title and words.
    All { title: String, body: String },
    /// None of its keys is plugged in.
    None(String),
}

/// The note for erasing `identity` from the keys plugged in, given each
/// key's name and whether it is plugged in.
pub fn erase_note(identity: &str, keys: &[(String, bool)]) -> EraseNote {
    let plugged: Vec<String> = keys
        .iter()
        .filter(|(_, p)| *p)
        .map(|(n, _)| n.clone())
        .collect();
    let out: Vec<String> = keys
        .iter()
        .filter(|(_, p)| !*p)
        .map(|(n, _)| n.clone())
        .collect();
    if plugged.is_empty() {
        return EraseNote::None(format!(
            "None of {identity}’s keys is plugged in: there is nothing to erase."
        ));
    }
    if out.is_empty() {
        let from = match plugged.len() {
            1 => "Erased from it".to_owned(),
            2 => "Erased from both".to_owned(),
            _ => "Erased from all of them".to_owned(),
        };
        return EraseNote::All {
            title: if plugged.len() == 1 {
                format!("This is {identity}’s only key")
            } else {
                format!("These are all of {identity}’s keys")
            },
            body: format!(
                "{from}, nobody can act for {identity} again: no pairing, renewing or revoking, and                  no recovery. {identity}’s other devices keep signing in only until their sign-ins                  end."
            ),
        };
    }
    let keeps = if out.len() == 1 { "keeps" } else { "keep" };
    EraseNote::Some(format!(
        "Plugged in: {}. {} {keeps} it, so {identity} can still be recovered.",
        plugged.join(", "),
        key_prompt::or_list(&out).replace(" or ", " and ")
    ))
}

/// The note when the keys plugged in will not say which they are: a
/// warning, as they may be all of them.
pub fn unknown_note(identity: &str) -> String {
    format!(
        "Couldn’t tell which of {identity}’s keys are plugged in. If they’re all of them, erasing \
         locks {identity} for good."
    )
}

/// What the page acts with.
struct View {
    identities: Identities,
    /// The saved servers, which a device's pairing code may be held on.
    client: Arc<dyn spoolframe_linux_core::backend::Client>,
    toasts: glib::WeakRef<adw::ToastOverlay>,
    page: glib::WeakRef<adw::PreferencesPage>,
    groups: std::cell::RefCell<Vec<adw::PreferencesGroup>>,
}

/// The Identity tab.
pub fn page(
    client: Arc<dyn spoolframe_linux_core::backend::Client>,
    toasts: &adw::ToastOverlay,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Identity")
        .icon_name(crate::icons::IDENTITY)
        .build();
    let view = Rc::new(View {
        identities: client.identities(),
        client,
        toasts: toasts.downgrade(),
        page: page.downgrade(),
        groups: Default::default(),
    });
    view.load();
    // The page owns its view; the loads and buttons hold it weakly.
    page.connect_destroy(move |_| {
        let _ = &view;
    });
    page
}

impl View {
    fn toast(&self, text: &str) {
        if let Some(toasts) = self.toasts.upgrade() {
            toasts.add_toast(crate::markup::toast(text));
        }
    }

    fn load(self: &Rc<Self>) {
        let (identities, weak) = (self.identities.clone(), Rc::downgrade(self));
        glib::spawn_future_local(async move {
            let current = identities
                .run(|ids| Box::pin(async move { ids.current().await }))
                .await;
            let Some(view) = weak.upgrade() else { return };
            match current {
                Ok(Some(identity)) => view.show(&identity),
                Ok(None) => view.show_start(),
                Err(e) => view.toast(&crate::app::sentence(&e.to_string())),
            }
        });
    }

    fn replace(&self, groups: Vec<adw::PreferencesGroup>) {
        let Some(page) = self.page.upgrade() else {
            return;
        };
        for group in self.groups.borrow_mut().drain(..) {
            page.remove(&group);
        }
        for group in &groups {
            page.add(group);
        }
        *self.groups.borrow_mut() = groups;
    }

    /// No identity yet: what it is, and the ways to start.
    fn show_start(self: &Rc<Self>) {
        let intro = adw::PreferencesGroup::builder()
            .title("Your own identity")
            .description(
                "One identity for every server and device: friends’ servers know you by it, and \
                 your devices sign in with it. A security key holds its root, so no computer or \
                 phone has to.",
            )
            .build();
        let start = adw::PreferencesGroup::builder()
            .title("Start")
            .description(
                "Already have an identity on your iPhone? Use Sign in with your identity on Who’s \
                 watching instead.",
            )
            .build();
        let create = row(
            "Create with a security key",
            "Plug in a FIDO2 key. A second key as a backup is a good idea.",
        );
        create.add_prefix(&gtk::Image::from_icon_name(crate::icons::PASSKEY));
        let create_button = gtk::Button::builder()
            .label("Create…")
            .valign(gtk::Align::Center)
            .css_classes(["suggested-action"])
            .build();
        let weak = Rc::downgrade(self);
        create_button.connect_clicked(move |button| {
            let Some(view) = weak.upgrade() else { return };
            let reload = Rc::downgrade(&view);
            crate::create_identity::open(
                button,
                view.identities.clone(),
                Rc::new(move || {
                    if let Some(view) = reload.upgrade() {
                        view.load();
                    }
                }),
            );
        });
        create.add_suffix(&create_button);
        start.add(&create);
        let recover = row(
            "Recover from a server",
            "Your identity is on a key, and a server you use knows it.",
        );
        recover.add_prefix(&gtk::Image::from_icon_name(crate::icons::SERVER));
        let recover_button = gtk::Button::builder()
            .label("Recover…")
            .valign(gtk::Align::Center)
            .build();
        let weak = Rc::downgrade(self);
        recover_button.connect_clicked(move |button| {
            let Some(view) = weak.upgrade() else { return };
            let reload = Rc::downgrade(&view);
            crate::recover_identity::open(
                button,
                view.identities.clone(),
                view.client.clone(),
                Rc::new(move || {
                    if let Some(view) = reload.upgrade() {
                        view.toast("Recovered, and this computer is paired");
                        view.load();
                    }
                }),
            );
        });
        recover.add_suffix(&recover_button);
        start.add(&recover);
        let import = row(
            "Import a file",
            "From an identity record you exported, and any one of its keys.",
        );
        import.add_prefix(&gtk::Image::from_icon_name(crate::icons::JOIN));
        let import_button = gtk::Button::builder()
            .label("Import…")
            .valign(gtk::Align::Center)
            .build();
        let weak = Rc::downgrade(self);
        import_button.connect_clicked(move |button| {
            if let Some(view) = weak.upgrade() {
                view.import(button);
            }
        });
        import.add_suffix(&import_button);
        start.add(&import);
        self.replace(vec![intro, start]);
    }

    fn show(self: &Rc<Self>, identity: &IdentityView) {
        let mut groups = Vec::new();

        let header = adw::PreferencesGroup::new();
        let who = adw::ActionRow::builder().use_markup(false).build();
        who.set_title(&identity.identity.name);
        who.set_subtitle(&header_line(identity));
        who.add_prefix(
            &gtk::Image::builder()
                .icon_name(crate::icons::IDENTITY)
                .pixel_size(24)
                .css_classes(["invite-identity", "identity-avatar"])
                .build(),
        );
        who.add_suffix(
            &gtk::Label::builder()
                .label("Portable")
                .valign(gtk::Align::Center)
                .css_classes(["chip", "invite-chip", "invite-identity"])
                .build(),
        );
        header.add(&who);
        groups.push(header);

        if let Some(ends) = identity.renewal_due {
            let banner = adw::PreferencesGroup::new();
            let renew_row = adw::ActionRow::builder()
                .use_markup(false)
                .title("Renew soon")
                .css_classes(["renew-banner"])
                .build();
            renew_row.set_subtitle(&renew_line(ends));
            renew_row.add_prefix(&gtk::Image::from_icon_name(crate::icons::EXPIRED));
            renew_row.add_suffix(&self.renew_button(identity));
            banner.add(&renew_row);
            groups.push(banner);
        }

        let keys = adw::PreferencesGroup::builder()
            .title("Security keys")
            .description("Any one of them can act for your identity. Keep one somewhere safe.")
            .build();
        let only_one = identity.keys.len() == 1;
        for (index, key) in identity.keys.iter().enumerate() {
            let row = adw::ActionRow::builder().use_markup(false).build();
            row.set_title(&key_name(key, index));
            row.set_subtitle(&key_line(key));
            row.add_prefix(
                &gtk::Image::builder()
                    .icon_name(crate::icons::PASSKEY)
                    .css_classes(["invite-identity"])
                    .build(),
            );
            let remove = gtk::Button::builder()
                .label("Remove…")
                .valign(gtk::Align::Center)
                .sensitive(!only_one)
                .css_classes(["destructive-action"])
                .build();
            if only_one {
                remove.set_tooltip_text(Some("Your only key can’t be removed: add another first."));
            }
            let (weak, identity_c, key_c, name) = (
                Rc::downgrade(self),
                identity.clone(),
                key.root.clone(),
                key_name(key, index),
            );
            remove.connect_clicked(move |button| {
                if let Some(view) = weak.upgrade() {
                    view.ask_remove(button, &identity_c, &key_c, &name);
                }
            });
            row.add_suffix(&remove);
            keys.add(&row);
        }
        let add = gtk::Button::builder()
            .label("Add a key…")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let (weak, identity_c) = (Rc::downgrade(self), identity.clone());
        add.connect_clicked(move |button| {
            if let Some(view) = weak.upgrade() {
                view.add_key(button, &identity_c);
            }
        });
        keys.set_header_suffix(Some(&add));
        if only_one {
            keys.set_description(Some(
                "Any one of them can act for your identity. Keep one somewhere safe. Your only \
                 key can’t be removed: add another first.",
            ));
        }
        groups.push(keys);

        let devices = adw::PreferencesGroup::builder()
            .title("Devices")
            .description(
                "Each has its own sign-in for 90 days, renewed early when a key is at hand.",
            )
            .build();
        let pair = gtk::Button::builder()
            .label("Pair a device…")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let (weak, identity_c) = (Rc::downgrade(self), identity.clone());
        pair.connect_clicked(move |button| {
            let Some(view) = weak.upgrade() else { return };
            let reload = Rc::downgrade(&view);
            crate::pair_device::open(
                button,
                crate::pair_device::Needs {
                    identities: view.identities.clone(),
                    client: view.client.clone(),
                    action: View::action(&identity_c, String::new(), None),
                    id: identity_c.identity.id,
                    done: Rc::new(move || {
                        if let Some(view) = reload.upgrade() {
                            view.load();
                        }
                    }),
                },
            );
        });
        devices.set_header_suffix(Some(&pair));
        for device in &identity.devices {
            let row = adw::ActionRow::builder().use_markup(false).build();
            row.set_title(&device_name(device));
            row.set_subtitle(&device_line(device));
            row.add_prefix(&gtk::Image::from_icon_name(if device.this_computer {
                crate::icons::LAPTOP
            } else {
                crate::icons::MONITOR
            }));
            if device.this_computer {
                row.add_suffix(
                    &gtk::Label::builder()
                        .label("This computer")
                        .valign(gtk::Align::Center)
                        .css_classes(["chip", "invite-chip", "invite-account"])
                        .build(),
                );
                row.add_suffix(&self.renew_button(identity));
            } else {
                let revoke = gtk::Button::builder()
                    .label("Revoke…")
                    .valign(gtk::Align::Center)
                    .css_classes(["destructive-action"])
                    .build();
                let (weak, identity_c, device_c) =
                    (Rc::downgrade(self), identity.clone(), device.clone());
                revoke.connect_clicked(move |button| {
                    if let Some(view) = weak.upgrade() {
                        view.ask_revoke(button, &identity_c, &device_c);
                    }
                });
                row.add_suffix(&revoke);
            }
            devices.add(&row);
        }
        groups.push(devices);

        let this_computer = adw::PreferencesGroup::builder()
            .title("On this computer")
            .build();
        let remove = row(
            "Remove from this computer",
            "This computer stops signing in with your identity. Your keys keep it.",
        );
        let remove_button = gtk::Button::builder()
            .label("Remove…")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        let (weak, identity_c) = (Rc::downgrade(self), identity.clone());
        remove_button.connect_clicked(move |button| {
            if let Some(view) = weak.upgrade() {
                view.ask_forget(button, &identity_c);
            }
        });
        remove.add_suffix(&remove_button);
        let export = row(
            "Export your identity record",
            "A file with no secret in it. With it and one of your keys, a new computer can act \
             for you even if no server knows you.",
        );
        let export_button = gtk::Button::builder()
            .label("Export…")
            .valign(gtk::Align::Center)
            .build();
        let (weak, identity_c) = (Rc::downgrade(self), identity.clone());
        export_button.connect_clicked(move |button| {
            if let Some(view) = weak.upgrade() {
                view.ask_export(button, &identity_c);
            }
        });
        export.add_suffix(&export_button);
        this_computer.add(&export);
        this_computer.add(&remove);
        groups.push(this_computer);
        self.replace(groups);
    }

    /// "Remove Alice from this computer?", with erasing it from the keys
    /// plugged in as a choice where real keys are.
    fn ask_forget(self: &Rc<Self>, anchor: &gtk::Button, identity: &IdentityView) {
        let name = identity.identity.name.clone();
        let alert = adw::AlertDialog::builder()
            .heading(format!("Remove {name} from this computer?"))
            .body(
                "This computer stops signing in with your identity. Your keys still hold it: to \
                 use it here again, recover it from a server or import its record.",
            )
            .close_response("cancel")
            .default_response("cancel")
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("remove", "Remove");
        alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        let erase = gtk::CheckButton::builder()
            .label("Also erase it from the keys plugged in: they can’t act for it after")
            .build();
        let note = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["dim-label", "caption"])
            .build();
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .visible(false)
            .css_classes(["card", "erase-all"])
            .build();
        let card_title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .margin_top(10)
            .margin_start(12)
            .margin_end(12)
            .css_classes(["heading", "error"])
            .build();
        let card_body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(12)
            .build();
        card.append(&card_title);
        card.append(&card_body);
        let extra = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        extra.append(&erase);
        extra.append(&note);
        extra.append(&card);
        if self.identities.can_erase() {
            alert.set_extra_child(Some(&extra));
        }
        {
            // Ticked: ask the keys plugged in which of its keys they are,
            // with no PIN and no touch, and say what erasing would leave.
            let (identities, alert_weak) = (self.identities.clone(), alert.downgrade());
            let keys: Vec<(String, RootKey)> = identity
                .keys
                .iter()
                .enumerate()
                .map(|(i, k)| (key_name(k, i), k.root.clone()))
                .collect();
            let (note, card, card_title, card_body) = (
                note.clone(),
                card.clone(),
                card_title.clone(),
                card_body.clone(),
            );
            let name = name.clone();
            erase.connect_toggled(move |erase| {
                let Some(alert) = alert_weak.upgrade() else {
                    return;
                };
                note.set_visible(false);
                card.set_visible(false);
                if !erase.is_active() {
                    alert.set_response_label("remove", "Remove");
                    return;
                }
                alert.set_response_label("remove", "Remove and erase");
                let (identities, keys, name) = (identities.clone(), keys.clone(), name.clone());
                let (note, card, card_title, card_body) = (
                    note.clone(),
                    card.clone(),
                    card_title.clone(),
                    card_body.clone(),
                );
                glib::spawn_future_local(async move {
                    let roots: Vec<RootKey> = keys.iter().map(|(_, r)| r.clone()).collect();
                    let plugged = identities
                        .run(move |ids| Box::pin(async move { Ok(ids.plugged_in(&roots).await) }))
                        .await
                        .ok()
                        .flatten();
                    // A key that will not say: a warning, since they may be
                    // all of its keys.
                    note.remove_css_class("warning");
                    let Some(plugged) = plugged else {
                        note.set_text(&unknown_note(&name));
                        note.add_css_class("warning");
                        note.set_visible(true);
                        return;
                    };
                    let named: Vec<(String, bool)> =
                        keys.iter().map(|(n, _)| n.clone()).zip(plugged).collect();
                    match erase_note(&name, &named) {
                        EraseNote::Some(line) | EraseNote::None(line) => {
                            note.set_text(&line);
                            note.set_visible(true);
                        }
                        EraseNote::All { title, body } => {
                            card_title.set_text(&title);
                            card_body.set_text(&body);
                            card.set_visible(true);
                        }
                    }
                });
            });
        }
        let (weak, id, anchor_weak) = (
            Rc::downgrade(self),
            identity.identity.id,
            anchor.downgrade(),
        );
        alert.connect_response(Some("remove"), move |_, _| {
            let Some(view) = weak.upgrade() else { return };
            if erase.is_active() {
                let Some(anchor) = anchor_weak.upgrade() else {
                    return;
                };
                let op = key_prompt::op_on(&view.identities, move |ids, prompt| {
                    Box::pin(async move {
                        ids.erase_from_keys(id, prompt).await?;
                        ids.forget(id).await
                    })
                });
                let action = Action {
                    touch: format!("Touch each key holding {name} to erase it"),
                    keys: Some("Plug in every key you want it erased from.".into()),
                    identity: Some(name.clone()),
                    other_key: None,
                    not_found: None,
                };
                let reload = Rc::downgrade(&view);
                key_prompt::act(&anchor, action, op, move |()| {
                    if let Some(view) = reload.upgrade() {
                        view.toast("Removed from this computer and erased from the keys");
                        view.load();
                    }
                });
            } else {
                let (identities, reload) = (view.identities.clone(), Rc::downgrade(&view));
                glib::spawn_future_local(async move {
                    let forgot = identities
                        .run(move |ids| Box::pin(async move { ids.forget(id).await }))
                        .await;
                    let Some(view) = reload.upgrade() else { return };
                    match forgot {
                        Ok(()) => {
                            view.toast("Removed from this computer");
                            view.load();
                        }
                        Err(e) => view.toast(&crate::app::sentence(&e.to_string())),
                    }
                });
            }
        });
        alert.present(Some(anchor));
    }

    /// "Export your identity record", then the portal's Save dialog.
    fn ask_export(self: &Rc<Self>, anchor: &gtk::Button, identity: &IdentityView) {
        let file = record_file_name(&identity.identity.name);
        let alert = adw::AlertDialog::builder()
            .heading("Export your identity record")
            .body(format!(
                "Saves {file}. With it and one of your keys, a new computer can act for you even \
                 if no server knows you. Keep it with your backup key."
            ))
            .close_response("cancel")
            .default_response("save")
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("save", "Save…");
        alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        let (weak, id) = (Rc::downgrade(self), identity.identity.id);
        alert.connect_response(Some("save"), move |_, _| {
            let Some(view) = weak.upgrade() else { return };
            let (identities, file, reload) =
                (view.identities.clone(), file.clone(), Rc::downgrade(&view));
            glib::spawn_future_local(async move {
                let saved = async {
                    let bytes = identities
                        .run(move |ids| Box::pin(async move { ids.export(id).await }))
                        .await
                        .map_err(|e| e.to_string())?;
                    gio::spawn_blocking(move || {
                        crate::file_helper::save("Export your identity record", &file, &bytes)
                    })
                    .await
                    .map_err(|_| "the save stopped".to_owned())?
                }
                .await;
                let Some(view) = reload.upgrade() else { return };
                match saved {
                    Ok(Some(name)) => view.toast(&format!("Saved {name}")),
                    Ok(None) => {}
                    Err(e) => view.toast(&crate::app::sentence(&e)),
                }
            });
        });
        alert.present(Some(anchor));
    }

    /// Import a file: the portal's Open dialog, the record read, then
    /// the board's check and any one of its keys.
    fn import(self: &Rc<Self>, anchor: &gtk::Button) {
        let (weak, anchor_weak) = (Rc::downgrade(self), anchor.downgrade());
        glib::spawn_future_local(async move {
            let filter = crate::file_helper::Filter {
                name: "Spoolframe identity".into(),
                pattern: format!("*.{RECORD_EXTENSION}"),
            };
            let opened = gio::spawn_blocking(move || {
                crate::file_helper::open("Import your identity", &filter)
            })
            .await
            .unwrap_or_else(|_| Err("the open stopped".to_owned()));
            let (Some(view), Some(anchor)) = (weak.upgrade(), anchor_weak.upgrade()) else {
                return;
            };
            let file = match opened {
                Ok(Some(file)) => file,
                Ok(None) => return,
                Err(e) => return view.toast(&crate::app::sentence(&e)),
            };
            match Identities::read_record(&file.bytes) {
                Ok(record) => crate::import_identity::open(
                    &anchor,
                    view.identities.clone(),
                    file.name,
                    record,
                    {
                        let reload = Rc::downgrade(&view);
                        Rc::new(move || {
                            if let Some(view) = reload.upgrade() {
                                view.toast("Imported, and this computer is paired");
                                view.load();
                            }
                        })
                    },
                ),
                Err(e) => view.toast(&crate::app::sentence(&e.to_string())),
            }
        });
    }

    /// The key prompt's action, for this identity: "Any of your keys: …".
    fn action(identity: &IdentityView, touch: String, except: Option<&RootKey>) -> Action {
        let names = key_names(identity);
        let other_key = identity
            .keys
            .iter()
            .enumerate()
            .find(|(_, k)| Some(&k.root) != except)
            .filter(|_| identity.keys.len() > 1)
            .map(|(i, k)| key_name(k, i));
        Action {
            touch,
            keys: Some(match names.len() {
                1 => format!("Your key: {}.", names[0]),
                _ => format!("Any of your keys: {}.", key_prompt::or_list(&names)),
            }),
            identity: Some(identity.identity.name.clone()),
            other_key,
            not_found: None,
        }
    }

    /// Runs `work` with a key behind the prompt over `anchor`, then
    /// `toast` and reloads.
    fn with_key(
        self: &Rc<Self>,
        anchor: &impl IsA<gtk::Widget>,
        action: Action,
        work: impl Fn(
            Identities,
            Arc<dyn SecurityKey>,
        )
            -> spoolframe_linux_core::backend::BoxFuture<'static, Result<(), SdkError>>
        + 'static,
        toast: &'static str,
    ) {
        let op = key_prompt::op_on(&self.identities, move |ids, prompt| {
            let key = ids.key(prompt);
            work(ids, key)
        });
        let weak = Rc::downgrade(self);
        key_prompt::act(anchor, action, op, move |()| {
            if let Some(view) = weak.upgrade() {
                view.toast(toast);
                view.load();
            }
        });
    }

    /// Add a key: the new one registers and proves it holds its root,
    /// then one already a root approves.
    fn add_key(self: &Rc<Self>, anchor: &gtk::Button, identity: &IdentityView) {
        let [first, second] = add_key_steps(identity);
        let (id, view_c) = (identity.identity.id, identity.clone());
        let name = added_key_name(identity);
        let op = key_prompt::op_on(&self.identities, move |ids, prompt| {
            let new_key = ids.new_key(stepped(prompt.clone(), first.clone()), &view_c);
            let signer = ids.key(stepped(prompt, second.clone()));
            let label = KeyLabel {
                name: name.clone(),
                maker: None,
                added_at: crate::invite::unix_now(),
            };
            Box::pin(async move {
                ids.add_key(id, signer.as_ref(), new_key.as_ref(), label)
                    .await
            })
        });
        let action = Action {
            touch: "Plug in the new key and touch it".into(),
            keys: None,
            identity: Some(identity.identity.name.clone()),
            other_key: None,
            not_found: None,
        };
        let weak = Rc::downgrade(self);
        key_prompt::act(anchor, action, op, move |()| {
            if let Some(view) = weak.upgrade() {
                view.toast("Key added");
                view.load();
            }
        });
    }

    fn renew_button(self: &Rc<Self>, identity: &IdentityView) -> gtk::Button {
        let button = gtk::Button::builder()
            .label("Renew")
            .valign(gtk::Align::Center)
            .build();
        let (weak, identity) = (Rc::downgrade(self), identity.clone());
        button.connect_clicked(move |button| {
            let Some(view) = weak.upgrade() else { return };
            let id = identity.identity.id;
            view.with_key(
                button,
                View::action(
                    &identity,
                    "Touch your key to renew this computer".into(),
                    None,
                ),
                move |ids, key| Box::pin(async move { ids.pair_self(id, key.as_ref()).await }),
                "Renewed for 90 days",
            );
        });
        button
    }

    fn ask_remove(
        self: &Rc<Self>,
        anchor: &gtk::Button,
        identity: &IdentityView,
        root: &RootKey,
        name: &str,
    ) {
        let others: Vec<String> = identity
            .keys
            .iter()
            .enumerate()
            .filter(|(_, k)| &k.root != root)
            .map(|(i, k)| key_name(k, i))
            .collect();
        let alert = adw::AlertDialog::builder()
            .heading(format!("Remove {name}?"))
            .body(format!(
                "It can never act for your identity again, even if it turns up. {} stays. You’ll \
                 touch {} to approve.\n\nLost a key? Remove it now. Anyone with it and its PIN \
                 could act for you.",
                key_prompt::or_list(&others),
                key_prompt::or_list(&others)
            ))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("remove", "Remove");
        alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        let (weak, identity, root, name) = (
            Rc::downgrade(self),
            identity.clone(),
            root.clone(),
            name.to_owned(),
        );
        let anchor_weak = anchor.downgrade();
        alert.connect_response(Some("remove"), move |_, _| {
            let (Some(view), Some(anchor)) = (weak.upgrade(), anchor_weak.upgrade()) else {
                return;
            };
            let (id, root) = (identity.identity.id, root.clone());
            view.with_key(
                &anchor,
                View::action(
                    &identity,
                    format!("Touch another key to remove {name}"),
                    Some(&root),
                ),
                move |ids, key| {
                    let root = root.clone();
                    Box::pin(async move { ids.remove_key(id, key.as_ref(), &root).await })
                },
                "Key removed",
            );
        });
        alert.present(Some(anchor));
    }

    fn ask_revoke(self: &Rc<Self>, anchor: &gtk::Button, identity: &IdentityView, device: &Device) {
        let name = device_name(device);
        let alert = adw::AlertDialog::builder()
            .heading(format!("Revoke {name}?"))
            .body(
                "It stops signing in as you on every server, once each server hears of it. Your \
                 servers keep your history.",
            )
            .close_response("cancel")
            .default_response("cancel")
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("revoke", "Revoke");
        alert.set_response_appearance("revoke", adw::ResponseAppearance::Destructive);
        let (weak, identity, key) = (
            Rc::downgrade(self),
            identity.clone(),
            device.credential.device_key,
        );
        let anchor_weak = anchor.downgrade();
        alert.connect_response(Some("revoke"), move |_, _| {
            let (Some(view), Some(anchor)) = (weak.upgrade(), anchor_weak.upgrade()) else {
                return;
            };
            let id = identity.identity.id;
            view.with_key(
                &anchor,
                View::action(&identity, format!("Touch your key to revoke {name}"), None),
                move |ids, signer| {
                    Box::pin(async move { ids.revoke(id, signer.as_ref(), key).await })
                },
                "Device revoked",
            );
        });
        alert.present(Some(anchor));
    }
}

fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().use_markup(false).build();
    row.set_title(title);
    row.set_subtitle(subtitle);
    row
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::identity::{KeyLabel, in_memory};
    use spoolframe_linux_core::testing::ready;

    use super::*;

    use spoolframe_linux_core::testing::NoPrompt;

    fn alice() -> IdentityView {
        let identities = in_memory();
        let key = identities.key(Arc::new(NoPrompt));
        let label = |name: &str| KeyLabel {
            name: name.into(),
            maker: Some("Feitian ePass".into()),
            added_at: 1_791_000_000,
        };
        let id = ready(identities.create("Alice", &[(key.clone(), label("Keyring key"))])).unwrap();
        ready(identities.add_key(id, key.as_ref(), key.as_ref(), label("Drawer key"))).unwrap();
        ready(identities.current()).unwrap().unwrap()
    }

    #[test]
    fn the_header_names_the_fingerprint_and_how_many_keys() {
        let view = alice();
        assert_eq!(
            header_line(&view),
            format!("Identity {} · rooted in 2 security keys", view.fingerprint)
        );
    }

    #[test]
    fn a_key_says_its_maker_and_when_it_was_added() {
        let view = alice();
        assert!(
            key_line(&view.keys[0]).starts_with("Feitian ePass · added "),
            "{}",
            key_line(&view.keys[0])
        );
    }

    #[test]
    fn this_computer_is_named_so_and_shows_no_key() {
        let view = alice();
        let here = view.devices.iter().find(|d| d.this_computer).unwrap();
        assert_eq!(device_name(here), "This computer");
        assert!(!device_line(here).contains("key"), "{}", device_line(here));
    }

    #[test]
    fn the_prompt_lists_the_keys_and_offers_the_other_one_when_locked() {
        let view = alice();
        let action = View::action(&view, "Touch".into(), Some(&view.keys[0].root));
        assert_eq!(
            (action.keys.as_deref(), action.other_key.as_deref()),
            (
                Some("Any of your keys: Keyring key or Drawer key."),
                Some("Drawer key")
            )
        );
    }

    fn keys(plugged: &[bool]) -> Vec<(String, bool)> {
        ["Main key", "Backup key"]
            .iter()
            .zip(plugged)
            .map(|(n, p)| ((*n).to_owned(), *p))
            .collect()
    }

    #[test]
    fn erasing_with_a_key_left_out_says_it_can_still_be_recovered() {
        assert_eq!(
            erase_note("Alice", &keys(&[true, false])),
            EraseNote::Some(
                "Plugged in: Main key. Backup key keeps it, so Alice can still be recovered."
                    .into()
            )
        );
    }

    #[test]
    fn erasing_from_every_key_warns_that_nobody_can_act_again() {
        let EraseNote::All { title, body } = erase_note("Alice", &keys(&[true, true])) else {
            panic!("no warning");
        };
        assert_eq!(title, "These are all of Alice’s keys");
        assert!(
            body.starts_with("Erased from both, nobody can act for Alice again"),
            "{body}"
        );
    }

    #[test]
    fn an_only_key_plugged_in_is_all_of_them() {
        let only = vec![("Main key".to_owned(), true)];
        assert!(matches!(erase_note("Alice", &only), EraseNote::All { .. }));
    }

    #[test]
    fn with_no_key_plugged_in_there_is_nothing_to_erase() {
        assert!(matches!(
            erase_note("Alice", &keys(&[false, false])),
            EraseNote::None(_)
        ));
    }

    #[test]
    fn keys_that_will_not_say_get_a_warning() {
        assert_eq!(
            unknown_note("Alice"),
            "Couldn’t tell which of Alice’s keys are plugged in. If they’re all of them, erasing \
             locks Alice for good."
        );
    }

    #[test]
    fn a_key_added_beside_two_named_ones_is_the_backup() {
        assert_eq!(added_key_name(&alice()), "Backup key");
    }

    #[test]
    fn adding_asks_the_new_key_then_any_old_one() {
        let [first, second] = add_key_steps(&alice());
        assert_eq!(
            (first.caption.as_str(), second.title.as_str()),
            ("Step 1 of 2", "Now touch Keyring key or Drawer key")
        );
    }

    /// The page keeps its view: once the first load lands, the start rows
    /// show, though `page` has long returned.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_tab_fills_in_after_it_is_made() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client: Arc<dyn spoolframe_linux_core::backend::Client> =
            Arc::new(spoolframe_linux_core::fake::FakeClient::new(None));
        let page = page(client, &adw::ToastOverlay::new());
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(2),
            || {
                crate::ui_test::row_titled(page.upcast_ref(), "Create with a security key")
                    .is_some()
            }
        ));
    }

    #[test]
    fn the_record_file_is_named_after_the_identity() {
        assert_eq!(record_file_name("Alice"), "alice.spoolframe-identity");
    }

    #[test]
    fn a_name_no_file_can_hold_still_gives_a_file() {
        assert_eq!(record_file_name("/../"), "identity.spoolframe-identity");
    }

    #[test]
    fn the_renew_banner_names_the_day() {
        assert!(
            renew_line(1_791_000_000)
                .starts_with("This computer’s sign-in with your identity ends on ")
        );
    }
}
