//! A saved server's relays (per-server relays, SDK d0d0c701), as on the
//! LinuxServerRelays board: whether it is reached through relays (the
//! app's "Use relays", or On or Off for this server alone), and the relays
//! it is reached through, the server's own list or the user's. As the
//! Apple apps have it: removing the last relay gives the list back to the
//! server, and Off is how a server gets no relays.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use spoolframe_linux_core::backend::{BackendError, Client, Server};

/// The choice's three places in its picker: the app's setting, On, Off.
pub fn choice_index(choice: Option<bool>) -> u32 {
    match choice {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    }
}

pub fn choice_at(index: u32) -> Option<bool> {
    match index {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// The picker's names, the first saying what the app's setting is now.
pub fn choice_names(app_on: bool) -> [String; 3] {
    [
        format!("App setting ({})", on_off(app_on)),
        "On".to_owned(),
        "Off".to_owned(),
    ]
}

/// What the choice does, under the picker, in the Apple apps' words.
pub fn choice_detail(choice: Option<bool>) -> &'static str {
    match choice {
        None => "Follows Use relays in Settings › Connection.",
        Some(true) => {
            "Reached through its relays when it can't be reached directly, whatever Use relays \
             says."
        }
        Some(false) => {
            "Reached only at its addresses. A relay connection already open may last until \
             Spoolframe restarts."
        }
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "On" } else { "Off" }
}

/// The Relays row's line: "App setting (On) · 2 from the server", "Off",
/// "On · 1 of yours".
pub fn summary(server: &Server, app_on: bool) -> String {
    let choice = match server.use_relays {
        None => choice_names(app_on)[0].clone(),
        Some(true) => "On".to_owned(),
        Some(false) => return "Off".to_owned(),
    };
    let list = match (server.relays.len(), server.relays_edited) {
        (0, _) => "none from the server yet".to_owned(),
        (n, true) => format!("{n} of yours"),
        (n, false) => format!("{n} from the server"),
    };
    format!("{choice} · {list}")
}

/// What the list's caption says, by whose list it is.
pub fn list_caption(edited: bool) -> &'static str {
    if edited {
        "Your list: the server's changes don't replace it."
    } else {
        "From the server and the invite it came from. Add or remove one to make the list yours."
    }
}

/// What a relay that is not one says.
const NOT_A_RELAY: &str = "A relay is an https:// address, like https://relay.example.com.";

/// A relay typed in, trimmed, if it is an http(s) URL with a host.
pub fn relay_url(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    let host = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .ok_or(NOT_A_RELAY)?;
    if host.split('/').next().is_none_or(str::is_empty) {
        return Err(NOT_A_RELAY);
    }
    Ok(text.to_owned())
}

/// The list with `url` added once.
pub fn with(list: &[String], url: String) -> Vec<String> {
    let mut list = list.to_vec();
    if !list.contains(&url) {
        list.push(url);
    }
    list
}

/// What the dialog needs: the client, the server, the app's setting, and
/// what to do once a change is saved (list the servers again).
pub struct Needs {
    pub client: Arc<dyn Client>,
    pub server: Server,
    pub app_relays: bool,
    pub changed: Rc<dyn Fn()>,
}

/// The "Relays for <name>" dialog.
pub fn dialog(needs: Needs) -> adw::Dialog {
    let view = Rc::new(View::new(needs));
    view.show();
    view.dialog.clone()
}

struct View {
    dialog: adw::Dialog,
    client: Arc<dyn Client>,
    server: std::cell::RefCell<Server>,
    changed: Rc<dyn Fn()>,
    choice: adw::ComboRow,
    list: adw::PreferencesGroup,
    rows: std::cell::RefCell<Vec<gtk::Widget>>,
    entry: gtk::Entry,
    add: gtk::Button,
    error: gtk::Label,
    servers_list: gtk::Button,
}

