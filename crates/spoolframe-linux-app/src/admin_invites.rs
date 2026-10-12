//! Settings › a server › Invites (PX-44), as on the LinuxPrefsInvites and
//! LinuxInviteMint boards: an admin makes invites from the app, as from
//! the web admin, lists the server's working ones and revokes them, and
//! copies a link that only finds the server. Making or revoking one needs
//! a recent sign-in; when the server asks, "Confirm it's you" runs and the
//! action is tried again. A session that is no longer the admin's loses
//! the tab.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{
    BackendError, BoxFuture, InviteKind, InviteOrigin, InviteSpec, InviteSummary, MintedInvite,
    Server, Session,
};
use spoolframe_types::{Platform, Role};

/// Every kind, in the picker's order, with whether the server builds it
/// yet: federation arrives with P8.
const KINDS: [(InviteKind, bool); 3] = [
    (InviteKind::CreateUser, true),
    (InviteKind::PortableAccount, true),
    (InviteKind::Federation, false),
];

/// How long a new invite works for, in the picker's order; a week first.
const EXPIRIES: [(&str, Duration); 4] = [
    ("7 days", Duration::from_secs(7 * 24 * 3600)),
    ("1 day", Duration::from_secs(24 * 3600)),
    ("30 days", Duration::from_secs(30 * 24 * 3600)),
    ("1 hour", Duration::from_secs(3600)),
];

/// The roles a new account may get, as the web admin names them.
const ROLES: [(Role, &str); 2] = [(Role::Consumer, "Consumer"), (Role::Admin, "Admin")];

/// The most uses the form offers.
const MAX_USES: u32 = 50;

/// Under a day left: the row says "soon".
const SOON: Duration = Duration::from_secs(24 * 3600);

/// The kind as the New invite cards name it, and what it does.
pub fn kind_card(kind: InviteKind, server: &str) -> (&'static str, String) {
    match kind {
        InviteKind::CreateUser => ("Someone new", format!("Makes an account on {server}.")),
        InviteKind::PortableAccount => (
            "Someone with their own identity",
            "They watch as themselves, with no account here.".to_owned(),
        ),
        InviteKind::Federation => ("Another server", "Pairs a friend’s server.".to_owned()),
        InviteKind::Unknown => ("An invite", "A kind this app doesn’t know yet.".to_owned()),
    }
}

/// The kind's chip ("Account", teal), its CSS class and its icon.
pub fn kind_chip(kind: InviteKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        InviteKind::CreateUser => ("Account", "invite-account", crate::icons::JOIN),
        InviteKind::PortableAccount => ("Identity", "invite-identity", crate::icons::IDENTITY),
        InviteKind::Federation => ("Server", "invite-server", crate::icons::SERVER),
        InviteKind::Unknown => ("Invite", "dim-label", crate::icons::INVITES),
    }
}

/// "3 of 4 left"; "Any number of uses" without a limit.
pub fn uses_left(uses: Option<u32>, used: u32) -> String {
    match uses {
        Some(uses) => format!("{} of {uses} left", uses.saturating_sub(used)),
        None => "Any number of uses".to_owned(),
    }
}

/// "expires in 2 days", "expires in 23 hours", with whether that is soon.
pub fn expires_in(at: SystemTime, now: SystemTime) -> (String, bool) {
    let left = at.duration_since(now).unwrap_or_default();
    let secs = left.as_secs();
    let (n, unit) = if secs >= 2 * 24 * 3600 {
        (secs / (24 * 3600), "day")
    } else if secs >= 2 * 3600 {
        (secs / 3600, "hour")
    } else {
        ((secs / 60).max(1), "minute")
    };
    let text = match n {
        1 => format!("expires in 1 {unit}"),
        n => format!("expires in {n} {unit}s"),
    };
    (text, left < SOON)
}

/// "by Andrey on iPhone", "in the web admin", "on this computer".
pub fn made_where(by: &str, from: &InviteOrigin) -> String {
    let place = match from {
        InviteOrigin::WebAdmin => "in the web admin".to_owned(),
        InviteOrigin::Cli => "from the command line".to_owned(),
        InviteOrigin::Unknown => "elsewhere".to_owned(),
        InviteOrigin::Device {
            this_device: true, ..
        } => "on this computer".to_owned(),
        InviteOrigin::Device {
            name: Some(name), ..
        } if !name.trim().is_empty() => format!("on {name}"),
        InviteOrigin::Device { platform, .. } => match platform {
            Platform::Ios => "on an iPhone or iPad".to_owned(),
            Platform::Macos => "on a Mac".to_owned(),
            Platform::Tvos => "on an Apple TV".to_owned(),
            Platform::Linux => "on a Linux computer".to_owned(),
            _ => "on another device".to_owned(),
        },
    };
    format!("by {by} {place}")
}

