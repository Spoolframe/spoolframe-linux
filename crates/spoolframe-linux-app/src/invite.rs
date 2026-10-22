//! "Add a server" from an invite (P6-01), as the design's LinuxInviteAdd,
//! LinuxInviteFingerprint, LinuxInviteJoin and LinuxInviteStates boards
//! draw it: the pasted link and what it says, the server's fingerprint,
//! then the join form for a link with a code, or the server saved for one
//! without. A refused code offers "Save server only".
//!
//! Before Continue, a link's code is looked up at its server (SDK
//! 0.14.4), and the summary says what it is: who it is from, or that it
//! expired, was used or revoked, or matches nothing. Opened for a server
//! on this network ("Use invite" on its row), the field also takes the
//! code alone, typed in.
//!
//! The dialog draws; the app makes the calls and tells it what came back.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use adw::prelude::*;
use spoolframe_linux_core::backend::{
    BackendError, Invite, InviteKind, InviteLookup, InviteOutcome, Server, ServerKey,
};
use spoolframe_linux_core::invites;
use spoolframe_types::Role;

use crate::start::{action, field, with_cancel};

/// An invite to look up before joining.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// A link with a code, pasted or opened.
    Link(String),
    /// A code typed in at `server`, normalized; `link` is the one made
    /// from it, which the answer comes back for.
    Code {
        link: String,
        server: ServerKey,
        addresses: Vec<String>,
        code: String,
    },
}

impl Lookup {
    /// The link the answer is for.
    pub fn link(&self) -> &str {
        match self {
            Lookup::Link(link) | Lookup::Code { link, .. } => link,
        }
    }
}

/// How long the field must rest before its invite is looked up: a code
/// typed a character at a time is looked up once, and each code that
/// matches nothing counts on the server as a wrong guess.
const LOOKUP_PAUSE: Duration = Duration::from_millis(500);

/// Which change of the field a lookup waits for. Each change makes every
/// earlier wait stale, so only the one after the last change looks up.
#[derive(Debug, Default)]
pub struct Debounce(Cell<u64>);

impl Debounce {
    /// The field changed: the waits begun before are stale.
    pub fn change(&self) {
        self.0.set(self.0.get() + 1);
    }

    /// A wait begun now, for [`Debounce::is_current`] once it is over.
    pub fn wait(&self) -> u64 {
        self.0.get()
    }

    /// No change since `wait` began.
    pub fn is_current(&self, wait: u64) -> bool {
        self.0.get() == wait
    }
}

/// What the dialog asks the app to do.
pub struct Actions {
    /// Read the link in the field: nothing is sent.
    pub read: Rc<dyn Fn(String)>,
    /// Ask the server what an invite's code is; nothing is used up.
    pub look_up: Rc<dyn Fn(Lookup)>,
    /// Connect to the server the read link names, for its fingerprint.
    pub connect: Rc<dyn Fn(String)>,
    /// Save the server only: a link without a code, or a refused one.
    pub save: Rc<dyn Fn(String)>,
    /// Make the account: the link, username, display name and password.
    pub join: Rc<dyn Fn(String, String, Option<String>, String)>,
    /// Sign in to the saved server, after the dialog closes.
    pub sign_in: Rc<dyn Fn(Server)>,
    /// The way without an invite: the server's key and an address.
    pub by_key: Rc<dyn Fn()>,
}

/// The dialog, kept by the app to show each answer.
pub struct InviteDialog {
    pub dialog: adw::Dialog,
    stack: gtk::Stack,
    link: gtk::TextView,
    link_error: gtk::Label,
    summary: gtk::Box,
    summary_name: gtk::Label,
    summary_where: gtk::Label,
    summary_terms: gtk::Label,
    summary_account: gtk::Label,
    /// What the server says the code is, once looked up.
    summary_outcome: gtk::Label,
    proceed: gtk::Button,
    /// Save server only, for a link whose code no longer works.
    save_instead: gtk::Button,
    check_who: gtk::Label,
    check_code: gtk::Label,
    check_site: gtk::Label,
    check_how: gtk::Label,
    connect: gtk::Button,
    join_strips: Vec<(gtk::Label, gtk::Label)>,
    join_terms: gtk::Label,
    username: gtk::Entry,
    username_error: gtk::Label,
    display_name: gtk::Entry,
    password: gtk::PasswordEntry,
    join_error: gtk::Label,
    create: gtk::Button,
    refusal_title: gtk::Label,
    refusal_body: gtk::Label,
    save_only: gtk::Button,
    saved_title: gtk::Label,
    sign_in: gtk::Button,
    /// The server on this network the dialog was opened for, whose code
    /// may be typed in.
    at: Option<Server>,
    look_up: Rc<dyn Fn(Lookup)>,
    debounce: Rc<Debounce>,
    /// The link read, what it says, and who answered, once known.
    state: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    /// The link in the field, or the one made from the code typed.
    link: String,
    /// A code typed in: its server came from this network, not the link.
    typed: bool,
    invite: Option<Invite>,
    looked_up: Looked,
    server: Option<Server>,
}

/// Where the lookup of the field's code is.
#[derive(Debug, Default)]
enum Looked {
    /// None to make: no code, or the server cannot look one up.
    #[default]
    Not,
    Waiting,
    Answered(InviteOutcome),
    /// It could not be asked; the join tells more.
    Failed(String),
}