impl View {
    fn new(needs: Needs) -> Self {
        let names = choice_names(needs.app_relays);
        let choice = adw::ComboRow::builder()
            .use_markup(false)
            .title("Use relays")
            .model(&gtk::StringList::new(&names.each_ref().map(String::as_str)))
            .selected(choice_index(needs.server.use_relays))
            .build();
        choice.set_subtitle(choice_detail(needs.server.use_relays));
        let how = adw::PreferencesGroup::new();
        how.add(&choice);
        let list = adw::PreferencesGroup::builder().title("Relays").build();
        let entry = gtk::Entry::builder()
            .placeholder_text("https://relay.example.com")
            .hexpand(true)
            .css_classes(["monospace"])
            .build();
        let add = gtk::Button::builder().label("Add").build();
        let adding = gtk::Box::builder().spacing(8).build();
        adding.append(&entry);
        adding.append(&add);
        let error = gtk::Label::builder()
            .label(NOT_A_RELAY)
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .css_classes(["error", "caption"])
            .build();
        let servers_list = gtk::Button::builder()
            .label("Use the server's list")
            .halign(gtk::Align::End)
            .css_classes(["flat"])
            .build();
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        body.append(&how);
        body.append(&list);
        body.append(&adding);
        body.append(&error);
        body.append(&servers_list);
        let done = gtk::Button::builder()
            .label("Done")
            .css_classes(["suggested-action"])
            .build();
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .css_classes(["dialog-header"])
            .build();
        header.pack_end(&done);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        let dialog = adw::Dialog::builder()
            .title(format!("Relays for {}", needs.server.name))
            .content_width(520)
            .child(&toolbar)
            .build();
        let weak = dialog.downgrade();
        done.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
        Self {
            dialog,
            client: needs.client,
            server: std::cell::RefCell::new(needs.server),
            changed: needs.changed,
            choice,
            list,
            rows: Default::default(),
            entry,
            add,
            error,
            servers_list,
        }
    }