/// A list row's title (the note, else the kind's card name) and its line:
/// "3 of 4 left · expires in 2 days · by Andrey on iPhone".
pub fn row_lines(invite: &InviteSummary, now: SystemTime) -> (String, String) {
    let mut parts = vec![uses_left(invite.uses, invite.used)];
    if let Some(at) = invite.expires_at {
        let (text, soon) = expires_in(at, now);
        parts.push(text);
        if soon {
            parts.push("soon".to_owned());
        }
    }
    parts.push(made_where(&invite.created_by, &invite.created_from));
    (
        title(invite.note.as_deref(), invite.kind),
        parts.join(" · "),
    )
}

/// An invite's name: its note, else what its kind is for.
fn title(note: Option<&str>, kind: InviteKind) -> String {
    note.map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| kind_card(kind, "").0.to_owned(), str::to_owned)
}

/// Whether an invite still works: not revoked, expired or used up. Only
/// those are listed; the others drop off, as in the web admin.
pub fn working(invite: &InviteSummary, now: SystemTime) -> bool {
    !invite.revoked
        && invite.expires_at.is_none_or(|at| at > now)
        && invite.uses.is_none_or(|uses| invite.used < uses)
}

/// The Revoke dialog's body: what stops, and what stays.
pub fn revoke_body(invite: &InviteSummary) -> String {
    let stops = match invite.uses.map(|uses| uses.saturating_sub(invite.used)) {
        Some(1) => "The one use left stops working now.".to_owned(),
        Some(n) => format!("The {n} uses left stop working now."),
        None => "It stops working now.".to_owned(),
    };
    let stays = match (invite.kind, invite.used) {
        (_, 0) => None,
        (InviteKind::CreateUser, 1) => Some("The account made with it stays.".to_owned()),
        (InviteKind::CreateUser, n) => Some(format!("The {n} accounts made with it stay.")),
        (InviteKind::PortableAccount, 1) => Some("The person who joined with it stays.".to_owned()),
        (InviteKind::PortableAccount, n) => {
            Some(format!("The {n} people who joined with it stay."))
        }
        (InviteKind::Federation, _) => Some("The pairing made with it stays.".to_owned()),
        (InviteKind::Unknown, _) => Some("What was made with it stays.".to_owned()),
    };
    match stays {
        Some(stays) => format!("{stops} {stays}"),
        None => stops,
    }
}

/// The made invite's line under its name: "Book club · 4 uses · 7 days".
pub fn made_line(note: &str, kind: InviteKind, uses: Option<u32>, expiry: &str) -> String {
    let mut parts = vec![title(Some(note), kind)];
    if let Some(uses) = uses {
        parts.push(match uses {
            1 => "1 use".to_owned(),
            n => format!("{n} uses"),
        });
    }
    parts.push(expiry.to_owned());
    parts.join(" · ")
}

/// What to do with the invite made, by its kind.
pub fn guidance(kind: InviteKind) -> &'static str {
    match kind {
        InviteKind::CreateUser => {
            "Send it to them. Their app scans the code or opens the link, and they choose a \
             username and password."
        }
        InviteKind::PortableAccount => {
            "Send it to them. Their app scans the code or opens the link, and they come in as \
             themselves with their own identity."
        }
        InviteKind::Federation => {
            "For the other server’s admin: they scan it with Spoolframe on an iPhone or iPad, or \
             paste the link in their admin UI under Federation."
        }
        InviteKind::Unknown => "Send it to them. Their app scans the code or opens the link.",
    }
}

/// A link that only finds `server`, with no code: anyone with an account
/// there signs in with it.
pub fn share_link(server: &Server) -> String {
    spoolframe_protocol::invite::Invite {
        server: server.key.0.clone(),
        name: Some(server.name.clone()),
        addrs: server
            .advertised
            .iter()
            .filter_map(|a| a.parse().ok())
            .collect(),
        relays: server.relays.clone(),
        ..Default::default()
    }
    .link()
}

/// What the New invite form holds, by the pickers' positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub kind: usize,
    pub note: String,
    pub uses: u32,
    pub expiry: usize,
    pub role: usize,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            kind: 0,
            note: String::new(),
            uses: 1,
            expiry: 0,
            role: 0,
        }
    }
}

impl Form {
    /// The invite to ask for. A role only for an account; an empty note is
    /// none.
    pub fn spec(&self) -> InviteSpec {
        let kind = KINDS.get(self.kind).unwrap_or(&KINDS[0]).0;
        let note = self.note.trim();
        InviteSpec {
            kind,
            note: (!note.is_empty()).then(|| note.to_owned()),
            uses: Some(self.uses.clamp(1, MAX_USES)),
            expires_in: Some(EXPIRIES.get(self.expiry).unwrap_or(&EXPIRIES[0]).1),
            role: (kind == InviteKind::CreateUser)
                .then(|| ROLES.get(self.role).unwrap_or(&ROLES[0]).0),
        }
    }

