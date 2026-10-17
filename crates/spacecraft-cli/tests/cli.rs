//! End-to-end tests for the `spacecraft` binary.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use assert_cmd::Command;
use predicates::prelude::*;

fn spacecraft() -> Command {
    let mut command = Command::cargo_bin("spacecraft").expect("binary is built by cargo test");
    command
        .env_remove("SPACECRAFT_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME");
    command
}

/// The shared workspace version, read from disk so the expectation does not
/// share the binary's compile-time constant.
fn manifest_version() -> String {
    let manifest = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
        .expect("workspace Cargo.toml is readable");
    let manifest: toml::Value = toml::from_str(&manifest).expect("valid workspace manifest");
    manifest["workspace"]["package"]["version"]
        .as_str()
        .expect("workspace package version")
        .to_owned()
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

fn assert_initial_config(path: &std::path::Path) {
    let contents = fs::read_to_string(path).unwrap();
    let config: spacecraft::Config = toml::from_str(&contents).unwrap();
    assert_eq!(config, spacecraft::Config::recommended().unwrap());
    assert!((1..=8).contains(&config.resources.cpus.unwrap()));
    assert!((512..=8192).contains(&config.resources.memory.unwrap()));
    if config.resources.gpu == Some(true) {
        let vram_mib = config
            .resources
            .gpu_vram
            .expect("graphics memory recommendation");
        assert!((1..=4096).contains(&vram_mib));
        assert!(contents.contains("gpu_vram = "));
    } else {
        assert!(config.resources.gpu_vram.is_none());
        assert!(!contents.contains("gpu_vram"));
    }
    assert!(!contents.contains("# Old configuration"));
}

#[test]
fn setup_honors_xdg_config_home() {
    let home = tempfile::tempdir().unwrap();
    let xdg = tempfile::tempdir().unwrap();
    let path = xdg.path().join("spacecraft/config.toml");

    spacecraft()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg.path())
        .arg("setup")
        .assert()
        .success()
        .stdout(format!("Created {}\n", path.display()))
        .stderr("");

    assert_initial_config(&path);
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn setup_refuses_existing_file_and_suggests_force() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".config/spacecraft");
    let path = directory.join("config.toml");
    fs::create_dir_all(directory).unwrap();
    fs::write(&path, "# Keep this configuration\n").unwrap();

    spacecraft()
        .env("HOME", home.path())
        .arg("setup")
        .assert()
        .failure()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains(path.to_str().unwrap()))
        .stderr(predicate::str::contains("already exists"))
        .stderr(predicate::str::contains("--force"));

    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "# Keep this configuration\n"
    );
}

#[test]
fn setup_force_overwrites_existing_file() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".config/spacecraft");
    let path = directory.join("config.toml");
    fs::create_dir_all(directory).unwrap();
    fs::write(&path, "# Old configuration\n".repeat(100)).unwrap();

    spacecraft()
        .env("HOME", home.path())
        .args(["setup", "--force"])
        .assert()
        .success()
        .stderr("");

    assert_initial_config(&path);
}

#[test]
fn setup_force_creates_missing_file() {
    let home = tempfile::tempdir().unwrap();

    spacecraft()
        .env("HOME", home.path())
        .args(["setup", "--force"])
        .assert()
        .success()
        .stderr("");

    assert_initial_config(&home.path().join(".config/spacecraft/config.toml"));
}

#[test]
fn setup_fails_when_home_is_unset_or_empty() {
    let working_directory = tempfile::tempdir().unwrap();

    for home in [None, Some("")] {
        let mut command = spacecraft();
        command.current_dir(working_directory.path());
        if let Some(home) = home {
            command.env("HOME", home);
        } else {
            command.env_remove("HOME");
        }

        command
            .arg("setup")
            .assert()
            .failure()
            .code(1)
            .stdout("")
            .stderr(predicate::str::contains("HOME is unset or empty"));
    }

    assert!(!working_directory.path().join(".config").exists());
}

#[test]
fn setup_reports_parent_directory_failure() {
    let home = tempfile::tempdir().unwrap();
    let config_directory = home.path().join(".config");
    fs::write(&config_directory, "not a directory").unwrap();

    spacecraft()
        .env("HOME", home.path())
        .arg("setup")
        .assert()
        .failure()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("failed to access"));

    assert_eq!(
        fs::read_to_string(config_directory).unwrap(),
        "not a directory"
    );
}

#[test]
fn setup_does_not_replace_a_directory_even_with_force() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".config/spacecraft/config.toml");
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("keep"), "keep").unwrap();

    spacecraft()
        .env("HOME", home.path())
        .args(["setup", "--force"])
        .assert()
        .failure()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("failed to access"));

    assert_eq!(fs::read_to_string(path.join("keep")).unwrap(), "keep");
}

#[cfg(unix)]
#[test]
fn setup_refuses_existing_symlinks_including_dangling_ones() {
    use std::os::unix::fs::symlink;

    for target_exists in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".config/spacecraft");
        let path = directory.join("config.toml");
        let target = home.path().join("target.toml");
        fs::create_dir_all(directory).unwrap();
        if target_exists {
            fs::write(&target, "# Keep this\n").unwrap();
        }
        symlink(&target, &path).unwrap();

        spacecraft()
            .env("HOME", home.path())
            .arg("setup")
            .assert()
            .failure()
            .code(1)
            .stdout("")
            .stderr(predicate::str::contains("already exists"));

        assert_eq!(fs::read_link(path).unwrap(), target);
        if target_exists {
            assert_eq!(fs::read_to_string(target).unwrap(), "# Keep this\n");
        } else {
            assert!(!target.exists());
        }
    }
}

#[test]
fn setup_help_documents_force_without_creating_configuration() {
    let home = tempfile::tempdir().unwrap();

    spacecraft()
        .env("HOME", home.path())
        .args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--force"));

    assert!(!home.path().join(".config").exists());
}

#[test]
fn setup_honors_spacecraft_home() {
    let home = tempfile::tempdir().unwrap();
    let xdg = tempfile::tempdir().unwrap();
    let outer = tempfile::tempdir().unwrap();
    // Missing on purpose: a tempdir is already 0700, which would hide a broken mode.
    let spacecraft_home = outer.path().join("spacecraft");
    let path = spacecraft_home.join("config.toml");

    spacecraft()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("SPACECRAFT_HOME", &spacecraft_home)
        .arg("setup")
        .assert()
        .success()
        .stdout(format!("Created {}\n", path.display()))
        .stderr("");

    assert_initial_config(&path);
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(xdg.path()).unwrap().count(), 0);
    let mode = fs::metadata(&spacecraft_home).unwrap().permissions().mode();
    assert_eq!(mode & 0o7777, 0o700);
}
