mod admin_invites;
mod app;
mod artwork;
mod browser;
mod capabilities;
mod confirm;
mod denylist;
mod fonts;
mod icons;
mod invite;
mod library;
mod logo;
// Text shown as it is where GTK reads markup, and the checks over the
// sources that keep it so.
mod markup;
mod minibar;
mod mpris;
mod nowplaying;
mod pages;
mod passkey_dialog;
mod passkeys;
mod pin;
mod pipeline;
mod playback;
mod player;
mod preferences;
mod profile_page;
mod qr;
mod quality;
mod queue;
mod reachability;
mod remote_sign_in;
mod sandbox;
mod saved_queue;
mod scrub;
mod search;
mod server_relays;
mod settings;
mod source;
mod srt_markup;
mod start;
#[cfg(test)]
mod stylesheet;
mod theme;
mod tracks;
mod transport;
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

    let artwork_cache = open_artwork_cache(&runtime);
    app.run::<app::App>(app::Init {
        client,
        artwork_cache: artwork_cache.clone(),
    });
    playback::shutdown();
    if let Some(cache) = artwork_cache {
        runtime.block_on(cache.close());
    }
    drop(runtime);
    Ok(())
}

/// How the SDK opens, from the user's settings: with or without relays
/// (P6-03), which it cannot change while it runs.
fn client_options(settings: &settings::Settings) -> sdk::ClientOptions {
    sdk::ClientOptions {
        relays: settings.use_relays(),
        ..sdk::ClientOptions::default()
    }
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
}