    fn expiry_name(&self) -> &'static str {
        EXPIRIES.get(self.expiry).unwrap_or(&EXPIRIES[0]).0
    }
}

/// The lead of "Confirm it's you" for an invite.
fn confirm_lead(server: &str) -> String {
    format!(
        "An invite lets someone into {server}, so confirm it’s you. You won’t be asked again \
         for a few minutes."
    )
}

/// What a refused invite request says; AdminOnly has its own dialog.
fn refusal(error: &BackendError) -> String {
    crate::confirm::refusal(error)
}

/// What the page needs: the admin's session, their server and username.
pub struct Admin {
    pub session: Arc<dyn Session>,
    pub server: Server,
    pub username: String,
}

/// The Invites groups, added to `page` (a server's page in Settings).
/// They hide themselves when the server says the user is no longer its
/// admin.
pub fn fill(page: &adw::PreferencesPage, admin: Admin, toasts: &adw::ToastOverlay) {
    let view = View::new(admin, toasts);
    // A heading over the section: a group with no rows shows only its
    // title.
    let section = adw::PreferencesGroup::builder()
        .title("Admin")
        .description("Only admins see this.")
        .build();
    let share = share_group(&view);
    let note = adw::PreferencesGroup::builder()
        .description(
            "Used-up and expired invites drop off. Revoking stops the unused codes; accounts and \
             pairings already made stay.",
        )
        .build();
    page.add(&section);
    page.add(&view.group);
    page.add(&share);
    page.add(&note);
    view.also.borrow_mut().extend([section, share, note]);
    if let Some(web) = web_admin(view.session.as_ref()) {
        page.add(&web);
        view.also.borrow_mut().push(web);
    }
    view.load();
    // The page owns the view: its handlers hold it weakly, so without
    // this the list never loads. Not one of its groups, which the view
    // holds itself.
    let held = std::cell::RefCell::new(Some(view));
    page.connect_destroy(move |_| {
        held.borrow_mut().take();
    });
}

/// "Web admin": users, libraries, federation and settings are the web
/// UI's, when the server has a web address a browser can reach.
fn web_admin(session: &dyn Session) -> Option<adw::PreferencesGroup> {
    let url = session.web_url().filter(|u| crate::browser::is_web(u))?;
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title("Web admin")
        .subtitle("Users, libraries, federation and settings.")
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(crate::icons::BROWSER));
    let open = gtk::Button::builder()
        .label("Open…")
        .valign(gtk::Align::Center)
        .css_classes(["row-button"])
        .build();
    open.connect_clicked(move |button| {
        let window = button.root().and_downcast::<gtk::Window>();
        crate::browser::open(&url, window.as_ref());
    });
    row.add_suffix(&open);
    let group = adw::PreferencesGroup::new();
    group.add(&row);
    Some(group)
}

/// "Share the server without an account": the code-less link.
fn share_group(view: &Rc<View>) -> adw::PreferencesGroup {
    let name = &view.server.name;
    let group = adw::PreferencesGroup::builder()
        .title("Share the server without an account")
        .description(crate::markup::plain(&format!(
            "A link with no code: it finds {name}; people sign in with an account they have."
        )))
        .build();
    let row = adw::ActionRow::builder().use_markup(false).build();
    row.set_title(name);
    row.set_subtitle("Never expires, needs no confirming");
    row.add_prefix(
        &gtk::Image::builder()
            .icon_name(crate::icons::LINK)
            .css_classes(["invite-account"])
            .build(),
    );
    let copy = gtk::Button::builder()
        .label("Copy link")
        .valign(gtk::Align::Center)
        .build();
    let (weak, link) = (Rc::downgrade(view), share_link(&view.server));
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&link);
        if let Some(view) = weak.upgrade() {
            view.toast("Link copied");
        }
    });
    row.add_suffix(&copy);
    group.add(&row);
    group
}

/// The page's list, and what it acts through.
struct View {
    session: Arc<dyn Session>,
    server: Server,
    username: String,
    toasts: glib::WeakRef<adw::ToastOverlay>,
    group: adw::PreferencesGroup,
    /// The section's other groups, hidden with `group`.
    also: std::cell::RefCell<Vec<adw::PreferencesGroup>>,
    rows: std::cell::RefCell<Vec<gtk::Widget>>,
}

