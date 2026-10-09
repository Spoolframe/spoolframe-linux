//! The app confined (C-15): what `--self-check` finds it can and cannot
//! do, in a home directory of its own.

use std::path::Path;
use std::process::{Command, Output};

/// Runs `--self-check` with a fresh home holding a probe file, and the
/// sandbox on or turned off.
fn self_check(sandbox: bool) -> Output {
    let home = tempfile::tempdir().unwrap();
    let probe = home.path().join("probe-secret");
    std::fs::write(&probe, "not for the app").unwrap();
    let runtime = home.path().join("run");
    std::fs::create_dir(&runtime).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_spoolframe-linux"));
    command
        .arg("--self-check")
        .arg(&probe)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .env("XDG_RUNTIME_DIR", &runtime)
        .env_remove("SPOOLFRAME_NO_SANDBOX");
    if !sandbox {
        command.env("SPOOLFRAME_NO_SANDBOX", "1");
    }
    let output = command.output().expect("the binary runs");
    assert!(Path::new(&probe).exists());
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn confined_the_app_reads_none_of_the_home_and_starts_no_program() {
    let output = self_check(true);
    let said = stdout(&output);
    assert!(output.status.success(), "{output:?}");
    for line in [
        "probe: refused",
        "exec: refused",
        "data: written",
        "gstreamer: ok",
    ] {
        assert!(said.contains(line), "no {line:?} in:\n{said}");
    }
}

#[test]
fn unconfined_the_check_fails() {
    let output = self_check(false);
    let said = stdout(&output);
    assert!(!output.status.success(), "{output:?}");
    assert!(said.contains("probe: read"), "{said}");
    assert!(said.contains("exec: started"), "{said}");
}

/// Confined, GStreamer scans its plugins in the process, as it can start
/// no helper: it must not try to, and warn on every start that its
/// plugin loader failed.
#[test]
fn confined_gstreamer_does_not_try_its_plugin_scanner() {
    let output = self_check(true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{output:?}");
    assert!(
        !stderr.contains("External plugin loader failed"),
        "{stderr}"
    );
}
