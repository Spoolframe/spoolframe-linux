//! The design's faces, carried in the binary rather than hoped to be
//! installed: Figtree for text, Outfit for headings, JetBrains Mono for
//! codes and times, Bricolage Grotesque for the wordmark (`fonts/`, each
//! under the OFL beside it). style.css names them.

use gtk::prelude::*;
use spoolframe_linux_core::paths;

const FONTS: &[(&str, &[u8])] = &[
    ("Figtree.ttf", include_bytes!("../fonts/Figtree.ttf")),
    ("Outfit.ttf", include_bytes!("../fonts/Outfit.ttf")),
    (
        "JetBrainsMono.ttf",
        include_bytes!("../fonts/JetBrainsMono.ttf"),
    ),
    (
        "BricolageGrotesque.ttf",
        include_bytes!("../fonts/BricolageGrotesque.ttf"),
    ),
];

/// Makes the faces known to Pango. It takes a font only by path, so each
/// is written to the cache directory first. Call once, after GTK is up and
/// before the first window; without it the system's UI face stands in.
pub fn register() {
    let Some(dir) = paths::cache_dir().map(|dir| dir.join("fonts")) else {
        return tracing::warn!("no cache directory; the system's UI face stands in");
    };
    let Some(map) = gtk::Label::new(None).pango_context().font_map() else {
        return;
    };
    for (name, bytes) in FONTS {
        let path = dir.join(name);
        let written = std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64);
        if !written
            && let Err(e) =
                std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, bytes))
        {
            tracing::warn!(error = %e, path = %path.display(), "cannot write a font");
            continue;
        }
        if let Err(e) = map.add_font_file(&path) {
            tracing::warn!(error = %e, path = %path.display(), "cannot load a font");
        }
    }
}
