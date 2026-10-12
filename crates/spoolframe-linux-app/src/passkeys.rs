//! Preferences › Profile › Passkeys (P3-19), as the design's
//! LinuxPrefsProfile and LinuxPasskeyManage boards have them: the user's
//! passkeys, the web UI's too, with Rename and Remove, and Add a passkey…
//! on a security key in three steps.
//!
//! Adding or removing one needs a sign-in in the last 10 minutes. When
//! the server says it is older, "Confirm it's you" signs in again, with
//! the password or a passkey, and the action goes on with the new session.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{
    BackendError, FinishPasskeyRegistration, PasskeyInfo, Server, Session,
};

use crate::passkey_dialog::{self, Ceremony, Done, KeyPage};
use spoolframe_linux_core::passkey::Via;

/// What the group needs: who, where, and how to sign in again.
pub struct Needs {
    pub server: Server,
    pub session: Arc<dyn Session>,
}

/// The name a passkey made here gets until the user names it.
const FIRST_NAME: &str = "Security key";

/// "Made on the web · created 20 Aug · last used 3 weeks ago".
pub fn passkey_line(passkey: &PasskeyInfo, created: &str, now: i64) -> String {
    let kind = match (passkey.app, passkey.usable_here) {
        (true, true) => "Spoolframe app".to_owned(),
        // Made for spoolframe.app before each server had its own relying
        // party (PX-14): it signs in nowhere now.
        (true, false) => "Old: add again".to_owned(),
        (false, _) => format!("Made on the web at {}", passkey.rp_id),
    };
    let used = match passkey.last_used_at {
        Some(at) => format!("last used {}", ago(at, now)),
        None => "never used".to_owned(),
    };
    format!("{kind} · created {created} · {used}")
}

/// "today", "yesterday", "3 days ago", "3 weeks ago", "4 months ago".
pub fn ago(at: i64, now: i64) -> String {
    match (now - at).max(0) / 86_400 {
        0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        days @ 2..=13 => format!("{days} days ago"),
        days @ 14..=59 => format!("{} weeks ago", days / 7),
        days => format!("{} months ago", days / 30),
    }
}

/// "12 Sep", in the local time zone.
fn day(at: i64) -> String {
    glib::DateTime::from_unix_local(at)
        .and_then(|d| d.format("%-d %b"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The Passkeys group, first on the Profile page.
pub fn group(needs: Needs, toasts: &adw::ToastOverlay) -> adw::PreferencesGroup {
    let add = gtk::Button::builder()
        .label("Add a passkey…")
        .valign(gtk::Align::Center)
        .css_classes(["row-button"])
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Passkeys")
        .description("Sign in with a security key or your phone instead of a password.")
        .header_suffix(&add)
        .build();
    let view = Rc::new(View {
        server: needs.server,
        session: RefCell::new(needs.session),
        toasts: toasts.downgrade(),
        group: group.clone(),
        rows: RefCell::new(Vec::new()),
    });
    {
        let weak = Rc::downgrade(&view);
        add.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.add();
            }
        });
    }
    view.load();
    group.connect_destroy(move |_| {
        let _ = &view;
    });
    group
}

struct View {
    server: Server,
    /// Replaced when "Confirm it's you" signs in again.
    session: RefCell<Arc<dyn Session>>,
    toasts: glib::WeakRef<adw::ToastOverlay>,
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
}

/// A session call to make again once the user has confirmed it's them.
type Again = Rc<dyn Fn(&Rc<View>)>;

impl View {
    fn toast(&self, text: &str) {
        if let Some(toasts) = self.toasts.upgrade() {
            toasts.add_toast(crate::markup::toast(text));
        }
    }

    fn parent(&self) -> Option<gtk::Widget> {
        self.toasts.upgrade().map(|t| t.upcast())
    }