impl InviteDialog {
    /// The dialog; `at`, a server on this network, takes its invite's
    /// code typed in as well as a link.
    pub fn new(actions: Actions, at: Option<Server>) -> Rc<Self> {
        let state = Rc::new(RefCell::new(State::default()));
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vhomogeneous(false)
            .build();

        // 1 · The link, and what it says.
        let lead = paragraph(&match &at {
            Some(server) => format!(
                "Type the code from your invite to {}, or paste the whole link.",
                server.name
            ),
            None => "Paste the link from your invite: an https://spoolframe.app link or a \
                     spoolframe:// one. Opening the link does the same."
                .to_owned(),
        });
        let link = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::Char)
            .accepts_tab(false)
            .height_request(88)
            .top_margin(10)
            .bottom_margin(10)
            .left_margin(12)
            .right_margin(12)
            .css_classes(["invite-link", "monospace"])
            .build();
        let link_error = error_label();
        let summary_name = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        let summary_where = caption();
        let summary_terms = caption();
        summary_terms.add_css_class("invite-terms");
        let summary_account = gtk::Label::builder()
            .label("Includes an account")
            .valign(gtk::Align::Center)
            .css_classes(["invite-badge", "caption-heading"])
            .build();
        let summary_outcome = caption();
        summary_outcome.set_visible(false);
        let summary = summary_card(
            &summary_name,
            &summary_where,
            &[&summary_terms, &summary_outcome],
            &summary_account,
        );
        summary.set_visible(false);
        let paste = action("Paste", Some(crate::icons::PASTE), &["secondary"]);
        let proceed = action("Continue", None, &["suggested-action"]);
        proceed.set_sensitive(false);
        let save_instead = gtk::Button::builder()
            .label("Save server only")
            .halign(gtk::Align::Center)
            .visible(false)
            .css_classes(["flat"])
            .build();
        let by_key = gtk::Button::builder()
            .label("Add by its key instead")
            .halign(gtk::Align::Center)
            .css_classes(["flat", "caption"])
            .build();
        let add = page_box();
        add.append(&lead);
        add.append(&field(
            if at.is_some() {
                "Invite code or link"
            } else {
                "Invite link"
            },
            &link_frame(&link),
        ));
        add.append(&link_error);
        add.append(&summary);
        add.append(&pair(&paste, &proceed));
        add.append(&save_instead);
        add.append(&by_key);
        stack.add_named(&add, Some("add"));

