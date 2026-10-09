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
    BackendError, ChosenTracks, Client, ConnectionState, HomeScreen, Invite, Person, Profile,
    ProfileSignIn, SearchScope, Selection, Server, ServerKey, Session, TrackPreferencesInfo,
};
use spoolframe_linux_core::next_episode::next_episode;
use spoolframe_linux_core::theme::Theme;
use spoolframe_types::{
    Album, Artist, Item, Library, LibraryKind, MediaId, MediaTracks, Season, Show, TrackPreferences,
};

use crate::artwork::Art;
use crate::minibar::MiniBar;
use crate::mpris::Mpris;
use crate::passkey_dialog::{self, PasskeySignIn};
use crate::playback::{Change, Playback, Playlist};
use crate::queue::Queued;
use crate::remote_sign_in::{self, RemoteSignIn, Via};
use crate::saved_queue::SavedQueue;
use crate::search::{self, SearchPage};
use crate::settings::Settings;
use crate::theme::ThemeApplier;
use crate::views::{Cards, Enqueue};
use crate::{library, nowplaying, pages, pin, player, preferences, start, views};
use spoolframe_linux_core::artcache::ArtworkCache;

/// A password or PIN in a message. Msg derives Debug, and Relm4 records
/// each message in a tracing span: this keeps them out of logs.
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

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
    /// Shuffle on from the start (an album's Shuffle button).
    pub shuffle: bool,
    /// The quality chosen on the item page (P6-06); Auto otherwise.
    pub quality: spoolframe_client_sdk::PlaybackQuality,
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
            shuffle: false,
            quality: Default::default(),
        }
    }

    /// At the quality the viewer chose before playing (P6-06).
    fn at_quality(mut self, quality: spoolframe_client_sdk::PlaybackQuality) -> Self {
        self.quality = quality;
        self
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
    /// A PIN or a sign-in is with the server: another is not sent.
    unlock_in_flight: bool,
    profile_scope: spoolframe_client_sdk::ProfileScope,
    shutdown_calls: Vec<tokio::task::JoinHandle<()>>,
    sign_in_in_flight: bool,
    /// What the root page shows (or is about to).
    root: Root,
    /// The root page's load in flight: aborted by the next sidebar choice
    /// or search, so a late answer never replaces a newer page.
    root_load: Option<glib::JoinHandle<()>>,
    libraries: Vec<Library>,
    settings: Settings,
    theme: ThemeApplier,
    /// Given to every card: opens its item, loads its artwork.
    cards: Rc<Cards>,
    /// Follows the connection to the server in use; replaced on switching.
    connection_watch: Option<glib::JoinHandle<()>>,
    /// The server signing this device out (PX-10): back to "Who's
    /// watching" at once.
    sign_out_watch: Option<glib::JoinHandle<()>>,
    /// What plays, shown full or in the bar; dropping it stops it.
    playback: Option<Rc<Playback>>,
    /// The play queue (P1-23) while no playback holds it: kept from the
    /// one stopped last, for the next one.
    queued: Vec<Queued>,
    /// Where the profile's queue is saved, once a profile is chosen.
    saved_queue: Option<Rc<SavedQueue>>,
    /// The session's libraries as far as fetched, kept between visits.
    library_cache: Option<Rc<library::Cache>>,
    /// The library open last, for search's default scope (P3-09).
    library: Option<Library>,
    /// Whether the server used last has been opened already. It may be
    /// heard only on a later listing; it is opened once, not on every
    /// change after.
    opened_last_server: bool,
    /// The first listing of profiles this run is yet to come: the one that
    /// opens at start is opened from it, past Who's watching (P4-02).
    starting: bool,
    /// A remembered profile the server would not resume: Who's watching
    /// says it needs a sign-in, once.
    needs_sign_in: Option<Profile>,
    /// The item whose page is loading: a second click on it while it loads
    /// opens nothing more, and the page of an item clicked before it is
    /// dropped when it arrives, so only the last click opens a page.
    opening: Option<MediaId>,
    /// The item playing, for pages that mark it (an album's track list).
    now_playing: tokio::sync::watch::Sender<Option<MediaId>>,
}

/// The sidebar's choice.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Root {
    Home,
    Search,
    Library,
}

pub struct AppWidgets {
    /// "servers", "profiles", "library", or "not-set-up" in its place.
    screens: gtk::Stack,
    /// What shows where the library would be on a server whose admin has
    /// not finished setup (P3-10).
    not_set_up: start::NotSetUp,
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
    library_heading: gtk::Label,
    /// The root header's title, at its start.
    root_title: gtk::Label,
    /// The root page's header bar: search puts its field there.
    root_header: adw::HeaderBar,
    sign_in: Option<start::SignIn>,
    remote_sign_in: Option<Rc<RemoteSignIn>>,
    passkey_sign_in: Option<Rc<PasskeySignIn>>,
    add_server: Option<start::AddServer>,
    /// "Add a server" from an invite (P6-01), while it is open.
    invite: Option<Rc<crate::invite::InviteDialog>>,
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
    /// Checks the connections after a wake or a network change.
    _wake: crate::wake::Wake,
    root_page: adw::NavigationPage,
    /// The root page's scrolled area: `content`, or a library's grid,
    /// which must be the scrolled child itself to load row by row.
    scroller: gtk::ScrolledWindow,
    content: gtk::Box,
    /// The search page (P3-09), made for the profile on first use.
    search: Option<Rc<SearchPage>>,
}

#[derive(Debug)]
pub enum Msg {
    ShowServers,
    ChooseServer(Server),
    /// "Add a server": from an invite link (P6-01), pasted, or opened
    /// from the desktop when given one.
    OpenAddServer(Option<String>),
    /// The way without an invite: a server's key and an address.
    OpenAddServerByKey,
    AddServer(String, String),
    /// Read the invite link pasted; nothing is sent.
    ReadInvite(String),
    /// Connect to the server an invite names, for its fingerprint.
    ConnectInvite(String),
    /// Save the server an invite names, without an account.
    SaveInviteServer(String),
    /// Make the account an invite offers: the link, username, display
    /// name and password.
    JoinInvite(String, String, Option<String>, Secret),
    /// Sign in to a server an invite saved.
    SignInToInvited(Server),
    ChooseProfile(Profile),
    /// The PIN typed for the locked profile being chosen (P2-08).
    Unlock(Secret),
    /// From the PIN pad: back to "Who's watching", or to the password.
    BackToProfiles,
    ForgotPin,
    OpenSignIn,
    /// Sign in again as a profile whose session the server ended.
    SignInAs(String),
    /// Drop a profile kept on this device.
    ForgetProfile(Profile),
    /// "Sign out Alice?" for a profile on Who's watching, or the one
    /// watching when None (P4-02).
    AskSignOut(Option<Profile>),
    /// Signs a profile out here, which forgets this computer for it.
    SignOutProfile(Profile),
    /// Ask a server that was not set up for its catalog again (P3-10).
    RetryCatalog,
    /// Sign in by approving on a phone (P3-05) or in the browser (P3-06).
    OpenRemoteSignIn(Via, bool),
    /// Sign in with a passkey (P3-19), narrowed to a typed username.
    OpenPasskeySignIn(Option<String>, bool),
    /// The browser sign-in has no page to open: the server has no web
    /// address.
    BrowserSignInUnavailable,
    SubmitSignIn(String, Secret, bool),
    /// Back to "Who's watching" from the library.
    SwitchProfile,
    /// A sidebar row was activated: see the `ROW_*` constants.
    Select(usize),
    /// A person a search found: their items.
    OpenPerson(Box<Person>),
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
    /// Skip intros without asking (P2-10).
    SetSkipIntros(bool),
    SetShowHdr(bool),
    /// Relays on or off (P6-03), from the next start.
    SetUseRelays(bool),
    /// The session ended under the screen (signed out elsewhere, or its
    /// password changed): back to "Who's watching".
    SessionEnded,
    /// Preferences › Profile stopped remembering this computer, which
    /// ended the session here (P4-01).
    StoppedRemembering,
    SetShowDiagnostics(bool),
    /// Leave the full player and keep playing in the bar.
    MinimizePlayer,
    /// Take a card out of its home row (`true`), or put it back (`false`,
    /// from the toast's Undo).
    HideFromContinue(Box<Item>, views::Shelf, bool),
    /// Connect to `server` and show its fingerprint; `true` when it is a
    /// saved server being verified.
    CheckServer(Server, bool),
    /// Save `server`: verified (It matches) or not (Skip); then, unless it
    /// was being verified, go on to "Who's watching".
    PinServer(Server, bool, bool),
    /// Replace a saved server's pin with the key that answered for it.
    TrustIdentity(ServerKey, ServerKey),
    /// Forget a saved server: its server id changed (PX-14), and only
    /// adding it again can trust it.
    RemoveServer(ServerKey),
    /// Back to the full player from the bar.
    ExpandPlayer,
    StopPlayback,
}

/// Sidebar rows before the libraries.
const ROW_HOME: usize = 0;
const ROW_SEARCH: usize = 1;
const FIRST_LIBRARY_ROW: usize = 2;

/// How many search results to show.
/// The most of a person's items listed.
const PERSON_ITEMS: u32 = 200;

/// A season with its episodes.
type SeasonEpisodes = (Season, Vec<Item>);

#[derive(Debug)]
pub struct Completion {
    token: spoolframe_client_sdk::ProfileToken,
    loaded: Loaded,
}

impl Completion {
    fn accept(self) -> Option<Loaded> {
        self.token.accept(self.loaded)
    }
}

