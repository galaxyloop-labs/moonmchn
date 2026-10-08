//! Create the initial user configuration.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{Config, Error, Result};

const HOME_ENV_VAR: &str = "HOME";
const CONFIG_DIRECTORY: &str = ".config/spacecraft";
const CONFIG_FILE_NAME: &str = "config.toml";

/// Creates `~/.config/spacecraft/config.toml` and any missing parent directories.
///
/// The home directory is taken from `HOME`. The initial configuration is
/// [`Config::recommended`], serialized as TOML. When `force` is true, an existing
/// file is overwritten; otherwise it is left untouched.
///
/// # Errors
///
/// Returns an error if `HOME` is unset or empty, the destination already exists
/// without `force`, host detection or serialization fails, or a directory or
/// file cannot be created or written.
pub fn setup(force: bool) -> Result<PathBuf> {
    let home = env::var_os(HOME_ENV_VAR)
        .filter(|home| !home.is_empty())
        .ok_or(Error::MissingHome)?;

    setup_in(Path::new(&home), force)
}

fn setup_in(home: &Path, force: bool) -> Result<PathBuf> {
    let directory = home.join(CONFIG_DIRECTORY);
    let path = directory.join(CONFIG_FILE_NAME);

    // Refuse existing destinations before doing host detection or filesystem writes.
    if !force {
        ensure_missing(&path)?;
    }

    // Detection and serialization must succeed before creating or truncating
    // any file, especially when --force replaces a user's configuration.
    let contents = Config::recommended()?.to_toml()?;

    fs::create_dir_all(&directory).map_err(|source| Error::Io {
        path: directory,
        source,
    })?;

    write_configuration(&path, &contents, force)?;
    Ok(path)
}

fn ensure_missing(path: &Path) -> Result<()> {
    // Inspect the entry itself, not its target, so dangling symlinks also count.
    match fs::symlink_metadata(path) {
        Ok(_) => Err(Error::ConfigAlreadyExists(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
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
                Error::Io {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })?;

    file.write_all(contents.as_bytes())
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ensure_missing, setup_in, write_configuration};
    use crate::{Config, Error};

    const CUSTOM_CONFIG: &str = "# Custom configuration\n";

    fn assert_recommended_config(path: &std::path::Path) {
        let contents = fs::read_to_string(path).unwrap();
        let config: Config = toml::from_str(&contents).unwrap();
        assert_eq!(config, Config::recommended().unwrap());
    }

    #[test]
    fn creates_configuration_and_parent_directories() {
        let home = tempfile::tempdir().unwrap();

        let path = setup_in(home.path(), false).unwrap();

        assert_eq!(path, home.path().join(".config/spacecraft/config.toml"));
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
        let path = home.path().join(".config/spacecraft/config.toml");
        fs::create_dir_all(&path).unwrap();
        let sentinel = path.join("keep");
        fs::write(&sentinel, CUSTOM_CONFIG).unwrap();

        let error = setup_in(home.path(), true).unwrap_err();

        assert!(matches!(error, Error::Io { path: failed_path, .. } if failed_path == path));
        assert_eq!(fs::read_to_string(sentinel).unwrap(), CUSTOM_CONFIG);
    }

    #[test]
    fn reports_filesystem_failure() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join(".config"), "not a directory").unwrap();

        let error = setup_in(home.path(), false).unwrap_err();

        assert!(matches!(error, Error::Io { .. }));
    }
}
