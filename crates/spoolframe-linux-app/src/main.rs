mod admin_invites;
mod app;
mod artwork;
mod browser;
mod capabilities;
mod closing;
mod confirm;
mod create_identity;
mod denylist;
mod dialog;
mod download_dialog;
mod downloads;
mod external_subtitle;
mod federation;
mod federation_view;
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
mod library_browse;
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
mod saved_choices;
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
mod subtitle_search;
mod subtitle_timing;
mod theme;
mod tracks;
mod transport;
#[cfg(test)]
mod ui_test;
mod views;
mod visualizer;
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
const APP_ID: &str = "app.spoolframe.relm4";

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
    // A second launch activates the one running: a window hidden while
    // downloads finish (PX-38) comes back, in front. Relm4 makes the
    // window once, at startup, and on activating only makes it visible.
    application.connect_activate(|application| {
        use gtk::prelude::{GtkApplicationExt, GtkWindowExt};
        if let Some(window) = application.active_window() {
            window.present();
        }
    });
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
    let sdk = Arc::new(open_sdk(&runtime)?);
    let client: Arc<dyn Client> = sdk.clone();

    let artwork_cache = open_artwork_cache(&runtime);
    app.run::<app::App>(app::Init {
        client,
        artwork_cache: artwork_cache.clone(),
    });
    playback::shutdown();
    // The downloads stop where they got to, for the next start (PX-38),
    // and the connections close.
    if runtime
        .block_on(tokio::time::timeout(CLOSE_WAIT, sdk.close()))
        .is_err()
    {
        tracing::warn!("the client did not close in time");
    }
    if let Some(cache) = artwork_cache {
        runtime.block_on(cache.close());
    }
    drop(runtime);
    Ok(())
}

/// How long quitting waits for the client to close.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How the SDK opens, from the user's settings: with or without relays
/// (P6-03), which it cannot change while it runs, and with its downloads
/// (PX-38) in a directory under the app's data, the one place the sandbox
/// lets it write to. It asks for federation (P8), experimental: a paired
/// server's libraries, Home row, search hits and playback. A server with
/// federation off does not turn it on, and the app is then as it was
/// without it (each session says, `Session::federation`). HTTP playback is
/// not asked for: this app plays over Iroh only.
fn client_options(settings: &settings::Settings) -> sdk::ClientOptions {
    sdk::ClientOptions {
        relays: settings.use_relays(),
        downloads_dir: downloads_dir(),
        experimental: sdk::Experimental {
            federation: true,
            ..sdk::Experimental::default()
        },
        ..sdk::ClientOptions::default()
    }
}

/// Where the downloads are kept (PX-38): under the app's data, where the
/// sandbox lets it write. Settings shows it.
pub fn downloads_dir() -> Option<std::path::PathBuf> {
    spoolframe_linux_core::paths::data_dir().map(|dir| dir.join("downloads"))
}

/// The artwork cache, or none if it cannot be opened: images are then
/// fetched each time, which is slower but works.
fn open_artwork_cache(runtime: &tokio::runtime::Runtime) -> Option<Arc<ArtworkCache>> {
    let Some(dir) = ArtworkCache::default_dir() else {
        tracing::warn!("no cache directory (no home directory?); artwork is not cached");
        return None;
    };
    match runtime.block_on(ArtworkCache::open(&dir, Limits::default())) {
        Ok(cache) => Some(Arc::new(cache)),
        Err(e) => {
            tracing::warn!(error = %e, "artwork is not cached");
            None
        }
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
    let capabilities = capabilities::with_hdr(capabilities::capabilities(), settings.show_hdr());
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

    /// Downloads (PX-38) are kept in the data directory, where the sandbox
    /// allows writing.
    #[test]
    fn the_sdk_keeps_downloads_under_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings::Settings::at(&dir.path().join("settings.ini"));
        let data = spoolframe_linux_core::paths::data_dir().expect("a home directory");
        assert_eq!(
            client_options(&settings).downloads_dir,
            Some(data.join("downloads"))
        );
    }

    /// Every regular feature is base protocol since SDK 0.16; of the two
    /// experimental ones, federation is asked for, HTTP playback is not.
    #[test]
    fn the_sdk_opens_asking_for_federation_and_not_http_playback() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings::Settings::at(&dir.path().join("settings.ini"));
        assert_eq!(
            client_options(&settings).experimental,
            sdk::Experimental {
                http_playback: false,
                federation: true,
            }
        );
    }
}
