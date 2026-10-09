//! The binary as a user runs it.

use std::process::Command;

#[test]
fn version_names_the_binary_and_its_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_spoolframe-linux"))
        .arg("--version")
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("--version prints UTF-8");
    assert_eq!(
        stdout.trim(),
        format!("spoolframe-linux {}", env!("CARGO_PKG_VERSION"))
    );
}

/// The desktop opens an invite link (P6-01) by running the app with it:
/// the entry handles the scheme and passes the link, and the binary
/// takes it.
#[test]
fn the_desktop_entry_hands_invite_links_to_the_binary() {
    let entry = include_str!("../../data/casa.coldsteel.Spoolframe.desktop");
    assert!(
        entry
            .lines()
            .any(|l| l == "MimeType=x-scheme-handler/spoolframe;")
    );
    assert!(entry.lines().any(|l| l == "Exec=spoolframe-linux %u"));
    let output = Command::new(env!("CARGO_BIN_EXE_spoolframe-linux"))
        .arg("--help")
        .output()
        .expect("the binary runs");
    let help = String::from_utf8(output.stdout).expect("--help prints UTF-8");
    assert!(help.contains("[LINK]"), "{help}");
}
