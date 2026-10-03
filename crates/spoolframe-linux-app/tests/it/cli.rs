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
