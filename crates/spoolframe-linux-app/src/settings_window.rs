//! Settings, as on the SettingsModelLinux and LinuxSettings* boards: one
//! window, open before signing in or inside a profile. A sidebar of what
//! belongs to This computer (Appearance, Playback, Connection, Downloads,
//! Your identity, Shared computer, About), then each saved server, then
//! Add a server…; the page picked beside it. It collapses to one column
//! when narrow.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{Server, ServerKey};
use spoolframe_linux_core::theme::Theme;

use crate::preferences::{self, Device, Servers};

/// Where Settings opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Open {
    /// This computer's first page, Appearance.
    ThisComputer,
    /// A saved server's page (from a profile's account menu).
    Server(ServerKey),
}

/// What Settings needs from the app.
pub struct Needs {
    pub theme: Theme,
    pub set_theme: Rc<dyn Fn(Theme)>,
    pub device: Device,
    pub servers: Servers,
    /// The profile signed in to the server in use, for that server's page.
    pub signed: Option<preferences::Signed>,
}

/// This computer's pages, in the sidebar's order: name, title, icon.
const DEVICE_PAGES: [(&str, &str, &str); 7] = [
    ("appearance", "Appearance", crate::icons::APPEARANCE),
    ("playback", "Playback", crate::icons::PLAYBACK_PAGE),
    ("connection", "Connection", crate::icons::CONNECTION),
    ("downloads", "Downloads", crate::icons::DOWNLOADS),
    ("identity", "Your identity", crate::icons::IDENTITY),
    ("shared", "Shared computer", crate::icons::SHARED),
    ("about", "About", crate::icons::ABOUT),
];

/// The stack's name for a server's page.
pub fn server_page_name(key: &ServerKey) -> String {
    format!("server:{}", key.0)
}

/// A server's line in the sidebar: who is signed in, or not.
pub fn server_line(user: Option<&str>) -> String {
    user.map_or_else(|| "Not signed in".to_owned(), str::to_owned)
}

/// The window's marker, so a widget in it can close it.
const WINDOW_CLASS: &str = "settings-window";

/// Closes Settings from `widget` in it: the adw dialog it is in first, if
/// any, then the window.
pub fn close(widget: &impl IsA<gtk::Widget>) {
    if let Some(dialog) = widget.ancestor(adw::Dialog::static_type()) {
        dialog.downcast::<adw::Dialog>().expect("a dialog").close();
    }
    if let Some(window) = widget.root().and_downcast::<gtk::Window>()
        && window.has_css_class(WINDOW_CLASS)
    {
        window.close();
    }
}

struct View {
    stack: gtk::Stack,
    split: adw::NavigationSplitView,
    content: adw::NavigationPage,
    device: gtk::ListBox,
    servers_list: gtk::ListBox,
    toasts: adw::ToastOverlay,
    servers: Rc<Servers>,
    signed: RefCell<Option<preferences::Signed>>,
    /// The servers' rows, by stack name, and what their page shows.
    rows: RefCell<Vec<(String, Server, Option<String>)>>,
}

