//! The main window. It opens on "Choose a server" (or straight on "Who's
//! watching" for the server used last), then shows the library: a sidebar
//! of Home, Search and the libraries, and a navigation stack on the right.
//! The stack's root is the page the sidebar picks; a card pushes its item's
//! page on top, with a Back button and swipe-back.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use relm4::{Component, ComponentParts, ComponentSender};
use spoolframe_linux_core::backend::{
    BackendError, ChosenTracks, Client, ConnectionState, HomeScreen, Profile, Selection, Server,
    ServerKey, Session,
};
use spoolframe_linux_core::theme::Theme;
use spoolframe_types::{
    Album, Artist, Item, Library, LibraryKind, MediaId, MediaTracks, Season, Show, TrackPreferences,
};

use crate::artwork::Art;
use crate::minibar::MiniBar;
use crate::mpris::Mpris;
use crate::playback::{Change, Playback, Playlist};
use crate::queue::Queued;
use crate::saved_queue::SavedQueue;
use crate::settings::Settings;
use crate::theme::ThemeApplier;
use crate::views::{Cards, Enqueue};
use crate::{library, nowplaying, pages, pin, player, preferences, start, views};
use spoolframe_linux_core::artcache::ArtworkCache;

/// What the app starts with.
pub struct Init {
    pub client: Arc<dyn Client>,
    /// The device's artwork cache, if it could be opened.
    pub artwork_cache: Option<Arc<ArtworkCache>>,
}

/// What to play, before a session is attached: see [`Playlist`].
#[derive(Debug)]
pub struct PlayRequest {
    pub items: Vec<Item>,
    pub index: usize,
    pub from: Option<Duration>,
    /// `None` lets the server choose from the user's preferences.
    pub selection: Option<Selection>,
    pub tracks: Option<MediaTracks>,
    /// The album the tracks are from, for its cover and name.
    pub album: Option<Album>,
}

impl PlayRequest {
    fn one(
        item: Item,
        from: Option<Duration>,
        selection: Option<Selection>,
        tracks: Option<MediaTracks>,
    ) -> Self {
        Self {
            items: vec![item],
            index: 0,
            from,
            selection,
            tracks,
            album: None,
        }
    }
}

pub struct App {
    client: Arc<dyn Client>,
    artwork_cache: Option<Arc<ArtworkCache>>,
    /// The signed-in user's session, once a profile is chosen.
    session: Option<Arc<dyn Session>>,
    /// The server being used, once one is chosen.
    server: Option<Server>,
    /// The profile in use, once one is entered.
    profile: Option<Profile>,
    /// The locked profile whose PIN the pad asks for (P2-08).
    unlocking: Option<Profile>,
    libraries: Vec<Library>,
    settings: Settings,
    theme: ThemeApplier,
    /// Given to every card: opens its item, loads its artwork.
    cards: Rc<Cards>,
    /// Follows the connection to the server in use; replaced on switching.
    connection_watch: Option<glib::JoinHandle<()>>,
    /// What plays, shown full or in the bar; dropping it stops it.
    playback: Option<Rc<Playback>>,
    /// The play queue (P1-23) while no playback holds it: kept from the
    /// one stopped last, for the next one.
    queued: Vec<Queued>,
    /// Where the profile's queue is saved, once a profile is chosen.
    saved_queue: Option<Rc<SavedQueue>>,
    /// The session's libraries as far as fetched, kept between visits.
    library_cache: Option<Rc<library::Cache>>,
    /// Whether the server used last has been opened already. It may be
    /// heard only on a later listing; it is opened once, not on every
    /// change after.
    opened_last_server: bool,
    /// What the search page last asked for; older answers are dropped.
    search_query: String,
    /// Bumped on each sidebar choice, so a library that loads late does
    /// not replace a newer page.
    root_generation: Rc<std::cell::Cell<u64>>,
}

pub struct AppWidgets {
    /// "servers", "profiles" or "library".
    screens: gtk::Stack,
    /// The PIN pad on the profiles screen, while it shows (P2-08).
    pin_pad: Option<Rc<pin::Pad>>,
    servers: adw::Bin,
    profiles: adw::Bin,
    toasts: adw::ToastOverlay,
    profile_avatar: adw::Avatar,
    profile_name: gtk::Label,
    profile_server: gtk::Label,
    /// Says when the connection is not up.
    banner: adw::Banner,
    sign_in: Option<start::SignIn>,
    add_server: Option<start::AddServer>,
    sidebar: gtk::ListBox,
    stack: adw::NavigationView,
    /// Above the split view: the player is pushed here, over the whole
    /// window, so the sidebar is not beside the video.
    window_stack: adw::NavigationView,
    library_page: adw::NavigationPage,
    /// What plays while the player is minimized, under the library.
    mini_bar: Rc<MiniBar>,
    /// The desktop's Now Playing and media keys (P1-24).
    mpris: Rc<Mpris>,
    root_page: adw::NavigationPage,
    /// The root page's scrolled area: `content`, or a library's grid,
    /// which must be the scrolled child itself to load row by row.
    scroller: gtk::ScrolledWindow,
    content: gtk::Box,
    search_entry: gtk::SearchEntry,
    search_results: gtk::Box,
}

#[derive(Debug)]
pub enum Msg {
    ShowServers,
    ChooseServer(Server),
    OpenAddServer,
    AddServer(String, String),
    ChooseProfile(Profile),
    /// The PIN typed for the locked profile being chosen (P2-08).
    Unlock(String),
    /// From the PIN pad: back to "Who's watching", or to the password.
    BackToProfiles,
    ForgotPin,
    OpenSignIn,
    SubmitSignIn(String, String),
    /// Back to "Who's watching" from the library.
    SwitchProfile,
    /// A sidebar row was activated: see the `ROW_*` constants.
    Select(usize),
    Search(String),
    Open(Box<Item>),
    Play(Box<PlayRequest>),
    /// A track, an album or an artist's albums to the play queue (P1-23).
    Enqueue(Box<Item>, Enqueue),
    /// Tracks of a known album to the play queue.
    EnqueueTracks(Vec<Item>, Option<Album>, Enqueue),
    SetWatched(MediaId, bool),
    /// Remember these tracks for the item (an episode: its series).
    RememberTracks(MediaId, Selection),
    /// Forget them, and reopen the item's page with the server's choice.
    ForgetTracks(Box<Item>),
    SetTrackPreferences(Box<TrackPreferences>),
    ConnectionChanged(ConnectionState),
    /// The client's server list changed (a server heard on the LAN).
    ServersChanged,
    OpenPreferences,
    SetTheme(Theme),
    /// The streaming bitrate limit (P2-06), for the next start.
    SetMaxBitrate(Option<u64>),
    /// Leave the full player and keep playing in the bar.
    MinimizePlayer,
    /// Take an item out of Continue watching (`true`), or put it back
    /// (`false`, from the toast's Undo).
    HideFromContinue(Box<Item>, bool),
    /// Connect to `server` and show its fingerprint; `true` when it is a
    /// saved server being verified.
    CheckServer(Server, bool),
    /// Save `server`: verified (It matches) or not (Skip); then, unless it
    /// was being verified, go on to "Who's watching".
    PinServer(Server, bool, bool),
    /// Replace a saved server's pin with the key that answered for it.
    TrustIdentity(ServerKey, ServerKey),
    /// Back to the full player from the bar.
    ExpandPlayer,
    StopPlayback,
}

/// Sidebar rows before the libraries.
const ROW_HOME: usize = 0;
const ROW_SEARCH: usize = 1;
const FIRST_LIBRARY_ROW: usize = 2;

/// How many search results to show.
const SEARCH_LIMIT: u32 = 60;

/// A season with its episodes.
type SeasonEpisodes = (Season, Vec<Item>);