        // 2 · Is this the server you were invited to?
        let check_who = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build();
        let check_code = gtk::Label::builder()
            .selectable(true)
            .css_classes(["fingerprint", "monospace"])
            .build();
        let check_site = gtk::Label::builder()
            .css_classes(["monospace", "passkey-site", "dim-label"])
            .build();
        let code_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        code_column.append(&check_code);
        code_column.append(&check_site);
        let check_how = paragraph("");
        check_how.set_justify(gtk::Justification::Center);
        let back = action("Back", None, &["secondary"]);
        let connect = action("Connect", Some(crate::icons::CHECK), &["suggested-action"]);
        let check = page_box();
        check.append(&check_icon());
        check.append(
            &gtk::Label::builder()
                .label("Is this the server you were invited to?")
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["title-2", "dialog-title"])
                .build(),
        );
        check.append(&check_who);
        check.append(
            &adw::Bin::builder()
                .child(&code_column)
                .halign(gtk::Align::Center)
                .css_classes(["fingerprint-box"])
                .build(),
        );
        check.append(&check_how);
        check.append(&pair(&back, &connect));
        stack.add_named(&check, Some("check"));

        // 3 · The join form.
        let mut join_strips = Vec::new();
        let (strip, strip_name, strip_code) = server_strip();
        join_strips.push((strip_name, strip_code));
        let join_terms = caption();
        join_terms.add_css_class("invite-terms");
        let username = gtk::Entry::builder().css_classes(["field"]).build();
        let username_error = error_label();
        let display_name = gtk::Entry::builder().css_classes(["field"]).build();
        let password = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .css_classes(["field"])
            .build();
        let join_error = error_label();
        let join_back = action("Back", None, &["secondary"]);
        let create = action(
            "Create account",
            Some(crate::icons::JOIN),
            &["suggested-action"],
        );
        let join = page_box();
        join.append(&strip);
        join.append(&paragraph(
            "Your invite includes an account on this server. Choose how you sign in.",
        ));
        join.append(&join_terms);
        join.append(&hinted(
            "Username",
            &username,
            &username_error,
            "Letters, digits, dots and dashes. You sign in with it.",
        ));
        join.append(&hinted(
            "Display name (optional)",
            &display_name,
            &gtk::Label::new(None),
            "What others see on Who’s watching.",
        ));
        join.append(&hinted(
            "Password",
            &password,
            &gtk::Label::new(None),
            PASSWORD_HINT,
        ));
        join.append(&join_error);
        join.append(&pair(&join_back, &create));
        stack.add_named(&join, Some("join"));

        // 4 · A refused code: save the server only, or close.
        let (strip, strip_name, strip_code) = server_strip();
        join_strips.push((strip_name, strip_code));
        let refusal_title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        let refusal_body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["body"])
            .build();
        let refusal = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(["invite-refusal"])
            .build();
        refusal.append(
            &gtk::Image::builder()
                .icon_name(crate::icons::ERROR)
                .valign(gtk::Align::Start)
                .css_classes(["invite-refusal-icon"])
                .build(),
        );
        let refusal_text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        refusal_text.append(&refusal_title);
        refusal_text.append(&refusal_body);
        refusal.append(&refusal_text);
        let save_only = action("Save server only", None, &["suggested-action"]);
        let close = action("Close", None, &["secondary"]);
        let refused = page_box();
        refused.append(&refusal);
        refused.append(&strip);
        refused.append(&pair(&save_only, &close));
        stack.add_named(&refused, Some("refused"));

        // 5 · Saved: a link without a code, or Save server only.
        let saved_title = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["title-2", "dialog-title"])
            .build();
        let (strip, strip_name, strip_code) = server_strip();
        join_strips.push((strip_name, strip_code));
        let later = action("Later", None, &["secondary"]);
        let sign_in = action("Sign in", None, &["suggested-action"]);
        let saved = page_box();
        saved.append(&check_icon());
        saved.append(&saved_title);
        let saved_body = paragraph(SAVED_BODY);
        saved_body.set_justify(gtk::Justification::Center);
        saved.append(&saved_body);
        saved.append(&strip);
        saved.append(&pair(&later, &sign_in));
        stack.add_named(&saved, Some("saved"));

        let dialog = adw::Dialog::builder()
            .title(add_title(at.as_ref()))
            .content_width(520)
            .focus_widget(&link)
            .build();
        with_cancel(&dialog, &stack);

        let look_up = actions.look_up.clone();
        let this = Rc::new(Self {
            dialog,
            stack,
            link,
            link_error,
            summary,
            summary_name,
            summary_where,
            summary_terms,
            summary_account,
            summary_outcome,
            proceed,
            save_instead,
            check_who,
            check_code,
            check_site,
            check_how,
            connect,
            join_strips,
            join_terms,
            username,
            username_error,
            display_name,
            password,
            join_error,
            create,
            refusal_title,
            refusal_body,
            save_only,
            saved_title,
            sign_in,
            at,
            look_up,
            debounce: Rc::default(),
            state,
        });
        this.wire(actions, &paste, &by_key, &back, &join_back, &close, &later);
        this
    }

    #[allow(clippy::too_many_arguments)]
    fn wire(
        self: &Rc<Self>,
        actions: Actions,
        paste: &gtk::Button,
        by_key: &gtk::Button,
        back: &gtk::Button,
        join_back: &gtk::Button,
        close: &gtk::Button,
        later: &gtk::Button,
    ) {
        // Each change of the link is read again: reading sends nothing. A
        // code typed at the dialog's server is read as the link made from
        // it, and looked up once the field rests.
        {
            let (weak, read) = (Rc::downgrade(self), actions.read.clone());
            self.link.buffer().connect_changed(move |buffer| {
                let Some(this) = weak.upgrade() else { return };
                let text = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .trim()
                    .to_owned();
                this.debounce.change();
                this.state.replace(State {
                    link: text.clone(),
                    ..State::default()
                });
                this.summary.set_visible(false);
                this.link_error.set_visible(false);
                this.save_instead.set_visible(false);
                this.proceed.set_sensitive(false);
                if text.is_empty() {
                    return;
                }
                let typed = this.at.as_ref().and_then(|server| {
                    let code = invites::parse_code(&text).ok()?;
                    Some((server, code))
                });
                match typed {
                    Some((server, code)) => {
                        let link = invites::code_link(server, &code);
                        {
                            let mut state = this.state.borrow_mut();
                            state.link = link.clone();
                            state.typed = true;
                        }
                        this.wait_to_look_up(Lookup::Code {
                            link: link.clone(),
                            server: server.key.clone(),
                            addresses: server.addresses.clone(),
                            code,
                        });
                        read(link);
                    }
                    None => read(text),
                }
            });
        }
        {
            let (weak, save) = (Rc::downgrade(self), actions.save.clone());
            self.save_instead.connect_clicked(move |button| {
                let Some(this) = weak.upgrade() else { return };
                button.set_sensitive(false);
                save(this.state.borrow().link.clone());
            });
        }
        {
            let link = self.link.clone();
            paste.connect_clicked(move |_| {
                let buffer = link.buffer();
                link.clipboard()
                    .read_text_async(None::<&gtk::gio::Cancellable>, move |text| {
                        if let Ok(Some(text)) = text {
                            buffer.set_text(&text);
                        }
                    });
            });
        }
        {
            let (weak, connect) = (Rc::downgrade(self), actions.connect.clone());
            self.proceed.connect_clicked(move |button| {
                let Some(this) = weak.upgrade() else { return };
                button.set_sensitive(false);
                connect(this.state.borrow().link.clone());
            });
        }
        {
            let (dialog, open) = (self.dialog.downgrade(), actions.by_key.clone());
            by_key.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
                open();
            });
        }
        for button in [back, join_back] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.show_add();
                }
            });
        }
        // Connect: the join form for a link with a code; saving the server
        // for one without.
        {
            let (weak, save) = (Rc::downgrade(self), actions.save.clone());
            self.connect.connect_clicked(move |button| {
                let Some(this) = weak.upgrade() else { return };
                let has_code = this
                    .state
                    .borrow()
                    .invite
                    .as_ref()
                    .is_some_and(|i| i.has_code);
                if has_code {
                    this.show_join();
                } else {
                    button.set_sensitive(false);
                    save(this.state.borrow().link.clone());
                }
            });
        }
        {
            let (weak, join) = (Rc::downgrade(self), actions.join.clone());
            let send = move || {
                let Some(this) = weak.upgrade() else { return };
                let username = this.username.text().trim().to_owned();
                let display = this.display_name.text().trim().to_owned();
                let password = this.password.text().to_string();
                if username.is_empty() || password.is_empty() {
                    return this.refuse_join("Choose a username and a password.");
                }
                this.create.set_sensitive(false);
                this.username_error.set_visible(false);
                this.join_error.set_visible(false);
                join(
                    this.state.borrow().link.clone(),
                    username,
                    (!display.is_empty()).then_some(display),
                    password,
                );
            };
            let send_ = send.clone();
            self.create.connect_clicked(move |_| send_());
            self.password.connect_activate(move |_| send());
        }
        {
            let (weak, save) = (Rc::downgrade(self), actions.save.clone());
            self.save_only.connect_clicked(move |button| {
                let Some(this) = weak.upgrade() else { return };
                button.set_sensitive(false);
                save(this.state.borrow().link.clone());
            });
        }
        for button in [close, later] {
            let dialog = self.dialog.downgrade();
            button.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
        }
        {
            let (weak, sign_in) = (Rc::downgrade(self), actions.sign_in.clone());
            self.sign_in.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else { return };
                let server = this.state.borrow().server.clone();
                this.dialog.close();
                if let Some(server) = server {
                    sign_in(server);
                }
            });
        }
    }

    /// Puts `link` in the field, as opening one from the desktop does; it
    /// is read like a paste.
    pub fn set_link(&self, link: &str) {
        self.link.buffer().set_text(link);
    }

    /// What the link in the field says, or that it is not an invite.
    /// An answer for a link no longer in the field is dropped. A pasted
    /// link with a code is looked up once the field rests.
    pub fn read(self: &Rc<Self>, link: &str, answer: Result<Invite, BackendError>) {
        if self.state.borrow().link != link {
            return;
        }
        match answer {
            Ok(invite) => {
                let (name, place, terms) = summary_lines(&invite, SystemTime::now());
                self.summary_name.set_label(&name);
                self.summary_where.set_label(&place);
                self.summary_where.set_visible(!place.is_empty());
                self.summary_terms.set_label(&terms);
                self.summary_terms.set_visible(!terms.is_empty());
                self.summary_account.set_visible(invite.has_code);
                self.summary.set_visible(true);
                let typed = self.state.borrow().typed;
                if invite.has_code && !typed {
                    self.wait_to_look_up(Lookup::Link(link.to_owned()));
                }
                self.state.borrow_mut().invite = Some(invite);
                self.show_lookup();
            }
            Err(e) => {
                let text = paste_refusal(&e, link, self.at.is_some());
                self.link_error.set_label(&text);
                self.link_error.set_visible(true);
            }
        }
    }

    /// What the server says the field's code is. An answer for a link no
    /// longer in the field is dropped. A server that cannot say leaves
    /// the join as it was.
    pub fn looked_up(&self, link: &str, answer: Result<InviteLookup, BackendError>) {
        if self.state.borrow().link != link {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.looked_up = match answer {
                Ok(lookup) => {
                    // The server's terms, in place of the link's hints: a
                    // typed code has none, and the join form shows them.
                    if let (
                        InviteOutcome::Valid {
                            role, expires_at, ..
                        },
                        Some(invite),
                    ) = (&lookup.outcome, state.invite.as_mut())
                    {
                        invite.role = Some(*role);
                        invite.expires_at = Some(*expires_at);
                    }
                    Looked::Answered(lookup.outcome)
                }
                Err(BackendError::Unsupported(_)) => Looked::Not,
                Err(e) => Looked::Failed(crate::app::sentence(&e.to_string())),
            };
        }
        self.show_lookup();
    }

    /// Looks `lookup` up once the field has rested, unless it changed
    /// again by then.
    fn wait_to_look_up(self: &Rc<Self>, lookup: Lookup) {
        self.state.borrow_mut().looked_up = Looked::Waiting;
        let (weak, wait) = (Rc::downgrade(self), self.debounce.wait());
        gtk::glib::timeout_add_local_once(LOOKUP_PAUSE, move || {
            if let Some(this) = weak.upgrade()
                && this.debounce.is_current(wait)
            {
                (this.look_up)(lookup);
            }
        });
    }

    /// The summary's lookup line and Continue, as the lookup stands.
    fn show_lookup(&self) {
        let state = self.state.borrow();
        if state.invite.is_none() {
            return;
        }
        let (line, joins, refused) = match &state.looked_up {
            Looked::Not => (None, true, false),
            Looked::Waiting => (Some("Checking the invite…".to_owned()), false, false),
            Looked::Answered(outcome) => {
                let (line, joins) = outcome_line(outcome, SystemTime::now());
                (Some(line), joins, !joins)
            }
            Looked::Failed(why) => (Some(why.clone()), true, false),
        };
        self.summary_outcome.set_visible(line.is_some());
        self.summary_outcome
            .set_label(line.as_deref().unwrap_or_default());
        if refused {
            self.summary_outcome.add_css_class("error");
        } else {
            self.summary_outcome.remove_css_class("error");
        }
        self.proceed.set_sensitive(joins);
        // A link the server's admin sent pins its server, so it can still
        // be saved; a typed code's server was only heard on this network,
        // and its row connects to it.
        let saves = refused
            && !state.typed
            && matches!(
                state.looked_up,
                Looked::Answered(
                    InviteOutcome::Expired { .. } | InviteOutcome::Used | InviteOutcome::Revoked
                )
            );
        self.save_instead.set_visible(saves);
        self.save_instead.set_sensitive(true);
    }

    /// Who answered for the invite: its fingerprint to check.
    pub fn connected(&self, answer: Result<Server, BackendError>) {
        self.proceed.set_sensitive(true);
        let server = match answer {
            Ok(server) => server,
            Err(e) => {
                self.link_error
                    .set_label(&crate::app::sentence(&e.to_string()));
                self.link_error.set_visible(true);
                return;
            }
        };
        let (has_code, typed) = {
            let state = self.state.borrow();
            let invite = state.invite.as_ref();
            self.check_who
                .set_label(&who_line(&server, invite.map_or(&[][..], |i| &i.relays)));
            (invite.is_some_and(|i| i.has_code), state.typed)
        };
        self.check_code.set_label(&server.fingerprint);
        match server.passkey_site() {
            Some(site) => {
                self.check_site.set_label(&format!("Passkeys · {site}"));
                self.check_site.set_visible(true);
            }
            None => self.check_site.set_visible(false),
        }
        self.check_how.set_label(&check_text(has_code, typed));
        self.connect.set_sensitive(true);
        for (name, code) in &self.join_strips {
            name.set_label(&server.name);
            code.set_label(&server.fingerprint);
        }
        self.state.borrow_mut().server = Some(server);
        self.stack.set_visible_child_name("check");
    }

    /// The account was refused. A taken name or a bad password goes back
    /// to the form; a code that no longer works offers Save server only.
    pub fn join_refused(&self, error: &BackendError, username: &str) {
        self.create.set_sensitive(true);
        match error {
            BackendError::UsernameTaken => {
                self.username_error
                    .set_label(&format!("{username} is taken. Try another."));
                self.username_error.set_visible(true);
                self.username.grab_focus();
            }
            BackendError::InviteInvalid
            | BackendError::InviteExpired
            | BackendError::InviteExhausted => {
                let (title, body) = code_refusal(error);
                self.refusal_title.set_label(title);
                self.refusal_body.set_label(body);
                self.save_only.set_sensitive(true);
                self.stack.set_visible_child_name("refused");
            }
            other => self.refuse_join(&crate::app::sentence(&other.to_string())),
        }
    }

    /// The server is saved: a link without a code, or Save server only.
    pub fn saved(&self, answer: Result<Server, BackendError>) {
        self.connect.set_sensitive(true);
        self.save_only.set_sensitive(true);
        match answer {
            Ok(server) => {
                self.dialog.set_title(&format!("{} added", server.name));
                self.saved_title
                    .set_label(&format!("{} is saved", server.name));
                self.state.borrow_mut().server = Some(server);
                self.stack.set_visible_child_name("saved");
            }
            Err(e) => {
                let text = crate::app::sentence(&e.to_string());
                self.save_instead.set_sensitive(true);
                match self.stack.visible_child_name().as_deref() {
                    Some("refused") => self.refusal_body.set_label(&text),
                    Some("add") => {
                        self.link_error.set_label(&text);
                        self.link_error.set_visible(true);
                    }
                    _ => {
                        self.check_how.set_label(&text);
                    }
                }
            }
        }
    }

    /// Who answered for the invite, once connected.
    pub fn server(&self) -> Option<Server> {
        self.state.borrow().server.clone()
    }

    fn show_add(&self) {
        self.dialog.set_title(add_title(self.at.as_ref()));
        self.stack.set_visible_child_name("add");
    }

    fn show_join(&self) {
        let state = self.state.borrow();
        if let Some(server) = &state.server {
            self.dialog.set_title(&format!("Join {}", server.name));
        }
        let terms = state
            .invite
            .as_ref()
            .map(|i| terms_line(i, SystemTime::now()))
            .unwrap_or_default();
        self.join_terms.set_label(&terms);
        self.join_terms.set_visible(!terms.is_empty());
        self.stack.set_visible_child_name("join");
        self.username.grab_focus();
    }

    fn refuse_join(&self, text: &str) {
        self.create.set_sensitive(true);
        self.join_error.set_label(text);
        self.join_error.set_visible(true);
    }

    /// Which page shows: add, check, join, refused or saved.
    #[cfg(test)]
    fn page(&self) -> String {
        self.stack
            .visible_child_name()
            .map(|n| n.to_string())
            .unwrap_or_default()
    }
}