#[derive(Debug)]
pub enum Loaded {
    Servers(Result<Vec<Server>, BackendError>),
    ServerAdded(Result<Server, BackendError>),
    /// What a pasted link says, for the link it was read from (P6-01).
    InviteRead(String, Result<Invite, BackendError>),
    /// Who answered for an invite.
    InviteConnected(Result<Server, BackendError>),
    /// The server an invite names, saved.
    InviteServerSaved(Result<Server, BackendError>),
    /// The account an invite made, signed in, with the username asked.
    InviteJoined(String, Result<Profile, BackendError>),
    /// A server connected for its fingerprint check (P1-21); the flag says
    /// whether it is a saved server being verified.
    Checked(Server, bool, Result<Server, BackendError>),
    /// The user decided at the check; the flag as for `Checked`.
    Pinned(bool, Result<Server, BackendError>),
    /// A card was taken out of its row (`true`) or put back.
    Hidden(Box<Item>, views::Shelf, bool, Result<(), BackendError>),
    /// What an enqueued item stands for: tracks, grouped by album.
    Enqueued(Enqueue, Result<Vec<AlbumTracks>, BackendError>),
    /// The user trusted a server's new identity (P1-13).
    Trusted(Result<Server, BackendError>),
    ServerRemoved(ServerKey, Result<(), BackendError>),
    Profiles(Server, Result<Vec<Profile>, BackendError>),
    ProfileForgotten(Server, Result<(), BackendError>),
    /// A profile signed out, from Who's watching or the sidebar (P4-02).
    ProfileSignedOut(Server, Result<(), BackendError>),
    /// A remembered profile opened without a password, or why not; its
    /// username, to sign in again.
    Resumed(Box<Profile>, Result<Profile, BackendError>),
    SignedIn(Result<Profile, BackendError>),
    Libraries(Result<Vec<Library>, BackendError>),
    Home(Result<HomeScreen, BackendError>),
    Person(Box<Person>, Result<(Vec<Item>, u32), BackendError>),
    /// A movie or episode, what it belongs to (an episode's show), and its
    /// tracks for the pickers.
    Playable(
        Item,
        Option<String>,
        Result<MediaTracks, BackendError>,
        Option<ChosenTracks>,
        Option<Box<Item>>,
    ),
    /// A show, its seasons with their episodes, and the season to open on
    /// ("Go to season"), if one was asked for.
    Show(
        Show,
        Result<Vec<SeasonEpisodes>, BackendError>,
        Option<MediaId>,
    ),
    Artist(Artist, Result<Vec<Item>, BackendError>),
    /// An album, its artist's name and its tracks.
    Album(Album, Result<(String, Vec<Item>), BackendError>),
    WatchChanged(Result<(), BackendError>),
    /// A change the user does not need to hear about unless it failed.
    Saved(Result<(), BackendError>),
    TracksForgotten(Box<Item>, Result<(), BackendError>),
    Preferences(Result<TrackPreferencesInfo, BackendError>),
    /// What the server said to the PIN typed for a locked profile.
    Unlocked(Box<Profile>, Result<(), BackendError>),
    Activated(u64, Box<Profile>, Result<(), BackendError>),
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

    /// The item an item page's answer is for: what [`App::open_item`] was
    /// asked to open (a season's show page answers for the season).
    fn opened(&self) -> Option<&MediaId> {
        match self {
            Loaded::Playable(item, ..) => Some(item.id()),
            Loaded::Show(show, _, open) => Some(open.as_ref().unwrap_or(&show.id)),
            Loaded::Artist(artist, _) => Some(&artist.id),
            Loaded::Album(album, _) => Some(&album.id),
            _ => None,
        }
    }