#[derive(Debug)]
pub enum Loaded {
    Servers(Result<Vec<Server>, BackendError>),
    ServerAdded(Result<Server, BackendError>),
    /// A server connected for its fingerprint check (P1-21); the flag says
    /// whether it is a saved server being verified.
    Checked(Server, bool, Result<Server, BackendError>),
    /// The user decided at the check; the flag as for `Checked`.
    Pinned(bool, Result<Server, BackendError>),
    /// An item was taken out of Continue watching (`true`) or put back.
    Hidden(Box<Item>, bool, Result<(), BackendError>),
    /// What an enqueued item stands for: tracks, grouped by album.
    Enqueued(Enqueue, Result<Vec<AlbumTracks>, BackendError>),
    /// The user trusted a server's new identity (P1-13).
    Trusted(Result<Server, BackendError>),
    Profiles(Server, Result<Vec<Profile>, BackendError>),
    SignedIn(Result<Profile, BackendError>),
    Libraries(Result<Vec<Library>, BackendError>),
    Home(Result<HomeScreen, BackendError>),
    Search(String, Result<Vec<Item>, BackendError>),
    /// A movie or episode, what it belongs to (an episode's show), and its
    /// tracks for the pickers.
    Playable(
        Item,
        Option<String>,
        Result<MediaTracks, BackendError>,
        Option<ChosenTracks>,
    ),
    Show(Show, Result<Vec<SeasonEpisodes>, BackendError>),
    Season(Season, Result<Vec<Item>, BackendError>),
    Artist(Artist, Result<Vec<Item>, BackendError>),
    /// An album, its artist's name and its tracks.
    Album(Album, Result<(String, Vec<Item>), BackendError>),
    WatchChanged(Result<(), BackendError>),
    /// A change the user does not need to hear about unless it failed.
    Saved(Result<(), BackendError>),
    TracksForgotten(Box<Item>, Result<(), BackendError>),
    Preferences(Result<TrackPreferences, BackendError>),
    /// What the server said to the PIN typed for a locked profile.
    Unlocked(Box<Profile>, Result<(), BackendError>),
}

impl Loaded {
    /// Whether the session behind this answer is gone. Any session call
    /// can find out; the app then asks the user to sign in again.
    fn sign_in_required(&self) -> bool {
        self.session_error() == Some(&BackendError::SignInRequired)
    }

    /// Whether the profile turned out locked mid-session (P2-08): another
    /// profile used the connection, or the requirement was just turned on
    /// from elsewhere. The app asks for the PIN again.
    fn profile_locked(&self) -> bool {
        self.session_error() == Some(&BackendError::ProfileLocked)
    }

    /// The error of an answer that comes from the profile's session.
    fn session_error(&self) -> Option<&BackendError> {
        match self {
            Loaded::Libraries(Err(e))
            | Loaded::Home(Err(e))
            | Loaded::Search(_, Err(e))
            | Loaded::Show(_, Err(e))
            | Loaded::Season(_, Err(e))
            | Loaded::Artist(_, Err(e))
            | Loaded::Album(_, Err(e))
            | Loaded::WatchChanged(Err(e))
            | Loaded::Hidden(_, _, Err(e))
            | Loaded::Enqueued(_, Err(e))
            | Loaded::Saved(Err(e))
            | Loaded::TracksForgotten(_, Err(e))
            | Loaded::Preferences(Err(e)) => Some(e),
            _ => None,
        }
    }
}

impl Component for App {
    type Init = Init;
    type Input = Msg;
    type Output = ();
    type CommandOutput = Loaded;
    type Root = adw::ApplicationWindow;
    type Widgets = AppWidgets;

    fn init_root() -> Self::Root {
        adw::ApplicationWindow::builder()
            .title("Spoolframe")
            .default_width(1280)
            .default_height(800)
            .build()
    }