/// The password's hint. The server takes 8 characters or more (the
/// boards' 10 is not what it checks).
const PASSWORD_HINT: &str = "At least 8 characters.";

const SAVED_BODY: &str = "This invite finds the server but makes no account. Sign in with your \
                          account, or ask an admin for an invite with one.";

/// The dialog's title on its first page.
fn add_title(at: Option<&Server>) -> &'static str {
    match at {
        Some(_) => "Use an invite",
        None => "Add a server",
    }
}

/// Under the link: why it is not an invite.
fn link_refusal(error: &BackendError) -> String {
    match error {
        BackendError::NotAnInvite => {
            "This isn’t a Spoolframe invite. Copy the whole link and try again.".to_owned()
        }
        BackendError::NotAnInviteCode => {
            "That isn’t an invite code. A code is 12 letters and digits, like QW7K-2MFP-XR4T."
                .to_owned()
        }
        other => crate::app::sentence(&other.to_string()),
    }
}

/// Under the field, for `text` that did not read as a link: opened for a
/// server, the field takes a code too, so it is neither; else a code
/// alone, with no server to ask, is told where it goes.
fn paste_refusal(error: &BackendError, text: &str, takes_codes: bool) -> String {
    match error {
        BackendError::NotAnInvite if takes_codes => link_refusal(&BackendError::NotAnInviteCode),
        BackendError::NotAnInvite if invites::parse_code(text).is_ok() => {
            "That’s an invite’s code, not its link. Paste the whole link, or choose Use invite on \
             the server’s row under On this network."
                .to_owned()
        }
        other => link_refusal(other),
    }
}