    /// Runs `call` on the session off the main loop, then `then` on it.
    fn run<T: Send + 'static>(
        self: &Rc<Self>,
        call: impl FnOnce(Arc<dyn Session>) -> spoolframe_linux_core::backend::BoxFuture<'static, T>,
        then: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        let handle = relm4::spawn(call(self.session.borrow().clone()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = handle.await else { return };
            if let Some(view) = weak.upgrade() {
                then(&view, answer);
            }
        });
    }

    fn load(self: &Rc<Self>) {
        self.run(
            |s| Box::pin(async move { s.passkeys().await }),
            |view, listed| match listed {
                Ok(passkeys) => view.show(&passkeys),
                Err(e) => view.toast(&e.to_string()),
            },
        );
    }

    fn show(self: &Rc<Self>, passkeys: &[PasskeyInfo]) {
        for row in self.rows.take() {
            self.group.remove(&row);
        }
        let now = crate::invite::unix_now();
        let mut rows = Vec::new();
        if passkeys.is_empty() {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title("No passkeys yet")
                .css_classes(["dim-label"])
                .build();
            self.group.add(&row);
            rows.push(row);
        }
        for passkey in passkeys {
            let row = adw::ActionRow::builder().use_markup(false).build();
            // Titles after the builder: it sets them before use_markup.
            row.set_title(&passkey.name);
            row.set_subtitle(&passkey_line(passkey, &day(passkey.created_at), now));
            row.add_prefix(&gtk::Image::from_icon_name(if passkey.app {
                crate::icons::PASSKEY
            } else {
                crate::icons::BROWSER
            }));
            let rename = gtk::Button::builder()
                .label("Rename")
                .valign(gtk::Align::Center)
                .css_classes(["row-button"])
                .build();
            let remove = gtk::Button::builder()
                .label("Remove")
                .valign(gtk::Align::Center)
                .css_classes(["row-button", "destructive-tint"])
                .build();
            {
                let (weak, to_rename) = (Rc::downgrade(self), passkey.clone());
                rename.connect_clicked(move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.rename(&to_rename);
                    }
                });
                let (weak, to_remove) = (Rc::downgrade(self), passkey.clone());
                remove.connect_clicked(move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.remove(&to_remove);
                    }
                });
            }
            row.add_suffix(&rename);
            row.add_suffix(&remove);
            self.group.add(&row);
            rows.push(row);
        }
        self.rows.replace(rows);
    }

    /// A small dialog: a heading, a line, maybe a field, and its button.
    fn ask(
        &self,
        title: &str,
        heading: &str,
        line: &str,
        field: Option<&gtk::Entry>,
        button: (&str, &str),
        go: Rc<dyn Fn(&adw::Dialog)>,
    ) {
        let Some(parent) = self.parent() else { return };
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(380)
            .build();
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .margin_top(24)
            .margin_bottom(26)
            .margin_start(26)
            .margin_end(26)
            .build();
        if !heading.is_empty() {
            body.append(
                &gtk::Label::builder()
                    .label(heading)
                    .wrap(true)
                    .max_width_chars(1)
                    .justify(gtk::Justification::Center)
                    .css_classes(["title-3"])
                    .build(),
            );
        }
        if !line.is_empty() {
            body.append(
                &gtk::Label::builder()
                    .label(line)
                    .wrap(true)
                    .max_width_chars(1)
                    .justify(gtk::Justification::Center)
                    .css_classes(["dim-label", "small-text"])
                    .build(),
            );
        }
        if let Some(field) = field {
            body.append(field);
        }
        let action = gtk::Button::builder()
            .label(button.0)
            .css_classes(["action", button.1])
            .build();
        body.append(&action);
        crate::start::with_cancel(&dialog, &body);
        {
            let dialog = dialog.downgrade();
            let go_ = go.clone();
            action.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    go_(&dialog);
                }
            });
            if let Some(field) = field {
                let dialog = action.clone();
                field.connect_activate(move |_| dialog.emit_clicked());
            }
        }
        dialog.present(Some(&parent));
    }

    fn rename(self: &Rc<Self>, passkey: &PasskeyInfo) {
        let field = gtk::Entry::builder()
            .text(&passkey.name)
            .css_classes(["field"])
            .build();
        let (weak, id, field_) = (Rc::downgrade(self), passkey.id.clone(), field.clone());
        self.ask(
            "Rename passkey",
            "",
            "",
            Some(&field),
            ("Save", "suggested-action"),
            Rc::new(move |dialog| {
                let Some(view) = weak.upgrade() else { return };
                let (id, name) = (id.clone(), field_.text().to_string());
                dialog.close();
                view.run(
                    move |s| Box::pin(async move { s.rename_passkey(&id, &name).await }),
                    |view, renamed| match renamed {
                        Ok(()) => view.load(),
                        Err(e) => view.toast(&e.to_string()),
                    },
                );
            }),
        );
    }

    fn remove(self: &Rc<Self>, passkey: &PasskeyInfo) {
        let line = if passkey.app {
            format!(
                "It stops working for {}. The passkey stays on the key until you delete it \
                 there.",
                self.server.name
            )
        } else {
            format!(
                "It stops working for {}. The passkey stays where it was saved until you \
                 delete it there.",
                self.server.name
            )
        };
        let (weak, id) = (Rc::downgrade(self), passkey.id.clone());
        self.ask(
            "Remove passkey",
            &format!("Remove “{}”?", passkey.name),
            &line,
            None,
            ("Remove", "destructive-action"),
            Rc::new(move |dialog| {
                dialog.close();
                if let Some(view) = weak.upgrade() {
                    view.remove_now(id.clone());
                }
            }),
        );
    }

    fn remove_now(self: &Rc<Self>, id: String) {
        let again_id = id.clone();
        self.run(
            move |s| Box::pin(async move { s.remove_passkey(&id).await }),
            move |view, removed| match removed {
                Ok(()) => {
                    view.toast("Passkey removed");
                    view.load();
                }
                Err(BackendError::RecentSignInRequired) => {
                    let again: Again = Rc::new(move |view| view.remove_now(again_id.clone()));
                    view.confirm("Remove passkey", again);
                }
                Err(BackendError::LastSignInMethod) => view.only_way_in(),
                Err(e) => view.toast(&e.to_string()),
            },
        );
    }

    fn only_way_in(&self) {
        self.ask(
            "Remove passkey",
            "This is your only way in",
            "Without it you couldn’t sign in at all: your account has no password, no other \
             passkey and no sign-in provider. Add another way first.",
            None,
            ("OK", "suggested-action"),
            Rc::new(|dialog| {
                dialog.close();
            }),
        );
    }

    /// "Confirm it's you" on this session, then the call again.
    fn confirm(self: &Rc<Self>, title: &str, again: Again) {
        let Some(parent) = self.parent() else { return };
        let weak = Rc::downgrade(self);
        crate::confirm::ask(
            &parent,
            self.session.borrow().clone(),
            &self.server.name,
            title,
            Rc::new(move || {
                if let Some(view) = weak.upgrade() {
                    again(&view);
                }
            }),
        );
    }

    /// Add a passkey…: which kind, the key, then its name.
    fn add(self: &Rc<Self>) {
        let Some(parent) = self.parent() else { return };
        let adding = Adding::new(self);
        adding.dialog.present(Some(&parent));
    }
}

