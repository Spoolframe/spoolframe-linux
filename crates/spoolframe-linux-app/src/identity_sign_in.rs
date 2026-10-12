//! Who's watching › Sign in (P7-08): an account, or your identity. With
//! your identity, as on the LinuxIdentityPair board: a computer holding a
//! key identity pairs itself with one touch (1); one without shows a code
//! for the computer with the key, then the identity to compare and
//! accept (7–9), and beside it a QR an iPhone holding the identity scans
//! (P7-03), whichever comes first. Either way the app then signs in to the server as the
//! identity.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use spoolframe_client_sdk::{Pairing, RelayAnswer, RelayPairing, SdkError};
use spoolframe_linux_core::identity::{Identities, IdentityView};
use spoolframe_protocol::portable::PortableUserId;

use crate::dialog::{buttons, column};
use crate::key_prompt::{self, Action};
use crate::start::dim;

/// The identity row when sign-ins with other identities are off here.
pub const OFF_HERE: &str = "Off on this computer. Turn it on in Preferences › Servers.";

/// Under the QR (LinuxIdentityPair 7).
pub const SCAN: &str = "Or scan it with your iPhone.";

/// Once the iPhone connected: "Check your phone shows K7QX-3MPA-9TRD-WF2H".
pub fn check_line(fingerprint: &str) -> String {
    format!("Check your phone shows {fingerprint}")
}

/// How often the device asks whether the code was answered.
const POLL: Duration = Duration::from_secs(2);

/// "Waiting · expires in 4:41".
pub fn waiting_line(left: Duration) -> String {
    let secs = left.as_secs();
    format!("Waiting · expires in {}:{:02}", secs / 60, secs % 60)
}

/// Whether this computer can sign in as `view`'s identity with no touch:
/// it is one of its devices, and not in its last 14 days.
pub fn paired_already(view: &IdentityView) -> bool {
    view.renewal_due.is_none() && view.devices.iter().any(|d| d.this_computer)
}

/// What the dialog needs.
pub struct Needs {
    pub identities: Identities,
    /// The server's id, which holds the code, and its name.
    pub server: String,
    pub server_name: String,
    /// Sign in as the identity.
    pub sign_in: Rc<dyn Fn(PortableUserId)>,
    /// Sign in with an account instead.
    pub account: Rc<dyn Fn()>,
}

struct View {
    needs: Needs,
    dialog: adw::Dialog,
    stack: gtk::Stack,
    code: gtk::Label,
    device_key: gtk::Label,
    waiting: gtk::Label,
    identity: gtk::Label,
    trouble: gtk::Label,
    accept: gtk::Button,
    answer: RefCell<Option<RelayAnswer>>,
    /// The QR beside the code, and what it says under it.
    qr: gtk::Picture,
    scan: gtk::Label,
    /// The pairing by QR running beside the code; cancelled when the code
    /// is, or the dialog goes.
    local: RefCell<Option<Arc<Pairing>>>,
    /// Bumped by each new code and on close: an older poll stops.
    round: Cell<u64>,
}