/// What the server says a code is, as the summary says it, and whether
/// Continue goes on to the join: "Invite from Andrey · consumer account
/// · expires Oct 14", or why it does not. An outcome this build does not
/// know may still work, so it may be tried.
fn outcome_line(outcome: &InviteOutcome, now: SystemTime) -> (String, bool) {
    match outcome {
        InviteOutcome::Valid {
            kind,
            role,
            expires_at,
            inviter,
        } => {
            let mut parts = vec![match inviter {
                Some(name) => format!("Invite from {name}"),
                None => "A working invite".to_owned(),
            }];
            match (kind, role) {
                (InviteKind::CreateUser, Role::Admin) => parts.push("admin account".to_owned()),
                (InviteKind::CreateUser, _) => parts.push("consumer account".to_owned()),
                (InviteKind::PortableAccount, _) => {
                    parts.push("access for your identity".to_owned())
                }
                _ => {}
            }
            parts.push(if *expires_at <= now {
                "expires now".to_owned()
            } else {
                format!("expires {}", month_day(*expires_at))
            });
            (parts.join(" · "), true)
        }
        InviteOutcome::Expired { at } => (
            format!("This invite expired {}. Ask for a new one.", month_day(*at)),
            false,
        ),
        InviteOutcome::Used => (
            "This invite has been used. Ask for a new one.".to_owned(),
            false,
        ),
        InviteOutcome::Revoked => (
            "This invite was revoked. Ask for a new one.".to_owned(),
            false,
        ),
        InviteOutcome::NoMatch => (
            "No invite has this code. Check it, or ask for a new invite.".to_owned(),
            false,
        ),
        InviteOutcome::Unknown => (
            "The server didn’t say whether this invite works. You can still try it.".to_owned(),
            true,
        ),
    }
}

/// A refused code's heading and what to do, as the states board says it.
fn code_refusal(error: &BackendError) -> (&'static str, &'static str) {
    match error {
        BackendError::InviteExpired => (
            "This invite has expired",
            "Invites last a few days. Ask whoever sent it for a new one, or save the server \
             and sign in.",
        ),
        BackendError::InviteExhausted => (
            "This invite has been used",
            "Every account it could make is made. Ask for a new invite, or save the server \
             and sign in.",
        ),
        _ => (
            "This invite doesn’t work",
            "Its code is wrong or was revoked. Ask for a new invite, or save the server and \
             sign in with an account you have.",
        ),
    }
}

