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
pub const MENU: &str = "lucide-menu-symbolic";
pub const RIGHT: &str = "lucide-chevron-right-symbolic";
pub const SERVER: &str = "lucide-server-symbolic";
pub const ERROR: &str = "lucide-circle-alert-symbolic";
pub const APPEARANCE: &str = "lucide-palette-symbolic";

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
    MENU,
    RIGHT,
    SERVER,
    ERROR,
    APPEARANCE,
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