impl View {
    fn new(admin: Admin, toasts: &adw::ToastOverlay) -> Rc<Self> {
        let add = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name(crate::icons::ADD)
                    .label("New invite")
                    .build(),
            )
            .valign(gtk::Align::Center)
            .css_classes(["suggested-action"])
            .build();
        let group = adw::PreferencesGroup::builder()
            .title(crate::markup::plain(&format!(
                "Invites on {}",
                admin.server.name
            )))
            .description("The same list as the web admin’s.")
            .header_suffix(&add)
            .build();
        let view = Rc::new(View {
            session: admin.session,
            server: admin.server,
            username: admin.username,
            toasts: toasts.downgrade(),
            also: Default::default(),
            group,
            rows: Default::default(),
        });
        let weak = Rc::downgrade(&view);
        add.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.new_invite();
            }
        });
        view
    }

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
        let handle = relm4::spawn(call(self.session.clone()));
        let this = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(answer) = handle.await else { return };
            if let Some(this) = this.upgrade() {
                then(&this, answer);
            }
        });
    }

    /// "Confirm it's you", then `again`.
    fn confirm(self: &Rc<Self>, title: &str, again: impl Fn(&Rc<Self>) + 'static) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        crate::confirm::ask_because(
            &parent,
            self.session.clone(),
            &self.server.name,
            title,
            &confirm_lead(&self.server.name),
            Rc::new(move || {
                if let Some(view) = weak.upgrade() {
                    again(&view);
                }
            }),
        );
    }

    /// The server says the user is no longer its admin: say so, over
    /// `over` or the page, and take the tab away.
    fn no_longer_admin(&self, heading: &str, over: Option<&adw::Dialog>) {
        let alert = adw::AlertDialog::builder()
            .heading(heading)
            .body(format!(
                "{} refused: {} isn’t an admin any more. The Invites tab is gone from \
                 Preferences.",
                self.server.name, self.username
            ))
            .build();
        alert.add_response("ok", "OK");
        match (over, self.toasts.upgrade()) {
            (Some(dialog), _) => alert.present(Some(dialog)),
            (None, Some(toasts)) => alert.present(Some(&toasts)),
            (None, None) => {}
        }
        self.group.set_visible(false);
        for group in self.also.borrow().iter() {
            group.set_visible(false);
        }
    }

    fn load(self: &Rc<Self>) {
        self.run(
            |s| Box::pin(async move { s.invites().await }),
            |view, listed| match listed {
                Ok(invites) => view.show(&invites),
                Err(BackendError::AdminOnly) => view.no_longer_admin("Invites", None),
                Err(e) => view.toast(&refusal(&e)),
            },
        );
    }

    fn show(self: &Rc<Self>, invites: &[InviteSummary]) {
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }
        let now = SystemTime::now();
        let mut rows = Vec::new();
        let listed: Vec<&InviteSummary> = invites.iter().filter(|i| working(i, now)).collect();
        if listed.is_empty() {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title("No invites")
                .subtitle("Make one to let someone in")
                .build();
            rows.push(row.upcast());
        }
        for invite in listed {
            rows.push(self.row(invite, now).upcast());
        }
        for row in &rows {
            self.group.add(row);
        }
        *self.rows.borrow_mut() = rows;
    }

    fn row(self: &Rc<Self>, invite: &InviteSummary, now: SystemTime) -> adw::ActionRow {
        let (title, line) = row_lines(invite, now);
        let (chip, class, icon) = kind_chip(invite.kind);
        let row = adw::ActionRow::builder().use_markup(false).build();
        row.set_title(&title);
        row.set_subtitle(&line);
        row.add_prefix(
            &gtk::Image::builder()
                .icon_name(icon)
                .css_classes([class])
                .build(),
        );
        row.add_suffix(
            &gtk::Label::builder()
                .label(chip)
                .valign(gtk::Align::Center)
                .css_classes(["chip", "invite-chip", class])
                .build(),
        );
        let revoke = gtk::Button::builder()
            .label("Revoke")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        let (weak, invite) = (Rc::downgrade(self), invite.clone());
        revoke.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.ask_revoke(&invite);
            }
        });
        row.add_suffix(&revoke);
        row
    }

    /// "Revoke “Book club”?", then the revoke.
    fn ask_revoke(self: &Rc<Self>, invite: &InviteSummary) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let alert = adw::AlertDialog::builder()
            .heading(format!(
                "Revoke “{}”?",
                title(invite.note.as_deref(), invite.kind)
            ))
            .body(format!(
                "{}\n\nRevoking asks you to confirm it’s you when the last confirmation is more \
                 than a few minutes old.",
                revoke_body(invite)
            ))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        alert.add_response("cancel", "Cancel");
        alert.add_response("revoke", "Revoke");
        alert.set_response_appearance("revoke", adw::ResponseAppearance::Destructive);
        let (weak, id) = (Rc::downgrade(self), invite.id.clone());
        alert.connect_response(Some("revoke"), move |_, _| {
            if let Some(view) = weak.upgrade() {
                view.revoke(id.clone());
            }
        });
        alert.present(Some(&parent));
    }

    fn revoke(self: &Rc<Self>, id: String) {
        let again = id.clone();
        self.run(
            move |s| Box::pin(async move { s.revoke_invite(&id).await }),
            move |view, revoked| match revoked {
                Ok(()) => {
                    view.toast("Invite revoked");
                    view.load();
                }
                Err(BackendError::RecentSignInRequired) => {
                    view.confirm("Revoke invite", move |view| view.revoke(again.clone()));
                }
                Err(BackendError::AdminOnly) => {
                    view.no_longer_admin("Couldn’t revoke the invite", None)
                }
                Err(e) => view.toast(&refusal(&e)),
            },
        );
    }

    fn new_invite(self: &Rc<Self>) {
        let Some(parent) = self.toasts.upgrade() else {
            return;
        };
        let making = Making::new(self);
        making.dialog.present(Some(&parent));
    }
}