/// The summary card's lines: the server's name, where the link says it
/// is ("1 address · relay.home-media.example"), and its terms ("Joins as
/// a consumer · expires Oct 14").
fn summary_lines(invite: &Invite, now: SystemTime) -> (String, String, String) {
    let name = invite
        .name
        .clone()
        .unwrap_or_else(|| format!("Server {}", invite.key.short()));
    let mut place = Vec::new();
    match invite.addresses.len() {
        0 => {}
        1 => place.push("1 address".to_owned()),
        n => place.push(format!("{n} addresses")),
    }
    place.extend(invite.relays.iter().map(|r| relay_host(r).to_owned()));
    (name, place.join(" · "), terms_line(invite, now))
}

/// "Joins as a consumer · expires Oct 14": what the link says of the
/// account; nothing for a link without a code.
fn terms_line(invite: &Invite, now: SystemTime) -> String {
    if !invite.has_code {
        return String::new();
    }
    let mut parts = Vec::new();
    match invite.role {
        Some(Role::Admin) => parts.push("Joins as an admin".to_owned()),
        Some(Role::Consumer) => parts.push("Joins as a consumer".to_owned()),
        _ => {}
    }
    if let Some(at) = invite.expires_at {
        parts.push(if at <= now {
            "expired".to_owned()
        } else {
            format!("expires {}", month_day(at))
        });
    }
    let line = parts.join(" · ");
    let mut chars = line.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => line,
    }
}

/// Now, in unix seconds.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// "Oct 14", in local time.
pub fn month_day(at: SystemTime) -> String {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    gtk::glib::DateTime::from_unix_local(secs)
        .ok()
        .and_then(|t| t.format("%b %-d").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// `https://relay.home-media.example/` as `relay.home-media.example`.
fn relay_host(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// "Home Media · through relay.home-media.example": the name, and the
/// address it answered at, or the relay the link names.
fn who_line(server: &Server, relays: &[String]) -> String {
    match (server.shown_address(), relays.first()) {
        (Some(address), _) => format!("{} · {}", server.name, address),
        (None, Some(relay)) => format!("{} · through {}", server.name, relay_host(relay)),
        (None, None) => server.name.clone(),
    }
}

/// Under the fingerprint: why it can be trusted, and what comes next. A
/// code typed in names no key: the server was heard on this network.
fn check_text(has_code: bool, typed: bool) -> String {
    let mut text = match typed {
        true => {
            "This server was found on this network, and your code names no key. Ask \
                 whoever invited you to read the code from their server, to be sure it is \
                 theirs."
        }
        false => {
            "The invite names this key, so Spoolframe connects to nothing else. To be \
                  sure, ask whoever invited you to read the code from their server."
        }
    }
    .to_owned();
    if has_code {
        text.push_str(" Then you choose a username and password.");
    }
    text
}

fn page_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(22)
        .margin_bottom(22)
        .margin_start(26)
        .margin_end(26)
        .build()
}

fn paragraph(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["body", "dim-label"])
        .build()
}

fn caption() -> gtk::Label {
    gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["caption", "dim-label"])
        .build()
}

fn error_label() -> gtk::Label {
    gtk::Label::builder()
        .visible(false)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["error", "caption"])
        .build()
}

fn link_frame(link: &gtk::TextView) -> gtk::Frame {
    gtk::Frame::builder()
        .child(link)
        .css_classes(["invite-link-frame"])
        .build()
}

/// Two buttons side by side, equal widths.
fn pair(start: &gtk::Button, end: &gtk::Button) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(10)
        .homogeneous(true)
        .margin_top(8)
        .build();
    row.append(start);
    row.append(end);
    row
}

fn check_icon() -> gtk::Image {
    gtk::Image::builder()
        .icon_name(crate::icons::VERIFIED)
        .pixel_size(30)
        .width_request(56)
        .height_request(56)
        .halign(gtk::Align::Center)
        .css_classes(["check-icon"])
        .build()
}

/// The server, its name and fingerprint in a band, over a form.
fn server_strip() -> (gtk::Box, gtk::Label, gtk::Label) {
    let strip = gtk::Box::builder()
        .spacing(10)
        .css_classes(["invite-server"])
        .build();
    strip.append(
        &gtk::Image::builder()
            .icon_name(crate::icons::VERIFIED)
            .css_classes(["invite-server-icon"])
            .build(),
    );
    let name = gtk::Label::builder().css_classes(["heading"]).build();
    let code = gtk::Label::builder()
        .css_classes(["monospace", "caption", "dim-label"])
        .build();
    strip.append(&name);
    strip.append(&code);
    (strip, name, code)
}

/// The summary card: a server tile, the link's lines, and whether it
/// makes an account.
fn summary_card(
    name: &gtk::Label,
    place: &gtk::Label,
    terms: &[&gtk::Label],
    account: &gtk::Label,
) -> gtk::Box {
    let card = gtk::Box::builder()
        .spacing(14)
        .css_classes(["invite-summary"])
        .build();
    card.append(
        &gtk::Image::builder()
            .icon_name(crate::icons::SERVER)
            .width_request(40)
            .height_request(40)
            .valign(gtk::Align::Center)
            .css_classes(["server-tile", "tint-accent"])
            .build(),
    );
    let lines = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .hexpand(true)
        .build();
    lines.append(name);
    lines.append(place);
    for line in terms {
        lines.append(*line);
    }
    card.append(&lines);
    card.append(account);
    card
}

/// A field with its hint under it, and room for its own error.
fn hinted(label: &str, entry: &impl IsA<gtk::Widget>, error: &gtk::Label, hint: &str) -> gtk::Box {
    let column = field(label, entry);
    column.append(error);
    let hint_label = caption();
    hint_label.set_label(hint);
    column.append(&hint_label);
    column
}

#[cfg(test)]
mod tests {
    use super::*;
    use spoolframe_linux_core::backend::{Client, ServerKey};
    use spoolframe_linux_core::fake::{
        EXPIRED_INVITE_CODE, FakeClient, INVITE_CODE, REVOKED_INVITE_CODE,
    };
    use std::time::Duration;