    fn init(
        Init {
            client,
            artwork_cache,
        }: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let settings = Settings::load();
        let theme = ThemeApplier::new();
        theme.apply(settings.theme());

        let sidebar = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .build();
        sidebar.append(&sidebar_row("Home", crate::icons::HOME));
        sidebar.append(&sidebar_row("Search", crate::icons::SEARCH));
        // A rule between Home and Search and the libraries, as designed.
        sidebar.set_header_func(|row, _| {
            let first_library = row.index() == FIRST_LIBRARY_ROW as i32;
            row.set_header(
                first_library
                    .then(|| {
                        gtk::Separator::builder()
                            .margin_top(8)
                            .margin_bottom(8)
                            .build()
                    })
                    .as_ref(),
            );
        });
        {
            let sender = sender.clone();
            sidebar.connect_row_activated(move |_, row| {
                sender.input(Msg::Select(row.index() as usize));
            });
        }
        // Who is watching, at the foot of the sidebar; it switches profile.
        let profile_avatar = adw::Avatar::new(34, None, true);
        let profile_name = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        let profile_server = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        let names = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        names.append(&profile_name);
        names.append(&profile_server);
        let profile_body = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        profile_body.append(&profile_avatar);
        profile_body.append(&names);
        profile_body.append(&gtk::Image::from_icon_name(crate::icons::DOWN));
        let profile_button = gtk::Button::builder()
            .child(&profile_body)
            .tooltip_text("Switch profile")
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .css_classes(["flat"])
            .build();
        {
            let sender = sender.clone();
            profile_button.connect_clicked(move |_| sender.input(Msg::SwitchProfile));
        }
        // The mark and the name head the sidebar.
        let brand = gtk::Box::builder().spacing(10).build();
        brand.append(&crate::logo::mark(26));
        brand.append(
            &gtk::Label::builder()
                .label("Spoolframe")
                .css_classes(["brand"])
                .build(),
        );
        let sidebar_header = adw::HeaderBar::builder().title_widget(&brand).build();
        let sidebar_toolbar = toolbar(
            &sidebar_header,
            &gtk::ScrolledWindow::builder()
                .child(&sidebar)
                .vexpand(true)
                .build(),
        );
        sidebar_toolbar.add_bottom_bar(&profile_button);
        let sidebar_page = adw::NavigationPage::builder()
            .title("Spoolframe")
            .child(&sidebar_toolbar)
            .build();

        // The main menu, as in every libadwaita app; its items are window
        // actions.
        let preferences = gtk::gio::SimpleAction::new("preferences", None);
        {
            let sender = sender.clone();
            preferences.connect_activate(move |_, _| sender.input(Msg::OpenPreferences));
        }
        root.add_action(&preferences);
        let menu = gtk::gio::Menu::new();
        menu.append(Some("Preferences"), Some("win.preferences"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name(crate::icons::MENU)
            .tooltip_text("Main menu")
            .menu_model(&menu)
            .primary(true)
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&menu_button);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(32)
            .margin_top(28)
            .margin_bottom(28)
            .margin_start(28)
            .margin_end(28)
            .build();
        let banner = adw::Banner::new("");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&content)
            .vexpand(true)
            .build();
        let root_toolbar = toolbar(&header, &scroller);
        root_toolbar.add_top_bar(&banner);
        let root_page = adw::NavigationPage::builder()
            .title("Home")
            .child(&root_toolbar)
            .build();
        let stack = adw::NavigationView::new();
        stack.add(&root_page);
        let content_page = adw::NavigationPage::builder()
            .title("Spoolframe")
            .child(&stack)
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Movies, shows, episodes, music")
            .hexpand(true)
            .build();
        {
            let sender = sender.clone();
            // search-changed is already debounced by the entry.
            search_entry.connect_search_changed(move |entry| {
                sender.input(Msg::Search(entry.text().trim().to_owned()));
            });
        }
        let search_results = gtk::Box::new(gtk::Orientation::Vertical, 24);

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content_page)
            .build();
        // A narrow window shows one pane at a time, sidebar or content.
        let narrow = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 760sp").expect("a valid condition"),
        );
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        root.add_breakpoint(narrow);
        // Collapsed, picking a row goes on to its pane.
        sidebar.connect_row_activated(glib::clone!(
            #[weak]
            split,
            move |_, _| split.set_show_content(true)
        ));
        // Breakpoints need a smallest size to measure against.
        root.set_size_request(360, 480);

        let servers = adw::Bin::new();
        let profiles = adw::Bin::new();
        // Each screen is as wide as it needs: a stack is as wide as its
        // widest child by default, so in a narrow window the library's
        // width pushed "Who's watching" off centre.
        let screens = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .build();
        screens.add_named(&servers, Some("servers"));
        screens.add_named(&profiles, Some("profiles"));
        let mini_bar = MiniBar::new(
            {
                let sender = sender.clone();
                Rc::new(move || sender.input(Msg::ExpandPlayer))
            },
            {
                let sender = sender.clone();
                Rc::new(move || sender.input(Msg::StopPlayback))
            },
        );
        let mpris = Mpris::start(
            {
                let (sender, window) = (sender.clone(), root.clone());
                Rc::new(move || {
                    window.present();
                    sender.input(Msg::ExpandPlayer);
                })
            },
            {
                let sender = sender.clone();
                Rc::new(move || sender.input(Msg::StopPlayback))
            },
            {
                let window = root.clone();
                Rc::new(move || window.close())
            },
        );
        let library = gtk::Box::new(gtk::Orientation::Vertical, 0);
        split.set_vexpand(true);
        library.append(&split);
        library.append(&mini_bar.revealer);
        let library_page = adw::NavigationPage::builder()
            .title("Spoolframe")
            .child(&library)
            .build();
        // Space plays and pauses what plays, wherever the focus is (the
        // library, the player, Now Playing), except where Space types (a
        // search entry, a dialog field).
        let space_keys = {
            let keys = gtk::EventControllerKey::builder()
                .propagation_phase(gtk::PropagationPhase::Capture)
                .build();
            let bar = mini_bar.clone();
            keys.connect_key_pressed(move |keys, key, _, modifiers| {
                let typing = keys
                    .widget()
                    .and_then(|w| w.root())
                    .and_then(|root| root.focus())
                    .is_some_and(|focus| focus.is::<gtk::Text>() || focus.is::<gtk::TextView>());
                if key == gtk::gdk::Key::space && modifiers.is_empty() && !typing && bar.toggle() {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            keys
        };
        let window_stack = adw::NavigationView::new();
        window_stack.add(&library_page);
        window_stack.add_controller(space_keys);
        screens.add_named(&window_stack, Some("library"));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&screens));
        root.set_content(Some(&toasts));

        let model = App {
            client,
            artwork_cache,
            session: None,
            server: None,
            profile: None,
            unlocking: None,
            libraries: Vec::new(),
            settings,
            theme,
            cards: Rc::new(Cards {
                open: open_with(&sender),
                art: None,
                set_watched: None,
                hide: None,
                enqueue: None,
            }),
            connection_watch: None,
            playback: None,
            queued: Vec::new(),
            saved_queue: None,
            library_cache: None,
            opened_last_server: false,
            search_query: String::new(),
            root_generation: Rc::new(std::cell::Cell::new(0)),
        };
        model.load_servers(&sender);
        // Discovery hears servers a moment after the client opens, so the
        // first list is often empty; it is listed again on each change.
        let mut changes = model.client.servers_changed();
        let changed = sender.clone();
        glib::spawn_future_local(async move {
            while changes.changed().await.is_ok() {
                changed.input(Msg::ServersChanged);
            }
        });
        ComponentParts {
            model,
            widgets: AppWidgets {
                screens,
                servers,
                profiles,
                toasts,
                profile_avatar,
                profile_name,
                profile_server,
                banner,
                sign_in: None,
                pin_pad: None,
                add_server: None,
                sidebar,
                stack,
                root_page,
                window_stack,
                library_page,
                mini_bar,
                mpris,
                scroller,
                content,
                search_entry,
                search_results,
            },
        }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match msg {
            Msg::ShowServers => {
                widgets.screens.set_visible_child_name("servers");
                self.load_servers(&sender);
            }
            // Listed again only while the picker shows; elsewhere the
            // list is fetched fresh when the picker opens.
            Msg::ServersChanged => {
                if widgets.screens.visible_child_name().as_deref() == Some("servers") {
                    self.load_servers(&sender);
                }
            }
            // A server not saved yet has its fingerprint checked first.
            Msg::ChooseServer(server) if server.pinned => self.load_profiles(server, &sender),
            Msg::ChooseServer(server) => sender.input(Msg::CheckServer(server, false)),
            Msg::HideFromContinue(item, hide) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                sender.oneshot_command(async move {
                    let done = if hide {
                        session.hide_from_continue(item.id()).await
                    } else {
                        session.unhide_from_continue(item.id()).await
                    };
                    Loaded::Hidden(item, hide, done)
                });
            }
            Msg::CheckServer(server, verifying) => {
                let client = self.client.clone();
                sender.oneshot_command(async move {
                    let checked = client.connect(&server.key).await;
                    Loaded::Checked(server, verifying, checked)
                });
            }
            Msg::PinServer(server, verified, verifying) => {
                let client = self.client.clone();
                sender.oneshot_command(async move {
                    Loaded::Pinned(verifying, client.pin(&server.key, verified).await)
                });
            }
            Msg::TrustIdentity(saved, now) => {
                let client = self.client.clone();
                sender.oneshot_command(async move {
                    Loaded::Trusted(client.trust_new_identity(&saved, &now).await)
                });
            }
            Msg::OpenAddServer => {
                let submit: Rc<dyn Fn(String, String)> = {
                    let sender = sender.clone();
                    Rc::new(move |key, address| sender.input(Msg::AddServer(key, address)))
                };
                let dialog = start::AddServer::new(submit);
                dialog.dialog.present(Some(root));
                widgets.add_server = Some(dialog);
            }
            Msg::AddServer(key, address) => {
                if let Some(dialog) = &widgets.add_server {
                    if key.is_empty() || address.is_empty() {
                        return dialog.refuse("Both the key and an address are needed.");
                    }
                    dialog.set_busy(true);
                }
                let client = self.client.clone();
                sender.oneshot_command(async move {
                    Loaded::ServerAdded(client.add_address(&ServerKey(key), &address).await)
                });
            }
            Msg::ChooseProfile(profile) if profile.locked => {
                self.ask_pin(profile, widgets, &sender)
            }
            Msg::ChooseProfile(profile) => self.enter(profile, widgets, &sender),
            Msg::Unlock(pin) => {
                let Some(profile) = self.unlocking.clone() else {
                    return;
                };
                let session = match self.client.session(&profile) {
                    Ok(session) => session,
                    Err(e) => return toast(widgets, &e),
                };
                sender.oneshot_command(async move {
                    let unlocked = session.unlock(&pin).await;
                    Loaded::Unlocked(Box::new(profile), unlocked)
                });
            }
            Msg::BackToProfiles => {
                self.unlocking = None;
                widgets.pin_pad = None;
                match self.server.clone() {
                    Some(server) => self.load_profiles(server, &sender),
                    None => sender.input(Msg::ShowServers),
                }
            }
            Msg::ForgotPin => sender.input(Msg::OpenSignIn),
            Msg::OpenSignIn => {
                let Some(server) = &self.server else { return };
                let submit: Rc<dyn Fn(String, String)> = {
                    let sender = sender.clone();
                    Rc::new(move |u, p| sender.input(Msg::SubmitSignIn(u, p)))
                };
                let dialog = start::SignIn::new(server, submit);
                dialog.dialog.present(Some(root));
                widgets.sign_in = Some(dialog);
            }
            Msg::SubmitSignIn(username, password) => {
                let Some(server) = self.server.clone() else {
                    return;
                };
                if let Some(dialog) = &widgets.sign_in {
                    dialog.set_busy(true);
                }
                let client = self.client.clone();
                sender.oneshot_command(async move {
                    Loaded::SignedIn(client.sign_in(&server.key, &username, &password).await)
                });
            }
            Msg::SwitchProfile => {
                if let Some(server) = self.server.clone() {
                    self.load_profiles(server, &sender);
                }
            }
            Msg::Select(ROW_HOME) => {
                self.root_generation.set(self.root_generation.get() + 1);
                widgets.stack.pop_to_page(&widgets.root_page);
                self.load_home(&sender);
            }
            Msg::Select(ROW_SEARCH) => {
                self.root_generation.set(self.root_generation.get() + 1);
                widgets.stack.pop_to_page(&widgets.root_page);
                widgets.root_page.set_title("Search");
                show_content(widgets);
                clear(&widgets.content);
                widgets.content.append(&widgets.search_entry);
                widgets.content.append(&widgets.search_results);
                widgets.search_entry.grab_focus();
            }
            Msg::Select(row) => {
                let Some(library) = self.libraries.get(row - FIRST_LIBRARY_ROW).cloned() else {
                    return;
                };
                let Some(cache) = self.library_cache.clone() else {
                    return;
                };
                widgets.stack.pop_to_page(&widgets.root_page);
                widgets.root_page.set_title(&library.title);
                show_content(widgets);
                clear(&widgets.content);
                // Pulsing blank posters until the first page arrives, so
                // the library does not land on an empty page all at once.
                let skeleton =
                    views::grid(&library.title, (0..18).map(|_| views::placeholder_card()));
                skeleton.add_css_class("loading");
                widgets.content.append(&skeleton);
                // A newer choice in the sidebar makes this one stale.
                let generation = self.root_generation.get() + 1;
                self.root_generation.set(generation);
                let current = self.root_generation.clone();
                let (scroller, content) = (widgets.scroller.clone(), widgets.content.clone());
                let cards = self.cards.clone();
                glib::spawn_future_local(async move {
                    let grid = cache.grid(library, cards).await;
                    if current.get() != generation {
                        return;
                    }
                    match grid {
                        Ok(grid) => scroller.set_child(Some(&grid)),
                        Err(e) => {
                            clear(&content);
                            content.append(&error_status(&e));
                        }
                    }
                });
            }
            Msg::Search(query) => {
                self.search_query = query.clone();
                if query.is_empty() {
                    clear(&widgets.search_results);
                    return;
                }
                let Some(session) = self.session.clone() else {
                    return;
                };
                sender.oneshot_command(async move {
                    let items = session.search(&query, SEARCH_LIMIT).await;
                    Loaded::Search(query, items)
                });
            }
            Msg::Open(item) => self.open_item(*item, &sender),
            Msg::Play(request) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                let PlayRequest {
                    items,
                    index,
                    from,
                    selection,
                    tracks,
                    album,
                } = *request;
                // A new context keeps the queue, as Spotify does.
                self.stop_playback(widgets);
                match Playback::start(Playlist {
                    session,
                    items,
                    index,
                    queued: std::mem::take(&mut self.queued),
                    from,
                    selection,
                    tracks,
                    album,
                }) {
                    Ok(playback) => {
                        self.attach_playback(&playback, widgets);
                        widgets
                            .window_stack
                            .push(&player_page(&playback, &sender, &self.cards));
                        self.playback = Some(playback);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "cannot build the player");
                        toast_text(widgets, &format!("Cannot play: {e}"));
                    }
                }
            }
            Msg::MinimizePlayer => {
                widgets.window_stack.pop_to_page(&widgets.library_page);
            }
            Msg::ExpandPlayer => {
                if let Some(playback) = &self.playback
                    && widgets.window_stack.visible_page().as_ref() == Some(&widgets.library_page)
                {
                    widgets
                        .window_stack
                        .push(&player_page(playback, &sender, &self.cards));
                }
            }
            Msg::StopPlayback => self.stop_playback(widgets),
            Msg::Enqueue(item, at) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                sender.oneshot_command(async move {
                    Loaded::Enqueued(at, tracks_of(session.as_ref(), *item).await)
                });
            }
            Msg::EnqueueTracks(items, album, at) => {
                self.enqueue(vec![(items, album)], at, widgets, &sender)
            }
            Msg::SetWatched(id, watched) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                sender.oneshot_command(async move {
                    let done = if watched {
                        session.mark_watched(&id).await
                    } else {
                        session.mark_unwatched(&id).await
                    };
                    Loaded::WatchChanged(done)
                });
            }
            Msg::ConnectionChanged(state) => show_connection(widgets, self.server.as_ref(), state),
            Msg::RememberTracks(id, selection) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                sender.oneshot_command(async move {
                    Loaded::Saved(session.set_track_override(&id, selection).await)
                });
            }
            Msg::ForgetTracks(item) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                sender.oneshot_command(async move {
                    let done = session.clear_track_override(item.id()).await;
                    Loaded::TracksForgotten(item, done)
                });
            }
            Msg::SetTrackPreferences(preferences) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                sender.oneshot_command(async move {
                    Loaded::Saved(session.set_track_preferences(&preferences).await)
                });
            }
            Msg::OpenPreferences => match self.session.clone() {
                // The Playback page shows the user's preferences, from the
                // server.
                Some(session) => sender.oneshot_command(async move {
                    Loaded::Preferences(session.track_preferences().await)
                }),
                None => self.preferences_dialog(None, &sender).present(Some(root)),
            },
            Msg::SetTheme(theme) => {
                self.theme.apply(theme);
                self.settings.set_theme(theme);
            }
            Msg::SetMaxBitrate(limit) => {
                // The server learns it with the device's capabilities, at
                // the next start.
                self.settings.set_max_bitrate(limit);
                toast_text(
                    widgets,
                    &format!(
                        "Bitrate limit: {} · applies when Spoolframe next starts",
                        preferences::bitrate_name(limit)
                    ),
                );
            }
        }
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        loaded: Self::CommandOutput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        if loaded.sign_in_required() {
            return self.signed_out(widgets, &sender);
        }
        if loaded.profile_locked() {
            return self.locked_mid_session(widgets, &sender);
        }
        let cards = self.cards.clone();
        let cards = cards.as_ref();
        match loaded {
            Loaded::Servers(Ok(servers)) => {
                // Open on the server used last, if this device still knows it.
                let last = self.settings.server();
                if self.server.is_none()
                    && !self.opened_last_server
                    && let Some(server) = servers.iter().find(|s| Some(&s.key.0) == last.as_ref())
                {
                    self.opened_last_server = true;
                    self.load_profiles(server.clone(), &sender);
                }
                let choose: start::Choose<Server> = {
                    let sender = sender.clone();
                    Rc::new(move |s| sender.input(Msg::ChooseServer(s)))
                };
                let add: Rc<dyn Fn()> = {
                    let sender = sender.clone();
                    Rc::new(move || sender.input(Msg::OpenAddServer))
                };
                let verify: start::Choose<Server> = {
                    let sender = sender.clone();
                    Rc::new(move |s| sender.input(Msg::CheckServer(s, true)))
                };
                widgets
                    .servers
                    .set_child(Some(&start::servers(&servers, &choose, &verify, &add)));
                if self.server.is_none() {
                    widgets.screens.set_visible_child_name("servers");
                }
            }
            Loaded::Servers(Err(e)) => toast(widgets, &e),
            Loaded::ServerAdded(Ok(server)) => {
                if let Some(dialog) = widgets.add_server.take() {
                    dialog.dialog.close();
                }
                self.load_servers(&sender);
                if server.pinned {
                    self.load_profiles(server, &sender);
                } else {
                    // Connected and learned its fingerprint: check it.
                    check_fingerprint(&server, false, &sender, root);
                }
            }
            Loaded::ServerAdded(Err(e)) => match &widgets.add_server {
                Some(dialog) if !matches!(e, BackendError::IdentityChanged { .. }) => {
                    dialog.refuse(&e.to_string())
                }
                _ => {
                    if let Some(dialog) = widgets.add_server.take() {
                        dialog.dialog.close();
                    }
                    server_error(widgets, "This server", &e, &sender, root);
                }
            },
            Loaded::Checked(_, verifying, Ok(server)) => {
                check_fingerprint(&server, verifying, &sender, root)
            }
            Loaded::Checked(server, _, Err(e)) => {
                server_error(widgets, &server.name, &e, &sender, root)
            }
            Loaded::Pinned(verifying, Ok(server)) => {
                self.load_servers(&sender);
                if verifying {
                    toast_text(widgets, &format!("{} is verified", server.name));
                    if self.server.as_ref().is_some_and(|s| s.key == server.key) {
                        self.server = Some(server);
                    }
                } else {
                    self.load_profiles(server, &sender);
                }
            }
            Loaded::Pinned(_, Err(e)) => server_error(widgets, "This server", &e, &sender, root),
            Loaded::Hidden(item, hide, Ok(())) => {
                self.load_home(&sender);
                if hide {
                    let title = views::Facts::of(&item).caption();
                    let toast = adw::Toast::builder()
                        .title(format!("{title} removed from Continue watching"))
                        .button_label("Undo")
                        .build();
                    let sender = sender.clone();
                    toast.connect_button_clicked(move |_| {
                        sender.input(Msg::HideFromContinue(item.clone(), false))
                    });
                    widgets.toasts.add_toast(toast);
                }
            }
            Loaded::Hidden(_, _, Err(e)) => toast(widgets, &e),
            Loaded::Unlocked(profile, Ok(())) => {
                self.unlocking = None;
                widgets.pin_pad = None;
                let profile = Profile {
                    locked: false,
                    ..*profile
                };
                self.enter(profile, widgets, &sender);
            }
            Loaded::Unlocked(_, Err(e)) => self.pin_refused(&e, widgets, &sender),
            Loaded::Enqueued(at, Ok(groups)) => self.enqueue(groups, at, widgets, &sender),
            Loaded::Enqueued(_, Err(e)) => toast(widgets, &e),
            Loaded::Trusted(Ok(server)) => {
                self.load_servers(&sender);
                self.load_profiles(server, &sender);
            }
            Loaded::Trusted(Err(e)) => toast(widgets, &e),
            Loaded::Profiles(server, Ok(profiles)) => {
                let choose: start::Choose<Profile> = {
                    let sender = sender.clone();
                    Rc::new(move |p| sender.input(Msg::ChooseProfile(p)))
                };
                let sign_in: Rc<dyn Fn()> = {
                    let sender = sender.clone();
                    Rc::new(move || sender.input(Msg::OpenSignIn))
                };
                let back: Rc<dyn Fn()> = {
                    let sender = sender.clone();
                    Rc::new(move || sender.input(Msg::ShowServers))
                };
                widgets.profiles.set_child(Some(&start::profiles(
                    &server, &profiles, &choose, &sign_in, &back,
                )));
                widgets.screens.set_visible_child_name("profiles");
                self.server = Some(server);
                // Nobody signed in here yet: go straight to signing in.
                if profiles.is_empty() {
                    sender.input(Msg::OpenSignIn);
                }
            }
            Loaded::Profiles(server, Err(e)) => {
                tracing::warn!(server = %server.name, error = %e, "cannot list profiles");
                server_error(widgets, &server.name, &e, &sender, root);
                widgets.screens.set_visible_child_name("servers");
            }
            Loaded::SignedIn(Ok(profile)) => {
                if let Some(dialog) = widgets.sign_in.take() {
                    dialog.dialog.close();
                }
                self.enter(profile, widgets, &sender);
            }
            Loaded::SignedIn(Err(e)) => match &widgets.sign_in {
                Some(dialog) => dialog.refuse(&e.to_string()),
                None => toast(widgets, &e),
            },
            Loaded::Libraries(Ok(libraries)) => {
                for library in &libraries {
                    widgets
                        .sidebar
                        .append(&sidebar_row(&library.title, library_icon(library.kind)));
                }
                // Fetched ahead, so opening one rarely waits.
                if let Some(cache) = &self.library_cache {
                    for library in &libraries {
                        cache.prefetch(library.clone());
                    }
                }
                self.libraries = libraries;
            }
            Loaded::Libraries(Err(e)) => show_error(widgets, "Home", &e),
            Loaded::Home(Ok(home)) => {
                let mut rows = Vec::new();
                // A row with nothing in it is left out, not shown empty.
                if !home.continue_watching.is_empty() {
                    rows.push(views::row(
                        "Continue watching",
                        home.continue_watching
                            .iter()
                            .map(|i| views::continue_card(i, cards)),
                    ));
                }
                if !home.next_up.is_empty() {
                    rows.push(views::row(
                        "Next up",
                        home.next_up.iter().map(|i| views::continue_card(i, cards)),
                    ));
                }
                if !home.recently_added.is_empty() {
                    rows.push(views::row(
                        "Recently added",
                        home.recently_added
                            .iter()
                            .map(|i| views::poster_card(i, cards)),
                    ));
                }
                show(widgets, "Home", rows);
            }
            Loaded::Home(Err(e)) => show_error(widgets, "Home", &e),
            Loaded::Search(query, result) => {
                // An answer to an older query, overtaken while typing.
                if query != self.search_query {
                    return;
                }
                clear(&widgets.search_results);
                match result {
                    Ok(items) if items.is_empty() => widgets.search_results.append(
                        &adw::StatusPage::builder()
                            .icon_name(crate::icons::SEARCH)
                            .title("Nothing found")
                            .description(format!("Nothing matches “{query}”."))
                            .build(),
                    ),
                    Ok(items) => widgets.search_results.append(&views::grid(
                        "Results",
                        items.iter().map(|i| views::poster_card(i, cards)),
                    )),
                    Err(e) => widgets.search_results.append(&error_status(&e)),
                }
            }
            Loaded::Playable(item, context, tracks, chosen) => {
                let title = views::Facts::of(&item).title;
                // The page opens without pickers if the tracks cannot be
                // had; playback then takes the server's choice.
                let tracks = tracks
                    .inspect_err(|e| tracing::warn!(error = %e, "no track list"))
                    .ok();
                let play: pages::Play = {
                    let (sender, item, tracks) = (sender.clone(), item.clone(), tracks.clone());
                    Rc::new(move |from, selection| {
                        sender.input(Msg::Play(Box::new(PlayRequest::one(
                            item.clone(),
                            from,
                            selection,
                            tracks.clone(),
                        ))))
                    })
                };
                let id = item.id().clone();
                let actions = pages::PlayableActions {
                    play,
                    set_watched: {
                        let (sender, id) = (sender.clone(), id.clone());
                        Rc::new(move |watched| sender.input(Msg::SetWatched(id.clone(), watched)))
                    },
                    remember: {
                        let (sender, id) = (sender.clone(), id.clone());
                        Rc::new(move |selection| {
                            sender.input(Msg::RememberTracks(id.clone(), selection))
                        })
                    },
                    forget: {
                        let (sender, item) = (sender.clone(), item.clone());
                        Rc::new(move || sender.input(Msg::ForgetTracks(Box::new(item.clone()))))
                    },
                };
                push(
                    widgets,
                    &title,
                    &pages::playable(
                        &item,
                        context.as_deref(),
                        tracks.as_ref(),
                        chosen.as_ref(),
                        actions,
                        cards,
                    ),
                );
            }
            Loaded::Show(show, Ok(seasons)) => {
                push(widgets, &show.title, &pages::show(&show, &seasons, cards));
            }
            Loaded::Show(show, Err(e)) => push_error(widgets, &show.title, &e),
            Loaded::Season(season, Ok(episodes)) => {
                push(
                    widgets,
                    &season.title,
                    &pages::season(&season, &episodes, cards),
                );
            }
            Loaded::Season(season, Err(e)) => push_error(widgets, &season.title, &e),
            Loaded::Artist(artist, Ok(albums)) => {
                push(
                    widgets,
                    &artist.name,
                    &pages::artist(&artist, &albums, cards),
                );
            }
            Loaded::Artist(artist, Err(e)) => push_error(widgets, &artist.name, &e),
            Loaded::Album(album, Ok((artist, tracks))) => {
                let play: pages::PlayTracks = {
                    let (sender, album) = (sender.clone(), album.clone());
                    Rc::new(move |items, index| {
                        sender.input(Msg::Play(Box::new(PlayRequest {
                            items,
                            index,
                            from: None,
                            selection: None,
                            tracks: None,
                            album: Some(album.clone()),
                        })))
                    })
                };
                let enqueue: pages::EnqueueTracks = {
                    let (sender, album) = (sender.clone(), album.clone());
                    Rc::new(move |items, at| {
                        sender.input(Msg::EnqueueTracks(items, Some(album.clone()), at))
                    })
                };
                push(
                    widgets,
                    &album.title,
                    &pages::album(&album, &artist, &tracks, play, enqueue, cards),
                );
            }
            Loaded::Album(album, Err(e)) => push_error(widgets, &album.title, &e),
            // Home shows watch state (Continue watching, Next up): show it
            // again, if it is what is on screen.
            Loaded::WatchChanged(Ok(())) => {
                let on_home = widgets.root_page.title() == "Home"
                    && widgets.stack.visible_page().as_ref() == Some(&widgets.root_page);
                if on_home {
                    self.load_home(&sender);
                }
            }
            Loaded::WatchChanged(Err(e)) => toast(widgets, &e),
            Loaded::Saved(Ok(())) => {}
            Loaded::Saved(Err(e)) => toast(widgets, &e),
            Loaded::TracksForgotten(item, Ok(())) => {
                toast_text(widgets, "Back to your default tracks");
                // Reopen the page so its pickers show the server's choice.
                widgets.stack.pop();
                self.open_item(*item, &sender);
            }
            Loaded::TracksForgotten(_, Err(e)) => toast(widgets, &e),
            Loaded::Preferences(result) => {
                // Without the server's preferences, the dialog still has
                // Appearance.
                let tracks = result
                    .inspect_err(|e| tracing::warn!(error = %e, "no track preferences"))
                    .ok();
                self.preferences_dialog(tracks, &sender).present(Some(root));
            }
        }
    }
}

