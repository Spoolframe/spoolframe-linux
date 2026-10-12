//! A server's page › Your identity here, as on LinuxSettingsServer: whether
//! the account is tied to the user's portable identity (P7-04), with Tie…
//! or Sever…, or that the profile is a guest here with it (P7-06).
//!
//! Tying needs a sign-in in the last 10 minutes, so the server may ask
//! "Confirm it's you" first ([`crate::confirm`]). The tie is proved with
//! this computer's own sign-in as the identity, never the key itself: the
//! key is touched only when this computer has none, or one past its time,
//! to pair it again first.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{BackendError, BoxFuture, Session};
use spoolframe_linux_core::identity::{Identities, IdentityView};
use spoolframe_protocol::portable::PortableUserId;

use crate::dialog::{closes, column, dim};
use crate::key_prompt::{self, Action};

/// What the group needs.
pub struct Needs {
    pub session: Arc<dyn Session>,
    pub identities: Identities,
    pub server_name: String,
    pub username: String,
    /// Opens This computer › Your identity, to make one.
    pub open_identity: Rc<dyn Fn()>,
}

/// Where the profile stands with the user's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Signed in as the identity itself: no account here.
    Guest(PortableUserId),
    /// The account opens for the identity's devices.
    Tied(PortableUserId),
    NotTied,
}

/// A guest has no account to tie, so its tie is never asked.
pub fn standing(guest: Option<PortableUserId>, tie: Option<PortableUserId>) -> Standing {
    match (guest, tie) {
        (Some(id), _) => Standing::Guest(id),
        (None, Some(id)) => Standing::Tied(id),
        (None, None) => Standing::NotTied,
    }
}

/// What Tie… does: make an identity first, or tie the one here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TieWith {
    Create,
    Identity { id: PortableUserId, name: String },
}

/// Tie… with the key identity on this computer, if it has one.
pub fn tie_with(current: Option<&IdentityView>) -> TieWith {
    match current {
        Some(view) => TieWith::Identity {
            id: view.identity.id,
            name: view.identity.name.clone(),
        },
        None => TieWith::Create,
    }
}

/// "R2VN-8KQT-M4XA-H7PD · Devices that sign in with your identity open
/// this account. Your password keeps working."
pub fn tied_line(id: &PortableUserId) -> String {
    format!(
        "{} · Devices that sign in with your identity open this account. Your password keeps \
         working.",
        id.fingerprint()
    )
}

/// "R2VN-8KQT-M4XA-H7PD · No account or password here: …"
pub fn guest_line(id: &PortableUserId) -> String {
    format!(
        "{} · No account or password here: your identity is how you get in.",
        id.fingerprint()
    )
}

/// The key prompt's line: "Touch your key to tie Alice to alice on Home
/// Media".
pub fn touch_line(name: &str, username: &str, server: &str) -> String {
    format!("Touch your key to tie {name} to {username} on {server}")
}

/// What the Tied dialog says.
pub fn tied_body(username: &str, server: &str) -> String {
    format!(
        "Your identity now opens {username} on {server}. Your devices with it sign in here as \
         you, with your history."
    )
}

/// "Your identity here", for a server's page; it fills itself in once the
/// server and this computer have said.
pub fn group(needs: Needs, toasts: &adw::ToastOverlay) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Your identity here")
        .description("Your identity is in This computer › Your identity.")
        .build();
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title("Your identity")
        .subtitle("…")
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(crate::icons::IDENTITY));
    let button = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .css_classes(["row-button"])
        .build();
    row.add_suffix(&button);
    group.add(&row);
    let view = Rc::new(View {
        needs,
        toasts: toasts.downgrade(),
        row,
        button,
        standing: std::cell::Cell::new(Standing::NotTied),
        tie_with: RefCell::new(TieWith::Create),
    });
    {
        let weak = Rc::downgrade(&view);
        view.button.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.pressed();
            }
        });
    }
    view.load();
    // The group owns its view, and goes with the page.
    let held = RefCell::new(Some(view));
    group.connect_destroy(move |_| {
        held.borrow_mut().take();
    });
    group
}

struct View {
    needs: Needs,
    /// Settings' toasts, and the parent its dialogs show over.
    toasts: glib::WeakRef<adw::ToastOverlay>,
    row: adw::ActionRow,
    button: gtk::Button,
    standing: std::cell::Cell<Standing>,
    tie_with: RefCell<TieWith>,
}

impl View {
    fn toast(&self, text: &str) {
        if let Some(toasts) = self.toasts.upgrade() {
            toasts.add_toast(crate::markup::toast(text));
        }
    }