    fn invite(has_code: bool) -> Invite {
        Invite {
            key: ServerKey("c4b1".repeat(16)),
            fingerprint: "K6EQ-ZXKC-HT6H-AJ4F".into(),
            name: Some("Home Media".into()),
            addresses: vec!["203.0.113.24:41641".into()],
            relays: vec!["https://relay.home-media.example".into()],
            has_code,
            expires_at: Some(SystemTime::now() + Duration::from_secs(6 * 24 * 3600)),
            role: Some(Role::Consumer),
            saved: false,
        }
    }

    #[test]
    fn the_summary_says_where_the_server_is_and_what_the_account_is() {
        let (name, place, terms) = summary_lines(&invite(true), SystemTime::now());
        assert_eq!(name, "Home Media");
        assert_eq!(place, "1 address · relay.home-media.example");
        assert!(
            terms.starts_with("Joins as a consumer · expires "),
            "{terms}"
        );
    }

    #[test]
    fn a_link_without_a_code_promises_no_account() {
        let (_, _, terms) = summary_lines(&invite(false), SystemTime::now());
        assert_eq!(terms, "");
        assert!(!check_text(false, false).contains("username"));
        assert!(check_text(true, false).ends_with("Then you choose a username and password."));
    }

    #[test]
    fn a_typed_code_does_not_claim_to_name_the_key() {
        assert!(check_text(true, true).starts_with("This server was found on this network"));
    }

    #[test]
    fn a_paste_that_is_not_an_invite_says_so() {
        assert_eq!(
            paste_refusal(&BackendError::NotAnInvite, "hello", false),
            "This isn’t a Spoolframe invite. Copy the whole link and try again."
        );
    }

    #[test]
    fn opened_for_a_server_text_that_is_neither_says_it_is_not_a_code() {
        assert_eq!(
            paste_refusal(&BackendError::NotAnInvite, "hello", true),
            "That isn’t an invite code. A code is 12 letters and digits, like QW7K-2MFP-XR4T."
        );
    }

    #[test]
    fn the_sdks_not_an_invite_code_says_what_a_code_is() {
        assert_eq!(
            link_refusal(&BackendError::NotAnInviteCode),
            "That isn’t an invite code. A code is 12 letters and digits, like QW7K-2MFP-XR4T."
        );
    }

    #[test]
    fn a_code_pasted_with_no_server_to_ask_is_told_where_it_goes() {
        assert!(
            paste_refusal(&BackendError::NotAnInvite, "qw7k-2mfp-xr4t", false)
                .starts_with("That’s an invite’s code, not its link.")
        );
    }

    fn valid(inviter: Option<&str>, kind: InviteKind, role: Role) -> InviteOutcome {
        InviteOutcome::Valid {
            kind,
            role,
            expires_at: SystemTime::now() + Duration::from_secs(6 * 24 * 3600),
            inviter: inviter.map(str::to_owned),
        }
    }

    #[test]
    fn a_working_invite_says_whom_from_what_it_makes_and_until_when() {
        let (line, joins) = outcome_line(
            &valid(Some("Andrey"), InviteKind::CreateUser, Role::Consumer),
            SystemTime::now(),
        );
        assert!(
            line.starts_with("Invite from Andrey · consumer account · expires "),
            "{line}"
        );
        assert!(joins);
    }

    #[test]
    fn a_working_invite_with_no_inviter_says_what_it_makes() {
        let (line, _) = outcome_line(
            &valid(None, InviteKind::PortableAccount, Role::Consumer),
            SystemTime::now(),
        );
        assert!(
            line.starts_with("A working invite · access for your identity · expires "),
            "{line}"
        );
    }

    #[test]
    fn an_admins_invite_says_so() {
        let (line, _) = outcome_line(
            &valid(Some("Andrey"), InviteKind::CreateUser, Role::Admin),
            SystemTime::now(),
        );
        assert!(line.contains(" · admin account · "), "{line}");
    }

    #[test]
    fn a_code_that_no_longer_works_does_not_join() {
        let joins: Vec<bool> = [
            InviteOutcome::Expired {
                at: SystemTime::now() - Duration::from_secs(3600),
            },
            InviteOutcome::Used,
            InviteOutcome::Revoked,
            InviteOutcome::NoMatch,
        ]
        .iter()
        .map(|o| outcome_line(o, SystemTime::now()).1)
        .collect();
        assert_eq!(joins, [false, false, false, false]);
    }

    #[test]
    fn each_refusal_has_its_own_words() {
        let lines: Vec<String> = [
            InviteOutcome::Used,
            InviteOutcome::Revoked,
            InviteOutcome::NoMatch,
        ]
        .iter()
        .map(|o| outcome_line(o, SystemTime::now()).0)
        .collect();
        assert_eq!(
            lines,
            [
                "This invite has been used. Ask for a new one.",
                "This invite was revoked. Ask for a new one.",
                "No invite has this code. Check it, or ask for a new invite."
            ]
        );
    }

    #[test]
    fn an_outcome_this_build_does_not_know_may_still_be_tried() {
        assert_eq!(
            outcome_line(&InviteOutcome::Unknown, SystemTime::now()),
            (
                "The server didn’t say whether this invite works. You can still try it.".to_owned(),
                true
            )
        );
    }

    #[test]
    fn a_change_makes_the_wait_before_it_stale() {
        let debounce = Debounce::default();
        let first = debounce.wait();
        debounce.change();
        let second = debounce.wait();
        assert_eq!(
            (debounce.is_current(first), debounce.is_current(second)),
            (false, true)
        );
    }

    #[test]
    fn each_refused_code_has_its_own_heading() {
        let headings: Vec<_> = [
            BackendError::InviteInvalid,
            BackendError::InviteExpired,
            BackendError::InviteExhausted,
        ]
        .iter()
        .map(|e| code_refusal(e).0)
        .collect();
        assert_eq!(
            headings,
            [
                "This invite doesn’t work",
                "This invite has expired",
                "This invite has been used"
            ]
        );
    }

    /// What the dialog's buttons asked for, in order.
    #[derive(Default)]
    struct Asked(RefCell<Vec<String>>);

    fn dialog(asked: &Rc<Asked>) -> Rc<InviteDialog> {
        dialog_at(asked, None)
    }