impl App {
    fn preferences_dialog(
        &self,
        tracks: Option<TrackPreferences>,
        sender: &ComponentSender<Self>,
    ) -> adw::PreferencesDialog {
        let set_theme: Rc<dyn Fn(Theme)> = {
            let sender = sender.clone();
            Rc::new(move |theme| sender.input(Msg::SetTheme(theme)))
        };
        let set_tracks: Rc<dyn Fn(TrackPreferences)> = {
            let sender = sender.clone();
            Rc::new(move |p| sender.input(Msg::SetTrackPreferences(Box::new(p))))
        };
        let server = self.server.clone().map(|server| {
            let sender = sender.clone();
            let check = server.clone();
            let verify: Rc<dyn Fn()> =
                Rc::new(move || sender.input(Msg::CheckServer(check.clone(), true)));
            (server, verify)
        });
        let set_bitrate: preferences::SetBitrate = {
            let sender = sender.clone();
            Rc::new(move |limit| sender.input(Msg::SetMaxBitrate(limit)))
        };
        preferences::dialog(
            self.settings.theme(),
            tracks,
            server,
            set_theme,
            set_tracks,
            (self.settings.max_bitrate(), set_bitrate),
            self.signed(),
        )
    }

    fn load_servers(&self, sender: &ComponentSender<Self>) {
        let client = self.client.clone();
        sender.oneshot_command(async move { Loaded::Servers(client.servers().await) });
    }