/// Step 3's note: what a name is for, and where the password manager lists
/// the passkey (PX-14): under the server's site, as "alice @ Home Media".
fn listed_line(creation: &spoolframe_linux_core::backend::PasskeyCreation) -> String {
    format!(
        "A name to tell it apart later, like “YubiKey 5C” or “Alice’s iPhone”. Your password \
         manager lists it under {}, as “{}”.",
        creation.rp_id, creation.user_name
    )
}

/// Why the server would not take a name.
fn rename_refusal(error: &BackendError) -> String {
    match error {
        BackendError::InvalidInput(_) => "A name is 1 to 64 characters.".to_owned(),
        other => crate::confirm::refusal(other),
    }
}

/// The Add a passkey dialog's three steps.
struct Adding {
    dialog: adw::Dialog,
    view: std::rc::Weak<View>,
    stack: gtk::Stack,
    step: gtk::Label,
    back: gtk::Button,
    key: Rc<KeyPage>,
    name: gtk::Entry,
    /// Under the name: why the server would not take it.
    problem: gtk::Label,
    /// Where the password manager lists it, once the server has said.
    listed: gtk::Label,
    /// What the key made, kept to rename it from step 3.
    made: RefCell<Option<FinishPasskeyRegistration>>,
    /// The name the server refused last: closing again with it leaves the
    /// passkey under its first name.
    refused: RefCell<Option<String>>,
    /// A rename is on its way.
    renaming: std::cell::Cell<bool>,
}

