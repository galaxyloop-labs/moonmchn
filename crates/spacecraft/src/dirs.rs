//! Configuration and state directories resolved from the environment.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

/// Overrides both the configuration and the state directory.
pub(crate) const SPACECRAFT_HOME_ENV: &str = "SPACECRAFT_HOME";
/// The XDG base directory for configuration files.
pub(crate) const XDG_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";
/// The XDG base directory for state files.
pub(crate) const XDG_STATE_HOME_ENV: &str = "XDG_STATE_HOME";
/// The user's home directory, the last fallback.
pub(crate) const HOME_ENV: &str = "HOME";
/// The directory created under an XDG or home base.
pub(crate) const APP_DIRECTORY: &str = "spacecraft";
/// The configuration base under `$HOME`, used when no other variable is set.
const HOME_CONFIG_FALLBACK: &str = ".config";
/// The state base under `$HOME`, used when no other variable is set.
const HOME_STATE_FALLBACK: &str = ".local/state";
/// The configuration file inside the configuration directory.
pub(crate) const CONFIG_FILE_NAME: &str = "config.toml";
/// The mode of a private directory: read, write and search for the owner only.
pub(crate) const PRIVATE_DIRECTORY_MODE: u32 = 0o700;

/// Returns `$SPACECRAFT_HOME`, `$XDG_CONFIG_HOME/spacecraft` or `$HOME/.config/spacecraft`.
pub(crate) fn config_dir() -> Result<PathBuf> {
    resolve_in(
        env::var_os(SPACECRAFT_HOME_ENV).as_deref(),
        env::var_os(XDG_CONFIG_HOME_ENV).as_deref(),
        env::var_os(HOME_ENV).as_deref(),
        HOME_CONFIG_FALLBACK,
    )
}

/// Returns `$SPACECRAFT_HOME`, `$XDG_STATE_HOME/spacecraft` or `$HOME/.local/state/spacecraft`.
pub(crate) fn state_dir() -> Result<PathBuf> {
    resolve_in(
        env::var_os(SPACECRAFT_HOME_ENV).as_deref(),
        env::var_os(XDG_STATE_HOME_ENV).as_deref(),
        env::var_os(HOME_ENV).as_deref(),
        HOME_STATE_FALLBACK,
    )
}

/// Creates `path` with mode `0700` and its missing parents with default
/// permissions. An existing `path` keeps its permissions.
pub(crate) fn create_private_directory(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::io(parent, source))?;
    }

    match DirBuilder::new().mode(PRIVATE_DIRECTORY_MODE).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(Error::io(path, source)),
    }
}

/// Picks the first non-empty value: `spacecraft_home` as is, `xdg` joined with
/// [`APP_DIRECTORY`], or `home` joined with `fallback` and [`APP_DIRECTORY`].
fn resolve_in(
    spacecraft_home: Option<&OsStr>,
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
    fallback: &str,
) -> Result<PathBuf> {
    match (non_empty(spacecraft_home), non_empty(xdg), non_empty(home)) {
        (Some(path), _, _) => checked(path),
        (None, Some(path), _) => Ok(checked(path)?.join(APP_DIRECTORY)),
        (None, None, Some(path)) => Ok(checked(path)?.join(fallback).join(APP_DIRECTORY)),
        (None, None, None) => Err(Error::MissingHome),
    }
}

/// Treats an empty value as unset.
fn non_empty(value: Option<&OsStr>) -> Option<&Path> {
    value.filter(|value| !value.is_empty()).map(Path::new)
}

/// Rejects a path that is relative or contains `..`.
fn checked(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(Error::UnsafeDirectory {
            path: path.to_path_buf(),
            reason: "the path must be absolute and must not contain '..'",
        });
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::resolve_in;
    use crate::Error;

    const FALLBACKS: [&str; 2] = [".config", ".local/state"];

    #[test]
    fn spacecraft_home_wins() {
        for fallback in FALLBACKS {
            let path = resolve_in(
                Some("/opt/sc".as_ref()),
                Some("/x".as_ref()),
                Some("/h".as_ref()),
                fallback,
            )
            .unwrap();
            assert_eq!(path, Path::new("/opt/sc"));
        }
    }

    #[test]
    fn xdg_is_used_when_spacecraft_home_is_unset() {
        for spacecraft_home in [None, Some("".as_ref())] {
            for fallback in FALLBACKS {
                let path = resolve_in(
                    spacecraft_home,
                    Some("/x".as_ref()),
                    Some("/h".as_ref()),
                    fallback,
                )
                .unwrap();
                assert_eq!(path, Path::new("/x/spacecraft"));
            }
        }
    }

    #[test]
    fn home_is_the_last_fallback() {
        for unset in [None, Some("".as_ref())] {
            for (fallback, expected) in [
                (".config", "/h/.config/spacecraft"),
                (".local/state", "/h/.local/state/spacecraft"),
            ] {
                let path = resolve_in(unset, unset, Some("/h".as_ref()), fallback).unwrap();
                assert_eq!(path, Path::new(expected));
            }
        }
    }

    #[test]
    fn missing_home_is_an_error() {
        for home in [None, Some("".as_ref())] {
            for fallback in FALLBACKS {
                assert!(matches!(
                    resolve_in(None, None, home, fallback),
                    Err(Error::MissingHome)
                ));
            }
        }
    }

    #[test]
    fn relative_and_parent_paths_are_rejected() {
        for value in ["relative", "/tmp/../x"] {
            for fallback in FALLBACKS {
                for result in [
                    resolve_in(
                        Some(value.as_ref()),
                        Some("/x".as_ref()),
                        Some("/h".as_ref()),
                        fallback,
                    ),
                    resolve_in(None, Some(value.as_ref()), Some("/h".as_ref()), fallback),
                    resolve_in(None, None, Some(value.as_ref()), fallback),
                ] {
                    assert!(matches!(
                        result,
                        Err(Error::UnsafeDirectory { path, .. }) if path == Path::new(value)
                    ));
                }
            }
        }
    }
}