/// The chooser, over `parent`.
pub fn open(parent: &impl IsA<gtk::Widget>, needs: Needs) {
    let dialog = adw::Dialog::builder()
        .title("Sign in")
        .content_width(460)
        .build();
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vhomogeneous(false)
        .build();

    // Choose.
    let choose = column();
    let ways = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let account = adw::ActionRow::builder()
        .use_markup(false)
        .activatable(true)
        .build();
    account.set_title("Sign in with an account");
    account.set_subtitle("Password, passkey or your phone");
    account.add_prefix(&gtk::Image::from_icon_name(crate::icons::PROFILE));
    account.add_suffix(&gtk::Image::from_icon_name(crate::icons::RIGHT));
    let identity_row = adw::ActionRow::builder()
        .use_markup(false)
        .activatable(true)
        .build();
    identity_row.set_title("Sign in with your identity");
    identity_row.set_subtitle("A security key, or a device that holds your identity");
    identity_row.add_prefix(&gtk::Image::from_icon_name(crate::icons::IDENTITY));
    identity_row.add_suffix(&gtk::Image::from_icon_name(crate::icons::RIGHT));
    ways.append(&account);
    ways.append(&identity_row);
    // Off on this computer, with no identity of its own to sign in with:
    // said, and not offered (LinuxPortableStates 4).
    identity_row.set_sensitive(false);
    {
        let (identities, row) = (needs.identities.clone(), identity_row.downgrade());
        glib::spawn_future_local(async move {
            let open = identities
                .run(|ids| {
                    Box::pin(async move {
                        Ok(ids.current().await?.is_some() || ids.allow_guests().await?)
                    })
                })
                .await;
            let Some(row) = row.upgrade() else { return };
            match open {
                Ok(true) => row.set_sensitive(true),
                Ok(false) => row.set_subtitle(OFF_HERE),
                Err(e) => row.set_subtitle(&crate::app::sentence(&e.to_string())),
            }
        });
    }
    choose.append(&ways);
    stack.add_named(&choose, Some("choose"));

    // 7 · The code, on this computer as the device.
    let code_page = column();
    code_page.append(&dim(
        "On the computer with your key, open Preferences › Identity › Pair a device and type:",
    ));
    let code = gtk::Label::builder()
        .selectable(true)
        .css_classes(["title-1", "monospace", "invite-identity"])
        .build();
    code_page.append(&code);
    let device_key = dim("");
    device_key.add_css_class("monospace");
    code_page.append(&device_key);
    let waiting = gtk::Label::builder()
        .css_classes(["caption", "dim-label"])
        .build();
    let qr = gtk::Picture::builder()
        .width_request(180)
        .height_request(180)
        .halign(gtk::Align::Center)
        .visible(false)
        .css_classes(["qr"])
        .build();
    code_page.append(&qr);
    let scan = dim(SCAN);
    scan.set_visible(false);
    code_page.append(&scan);
    code_page.append(&waiting);
    let note = dim(&format!(
        "This computer gets its own sign-in as you, and signs in to {} once you accept.",
        needs.server_name
    ));
    note.add_css_class("caption");
    code_page.append(&note);
    let code_cancel = gtk::Button::builder()
        .label("Cancel")
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    code_page.append(&code_cancel);
    stack.add_named(&code_page, Some("code"));

    // 8 · Compare, then accept.
    let compare = column();
    compare.append(&dim(
        "Your key approved this computer. Check this is your identity before accepting:",
    ));
    let identity = gtk::Label::builder()
        .selectable(true)
        .css_classes(["title-2", "monospace", "invite-identity"])
        .build();
    compare.append(&identity);
    let compare_buttons = buttons();
    let not_mine = gtk::Button::builder().label("Not mine").build();
    let accept = gtk::Button::builder()
        .label("Accept")
        .css_classes(["suggested-action"])
        .build();
    compare_buttons.append(&not_mine);
    compare_buttons.append(&accept);
    compare.append(&compare_buttons);
    stack.add_named(&compare, Some("compare"));

    // 9 · Expired, or anything else that stopped it.
    let expired = column();
    let expired_title = gtk::Label::builder()
        .label("The code expired")
        .css_classes(["title-3"])
        .build();
    expired.append(&expired_title);
    let trouble = dim("Nobody entered it in 5 minutes. Get a new one and try again.");
    expired.append(&trouble);
    let expired_buttons = buttons();
    let expired_cancel = gtk::Button::builder().label("Cancel").build();
    let new_code = gtk::Button::builder()
        .label("New code")
        .css_classes(["suggested-action"])
        .build();
    expired_buttons.append(&expired_cancel);
    expired_buttons.append(&new_code);
    expired.append(&expired_buttons);
    stack.add_named(&expired, Some("expired"));

    crate::start::with_cancel(&dialog, &stack);
    let view = Rc::new(View {
        needs,
        dialog,
        stack,
        code,
        device_key,
        waiting,
        identity,
        trouble,
        accept,
        answer: RefCell::new(None),
        qr,
        scan,
        local: RefCell::new(None),
        round: Cell::new(0),
    });
    {
        // Before the view is let go, below: the pairing stops with it.
        let weak = Rc::downgrade(&view);
        view.dialog.connect_closed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.next_round();
                view.stop_local();
            }
        });
    }
    crate::dialog::keep_until_closed(&view.dialog, view.clone());
    {
        let weak = Rc::downgrade(&view);
        account.connect_activated(move |_| {
            if let Some(view) = weak.upgrade() {
                view.dialog.close();
                (view.needs.account)();
            }
        });
    }
    {
        let weak = Rc::downgrade(&view);
        identity_row.connect_activated(move |_| {
            if let Some(view) = weak.upgrade() {
                view.with_identity();
            }
        });
    }
    for cancel in [&code_cancel, &not_mine, &expired_cancel] {
        let weak = Rc::downgrade(&view);
        cancel.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.dialog.close();
            }
        });
    }
    {
        let weak = Rc::downgrade(&view);
        new_code.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.offer();
            }
        });
    }
    {
        let weak = Rc::downgrade(&view);
        view.accept.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.take();
            }
        });
    }
    view.dialog.present(Some(parent));
}

impl View {
    /// Starts a new round: whatever an older one still waits on stops
    /// once it next looks. The new round's number.
    fn next_round(&self) -> u64 {
        let round = self.round.get() + 1;
        self.round.set(round);
        round
    }

    fn is_current(&self, round: u64) -> bool {
        self.round.get() == round
    }

