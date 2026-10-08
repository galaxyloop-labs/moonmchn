//! Private, per-user runtime directories for daemon state.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, Metadata};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

#[cfg(target_os = "linux")]
pub(super) const RUNTIME_ENV: &str = "XDG_RUNTIME_DIR";
#[cfg(target_os = "macos")]
pub(super) const RUNTIME_ENV: &str = "TMPDIR";

const DIRECTORY_NAME: &str = "spacecraft";

/// Creates or reuses the private `spacecraft` child of the runtime root `value`.
pub(super) fn prepare_in(value: Option<&OsStr>, uid: u32) -> Result<PathBuf> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or(Error::MissingDaemonRuntime {
            variable: RUNTIME_ENV,
        })?;
    let path = Path::new(value);
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(unsafe_directory(
            path,
            "the path must be absolute and must not contain '..'",
        ));
    }
    let (Some(name), Some(parent)) = (path.file_name(), path.parent()) else {
        return Err(unsafe_directory(
            path,
            "the filesystem root is not a private runtime directory",
        ));
    };

    // Allow system ancestor links such as macOS's /var -> /private/var, but
    // leave the final component untouched so a symlinked root is rejected.
    let parent = fs::canonicalize(parent).map_err(|source| super::io_error(parent, source))?;
    // Plain path lookups are enough: once every ancestor passes, only root or
    // the effective user can replace any directory on this path.
    for ancestor in parent.ancestors() {
        validate_ancestor(&lstat(ancestor)?, ancestor, uid)?;
    }
    let root = parent.join(name);
    validate_private(&lstat(&root)?, &root, uid)?;

    let directory = root.join(DIRECTORY_NAME);
    // EEXIST is expected on repeated or concurrent calls, not proof of safety.
    match DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(super::io_error(&directory, source)),
    }
    validate_private(&lstat(&directory)?, &directory, uid)?;
    Ok(directory)
}

/// Reads metadata without following a final symlink.
fn lstat(path: &Path) -> Result<Metadata> {
    fs::symlink_metadata(path).map_err(|source| super::io_error(path, source))
}

/// Requires a trusted owner and protection against replacement by other users.
fn validate_ancestor(metadata: &Metadata, path: &Path, uid: u32) -> Result<()> {
    if metadata.uid() != 0 && metadata.uid() != uid {
        return Err(unsafe_directory(
            path,
            "an ancestor must belong to root or the effective user",
        ));
    }
    // Root-owned sticky directories such as /tmp prevent other users from
    // replacing our entries. Ordinary group/world-writable ancestors do not.
    let root_owned_sticky = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
    if metadata.mode() & 0o022 != 0 && !root_owned_sticky {
        return Err(unsafe_directory(
            path,
            "an ancestor is writable by other users without root-owned sticky protection",
        ));
    }
    Ok(())
}

/// Requires a real directory owned by `uid` with exactly mode `0700`, without repairing it.
fn validate_private(metadata: &Metadata, path: &Path, uid: u32) -> Result<()> {
    if !metadata.is_dir() {
        return Err(unsafe_directory(
            path,
            "the path must be a real directory, not a symlink",
        ));
    }
    if metadata.uid() != uid {
        return Err(unsafe_directory(
            path,
            "the directory must belong to the effective user",
        ));
    }
    if metadata.mode() & 0o7777 != 0o700 {
        return Err(unsafe_directory(
            path,
            "the directory must have permissions 0700",
        ));
    }
    Ok(())
}

