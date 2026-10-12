//! The app's icons: Lucide (https://lucide.dev, ISC), the set the user
//! chose for every Spoolframe client. They are compiled into the binary
//! (see `build.rs`) and named here once; widgets use these names, and the
//! theme recolours them like any symbolic icon. Libadwaita's own widgets
//! (window controls, dropdown arrows) keep Adwaita's.

use gtk::{gdk, gio};

/// Registers the compiled icons with the icon theme. Call once, after GTK
/// is up.
pub fn register() {
    gio::resources_register_include!("icons.gresource").expect("the icons are compiled in");
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path(RESOURCE_PATH);
    }
}

/// Where `build.rs` puts the icons, above `scalable/actions`.
const RESOURCE_PATH: &str = "/casa/coldsteel/Spoolframe/icons";

// Player and the bar.
pub const PLAY: &str = "lucide-play-symbolic";
pub const PAUSE: &str = "lucide-pause-symbolic";
pub const BACK_10: &str = "lucide-rotate-ccw-symbolic";
pub const FORWARD_30: &str = "lucide-rotate-cw-symbolic";
pub const TRACKS: &str = "lucide-captions-symbolic";
pub const DIAGNOSTICS: &str = "lucide-activity-symbolic";
pub const FULLSCREEN: &str = "lucide-maximize-symbolic";
pub const LEAVE_FULLSCREEN: &str = "lucide-minimize-symbolic";
pub const VOLUME: &str = "lucide-volume-2-symbolic";
pub const MUTED: &str = "lucide-volume-x-symbolic";
pub const DOWN: &str = "lucide-chevron-down-symbolic";
pub const UP: &str = "lucide-chevron-up-symbolic";
pub const BACK: &str = "lucide-chevron-left-symbolic";
pub const CLOSE: &str = "lucide-x-symbolic";
// Music.
pub const PREVIOUS: &str = "lucide-skip-back-symbolic";
pub const NEXT: &str = "lucide-skip-forward-symbolic";
pub const SHUFFLE: &str = "lucide-shuffle-symbolic";
pub const REPEAT: &str = "lucide-repeat-symbolic";
pub const REPEAT_ONE: &str = "lucide-repeat-1-symbolic";
// The sidebar.
pub const HOME: &str = "lucide-house-symbolic";
pub const SEARCH: &str = "lucide-search-symbolic";
pub const MOVIES: &str = "lucide-film-symbolic";
pub const SHOWS: &str = "lucide-tv-symbolic";
pub const MUSIC: &str = "lucide-music-symbolic";
// Pages and dialogs.
pub const WATCHED: &str = "lucide-eye-symbolic";
pub const CHECK: &str = "lucide-check-symbolic";
pub const ADD: &str = "lucide-plus-symbolic";
pub const RIGHT: &str = "lucide-chevron-right-symbolic";
pub const SERVER: &str = "lucide-server-symbolic";
pub const ERROR: &str = "lucide-circle-alert-symbolic";
pub const APPEARANCE: &str = "lucide-palette-symbolic";
pub const VERIFIED: &str = "lucide-shield-check-symbolic";
pub const UNVERIFIED: &str = "lucide-shield-alert-symbolic";
pub const REMOVE: &str = "lucide-circle-x-symbolic";
// The play queue (P1-23).
pub const PLAY_NEXT: &str = "lucide-list-start-symbolic";
pub const ADD_TO_QUEUE: &str = "lucide-list-plus-symbolic";
pub const MORE: &str = "lucide-ellipsis-symbolic";
pub const DRAG: &str = "lucide-grip-vertical-symbolic";
// The profile PIN (P2-08).
pub const LOCK: &str = "lucide-lock-symbolic";
pub const DELETE: &str = "lucide-delete-symbolic";
// Signing in and setting up.
pub const BROWSER: &str = "lucide-globe-symbolic";
pub const PHONE: &str = "lucide-smartphone-symbolic";
pub const PASSKEY: &str = "lucide-key-round-symbolic";
pub const SECURITY_KEY: &str = "lucide-usb-symbolic";
pub const EXPIRED: &str = "lucide-clock-symbolic";
pub const WARNING: &str = "lucide-triangle-alert-symbolic";
pub const SET_UP: &str = "lucide-settings-symbolic";
// Preferences' pages.
pub const PLAYBACK_PAGE: &str = "lucide-monitor-play-symbolic";
pub const PROFILE: &str = "lucide-user-round-symbolic";
pub const SIGN_OUT: &str = "lucide-log-out-symbolic";
pub const FORGET: &str = "lucide-trash-2-symbolic";
pub const LINK: &str = "lucide-link-symbolic";
pub const PASTE: &str = "lucide-clipboard-symbolic";
pub const JOIN: &str = "lucide-user-plus-symbolic";
pub const INVITES: &str = "lucide-ticket-symbolic";
pub const IDENTITY: &str = "lucide-fingerprint-symbolic";
pub const COPY: &str = "lucide-copy-symbolic";
pub const EXTERNAL: &str = "lucide-external-link-symbolic";
pub const LAPTOP: &str = "lucide-laptop-symbolic";
pub const MONITOR: &str = "lucide-monitor-symbolic";
pub const SEASON: &str = "lucide-layers-2-symbolic";
/// The main menu, on Choose a server.
pub const MENU: &str = "lucide-menu-symbolic";
// Settings' This computer pages.
pub const CONNECTION: &str = "lucide-globe-symbolic";
pub const DOWNLOADS: &str = "lucide-download-symbolic";
pub const SHARED: &str = "lucide-users-symbolic";
pub const ABOUT: &str = "lucide-info-symbolic";
// Search.
pub const HISTORY: &str = "lucide-history-symbolic";
pub const CLEAR_RECENT: &str = "lucide-list-x-symbolic";
pub const CALENDAR: &str = "lucide-calendar-symbolic";
// Going to a track's artist and album.
pub const ARTIST: &str = "lucide-mic-vocal-symbolic";
pub const ALBUM: &str = "lucide-disc-3-symbolic";