    fn load_profiles(&self, server: Server, sender: &ComponentSender<Self>) {
        let client = self.client.clone();
        sender.oneshot_command(async move {
            let profiles = client.profiles(&server.key).await;
            Loaded::Profiles(server, profiles)
        });
    }

    /// Opens the library as `profile`: its own session on the same
    /// connection, a fresh sidebar and Home.
    fn enter(
        &mut self,
        profile: Profile,
        widgets: &mut AppWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let session = match self.client.session(&profile) {
            Ok(session) => session,
            Err(e) => return toast(widgets, &e),
        };
        self.cards = Rc::new(Cards {
            open: open_with(sender),
            art: Some(Art::new(
                session.clone(),
                profile.server.clone(),
                self.artwork_cache.clone(),
            )),
            set_watched: Some({
                let sender = sender.clone();
                Rc::new(move |id, watched| sender.input(Msg::SetWatched(id, watched)))
            }),
            hide: Some({
                let sender = sender.clone();
                Rc::new(move |item| sender.input(Msg::HideFromContinue(Box::new(item), true)))
            }),
            enqueue: Some({
                let sender = sender.clone();
                Rc::new(move |item, at| sender.input(Msg::Enqueue(Box::new(item), at)))
            }),
        });
        self.library_cache = Some(library::Cache::new(session.clone()));
        self.session = Some(session);
        self.profile = Some(profile.clone());
        self.settings.set_server(&profile.server.0);
        widgets.profile_avatar.set_text(Some(&profile.display_name));
        widgets.profile_name.set_label(&profile.display_name);
        self.watch_connection(&profile.server, sender);
        while let Some(row) = widgets.sidebar.row_at_index(FIRST_LIBRARY_ROW as i32) {
            widgets.sidebar.remove(&row);
        }
        self.libraries.clear();
        self.search_query.clear();
        widgets.search_entry.set_text("");
        clear(&widgets.search_results);
        self.stop_playback(widgets);
        // The queue was the last profile's; this one's comes back paused.
        self.queued.clear();
        self.saved_queue = SavedQueue::for_profile(&profile).map(Rc::new);
        self.restore_queue(widgets);
        widgets.stack.pop_to_page(&widgets.root_page);
        clear(&widgets.content);
        widgets.screens.set_visible_child_name("library");
        self.load_libraries(sender);
        self.load_home(sender);
    }