/// Attaches the rejected path to a directory safety failure.
fn unsafe_directory(path: &Path, reason: &'static str) -> Error {
    Error::UnsafeDaemonRuntime {
        path: path.to_path_buf(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use rustix::process::geteuid;

    use super::{RUNTIME_ENV, prepare_in, validate_private};
    use crate::{Error, Result};

    fn prepare(path: &Path) -> Result<PathBuf> {
        prepare_in(Some(path.as_os_str()), geteuid().as_raw())
    }

    fn prepare_at(path: &Path) -> PathBuf {
        let directory = prepare(path).unwrap();
        assert_eq!(directory, path.canonicalize().unwrap().join("spacecraft"));
        directory
    }

    fn assert_unsafe(path: &Path) {
        assert!(matches!(
            prepare(path),
            Err(Error::UnsafeDaemonRuntime { .. })
        ));
    }

    fn private_runtime() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }

    #[test]
    fn creates_private_directory_and_reuses_it_without_changing_contents() {
        let runtime = private_runtime();
        let directory = prepare_at(runtime.path());
        let metadata = fs::metadata(&directory).unwrap();
        assert_eq!(metadata.uid(), geteuid().as_raw());
        assert_eq!(metadata.mode() & 0o7777, 0o700);
        fs::write(directory.join("keep"), "keep").unwrap();

        assert_eq!(prepare_at(runtime.path()), directory);
        assert_eq!(fs::read_to_string(directory.join("keep")).unwrap(), "keep");
    }

    #[test]
    fn rejects_unset_empty_relative_parent_and_root_paths() {
        for value in [None, Some(OsStr::new(""))] {
            assert!(matches!(
                prepare_in(value, geteuid().as_raw()),
                Err(Error::MissingDaemonRuntime { variable }) if variable == RUNTIME_ENV
            ));
        }
        for path in ["relative", "./relative", "/tmp/../tmp", "/"] {
            assert_unsafe(Path::new(path));
        }
    }

    #[test]
    fn does_not_create_a_missing_runtime_root() {
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("missing");
        assert!(matches!(prepare(&missing), Err(Error::Io { .. })));
        assert!(!missing.exists());
    }

    #[test]
    fn rejects_insecure_runtime_permissions_without_repairing_them() {
        let runtime = private_runtime();
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_unsafe(runtime.path());
        assert_eq!(fs::metadata(runtime.path()).unwrap().mode() & 0o7777, 0o755);
        assert!(!runtime.path().join("spacecraft").exists());
    }

    #[test]
    fn rejects_insecure_existing_child_without_repairing_it() {
        let runtime = private_runtime();
        let directory = prepare_at(runtime.path());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
        assert_unsafe(runtime.path());
        assert_eq!(fs::metadata(directory).unwrap().mode() & 0o7777, 0o750);
    }

    #[test]
    fn rejects_runtime_symlinks_even_with_trailing_separators() {
        let parent = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let link = parent.path().join("runtime");
        symlink(target.path(), &link).unwrap();
        for path in [&link, &link.join(""), &link.join(".")] {
            assert_unsafe(path);
        }
        assert!(!target.path().join("spacecraft").exists());
    }

    #[test]
    fn rejects_child_symlinks_including_dangling_ones() {
        for target_exists in [false, true] {
            let runtime = private_runtime();
            let target = runtime.path().join("target");
            if target_exists {
                fs::create_dir(&target).unwrap();
            }
            let directory = runtime.path().join("spacecraft");
            symlink(&target, &directory).unwrap();
            assert_unsafe(runtime.path());
            assert_eq!(fs::read_link(directory).unwrap(), target);
            assert_eq!(target.exists(), target_exists);
        }
    }

    #[test]
    fn rejects_files_without_modifying_them() {
        let runtime = private_runtime();
        let directory = runtime.path().join("spacecraft");
        fs::write(&directory, "keep").unwrap();
        assert_unsafe(runtime.path());
        assert_eq!(fs::read_to_string(directory).unwrap(), "keep");
    }

    #[test]
    fn rejects_writable_ancestors_before_creating_state() {
        let parent = tempfile::tempdir().unwrap();
        let runtime = parent.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert_unsafe(&runtime);
        assert!(!runtime.join("spacecraft").exists());
    }

    #[test]
    fn allows_system_style_ancestor_symlinks_and_returns_a_resolved_path() {
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("ancestor");
        let target = tempfile::tempdir().unwrap();
        symlink(target.path(), &link).unwrap();
        let runtime = target.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            prepare_at(&link.join("runtime")),
            runtime.canonicalize().unwrap().join("spacecraft")
        );
    }

    #[test]
    fn rejects_wrong_owner_without_needing_chown() {
        let runtime = private_runtime();
        let metadata = fs::metadata(runtime.path()).unwrap();
        let wrong_uid = geteuid().as_raw().wrapping_add(1);
        assert!(matches!(
            validate_private(&metadata, runtime.path(), wrong_uid),
            Err(Error::UnsafeDaemonRuntime { .. })
        ));
    }
}
