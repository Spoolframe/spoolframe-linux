//! The user's preferences, in `settings.ini` in the app's config
//! directory (`paths::config_dir`: `~/.config/spoolframe-relm4` on Linux).
//! A GLib key file rather than GSettings: GSettings needs a schema
//! installed system-wide, which a `cargo run` does not have.

use std::path::PathBuf;

use gtk::glib;
use spoolframe_linux_core::paths;
use spoolframe_linux_core::theme::Theme;

const APPEARANCE: &str = "appearance";
const THEME: &str = "theme";
const CONNECTION: &str = "connection";
const SERVER: &str = "server";
const RELAYS: &str = "relays";
const STREAMING: &str = "streaming";
const MAX_BITRATE: &str = "max-bitrate";
const SHOW_HDR: &str = "show-hdr";
const PLAYBACK: &str = "playback";
const SKIP_INTROS: &str = "skip-intros";
const DIAGNOSTICS: &str = "show-diagnostics";

pub struct Settings {
    /// `None` without a home directory: settings then last for this run.
    path: Option<PathBuf>,
    file: glib::KeyFile,
}

impl Settings {
    /// Reads the settings file. A missing file means every setting is at
    /// its default.
    pub fn load() -> Self {
        let path = paths::config_dir().map(|dir| dir.join("settings.ini"));
        let file = glib::KeyFile::new();
        match &path {
            Some(path) => {
                if let Err(e) = file.load_from_file(path, glib::KeyFileFlags::KEEP_COMMENTS)
                    && !e.matches(glib::FileError::Noent)
                {
                    tracing::warn!(path = %path.display(), error = %e, "settings file unreadable; using defaults");
                }
            }
            None => tracing::warn!("no home directory; settings last for this run only"),
        }
        Self { path, file }
    }

    /// The settings in the file at `path`, for tests.
    #[cfg(test)]
    pub fn at(path: &std::path::Path) -> Self {
        let file = glib::KeyFile::new();
        let _ = file.load_from_file(path, glib::KeyFileFlags::KEEP_COMMENTS);
        Self {
            path: Some(path.to_owned()),
            file,
        }
    }

    pub fn theme(&self) -> Theme {
        let Ok(key) = self.file.string(APPEARANCE, THEME) else {
            return Theme::default();
        };
        Theme::from_key(&key).unwrap_or_else(|| {
            tracing::warn!(path = ?self.path, theme = %key, "unknown theme in settings; using the default");
            Theme::default()
        })
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.file.set_string(APPEARANCE, THEME, theme.key());
        self.save();
    }

    /// The key of the server used last, which the app opens to.
    pub fn server(&self) -> Option<String> {
        self.file
            .string(CONNECTION, SERVER)
            .ok()
            .map(|s| s.to_string())
    }

    pub fn set_server(&mut self, key: &str) {
        self.file.set_string(CONNECTION, SERVER, key);
        self.save();
    }

    /// Whether a server is reached through the relays it told this device
    /// of, when it cannot be dialled directly (P6-03); on by default. Off,
    /// no relay is ever contacted. Read once at start-up: the SDK's
    /// endpoint has or lacks its relay transport for as long as it runs.
    pub fn use_relays(&self) -> bool {
        self.file.boolean(CONNECTION, RELAYS).unwrap_or(true)
    }

    pub fn set_use_relays(&mut self, on: bool) {
        self.file.set_boolean(CONNECTION, RELAYS, on);
        self.save();
    }

    /// The most the server may send, in bits per second (P2-06); `None`,
    /// the default, for no limit, as on a LAN. Read once at start-up: it is
    /// part of what this device tells the server it can take.
    pub fn max_bitrate(&self) -> Option<u64> {
        self.file
            .uint64(STREAMING, MAX_BITRATE)
            .ok()
            .filter(|b| *b > 0)
    }

    /// Whether HDR films come as HDR, for GTK and the compositor to show on
    /// each monitor; on by default. Off, the server tone-maps them to SDR.
    /// Read once at start-up, like the bitrate limit.
    pub fn show_hdr(&self) -> bool {
        self.file.boolean(STREAMING, SHOW_HDR).unwrap_or(true)
    }

    pub fn set_show_hdr(&mut self, on: bool) {
        self.file.set_boolean(STREAMING, SHOW_HDR, on);
        self.save();
    }

    pub fn set_max_bitrate(&mut self, bitrate: Option<u64>) {
        match bitrate {
            Some(bitrate) => self.file.set_uint64(STREAMING, MAX_BITRATE, bitrate),
            None => {
                let _ = self.file.remove_key(STREAMING, MAX_BITRATE);
            }
        }
        self.save();
    }

    /// Whether intros are skipped without asking (P2-10); off by default.
    pub fn skip_intros(&self) -> bool {
        self.file.boolean(PLAYBACK, SKIP_INTROS).unwrap_or(false)
    }

    pub fn set_skip_intros(&mut self, on: bool) {
        self.file.set_boolean(PLAYBACK, SKIP_INTROS, on);
        self.save();
    }

    /// Whether the player opens with its diagnostics showing; off by
    /// default. The player's own button still toggles them while it plays.
    pub fn show_diagnostics(&self) -> bool {
        self.file.boolean(PLAYBACK, DIAGNOSTICS).unwrap_or(false)
    }

    pub fn set_show_diagnostics(&mut self, on: bool) {
        self.file.set_boolean(PLAYBACK, DIAGNOSTICS, on);
        self.save();
    }

    /// Writes the file. A failure is logged, not fatal: the setting
    /// still applies for this run.
    fn save(&self) {
        if let Err(e) = self.write() {
            tracing::warn!(path = ?self.path, error = %e, "could not save settings");
        }
    }

    fn write(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        self.file.save_to_file(path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relays are used unless the user turned them off, and turning them
    /// off is kept for the next start (P6-03).
    #[test]
    fn relays_are_on_until_turned_off_and_stay_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.ini");
        let mut settings = Settings::at(&path);
        assert!(settings.use_relays());
        settings.set_use_relays(false);
        assert!(!Settings::at(&path).use_relays());
    }

    /// Spoolframe's own look until the user picks another: the system's
    /// plain Adwaita leaves the app's controls without their styling.
    #[test]
    fn with_no_theme_saved_the_app_opens_in_mocha() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::at(&dir.path().join("settings.ini")).theme(),
            Theme::Mocha
        );
    }
}
