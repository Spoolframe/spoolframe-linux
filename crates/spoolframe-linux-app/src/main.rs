mod admin_invites;
mod app;
mod artwork;
mod browser;
mod capabilities;
mod confirm;
mod create_identity;
mod denylist;
mod dialog;
mod file_helper;
mod fonts;
mod hearts;
mod history;
mod icons;
mod identity_here;
mod identity_page;
mod identity_sign_in;
mod import_identity;
mod invite;
mod key_prompt;
mod leave_server;
mod library;
mod licenses;
mod licenses_page;
mod logo;
// Text shown as it is where GTK reads markup, and the checks over the
// sources that keep it so.
mod markup;
mod minibar;
mod mpris;
mod nowplaying;
mod pages;
mod pair_device;
mod passkey_dialog;
mod passkeys;
mod password_change;
mod pin;
mod pipeline;
mod playback;
mod player;
mod playlist_edit;
mod playlists;
mod preferences;
mod profile_page;
mod qr;
mod quality;
mod queue;
mod reachability;
mod recover_identity;
mod remote_sign_in;
mod sandbox;
mod saved_queue;
mod scrub;
mod search;
mod server_relays;
mod settings;
mod settings_window;
mod share_picker;
mod sidebar;
mod source;
mod srt_markup;
mod start;
#[cfg(test)]
mod stylesheet;
mod theme;
mod tracks;
mod transport;
#[cfg(test)]
mod ui_test;
mod views;
mod volume;
mod wake;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use relm4::RelmApp;
use spoolframe_client_sdk as sdk;
use spoolframe_linux_core::artcache::{ArtworkCache, Limits};
use spoolframe_linux_core::backend::Client;
use spoolframe_linux_core::sdk::{SdkClient, SdkStorage};
use spoolframe_linux_core::storage::{DataStore, Storage};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static ALLOC: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

/// The application ID: GNOME uses it for the desktop file, the settings
/// and the single-instance check.
const APP_ID: &str = "casa.coldsteel.Spoolframe";

/// Spoolframe Linux: Spoolframe Linux client: Relm4, libadwaita, GStreamer.
#[derive(Parser)]
#[command(name = "spoolframe-linux")]
#[command(version)]
struct Cli {
    /// Increase log verbosity (-v = debug, -vv = trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Confine the app, say what it can and cannot do (`FILE` is a file
    /// it must not read), and exit: 0 when confined as it should be.
    #[arg(long, hide = true, value_name = "FILE")]
    self_check: Option<std::path::PathBuf>,
    /// An invite link to open in "Add a server" (spoolframe://invite?…),
    /// as the desktop passes one it was asked to open.
    #[arg(value_name = "LINK")]
    link: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    // SPOOLFRAME_LINUX_LOG overrides the level with a full filter
    // expression, e.g. `SPOOLFRAME_LINUX_LOG=spoolframe_linux_core=trace`.
    let filter = std::env::var("SPOOLFRAME_LINUX_LOG")
        .map(EnvFilter::new)
        .unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    // Before GTK, GStreamer or the SDK start a thread (C-15).
    // Security keys (P7-08) through a helper forked now, before the app
    // confines itself and before any thread: it alone opens keys, with
    // every device under /dev, so a key plugged in later still works.
    if std::env::var_os(sandbox::OPT_OUT).is_none_or(|v| v != "1")
        && let Err(e) = spoolframe_linux_core::fido2::broker::start(sandbox::confine_broker)
    {
        tracing::warn!(error = %e, "no key helper: keys must be plugged in before the app starts");
    }
    // Files the person picks (the identity record, P7-08), through a
    // helper that shows the portal's dialog itself: the app may not reach
    // the home directory once confined.
    if std::env::var_os(sandbox::OPT_OUT).is_none_or(|v| v != "1")
        && let Err(e) = file_helper::start(sandbox::confine_file_helper)
    {
        tracing::warn!(error = %e, "no file helper: exporting and importing will fail");
    }
    let confinement = sandbox::confine();
    if let Some(probe) = cli.self_check {
        std::process::exit(if sandbox::self_check(confinement, &probe) {
            0
        } else {
            1
        });
    }

    // clap has parsed the arguments; GTK gets only an invite link, which
    // it hands to the running app if there is one (P6-01), and none of
    // ours, or it would reject them.
    let app = RelmApp::new(APP_ID).with_args(match cli.link {
        Some(link) => vec!["spoolframe-linux".to_owned(), link],
        None => Vec::new(),
    });
    use gtk::gio::prelude::*;
    let application = relm4::main_application();
    application.set_flags(application.flags() | gtk::gio::ApplicationFlags::HANDLES_OPEN);
    application.connect_open(|application, links, _| {
        // Opening is not activating: the window is made by activate.
        application.activate();
        for link in links {
            app::opened::open(link.uri().to_string());
        }
    });
    playback::init()?;
    icons::register();
    // The app's icon, from the resource when not installed.
    gtk::Window::set_default_icon_name(APP_ID);
    fonts::register();

