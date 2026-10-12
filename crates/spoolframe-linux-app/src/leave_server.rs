//! A server's page › Leave, as on LinuxSettingsServer: the last group of
//! the profile's part. Leaving (PX-47) disables a local account (the owner
//! can restore it) or ends a guest's access for good; either way the server
//! is forgotten on this computer.
//!
//! Leaving needs a sign-in in the last 10 minutes. An account confirms it
//! is them ([`crate::confirm`]); a guest has no password, so it signs in
//! again with its identity, which counts as recent, and leaves on that new
//! session. The server's last admin is refused, and Leave greys out.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{BackendError, BoxFuture, Client, ServerKey, Session};
use spoolframe_protocol::portable::PortableUserId;

/// What the group needs.
pub struct Needs {
    pub session: Arc<dyn Session>,
    /// To sign a guest in again with its identity.
    pub client: Arc<dyn Client>,
    pub server: ServerKey,
    pub server_name: String,
    /// Settings drops the server's row and page.
    pub dropped: Rc<dyn Fn()>,
    /// The app lets go of the server, if it was in use.
    pub forgotten: Rc<dyn Fn(ServerKey)>,
}

/// What leaving says when the server refuses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// "Confirm it's you", then leave again.
    Confirm,
    /// Sign in again as the guest's identity, then leave on that session.
    SignInAgain(PortableUserId),
    /// The last admin: Leave greys out, saying why.
    LastAdmin,
    /// A toast saying so.
    Say(String),
}

/// What to do once the server refused to let `guest` (or an account,
/// with none) leave with `error`; `signed_in_again` once a guest already
/// has, so it is not asked round again.
pub fn refused(
    error: &BackendError,
    guest: Option<PortableUserId>,
    signed_in_again: bool,
) -> Refused {
    match (error, guest) {
        (BackendError::LastAdmin, _) => Refused::LastAdmin,
        (BackendError::RecentSignInRequired, None) => Refused::Confirm,
        (BackendError::RecentSignInRequired, Some(id)) if !signed_in_again => {
            Refused::SignInAgain(id)
        }
        (other, _) => Refused::Say(crate::app::sentence(&other.to_string())),
    }
}

/// What leaving does, under the row.
pub fn subtitle(guest: bool) -> &'static str {
    if guest {
        "Gives up your guest access. Your history and settings there are deleted."
    } else {
        "Your account is disabled and kept; the owner can restore it."
    }
}

/// What the question says leaving does.
pub fn question_body(guest: bool) -> &'static str {
    if guest {
        "Your watch history and settings there are deleted and the server is removed from this \
         device. To come back you’ll need a new invite."
    } else {
        "You’re signed out everywhere and the server is removed from this device. The owner can \
         restore your account."
    }
}

/// The toast once the user has left.
pub fn left_line(server_name: &str) -> String {
    format!("You left {server_name}. It’s no longer saved here.")
}

