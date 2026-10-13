//! A server's page › Your identity here, as on LinuxTies: every portable
//! identity the account is tied to (P7-04, P7-09), oldest first, each with
//! Sever…, and Tie another… for this computer's; or that the profile is a
//! guest here with its identity (P7-06).
//!
//! Tying and severing need a sign-in in the last 10 minutes, so the server
//! may ask "Confirm it's you" first ([`crate::confirm`]); Sever… asks its
//! question before that. The tie is proved with this computer's own
//! sign-in as the identity, never the key itself: the key is touched only
//! when this computer has none, or one past its time, to pair it again
//! first.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{BackendError, BoxFuture, Session, Tie};
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

/// Whether `id` is this computer's identity: its row says "This device".
pub fn this_device(id: &PortableUserId, with: &TieWith) -> bool {
    matches!(with, TieWith::Identity { id: here, .. } if here == id)
}

/// Whether Tie another… shows: not when this computer's identity is tied
/// already, as tying it again would change nothing, and not with no ties,
/// where the empty row's Tie… does it.
pub fn offers_another(ties: &[Tie], with: &TieWith) -> bool {
    !ties.is_empty() && !ties.iter().any(|t| this_device(&t.id, with))
}

/// A tie's subtitle from its days as [`crate::start::day`] gives them:
/// "Tied 12 Oct · last used today", or "Tied 12 Oct · Not used yet".
pub fn tie_line(tied: &str, last_used: Option<&str>) -> String {
    match last_used {
        Some(used) => format!("Tied {tied} · last used {used}"),
        None => format!("Tied {tied} · Not used yet"),
    }
}

/// What Sever… asks, naming the identity by its fingerprint.
pub fn sever_body(fingerprint: &str, this_computer: bool) -> String {
    let body = format!(
        "Devices that sign in with {fingerprint} stop opening this account. The other identities \
         stay tied, and the account keeps everything."
    );
    if this_computer {
        body + " This computer stops signing in with it."
    } else {
        body
    }
}

/// What Tie… asks: "Tie your identity to alice on Home Media?". The
/// identity's name is in the card below, as it is often the username.
pub fn tie_heading(username: &str, server: &str) -> String {
    format!("Tie your identity to {username} on {server}?")
}

/// Under the card: a tie adds to the others (P7-09), and a device proves
/// only its own identity, so another is tied from where it is.
pub fn tie_body(username: &str) -> String {
    format!(
        "Devices that sign in with it will open {username}’s account here, alongside any \
         identity already tied. To tie a different identity, tie it from a device that has it."
    )
}