/// The window, over `parent`, open on `open`.
pub fn window(parent: &impl IsA<gtk::Window>, needs: Needs, open: Open) -> adw::Window {
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vexpand(true)
        .build();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let client = needs.servers.client.clone();
    // About opens Open source licenses and a license's text over itself.
    let about = crate::licenses_page::about_view();
    let pages: [(&str, gtk::Widget); 7] = [
        (
            "appearance",
            preferences::appearance(needs.theme, needs.set_theme).upcast(),
        ),
        ("playback", preferences::playback(&needs.device).upcast()),
        (
            "connection",
            preferences::connection(&needs.device).upcast(),
        ),
        (
            "downloads",
            preferences::downloads(client.clone(), crate::downloads_dir()).upcast(),
        ),
        (
            "identity",
            crate::identity_page::page(client.clone(), &toasts).upcast(),
        ),
        ("shared", preferences::shared(client.identities()).upcast()),
        ("about", about.clone().upcast()),
    ];
    for (name, page) in pages {
        stack.add_named(&page, Some(name));
    }

    // The sidebar: This computer, then the servers.
    let device = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar"])
        .build();
    for (name, title, icon) in DEVICE_PAGES {
        let row = sidebar_row(title, None, icon);
        row.set_widget_name(name);
        device.append(&row);
    }
    let servers_list = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar"])
        .build();
    let add = sidebar_row("Add a server…", None, crate::icons::ADD);
    add.set_widget_name("add");
    add.set_selectable(false);
    servers_list.append(&add);
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    column.append(&heading("This computer"));
    column.append(&device);
    column.append(&heading("Servers"));
    column.append(&servers_list);
    let sidebar_header = adw::HeaderBar::new();
    let sidebar = adw::NavigationPage::builder()
        .title("Settings")
        .child(&crate::views::toolbar(
            &sidebar_header,
            &[],
            &gtk::ScrolledWindow::builder()
                .child(&column)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .build(),
        ))
        .build();
    let content = adw::NavigationPage::builder()
        .title("Appearance")
        .child(&crate::views::toolbar(&adw::HeaderBar::new(), &[], &toasts))
        .build();
    let split = adw::NavigationSplitView::builder()
        .sidebar(&sidebar)
        .content(&content)
        .min_sidebar_width(220.0)
        .build();

    let window = adw::Window::builder()
        .title("Settings")
        .default_width(980)
        .default_height(720)
        .transient_for(parent)
        .content(&split)
        .css_classes([WINDOW_CLASS])
        .build();
    // One column when narrow, the sidebar first.
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        640.0,
        adw::LengthUnit::Sp,
    ));
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    // The header names About's page in view, as it changes.
    {
        let (content, stack) = (content.clone(), stack.clone());
        about.connect_visible_page_notify(move |about| {
            if stack.visible_child_name().as_deref() == Some("about")
                && let Some(page) = about.visible_page()
            {
                content.set_title(&page.title());
            }
        });
    }

    let view = Rc::new(View {
        stack,
        split,
        content,
        device,
        servers_list,
        toasts,
        servers: Rc::new(needs.servers),
        signed: RefCell::new(needs.signed),
        rows: RefCell::new(Vec::new()),
    });
    {
        let weak = Rc::downgrade(&view);
        view.device.connect_row_activated(move |_, row| {
            if let Some(view) = weak.upgrade() {
                view.servers_list.unselect_all();
                // About from the sidebar is About, not the license last read.
                if let Some(about) = view
                    .stack
                    .child_by_name("about")
                    .and_downcast::<adw::NavigationView>()
                    && row.widget_name() == "about"
                {
                    crate::licenses_page::back_to_about(&about);
                }
                view.show(&row.widget_name());
            }
        });
    }
    {
        let weak = Rc::downgrade(&view);
        view.servers_list.connect_row_activated(move |_, row| {
            let Some(view) = weak.upgrade() else { return };
            if row.widget_name() == "add" {
                crate::settings_window::close(row);
                (view.servers.add)();
                return;
            }
            view.device.unselect_all();
            view.show(&row.widget_name());
        });
    }
    view.load_servers(open.clone());
    if open == Open::ThisComputer {
        view.select_device("appearance");
    }
    // The window owns the view; its handlers hold it weakly.
    let owned = RefCell::new(Some(view));
    window.connect_close_request(move |_| {
        owned.borrow_mut().take();
        glib::Propagation::Proceed
    });
    window
}

