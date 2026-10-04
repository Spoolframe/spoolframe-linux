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
const STREAMING: &str = "streaming";
const MAX_BITRATE: &str = "max-bitrate";
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

    pub fn theme(&self) -> Theme {
        let Ok(key) = self.file.string(APPEARANCE, THEME) else {
            return Theme::default();
        };
        Theme::from_key(&key).unwrap_or_else(|| {
            tracing::warn!(path = ?self.path, theme = %key, "unknown theme in settings; following the system");
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

    /// The most the server may send, in bits per second (P2-06); `None`,
    /// the default, for no limit, as on a LAN. Read once at start-up: it is
    /// part of what this device tells the server it can take.
    pub fn max_bitrate(&self) -> Option<u64> {
        self.file
            .uint64(STREAMING, MAX_BITRATE)
            .ok()
            .filter(|b| *b > 0)
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