/// The identity Tie… ties, as a card: its name, its fingerprint, and
/// whose it is.
fn identity_card(name: &str, fingerprint: &str) -> gtk::Box {
    let card = gtk::Box::builder()
        .spacing(12)
        .css_classes(["card", "tie-card"])
        .build();
    card.append(
        &gtk::Image::builder()
            .icon_name(crate::icons::IDENTITY)
            .pixel_size(24)
            .css_classes(["invite-identity"])
            .build(),
    );
    let lines = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    let line = |text: &str, classes: &[&str]| {
        let label = gtk::Label::builder()
            .use_markup(false)
            .xalign(0.0)
            .wrap(true)
            .css_classes(classes.to_vec())
            .build();
        label.set_label(text);
        lines.append(&label);
    };
    line(name, &["heading"]);
    line(fingerprint, &["monospace", "invite-identity"]);
    line("This computer’s identity", &["caption", "dim-label"]);
    card.append(&lines);
    card
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
    let guest = needs.session.guest_identity();
    let group = if guest.is_some() {
        adw::PreferencesGroup::builder()
            .title("Your identity here")
            .description("Your identity is in This computer › Your identity.")
            .build()
    } else {
        adw::PreferencesGroup::builder()
            .title("Your identity here")
            .description(
                "Each identity tied here opens this account. Severing one leaves the others.",
            )
            .build()
    };
    let another = gtk::Button::builder()
        .label("Tie another…")
        .valign(gtk::Align::Center)
        .visible(false)
        .css_classes(["row-button"])
        .build();
    group.set_header_suffix(Some(&another));
    // Below the list, when Tie another… would only tie it again.
    let already = gtk::Label::builder()
        .label("This computer’s identity is already tied.")
        .xalign(0.0)
        .margin_top(12)
        .visible(false)
        .css_classes(["caption", "dim-label"])
        .build();
    group.add(&already);
    let view = Rc::new(View {
        needs,
        toasts: toasts.downgrade(),
        group: group.downgrade(),
        another,
        already,
        rows: RefCell::new(Vec::new()),
        tie_with: RefCell::new(TieWith::Create),
    });
    {
        let weak = Rc::downgrade(&view);
        view.another.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.tie_pressed();
            }
        });
    }
    match guest {
        Some(id) => view.show_guest(&id),
        None => {
            view.replace(vec![view.note_row("Your identity", "…")]);
            view.load();
        }
    }
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
    /// The group, which owns this view.
    group: glib::WeakRef<adw::PreferencesGroup>,
    /// Tie another…, in the group's header.
    another: gtk::Button,
    /// "This computer’s identity is already tied."
    already: gtk::Label,
    /// The rows shown now: the ties, or the one row in their place.
    rows: RefCell<Vec<adw::ActionRow>>,
    tie_with: RefCell<TieWith>,
}

impl View {
    fn toast(&self, text: &str) {
        if let Some(toasts) = self.toasts.upgrade() {
            toasts.add_toast(crate::markup::toast(text));
        }
    }

