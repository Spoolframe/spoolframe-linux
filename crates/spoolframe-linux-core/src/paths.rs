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
/// a private directory made for this run. None if that cannot be made.
pub fn runtime_dir() -> Option<PathBuf> {
    runtime_dir_or_private(project(APP).and_then(|p| p.runtime_dir().map(Path::to_path_buf)))
}

/// `runtime`, or a directory of this run's own under the temporary one:
/// randomly named and open to this user only, so no one else can guess a
/// file's name there or put a link in its place (PX-11, C-14). Made once,
/// and kept until the app ends.
fn runtime_dir_or_private(runtime: Option<PathBuf>) -> Option<PathBuf> {
    static PRIVATE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    runtime.or_else(|| {
        PRIVATE
            .get_or_init(|| {
                use std::os::unix::fs::PermissionsExt;
                // tempfile leaves the mode to the umask: 0755 by default.
                tempfile::Builder::new()
                    .prefix(&format!("{APP}-"))
                    .permissions(std::fs::Permissions::from_mode(0o700))
                    .tempdir()
                    .inspect_err(|e| tracing::warn!(error = %e, "no private temporary directory"))
                    .ok()
                    .map(tempfile::TempDir::keep)
            })
            .clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_runtime_directory_files_go_in_a_private_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = runtime_dir_or_private(None).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert!(
            dir != std::env::temp_dir() && mode == 0o700,
            "{} is shared, or open to others ({mode:o})",
            dir.display()
        );
    }

    #[test]
    fn the_private_directory_is_made_once_a_run() {
        assert_eq!(runtime_dir_or_private(None), runtime_dir_or_private(None));
    }

    #[test]
    fn the_platforms_runtime_directory_is_used_as_it_is() {
        let given = PathBuf::from("/run/user/1000/spoolframe-relm4");
        assert_eq!(runtime_dir_or_private(Some(given.clone())), Some(given));
    }

    #[test]
    fn the_directories_carry_the_apps_name() {
        let Some(data) = data_dir() else { return };
        assert!(data.to_string_lossy().contains(APP), "{}", data.display());
    }
}
