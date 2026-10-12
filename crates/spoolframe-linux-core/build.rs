//! Links libfido2 (P7-08), found through pkg-config.

fn main() {
    pkg_config::Config::new()
        .atleast_version("1.13")
        .probe("libfido2")
        .expect("libfido2 1.13 or later, found through pkg-config");
}