    /// The error of an answer that comes from the profile's session.
    fn session_error(&self) -> Option<&BackendError> {
        match self {
            Loaded::Activated(_, _, Err(e))
            | Loaded::Playable(_, _, Err(e), _, _)
            | Loaded::Libraries(Err(e))
            | Loaded::Home(Err(e))
            | Loaded::Person(_, Err(e))
            | Loaded::Show(_, Err(e), _)
            | Loaded::Artist(_, Err(e))
            | Loaded::Album(_, Err(e))
            | Loaded::WatchChanged(Err(e))
            | Loaded::Hidden(_, _, _, Err(e))
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
    type CommandOutput = Completion;
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
            .css_classes(["navigation-sidebar", "app-sidebar"])
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
                            .margin_start(4)
                            .margin_end(4)
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
        profile_avatar.add_css_class("sidebar-avatar");
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
            .tooltip_text("Account")
            .css_classes(["flat", "sidebar-profile"])
            .build();
        // Preferences, beside who is watching: the boards have no menu in
        // the header.
        let preferences_button = gtk::Button::builder()
            .icon_name(crate::icons::SET_UP)
            .tooltip_text("Preferences")
            .action_name("win.preferences")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        profile_button.set_hexpand(true);
        let profile_foot = gtk::Box::builder()
            .spacing(4)
            .css_classes(["sidebar-foot"])
            .build();
        profile_foot.append(&profile_button);
        profile_foot.append(&preferences_button);
        // The account menu, as the LinuxTrustStates board has it.
        {
            let (switch, sign_out) = (sender.clone(), sender.clone());
            let menu = crate::views::menu_popover(vec![
                vec![
                    crate::views::MenuEntry::new("Switch profile", crate::icons::DOWN, move || {
                        switch.input(Msg::SwitchProfile)
                    }),
                    crate::views::MenuEntry::new("Preferences", crate::icons::SET_UP, {
                        let button = profile_button.downgrade();
                        move || {
                            if let Some(button) = button.upgrade() {
                                let _ = button.activate_action("win.preferences", None);
                            }
                        }
                    }),
                ],
                vec![crate::views::MenuEntry {
                    destructive: true,
                    ..crate::views::MenuEntry::new("Sign out", crate::icons::SIGN_OUT, move || {
                        sign_out.input(Msg::AskSignOut(None))
                    })
                }],
            ]);
            menu.set_parent(&profile_button);
            menu.set_position(gtk::PositionType::Top);
            profile_button.connect_clicked(move |_| menu.popup());
        }
        // The mark and the wordmark head the sidebar.
        let brand = gtk::Box::builder().spacing(10).build();
        brand.append(&crate::logo::mark(26));
        brand.append(&crate::logo::wordmark());
        // At the start, not centred, as designed.
        let sidebar_header = adw::HeaderBar::builder()
            .title_widget(&adw::Bin::new())
            .build();
        sidebar_header.pack_start(&brand);
        let sidebar_toolbar = views::toolbar(
            &sidebar_header,
            &[],
            &gtk::ScrolledWindow::builder()
                .child(&sidebar)
                .vexpand(true)
                .build(),
        );
        sidebar_toolbar.add_bottom_bar(&profile_foot);
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
        // The page's title at the start, as designed, not centred: a
        // title widget that fills the header, its text at the left.
        let root_title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .margin_start(16)
            .css_classes(["title"])
            .build();
        let header = adw::HeaderBar::builder().title_widget(&root_title).build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            // 32 between rows, less the room each row keeps for a lifted
            // card's shadow.
            .spacing(32 - views::LIFT_BELOW)
            .margin_top(28)
            .margin_bottom(28)
            .margin_start(28)
            .margin_end(28)
            .build();
        let banner = adw::Banner::new("");
        banner.set_use_markup(false);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&content)
            .vexpand(true)
            .build();
        // A library's name over its grid: the grid scrolls on its own (it
        // loads as it goes), so the heading stays above it.
        let library_heading = gtk::Label::builder()
            .xalign(0.0)
            .visible(false)
            .css_classes(["title-2", "library-heading"])
            .build();
        let root_toolbar = views::toolbar(
            &header,
            &[banner.upcast_ref(), library_heading.upcast_ref()],
            &scroller,
        );
        let root_page = adw::NavigationPage::builder()
            .title("Home")
            .child(&root_toolbar)
            .build();
        root_page
            .bind_property("title", &root_title, "label")
            .sync_create()
            .build();
        let stack = adw::NavigationView::new();
        stack.add(&root_page);
        let content_page = adw::NavigationPage::builder()
            .title("Spoolframe")
            .child(&stack)
            .build();

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
        let not_set_up = start::NotSetUp::new(
            {
                let sender = sender.clone();
                Rc::new(move || sender.input(Msg::RetryCatalog))
            },
            {
                let sender = sender.clone();
                Rc::new(move || sender.input(Msg::ShowServers))
            },
        );
        screens.add_named(&not_set_up.page, Some("not-set-up"));
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
        let wake = {
            let client = client.clone();
            crate::wake::watch(Rc::new(move || {
                let client = client.clone();
                relm4::spawn(async move { client.resume().await });
            }))
        };
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
        let library_page = adw::NavigationPage::builder()
            .title("Spoolframe")
            .child(&split)
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
                if key == gtk::gdk::Key::space
                    && crate::player::plain(modifiers)
                    && !typing
                    && bar.toggle()
                {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            keys
        };
        let window_stack = adw::NavigationView::builder().vexpand(true).build();
        window_stack.add(&library_page);
        window_stack.add_controller(space_keys);
        // Item pages cover the window, as designed, and the bar stays under
        // them; only the full player and Now Playing hide it.
        {
            let bar = mini_bar.revealer.clone();
            window_stack.connect_visible_page_notify(move |stack| {
                bar.set_visible(!stack.visible_page().is_some_and(|p| is_player(&p)));
            });
        }
        let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
        shell.append(&window_stack);
        shell.append(&mini_bar.revealer);
        screens.add_named(&shell, Some("library"));
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
            unlock_in_flight: false,
            profile_scope: spoolframe_client_sdk::ProfileScope::default(),
            shutdown_calls: Vec::new(),
            sign_in_in_flight: false,
            root: Root::Home,
            root_load: None,
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
            sign_out_watch: None,
            playback: None,
            queued: Vec::new(),
            saved_queue: None,
            library_cache: None,
            library: None,
            opened_last_server: false,
            starting: true,
            needs_sign_in: None,
            opening: None,
            now_playing: tokio::sync::watch::Sender::new(None),
        };
        model.load_servers(&sender);
        // Invite links the desktop opened before the window was up.
        for link in opened::listen(sender.clone()) {
            sender.input(Msg::OpenAddServer(Some(link)));
        }
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
                not_set_up,
                toasts,
                profile_avatar,
                profile_name,
                profile_server,
                banner,
                library_heading,
                root_title,
                root_header: header.clone(),
                sign_in: None,
                remote_sign_in: None,
                passkey_sign_in: None,
                pin_pad: None,
                add_server: None,
                invite: None,
                sidebar,
                stack,
                root_page,
                window_stack,
                library_page,
                mini_bar,
                mpris,
                _wake: wake,
                scroller,
                content,
                search: None,
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
            Msg::RetryCatalog => {
                widgets.screens.set_visible_child_name("library");
                self.load_libraries(&sender);
                self.load_home(&sender);
            }
            Msg::ShowServers => {
                self.invalidate_session();
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
            Msg::HideFromContinue(item, shelf, hide) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.command(&sender, async move {
                    let done = if hide {
                        session.hide_from_continue(item.id()).await
                    } else {
                        session.unhide_from_continue(item.id()).await
                    };
                    Loaded::Hidden(item, shelf, hide, done)
                });
            }
            Msg::CheckServer(server, verifying) => {
                let client = self.client.clone();
                self.command(&sender, async move {
                    let checked = client.connect(&server.key).await;
                    Loaded::Checked(server, verifying, checked)
                });
            }
            Msg::PinServer(server, verified, verifying) => {
                let client = self.client.clone();
                self.command(&sender, async move {
                    Loaded::Pinned(verifying, client.pin(&server.key, verified).await)
                });
            }
            Msg::TrustIdentity(saved, now) => {
                let client = self.client.clone();
                self.command(&sender, async move {
                    Loaded::Trusted(client.trust_new_identity(&saved, &now).await)
                });
            }
            Msg::RemoveServer(key) => {
                let client = self.client.clone();
                self.command(&sender, async move {
                    let removed = client.forget(&key).await;
                    Loaded::ServerRemoved(key, removed)
                });
            }
            Msg::OpenAddServer(link) => {
                let dialog = crate::invite::InviteDialog::new(invite_actions(&sender));
                dialog.dialog.present(Some(root));
                if let Some(link) = link {
                    dialog.set_link(&link);
                }
                widgets.invite = Some(dialog);
            }
            Msg::ReadInvite(link) => {
                let client = self.client.clone();
                self.command(&sender, async move {
                    let read = client.read_invite(&link).await;
                    Loaded::InviteRead(link, read)
                });
            }
            Msg::ConnectInvite(link) => {
                let client = self.client.clone();
                self.command(&sender, async move {
                    Loaded::InviteConnected(client.connect_invite(&link).await)
                });
            }
            Msg::SaveInviteServer(link) => {
                let client = self.client.clone();
                self.command(&sender, async move {
                    Loaded::InviteServerSaved(client.save_invite_server(&link).await)
                });
            }
            Msg::JoinInvite(link, username, display_name, Secret(password)) => {
                let client = self.client.clone();
                self.command(&sender, async move {
                    let joined = client
                        .redeem_invite(&link, &username, display_name.as_deref(), &password)
                        .await;
                    Loaded::InviteJoined(username, joined)
                });
            }
            Msg::SignInToInvited(server) => self.load_profiles(server, &sender),
            Msg::OpenAddServerByKey => {
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
                self.command(&sender, async move {
                    Loaded::ServerAdded(client.add_address(&ServerKey(key), &address).await)
                });
            }
            Msg::ChooseProfile(profile) if profile.sign_in == ProfileSignIn::Password => {
                sender.input(Msg::SignInAs(profile.username))
            }
            // Last in with a passkey: offered one again.
            Msg::ChooseProfile(profile) if profile.sign_in == ProfileSignIn::Passkey => {
                sender.input(Msg::OpenPasskeySignIn(Some(profile.username), false))
            }
            // Remembered: opened without a password (P4-01).
            Msg::ChooseProfile(profile) if profile.sign_in == ProfileSignIn::Resume => {
                let Some(trust) = profile.trust.clone() else {
                    return sender.input(Msg::SignInAs(profile.username));
                };
                let client = self.client.clone();
                self.command(&sender, async move {
                    let resumed = client.resume_profile(&profile.server, &trust.user_id).await;
                    Loaded::Resumed(Box::new(profile), resumed)
                });
            }
            Msg::ChooseProfile(profile) if profile.locked => {
                self.ask_pin(profile, widgets, &sender)
            }
            Msg::ChooseProfile(profile) => self.enter(profile, widgets, &sender),
            Msg::Unlock(Secret(pin)) => {
                // One try at a time: a held Enter repeats, and each wrong
                // try counts towards the ten that end the session.
                if self.unlock_in_flight {
                    return;
                }
                let Some(profile) = self.unlocking.clone() else {
                    return;
                };
                let session = match self.client.session(&profile) {
                    Ok(session) => session,
                    Err(e) => return toast(widgets, &e),
                };
                self.stop_playback(widgets);
                let pending = std::mem::take(&mut self.shutdown_calls);
                self.unlock_in_flight = true;
                self.command(&sender, async move {
                    for call in pending {
                        let _ = call.await;
                    }
                    let unlocked = async {
                        session.activate().await?;
                        session.unlock(&pin).await
                    }
                    .await;
                    Loaded::Unlocked(Box::new(profile), unlocked)
                });
            }
            Msg::BackToProfiles => {
                self.invalidate_session();
                self.unlocking = None;
                widgets.pin_pad = None;
                match self.server.clone() {
                    Some(server) => self.load_profiles(server, &sender),
                    None => sender.input(Msg::ShowServers),
                }
            }
            Msg::ForgotPin => sender.input(Msg::OpenSignIn),
            Msg::ForgetProfile(profile) => {
                let Some(server) = self.server.clone() else {
                    return;
                };
                let client = self.client.clone();
                self.command(&sender, async move {
                    let forgotten = client
                        .forget_profile(&profile.server, &profile.username)
                        .await;
                    Loaded::ProfileForgotten(server, forgotten)
                });
            }
            Msg::AskSignOut(profile) => {
                let Some(profile) = profile.or_else(|| self.profile.clone()) else {
                    return;
                };
                let name = profile.display_name.clone();
                let sign_out: Rc<dyn Fn()> = {
                    let sender = sender.clone();
                    Rc::new(move || sender.input(Msg::SignOutProfile(profile.clone())))
                };
                start::sign_out_confirm(&name, sign_out).present(Some(root));
            }
            Msg::SignOutProfile(profile) => {
                let Some(server) = self.server.clone() else {
                    return;
                };
                // The one watching leaves the library first.
                let watching = self
                    .profile
                    .as_ref()
                    .is_some_and(|p| p.server == profile.server && p.username == profile.username);
                if watching {
                    self.stop_playback(widgets);
                    self.invalidate_session();
                }
                let client = self.client.clone();
                self.command(&sender, async move {
                    let signed_out = async {
                        // A remembered profile has no session here until it
                        // is resumed; resuming is what signing out ends.
                        let profile = match (&profile.trust, profile.sign_in) {
                            (Some(trust), ProfileSignIn::Resume) => {
                                client
                                    .resume_profile(&profile.server, &trust.user_id)
                                    .await?
                            }
                            _ => profile,
                        };
                        client.session(&profile)?.sign_out().await
                    }
                    .await;
                    Loaded::ProfileSignedOut(server, signed_out)
                });
            }
            Msg::OpenSignIn => sender.input(Msg::SignInAs(String::new())),
            Msg::SignInAs(username) => {
                let Some(server) = &self.server else { return };
                let submit: Rc<dyn Fn(String, String, bool)> = {
                    let sender = sender.clone();
                    Rc::new(move |u, p, remember| {
                        sender.input(Msg::SubmitSignIn(u, Secret(p), remember))
                    })
                };
                let elsewhere: Rc<dyn Fn(Via, bool)> = {
                    let sender = sender.clone();
                    Rc::new(move |via, remember| sender.input(Msg::OpenRemoteSignIn(via, remember)))
                };
                let passkey: Rc<dyn Fn(Option<String>, bool)> = {
                    let sender = sender.clone();
                    Rc::new(move |username, remember| {
                        sender.input(Msg::OpenPasskeySignIn(username, remember))
                    })
                };
                let dialog = start::SignIn::new(server, submit, elsewhere, passkey);
                if !username.is_empty() {
                    dialog.fill_username(&username);
                }
                dialog.dialog.present(Some(root));
                widgets.sign_in = Some(dialog);
            }
            Msg::OpenRemoteSignIn(via, remember) => {
                let Some(server) = &self.server else { return };
                let token = self.profile_scope.token();
                let ends = remote_sign_in::Ends {
                    // An approval ends as a password sign-in does.
                    done: {
                        let sender = sender.clone();
                        Rc::new(move |profile| {
                            sender.command_sender().emit(Completion {
                                token: token.clone(),
                                loaded: Loaded::SignedIn(Ok(profile)),
                            })
                        })
                    },
                    no_web: {
                        let sender = sender.clone();
                        Rc::new(move || sender.input(Msg::BrowserSignInUnavailable))
                    },
                    open: {
                        let window = root.downgrade();
                        Rc::new(move |url| crate::browser::open(url, window.upgrade().as_ref()))
                    },
                };
                let screen = RemoteSignIn::new(self.client.clone(), server, via, remember, ends);
                screen.dialog.present(Some(root));
                widgets.remote_sign_in = Some(screen);
            }
            Msg::OpenPasskeySignIn(username, remember) => {
                let Some(server) = &self.server else { return };
                let token = self.profile_scope.token();
                let ends = passkey_dialog::Ends {
                    // A passkey sign-in ends as a password sign-in does.
                    done: {
                        let sender = sender.clone();
                        Rc::new(move |profile| {
                            sender.command_sender().emit(Completion {
                                token: token.clone(),
                                loaded: Loaded::SignedIn(Ok(profile)),
                            })
                        })
                    },
                };
                let screen =
                    PasskeySignIn::new(self.client.clone(), server, username, remember, ends);
                screen.dialog.present(Some(root));
                widgets.passkey_sign_in = Some(screen);
            }
            Msg::BrowserSignInUnavailable => {
                if let Some(dialog) = &widgets.sign_in {
                    dialog.refuse(
                        "This server has no web address, so it cannot sign you in through \
                         a browser. Sign in with your password.",
                    );
                }
            }
            Msg::SubmitSignIn(username, Secret(password), remember) => {
                // Enter in the password field still sends while one is out.
                if self.sign_in_in_flight {
                    return;
                }
                let Some(server) = self.server.clone() else {
                    return;
                };
                self.sign_in_in_flight = true;
                if let Some(dialog) = &widgets.sign_in {
                    dialog.set_busy(true);
                }
                let client = self.client.clone();
                self.command(&sender, async move {
                    Loaded::SignedIn(
                        client
                            .sign_in(&server.key, &username, &password, remember)
                            .await,
                    )
                });
            }
            Msg::SwitchProfile => {
                self.invalidate_session();
                if let Some(server) = self.server.clone() {
                    self.load_profiles(server, &sender);
                }
            }
            Msg::Select(ROW_HOME) => {
                self.select_root(Root::Home);
                plain_header(widgets);
                self.library = None;
                widgets.stack.pop_to_page(&widgets.root_page);
                self.load_home(&sender);
            }
            Msg::Select(ROW_SEARCH) => {
                self.select_root(Root::Search);
                widgets.stack.pop_to_page(&widgets.root_page);
                widgets.root_page.set_title("Search");
                show_content(widgets);
                clear(&widgets.content);
                if widgets.search.is_none() {
                    widgets.search = self.search_page(&sender);
                }
                let Some(page) = widgets.search.clone() else {
                    return;
                };
                // The field sits in the header bar while search shows.
                widgets.root_header.set_title_widget(Some(&page.head));
                widgets.content.append(&page.widget);
                // Opened from a library: its kind is the scope.
                match &self.library {
                    Some(library) => page.open(scope_of(library.kind), Some(library.title.clone())),
                    None => page.open(SearchScope::All, None),
                }
            }
            Msg::Select(row) => {
                let Some(library) = self.libraries.get(row - FIRST_LIBRARY_ROW).cloned() else {
                    return;
                };
                let Some(cache) = self.library_cache.clone() else {
                    return;
                };
                self.select_root(Root::Library);
                plain_header(widgets);
                self.library = Some(library.clone());
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
                // A newer choice in the sidebar aborts this one.
                let (scroller, content) = (widgets.scroller.clone(), widgets.content.clone());
                let heading = widgets.library_heading.clone();
                let cards = self.cards.clone();
                self.root_load = Some(glib::spawn_future_local(async move {
                    let title = library.title.clone();
                    match cache.grid(library, cards).await {
                        Ok(grid) => {
                            heading.set_label(&title);
                            heading.set_visible(true);
                            scroller.set_child(Some(&grid));
                        }
                        Err(e) => {
                            clear(&content);
                            content.append(&error_status(&e, None));
                        }
                    }
                }));
            }
            Msg::OpenPerson(person) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.command(&sender, async move {
                    let items = session.person_items(&person.id, 0, PERSON_ITEMS).await;
                    Loaded::Person(person, items)
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
                    shuffle,
                    quality,
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
                    quality,
                }) {
                    Ok(playback) => {
                        if shuffle {
                            playback.toggle_shuffle();
                        }
                        self.attach_playback(&playback, widgets, &sender);
                        // Video opens the player; music keeps the page it
                        // was started from, playing in the bar.
                        if playback.is_video() {
                            widgets.window_stack.push(&player_page(
                                &playback,
                                &sender,
                                &self.cards,
                            ));
                        }
                        self.playback = Some(playback);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "cannot build the player");
                        toast_text(widgets, &format!("Cannot play: {e}"));
                    }
                }
            }
            Msg::MinimizePlayer => {
                close_player(widgets);
                self.refresh_home(&sender);
            }
            Msg::ExpandPlayer => {
                let showing_player = widgets
                    .window_stack
                    .visible_page()
                    .is_some_and(|p| is_player(&p));
                if let Some(playback) = &self.playback
                    && !showing_player
                {
                    widgets
                        .window_stack
                        .push(&player_page(playback, &sender, &self.cards));
                }
            }
            Msg::StopPlayback => {
                self.stop_playback(widgets);
                self.refresh_home(&sender);
            }
            Msg::Enqueue(item, at) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.command(&sender, async move {
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
                self.command(&sender, async move {
                    let done = if watched {
                        session.mark_watched(&id).await
                    } else {
                        session.mark_unwatched(&id).await
                    };
                    Loaded::WatchChanged(done)
                });
            }
            Msg::ConnectionChanged(state) => show_connection(widgets, self.server.as_ref(), &state),
            Msg::RememberTracks(id, selection) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.command(&sender, async move {
                    Loaded::Saved(session.set_track_override(&id, selection).await)
                });
            }
            Msg::ForgetTracks(item) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.command(&sender, async move {
                    let done = session.clear_track_override(item.id()).await;
                    Loaded::TracksForgotten(item, done)
                });
            }
            Msg::SetTrackPreferences(preferences) => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                self.command(&sender, async move {
                    Loaded::Saved(session.set_track_preferences(&preferences).await)
                });
            }
            Msg::OpenPreferences => match self.session.clone() {
                // The Playback page shows the user's preferences, from the
                // server.
                Some(session) => self.command(&sender, async move {
                    Loaded::Preferences(session.track_preferences().await)
                }),
                None => self.preferences_dialog(None, &sender).present(Some(root)),
            },
            Msg::SetTheme(theme) => {
                self.theme.apply(theme);
                self.settings.set_theme(theme);
            }
            Msg::SetSkipIntros(on) => {
                self.settings.set_skip_intros(on);
                if let Some(playback) = &self.playback {
                    playback.set_skip_intros(on);
                }
            }
            Msg::SetShowDiagnostics(on) => self.settings.set_show_diagnostics(on),
            Msg::SessionEnded => self.signed_out(widgets, &sender),
            Msg::StoppedRemembering => {
                self.invalidate_session();
                self.stop_playback(widgets);
                self.queued.clear();
                toast_text(
                    widgets,
                    "This computer no longer remembers you. Sign in again to watch.",
                );
                if let Some(server) = self.server.clone() {
                    self.load_profiles(server, &sender);
                }
            }
            Msg::SetShowHdr(on) => {
                // Part of the device's capabilities, told as it connects.
                self.settings.set_show_hdr(on);
                toast_text(
                    widgets,
                    if on {
                        "HDR on · applies when Spoolframe next starts"
                    } else {
                        "HDR off · applies when Spoolframe next starts"
                    },
                );
            }
            Msg::SetUseRelays(on) => {
                // The SDK's endpoint is made with or without relays.
                self.settings.set_use_relays(on);
                toast_text(
                    widgets,
                    if on {
                        "Relays on · applies when Spoolframe next starts"
                    } else {
                        "Relays off · applies when Spoolframe next starts"
                    },
                );
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
        let Some(loaded) = loaded.accept() else {
            return;
        };
        // A page for an item no longer the one being opened is dropped:
        // a later click asked for another.
        if let Some(id) = loaded.opened() {
            if self.opening.as_ref() != Some(id) {
                return;
            }
            self.opening = None;
        }
        if let Loaded::Activated(generation, ..) = &loaded
            && *generation != self.profile_scope.generation()
        {
            return;
        }
        // The answer is in: the next try may go.
        match &loaded {
            Loaded::SignedIn(_) => self.sign_in_in_flight = false,
            Loaded::Unlocked(..) => self.unlock_in_flight = false,
            _ => {}
        }
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
                    Rc::new(move || sender.input(Msg::OpenAddServer(None)))
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
            Loaded::InviteRead(link, read) => {
                if let Some(dialog) = &widgets.invite {
                    dialog.read(&link, read);
                }
            }
            // A key claiming a saved server's id: the identity alert, as
            // for any connect.
            Loaded::InviteConnected(Err(e @ BackendError::IdentityChanged { .. })) => {
                if let Some(dialog) = widgets.invite.take() {
                    dialog.dialog.close();
                }
                server_error(widgets, None, &e, &sender, root);
            }
            Loaded::InviteConnected(answer) => {
                if let Some(dialog) = &widgets.invite {
                    dialog.connected(answer);
                }
            }
            Loaded::InviteServerSaved(answer) => {
                if answer.is_ok() {
                    self.load_servers(&sender);
                }
                if let Some(dialog) = &widgets.invite {
                    dialog.saved(answer);
                }
            }
            Loaded::InviteJoined(_, Ok(profile)) => {
                let server = widgets.invite.take().and_then(|dialog| {
                    dialog.dialog.close();
                    dialog.server()
                });
                self.load_servers(&sender);
                if server.is_some() {
                    self.server = server;
                }
                self.enter(profile, widgets, &sender);
            }
            Loaded::InviteJoined(username, Err(e)) => {
                if let Some(dialog) = &widgets.invite {
                    dialog.join_refused(&e, &username);
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
                    server_error(widgets, None, &e, &sender, root);
                }
            },
            Loaded::Checked(_, verifying, Ok(server)) => {
                check_fingerprint(&server, verifying, &sender, root)
            }
            Loaded::Checked(server, _, Err(e)) => {
                server_error(widgets, Some(&server), &e, &sender, root)
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
            Loaded::Pinned(_, Err(e)) => server_error(widgets, None, &e, &sender, root),
            Loaded::Hidden(item, shelf, hide, Ok(())) => {
                self.load_home(&sender);
                if hide {
                    let title = views::Facts::of(&item).caption();
                    // Markup off before the title: a builder sets the title
                    // first, and "Crime & Punishment" failed to parse.
                    let toast = adw::Toast::builder()
                        .use_markup(false)
                        .button_label("Undo")
                        .build();
                    toast.set_title(&format!("{title} removed from {}", shelf.name()));
                    let sender = sender.clone();
                    toast.connect_button_clicked(move |_| {
                        sender.input(Msg::HideFromContinue(item.clone(), shelf, false))
                    });
                    widgets.toasts.add_toast(toast);
                }
            }
            Loaded::Hidden(_, _, _, Err(e)) => toast(widgets, &e),
            Loaded::Activated(generation, profile, result) => {
                if generation != self.profile_scope.generation() {
                    return;
                }
                match result {
                    Ok(()) => self.enter_active(*profile, widgets, &sender),
                    Err(e) => toast(widgets, &e),
                }
            }
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
            Loaded::ServerRemoved(key, Ok(())) => {
                if self.server.as_ref().is_some_and(|s| s.key == key) {
                    self.invalidate_session();
                    self.server = None;
                }
                self.load_servers(&sender);
                widgets.screens.set_visible_child_name("servers");
            }
            Loaded::ServerRemoved(_, Err(e)) => toast(widgets, &e),
            Loaded::ProfileForgotten(server, Ok(())) => self.load_profiles(server, &sender),
            Loaded::ProfileForgotten(_, Err(e)) => toast(widgets, &e),
            Loaded::ProfileSignedOut(server, signed_out) => {
                if let Err(e) = signed_out {
                    toast(widgets, &e);
                }
                self.load_profiles(server, &sender);
            }
            Loaded::Resumed(_, Ok(profile)) if profile.locked => {
                self.ask_pin(profile, widgets, &sender)
            }
            Loaded::Resumed(_, Ok(profile)) => self.enter(profile, widgets, &sender),
            // Trust lapsed, was removed, or the account was disabled: the
            // server does not say which, so one message for all, and the
            // profile leaves the picker as the list is asked again.
            Loaded::Resumed(profile, Err(BackendError::SignInRequired)) => {
                self.needs_sign_in = Some(*profile);
                if let Some(server) = self.server.clone() {
                    self.load_profiles(server, &sender);
                }
            }
            Loaded::Resumed(_, Err(e)) => toast(widgets, &e),
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
                let forget: start::Choose<Profile> = {
                    let sender = sender.clone();
                    Rc::new(move |p| sender.input(Msg::ForgetProfile(p)))
                };
                let sign_out: start::Choose<Profile> = {
                    let sender = sender.clone();
                    Rc::new(move |p| sender.input(Msg::AskSignOut(Some(p))))
                };
                let sign_in_as: start::Choose<Profile> = {
                    let sender = sender.clone();
                    Rc::new(move |p| sender.input(Msg::SignInAs(p.username)))
                };
                let path: Rc<dyn Fn() -> Option<&'static str>> = {
                    let (client, key) = (self.client.clone(), server.key.clone());
                    Rc::new(move || {
                        spoolframe_linux_core::format::path_chip(client.connection_path(&key))
                    })
                };
                let needs_sign_in = self.needs_sign_in.take();
                widgets.profiles.set_child(Some(&start::profiles(
                    &server,
                    &profiles,
                    &start::ProfileActions {
                        choose,
                        forget,
                        sign_out,
                        sign_in_as,
                    },
                    needs_sign_in.as_ref(),
                    &sign_in,
                    &back,
                    path,
                )));
                widgets.screens.set_visible_child_name("profiles");
                self.server = Some(server);
                let opens_at_start = std::mem::take(&mut self.starting)
                    .then(|| start::opens_at_start(&profiles))
                    .flatten();
                if let Some(profile) = opens_at_start {
                    // Past Who's watching; Switch profile still leads to it.
                    sender.input(Msg::ChooseProfile(profile.clone()));
                } else if profiles.is_empty() && needs_sign_in.is_none() {
                    // Nobody signed in here yet: go straight to signing in.
                    sender.input(Msg::OpenSignIn);
                }
            }
            Loaded::Profiles(server, Err(e)) => {
                tracing::warn!(server = %server.name, error = %e, "cannot list profiles");
                server_error(widgets, Some(&server), &e, &sender, root);
                widgets.screens.set_visible_child_name("servers");
            }
            Loaded::SignedIn(Ok(profile)) => {
                if let Some(phone) = widgets.remote_sign_in.take() {
                    phone.dialog.close();
                }
                if let Some(passkey) = widgets.passkey_sign_in.take() {
                    passkey.dialog.close();
                }
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
            // Signed in, but the admin has not finished setup: its own
            // page, where the library would be.
            Loaded::Libraries(Err(BackendError::NotSetUp))
            | Loaded::Home(Err(BackendError::NotSetUp)) => {
                if let Some(server) = &self.server {
                    widgets.not_set_up.show(server);
                }
                widgets.screens.set_visible_child_name("not-set-up");
            }
            Loaded::Libraries(Err(e)) => show_error(widgets, "Home", &e, home_again(&sender)),
            Loaded::Home(Ok(home)) => {
                let mut rows = Vec::new();
                // A row with nothing in it is left out, not shown empty.
                if !home.continue_watching.is_empty() {
                    rows.push(views::row(
                        "Continue watching",
                        home.continue_watching.iter().map(|i| {
                            views::continue_card(i, cards, views::Shelf::ContinueWatching)
                        }),
                    ));
                }
                if !home.next_up.is_empty() {
                    rows.push(views::row(
                        "Next up",
                        home.next_up
                            .iter()
                            .map(|i| views::continue_card(i, cards, views::Shelf::NextUp)),
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
            Loaded::Home(Err(e)) => show_error(widgets, "Home", &e, home_again(&sender)),
            Loaded::Person(person, Ok((items, total))) => push(
                widgets,
                &person.name,
                &views::grid(
                    &match total {
                        1 => "1 item".to_owned(),
                        n => format!("{n} items"),
                    },
                    items.iter().map(|i| views::poster_card(i, cards)),
                ),
            ),
            Loaded::Person(person, Err(e)) => push_error(widgets, &person.name, &e, None),
            Loaded::Playable(item, context, tracks, chosen, next) => {
                // An episode's page is titled with its season, as designed.
                let title = match &item {
                    Item::Episode(e) => views::season_name(e.season),
                    other => views::Facts::of(other).title,
                };
                // The page opens without pickers if the tracks cannot be
                // had; playback then takes the server's choice.
                let tracks = tracks
                    .inspect_err(|e| tracing::warn!(error = %e, "no track list"))
                    .ok();
                let play: pages::Play = {
                    let (sender, item, tracks) = (sender.clone(), item.clone(), tracks.clone());
                    Rc::new(move |from, selection, quality| {
                        sender.input(Msg::Play(Box::new(
                            PlayRequest::one(item.clone(), from, selection, tracks.clone())
                                .at_quality(quality),
                        )))
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
                        next.as_deref(),
                        cards,
                    ),
                );
            }
            Loaded::Show(show, Ok(seasons), open) => {
                let actions = pages::ShowActions {
                    play: {
                        let sender = sender.clone();
                        Rc::new(move |item, from| {
                            sender.input(Msg::Play(Box::new(PlayRequest::one(
                                item, from, None, None,
                            ))))
                        })
                    },
                    set_watched: {
                        let sender = sender.clone();
                        Rc::new(move |id, watched| sender.input(Msg::SetWatched(id, watched)))
                    },
                };
                push(
                    widgets,
                    &show.title,
                    &pages::show(&show, &seasons, open.as_ref(), actions, cards),
                );
            }
            Loaded::Show(show, Err(e), _) => {
                let title = show.title.clone();
                push_error(widgets, &title, &e, Some((Item::Show(show), &sender)))
            }
            Loaded::Artist(artist, Ok(albums)) => {
                push(
                    widgets,
                    &artist.name,
                    &pages::artist(&artist, &albums, cards),
                );
            }
            Loaded::Artist(artist, Err(e)) => {
                let title = artist.name.clone();
                push_error(widgets, &title, &e, Some((Item::Artist(artist), &sender)))
            }
            Loaded::Album(album, Ok((artist, tracks))) => {
                let play: pages::PlayTracks = {
                    let (sender, album) = (sender.clone(), album.clone());
                    Rc::new(move |items, index, shuffle| {
                        sender.input(Msg::Play(Box::new(PlayRequest {
                            items,
                            index,
                            from: None,
                            selection: None,
                            tracks: None,
                            album: Some(album.clone()),
                            shuffle,
                            quality: Default::default(),
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
                    &pages::album(
                        &album,
                        &artist,
                        &tracks,
                        play,
                        enqueue,
                        self.now_playing.subscribe(),
                        cards,
                    ),
                );
            }
            Loaded::Album(album, Err(e)) => {
                let title = album.title.clone();
                push_error(widgets, &title, &e, Some((Item::Album(album), &sender)))
            }
            // Home shows watch state (Continue watching, Next up): show it
            // again, if it is what is on screen.
            Loaded::WatchChanged(Ok(())) => {
                let on_home = self.root == Root::Home
                    && widgets.stack.visible_page().as_ref() == Some(&widgets.root_page)
                    && widgets.window_stack.visible_page().as_ref() == Some(&widgets.library_page);
                if on_home {
                    self.load_home(&sender);
                }
            }
            // The change may have gone through before its answer was lost:
            // Home shows what the server has now.
            Loaded::WatchChanged(Err(BackendError::TimedOut(_) | BackendError::Unreachable(_))) => {
                toast_text(widgets, "The server didn't answer; showing what it has now");
                if self.root == Root::Home {
                    self.load_home(&sender);
                }
            }
            Loaded::WatchChanged(Err(e)) => toast(widgets, &e),
            Loaded::Saved(Ok(())) => {}
            Loaded::Saved(Err(e)) => toast(widgets, &e),
            Loaded::TracksForgotten(item, Ok(())) => {
                toast_text(widgets, "Back to your default tracks");
                // Reopen the page so its pickers show the server's choice.
                widgets.window_stack.pop();
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
        tracks: Option<TrackPreferencesInfo>,
        sender: &ComponentSender<Self>,
    ) -> adw::Dialog {
        let set_theme: Rc<dyn Fn(Theme)> = {
            let sender = sender.clone();
            Rc::new(move |theme| sender.input(Msg::SetTheme(theme)))
        };
        let set_tracks: preferences::SetTracks = {
            let sender = sender.clone();
            Rc::new(move |p| sender.input(Msg::SetTrackPreferences(Box::new(p))))
        };
        let servers = preferences::Servers {
            client: self.client.clone(),
            current: self.server.clone(),
            user: self.profile.as_ref().map(|p| p.display_name.clone()),
            verify: {
                let sender = sender.clone();
                Rc::new(move |server| sender.input(Msg::CheckServer(server, true)))
            },
            use_server: {
                let sender = sender.clone();
                Rc::new(move |server| sender.input(Msg::ChooseServer(server)))
            },
            add: {
                let sender = sender.clone();
                Rc::new(move || sender.input(Msg::ShowServers))
            },
        };
        let set_bitrate: preferences::SetBitrate = {
            let sender = sender.clone();
            Rc::new(move |limit| sender.input(Msg::SetMaxBitrate(limit)))
        };
        let set_skip_intros: preferences::SetSkipIntros = {
            let sender = sender.clone();
            Rc::new(move |on| sender.input(Msg::SetSkipIntros(on)))
        };
        let set_show_hdr: preferences::SetSkipIntros = {
            let sender = sender.clone();
            Rc::new(move |on| sender.input(Msg::SetShowHdr(on)))
        };
        let set_use_relays: preferences::SetSkipIntros = {
            let sender = sender.clone();
            Rc::new(move |on| sender.input(Msg::SetUseRelays(on)))
        };
        let set_show_diagnostics: preferences::SetSkipIntros = {
            let sender = sender.clone();
            Rc::new(move |on| sender.input(Msg::SetShowDiagnostics(on)))
        };
        preferences::dialog(
            self.settings.theme(),
            tracks,
            servers,
            set_theme,
            set_tracks,
            preferences::Device {
                bitrate: self.settings.max_bitrate(),
                set_bitrate,
                show_hdr: self.settings.show_hdr(),
                set_show_hdr,
                use_relays: self.settings.use_relays(),
                set_use_relays,
                skip_intros: self.settings.skip_intros(),
                set_skip_intros,
                show_diagnostics: self.settings.show_diagnostics(),
                set_show_diagnostics,
            },
            self.signed(sender),
        )
    }

    fn load_servers(&self, sender: &ComponentSender<Self>) {
        let client = self.client.clone();
        self.command(
            sender,
            async move { Loaded::Servers(client.servers().await) },
        );
    }

    fn load_profiles(&self, server: Server, sender: &ComponentSender<Self>) {
        let client = self.client.clone();
        self.command(sender, async move {
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
        self.stop_playback(widgets);
        let pending = std::mem::take(&mut self.shutdown_calls);
        let session = match self.client.session(&profile) {
            Ok(session) => session,
            Err(e) => return toast(widgets, &e),
        };
        self.invalidate_session();
        let generation = self.profile_scope.generation();
        self.command(sender, async move {
            for call in pending {
                let _ = call.await;
            }
            Loaded::Activated(generation, Box::new(profile), session.activate().await)
        });
    }

    fn enter_active(
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
                Rc::new(move |item, shelf| {
                    sender.input(Msg::HideFromContinue(Box::new(item), shelf, true))
                })
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
        self.select_root(Root::Home);
        plain_header(widgets);
        // The next profile has its own searches.
        widgets.search = None;
        self.library = None;
        self.stop_playback(widgets);
        // The queue was the last profile's; this one's comes back paused.
        self.queued.clear();
        self.saved_queue = SavedQueue::for_profile(&profile).map(Rc::new);
        self.restore_queue(widgets, sender);
        widgets.stack.pop_to_page(&widgets.root_page);
        widgets.window_stack.pop_to_page(&widgets.library_page);
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
        if let Some(watch) = self.sign_out_watch.take() {
            watch.abort();
        }
        let mut sign_outs = self.client.signed_out(server);
        sign_outs.mark_unchanged();
        let ended = sender.clone();
        self.sign_out_watch = Some(glib::spawn_future_local(async move {
            while sign_outs.changed().await.is_ok() {
                ended.input(Msg::SessionEnded);
            }
        }));
        let mut states = self.client.connection(server);
        let sender = sender.clone();
        self.connection_watch = Some(glib::spawn_future_local(async move {
            loop {
                let state = states.borrow_and_update().clone();
                sender.input(Msg::ConnectionChanged(state));
                if states.changed().await.is_err() {
                    break;
                }
            }
        }));
    }

    /// Shows `playback` in the bar and to the desktop, and saves its queue
    /// as it changes.
    fn attach_playback(
        &self,
        playback: &Rc<Playback>,
        widgets: &AppWidgets,
        sender: &ComponentSender<Self>,
    ) {
        playback.set_skip_intros(self.settings.skip_intros());
        // A queued item that cannot play is passed over with a word, not
        // a stop (P1-23).
        // Music has no page that shows an error (the video player does):
        // a track that cannot play says so in a toast.
        let toasts = widgets.toasts.clone();
        let stop = sender.clone();
        let weak = Rc::downgrade(playback);
        let now_playing = self.now_playing.clone();
        now_playing.send_replace(playback.current().map(|i| i.id().clone()));
        playback.subscribe(move |change| {
            match change {
                Change::Item(item) => {
                    now_playing.send_replace(Some(item.id().clone()));
                }
                Change::Skipped(message) | Change::TrackFailed(message) => {
                    toasts.add_toast(crate::markup::toast(message))
                }
                // Nothing plays it again: the player and the bar close.
                Change::SignedOut => stop.input(Msg::SessionEnded),
                Change::Stopped => {
                    toasts.add_toast(crate::markup::toast(
                        "Playback was stopped by the server's admin",
                    ));
                    stop.input(Msg::StopPlayback);
                }
                Change::Failed(error) if weak.upgrade().is_some_and(|p| !p.is_video()) => {
                    toasts.add_toast(crate::markup::toast(&format!("Cannot play: {error}")));
                }
                _ => {}
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
    fn restore_queue(&mut self, widgets: &AppWidgets, sender: &ComponentSender<Self>) {
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
                self.attach_playback(&playback, widgets, sender);
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
                shuffle: false,
                quality: Default::default(),
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
            self.shutdown_calls.retain(|call| !call.is_finished());
            self.shutdown_calls.extend(playback.stop_waitable());
        }
        widgets.mini_bar.detach();
        widgets.mpris.detach();
        self.now_playing.send_replace(None);
        close_player(widgets);
    }

    /// Home again, if it shows: Continue watching moved with what played.
    fn refresh_home(&mut self, sender: &ComponentSender<Self>) {
        self.load_home(sender);
    }

    /// The search page for the profile in use.
    fn search_page(&self, sender: &ComponentSender<Self>) -> Option<Rc<SearchPage>> {
        let (session, profile, server) = (
            self.session.clone()?,
            self.profile.as_ref()?,
            self.server.as_ref()?,
        );
        let play: Rc<dyn Fn(Item)> = {
            let sender = sender.clone();
            Rc::new(move |item| {
                // Where it stopped, as Resume on its page plays from.
                let from = views::Facts::of(&item).position();
                sender.input(Msg::Play(Box::new(PlayRequest::one(
                    item, from, None, None,
                ))));
            })
        };
        let open_person: Rc<dyn Fn(Person)> = {
            let sender = sender.clone();
            Rc::new(move |person| sender.input(Msg::OpenPerson(Box::new(person))))
        };
        let shuffle: Rc<dyn Fn(Vec<Item>)> = {
            let sender = sender.clone();
            Rc::new(move |items: Vec<Item>| {
                // From a random one, the rest shuffled by the queue, as an
                // album's Shuffle does.
                let index = glib::random_int_range(0, items.len().max(1) as i32) as usize;
                sender.input(Msg::Play(Box::new(PlayRequest {
                    items,
                    index,
                    from: None,
                    selection: None,
                    tracks: None,
                    album: None,
                    shuffle: true,
                    quality: Default::default(),
                })));
            })
        };
        Some(SearchPage::new(search::Needs {
            session,
            cards: self.cards.clone(),
            play,
            open_person,
            signed_out: {
                let sender = sender.clone();
                Rc::new(move || sender.input(Msg::SessionEnded))
            },
            shuffle,
            server: server.name.clone(),
            profile: profile.display_name.clone(),
            recent: search::RecentSearches::for_profile(profile),
        }))
    }

    /// The profile in use, for Preferences → Profile.
    fn signed(&self, sender: &ComponentSender<Self>) -> Option<crate::profile_page::Signed> {
        let (session, server, profile) = (
            self.session.clone()?,
            self.server.as_ref()?,
            self.profile.as_ref()?,
        );
        Some(crate::profile_page::Signed {
            session,
            server: server.clone(),
            server_key: server.key.0.clone(),
            server_name: server.name.clone(),
            display_name: profile.display_name.clone(),
            username: profile.username.clone(),
            left: {
                let sender = sender.clone();
                Rc::new(move || sender.input(Msg::StoppedRemembering))
            },
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
        self.invalidate_session();
        self.unlock_in_flight = false;
        let server_name = self.server.as_ref().map_or("", |s| s.name.as_str());
        let unlock: Rc<dyn Fn(String)> = {
            let sender = sender.clone();
            Rc::new(move |pin| sender.input(Msg::Unlock(Secret(pin))))
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
        self.invalidate_session();
        // A player still open would keep playing behind "Who's watching".
        self.stop_playback(widgets);
        self.queued.clear();
        // Signed out elsewhere, a password changed, a session revoked:
        // it is no fault here, so it reads as news, not an error.
        toast_text(
            widgets,
            "You were signed out on this device. Sign in again.",
        );
        if let Some(server) = self.server.clone() {
            self.load_profiles(server, sender);
        } else {
            sender.input(Msg::ShowServers);
        }
    }

    fn invalidate_session(&mut self) {
        self.profile_scope.invalidate();
        self.session = None;
        self.opening = None;
        self.unlock_in_flight = false;
        self.sign_in_in_flight = false;
        if let Some(load) = self.root_load.take() {
            load.abort();
        }
    }

    fn command(
        &self,
        sender: &ComponentSender<Self>,
        load: impl std::future::Future<Output = Loaded> + Send + 'static,
    ) {
        let token = self.profile_scope.token();
        sender.oneshot_command(async move {
            Completion {
                token,
                loaded: load.await,
            }
        });
    }

    fn load_libraries(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.command(sender, async move {
            Loaded::Libraries(session.libraries().await)
        });
    }

    /// Home, if it is the sidebar's choice: asked for by Undo from
    /// another page, it would replace that page.
    fn load_home(&mut self, sender: &ComponentSender<Self>) {
        if self.root != Root::Home {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        self.load_root(sender, async move { Loaded::Home(session.home().await) });
    }

    /// A new sidebar choice: the last one's load, if still going, is
    /// aborted.
    fn select_root(&mut self, root: Root) {
        self.root = root;
        if let Some(load) = self.root_load.take() {
            load.abort();
        }
    }

    /// Loads the root page's content on the runtime and hands the answer
    /// to `update_cmd`, unless the next sidebar choice aborts it first.
    fn load_root(
        &mut self,
        sender: &ComponentSender<Self>,
        load: impl std::future::Future<Output = Loaded> + Send + 'static,
    ) {
        if let Some(old) = self.root_load.take() {
            old.abort();
        }
        let token = self.profile_scope.token();
        let (task, sender) = (relm4::spawn(load), sender.clone());
        self.root_load = Some(glib::spawn_future_local(async move {
            if let Ok(loaded) = task.await {
                sender.command_sender().emit(Completion { token, loaded });
            }
        }));
    }

    /// Loads what the item's page needs, then pushes the page.
    fn open_item(&mut self, item: Item, sender: &ComponentSender<Self>) {
        let Some(backend) = self.session.clone() else {
            return;
        };
        // A track plays; anything else opens a page once its load is in.
        if !matches!(item, Item::Track(_)) {
            if self.opening.as_ref() == Some(item.id()) {
                return;
            }
            self.opening = Some(item.id().clone());
        }
        match item {
            Item::Movie(ref m) => {
                let id = m.id.clone();
                self.command(sender, async move {
                    // The card's copy may be from before the last
                    // playback: Resume would start from where it was then.
                    let (item, tracks, chosen) = tokio::join!(
                        fresh(backend.as_ref(), item),
                        backend.tracks(&id),
                        server_choice(backend.as_ref(), &id),
                    );
                    Loaded::Playable(item, None, tracks, chosen, None)
                });
            }
            Item::Episode(ref e) => {
                let (id, show, episode) = (e.id.clone(), e.show_id.clone(), e.clone());
                self.command(sender, async move {
                    let (item, show, tracks, chosen, next) = tokio::join!(
                        fresh(backend.as_ref(), item),
                        backend.item(&show),
                        backend.tracks(&id),
                        server_choice(backend.as_ref(), &id),
                        next_episode(backend.as_ref(), &episode),
                    );
                    // "Next episode" is a convenience: none if it cannot be had.
                    let next = next.ok().flatten().map(Box::new);
                    // The show's title is context, not essential: the page
                    // opens without it if the show cannot be loaded.
                    let context = match show {
                        Ok((Item::Show(s), _)) => Some(s.title),
                        _ => None,
                    };
                    Loaded::Playable(item, context, tracks, chosen, next)
                });
            }
            Item::Show(show) => self.command(sender, async move {
                let (show, seasons) = show_with_seasons(backend.as_ref(), show).await;
                Loaded::Show(show, seasons, None)
            }),
            // A season opens its show's page, on that season's tab.
            Item::Season(season) => self.command(sender, async move {
                let show = Show {
                    id: season.show_id.clone(),
                    title: season.show_title.clone().unwrap_or_default(),
                    year: None,
                    summary: None,
                    artwork: Vec::new(),
                    content_rating: None,
                    genres: Vec::new(),
                    season_count: None,
                };
                let (show, seasons) = show_with_seasons(backend.as_ref(), show).await;
                Loaded::Show(show, seasons, Some(season.id))
            }),
            // The artist and the album as the server has them: one opened
            // from a track's menu carries only its ID.
            Item::Artist(artist) => self.command(sender, async move {
                match backend.item(&artist.id).await {
                    Ok((Item::Artist(full), albums)) => Loaded::Artist(full, Ok(albums)),
                    Ok((_, albums)) => Loaded::Artist(artist, Ok(albums)),
                    Err(e) => Loaded::Artist(artist, Err(e)),
                }
            }),
            Item::Album(album) => self.command(sender, async move {
                let (album, tracks) = match backend.item(&album.id).await {
                    Ok((Item::Album(full), tracks)) => (full, Ok(tracks)),
                    Ok((_, tracks)) => (album, Ok(tracks)),
                    Err(e) => (album, Err(e)),
                };
                let loaded = async {
                    let tracks = tracks?;
                    let artist = match &album.artist_name {
                        Some(name) => name.clone(),
                        None => match backend.item(&album.artist_id).await?.0 {
                            Item::Artist(a) => a.name,
                            other => other.id().to_string(),
                        },
                    };
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
/// A show as the server has it (an item opened from an episode carries
/// only its ID and title), and its seasons with their episodes.
async fn show_with_seasons(
    backend: &dyn Session,
    show: Show,
) -> (Show, Result<Vec<SeasonEpisodes>, BackendError>) {
    let (full, children) = match backend.item(&show.id).await {
        Ok((Item::Show(full), children)) => (full, children),
        Ok((_, children)) => (show, children),
        Err(e) => return (show, Err(e)),
    };
    let mut seasons = Vec::new();
    for child in children {
        if let Item::Season(season) = child {
            match backend.item(&season.id).await {
                Ok((_, episodes)) => seasons.push((season, episodes)),
                Err(e) => return (full, Err(e)),
            }
        }
    }
    (full, Ok(seasons))
}

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

/// `item` as the server has it now, with its watch state; the copy given
/// when the server cannot say.
async fn fresh(session: &dyn Session, item: Item) -> Item {
    match session.item(item.id()).await {
        Ok((now, _)) => now,
        Err(_) => item,
    }
}

fn open_with(sender: &ComponentSender<App>) -> views::Open {
    let sender = sender.clone();
    Rc::new(move |item| sender.input(Msg::Open(Box::new(item))))
}

fn show_connection(widgets: &AppWidgets, server: Option<&Server>, state: &ConnectionState) {
    let name = server.map_or("the server", |s| s.name.as_str());
    let (status, banner) = connection_text(name, state);
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

/// The status beside the server's name, and the banner over the page, if
/// any, for a connection in `state`. A link the SDK stopped dialling says
/// why, since it will not reconnect by itself.
fn connection_text(name: &str, state: &ConnectionState) -> (String, Option<String>) {
    match state {
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
        ConnectionState::Stopped(BackendError::UpdateRequired { server_protocol }) => (
            "update needed".to_owned(),
            Some(update_text(name, *server_protocol)),
        ),
        ConnectionState::Stopped(why) => (
            "not connected".to_owned(),
            Some(format!("Not connected to {name}: {why}.")),
        ),
    }
}

/// What to do when this app and the server speak different protocol
/// revisions: whichever is older needs updating, when the server said its
/// revision.
fn update_text(name: &str, server_protocol: Option<u32>) -> String {
    let ours = spoolframe_protocol::PROTOCOL_VERSION;
    match server_protocol {
        Some(theirs) if theirs > ours => {
            format!("{name} runs a newer Spoolframe. Update this app to connect.")
        }
        Some(theirs) if theirs < ours => {
            format!("{name} runs an older Spoolframe. Ask its admin to update the server.")
        }
        _ => format!(
            "This app and {name} speak different versions. Update Spoolframe, or ask the server's admin to update it."
        ),
    }
}

fn sidebar_row(title: &str, icon: &str) -> gtk::Box {
    let row = gtk::Box::builder().spacing(12).build();
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
    widgets.library_heading.set_visible(false);
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

/// Search's scope when opened from a library of `kind`.
fn scope_of(kind: LibraryKind) -> SearchScope {
    match kind {
        LibraryKind::Movies => SearchScope::Movies,
        LibraryKind::Shows => SearchScope::Shows,
        LibraryKind::Music => SearchScope::Music,
    }
}

fn show_error(widgets: &AppWidgets, title: &str, error: &BackendError, retry: Rc<dyn Fn()>) {
    show_content(widgets);
    widgets.root_page.set_title(title);
    clear(&widgets.content);
    widgets.content.append(&error_status(error, Some(retry)));
}

/// The root page's header back to its title, when search, which puts its
/// field there, gives way to another root. Done as the choice is made: a
/// header changed while GTK maps or unmaps the page breaks its widget tree.
fn plain_header(widgets: &AppWidgets) {
    widgets
        .root_header
        .set_title_widget(Some(&widgets.root_title));
}

/// An item page, over the whole window as designed: the sidebar is the
/// library's, and the page's own header has Back.
fn push(widgets: &AppWidgets, title: &str, body: &impl IsA<gtk::Widget>) {
    widgets.window_stack.push(&pages::page(title, body));
}

/// The full player and Now Playing are tagged so: the bar hides under
/// them, and minimizing closes only them.
const PLAYER_TAG: &str = "player";

fn is_player(page: &adw::NavigationPage) -> bool {
    page.tag().as_deref() == Some(PLAYER_TAG)
}

/// Closes the full player or Now Playing, leaving whatever page was under
/// it.
fn close_player(widgets: &AppWidgets) {
    while widgets
        .window_stack
        .visible_page()
        .is_some_and(|p| is_player(&p))
    {
        widgets.window_stack.pop();
    }
}

/// An item page that did not load; `reopen` gives it another go in its
/// place.
fn push_error(
    widgets: &AppWidgets,
    title: &str,
    error: &BackendError,
    reopen: Option<(Item, &ComponentSender<App>)>,
) {
    let retry = reopen.map(|(item, sender)| -> Rc<dyn Fn()> {
        let (window_stack, sender) = (widgets.window_stack.clone(), sender.clone());
        Rc::new(move || {
            window_stack.pop();
            sender.input(Msg::Open(Box::new(item.clone())));
        })
    });
    push(widgets, title, &error_status(error, retry));
}

/// Home, loaded again.
fn home_again(sender: &ComponentSender<App>) -> Rc<dyn Fn()> {
    let sender = sender.clone();
    Rc::new(move || sender.input(Msg::Select(ROW_HOME)))
}

fn toast(widgets: &AppWidgets, error: &BackendError) {
    toast_text(widgets, &error.to_string());
}

fn toast_text(widgets: &AppWidgets, text: &str) {
    widgets.toasts.add_toast(crate::markup::toast(text));
}

fn error_status(error: &BackendError, retry: Option<Rc<dyn Fn()>>) -> adw::StatusPage {
    let (title, description) = match error {
        // Off, asleep or out of reach; the SDK dials again on its own.
        BackendError::TimedOut(_) | BackendError::Unreachable(_) => (
            "The server didn't answer",
            "It may be off, asleep or out of reach. Try again in a moment.".to_owned(),
        ),
        // A library on a Plex server that is down (P3-11), or a mount:
        // listed, but not there now.
        BackendError::Unavailable(why) => (
            "Not available right now",
            format!("{} Try again once it is back.", sentence(why)),
        ),
        BackendError::UpdateRequired { server_protocol } => (
            "Spoolframe needs updating",
            update_text("The server", *server_protocol),
        ),
        other => ("Could not load this page", sentence(&other.to_string())),
    };
    let page = adw::StatusPage::builder()
        .icon_name(crate::icons::ERROR)
        .title(title)
        // The description is markup; the error's text is not.
        .description(crate::markup::plain(&description))
        .vexpand(true)
        .build();
    if let Some(retry) = retry {
        let button = gtk::Button::builder()
            .label("Try again")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        button.connect_clicked(move |_| retry());
        page.set_child(Some(&button));
    }
    page
}

/// A message as a sentence: capitalised, with a full stop.
pub(crate) fn sentence(message: &str) -> String {
    let mut chars = message.trim().chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut sentence: String = first.to_uppercase().chain(chars).collect();
    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
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
    let page = if playback.is_video() {
        player::page(playback, actions)
    } else {
        nowplaying::page(playback, actions, cards)
    };
    page.set_tag(Some(PLAYER_TAG));
    page
}

/// Invite links the desktop opens (P6-01): `spoolframe://invite?…`, from
/// a browser or a file manager, through the desktop entry's scheme
/// handler. GTK hands them to the running app, which may not have its
/// window yet: those wait for it.
pub mod opened {
    use std::cell::RefCell;

    use relm4::ComponentSender;

    use super::{App, Msg};

    thread_local! {
        static OPENED: RefCell<(Option<ComponentSender<App>>, Vec<String>)> =
            const { RefCell::new((None, Vec::new())) };
    }

    /// `link` goes to "Add a server", now or once the window is up.
    pub fn open(link: String) {
        OPENED.with(|opened| {
            let mut opened = opened.borrow_mut();
            match &opened.0 {
                Some(sender) => sender.input(Msg::OpenAddServer(Some(link))),
                None => opened.1.push(link),
            }
        });
    }

    /// From now on links go to `sender`; the ones that waited are
    /// returned, oldest first.
    pub(super) fn listen(sender: ComponentSender<App>) -> Vec<String> {
        OPENED.with(|opened| {
            let mut opened = opened.borrow_mut();
            opened.0 = Some(sender);
            std::mem::take(&mut opened.1)
        })
    }

    #[cfg(test)]
    pub(super) fn waiting() -> Vec<String> {
        OPENED.with(|opened| opened.borrow().1.clone())
    }
}

/// What the invite dialog's buttons send.
fn invite_actions(sender: &ComponentSender<App>) -> crate::invite::Actions {
    let send = |make: fn(String) -> Msg| -> Rc<dyn Fn(String)> {
        let sender = sender.clone();
        Rc::new(move |link| sender.input(make(link)))
    };
    let join: Rc<dyn Fn(String, String, Option<String>, String)> = {
        let sender = sender.clone();
        Rc::new(move |link, username, display_name, password| {
            sender.input(Msg::JoinInvite(
                link,
                username,
                display_name,
                Secret(password),
            ))
        })
    };
    let sign_in: Rc<dyn Fn(Server)> = {
        let sender = sender.clone();
        Rc::new(move |server| sender.input(Msg::SignInToInvited(server)))
    };
    let by_key: Rc<dyn Fn()> = {
        let sender = sender.clone();
        Rc::new(move || sender.input(Msg::OpenAddServerByKey))
    };
    crate::invite::Actions {
        read: send(Msg::ReadInvite),
        connect: send(Msg::ConnectInvite),
        save: send(Msg::SaveInviteServer),
        join,
        sign_in,
        by_key,
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
/// the server-id alert when it answered with another id (PX-14),
/// otherwise a toast. `server` is the one asked, when known.
fn server_error(
    widgets: &AppWidgets,
    server: Option<&Server>,
    error: &BackendError,
    sender: &ComponentSender<App>,
    root: &adw::ApplicationWindow,
) {
    let name = server.map_or("This server", |s| s.name.as_str());
    match (error, server) {
        (BackendError::IdentityChanged { saved, now }, _) => {
            let trust: Rc<dyn Fn()> = {
                let (sender, saved, now) = (sender.clone(), saved.clone(), now.clone());
                Rc::new(move || {
                    sender.input(Msg::TrustIdentity(
                        ServerKey(saved.clone()),
                        ServerKey(now.clone()),
                    ))
                })
            };
            let site = server.and_then(Server::passkey_site);
            start::identity_changed(name, saved, now, site.as_deref(), trust).present(Some(root));
        }
        (BackendError::ServerIdChanged { saved, now }, Some(server)) => {
            let remove: Rc<dyn Fn()> = {
                let (sender, key) = (sender.clone(), server.key.clone());
                Rc::new(move || sender.input(Msg::RemoveServer(key.clone())))
            };
            start::server_id_changed(name, saved, now, remove).present(Some(root));
        }
        (BackendError::UpdateRequired { server_protocol }, _) => {
            toast_text(widgets, &update_text(name, *server_protocol))
        }
        (other, _) => toast(widgets, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fake's server stops being dialled for `why`; what the banner
    /// then says.
    fn banner_once_stopped(why: BackendError) -> (String, Option<String>) {
        let client = spoolframe_linux_core::fake::FakeClient::new(None);
        let server = spoolframe_linux_core::fake::FakeClient::profile("andrey").server;
        let states = client.connection(&server);
        assert_eq!(*states.borrow(), ConnectionState::Connected);
        client.stop_dialling(&server, why);
        let state = states.borrow().clone();
        connection_text("Home Media", &state)
    }

    /// An app older than the server is told to update itself, not that
    /// it is reconnecting: the SDK has stopped dialling.
    #[test]
    fn an_app_too_old_for_the_server_says_to_update_it() {
        let newer = spoolframe_protocol::PROTOCOL_VERSION + 1;
        assert_eq!(
            banner_once_stopped(BackendError::UpdateRequired {
                server_protocol: Some(newer)
            }),
            (
                "update needed".to_owned(),
                Some("Home Media runs a newer Spoolframe. Update this app to connect.".to_owned())
            )
        );
    }

    #[test]
    fn a_server_older_than_the_app_is_its_admins_to_update() {
        let older = spoolframe_protocol::PROTOCOL_VERSION - 1;
        let (_, banner) = banner_once_stopped(BackendError::UpdateRequired {
            server_protocol: Some(older),
        });
        assert_eq!(
            banner.as_deref(),
            Some("Home Media runs an older Spoolframe. Ask its admin to update the server.")
        );
    }

    /// Any other reason the SDK stopped dialling is shown as it is, in
    /// place of "reconnecting".
    #[test]
    fn a_link_stopped_for_another_reason_says_why() {
        let (status, banner) = banner_once_stopped(BackendError::ServerFull);
        assert_eq!(status, "not connected");
        assert_eq!(
            banner.as_deref(),
            Some("Not connected to Home Media: the server is busy; try again later.")
        );
    }

    /// A link the desktop opens before the window is up waits for it.
    #[test]
    fn an_invite_link_opened_before_the_window_waits_for_it() {
        opened::open("spoolframe://invite?server=early".into());
        assert_eq!(opened::waiting(), ["spoolframe://invite?server=early"]);
    }

    /// GTK passes an opened link as a GFile; its URI must be the link as
    /// written, escapes and all, or the invite would not read.
    #[test]
    fn an_opened_invite_link_comes_through_gio_unchanged() {
        let link = spoolframe_linux_core::fake::FakeClient::invite_link(Some(
            spoolframe_linux_core::fake::INVITE_CODE,
        ));
        let link = link.replace("name=Cabin", "name=Home%20Media");
        assert_eq!(gtk::gio::File::for_uri(&link).uri(), link);
    }

    #[test]
    fn a_server_message_reads_as_a_sentence() {
        assert_eq!(
            sentence("Cabin does not answer: connection refused"),
            "Cabin does not answer: connection refused."
        );
    }

    #[test]
    fn a_message_is_capitalised_once() {
        assert_eq!(
            sentence("the media file could not be read."),
            "The media file could not be read."
        );
    }

    #[test]
    fn stale_profile_replies_drop_successes_and_authentication_errors() {
        let scope = spoolframe_client_sdk::ProfileScope::default();
        let old = scope.token();
        scope.invalidate();
        for loaded in [
            Loaded::Enqueued(Enqueue::Next, Ok(Vec::new())),
            Loaded::Libraries(Err(BackendError::SignInRequired)),
            Loaded::Home(Err(BackendError::ProfileLocked)),
        ] {
            assert!(
                Completion {
                    token: old.clone(),
                    loaded
                }
                .accept()
                .is_none()
            );
        }
        assert!(
            Completion {
                token: scope.token(),
                loaded: Loaded::Home(Err(BackendError::ProfileLocked))
            }
            .accept()
            .unwrap()
            .profile_locked()
        );
    }

    #[test]
    fn playable_pages_route_authentication_failures() {
        let item = Item::Movie(spoolframe_types::Movie {
            id: "plex:movie:1".parse().unwrap(),
            title: "Movie".into(),
            year: None,
            summary: None,
            content_rating: None,
            duration: None,
            artwork: Vec::new(),
            playback: Default::default(),
        });
        for error in [BackendError::SignInRequired, BackendError::ProfileLocked] {
            let loaded = Loaded::Playable(item.clone(), None, Err(error.clone()), None, None);
            assert_eq!(loaded.session_error(), Some(&error));
        }
    }

    #[test]
    fn a_secret_never_prints() {
        let shown = format!("{:?}", Msg::Unlock(Secret("1234".into())));
        assert!(!shown.contains("1234"), "{shown}");
    }
}