    /// Runs `call` on the session off the main loop, then `then` with its
    /// answer back on it.
    fn run<T: Send + 'static>(
        self: &Rc<Self>,
        call: impl FnOnce(Arc<dyn Session>) -> BoxFuture<'static, T>,
        then: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        let handle = relm4::spawn(call(self.needs.session.clone()));
        let this = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = handle.await else { return };
            if let Some(this) = this.upgrade() {
                then(&this, answer);
            }
        });
    }

    /// Asks the server for the tie, and this computer for its identity.
    fn load(self: &Rc<Self>) {
        if let Some(id) = self.needs.session.guest_identity() {
            return self.show(Standing::Guest(id));
        }
        let (identities, weak) = (self.needs.identities.clone(), Rc::downgrade(self));
        glib::spawn_future_local(async move {
            let current = identities
                .run(|ids| Box::pin(async move { ids.current().await }))
                .await;
            let Some(view) = weak.upgrade() else { return };
            match current {
                Ok(current) => *view.tie_with.borrow_mut() = tie_with(current.as_ref()),
                Err(e) => tracing::warn!(error = %e, "cannot read this computer's identity"),
            }
            view.run(
                |s| Box::pin(async move { s.tie_info().await }),
                |view, tie| match tie {
                    Ok(tie) => view.show(standing(None, tie)),
                    Err(e) => view.row.set_subtitle(&crate::confirm::refusal(&e)),
                },
            );
        });
    }

    fn show(&self, standing: Standing) {
        self.standing.set(standing);
        self.button.remove_css_class("destructive-action");
        match standing {
            Standing::Guest(id) => {
                self.row.set_title("A guest here with your identity");
                self.row.set_subtitle(&guest_line(&id));
                self.button.set_visible(false);
            }
            Standing::Tied(id) => {
                self.row.set_title("Tied to your identity");
                self.row.set_subtitle(&tied_line(&id));
                self.button.set_label("Sever…");
                self.button.add_css_class("destructive-action");
                self.button.set_visible(true);
            }
            Standing::NotTied => {
                self.row.set_title("Not tied to your identity");
                self.row.set_subtitle(
                    "Tie it, and devices that sign in with your identity open this account.",
                );
                if *self.tie_with.borrow() == TieWith::Create {
                    self.button.set_label("Create your identity…");
                } else {
                    self.button.set_label("Tie…");
                }
                self.button.set_visible(true);
            }
        }
    }

    fn pressed(self: &Rc<Self>) {
        match self.standing.get() {
            Standing::Guest(_) => {}
            Standing::Tied(_) => self.ask_sever(),
            Standing::NotTied => {
                let with = self.tie_with.borrow().clone();
                match with {
                    TieWith::Create => (self.needs.open_identity)(),
                    TieWith::Identity { id, name } => self.tie(id, name, false),
                }
            }
        }
    }

    /// "Confirm it's you" on this session, then `again`.
    fn confirm(self: &Rc<Self>, title: &str, doing: &str, again: impl Fn(&Rc<Self>) + 'static) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        crate::confirm::ask_because(
            &parent,
            self.needs.session.clone(),
            &self.needs.server_name,
            title,
            &format!("To {doing}, confirm it’s you."),
            Rc::new(move || {
                if let Some(view) = weak.upgrade() {
                    again(&view);
                }
            }),
        );
    }

    /// Ties the account to identity `id`. The server asks for a recent
    /// sign-in first; then this computer proves the identity with its own
    /// sign-in as it, or, with none, `touched` once for the key to pair it.
    fn tie(self: &Rc<Self>, id: PortableUserId, name: String, touched: bool) {
        self.button.set_sensitive(false);
        self.run(
            move |s| Box::pin(async move { s.tie(id).await }),
            move |view, tied| {
                view.button.set_sensitive(true);
                match tied {
                    Ok(()) => {
                        view.tied_dialog();
                        view.load();
                    }
                    Err(BackendError::RecentSignInRequired) => {
                        view.confirm("Tie your identity", "tie your identity", move |view| {
                            view.tie(id, name.clone(), touched);
                        })
                    }
                    Err(BackendError::NotPaired) if !touched => view.touch(id, name),
                    Err(e) => view.toast(&crate::confirm::refusal(&e)),
                }
            },
        );
    }

    /// The key prompt: a touch pairs this computer with the identity
    /// again, then the tie goes on.
    fn touch(self: &Rc<Self>, id: PortableUserId, name: String) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let op = key_prompt::op_on(&self.needs.identities, move |ids, prompt| {
            let key = ids.key(prompt);
            Box::pin(async move { ids.pair_self(id, key.as_ref()).await })
        });
        let action = Action {
            touch: touch_line(&name, &self.needs.username, &self.needs.server_name),
            keys: Some(
                "This computer’s sign-in as your identity is over, so your key renews it first."
                    .into(),
            ),
            identity: Some(name.clone()),
            other_key: None,
            not_found: None,
        };
        let weak = Rc::downgrade(self);
        key_prompt::act(&parent, action, op, move |()| {
            if let Some(view) = weak.upgrade() {
                view.tie(id, name.clone(), true);
            }
        });
    }

    /// "Tied", once the server has it.
    fn tied_dialog(&self) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let dialog = adw::Dialog::builder()
            .title("Tied")
            .content_width(380)
            .build();
        let body = column();
        body.append(&dim(&tied_body(
            &self.needs.username,
            &self.needs.server_name,
        )));
        let done = gtk::Button::builder()
            .label("Done")
            .halign(gtk::Align::End)
            .css_classes(["suggested-action"])
            .build();
        body.append(&done);
        closes(&done, &dialog);
        crate::start::with_cancel(&dialog, &body);
        dialog.present(Some(&parent));
    }

    /// Sever…: asks first, then severs.
    fn ask_sever(self: &Rc<Self>) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let alert = adw::AlertDialog::builder()
            .heading("Sever the tie?")
            .body(format!(
                "Devices that sign in with your identity stop opening {} on {}. The account, its \
                 password and its history stay.",
                self.needs.username, self.needs.server_name
            ))
            .default_response("cancel")
            .close_response("cancel")
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("sever", "Sever");
        alert.set_response_appearance("sever", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        alert.connect_response(Some("sever"), move |_, _| {
            if let Some(view) = weak.upgrade() {
                view.sever();
            }
        });
        alert.present(Some(&parent));
    }

    fn sever(self: &Rc<Self>) {
        self.button.set_sensitive(false);
        self.run(
            |s| Box::pin(async move { s.untie().await }),
            |view, severed| {
                view.button.set_sensitive(true);
                match severed {
                    Ok(()) => {
                        view.toast("Severed. Your password still opens this account.");
                        view.load();
                    }
                    Err(BackendError::RecentSignInRequired) => {
                        view.confirm("Sever the tie", "sever the tie", |view| view.sever())
                    }
                    Err(e) => view.toast(&crate::confirm::refusal(&e)),
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> PortableUserId {
        PortableUserId::from_bytes([byte; 32]).unwrap()
    }

    #[test]
    fn a_guest_is_shown_as_one_whatever_else_is_known() {
        assert_eq!(standing(Some(id(7)), Some(id(8))), Standing::Guest(id(7)));
    }

    #[test]
    fn an_account_with_a_tie_is_tied_to_that_identity() {
        assert_eq!(standing(None, Some(id(8))), Standing::Tied(id(8)));
    }

    #[test]
    fn with_no_identity_here_tie_makes_one_first() {
        assert_eq!(tie_with(None), TieWith::Create);
    }

    #[test]
    fn the_tied_line_leads_with_the_identitys_fingerprint() {
        assert!(tied_line(&id(7)).starts_with(&format!("{} · ", id(7).fingerprint())));
    }

    #[test]
    fn the_guest_line_leads_with_the_identitys_fingerprint() {
        assert!(guest_line(&id(7)).starts_with(&format!("{} · ", id(7).fingerprint())));
    }

    /// `username`'s group on the fake, which owns the view, and its button
    /// once the row titled `title` shows; `opened` counts Create your
    /// identity….
    fn shown(
        client: &spoolframe_linux_core::fake::FakeClient,
        username: &str,
        title: &str,
        opened: Rc<std::cell::Cell<u32>>,
    ) -> (adw::PreferencesGroup, gtk::Button) {
        use spoolframe_linux_core::backend::Client;
        use spoolframe_linux_core::fake::FakeClient;
        let group = group(
            Needs {
                session: client.session(&FakeClient::profile(username)).unwrap(),
                identities: client.identities(),
                server_name: "Home Media".into(),
                username: username.into(),
                open_identity: Rc::new(move || opened.set(opened.get() + 1)),
            },
            &adw::ToastOverlay::new(),
        );
        let found = || crate::ui_test::row_titled(group.upcast_ref(), title);
        assert!(
            crate::ui_test::spin_until(std::time::Duration::from_secs(5), || found().is_some()),
            "no row titled {title}"
        );
        let mut widgets = vec![found().unwrap().upcast::<gtk::Widget>()];
        while let Some(widget) = widgets.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Button>()
                && button.has_css_class("row-button")
            {
                return (group, button.clone());
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                widgets.push(c);
            }
        }
        panic!("the row has no button");
    }

    /// One test: GTK binds itself to the first thread that starts it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn identity_here_on_the_fake_server() {
        gtk::init().unwrap();
        adw::init().unwrap();
        with_no_identity_here_the_button_opens_your_identity();
        a_tied_account_offers_to_sever();
    }

    fn with_no_identity_here_the_button_opens_your_identity() {
        let client = spoolframe_linux_core::fake::FakeClient::new(None);
        let opened = Rc::new(std::cell::Cell::new(0));
        let (_group, button) = shown(
            &client,
            "andrey",
            "Not tied to your identity",
            opened.clone(),
        );
        button.emit_clicked();
        assert_eq!(opened.get(), 1);
    }

    fn a_tied_account_offers_to_sever() {
        use spoolframe_linux_core::backend::Client;
        use spoolframe_linux_core::fake::FakeClient;
        let client = FakeClient::new(None);
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        spoolframe_linux_core::testing::ready(session.tie(id(7))).unwrap();
        let (_group, button) = shown(&client, "andrey", "Tied to your identity", Rc::default());
        assert_eq!(button.label().as_deref(), Some("Sever…"));
    }

    #[test]
    fn the_touch_names_the_identity_the_account_and_the_server() {
        assert_eq!(
            touch_line("Alice", "alice", "Home Media"),
            "Touch your key to tie Alice to alice on Home Media"
        );
    }
}