    // The SDK's runtime: its tasks (discovery, reconnecting) run here for
    // as long as the app does. Kept in scope until the app returns.
    let runtime = tokio::runtime::Runtime::new().context("cannot start the async runtime")?;
    let client: Arc<dyn Client> = Arc::new(open_sdk(&runtime)?);

    let artwork_cache = open_artwork_cache();
    if let Some(cache) = &artwork_cache {
        trim_later(cache.clone());
    }
    app.run::<app::App>(app::Init {
        client,
        artwork_cache,
    });
    playback::shutdown();
    drop(runtime);
    Ok(())
}

/// How the SDK opens, from the user's settings: with or without relays
/// (P6-03), which it cannot change while it runs.
fn client_options(settings: &settings::Settings) -> sdk::ClientOptions {
    sdk::ClientOptions {
        relays: settings.use_relays(),
        features: FEATURES,
        ..sdk::ClientOptions::default()
    }
}

/// The regular features this app handles, listed to every server: the
/// SDK turns on none by itself (0.11), and a session has those the
/// server lists too. Each is shown only where the session has it
/// (`Session::has_*`).
const FEATURES: sdk::Features = sdk::Features {
    playlists: true,
    liked: true,
    last_viewed: true,
    playlist_sharing: true,
    play_history: true,
};

/// The artwork cache, or none if it cannot be opened: images are then
/// fetched each time, which is slower but works.
fn open_artwork_cache() -> Option<Arc<ArtworkCache>> {
    let Some(dir) = ArtworkCache::default_dir() else {
        tracing::warn!("no cache directory (no home directory?); artwork is not cached");
        return None;
    };
    match ArtworkCache::open(&dir, Limits::default()) {
        Ok(cache) => Some(Arc::new(cache)),
        Err(e) => {
            tracing::warn!(error = %e, "artwork is not cached");
            None
        }
    }
}

/// Trims the artwork kept on disk to its cap once, a little after start
/// and on a thread of its own, so the first pages are not slowed.
fn trim_later(cache: Arc<ArtworkCache>) {
    let spawned = std::thread::Builder::new()
        .name("artwork-trim".into())
        .spawn(move || {
            std::thread::sleep(spoolframe_linux_core::artcache::TRIM_AFTER);
            match cache.trim() {
                Ok(trimmed) => tracing::debug!(?trimmed, "artwork cache trimmed"),
                Err(e) => tracing::warn!(error = %e, "artwork cache not trimmed"),
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "artwork cache not trimmed");
    }
}

/// Opens the device: its storage (the Secret Service for keys and tokens,
/// files for the rest) and the SDK on it. No Secret Service is an error
/// the user sees, never a fallback to a plain file.
fn open_sdk(runtime: &tokio::runtime::Runtime) -> Result<SdkClient> {
    let data = DataStore::default_dir()
        .context("no data directory (no home directory?), so there is nowhere to keep data")?;
    let storage = runtime.block_on(Storage::open(DataStore::new(data)))?;
    let settings = settings::Settings::load();
    let mut capabilities =
        capabilities::with_hdr(capabilities::capabilities(), settings.show_hdr());
    capabilities.max_bitrate = settings.max_bitrate();
    tracing::debug!(?capabilities, "decoders found");
    let device = sdk::Device {
        platform: sdk::types::Platform::Linux,
        name: gtk::glib::host_name().to_string(),
        client_name: "spoolframe-linux".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        capabilities,
    };
    let client = runtime
        .block_on(sdk::Client::open_with_options(
            Arc::new(SdkStorage(storage)),
            device,
            client_options(&settings),
        ))
        .context("cannot open this device's Spoolframe identity")?;
    Ok(SdkClient::new(client, runtime.handle().clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relays turned off in Preferences reach the SDK at the next start
    /// (P6-03); left alone, they are on.
    #[test]
    fn the_relay_setting_opens_the_sdk_with_or_without_relays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.ini");
        let mut settings = settings::Settings::at(&path);
        assert!(client_options(&settings).relays);
        settings.set_use_relays(false);
        assert!(!client_options(&settings::Settings::at(&path)).relays);
    }

    /// The SDK lists no feature an app does not ask for (0.11): playlists
    /// and the rest are asked for, whatever the settings.
    #[test]
    fn the_sdk_opens_with_every_feature_the_app_handles() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings::Settings::at(&dir.path().join("settings.ini"));
        assert_eq!(
            client_options(&settings).features,
            sdk::Features {
                playlists: true,
                liked: true,
                last_viewed: true,
                playlist_sharing: true,
                play_history: true,
            }
        );
    }
}