    /// Wires the controls and lists the relays.
    fn show(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.choice.connect_selected_notify(move |row| {
            if let Some(view) = weak.upgrade() {
                let choice = choice_at(row.selected());
                row.set_subtitle(choice_detail(choice));
                view.choose(choice);
            }
        });
        let add = {
            let weak = Rc::downgrade(self);
            move || {
                let Some(view) = weak.upgrade() else { return };
                match relay_url(&view.entry.text()) {
                    Ok(url) => {
                        let list = with(&view.server.borrow().relays, url);
                        view.entry.set_text("");
                        view.set_list(list);
                    }
                    Err(_) => view.refuse(),
                }
            }
        };
        let on_activate = add.clone();
        self.entry.connect_activate(move |_| on_activate());
        self.add.connect_clicked(move |_| add());
        let weak = Rc::downgrade(self);
        self.entry.connect_changed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.entry.remove_css_class("error");
                view.error.set_visible(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.servers_list.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.set_list(Vec::new());
            }
        });
        self.list_relays();
    }

    fn list_relays(self: &Rc<Self>) {
        for row in self.rows.borrow_mut().drain(..) {
            self.list.remove(&row);
        }
        let server = self.server.borrow().clone();
        self.servers_list.set_visible(server.relays_edited);
        self.list
            .set_description(Some(&crate::markup::plain(list_caption(
                server.relays_edited,
            ))));
        let mut rows: Vec<gtk::Widget> = Vec::new();
        if server.relays.is_empty() {
            let none = adw::ActionRow::builder()
                .use_markup(false)
                .title("None")
                .build();
            rows.push(none.upcast());
        }
        for relay in &server.relays {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .css_classes(["monospace"])
                .build();
            row.set_title(relay);
            let remove = gtk::Button::builder()
                .icon_name(crate::icons::CLOSE)
                .tooltip_text("Remove")
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            let (weak, relay) = (Rc::downgrade(self), relay.clone());
            remove.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    let list: Vec<String> = view
                        .server
                        .borrow()
                        .relays
                        .iter()
                        .filter(|r| **r != relay)
                        .cloned()
                        .collect();
                    view.set_list(list);
                }
            });
            row.add_suffix(&remove);
            rows.push(row.upcast());
        }
        for row in &rows {
            self.list.add(row);
        }
        *self.rows.borrow_mut() = rows;
    }

    /// The entry turns red and says what a relay is.
    fn refuse(&self) {
        self.entry.add_css_class("error");
        self.error.set_label(NOT_A_RELAY);
        self.error.set_visible(true);
    }

    /// A refusal from the client, under the entry.
    fn refused(&self, error: &BackendError) {
        self.error.set_label(&crate::confirm::refusal(error));
        self.error.set_visible(true);
    }

    fn choose(self: &Rc<Self>, choice: Option<bool>) {
        if self.server.borrow().use_relays == choice {
            return;
        }
        let (client, key) = (self.client.clone(), self.server.borrow().key.clone());
        let weak = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let saved = client.set_server_relays(&key, choice).await;
            let Some(view) = weak.upgrade() else { return };
            match saved {
                Ok(()) => {
                    view.server.borrow_mut().use_relays = choice;
                    (view.changed)();
                }
                Err(e) => view.refused(&e),
            }
        });
    }

    /// Saves `list` as the server's relays; empty gives it back.
    fn set_list(self: &Rc<Self>, list: Vec<String>) {
        let (client, key) = (self.client.clone(), self.server.borrow().key.clone());
        let weak = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let saved = client.set_server_relay_urls(&key, list.clone()).await;
            let Some(view) = weak.upgrade() else { return };
            match saved {
                Ok(()) => {
                    {
                        let mut server = view.server.borrow_mut();
                        server.relays_edited = !list.is_empty();
                        server.relays = list;
                    }
                    view.error.set_visible(false);
                    view.list_relays();
                    (view.changed)();
                }
                Err(e) => view.refused(&e),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::ServerKey;
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::testing::ready;

    use super::*;

    fn server(use_relays: Option<bool>, relays: &[&str], edited: bool) -> Server {
        Server {
            key: ServerKey("k".into()),
            name: "Home Media".into(),
            addresses: Vec::new(),
            pinned: true,
            fingerprint: String::new(),
            verified: true,
            on_network: false,
            verified_at: None,
            last_connected_at: None,
            server_id: None,
            advertised: Vec::new(),
            relays: relays.iter().map(|r| (*r).to_owned()).collect(),
            advertised_at: None,
            use_relays,
            relays_edited: edited,
        }
    }

    #[test]
    fn the_choice_round_trips_through_its_picker() {
        for choice in [None, Some(true), Some(false)] {
            assert_eq!(choice_at(choice_index(choice)), choice);
        }
    }

    #[test]
    fn following_the_app_reads_as_the_board_draws_it() {
        assert_eq!(
            summary(&server(None, &["https://a", "https://b"], false), true),
            "App setting (On) · 2 from the server"
        );
    }

    #[test]
    fn off_says_only_off() {
        assert_eq!(
            summary(&server(Some(false), &["https://r.example"], true), true),
            "Off"
        );
    }

    #[test]
    fn an_edited_list_is_the_users() {
        assert_eq!(
            summary(&server(Some(true), &["https://a"], true), true),
            "On · 1 of yours"
        );
    }

    #[test]
    fn a_relay_is_an_https_address_with_a_host() {
        assert_eq!(
            (
                relay_url("  https://relay.example/ "),
                relay_url("relay.example"),
                relay_url("https://"),
            ),
            (
                Ok("https://relay.example/".to_owned()),
                Err(NOT_A_RELAY),
                Err(NOT_A_RELAY),
            )
        );
    }

    #[test]
    fn a_relay_already_listed_is_not_added_twice() {
        let list = vec!["https://a".to_owned()];
        assert_eq!(with(&list, "https://a".into()), list);
    }

    fn settle() {
        let context = gtk::glib::MainContext::default();
        for _ in 0..50 {
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The dialog with the fake: a bad relay is refused in place, a good
    /// one makes the list the user's, Off is saved, and "Use the server's
    /// list" gives the list back.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_relays_dialog_edits_a_servers_relays() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client = Arc::new(FakeClient::new(None));
        let home = ready(client.servers()).unwrap().remove(0);
        let view = Rc::new(View::new(Needs {
            client: client.clone(),
            server: home.clone(),
            app_relays: true,
            changed: Rc::new(|| {}),
        }));
        view.show();
        let saved = || {
            ready(client.servers())
                .unwrap()
                .into_iter()
                .find(|s| s.key == home.key)
                .unwrap()
        };

        view.entry.set_text("relay.example");
        view.add.emit_clicked();
        assert!(view.entry.has_css_class("error") && view.error.is_visible());

        view.entry.set_text("https://relay.example");
        view.add.emit_clicked();
        settle();
        assert_eq!(
            (saved().relays, saved().relays_edited),
            (vec!["https://relay.example".to_owned()], true)
        );
        assert!(view.servers_list.is_visible());

        view.choice.set_selected(2);
        settle();
        assert_eq!(saved().use_relays, Some(false));

        view.servers_list.emit_clicked();
        settle();
        assert!(!saved().relays_edited);
    }
}
