mod app;
mod artwork;
mod capabilities;
mod fonts;
mod icons;
mod library;
mod logo;
mod minibar;
mod mpris;
mod nowplaying;
mod pages;
mod pin;
mod pipeline;
mod playback;
mod player;
mod preferences;
mod profile_page;
mod qr;
mod queue;
mod remote_sign_in;
mod saved_queue;
mod scrub;
mod search;
mod settings;
mod source;
mod start;
mod theme;
mod tracks;
mod transport;
mod views;
mod volume;

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

    // clap has parsed the arguments; GTK gets none, or it would reject ours.
    let app = RelmApp::new(APP_ID).with_args(Vec::new());
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
    let mut capabilities = capabilities::capabilities();
    capabilities.max_bitrate = settings::Settings::load().max_bitrate();
    tracing::debug!(?capabilities, "decoders found");
    let device = sdk::Device {
        platform: sdk::types::Platform::Linux,
        name: gtk::glib::host_name().to_string(),
        client_name: "spoolframe-linux".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        capabilities,
    };
    let client = runtime
        .block_on(sdk::Client::open(Arc::new(SdkStorage(storage)), device))
        .context("cannot open this device's Spoolframe identity")?;
    Ok(SdkClient::new(client, runtime.handle().clone()))
}
