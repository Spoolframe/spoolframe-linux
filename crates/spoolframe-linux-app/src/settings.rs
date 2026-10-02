//! The user's preferences, in `$XDG_CONFIG_HOME/spoolframe-linux/settings.ini`.
//! A GLib key file rather than GSettings: GSettings needs a schema
//! installed system-wide, which a `cargo run` does not have.

use std::path::PathBuf;

use gtk::glib;
use spoolframe_linux_core::theme::Theme;

const APPEARANCE: &str = "appearance";
const THEME: &str = "theme";
const CONNECTION: &str = "connection";
const SERVER: &str = "server";

pub struct Settings {
    path: PathBuf,
    file: glib::KeyFile,
}

impl Settings {
    /// Reads the settings file. A missing file means every setting is at
    /// its default.
    pub fn load() -> Self {
        let path = glib::user_config_dir()
            .join("spoolframe-linux")
            .join("settings.ini");
        let file = glib::KeyFile::new();
        if let Err(e) = file.load_from_file(&path, glib::KeyFileFlags::KEEP_COMMENTS)
            && !e.matches(glib::FileError::Noent)
        {
            tracing::warn!(path = %path.display(), error = %e, "settings file unreadable; using defaults");
        }
        Self { path, file }
    }

    pub fn theme(&self) -> Theme {
        let Ok(key) = self.file.string(APPEARANCE, THEME) else {
            return Theme::default();
        };
        Theme::from_key(&key).unwrap_or_else(|| {
            tracing::warn!(path = %self.path.display(), theme = %key, "unknown theme in settings; following the system");
            Theme::default()
        })
    }

    /// Saves the theme. A failure is logged, not fatal: the theme still
    /// applies for this run.
    pub fn set_theme(&mut self, theme: Theme) {
        self.file.set_string(APPEARANCE, THEME, theme.key());
        if let Err(e) = self.save() {
            tracing::warn!(path = %self.path.display(), error = %e, "could not save settings");
        }
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
        if let Err(e) = self.save() {
            tracing::warn!(path = %self.path.display(), error = %e, "could not save settings");
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        self.file.save_to_file(&self.path)?;
        Ok(())
    }
}