    /// Follows the connection to `server` and shows its state, replacing
    /// the watch on the previous server.
    fn watch_connection(&mut self, server: &ServerKey, sender: &ComponentSender<Self>) {
        if let Some(watch) = self.connection_watch.take() {
            watch.abort();
        }
        let mut states = self.client.connection(server);
        let sender = sender.clone();
        self.connection_watch = Some(glib::spawn_future_local(async move {
            loop {
                let state = *states.borrow_and_update();
                sender.input(Msg::ConnectionChanged(state));
                if states.changed().await.is_err() {
                    break;
                }
            }
        }));
    }

    /// Shows `playback` in the bar and to the desktop, and saves its queue
    /// as it changes.
    fn attach_playback(&self, playback: &Rc<Playback>, widgets: &AppWidgets) {
        // A queued item that cannot play is passed over with a word, not
        // a stop (P1-23).
        let toasts = widgets.toasts.clone();
        playback.subscribe(move |change| {
            if let Change::Skipped(message) = change {
                toasts.add_toast(adw::Toast::new(message));
            }
            true
        });
        if let Some(saved) = self.saved_queue.clone() {
            save_queue_on_changes(playback, saved);
        }
        widgets.mini_bar.attach(playback, &self.cards);
        widgets.mpris.attach(playback, self.cards.art.clone());
    }

    /// Brings back the profile's queue from the last run, paused in the
    /// bar.
    fn restore_queue(&mut self, widgets: &AppWidgets) {
        let (Some(session), Some(saved)) = (self.session.clone(), self.saved_queue.clone()) else {
            return;
        };
        let Some((queue, at)) = saved.load() else {
            return;
        };
        if queue.current().is_none() {
            return;
        }
        match Playback::restore(session, queue, at) {
            Ok(playback) => {
                self.attach_playback(&playback, widgets);
                self.playback = Some(playback);
            }
            Err(e) => tracing::warn!(error = %e, "cannot restore the queue"),
        }
    }