    /// Runs `call` on the session off the main loop, then `then` with its
    /// answer back on it. The group is greyed out meanwhile, so nothing
    /// is pressed twice.
    fn run<T: Send + 'static>(
        self: &Rc<Self>,
        call: impl FnOnce(Arc<dyn Session>) -> BoxFuture<'static, T>,
        then: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        self.busy(true);
        let handle = relm4::spawn(call(self.needs.session.clone()));
        let this = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let answer = handle.await;
            let Some(this) = this.upgrade() else { return };
            this.busy(false);
            if let Ok(answer) = answer {
                then(&this, answer);
            }
        });
    }

    fn busy(&self, busy: bool) {
        if let Some(group) = self.group.upgrade() {
            group.set_sensitive(!busy);
        }
    }

    /// Puts `rows` in the group in place of the ones there.
    fn replace(&self, rows: Vec<adw::ActionRow>) {
        let Some(group) = self.group.upgrade() else {
            return;
        };
        for row in self.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        for row in &rows {
            group.add(row);
        }
        *self.rows.borrow_mut() = rows;
    }

    /// A row with the identity icon and no button.
    fn note_row(&self, title: &str, subtitle: &str) -> adw::ActionRow {
        let row = adw::ActionRow::builder().use_markup(false).build();
        row.set_title(title);
        row.set_subtitle(subtitle);
        row.add_prefix(&gtk::Image::from_icon_name(crate::icons::IDENTITY));
        row
    }

    fn show_guest(&self, id: &PortableUserId) {
        self.another.set_visible(false);
        self.already.set_visible(false);
        self.replace(vec![
            self.note_row("A guest here with your identity", &guest_line(id)),
        ]);
    }

    /// Asks this computer for its identity, then the server for the ties.
    fn load(self: &Rc<Self>) {
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
                |view, ties| match ties {
                    Ok(ties) => view.show(&ties),
                    Err(e) => view.replace(vec![
                        view.note_row("Your identity", &crate::confirm::refusal(&e)),
                    ]),
                },
            );
        });
    }

    fn show(self: &Rc<Self>, ties: &[Tie]) {
        let with = self.tie_with.borrow().clone();
        self.another.set_visible(offers_another(ties, &with));
        self.already
            .set_visible(ties.iter().any(|t| this_device(&t.id, &with)));
        // With no identity here, the button makes one first.
        if with == TieWith::Create {
            self.another.set_label("Create your identity…");
        } else {
            self.another.set_label("Tie another…");
        }
        if ties.is_empty() {
            let row = self.note_row(
                "Not tied to an identity",
                "Tie one, and devices that sign in with it open this account.",
            );
            let tie = gtk::Button::builder()
                .valign(gtk::Align::Center)
                .css_classes(["row-button"])
                .build();
            if with == TieWith::Create {
                tie.set_label("Create your identity…");
            } else {
                tie.set_label("Tie…");
            }
            let weak = Rc::downgrade(self);
            tie.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.tie_pressed();
                }
            });
            row.add_suffix(&tie);
            return self.replace(vec![row]);
        }
        let rows = ties
            .iter()
            .map(|tie| self.tie_row(tie, this_device(&tie.id, &with)))
            .collect();
        self.replace(rows);
    }

    /// One tie: its fingerprint, when it was tied and last used, and
    /// Sever….
    fn tie_row(self: &Rc<Self>, tie: &Tie, this_computer: bool) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .css_classes(["tie-row"])
            .build();
        let fingerprint = tie.id.fingerprint();
        row.set_title(&fingerprint);
        let used = tie.last_used_at.map(crate::start::day);
        row.set_subtitle(&tie_line(&crate::start::day(tie.tied_at), used.as_deref()));
        row.add_prefix(
            &gtk::Image::builder()
                .icon_name(crate::icons::IDENTITY)
                .css_classes(["invite-identity"])
                .build(),
        );
        if this_computer {
            row.add_suffix(
                &gtk::Label::builder()
                    .label("This device")
                    .valign(gtk::Align::Center)
                    .css_classes(["chip", "invite-chip", "invite-account"])
                    .build(),
            );
        }
        let sever = gtk::Button::builder()
            .label("Sever…")
            .valign(gtk::Align::Center)
            .css_classes(["row-button", "destructive-action"])
            .build();
        let (weak, id) = (Rc::downgrade(self), tie.id);
        sever.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.ask_sever(id, this_computer);
            }
        });
        row.add_suffix(&sever);
        row
    }

    /// Tie… and Tie another…: this computer's identity, made first when
    /// it has none.
    fn tie_pressed(self: &Rc<Self>) {
        let with = self.tie_with.borrow().clone();
        match with {
            TieWith::Create => (self.needs.open_identity)(),
            TieWith::Identity { id, name } => self.ask_tie(id, name),
        }
    }

    /// Asks before tying, naming the identity; the server may ask for a
    /// recent sign-in after, as Sever… does.
    fn ask_tie(self: &Rc<Self>, id: PortableUserId, name: String) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let alert = adw::AlertDialog::builder()
            .heading(tie_heading(&self.needs.username, &self.needs.server_name))
            .body(tie_body(&self.needs.username))
            .extra_child(&identity_card(&name, &id.fingerprint()))
            .default_response("cancel")
            .close_response("cancel")
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("tie", "Tie");
        alert.set_response_appearance("tie", adw::ResponseAppearance::Suggested);
        let weak = Rc::downgrade(self);
        alert.connect_response(Some("tie"), move |_, _| {
            if let Some(view) = weak.upgrade() {
                view.tie(id, name.clone(), false);
            }
        });
        alert.present(Some(&parent));
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
        self.run(
            move |s| Box::pin(async move { s.tie(id).await }),
            move |view, tied| match tied {
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

    /// Sever…: asks first, then severs; the server may ask for a recent
    /// sign-in after.
    fn ask_sever(self: &Rc<Self>, id: PortableUserId, this_computer: bool) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let alert = adw::AlertDialog::builder()
            .heading("Sever this identity?")
            .body(sever_body(&id.fingerprint(), this_computer))
            .default_response("cancel")
            .close_response("cancel")
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("sever", "Sever");
        alert.set_response_appearance("sever", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        alert.connect_response(Some("sever"), move |_, _| {
            if let Some(view) = weak.upgrade() {
                view.sever(id);
            }
        });
        alert.present(Some(&parent));
    }

    fn sever(self: &Rc<Self>, id: PortableUserId) {
        self.run(
            move |s| Box::pin(async move { s.untie(id).await }),
            move |view, severed| match severed {
                Ok(()) => {
                    view.toast(&format!(
                        "Severed {}. The account keeps everything.",
                        id.fingerprint()
                    ));
                    view.load();
                }
                Err(BackendError::RecentSignInRequired) => {
                    view.confirm("Sever this identity", "sever this identity", move |view| {
                        view.sever(id)
                    })
                }
                Err(e) => view.toast(&crate::confirm::refusal(&e)),
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

    fn with(n: u8) -> TieWith {
        TieWith::Identity {
            id: id(n),
            name: "Alice".into(),
        }
    }

    fn ties(bytes: &[u8]) -> Vec<Tie> {
        bytes
            .iter()
            .map(|&b| Tie {
                id: id(b),
                tied_at: std::time::UNIX_EPOCH,
                last_used_at: None,
            })
            .collect()
    }

    #[test]
    fn tying_asks_naming_the_identity_the_account_and_the_server() {
        assert_eq!(
            tie_heading("andoriyu", "Nimble"),
            "Tie your identity to andoriyu on Nimble?"
        );
    }

    #[test]
    fn tying_adds_to_the_others_and_says_where_to_tie_another() {
        let body = tie_body("andoriyu");
        assert!(
            body.contains("open andoriyu’s account here, alongside any identity already tied")
                && body.ends_with("tie it from a device that has it.")
        );
    }

    #[test]
    fn with_no_identity_here_tie_makes_one_first() {
        assert_eq!(tie_with(None), TieWith::Create);
    }

    #[test]
    fn only_this_computers_identity_is_marked_this_device() {
        let marked: Vec<bool> = ties(&[7, 8, 9])
            .iter()
            .map(|t| this_device(&t.id, &with(8)))
            .collect();
        assert_eq!(marked, [false, true, false]);
    }

    #[test]
    fn with_no_identity_here_no_tie_is_this_device() {
        assert!(!this_device(&id(8), &TieWith::Create));
    }

    #[test]
    fn tie_another_hides_once_this_computers_identity_is_tied() {
        assert!(!offers_another(&ties(&[7, 8]), &with(8)));
    }

    #[test]
    fn tie_another_offers_this_computers_identity_beside_others() {
        assert!(offers_another(&ties(&[7]), &with(8)));
    }

    #[test]
    fn tie_another_offers_to_make_an_identity_with_none_here() {
        assert!(offers_another(&ties(&[7]), &TieWith::Create));
    }

    #[test]
    fn with_no_ties_the_empty_rows_tie_is_the_only_one() {
        assert!(!offers_another(&[], &with(8)));
    }

    #[test]
    fn a_used_tie_says_when_it_was_tied_and_last_used() {
        assert_eq!(
            tie_line("12 Oct", Some("today")),
            "Tied 12 Oct · last used today"
        );
    }

    #[test]
    fn an_unused_tie_says_it_is_not_used_yet() {
        assert_eq!(tie_line("12 Oct", None), "Tied 12 Oct · Not used yet");
    }

    #[test]
    fn severing_another_identity_names_it_and_what_stays() {
        assert_eq!(
            sever_body("R2VN-8KQT-M4XA-H7PD", false),
            "Devices that sign in with R2VN-8KQT-M4XA-H7PD stop opening this account. The other \
             identities stay tied, and the account keeps everything."
        );
    }

    #[test]
    fn severing_this_computers_identity_says_it_stops_signing_in() {
        assert_eq!(
            sever_body("R2VN-8KQT-M4XA-H7PD", true),
            "Devices that sign in with R2VN-8KQT-M4XA-H7PD stop opening this account. The other \
             identities stay tied, and the account keeps everything. This computer stops signing \
             in with it."
        );
    }

    #[test]
    fn the_guest_line_leads_with_the_identitys_fingerprint() {
        assert!(guest_line(&id(7)).starts_with(&format!("{} · ", id(7).fingerprint())));
    }

    #[test]
    fn the_touch_names_the_identity_the_account_and_the_server() {
        assert_eq!(
            touch_line("Alice", "alice", "Home Media"),
            "Touch your key to tie Alice to alice on Home Media"
        );
    }

    /// The button under `root` labelled `label`.
    fn button_labelled(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = root.downcast_ref::<gtk::Button>()
            && button.label().as_deref() == Some(label)
        {
            return Some(button.clone());
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            if let Some(found) = button_labelled(&widget, label) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }

    /// Whether a visible label under `root` reads `text`.
    fn label_reading(root: &gtk::Widget, text: &str) -> bool {
        if let Some(label) = root.downcast_ref::<gtk::Label>()
            && label.is_visible()
            && label.label() == text
        {
            return true;
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            if label_reading(&widget, text) {
                return true;
            }
            child = widget.next_sibling();
        }
        false
    }

    /// `username`'s group on the fake in a window, which owns the view,
    /// once a row titled `title` shows; `opened` counts the times This
    /// computer › Your identity was opened to make one.
    fn shown(
        client: &spoolframe_linux_core::fake::FakeClient,
        username: &str,
        title: &str,
        opened: Rc<std::cell::Cell<u32>>,
    ) -> (adw::Window, adw::PreferencesGroup) {
        use spoolframe_linux_core::backend::Client;
        use spoolframe_linux_core::fake::FakeClient;
        let toasts = adw::ToastOverlay::new();
        let window = adw::Window::builder().content(&toasts).build();
        let page = adw::PreferencesPage::new();
        toasts.set_child(Some(&page));
        let group = group(
            Needs {
                session: client.session(&FakeClient::profile(username)).unwrap(),
                identities: client.identities(),
                server_name: "Home Media".into(),
                username: username.into(),
                open_identity: Rc::new(move || opened.set(opened.get() + 1)),
            },
            &toasts,
        );
        page.add(&group);
        window.present();
        assert!(
            crate::ui_test::spin_until(std::time::Duration::from_secs(5), || {
                crate::ui_test::row_titled(group.upcast_ref(), title).is_some()
                    && group.is_sensitive()
            }),
            "no row titled {title}"
        );
        (window, group)
    }

    /// One test: GTK binds itself to the first thread that starts it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn identity_here_on_the_fake_server() {
        gtk::init().unwrap();
        adw::init().unwrap();
        with_no_identity_here_tie_opens_your_identity();
        every_tie_is_listed_and_severing_one_leaves_the_other();
        tie_asks_first_and_cancel_ties_nothing();
    }

    /// Tie… names this computer's identity and waits: Cancel leaves the
    /// account untied, Tie ties it.
    fn tie_asks_first_and_cancel_ties_nothing() {
        use spoolframe_linux_core::backend::Client;
        use spoolframe_linux_core::fake::FakeClient;
        use spoolframe_linux_core::testing::{NoPrompt, ready};
        let client = FakeClient::new(None);
        let identities = client.identities();
        let key = identities.key(std::sync::Arc::new(NoPrompt));
        let label = spoolframe_linux_core::identity::KeyLabel {
            name: "Keyring key".into(),
            maker: None,
            added_at: 1_791_000_000,
        };
        let mine = ready(identities.create("Alice", &[(key, label)])).unwrap();
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        let (window, group) = shown(&client, "andrey", "Not tied to an identity", Rc::default());
        let press_tie = || {
            let row = crate::ui_test::row_titled(group.upcast_ref(), "Not tied to an identity");
            button_labelled(row.unwrap().upcast_ref(), "Tie…")
                .expect("the empty row has Tie…")
                .emit_clicked();
            assert!(crate::ui_test::spin_until(
                std::time::Duration::from_secs(2),
                || window.visible_dialog().is_some()
            ));
            window.visible_dialog().unwrap()
        };
        let asked = press_tie().downcast::<adw::AlertDialog>().unwrap();
        assert_eq!(
            (
                asked.heading().map(|h| h.to_string()),
                label_reading(asked.upcast_ref(), "Alice"),
                label_reading(asked.upcast_ref(), &mine.fingerprint()),
            ),
            (
                Some("Tie your identity to andrey on Home Media?".to_owned()),
                true,
                true
            )
        );
        button_labelled(asked.upcast_ref(), "Cancel")
            .unwrap()
            .emit_clicked();
        crate::ui_test::spin_until(std::time::Duration::from_millis(300), || false);
        assert!(ready(session.tie_info()).unwrap().is_empty());
        let asked = press_tie();
        button_labelled(asked.upcast_ref(), "Tie")
            .unwrap()
            .emit_clicked();
        assert!(
            crate::ui_test::spin_until(std::time::Duration::from_secs(5), || {
                crate::ui_test::row_titled(group.upcast_ref(), &mine.fingerprint()).is_some()
            }),
            "Tie did not tie this computer's identity"
        );
        let tied_dialog = window.visible_dialog();
        if let Some(dialog) = tied_dialog {
            dialog.force_close();
        }
        assert!(label_reading(
            group.upcast_ref(),
            "This computer’s identity is already tied."
        ));
        window.close();
    }

    fn with_no_identity_here_tie_opens_your_identity() {
        let client = spoolframe_linux_core::fake::FakeClient::new(None);
        let opened = Rc::new(std::cell::Cell::new(0));
        let (window, group) = shown(&client, "andrey", "Not tied to an identity", opened.clone());
        let row = crate::ui_test::row_titled(group.upcast_ref(), "Not tied to an identity");
        button_labelled(row.unwrap().upcast_ref(), "Create your identity…")
            .expect("the empty row has Create your identity…")
            .emit_clicked();
        assert_eq!(opened.get(), 1);
        window.close();
    }

    /// Two ties listed, oldest first; Sever… on the first asks, and once
    /// answered Sever only the second is left.
    fn every_tie_is_listed_and_severing_one_leaves_the_other() {
        use spoolframe_linux_core::backend::Client;
        use spoolframe_linux_core::fake::FakeClient;
        use spoolframe_linux_core::testing::ready;
        let client = FakeClient::new(None);
        let session = client.session(&FakeClient::profile("andrey")).unwrap();
        ready(session.tie(id(7))).unwrap();
        ready(session.tie(id(8))).unwrap();
        let (first, second) = (id(7).fingerprint(), id(8).fingerprint());
        let (window, group) = shown(&client, "andrey", &first, Rc::default());
        let listed = || {
            (
                crate::ui_test::row_titled(group.upcast_ref(), &first).is_some(),
                crate::ui_test::row_titled(group.upcast_ref(), &second).is_some(),
            )
        };
        assert_eq!(listed(), (true, true));
        let row = crate::ui_test::row_titled(group.upcast_ref(), &first).unwrap();
        button_labelled(row.upcast_ref(), "Sever…")
            .expect("a tie has Sever…")
            .emit_clicked();
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(2),
            || window.visible_dialog().is_some()
        ));
        let dialog = window.visible_dialog().unwrap();
        button_labelled(dialog.upcast_ref(), "Sever")
            .expect("the question has Sever")
            .emit_clicked();
        assert!(
            crate::ui_test::spin_until(std::time::Duration::from_secs(5), || listed()
                == (false, true)),
            "the severed tie is still listed, or the other is gone"
        );
        window.close();
    }
}