/// "Leave", for the session in `needs`.
pub fn group(needs: Needs, toasts: &adw::ToastOverlay) -> adw::PreferencesGroup {
    let guest = needs.session.guest_identity();
    let group = adw::PreferencesGroup::builder().title("Leave").build();
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title("Leave this server")
        .build();
    row.set_subtitle(subtitle(guest.is_some()));
    let button = gtk::Button::builder()
        .label("Leave…")
        .valign(gtk::Align::Center)
        .css_classes(["row-button", "destructive-action"])
        .build();
    row.add_suffix(&button);
    group.add(&row);
    let view = Rc::new(View {
        needs,
        guest,
        toasts: toasts.downgrade(),
        row,
        button,
    });
    {
        let weak = Rc::downgrade(&view);
        view.button.connect_clicked(move |button| {
            let Some(view) = weak.upgrade() else { return };
            let weak = Rc::downgrade(&view);
            ask(
                button,
                &view.needs.server_name,
                view.guest.is_some(),
                Rc::new(move || {
                    if let Some(view) = weak.upgrade() {
                        let session = view.needs.session.clone();
                        view.leave(session, false);
                    }
                }),
            );
        });
    }
    // The last admin, as the session was told: greyed out from the start,
    // then asked again in case that changed since.
    if !view.needs.session.can_leave() {
        view.greyed();
    }
    {
        let (session, weak) = (view.needs.session.clone(), Rc::downgrade(&view));
        glib::spawn_future_local(async move {
            let can_leave = session.can_leave_now().await;
            let Some(view) = weak.upgrade() else { return };
            match can_leave {
                Ok(false) => view.greyed(),
                Ok(true) => view.ungreyed(),
                // Kept as it was: the refusal is the backstop.
                Err(e) => tracing::warn!(error = %e, "cannot ask whether the user can leave"),
            }
        });
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
    guest: Option<PortableUserId>,
    /// Settings' toasts, and the parent its dialogs show over.
    toasts: glib::WeakRef<adw::ToastOverlay>,
    row: adw::ActionRow,
    button: gtk::Button,
}

impl View {
    fn toast(&self, text: &str) {
        if let Some(toasts) = self.toasts.upgrade() {
            toasts.add_toast(crate::markup::toast(text));
        }
    }

    /// Runs `call` off the main loop, then `then` with its answer back on
    /// it, while the view lasts.
    fn run<T: Send + 'static>(
        self: &Rc<Self>,
        call: BoxFuture<'static, T>,
        then: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        let handle = relm4::spawn(call);
        let this = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = handle.await else { return };
            if let Some(this) = this.upgrade() {
                then(&this, answer);
            }
        });
    }

    /// Leaves on `session`: the page's own, or a guest's new one once it
    /// has `signed_in_again`.
    fn leave(self: &Rc<Self>, session: Arc<dyn Session>, signed_in_again: bool) {
        self.button.set_sensitive(false);
        let leaving = session.clone();
        self.run(
            Box::pin(async move { leaving.leave_server().await }),
            move |view, answer| {
                view.button.set_sensitive(true);
                let Err(e) = answer else {
                    return view.left();
                };
                match refused(&e, view.guest, signed_in_again) {
                    Refused::Confirm => view.confirm(session),
                    Refused::SignInAgain(id) => view.sign_in_again(id),
                    Refused::LastAdmin => view.last_admin(),
                    Refused::Say(text) => view.toast(&text),
                }
            },
        );
    }

    /// "Confirm it's you" on `session`, then leave again.
    fn confirm(self: &Rc<Self>, session: Arc<dyn Session>) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let again = session.clone();
        crate::confirm::ask_because(
            &parent,
            session,
            &self.needs.server_name,
            "Leave this server",
            &format!("To leave {}, confirm it’s you.", self.needs.server_name),
            Rc::new(move || {
                if let Some(view) = weak.upgrade() {
                    view.leave(again.clone(), false);
                }
            }),
        );
    }

    /// A guest has no password to confirm with: a new sign-in as its
    /// identity is recent, so it leaves on that.
    fn sign_in_again(self: &Rc<Self>, identity: PortableUserId) {
        self.button.set_sensitive(false);
        let (client, server) = (self.needs.client.clone(), self.needs.server.clone());
        self.run(
            Box::pin(async move {
                let profile = client.sign_in_portable(&server, identity).await?;
                client.session(&profile)
            }),
            |view, session| {
                view.button.set_sensitive(true);
                match session {
                    Ok(session) => view.leave(session, true),
                    Err(e) => view.toast(&crate::app::sentence(&e.to_string())),
                }
            },
        );
    }

    /// Gone from the server and from this computer: Settings drops it, the
    /// app lets go of it, and says so.
    fn left(&self) {
        // The page, and this view with it, may go in `dropped`: keep what
        // comes after.
        let (forgotten, server) = (self.needs.forgotten.clone(), self.needs.server.clone());
        let (toasts, line) = (self.toasts.clone(), left_line(&self.needs.server_name));
        (self.needs.dropped)();
        forgotten(server);
        if let Some(toasts) = toasts.upgrade() {
            toasts.add_toast(crate::markup::toast(&line));
        }
    }

    /// The last admin cannot leave until someone else is one: the row is
    /// greyed out, saying why.
    fn greyed(&self) {
        self.row.set_subtitle(&BackendError::LastAdmin.to_string());
        self.row.set_sensitive(false);
        self.button.set_sensitive(false);
    }

    /// No longer the last admin: Leave as it was.
    fn ungreyed(&self) {
        self.row.set_subtitle(subtitle(self.guest.is_some()));
        self.row.set_sensitive(true);
        self.button.set_sensitive(true);
    }

    /// Refused as the last admin all the same (another admin went since
    /// the page asked): said at once, in a dialog with OK, and greyed out.
    fn last_admin(&self) {
        self.greyed();
        let why = BackendError::LastAdmin.to_string();
        if let Some(parent) = self.toasts.upgrade() {
            let alert = adw::AlertDialog::new(None, None);
            alert.set_heading(Some(&why));
            alert.add_response("ok", "OK");
            alert.present(Some(&parent));
        }
    }
}