    /// Puts `groups` (tracks with their album) in the play queue. With
    /// nothing playing they start playing: there is no queue to see
    /// without a player.
    fn enqueue(
        &mut self,
        groups: Vec<AlbumTracks>,
        at: Enqueue,
        widgets: &mut AppWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if groups.iter().all(|(items, _)| items.is_empty()) {
            return;
        }
        let Some(playback) = &self.playback else {
            let album = match groups.as_slice() {
                [(_, album)] => album.clone(),
                _ => None,
            };
            let items = groups.into_iter().flat_map(|(items, _)| items).collect();
            sender.input(Msg::Play(Box::new(PlayRequest {
                items,
                index: 0,
                from: None,
                selection: None,
                tracks: None,
                album,
            })));
            return;
        };
        match at {
            // Each group to the front, last first, so they play in order.
            Enqueue::Next => {
                for (items, album) in groups.into_iter().rev() {
                    playback.play_next(items, album);
                }
                toast_text(widgets, "Playing next");
            }
            Enqueue::Last => {
                for (items, album) in groups {
                    playback.add_to_queue(items, album);
                }
                toast_text(widgets, "Added to queue");
            }
        }
    }

    /// Stops what plays, closing the full player and the bar.
    fn stop_playback(&mut self, widgets: &mut AppWidgets) {
        if let Some(playback) = self.playback.take() {
            self.queued = playback.carry();
            playback.stop();
        }
        widgets.mini_bar.detach();
        widgets.mpris.detach();
        widgets.window_stack.pop_to_page(&widgets.library_page);
    }

    /// The profile in use, for Preferences → Profile.
    fn signed(&self) -> Option<crate::profile_page::Signed> {
        let (session, server, profile) = (
            self.session.clone()?,
            self.server.as_ref()?,
            self.profile.as_ref()?,
        );
        Some(crate::profile_page::Signed {
            session,
            server_key: server.key.0.clone(),
            server_name: server.name.clone(),
            display_name: profile.display_name.clone(),
        })
    }

    /// Asks for `profile`'s PIN on the profiles screen, before anything of
    /// it shows (P2-08).
    fn ask_pin(
        &mut self,
        profile: Profile,
        widgets: &mut AppWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let server_name = self.server.as_ref().map_or("", |s| s.name.as_str());
        let unlock: Rc<dyn Fn(String)> = {
            let sender = sender.clone();
            Rc::new(move |pin| sender.input(Msg::Unlock(pin)))
        };
        let back: Rc<dyn Fn()> = {
            let sender = sender.clone();
            Rc::new(move || sender.input(Msg::BackToProfiles))
        };
        let forgot: Rc<dyn Fn()> = {
            let sender = sender.clone();
            Rc::new(move || sender.input(Msg::ForgotPin))
        };
        let pad = pin::pad(&profile, server_name, unlock, forgot, back);
        widgets.profiles.set_child(Some(&pad.widget));
        widgets.screens.set_visible_child_name("profiles");
        widgets.pin_pad = Some(pad);
        self.unlocking = Some(profile);
    }

    /// What the server said no to a PIN with, on the pad.
    fn pin_refused(
        &mut self,
        error: &BackendError,
        widgets: &mut AppWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let Some(pad) = widgets.pin_pad.clone() else {
            return;
        };
        match error {
            BackendError::WrongCredentials => pad.wrong(),
            BackendError::RateLimited(wait) => pad.reject(&format!(
                "Too many tries. Try again in {} s.",
                wait.as_secs().max(1)
            )),
            // The tenth wrong PIN in a row ended the session here: the
            // password, from "Who's watching".
            BackendError::SignInRequired => {
                toast_text(
                    widgets,
                    "Too many wrong PINs. Sign in again with your password.",
                );
                sender.input(Msg::BackToProfiles);
            }
            other => pad.reject(&other.to_string()),
        }
    }

    /// The profile in use turned out locked: what plays stops, and the
    /// PIN is asked for before the library comes back.
    fn locked_mid_session(&mut self, widgets: &mut AppWidgets, sender: &ComponentSender<Self>) {
        self.stop_playback(widgets);
        match self.profile.clone() {
            Some(profile) => self.ask_pin(
                Profile {
                    locked: true,
                    ..profile
                },
                widgets,
                sender,
            ),
            None => self.signed_out(widgets, sender),
        }
    }

    /// The session is gone: back to "Who's watching", where the profile
    /// can be signed in again.
    fn signed_out(&mut self, widgets: &mut AppWidgets, sender: &ComponentSender<Self>) {
        self.session = None;
        // A player still open would keep playing behind "Who's watching".
        self.stop_playback(widgets);
        self.queued.clear();
        toast(widgets, &BackendError::SignInRequired);
        if let Some(server) = self.server.clone() {
            self.load_profiles(server, sender);
        } else {
            sender.input(Msg::ShowServers);
        }
    }

    fn load_libraries(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        sender.oneshot_command(async move { Loaded::Libraries(session.libraries().await) });
    }

    fn load_home(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        sender.oneshot_command(async move { Loaded::Home(session.home().await) });
    }

    /// Loads what the item's page needs, then pushes the page.
    fn open_item(&self, item: Item, sender: &ComponentSender<Self>) {
        let Some(backend) = self.session.clone() else {
            return;
        };
        match item {
            Item::Movie(ref m) => {
                let id = m.id.clone();
                sender.oneshot_command(async move {
                    let tracks = backend.tracks(&id).await;
                    let chosen = server_choice(backend.as_ref(), &id).await;
                    Loaded::Playable(item, None, tracks, chosen)
                });
            }
            Item::Episode(ref e) => {
                let (id, show) = (e.id.clone(), e.show_id.clone());
                sender.oneshot_command(async move {
                    // The show's title is context, not essential: the page
                    // opens without it if the show cannot be loaded.
                    let context = match backend.item(&show).await {
                        Ok((Item::Show(s), _)) => Some(s.title),
                        _ => None,
                    };
                    let tracks = backend.tracks(&id).await;
                    let chosen = server_choice(backend.as_ref(), &id).await;
                    Loaded::Playable(item, context, tracks, chosen)
                });
            }
            Item::Show(show) => sender.oneshot_command(async move {
                let seasons = async {
                    let mut seasons = Vec::new();
                    for child in backend.item(&show.id).await?.1 {
                        if let Item::Season(season) = child {
                            let episodes = backend.item(&season.id).await?.1;
                            seasons.push((season, episodes));
                        }
                    }
                    Ok(seasons)
                }
                .await;
                Loaded::Show(show, seasons)
            }),
            Item::Season(season) => sender.oneshot_command(async move {
                let episodes = backend.item(&season.id).await.map(|(_, c)| c);
                Loaded::Season(season, episodes)
            }),
            Item::Artist(artist) => sender.oneshot_command(async move {
                let albums = backend.item(&artist.id).await.map(|(_, c)| c);
                Loaded::Artist(artist, albums)
            }),
            Item::Album(album) => sender.oneshot_command(async move {
                let loaded = async {
                    let artist = match backend.item(&album.artist_id).await?.0 {
                        Item::Artist(a) => a.name,
                        other => other.id().to_string(),
                    };
                    let tracks = backend.item(&album.id).await?.1;
                    Ok((artist, tracks))
                }
                .await;
                Loaded::Album(album, loaded)
            }),
            // A track found by search plays on its own.
            Item::Track(_) => sender.input(Msg::Play(Box::new(PlayRequest::one(
                item, None, None, None,
            )))),
        }
    }
}