/// The New invite dialog: the form, then the invite made.
struct Making {
    dialog: adw::Dialog,
    view: std::rc::Weak<View>,
    stack: gtk::Stack,
    cancel: gtk::Button,
    kinds: Vec<gtk::CheckButton>,
    note: adw::EntryRow,
    uses: adw::SpinRow,
    expiry: adw::ComboRow,
    role: adw::ComboRow,
    make: gtk::Button,
    error: gtk::Label,
    qr: gtk::Picture,
    made_title: gtk::Label,
    made_chip: gtk::Label,
    made_guidance: gtk::Label,
    code: gtk::Label,
    link: gtk::Label,
    copy: gtk::Button,
    copy_link: gtk::Button,
    /// The invite made: what the Copy buttons copy.
    made: std::cell::RefCell<Option<MintedInvite>>,
}

/// A boxed list holding `row`, for rows laid side by side.
fn boxed(row: &impl IsA<gtk::Widget>) -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .hexpand(true)
        .css_classes(["boxed-list"])
        .build();
    list.append(row);
    list
}

impl Making {
    fn new(view: &Rc<View>) -> Rc<Self> {
        let server = &view.server.name;
        let who = adw::PreferencesGroup::builder()
            .title("Who is it for?")
            .build();
        let mut kinds: Vec<gtk::CheckButton> = Vec::new();
        for (kind, built) in KINDS {
            let (name, detail) = kind_card(kind, server);
            let (_, class, icon) = kind_chip(kind);
            let check = gtk::CheckButton::builder()
                .valign(gtk::Align::Center)
                .sensitive(built)
                .build();
            if let Some(first) = kinds.first() {
                check.set_group(Some(first));
            } else {
                check.set_active(true);
            }
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .activatable(built)
                .sensitive(built)
                .build();
            row.set_title(name);
            row.set_subtitle(&if built {
                detail
            } else {
                format!("{detail} Arrives with federation.")
            });
            row.add_prefix(&check);
            row.add_prefix(
                &gtk::Image::builder()
                    .icon_name(icon)
                    .css_classes([class])
                    .build(),
            );
            row.set_activatable_widget(Some(&check));
            who.add(&row);
            kinds.push(check);
        }
        let note = adw::EntryRow::builder()
            .use_markup(false)
            .title("Note")
            .build();
        let note_group = adw::PreferencesGroup::builder()
            .description("Only admins see it.")
            .build();
        note_group.add(&note);
        let uses = adw::SpinRow::builder()
            .use_markup(false)
            .title("Uses")
            .adjustment(&gtk::Adjustment::new(
                1.0,
                1.0,
                f64::from(MAX_USES),
                1.0,
                5.0,
                0.0,
            ))
            .build();
        let expiry = adw::ComboRow::builder()
            .use_markup(false)
            .title("Expires after")
            .model(&gtk::StringList::new(&EXPIRIES.map(|(name, _)| name)))
            .build();
        let side_by_side = gtk::Box::builder().spacing(12).homogeneous(true).build();
        side_by_side.append(&boxed(&uses));
        side_by_side.append(&boxed(&expiry));
        let role = adw::ComboRow::builder()
            .use_markup(false)
            .title("Role")
            .model(&gtk::StringList::new(&ROLES.map(|(_, name)| name)))
            .build();
        let role_list = boxed(&role);
        let error = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .css_classes(["error"])
            .build();
        let make = gtk::Button::builder()
            .label("Create invite")
            .halign(gtk::Align::End)
            .css_classes(["pill", "suggested-action"])
            .build();
        let form_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        form_page.append(&who);
        form_page.append(&note_group);
        form_page.append(&side_by_side);
        form_page.append(&role_list);
        form_page.append(&error);
        form_page.append(&make);

        let qr = gtk::Picture::builder()
            .can_shrink(false)
            .valign(gtk::Align::Start)
            .build();
        let made_title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        let made_chip = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .css_classes(["chip", "invite-chip"])
            .build();
        let made_guidance = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(30)
            .css_classes(["dim-label"])
            .build();
        let beside = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .hexpand(true)
            .build();
        beside.append(&made_title);
        beside.append(&made_chip);
        beside.append(&made_guidance);
        let top = gtk::Box::builder().spacing(20).build();
        top.append(&qr);
        top.append(&beside);
        let code = gtk::Label::builder()
            .selectable(true)
            .xalign(0.0)
            .css_classes(["title-2", "monospace"])
            .build();
        let link = gtk::Label::builder()
            .selectable(true)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["monospace", "dim-label", "caption"])
            .build();
        let copy = crate::start::action(
            "Copy invite text",
            Some(crate::icons::COPY),
            &["suggested-action"],
        );
        let copy_link = crate::start::action("Copy link", Some(crate::icons::LINK), &[]);
        let buttons = gtk::Box::builder().spacing(8).build();
        buttons.append(&copy);
        buttons.append(&copy_link);
        let made_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        made_page.append(&top);
        made_page.append(&code);
        made_page.append(&link);
        made_page.append(
            &gtk::Label::builder()
                .label("Shown once. Close this and the code is gone; the invite stays in the list.")
                .wrap(true)
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        made_page.append(&buttons);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeft)
            .vhomogeneous(false)
            .build();
        stack.add_named(&form_page, Some("form"));
        stack.add_named(&made_page, Some("made"));
        let cancel = gtk::Button::builder()
            .label("Cancel")
            .css_classes(["flat", "dialog-cancel"])
            .build();
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .css_classes(["dialog-header"])
            .build();
        header.pack_start(&cancel);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let dialog = adw::Dialog::builder()
            .title("New invite")
            .content_width(520)
            .child(&toolbar)
            .build();