/// Every name above, for the tests.
#[cfg(test)]
const ALL: &[&str] = &[
    PLAY,
    PAUSE,
    BACK_10,
    FORWARD_30,
    TRACKS,
    DIAGNOSTICS,
    FULLSCREEN,
    LEAVE_FULLSCREEN,
    VOLUME,
    MUTED,
    DOWN,
    UP,
    BACK,
    CLOSE,
    PREVIOUS,
    NEXT,
    SHUFFLE,
    REPEAT,
    REPEAT_ONE,
    HOME,
    SEARCH,
    MOVIES,
    SHOWS,
    MUSIC,
    WATCHED,
    CHECK,
    ADD,
    RIGHT,
    SERVER,
    ERROR,
    APPEARANCE,
    VERIFIED,
    UNVERIFIED,
    REMOVE,
    PLAY_NEXT,
    ADD_TO_QUEUE,
    MORE,
    DRAG,
    LOCK,
    DELETE,
    BROWSER,
    PHONE,
    PASSKEY,
    SECURITY_KEY,
    EXPIRED,
    WARNING,
    SET_UP,
    PLAYBACK_PAGE,
    PROFILE,
    SIGN_OUT,
    FORGET,
    LINK,
    EXTERNAL,
    LAPTOP,
    MONITOR,
    SEASON,
    MENU,
    CONNECTION,
    DOWNLOADS,
    SHARED,
    ABOUT,
    HISTORY,
    CLEAR_RECENT,
    CALENDAR,
    ARTIST,
    ALBUM,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(name: &str) -> String {
        format!("{RESOURCE_PATH}/scalable/actions/{name}.svg")
    }

    #[test]
    fn every_icon_named_is_compiled_in() {
        gio::resources_register_include!("icons.gresource").unwrap();
        let missing: Vec<_> = ALL
            .iter()
            .filter(|name| {
                gio::resources_lookup_data(&resource(name), gio::ResourceLookupFlags::NONE).is_err()
            })
            .collect();
        assert!(missing.is_empty(), "not in the resource: {missing:?}");
    }

    /// The app's own icon and its symbolic form, so a window shows it
    /// without an install.
    #[test]
    fn the_app_icon_is_compiled_in() {
        gio::resources_register_include!("icons.gresource").unwrap();
        let missing: Vec<_> = [
            "casa.coldsteel.Spoolframe.svg",
            "casa.coldsteel.Spoolframe-symbolic.svg",
        ]
        .into_iter()
        .filter(|name| {
            let path = format!("{RESOURCE_PATH}/scalable/apps/{name}");
            gio::resources_lookup_data(&path, gio::ResourceLookupFlags::NONE).is_err()
        })
        .collect();
        assert!(missing.is_empty(), "not in the resource: {missing:?}");
    }

    /// GTK recolours a symbolic icon through its fills; a stroke left in
    /// would stay black. `icons/convert.sh` outlines every stroke.
    #[test]
    fn every_icon_is_drawn_with_fills_only() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("icons");
        let stroked: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".svg"))
            .filter(|e| {
                let svg = std::fs::read_to_string(e.path()).unwrap();
                svg.contains("stroke=")
                    || svg
                        .match_indices("stroke:")
                        .any(|(i, _)| !svg[i..].starts_with("stroke:none"))
            })
            .map(|e| e.file_name())
            .collect();
        assert!(stroked.is_empty(), "icons with strokes: {stroked:?}");
    }

    /// Draws every icon through GTK's icon theme, the way the app shows
    /// them, into the PNG named by SPOOLFRAME_ICONS_PNG; run with
    /// `--ignored` (needs a display).
    #[test]
    #[ignore = "needs a display; writes a picture to look at"]
    fn the_icons_can_be_looked_at() {
        use gtk::prelude::*;
        let path = std::env::var("SPOOLFRAME_ICONS_PNG").expect("set SPOOLFRAME_ICONS_PNG");
        gtk::init().unwrap();
        register();
        let theme = gtk::IconTheme::for_display(&gdk::Display::default().unwrap());
        let (size, per_row) = (48.0_f32, 8);
        let snapshot = gtk::Snapshot::new();
        snapshot.append_color(
            &gdk::RGBA::new(0.118, 0.118, 0.18, 1.0),
            &gtk::graphene::Rect::new(0.0, 0.0, size * per_row as f32, size * 5.0),
        );
        for (i, name) in ALL.iter().enumerate() {
            let icon = theme.lookup_icon(
                name,
                &[],
                40,
                1,
                gtk::TextDirection::Ltr,
                gtk::IconLookupFlags::empty(),
            );
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(
                (i % per_row) as f32 * size + 4.0,
                (i / per_row) as f32 * size + 4.0,
            ));
            icon.snapshot_symbolic(
                &snapshot,
                40.0,
                40.0,
                &[gdk::RGBA::new(0.804, 0.839, 0.957, 1.0)],
            );
            snapshot.restore();
        }
        let node = snapshot.to_node().unwrap();
        let renderer = gtk::gsk::CairoRenderer::new();
        renderer.realize(None::<&gdk::Surface>).unwrap();
        let texture = renderer.render_texture(&node, None);
        renderer.unrealize();
        texture.save_to_png(path).unwrap();
    }
}
