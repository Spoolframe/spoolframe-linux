//! Where the app keeps things, by each platform's own rules (the
//! `directories` crate): XDG on Linux, `~/Library` on macOS, Known Folders
//! on Windows. On Linux, for example:
//!
//! - config: `$XDG_CONFIG_HOME/spoolframe-relm4` (`~/.config/…`)
//! - data: `$XDG_DATA_HOME/spoolframe-relm4` (`~/.local/share/…`)
//! - cache: `$XDG_CACHE_HOME/spoolframe-relm4` (`~/.cache/…`)
//! - runtime: `$XDG_RUNTIME_DIR/spoolframe-relm4`
//!
//! and on macOS `~/Library/Application Support/casa.coldsteel.spoolframe-relm4`
//! for config and data, and `~/Library/Caches/casa.coldsteel.spoolframe-relm4`.
//! The native macOS app is another app, with its own places.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// The app's name in its directories.
pub const APP: &str = "spoolframe-relm4";

fn project(app: &str) -> Option<ProjectDirs> {
    ProjectDirs::from("casa", "coldsteel", app)
}

/// The settings' directory; `None` without a home directory.
pub fn config_dir() -> Option<PathBuf> {
    project(APP).map(|p| p.config_dir().to_path_buf())
}

/// The device's data: its saved servers, sessions' files, saved queues.
pub fn data_dir() -> Option<PathBuf> {
    project(APP).map(|p| p.data_dir().to_path_buf())
}

/// What can be fetched again: artwork, MPRIS covers.
pub fn cache_dir() -> Option<PathBuf> {
    project(APP).map(|p| p.cache_dir().to_path_buf())
}

/// Files that live only while the app runs (an external subtitle handed
/// to GStreamer): the runtime directory where the platform has one, else
/// the temporary directory.
pub fn runtime_dir() -> PathBuf {
    project(APP)
        .and_then(|p| p.runtime_dir().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directories_carry_the_apps_name() {
        let Some(data) = data_dir() else { return };
        assert!(data.to_string_lossy().contains(APP), "{}", data.display());
    }
}