impl Adding {
    fn new(view: &Rc<View>) -> Rc<Self> {
        let dialog = adw::Dialog::builder()
            .title("Add a passkey")
            .content_width(360)
            .build();
        let key = KeyPage::new(&view.server.name, None);
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .vhomogeneous(false)
            .build();
        let step = gtk::Label::builder()
            .css_classes(["caption-heading", "dim-label"])
            .margin_top(16)
            .build();
        let back = gtk::Button::builder()
            .label("Cancel")
            .css_classes(["flat", "dialog-cancel"])
            .build();
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .css_classes(["dialog-header"])
            .build();
        header.pack_start(&back);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&step);
        column.append(&stack);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&column));
        dialog.set_child(Some(&toolbar));
        let name = gtk::Entry::builder()
            .text(FIRST_NAME)
            .css_classes(["field"])
            .build();
        let adding = Rc::new(Self {
            dialog,
            view: Rc::downgrade(view),
            stack,
            step,
            back,
            key,
            name,
            problem: gtk::Label::builder()
                .visible(false)
                .wrap(true)
                .max_width_chars(1)
                .xalign(0.0)
                .css_classes(["error", "small-text"])
                .build(),
            listed: gtk::Label::builder()
                .label("A name to tell it apart later, like “YubiKey 5C” or “Alice’s iPhone”.")
                .wrap(true)
                .max_width_chars(1)
                .css_classes(["dim-label", "key-body"])
                .build(),
            made: RefCell::new(None),
            refused: RefCell::new(None),
            renaming: std::cell::Cell::new(false),
        });
        adding
            .stack
            .add_named(&adding.choose_page(), Some("choose"));
        adding.stack.add_named(&adding.key.widget, Some("key"));
        adding.stack.add_named(&adding.name_page(), Some("name"));
        adding.go("choose");
        {
            let weak = Rc::downgrade(&adding);
            adding.back.connect_clicked(move |_| {
                if let Some(adding) = weak.upgrade() {
                    if adding.stack.visible_child_name().as_deref() == Some("key") {
                        adding.key.stop();
                        adding.go("choose");
                    } else {
                        adding.dialog.close();
                    }
                }
            });
            let weak = Rc::downgrade(&adding);
            adding.key.set_retry(Rc::new(move || {
                if let Some(adding) = weak.upgrade() {
                    adding.with(adding.key.via());
                }
            }));
            let weak = Rc::downgrade(&adding);
            adding.key.set_alternative(Rc::new(move || {
                if let Some(adding) = weak.upgrade() {
                    adding.with(Via::Key);
                }
            }));
            // At step 3 the passkey is saved, and closing any way (Escape,
            // the window) names it as Done does.
            let weak = Rc::downgrade(&adding);
            adding.dialog.connect_close_attempt(move |_| {
                if let Some(adding) = weak.upgrade() {
                    adding.finish_name();
                }
            });
            let weak = Rc::downgrade(&adding);
            adding.name.connect_changed(move |_| {
                if let Some(adding) = weak.upgrade() {
                    adding.problem.set_visible(false);
                }
            });
            let weak = Rc::downgrade(&adding);
            adding.dialog.connect_closed(move |_| {
                if let Some(adding) = weak.upgrade() {
                    adding.key.stop();
                    // Saved and named or not, the list shows it.
                    if let Some(view) = adding.view.upgrade() {
                        view.load();
                    }
                }
            });
            // The dialog owns its steps.
            let held = adding.clone();
            adding.dialog.connect_closed(move |_| {
                let _ = &held;
            });
        }
        adding
    }

    fn go(&self, page: &str) {
        let (step, back) = match page {
            "choose" => ("STEP 1 OF 3", "Cancel"),
            "key" => ("STEP 2 OF 3", "Back"),
            _ => ("STEP 3 OF 3", ""),
        };
        self.step.set_label(step);
        self.dialog.set_can_close(page != "name");
        self.back.set_label(back);
        self.back.set_visible(!back.is_empty());
        self.stack.set_visible_child_name(page);
    }

    fn choose_page(self: &Rc<Self>) -> gtk::Box {
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(16)
            .margin_bottom(26)
            .margin_start(26)
            .margin_end(26)
            .build();
        let key = passkey_dialog::option(
            crate::icons::SECURITY_KEY,
            "Security key",
            "Saves the passkey on the key",
        );
        {
            let weak = Rc::downgrade(self);
            key.connect_clicked(move |_| {
                if let Some(adding) = weak.upgrade() {
                    adding.with(Via::Key);
                }
            });
        }
        page.append(&key);
        let phone = passkey_dialog::option(
            crate::icons::PHONE,
            "Phone or tablet",
            "Saves it on your phone: scan a code",
        );
        {
            let weak = Rc::downgrade(self);
            phone.connect_clicked(move |_| {
                if let Some(adding) = weak.upgrade() {
                    adding.with(Via::Phone);
                }
            });
        }
        page.append(&phone);
        page
    }

    fn name_page(self: &Rc<Self>) -> gtk::Box {
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .margin_top(16)
            .margin_bottom(26)
            .margin_start(26)
            .margin_end(26)
            .build();
        page.append(
            &gtk::Image::builder()
                .icon_name(crate::icons::CHECK)
                .pixel_size(32)
                .css_classes(["accent"])
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label("Passkey saved")
                .css_classes(["key-title"])
                .build(),
        );
        let field = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        field.append(
            &gtk::Label::builder()
                .label("Name")
                .xalign(0.0)
                .css_classes(["field-label"])
                .build(),
        );
        field.append(&self.name);
        field.append(&self.problem);
        page.append(&field);
        page.append(&self.listed);
        let done = gtk::Button::builder()
            .label("Done")
            .css_classes(["action", "suggested-action"])
            .build();
        {
            let weak = Rc::downgrade(self);
            done.connect_clicked(move |_| {
                if let Some(adding) = weak.upgrade() {
                    adding.finish_name();
                }
            });
            let done_ = done.clone();
            self.name.connect_activate(move |_| done_.emit_clicked());
        }
        page.append(&done);
        page
    }

    /// Asks the server to begin, then the key or phone to make the
    /// passkey.
    fn with(self: &Rc<Self>, via: Via) {
        let Some(view) = self.view.upgrade() else {
            return;
        };
        self.go("key");
        self.key
            .show(passkey_dialog::KeyState::Working("Asking the server…"));
        let weak = Rc::downgrade(self);
        view.run(
            |s| Box::pin(async move { s.begin_passkey_registration().await }),
            move |view, begun| {
                let Some(adding) = weak.upgrade() else { return };
                match begun {
                    Ok(creation) => {
                        adding.listed.set_label(&listed_line(&creation));
                        let done: Rc<dyn Fn(Done)> = {
                            let weak = Rc::downgrade(&adding);
                            Rc::new(move |done| {
                                if let (Some(adding), Done::Made(made)) = (weak.upgrade(), done) {
                                    adding.save(made);
                                }
                            })
                        };
                        adding.key.run(
                            Ceremony::Register(creation, FIRST_NAME.to_owned()),
                            via,
                            done,
                        );
                    }
                    Err(BackendError::RecentSignInRequired) => {
                        let weak = Rc::downgrade(&adding);
                        let again: Again = Rc::new(move |_| {
                            if let Some(adding) = weak.upgrade() {
                                adding.with(via);
                            }
                        });
                        adding.go("choose");
                        view.confirm("Add a passkey", again);
                    }
                    Err(e) => adding
                        .key
                        .show(passkey_dialog::KeyState::Refused(e.to_string())),
                }
            },
        );
    }

    /// Keeps what the key made, under its first name.
    fn save(self: &Rc<Self>, made: FinishPasskeyRegistration) {
        let Some(view) = self.view.upgrade() else {
            return;
        };
        self.made.replace(Some(made.clone()));
        let weak = Rc::downgrade(self);
        view.run(
            move |s| Box::pin(async move { s.finish_passkey_registration(made).await }),
            move |_, saved| {
                let Some(adding) = weak.upgrade() else { return };
                match saved {
                    Ok(()) => {
                        adding.go("name");
                        adding.name.grab_focus();
                    }
                    Err(e) => adding
                        .key
                        .show(passkey_dialog::KeyState::Refused(e.to_string())),
                }
            },
        );
    }

    /// Renames it if the user changed its name, then closes. A refusal is
    /// said under the name and the dialog stays; closing again with the
    /// same name keeps the first one.
    fn finish_name(self: &Rc<Self>) {
        if self.renaming.get() {
            return;
        }
        let name = self.name.text().trim().to_owned();
        let made = self.made.borrow().clone();
        let (Some(view), Some(made)) = (self.view.upgrade(), made) else {
            self.dialog.force_close();
            return;
        };
        if name.is_empty()
            || name == FIRST_NAME
            || self.refused.borrow().as_deref() == Some(name.as_str())
        {
            self.dialog.force_close();
            return;
        }
        self.renaming.set(true);
        let id = spoolframe_linux_core::passkey::passkey_id(&made.credential_id);
        let weak = Rc::downgrade(self);
        let asked = name.clone();
        view.run(
            move |s| Box::pin(async move { s.rename_passkey(&id, &name).await }),
            move |_, renamed| {
                let Some(adding) = weak.upgrade() else { return };
                adding.renaming.set(false);
                match renamed {
                    Ok(()) => adding.dialog.force_close(),
                    Err(e) => {
                        adding.problem.set_label(&format!(
                            "{} Close again to keep it as “{FIRST_NAME}”.",
                            rename_refusal(&e)
                        ));
                        adding.problem.set_visible(true);
                        adding.refused.replace(Some(asked));
                    }
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passkey(app: bool, last_used_at: Option<i64>) -> PasskeyInfo {
        PasskeyInfo {
            id: "AQID".into(),
            name: "YubiKey 5C".into(),
            created_at: 0,
            last_used_at,
            rp_id: if app {
                "homemediaserver2.spoolframe.app".into()
            } else {
                "media.example.org".into()
            },
            app,
            usable_here: app,
        }
    }

    #[test]
    fn a_web_passkey_says_where_it_was_made_and_when_it_was_used() {
        let day = 86_400;
        assert_eq!(
            passkey_line(&passkey(false, Some(0)), "20 Aug", 21 * day),
            "Made on the web at media.example.org · created 20 Aug · last used 3 weeks ago"
        );
    }

    #[test]
    fn step_three_says_where_the_password_manager_lists_it() {
        let creation = spoolframe_linux_core::backend::PasskeyCreation {
            rp_id: "k7f3q2m4x5pq6z2a.spoolframe.app".into(),
            origin: "https://k7f3q2m4x5pq6z2a.spoolframe.app".into(),
            challenge: Vec::new(),
            user_id: Vec::new(),
            user_name: "alice @ Home Media".into(),
            display_name: "Alice".into(),
            exclude_credential_ids: Vec::new(),
        };
        assert!(
            listed_line(&creation)
                .ends_with("under k7f3q2m4x5pq6z2a.spoolframe.app, as “alice @ Home Media”.")
        );
    }

    #[test]
    fn an_app_passkey_from_before_each_server_had_its_own_says_to_add_it_again() {
        let old = PasskeyInfo {
            rp_id: "spoolframe.app".into(),
            usable_here: false,
            ..passkey(true, None)
        };
        assert!(passkey_line(&old, "2 Oct", 0).starts_with("Old: add again"));
    }

    fn run_until(limit: std::time::Duration, done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + limit;
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        done()
    }

    /// One test: GTK binds itself to the first thread that starts it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn passkeys_on_the_fake_server() {
        gtk::init().unwrap();
        adw::init().unwrap();
        a_web_ui_passkey_is_listed();
        a_name_typed_at_step_three_sticks_however_the_dialog_closes();
        a_refused_name_is_said_in_the_dialog();
    }

    /// A view of the fake's Home Media for andrey, and his session.
    fn fake_view() -> (Rc<View>, Arc<dyn Session>) {
        use spoolframe_linux_core::backend::Client;
        use spoolframe_linux_core::fake::FakeClient;
        let client: Arc<dyn Client> = Arc::new(FakeClient::new(None));
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let view = Rc::new(View {
            server: Server {
                key: profile.server.clone(),
                name: "Home Media".into(),
                addresses: Vec::new(),
                pinned: true,
                fingerprint: String::new(),
                verified: true,
                on_network: true,
                verified_at: None,
                last_connected_at: None,
                server_id: None,
                advertised: Vec::new(),
                relays: Vec::new(),
                advertised_at: None,
                use_relays: None,
                relays_edited: false,
            },
            session: RefCell::new(session.clone()),
            toasts: adw::ToastOverlay::new().downgrade(),
            group: adw::PreferencesGroup::new(),
            rows: RefCell::new(Vec::new()),
        });
        (view, session)
    }

    /// Add a passkey…, shown in a window, with the key done: step 3.
    fn at_step_three(view: &Rc<View>) -> Rc<Adding> {
        let window = adw::Window::builder()
            .default_width(600)
            .default_height(600)
            .build();
        window.present();
        let adding = Adding::new(view);
        adding.dialog.present(Some(&window));
        adding.save(FinishPasskeyRegistration {
            credential_id: vec![7; 16],
            attestation_object: vec![1],
            client_data_json: vec![1],
            name: FIRST_NAME.to_owned(),
        });
        assert!(
            run_until(std::time::Duration::from_secs(5), || adding
                .stack
                .visible_child_name()
                .as_deref()
                == Some("name")),
            "the passkey was not saved"
        );
        adding
    }

    fn names(session: &Arc<dyn Session>) -> Vec<String> {
        spoolframe_linux_core::testing::ready(session.passkeys())
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    fn a_name_typed_at_step_three_sticks_however_the_dialog_closes() {
        let (view, session) = fake_view();
        let adding = at_step_three(&view);
        adding.name.set_text("thumb");
        // Escape, or the window closing: not Done.
        adding.dialog.close();
        assert!(
            run_until(std::time::Duration::from_secs(5), || names(&session)
                .contains(&"thumb".to_owned())),
            "names: {:?}",
            names(&session)
        );
    }

    fn a_refused_name_is_said_in_the_dialog() {
        let (view, _) = fake_view();
        let adding = at_step_three(&view);
        // Longer than the 64 characters a name may have.
        adding.name.set_text(&"t".repeat(65));
        adding.name.emit_activate();
        assert!(
            run_until(std::time::Duration::from_secs(5), || adding
                .problem
                .is_visible()),
            "the refusal was not said in the dialog"
        );
        assert!(adding.dialog.is_mapped(), "the dialog closed on a refusal");
        adding.dialog.force_close();
    }

    fn a_web_ui_passkey_is_listed() {
        use spoolframe_linux_core::backend::Client;
        use spoolframe_linux_core::fake::FakeClient;
        let client: Arc<dyn Client> = Arc::new(FakeClient::new(None));
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        let server = Server {
            key: profile.server.clone(),
            name: "Home Media".into(),
            addresses: Vec::new(),
            pinned: true,
            fingerprint: String::new(),
            verified: true,
            on_network: true,
            verified_at: None,
            last_connected_at: None,
            server_id: None,
            advertised: Vec::new(),
            relays: Vec::new(),
            advertised_at: None,
            use_relays: None,
            relays_edited: false,
        };
        let toasts = adw::ToastOverlay::new();
        let group = group(Needs { server, session }, &toasts);
        let titles = || {
            let mut titles = Vec::new();
            let mut child = group.first_child();
            while let Some(widget) = child {
                for row in widget_rows(&widget) {
                    titles.push(row.title().to_string());
                }
                child = widget.next_sibling();
            }
            titles
        };
        assert!(
            run_until(std::time::Duration::from_secs(5), || titles()
                .contains(&"Firefox on nimble".to_owned())),
            "rows: {:?}",
            titles()
        );
    }

    /// Every action row under `widget`.
    fn widget_rows(widget: &gtk::Widget) -> Vec<adw::ActionRow> {
        let mut rows = Vec::new();
        if let Some(row) = widget.downcast_ref::<adw::ActionRow>() {
            rows.push(row.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            rows.extend(widget_rows(&c));
            child = c.next_sibling();
        }
        rows
    }

    #[test]
    fn a_passkey_never_used_says_so() {
        assert_eq!(
            passkey_line(&passkey(true, None), "12 Sep", 0),
            "Spoolframe app · created 12 Sep · never used"
        );
    }
}
