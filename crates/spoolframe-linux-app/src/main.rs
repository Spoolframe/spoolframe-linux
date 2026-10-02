mod app;
mod artwork;
mod capabilities;
mod icons;
mod library;
mod logo;
mod minibar;
mod nowplaying;
mod pages;
mod playback;
mod player;
mod preferences;
mod settings;
mod source;
mod start;
mod theme;
mod tracks;
mod views;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use relm4::RelmApp;
use spoolframe_client_sdk as sdk;
use spoolframe_linux_core::artcache::{ArtworkCache, Limits};
use spoolframe_linux_core::backend::Client;
use spoolframe_linux_core::fake::{FAKE_PASSWORD, FakeClient};
use spoolframe_linux_core::sdk::{SdkClient, SdkStorage};
use spoolframe_linux_core::storage::{DataStore, Storage};
use tracing_subscriber::EnvFilter;

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

    /// Use a built-in fake server instead of real ones: the mockups'
    /// content, for working on the UI without a server.
    #[arg(long)]
    fake: bool,

    /// The file the fake server plays for every movie, episode and track.
    /// Without it, playback says the fake has no media.
    #[arg(long, value_name = "FILE", requires = "fake")]
    fake_media: Option<std::path::PathBuf>,
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

    // The SDK's runtime: its tasks (discovery, reconnecting) run here for
    // as long as the app does. Kept in scope until the app returns.
    let runtime = tokio::runtime::Runtime::new().context("cannot start the async runtime")?;
    let (client, sign_in_hint): (Arc<dyn Client>, _) = if cli.fake {
        let fake_media = match cli.fake_media {
            Some(path) => Some(
                path.canonicalize()
                    .with_context(|| format!("--fake-media {}", path.display()))?,
            ),
            None => None,
        };
        (
            Arc::new(FakeClient::new(fake_media)),
            Some(format!(
                "Fake accounts: andrey, alice, steve · password {FAKE_PASSWORD}"
            )),
        )
    } else {
        (Arc::new(open_sdk(&runtime)?), None)
    };

    let artwork_cache = open_artwork_cache(&runtime);
    app.run::<app::App>(app::Init {
        client,
        sign_in_hint,
        artwork_cache: artwork_cache.clone(),
    });
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
        tracing::warn!("neither XDG_CACHE_HOME nor HOME is set; artwork is not cached");
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
        .context("neither XDG_DATA_HOME nor HOME is set, so there is nowhere to keep data")?;
    let storage = runtime.block_on(Storage::open(DataStore::new(data)))?;
    let capabilities = capabilities::capabilities();
    tracing::debug!(?capabilities, "decoders found");
    let device = sdk::Device {
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