impl View {
    /// Shows the page named `name`, building a server's on first use.
    fn show(self: &Rc<Self>, name: &str) {
        if self.stack.child_by_name(name).is_none() {
            let built = self
                .rows
                .borrow()
                .iter()
                .find(|(n, ..)| n == name)
                .map(|(_, server, _)| server.clone());
            let Some(server) = built else { return };
            // The profile goes to the server in use's page, once.
            let signed = if self.servers.current.as_ref().map(|s| &s.key) == Some(&server.key) {
                self.signed.borrow_mut().take()
            } else {
                None
            };
            let dropped: Rc<dyn Fn()> = {
                let (weak, name) = (Rc::downgrade(self), name.to_owned());
                Rc::new(move || {
                    if let Some(view) = weak.upgrade() {
                        view.drop_server(&name);
                    }
                })
            };
            let signed_out: Rc<dyn Fn()> = {
                let (weak, name) = (Rc::downgrade(self), name.to_owned());
                Rc::new(move || {
                    if let Some(view) = weak.upgrade() {
                        view.signed_out(&name);
                    }
                })
            };
            let open_identity: Rc<dyn Fn()> = {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(view) = weak.upgrade() {
                        view.servers_list.unselect_all();
                        view.select_device("identity");
                    }
                })
            };
            let page = preferences::server_page(
                &server,
                preferences::ServerPage {
                    servers: self.servers.clone(),
                    signed,
                    dropped,
                    signed_out,
                    open_identity,
                },
                &self.toasts,
            );
            self.stack.add_named(&page, Some(name));
        }
        self.stack.set_visible_child_name(name);
        // A server's page by its name; This computer's by their own.
        let server_name = self
            .rows
            .borrow()
            .iter()
            .find(|(n, ..)| n == name)
            .map(|(_, server, _)| server.name.clone());
        let title = server_name.or_else(|| {
            let child = self.stack.visible_child()?;
            if let Some(about) = child.downcast_ref::<adw::NavigationView>() {
                return about.visible_page().map(|page| page.title().to_string());
            }
            child
                .downcast::<adw::PreferencesPage>()
                .ok()
                .map(|page| page.title().to_string())
        });
        if let Some(title) = title {
            self.content.set_title(&title);
        }
        self.split.set_show_content(true);
    }

    /// A forgotten server: its row and page go, and This computer shows.
    fn drop_server(self: &Rc<Self>, name: &str) {
        self.rows.borrow_mut().retain(|(n, ..)| n != name);
        let mut child = self.servers_list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.widget_name() == name {
                self.servers_list.remove(&widget);
            }
        }
        if let Some(page) = self.stack.child_by_name(name) {
            self.stack.remove(&page);
        }
        self.select_device("appearance");
    }

    /// Left by the profile in use, kept for others (C7): its row says Not
    /// signed in, and its page is made again with no profile, listing
    /// those kept here.
    fn signed_out(self: &Rc<Self>, name: &str) {
        for (row, _, user) in self.rows.borrow_mut().iter_mut() {
            if row == name {
                *user = None;
            }
        }
        let mut child = self.servers_list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.widget_name() == name
                && let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
            {
                set_subtitle(row, &server_line(None));
            }
        }
        // The profile went to the page once; it is not handed out again.
        self.signed.borrow_mut().take();
        if let Some(page) = self.stack.child_by_name(name) {
            self.stack.remove(&page);
        }
        self.show(name);
    }

    fn select_device(self: &Rc<Self>, name: &str) {
        let mut child = self.device.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
                && row.widget_name() == name
            {
                self.device.select_row(Some(row));
            }
            child = widget.next_sibling();
        }
        self.show(name);
    }

    /// The saved servers, each with who is signed in there, listed above
    /// Add a server…; then the page `open` asked for.
    fn load_servers(self: &Rc<Self>, open: Open) {
        let weak = Rc::downgrade(self);
        let (client, current) = (
            self.servers.client.clone(),
            self.servers.current.as_ref().map(|s| s.key.clone()),
        );
        let user_here = self.servers.user.clone();
        glib::spawn_future_local(async move {
            let saved = match client.servers().await {
                Ok(list) => list,
                Err(e) => return tracing::warn!(error = %e, "cannot list servers"),
            };
            let mut listed = Vec::new();
            for server in saved.into_iter().filter(|s| s.pinned) {
                let user = if Some(&server.key) == current.as_ref() {
                    user_here.clone()
                } else {
                    match client.profiles(&server.key).await {
                        Ok(profiles) => profiles.first().map(|p| p.display_name.clone()),
                        Err(_) => None,
                    }
                };
                listed.push((server, user));
            }
            let Some(view) = weak.upgrade() else { return };
            for (server, user) in listed {
                let name = server_page_name(&server.key);
                let row = sidebar_row(
                    &server.name,
                    Some(&server_line(user.as_deref())),
                    crate::icons::SERVER,
                );
                row.set_widget_name(&name);
                // Above Add a server…, the last row.
                let at = view.rows.borrow().len() as i32;
                view.servers_list.insert(&row, at);
                if open == Open::Server(server.key.clone()) {
                    view.servers_list.select_row(Some(&row));
                }
                view.rows.borrow_mut().push((name, server, user));
            }
            if let Open::Server(key) = &open {
                view.show(&server_page_name(key));
            }
        });
    }
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .margin_start(12)
        .margin_top(12)
        .css_classes(["heading", "dim-label"])
        .build()
}