    /// The key identity on this computer signs in, pairing itself first
    /// if it must; with none, the code.
    fn with_identity(self: &Rc<Self>) {
        let (identities, weak) = (self.needs.identities.clone(), Rc::downgrade(self));
        glib::spawn_future_local(async move {
            let current = identities
                .run(|ids| Box::pin(async move { ids.current().await }))
                .await;
            let Some(view) = weak.upgrade() else { return };
            match current {
                Ok(Some(identity)) if paired_already(&identity) => view.done(identity.identity.id),
                Ok(Some(identity)) => view.pair_self(&identity),
                Ok(None) => view.offer(),
                Err(e) => view.stop(&e),
            }
        });
    }

    /// 1 · One touch: this computer approves itself.
    fn pair_self(self: &Rc<Self>, identity: &IdentityView) {
        let id = identity.identity.id;
        let op = key_prompt::op_on(&self.needs.identities, move |ids, prompt| {
            let key = ids.key(prompt);
            Box::pin(async move { ids.pair_self(id, key.as_ref()).await })
        });
        let name = identity.identity.name.clone();
        let action = Action {
            touch: "Touch your key to pair this computer".into(),
            keys: Some(format!(
                "Your key is here, so this computer approves itself. It gets its own sign-in as \
                 {name} for 90 days."
            )),
            identity: Some(name),
            other_key: None,
            not_found: None,
        };
        let weak = Rc::downgrade(self);
        key_prompt::act(&self.dialog, action, op, move |()| {
            if let Some(view) = weak.upgrade() {
                view.done(id);
            }
        });
    }