/// "Leave <server>?", over `anchor`; `leave` on Leave.
fn ask(anchor: &impl IsA<gtk::Widget>, server_name: &str, guest: bool, leave: Rc<dyn Fn()>) {
    let alert = adw::AlertDialog::builder()
        .heading(format!("Leave {server_name}?"))
        .body(question_body(guest))
        .default_response("cancel")
        .close_response("cancel")
        .build();
    alert.add_response("cancel", "Cancel");
    alert.add_response("leave", "Leave");
    alert.set_response_appearance("leave", adw::ResponseAppearance::Destructive);
    alert.connect_response(Some("leave"), move |_, _| leave());
    alert.present(Some(anchor));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> PortableUserId {
        PortableUserId::from_bytes([byte; 32]).unwrap()
    }

    #[test]
    fn a_guest_is_told_their_access_ends() {
        assert!(subtitle(true).contains("guest access"));
    }

    #[test]
    fn an_account_is_told_it_can_be_restored() {
        assert!(subtitle(false).contains("restore"));
    }

    #[test]
    fn a_guest_needs_a_new_invite_to_come_back() {
        assert!(question_body(true).contains("new invite"));
    }

    #[test]
    fn the_left_line_names_the_server() {
        assert!(left_line("Home Media").starts_with("You left Home Media."));
    }

    #[test]
    fn an_account_asked_for_a_recent_sign_in_confirms_it_is_them() {
        assert_eq!(
            refused(&BackendError::RecentSignInRequired, None, false),
            Refused::Confirm
        );
    }

    #[test]
    fn a_guest_asked_for_a_recent_sign_in_signs_in_again_as_its_identity() {
        assert_eq!(
            refused(&BackendError::RecentSignInRequired, Some(id(7)), false),
            Refused::SignInAgain(id(7))
        );
    }

    /// A fresh portable sign-in is recent: asked again, it is said, not
    /// gone round forever.
    #[test]
    fn a_guest_asked_again_after_signing_in_again_is_told() {
        assert!(matches!(
            refused(&BackendError::RecentSignInRequired, Some(id(7)), true),
            Refused::Say(_)
        ));
    }

    #[test]
    fn the_last_admin_greys_leave_out() {
        assert_eq!(
            refused(&BackendError::LastAdmin, None, false),
            Refused::LastAdmin
        );
    }

    #[test]
    fn anything_else_is_said_as_a_sentence() {
        assert_eq!(
            refused(
                &BackendError::Other("unsupported request".into()),
                None,
                false
            ),
            Refused::Say("Unsupported request.".into())
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

    /// The window the group shows in, its row, how often Settings was told
    /// to drop the server, and the servers the app was told of.
    struct Left {
        window: adw::Window,
        row: adw::ActionRow,
        dropped: Rc<std::cell::Cell<u32>>,
        forgotten: Rc<RefCell<Vec<ServerKey>>>,
    }

    /// `username`'s Leave group on the fake, in a window, with Leave…
    /// pressed and the question answered Leave.
    fn left_as(username: &str) -> Left {
        use spoolframe_linux_core::fake::FakeClient;
        let client = Arc::new(FakeClient::new(None));
        let profile = FakeClient::profile(username);
        let dropped = Rc::new(std::cell::Cell::new(0));
        let forgotten = Rc::new(RefCell::new(Vec::new()));
        let toasts = adw::ToastOverlay::new();
        let window = adw::Window::builder().content(&toasts).build();
        let page = adw::PreferencesPage::new();
        toasts.set_child(Some(&page));
        let group = group(
            Needs {
                session: client.session(&profile).unwrap(),
                client: client.clone(),
                server: profile.server.clone(),
                server_name: "Home Media".into(),
                dropped: {
                    let dropped = dropped.clone();
                    Rc::new(move || dropped.set(dropped.get() + 1))
                },
                forgotten: {
                    let forgotten = forgotten.clone();
                    Rc::new(move |key| forgotten.borrow_mut().push(key))
                },
            },
            &toasts,
        );
        page.add(&group);
        window.present();
        let row = crate::ui_test::row_titled(group.upcast_ref(), "Leave this server").unwrap();
        // Clicked as the code does, so the race (greyed out, refused all
        // the same) can be driven too.
        button_labelled(row.upcast_ref(), "Leave…")
            .unwrap()
            .emit_clicked();
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(2),
            || window.visible_dialog().is_some()
        ));
        let dialog = window.visible_dialog().unwrap();
        button_labelled(dialog.upcast_ref(), "Leave")
            .expect("the question has Leave")
            .emit_clicked();
        Left {
            window,
            row,
            dropped,
            forgotten,
        }
    }

    /// One test: GTK binds itself to the first thread that starts it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn leave_server_on_the_fake_server() {
        gtk::init().unwrap();
        adw::init().unwrap();
        leaving_drops_the_server_and_lets_the_app_go_of_it();
        the_last_admin_is_greyed_out_before_trying();
        the_last_admin_is_told_and_greyed_out();
    }

    fn leaving_drops_the_server_and_lets_the_app_go_of_it() {
        let Left {
            window,
            dropped,
            forgotten,
            ..
        } = left_as("steve");
        assert!(crate::ui_test::spin_until(
            std::time::Duration::from_secs(5),
            || dropped.get() == 1
        ));
        let home = spoolframe_linux_core::fake::FakeClient::profile("steve").server;
        assert_eq!(*forgotten.borrow(), vec![home]);
        window.close();
    }

    /// The last admin, as the session says: greyed out from the start,
    /// saying why, with nothing asked.
    fn the_last_admin_is_greyed_out_before_trying() {
        use spoolframe_linux_core::fake::{FAKE_ADMIN, FakeClient};
        let client = Arc::new(FakeClient::new(None));
        let profile = FakeClient::profile(FAKE_ADMIN);
        let toasts = adw::ToastOverlay::new();
        let group = group(
            Needs {
                session: client.session(&profile).unwrap(),
                client: client.clone(),
                server: profile.server.clone(),
                server_name: "Home Media".into(),
                dropped: Rc::new(|| {}),
                forgotten: Rc::new(|_| {}),
            },
            &toasts,
        );
        let row = crate::ui_test::row_titled(group.upcast_ref(), "Leave this server").unwrap();
        assert_eq!(
            (row.is_sensitive(), row.subtitle().map(|s| s.to_string())),
            (false, Some(BackendError::LastAdmin.to_string()))
        );
    }

    /// Refused as the last admin all the same: the backstop dialog.
    fn the_last_admin_is_told_and_greyed_out() {
        let Left {
            window,
            row,
            dropped,
            ..
        } = left_as(spoolframe_linux_core::fake::FAKE_ADMIN);
        // Said at once, not only on the greyed-out row: the row is greyed
        // out from the start, so wait for the refusal's dialog itself.
        let said = || {
            window
                .visible_dialog()
                .and_downcast::<adw::AlertDialog>()
                .and_then(|alert| alert.heading())
                .map(|heading| heading.to_string())
        };
        let why = BackendError::LastAdmin.to_string();
        crate::ui_test::spin_until(std::time::Duration::from_secs(5), || {
            said().as_deref() == Some(why.as_str())
        });
        assert_eq!(
            (dropped.get(), row.is_sensitive(), said()),
            (0, false, Some(why.clone()))
        );
        window.close();
    }
}