/// A sidebar row: an icon, a title, and a line under it.
fn sidebar_row(title: &str, subtitle: Option<&str>, icon: &str) -> gtk::ListBoxRow {
    let texts = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    texts.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    let line = gtk::Label::builder()
        .xalign(0.0)
        .visible(subtitle.is_some())
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["caption", "dim-label"])
        .build();
    line.set_text(subtitle.unwrap_or_default());
    texts.append(&line);
    let body = gtk::Box::builder().spacing(12).build();
    body.append(&gtk::Image::from_icon_name(icon));
    body.append(&texts);
    gtk::ListBoxRow::builder().child(&body).build()
}

/// Sets the line under a sidebar row's title.
fn set_subtitle(row: &gtk::ListBoxRow, text: &str) {
    let line = row
        .child()
        .and_then(|body| body.last_child())
        .and_then(|texts| texts.last_child())
        .and_downcast::<gtk::Label>();
    if let Some(line) = line {
        line.set_text(text);
        line.set_visible(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_nobody_is_signed_in_to_says_so() {
        assert_eq!(server_line(None), "Not signed in");
    }

    fn needs() -> Needs {
        use std::sync::Arc;
        let client: Arc<dyn spoolframe_linux_core::backend::Client> =
            Arc::new(spoolframe_linux_core::fake::FakeClient::new(None));
        let off: preferences::SetSkipIntros = Rc::new(|_| {});
        Needs {
            theme: Theme::default(),
            set_theme: Rc::new(|_| {}),
            device: Device {
                quality: Default::default(),
                set_quality: Rc::new(|_| {}),
                show_hdr: true,
                set_show_hdr: off.clone(),
                use_relays: true,
                set_use_relays: off.clone(),
                skip_intros: false,
                set_skip_intros: off.clone(),
                show_diagnostics: false,
                set_show_diagnostics: off,
            },
            servers: Servers {
                client,
                current: None,
                user: None,
                verify: Rc::new(|_| {}),
                use_server: Rc::new(|_| {}),
                add: Rc::new(|| {}),
                forgotten: Rc::new(|_| {}),
                left: Rc::new(|_| {}),
                app_relays: true,
            },
            signed: None,
        }
    }

    /// Settings opens on This computer, before any sign-in, and lists the
    /// saved servers beside it.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn settings_opens_on_this_computer_and_lists_the_servers() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let parent = adw::Window::new();
        let window = window(&parent, needs(), Open::ThisComputer);
        window.present();
        let listed = crate::ui_test::spin_until(std::time::Duration::from_secs(2), || {
            let mut found = false;
            let mut child = window.first_child();
            let mut stack = Vec::new();
            while let Some(widget) = child.take().or_else(|| stack.pop()) {
                if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
                    && row.widget_name().starts_with("server:")
                {
                    found = true;
                }
                if let Some(next) = widget.next_sibling() {
                    stack.push(next);
                }
                child = widget.first_child();
            }
            found
        });
        let page = window
            .content()
            .and_downcast::<adw::NavigationSplitView>()
            .and_then(|split| split.content())
            .map(|content| content.title());
        assert_eq!((listed, page.as_deref()), (true, Some("Appearance")));
    }

    #[test]
    fn a_servers_page_is_named_by_its_key() {
        assert_eq!(server_page_name(&ServerKey("k6eq".into())), "server:k6eq");
    }
}
