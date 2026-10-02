//! Compiles the icons in `icons/` into a GResource the binary carries, so
//! the app needs no icon files installed beside it. Every
//! `lucide-*-symbolic.svg` there is listed; adding one is dropping it in.

use std::fmt::Write as _;
use std::path::Path;

/// Where the icons live in the resource; `main.rs` hands it to the icon
/// theme.
const PREFIX: &str = "/casa/coldsteel/Spoolframe/icons/scalable/actions";

fn main() {
    let icons = Path::new("icons");
    println!("cargo:rerun-if-changed=icons");
    let mut names: Vec<String> = std::fs::read_dir(icons)
        .expect("the icons directory")
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.ends_with("-symbolic.svg"))
        .collect();
    names.sort();

    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"{PREFIX}\">\n"
    );
    for name in &names {
        writeln!(
            xml,
            "    <file preprocess=\"xml-stripblanks\">{name}</file>"
        )
        .unwrap();
    }
    xml.push_str("  </gresource>\n</gresources>\n");
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let manifest = Path::new(&out).join("icons.gresource.xml");
    std::fs::write(&manifest, xml).expect("write the resource manifest");

    glib_build_tools::compile_resources(&[icons], manifest.to_str().unwrap(), "icons.gresource");
}