        let making = Rc::new(Making {
            dialog,
            view: Rc::downgrade(view),
            stack,
            cancel,
            kinds,
            note,
            uses,
            expiry,
            role,
            make,
            error,
            qr,
            made_title,
            made_chip,
            made_guidance,
            code,
            link,
            copy,
            copy_link,
            made: Default::default(),
        });
        making.wire(role_list);
        making
    }

    fn wire(self: &Rc<Self>, role_list: gtk::ListBox) {
        for check in &self.kinds {
            let (weak, role_list) = (Rc::downgrade(self), role_list.clone());
            check.connect_toggled(move |_| {
                if let Some(making) = weak.upgrade() {
                    role_list.set_visible(making.form().spec().role.is_some());
                }
            });
        }
        let dialog = self.dialog.downgrade();
        self.cancel.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
        let weak = Rc::downgrade(self);
        self.make.connect_clicked(move |_| {
            if let Some(making) = weak.upgrade() {
                making.make();
            }
        });
        let weak = Rc::downgrade(self);
        self.copy.connect_clicked(move |button| {
            if let Some(making) = weak.upgrade()
                && let Some(made) = making.made.borrow().as_ref()
            {
                button.clipboard().set_text(&made.copy_string);
                making.copied("Invite copied");
            }
        });
        let weak = Rc::downgrade(self);
        self.copy_link.connect_clicked(move |button| {
            if let Some(making) = weak.upgrade()
                && let Some(made) = making.made.borrow().as_ref()
            {
                button.clipboard().set_text(&made.link);
                making.copied("Link copied");
            }
        });
    }

    fn copied(&self, text: &str) {
        if let Some(view) = self.view.upgrade() {
            view.toast(text);
        }
    }

    fn form(&self) -> Form {
        Form {
            kind: self.kinds.iter().position(|k| k.is_active()).unwrap_or(0),
            note: self.note.text().to_string(),
            uses: self.uses.value() as u32,
            expiry: self.expiry.selected() as usize,
            role: self.role.selected() as usize,
        }
    }

    fn make(self: &Rc<Self>) {
        let Some(view) = self.view.upgrade() else {
            return;
        };
        let form = self.form();
        let spec = form.spec();
        self.make.set_sensitive(false);
        self.error.set_visible(false);
        let weak = Rc::downgrade(self);
        view.run(
            move |s| Box::pin(async move { s.create_invite(spec).await }),
            move |view, made| {
                let Some(making) = weak.upgrade() else { return };
                making.make.set_sensitive(true);
                match made {
                    Ok(made) => {
                        making.show(&form, made);
                        view.load();
                    }
                    Err(BackendError::RecentSignInRequired) => {
                        let weak = Rc::downgrade(&making);
                        view.confirm("Confirm it’s you", move |_| {
                            if let Some(making) = weak.upgrade() {
                                making.make();
                            }
                        });
                    }
                    Err(BackendError::AdminOnly) => {
                        making.dialog.close();
                        view.no_longer_admin("Couldn’t make the invite", None);
                    }
                    Err(e) => {
                        making.error.set_label(&refusal(&e));
                        making.error.set_visible(true);
                    }
                }
            },
        );
    }

    /// The invite made: its QR code, name and kind, what to do with it, its
    /// code as shown and its link.
    fn show(&self, form: &Form, made: MintedInvite) {
        let texture = crate::qr::texture(&made.link, crate::qr::MODULE)
            .inspect_err(|e| tracing::warn!(error = %e, "cannot draw the QR code"))
            .ok();
        self.qr.set_paintable(texture.as_ref());
        self.qr.set_visible(texture.is_some());
        self.made_title.set_label(&made_line(
            &form.note,
            made.kind,
            made.uses,
            form.expiry_name(),
        ));
        let (chip, class, _) = kind_chip(made.kind);
        self.made_chip.set_label(chip);
        self.made_chip.add_css_class(class);
        self.made_guidance.set_label(guidance(made.kind));
        self.code.set_label(&made.code);
        self.link.set_label(&made.link);
        self.dialog.set_title(match made.kind {
            InviteKind::Federation => "Server invite made",
            _ => "Invite made",
        });
        self.cancel.set_label("Done");
        *self.made.borrow_mut() = Some(made);
        self.stack.set_visible_child_name("made");
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::Client;
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::testing::ready;

    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn summary() -> InviteSummary {
        InviteSummary {
            id: "inv-1".into(),
            kind: InviteKind::CreateUser,
            note: Some("Book club".into()),
            uses: Some(4),
            used: 1,
            expires_at: None,
            created_by: "Andrey".into(),
            created_by_username: Some("andrey".into()),
            created_from: InviteOrigin::Device {
                name: Some("iPhone".into()),
                platform: Platform::Ios,
                this_device: false,
            },
            revoked: false,
        }
    }

    #[test]
    fn a_row_reads_as_the_board_draws_it() {
        let now = SystemTime::now();
        let invite = InviteSummary {
            expires_at: Some(now + 2 * DAY + Duration::from_secs(60)),
            ..summary()
        };
        assert_eq!(
            row_lines(&invite, now),
            (
                "Book club".to_owned(),
                "3 of 4 left · expires in 2 days · by Andrey on iPhone".to_owned()
            )
        );
    }

    #[test]
    fn under_a_day_left_is_soon() {
        let now = SystemTime::now();
        assert_eq!(
            expires_in(now + Duration::from_secs(23 * 3600 + 60), now),
            ("expires in 23 hours".to_owned(), true)
        );
    }

    #[test]
    fn a_row_without_a_note_is_named_for_its_kind() {
        let invite = InviteSummary {
            note: None,
            kind: InviteKind::PortableAccount,
            ..summary()
        };
        assert_eq!(
            row_lines(&invite, SystemTime::now()).0,
            "Someone with their own identity"
        );
    }

    #[test]
    fn an_invite_made_in_the_web_admin_says_so() {
        assert_eq!(
            made_where("Andrey", &InviteOrigin::WebAdmin),
            "by Andrey in the web admin"
        );
    }

    #[test]
    fn an_invite_made_here_says_this_computer() {
        let here = InviteOrigin::Device {
            name: Some("nimble".into()),
            platform: Platform::Linux,
            this_device: true,
        };
        assert_eq!(made_where("Andrey", &here), "by Andrey on this computer");
    }

    #[test]
    fn revoked_expired_and_used_up_invites_drop_off() {
        let now = SystemTime::now();
        let ended = [
            InviteSummary {
                revoked: true,
                ..summary()
            },
            InviteSummary {
                expires_at: Some(now - Duration::from_secs(1)),
                ..summary()
            },
            InviteSummary {
                used: 4,
                ..summary()
            },
        ];
        assert!(ended.iter().all(|i| !working(i, now)));
        assert!(working(&summary(), now));
    }

    #[test]
    fn revoking_says_what_stops_and_what_stays() {
        let invite = InviteSummary {
            used: 3,
            ..summary()
        };
        assert_eq!(
            revoke_body(&invite),
            "The one use left stops working now. The 3 accounts made with it stay."
        );
    }

    #[test]
    fn revoking_an_unused_invite_mentions_nothing_made() {
        let invite = InviteSummary {
            used: 0,
            uses: Some(2),
            ..summary()
        };
        assert_eq!(revoke_body(&invite), "The 2 uses left stop working now.");
    }

    #[test]
    fn the_made_line_names_note_uses_and_how_long() {
        assert_eq!(
            made_line("Book club", InviteKind::CreateUser, Some(4), "7 days"),
            "Book club · 4 uses · 7 days"
        );
    }

    #[test]
    fn the_form_at_first_asks_for_one_new_account_for_a_week_as_a_consumer() {
        assert_eq!(
            Form::default().spec(),
            InviteSpec {
                kind: InviteKind::CreateUser,
                note: None,
                uses: Some(1),
                expires_in: Some(7 * DAY),
                role: Some(Role::Consumer),
            }
        );
    }

    #[test]
    fn an_identity_invite_asks_for_no_role() {
        let form = Form {
            kind: 1,
            role: 1,
            ..Form::default()
        };
        assert_eq!(form.spec().role, None);
    }

    #[test]
    fn a_blank_note_is_no_note() {
        let form = Form {
            note: "   ".into(),
            ..Form::default()
        };
        assert_eq!(form.spec().note, None);
    }

    #[test]
    fn the_share_link_finds_the_server_without_a_code() {
        let server = Server {
            key: spoolframe_linux_core::backend::ServerKey("c4b1".repeat(16)),
            name: "Home Media".into(),
            addresses: Vec::new(),
            pinned: true,
            fingerprint: String::new(),
            verified: true,
            on_network: true,
            verified_at: None,
            last_connected_at: None,
            server_id: None,
            advertised: vec!["192.168.1.20:7430".into()],
            relays: Vec::new(),
            advertised_at: None,
            use_relays: None,
            relays_edited: false,
        };
        let read = spoolframe_protocol::invite::parse_link(&share_link(&server)).unwrap();
        assert_eq!(
            (read.server, read.name, read.code),
            ("c4b1".repeat(16), Some("Home Media".into()), None)
        );
    }

    #[test]
    fn what_the_form_asks_for_the_fake_server_makes() {
        let client = FakeClient::new(None);
        let andrey = client.session(&FakeClient::profile("andrey")).unwrap();
        let form = Form {
            kind: 0,
            note: "For Sam".into(),
            uses: 3,
            expiry: 2,
            role: 1,
        };
        let made = ready(andrey.create_invite(form.spec())).unwrap();
        assert_eq!((made.kind, made.uses), (InviteKind::CreateUser, Some(3)));
    }

    fn settle() {
        let context = glib::MainContext::default();
        for _ in 0..50 {
            while context.iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn admin(client: &FakeClient, username: &str) -> Admin {
        Admin {
            session: client.session(&FakeClient::profile(username)).unwrap(),
            server: ready(client.servers()).unwrap().remove(0),
            username: username.into(),
        }
    }

    /// The page with the fake: the list, a new invite made through the
    /// dialog and shown, the list with it, and revoking it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_invites_page_makes_lists_and_revokes() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client = FakeClient::new(None);
        let admin = admin(&client, "andrey");
        let session = admin.session.clone();
        let toasts = adw::ToastOverlay::new();
        let page = adw::PreferencesPage::new();
        let view = View::new(admin, &toasts);
        page.add(&view.group);
        view.load();
        settle();
        assert_eq!(view.rows.borrow().len(), 1, "the empty row");

        let making = Making::new(&view);
        making.note.set_text("Book club");
        making.make();
        settle();
        assert_eq!(making.stack.visible_child_name().as_deref(), Some("made"));
        let made = making.made.borrow().clone().expect("an invite made");
        assert_eq!(
            (making.code.label(), making.cancel.label()),
            (made.code.as_str().into(), Some("Done".into()))
        );
        assert_eq!(view.rows.borrow().len(), 1, "the invite, not the empty row");

        view.revoke(made.id.clone());
        settle();
        assert!(ready(session.invites()).unwrap()[0].revoked);
        assert_eq!(view.rows.borrow().len(), 1, "the empty row again");
    }

    /// A user who is no longer the admin: the section hides itself.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn a_refused_list_takes_the_page_away() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client = FakeClient::new(None);
        let toasts = adw::ToastOverlay::new();
        let page = adw::PreferencesPage::new();
        let view = View::new(admin(&client, "bob"), &toasts);
        page.add(&view.group);
        view.load();
        settle();
        assert!(!view.group.is_visible());
        the_admin_heading_goes_with_the_list();
    }

    /// Part of the display test above: GTK binds itself to one thread.
    fn the_admin_heading_goes_with_the_list() {
        let client = FakeClient::new(None);
        let toasts = adw::ToastOverlay::new();
        let page = adw::PreferencesPage::new();
        fill(&page, admin(&client, "bob"), &toasts);
        settle();
        let mut shown = Vec::new();
        let mut widgets = vec![page.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = widgets.pop() {
            if let Some(group) = widget.downcast_ref::<adw::PreferencesGroup>()
                && group.is_visible()
                && group.title() == "Admin"
            {
                shown.push(group.clone());
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                widgets.push(c);
            }
        }
        assert!(shown.is_empty(), "the Admin heading stayed for a non-admin");
    }
}
