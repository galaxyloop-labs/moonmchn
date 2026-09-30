//! End-to-end tests for the `spacecraft` binary.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

fn spacecraft() -> Command {
    Command::cargo_bin("spacecraft").expect("binary is built by cargo test")
}

/// The `version` declared under `[package]` in this crate's `Cargo.toml`,
/// read from disk so the expectation does not share the binary's
/// compile-time constant.
fn manifest_version() -> String {
    let manifest = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("Cargo.toml is readable");
    let mut in_package = false;

    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some(version) = line.strip_prefix("version = ") {
            return version.trim_matches('"').to_owned();
        }
    }

    panic!("no `version` under `[package]` in Cargo.toml");
}

#[test]
fn version_prints_v_prefixed_manifest_version() {
    spacecraft()
        .arg("version")
        .assert()
        .success()
        .stdout(format!("v{}\n", manifest_version()))
        .stderr("");
}

#[test]
fn missing_subcommand_prints_usage() {
    spacecraft()
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn version_flag_prints_package_version() {
    spacecraft()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(manifest_version()));
}