/// How often the position is saved with the queue while playing; the queue
/// itself is saved on every change.
const SAVE_POSITION_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// Saves the queue and position whenever what plays changes, on pause,
/// and every few seconds while playing. Music only, for now (P1-23): a
/// film started from the library leaves the music saved last alone.
fn save_queue_on_changes(playback: &Rc<Playback>, saved: Rc<SavedQueue>) {
    let weak = Rc::downgrade(playback);
    let last = std::cell::Cell::new(std::time::Instant::now());
    playback.subscribe(move |change| {
        let Some(playback) = weak.upgrade() else {
            return false;
        };
        let due = match change {
            Change::Item(_) | Change::Queue | Change::Modes { .. } | Change::Playing(false) => true,
            Change::Position { .. } => last.get().elapsed() >= SAVE_POSITION_EVERY,
            _ => false,
        };
        if due && !playback.is_video() {
            saved.save(&playback.snapshot(), playback.position());
            last.set(std::time::Instant::now());
        }
        true
    });
}

/// Tracks, with the album they are from (for the cover and name).
type AlbumTracks = (Vec<Item>, Option<Album>);

/// The tracks an item stands for in the play queue, grouped by album, in
/// album order: a track (with its album, for the cover), an album, or each
/// of an artist's albums.
async fn tracks_of(session: &dyn Session, item: Item) -> Result<Vec<AlbumTracks>, BackendError> {
    match item {
        Item::Track(ref track) => {
            // The album is for the cover and name only.
            let album = match session.item(&track.album_id).await {
                Ok((Item::Album(album), _)) => Some(album),
                _ => None,
            };
            Ok(vec![(vec![item], album)])
        }
        Item::Album(album) => {
            let tracks = session.item(&album.id).await?.1;
            Ok(vec![(tracks, Some(album))])
        }
        Item::Artist(artist) => {
            let mut groups = Vec::new();
            for child in session.item(&artist.id).await?.1 {
                if let Item::Album(album) = child {
                    let tracks = session.item(&album.id).await?.1;
                    groups.push((tracks, Some(album)));
                }
            }
            Ok(groups)
        }
        _ => Ok(Vec::new()),
    }
}

fn open_with(sender: &ComponentSender<App>) -> views::Open {
    let sender = sender.clone();
    Rc::new(move |item| sender.input(Msg::Open(Box::new(item))))
}

fn show_connection(widgets: &AppWidgets, server: Option<&Server>, state: ConnectionState) {
    let name = server.map_or("the server", |s| s.name.as_str());
    let (status, banner) = match state {
        ConnectionState::Connected => ("connected".to_owned(), None),
        ConnectionState::Connecting => (
            "connecting…".to_owned(),
            Some(format!("Connecting to {name}…")),
        ),
        ConnectionState::Reconnecting { attempt } => (
            format!("reconnecting ({attempt})…"),
            Some(format!(
                "Lost the connection to {name}; reconnecting (attempt {attempt})…"
            )),
        ),
        ConnectionState::Closed => (
            "offline".to_owned(),
            Some(format!("Not connected to {name}.")),
        ),
    };
    widgets
        .profile_server
        .set_label(&format!("{name} · {status}"));
    match banner {
        Some(text) => {
            widgets.banner.set_title(&text);
            widgets.banner.set_revealed(true);
        }
        None => widgets.banner.set_revealed(false),
    }
}

fn toolbar(header: &adw::HeaderBar, content: &impl IsA<gtk::Widget>) -> adw::ToolbarView {
    let view = adw::ToolbarView::new();
    view.add_top_bar(header);
    view.set_content(Some(content));
    view
}

fn sidebar_row(title: &str, icon: &str) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    row.append(&gtk::Image::from_icon_name(icon));
    row.append(&gtk::Label::builder().label(title).xalign(0.0).build());
    row
}

fn library_icon(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Movies => crate::icons::MOVIES,
        LibraryKind::Shows => crate::icons::SHOWS,
        LibraryKind::Music => crate::icons::MUSIC,
    }
}

fn clear(content: &gtk::Box) {
    while let Some(child) = content.first_child() {
        content.remove(&child);
    }
}

/// Puts the root page's ordinary content back in its scrolled area, in
/// place of a library grid.
fn show_content(widgets: &AppWidgets) {
    if widgets.scroller.child().as_ref() != Some(widgets.content.upcast_ref()) {
        widgets.scroller.set_child(Some(&widgets.content));
    }
}

fn show(widgets: &AppWidgets, title: &str, sections: impl IntoIterator<Item = gtk::Box>) {
    show_content(widgets);
    widgets.root_page.set_title(title);
    clear(&widgets.content);
    for section in sections {
        widgets.content.append(&section);
    }
}

fn show_error(widgets: &AppWidgets, title: &str, error: &BackendError) {
    show_content(widgets);
    widgets.root_page.set_title(title);
    clear(&widgets.content);
    widgets.content.append(&error_status(error));
}

fn push(widgets: &AppWidgets, title: &str, body: &impl IsA<gtk::Widget>) {
    widgets.stack.push(&pages::page(title, body));
}

fn push_error(widgets: &AppWidgets, title: &str, error: &BackendError) {
    push(widgets, title, &error_status(error));
}

fn toast(widgets: &AppWidgets, error: &BackendError) {
    toast_text(widgets, &error.to_string());
}

fn toast_text(widgets: &AppWidgets, text: &str) {
    widgets.toasts.add_toast(adw::Toast::new(text));
}

fn error_status(error: &BackendError) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name(crate::icons::ERROR)
        .title("Could not load this page")
        .description(error.to_string())
        .vexpand(true)
        .build()
}

/// The tracks the server would choose for `id`, and why. The page opens
/// without them if the server cannot say.
async fn server_choice(session: &dyn Session, id: &MediaId) -> Option<ChosenTracks> {
    match session.playback_plan(id, None).await {
        Ok((_, chosen)) => chosen,
        Err(e) => {
            tracing::warn!(error = %e, "no track choice from the server");
            None
        }
    }
}

/// The full player for `playback`. Leaving it, however, keeps it playing
/// in the bar; the bar's stop ends it.
fn player_page(
    playback: &Rc<Playback>,
    sender: &ComponentSender<App>,
    cards: &Cards,
) -> adw::NavigationPage {
    let minimize = {
        let sender = sender.clone();
        Rc::new(move || sender.input(Msg::MinimizePlayer)) as Rc<dyn Fn()>
    };
    let actions = player::Actions { minimize };
    if playback.is_video() {
        player::page(playback, actions)
    } else {
        nowplaying::page(playback, actions, cards)
    }
}

/// Shows "Is this your server?" for `server`; the user's answer pins it.
fn check_fingerprint(
    server: &Server,
    verifying: bool,
    sender: &ComponentSender<App>,
    root: &adw::ApplicationWindow,
) {
    let decide: Rc<dyn Fn(start::Check)> = {
        let (sender, server) = (sender.clone(), server.clone());
        Rc::new(move |check| match check {
            start::Check::Matches => sender.input(Msg::PinServer(server.clone(), true, verifying)),
            // Skipping a check of a saved server leaves it as it is.
            start::Check::Skip if verifying => {}
            start::Check::Skip => sender.input(Msg::PinServer(server.clone(), false, false)),
        })
    };
    start::fingerprint_check(server, verifying, decide).present(Some(root));
}

/// A server's error: the identity alert when it answered with another key,
/// otherwise a toast.
fn server_error(
    widgets: &AppWidgets,
    name: &str,
    error: &BackendError,
    sender: &ComponentSender<App>,
    root: &adw::ApplicationWindow,
) {
    let BackendError::IdentityChanged { saved, now } = error else {
        return toast(widgets, error);
    };
    let trust: Rc<dyn Fn()> = {
        let (sender, saved, now) = (sender.clone(), saved.clone(), now.clone());
        Rc::new(move || {
            sender.input(Msg::TrustIdentity(
                ServerKey(saved.clone()),
                ServerKey(now.clone()),
            ))
        })
    };
    start::identity_changed(name, saved, now, trust).present(Some(root));
}
