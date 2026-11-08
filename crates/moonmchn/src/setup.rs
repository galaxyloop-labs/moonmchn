//! Create the initial user configuration.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::dirs;
use crate::{Config, Error, Result};

/// Creates `config.toml` in the configuration directory and returns its path.
///
/// The directory is `$MOONMCHN_HOME`, else `$XDG_CONFIG_HOME/moonmchn`, else
/// `$HOME/.config/moonmchn`; an empty variable counts as unset. A missing
/// directory is created with mode `0700`, its missing parents with default
/// permissions; an existing one keeps its permissions. The initial configuration
/// is [`Config::recommended`], serialized as TOML. When `force` is true, an
/// existing file is overwritten; otherwise it is left untouched.
///
/// # Errors
///
/// Returns an error if none of those variables is set, the chosen path is
/// relative or contains `..`, the destination already exists without `force`,
/// host detection or serialization fails, or a directory or file cannot be
/// created or written.
pub fn setup(force: bool) -> Result<PathBuf> {
    setup_in(&dirs::config_dir()?, force)
}

fn setup_in(dir: &Path, force: bool) -> Result<PathBuf> {
    let path = dir.join(dirs::CONFIG_FILE_NAME);

    // Refuse existing destinations before doing host detection or filesystem writes.
    if !force {
        ensure_missing(&path)?;
    }

    // Detection and serialization must succeed before creating or truncating
    // any file, especially when --force replaces a user's configuration.
    let contents = Config::recommended()?.to_toml()?;

    dirs::create_private_directory(dir)?;

    write_configuration(&path, &contents, force)?;
    Ok(path)
}

fn ensure_missing(path: &Path) -> Result<()> {
    // Inspect the entry itself, not its target, so dangling symlinks also count.
    match fs::symlink_metadata(path) {
        Ok(_) => Err(Error::ConfigAlreadyExists(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::io(path, source)),
    }
}

fn write_configuration(path: &Path, contents: &str, force: bool) -> Result<()> {
    // create_new refuses a destination that appeared after ensure_missing.
    // With --force, create a missing file or truncate an existing one so no
    // trailing bytes from the old configuration survive a shorter replacement.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(!force)
        .create(force)
        .truncate(force)
        .open(path)
        .map_err(|source| {
            if !force && source.kind() == io::ErrorKind::AlreadyExists {
                Error::ConfigAlreadyExists(path.to_path_buf())
            } else {
                Error::io(path, source)
            }
        })?;

    file.write_all(contents.as_bytes())
        .map_err(|source| Error::io(path, source))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::{ensure_missing, setup_in, write_configuration};
    use crate::{Config, Error};

    const CUSTOM_CONFIG: &str = "# Custom configuration\n";

    fn assert_recommended_config(path: &Path) {
        let contents = fs::read_to_string(path).unwrap();
        let config: Config = toml::from_str(&contents).unwrap();
        assert_eq!(config, Config::recommended().unwrap());
    }

    #[test]
    fn creates_configuration_and_parent_directories() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".config/moonmchn");

        let path = setup_in(&config_dir, false).unwrap();

        assert_eq!(path, config_dir.join("config.toml"));
        assert_recommended_config(&path);
    }

    #[test]
    fn refuses_existing_configuration_without_modifying_it() {
        let home = tempfile::tempdir().unwrap();
        let path = setup_in(home.path(), false).unwrap();
        fs::write(&path, CUSTOM_CONFIG).unwrap();

        let error = setup_in(home.path(), false).unwrap_err();

        assert!(matches!(error, Error::ConfigAlreadyExists(existing) if existing == path));
        assert_eq!(fs::read_to_string(path).unwrap(), CUSTOM_CONFIG);
    }

    #[test]
    fn force_replaces_entire_configuration() {
        let home = tempfile::tempdir().unwrap();
        let path = setup_in(home.path(), false).unwrap();
        // Make the old file longer than its replacement to catch missing truncation.
        let contents = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{contents}{CUSTOM_CONFIG}")).unwrap();

        assert_eq!(setup_in(home.path(), true).unwrap(), path);
        assert_eq!(fs::read_to_string(&path).unwrap(), contents);
        assert_recommended_config(&path);
    }

    #[test]
    fn force_also_creates_missing_configuration() {
        let home = tempfile::tempdir().unwrap();

        let path = setup_in(home.path(), true).unwrap();

        assert_recommended_config(&path);
    }

    #[test]
    fn refuses_a_destination_created_after_validation() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        ensure_missing(&path).unwrap();

        // Simulate another process creating the file between validation and open.
        fs::write(&path, CUSTOM_CONFIG).unwrap();
        let error = write_configuration(&path, "", false).unwrap_err();

        assert!(matches!(error, Error::ConfigAlreadyExists(existing) if existing == path));
        assert_eq!(fs::read_to_string(path).unwrap(), CUSTOM_CONFIG);
    }

    #[test]
    fn force_does_not_replace_a_directory() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".config/moonmchn");
        let path = config_dir.join("config.toml");
        fs::create_dir_all(&path).unwrap();
        let sentinel = path.join("keep");
        fs::write(&sentinel, CUSTOM_CONFIG).unwrap();

        let error = setup_in(&config_dir, true).unwrap_err();

        assert!(matches!(error, Error::Io { path: failed_path, .. } if failed_path == path));
        assert_eq!(fs::read_to_string(sentinel).unwrap(), CUSTOM_CONFIG);
    }

    #[test]
    fn reports_filesystem_failure() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".config/moonmchn");
        fs::write(home.path().join(".config"), "not a directory").unwrap();

        let error = setup_in(&config_dir, false).unwrap_err();

        assert!(matches!(error, Error::Io { .. }));
    }

    #[test]
    fn creates_the_configuration_directory_private() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".config/moonmchn");

        let parent = home.path().join(".config");
        // A sibling created with default permissions, under the same umask as the parent.
        let control = home.path().join("control");
        fs::create_dir(&control).unwrap();

        setup_in(&config_dir, false).unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&config_dir), 0o700, "{config_dir:?} must be private");
        assert_eq!(
            mode(&parent),
            mode(&control),
            "{parent:?} must keep default permissions"
        );
    }
}