    fn dialog_at(asked: &Rc<Asked>, at: Option<Server>) -> Rc<InviteDialog> {
        let note = |asked: &Rc<Asked>, what: &'static str| {
            let asked = asked.clone();
            move || asked.0.borrow_mut().push(what.to_owned())
        };
        let (read, look_up, connect, save, join, sign_in, by_key) = (
            note(asked, "read"),
            note(asked, "look up"),
            note(asked, "connect"),
            note(asked, "save"),
            note(asked, "join"),
            note(asked, "sign in"),
            note(asked, "by key"),
        );
        InviteDialog::new(
            Actions {
                read: Rc::new(move |_| read()),
                look_up: Rc::new(move |_| look_up()),
                connect: Rc::new(move |_| connect()),
                save: Rc::new(move |_| save()),
                join: Rc::new(move |_, _, _, _| join()),
                sign_in: Rc::new(move |_| sign_in()),
                by_key: Rc::new(by_key),
            },
            at,
        )
    }

    fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
        gtk::glib::MainContext::default().block_on(future)
    }

    /// The flow through each state of the boards, with the fake's
    /// answers: a bad paste, the summary, the fingerprint, a taken name,
    /// an expired code and Save server only, and the saved server.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_invite_dialog_walks_the_boards_states() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client = FakeClient::new(None);
        let asked = Rc::new(Asked::default());
        let dialog = dialog(&asked);

        // 1 · Not an invite.
        let pasted = "https://home-media.example/admin/invites";
        dialog.set_link(pasted);
        dialog.read(pasted, ready(client.read_invite(pasted)));
        assert!(dialog.link_error.is_visible());
        assert!(!dialog.proceed.is_sensitive());

        // The summary, then who answers.
        let link = FakeClient::invite_link(Some(INVITE_CODE));
        dialog.set_link(&link);
        dialog.read(&link, ready(client.read_invite(&link)));
        assert!(dialog.summary.is_visible() && dialog.summary_account.is_visible());
        assert_eq!(dialog.summary_name.label(), "Cabin");
        // Continue waits for the server to say what the code is.
        assert_eq!(
            (
                dialog.summary_outcome.label().as_str(),
                dialog.proceed.is_sensitive()
            ),
            ("Checking the invite…", false)
        );
        dialog.looked_up(&link, ready(client.look_up_invite(&link)));
        assert!(
            dialog
                .summary_outcome
                .label()
                .starts_with("Invite from Andrey · consumer account · expires ")
        );
        assert!(dialog.proceed.is_sensitive());
        dialog.proceed.emit_clicked();
        dialog.connected(ready(client.connect_invite(&link)));
        assert_eq!(dialog.page(), "check");
        assert!(dialog.check_site.is_visible());

        // The form; a taken name stays on it.
        dialog.connect.emit_clicked();
        assert_eq!(dialog.page(), "join");
        assert_eq!(dialog.dialog.title(), "Join Cabin");
        let taken = ready(client.redeem_invite(&link, "alice", None, "long enough"));
        dialog.join_refused(&taken.unwrap_err(), "alice");
        assert_eq!(dialog.page(), "join");
        assert_eq!(
            dialog.username_error.label(),
            "alice is taken. Try another."
        );

        // An expired code: Save server only, then saved.
        let expired = FakeClient::invite_link(Some(EXPIRED_INVITE_CODE));
        let refused = ready(client.redeem_invite(&expired, "mum", None, "long enough"));
        dialog.join_refused(&refused.unwrap_err(), "mum");
        assert_eq!(dialog.page(), "refused");
        assert_eq!(dialog.refusal_title.label(), "This invite has expired");
        dialog.save_only.emit_clicked();
        dialog.saved(ready(client.save_invite_server(&expired)));
        assert_eq!(dialog.page(), "saved");
        assert_eq!(dialog.saved_title.label(), "Cabin is saved");
        assert_eq!(dialog.dialog.title(), "Cabin added");
        dialog.sign_in.emit_clicked();
        assert_eq!(
            without_lookups(&asked),
            ["read", "read", "connect", "save", "sign in"]
        );
    }

    /// What was asked, but the lookups, which go when the field has
    /// rested, whenever the main loop runs.
    fn without_lookups(asked: &Asked) -> Vec<String> {
        let asked = asked.0.borrow();
        asked.iter().filter(|a| *a != "look up").cloned().collect()
    }

    /// Opened for a server on this network: a code typed in is looked
    /// up there, a revoked one stops the join with no Save server only,
    /// and text that cannot be a code says so.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_code_typed_at_a_server_is_looked_up_before_joining() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client = FakeClient::new(None).with_cabin_nearby();
        let cabin = ready(client.servers())
            .unwrap()
            .into_iter()
            .find(|s| s.name == "Cabin")
            .unwrap();
        let asked = Rc::new(Asked::default());
        let dialog = dialog_at(&asked, Some(cabin.clone()));
        assert_eq!(dialog.dialog.title(), "Use an invite");

        let look = |typed: &str| {
            dialog.set_link(typed);
            let link = dialog.state.borrow().link.clone();
            dialog.read(&link, ready(client.read_invite(&link)));
            dialog.looked_up(
                &link,
                ready(client.look_up_invite_code(&cabin.key, &cabin.addresses, typed)),
            );
        };
        look("qw7k-2mfp-xr4t");
        assert_eq!(dialog.summary_name.label(), "Cabin");
        assert!(
            dialog
                .summary_outcome
                .label()
                .starts_with("Invite from Andrey")
        );
        assert!(dialog.proceed.is_sensitive());

        look(REVOKED_INVITE_CODE);
        assert_eq!(
            (
                dialog.summary_outcome.label().as_str(),
                dialog.proceed.is_sensitive(),
                dialog.save_instead.is_visible()
            ),
            ("This invite was revoked. Ask for a new one.", false, false)
        );

        dialog.set_link("hello");
        dialog.read("hello", ready(client.read_invite("hello")));
        assert_eq!(
            dialog.link_error.label(),
            "That isn’t an invite code. A code is 12 letters and digits, like QW7K-2MFP-XR4T."
        );
    }
}