    /// 7 · A new code held on the server, then waiting for its answer.
    fn offer(self: &Rc<Self>) {
        let round = self.next_round();
        self.stop_local();
        self.dialog.set_title("Sign in with your identity");
        let (identities, server) = (self.needs.identities.clone(), self.needs.server.clone());
        let name = glib::host_name().to_string();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let offered = identities
                .run(move |ids| Box::pin(async move { ids.offer_pairing(&server, &name).await }))
                .await;
            let Some(view) = weak.upgrade() else { return };
            match offered {
                Ok(pairing) => {
                    view.code.set_text(&pairing.code());
                    view.device_key
                        .set_text(&format!("This computer’s key {}", pairing.fingerprint()));
                    view.waiting.set_text(&waiting_line(pairing.expires_in()));
                    view.stack.set_visible_child_name("code");
                    view.wait(pairing, round);
                    view.start_local(round);
                }
                Err(e) => view.stop(&e),
            }
        });
    }

    /// Asks for the answer every few seconds, counting down, until it
    /// comes, the code expires, or the person leaves.
    fn wait(self: &Rc<Self>, pairing: RelayPairing, round: u64) {
        let (identities, weak) = (self.needs.identities.clone(), Rc::downgrade(self));
        glib::spawn_future_local(async move {
            loop {
                glib::timeout_future(POLL).await;
                let Some(view) = weak.upgrade() else { return };
                if !view.is_current(round) {
                    return;
                }
                let left = pairing.expires_in();
                if left.is_zero() {
                    return view.expired();
                }
                view.waiting.set_text(&waiting_line(left));
                drop(view);
                let asked = pairing.clone();
                let collected = identities
                    .run(move |ids| Box::pin(async move { ids.collect_pairing(&asked).await }))
                    .await;
                let Some(view) = weak.upgrade() else { return };
                if !view.is_current(round) {
                    return;
                }
                match collected {
                    Ok(None) => {}
                    Ok(Some(answer)) => return view.compare(answer),
                    Err(SdkError::NotFound(_)) => return view.expired(),
                    Err(e) => return view.stop(&e),
                }
            }
        });
    }

    /// The QR beside the code: an iPhone that holds the identity scans
    /// it (P7-03). Without it (guests off, a pairing already running) the
    /// code alone still works, so a failure only hides it.
    fn start_local(self: &Rc<Self>, round: u64) {
        let (identities, weak) = (self.needs.identities.clone(), Rc::downgrade(self));
        let name = glib::host_name().to_string();
        glib::spawn_future_local(async move {
            let started = identities
                .run(move |ids| Box::pin(async move { ids.start_pairing(&name).await }))
                .await;
            let Some(view) = weak.upgrade() else { return };
            let pairing = match started {
                Ok(pairing) if view.is_current(round) => pairing,
                Ok(pairing) => return pairing.cancel(),
                Err(e) => return tracing::warn!(error = %e, "no QR beside the pairing code"),
            };
            match crate::qr::texture(pairing.link(), crate::qr::MODULE) {
                Ok(texture) => {
                    view.qr.set_paintable(Some(&texture));
                    view.qr.set_visible(true);
                    view.scan.set_text(SCAN);
                    view.scan.set_visible(true);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot draw the pairing QR");
                    return pairing.cancel();
                }
            }
            *view.local.borrow_mut() = Some(pairing.clone());
            drop(view);
            // The phone connected: its fingerprint, to compare.
            let asked = pairing.clone();
            let connected = identities
                .run(move |_| Box::pin(async move { asked.connected().await }))
                .await;
            let Some(view) = weak.upgrade() else { return };
            if !view.is_current(round) {
                return;
            }
            if let Ok(phone) = &connected {
                view.scan.set_text(&check_line(&phone.fingerprint));
            }
            drop(view);
            let paired = identities
                .run(move |_| Box::pin(async move { pairing.wait().await }))
                .await;
            let Some(view) = weak.upgrade() else { return };
            if !view.is_current(round) {
                return;
            }
            match paired {
                Ok(paired) => view.done(paired.identity),
                // The code still works: the QR goes.
                Err(e) => {
                    tracing::info!(error = %e, "the pairing by QR ended");
                    view.qr.set_visible(false);
                    view.scan.set_visible(false);
                }
            }
        });
    }

    fn stop_local(&self) {
        if let Some(pairing) = self.local.borrow_mut().take() {
            pairing.cancel();
        }
        self.qr.set_visible(false);
        self.scan.set_visible(false);
    }

    /// 8 · The identity the key that answered is a root of.
    fn compare(&self, answer: RelayAnswer) {
        self.identity
            .set_text(&format!("Identity {}", answer.identity_fingerprint()));
        *self.answer.borrow_mut() = Some(answer);
        self.stack.set_visible_child_name("compare");
        self.accept.grab_focus();
    }

    /// Accept: this computer becomes one of the identity's devices.
    fn take(self: &Rc<Self>) {
        let Some(answer) = self.answer.borrow_mut().take() else {
            return;
        };
        self.accept.set_sensitive(false);
        let (identities, weak) = (self.needs.identities.clone(), Rc::downgrade(self));
        glib::spawn_future_local(async move {
            let accepted = identities
                .run(move |ids| Box::pin(async move { ids.accept_pairing(answer).await }))
                .await;
            let Some(view) = weak.upgrade() else { return };
            view.accept.set_sensitive(true);
            match accepted {
                Ok(id) => view.done(id),
                Err(e) => view.stop(&e),
            }
        });
    }

    fn expired(&self) {
        self.stop_local();
        self.trouble
            .set_text("Nobody entered it in 5 minutes. Get a new one and try again.");
        self.stack.set_visible_child_name("expired");
    }

    /// What stopped it, on the expired page's frame, with a new code.
    fn stop(&self, error: &SdkError) {
        self.stop_local();
        self.trouble
            .set_text(&crate::app::sentence(&error.to_string()));
        self.stack.set_visible_child_name("expired");
    }

    fn done(&self, id: PortableUserId) {
        self.next_round();
        self.stop_local();
        self.dialog.close();
        (self.needs.sign_in)(id);
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::identity::{KeyLabel, in_memory};
    use spoolframe_linux_core::testing::ready;

    use super::*;

    use spoolframe_linux_core::testing::NoPrompt;

    /// The chooser keeps its view while it shows: "Sign in with an
    /// account" still answers once `open` has returned.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_account_row_answers_after_the_chooser_opens() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let window = adw::Window::new();
        window.present();
        let chose = Rc::new(Cell::new(false));
        let seen = chose.clone();
        open(
            &window,
            Needs {
                identities: in_memory(),
                server: "k".into(),
                server_name: "Home Media".into(),
                sign_in: Rc::new(|_| {}),
                account: Rc::new(move || seen.set(true)),
            },
        );
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(2),
            || window.visible_dialog().is_some()
        ));
        let dialog = window.visible_dialog().unwrap();
        let row =
            crate::ui_test::row_titled(dialog.upcast_ref(), "Sign in with an account").unwrap();
        adw::prelude::ActionRowExt::activate(&row);
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(2),
            || chose.get()
        ));
    }

    #[test]
    fn a_phone_that_scanned_is_named_by_its_fingerprint() {
        assert_eq!(
            check_line("K7QX-3MPA-9TRD-WF2H"),
            "Check your phone shows K7QX-3MPA-9TRD-WF2H"
        );
    }

    #[test]
    fn the_countdown_shows_minutes_and_seconds() {
        assert_eq!(
            waiting_line(Duration::from_secs(281)),
            "Waiting · expires in 4:41"
        );
    }

    #[test]
    fn a_computer_its_identity_paired_signs_in_with_no_touch() {
        let identities = in_memory();
        let key = identities.key(Arc::new(NoPrompt));
        let label = KeyLabel {
            name: "Main key".into(),
            maker: None,
            added_at: 0,
        };
        ready(identities.create("Alice", &[(key, label)])).unwrap();
        let view = ready(identities.current()).unwrap().unwrap();
        assert!(paired_already(&view));
    }
}
