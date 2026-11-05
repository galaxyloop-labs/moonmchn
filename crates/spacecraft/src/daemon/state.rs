//! The private, per-user state directory that holds the daemon's files.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, Metadata};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::daemon::egress::EGRESS_DIR_NAME;
use crate::daemon::machines::MACHINE_DIR_NAME;
use crate::dirs::{self, PRIVATE_DIRECTORY_MODE};
use crate::{Error, Result};

/// The directories the daemon keeps in its state directory, which `prepare_in` creates.
const LAYOUT: [&str; 2] = [MACHINE_DIR_NAME, EGRESS_DIR_NAME];

/// The permission bits of a mode, without the file type.
const PERMISSION_BITS: u32 = 0o7777;
/// Write permission for the group and for other users.
const WRITABLE_BY_OTHERS: u32 = 0o022;
/// The sticky bit: in a directory, only an entry's owner may rename or delete it.
const STICKY_BIT: u32 = 0o1000;
/// The user id of root.
const ROOT_UID: u32 = 0;

/// Creates or reuses the private state directory `leaf`, lays out the daemon's directories in
/// it, and returns its resolved path.
///
/// `leaf` is the directory Spacecraft owns, such as `~/.local/state/spacecraft` or
/// `$SPACECRAFT_HOME` as given; every directory above it is an ancestor. Missing
/// ancestors are created with mode `0700` and symlinks among them are resolved. Each
/// ancestor must belong to root or `uid` and must not be writable by other users,
/// unless it is a root-owned sticky directory such as `/tmp`. `leaf` must be a real
/// directory owned by `uid` with mode `0700`: a missing one is created that way, an
/// existing one is never repaired. Every name in `LAYOUT` follows, inside it: a missing
/// directory gets mode `0700`, and an existing one keeps its permissions.
///
/// This is the one function that creates directories in the state directory, and the daemon
/// calls it when it starts, before anything else touches the directory.
pub(super) fn prepare_in(leaf: &Path, uid: u32) -> Result<PathBuf> {
    let (parent, name) = split(leaf)?;
    // Private, so a umask such as 002 cannot leave a new ancestor group-writable.
    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIRECTORY_MODE)
        .create(parent)
        .map_err(|source| Error::io(parent, source))?;

    let leaf = trusted_parent(parent, uid)?.join(name);
    // An existing leaf is not proof of safety, so it is validated either way.
    dirs::create_private_directory(&leaf)?;
    validate_private(&lstat(&leaf)?, &leaf, uid)?;

    for directory in LAYOUT {
        dirs::create_private_directory(&leaf.join(directory))?;
    }

    Ok(leaf)
}

/// Checks the private state directory `leaf`, creating nothing, and returns its path.
///
/// A missing `leaf` comes back as given: no daemon has run there, and only the daemon creates
/// it, through `prepare_in`. Anything at that path, a dangling symlink included, passes the
/// checks `prepare_in` runs, the ancestors and then the leaf, and comes back resolved.
pub(super) fn locate_in(leaf: &Path, uid: u32) -> Result<PathBuf> {
    match fs::symlink_metadata(leaf) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(leaf.to_path_buf()),
        Err(source) => return Err(Error::io(leaf, source)),
    }

    let (parent, name) = split(leaf)?;
    let leaf = trusted_parent(parent, uid)?.join(name);
    validate_private(&lstat(&leaf)?, &leaf, uid)?;
    Ok(leaf)
}

/// Splits `leaf` into its parent and its name. The filesystem root has neither, and is no
/// private directory.
fn split(leaf: &Path) -> Result<(&Path, &OsStr)> {
    match (leaf.parent(), leaf.file_name()) {
        (Some(parent), Some(name)) => Ok((parent, name)),
        _ => Err(unsafe_directory(
            leaf,
            "the filesystem root is not a private directory",
        )),
    }
}

/// Resolves `parent`, which exists, and checks every ancestor on the resolved path.
fn trusted_parent(parent: &Path, uid: u32) -> Result<PathBuf> {
    // Allow system ancestor links such as macOS's /var -> /private/var, but
    // leave the final component untouched so a symlinked leaf is rejected.
    let parent = fs::canonicalize(parent).map_err(|source| Error::io(parent, source))?;
    // Plain path lookups are enough: once every ancestor passes, only root or
    // the effective user can replace any directory on this path.
    for ancestor in parent.ancestors() {
        validate_ancestor(&lstat(ancestor)?, ancestor, uid)?;
    }
    Ok(parent)
}

/// Reads metadata without following a final symlink.
fn lstat(path: &Path) -> Result<Metadata> {
    fs::symlink_metadata(path).map_err(|source| Error::io(path, source))
}

/// Requires a trusted owner and protection against replacement by other users.
fn validate_ancestor(metadata: &Metadata, path: &Path, uid: u32) -> Result<()> {
    if !has_trusted_owner(metadata, uid) {
        return Err(unsafe_directory(
            path,
            "an ancestor must belong to root or the effective user",
        ));
    }
    if is_writable_by_others(metadata) && !is_root_owned_sticky(metadata) {
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
    if !has_private_mode(metadata) {
        return Err(unsafe_directory(
            path,
            "the directory must have permissions 0700",
        ));
    }
    Ok(())
}

/// Whether root or `uid` owns the entry: nobody else is trusted with our path.
fn has_trusted_owner(metadata: &Metadata, uid: u32) -> bool {
    metadata.uid() == ROOT_UID || metadata.uid() == uid
}

/// Whether the group or other users may create, rename or delete entries inside.
fn is_writable_by_others(metadata: &Metadata) -> bool {
    metadata.mode() & WRITABLE_BY_OTHERS != 0
}

/// Whether root owns the directory and set its sticky bit, as on `/tmp`: other users
/// may write in it, but cannot rename or delete our entries.
fn is_root_owned_sticky(metadata: &Metadata) -> bool {
    metadata.uid() == ROOT_UID && metadata.mode() & STICKY_BIT != 0
}

/// Whether the permission bits are exactly [`PRIVATE_DIRECTORY_MODE`].
fn has_private_mode(metadata: &Metadata) -> bool {
    metadata.mode() & PERMISSION_BITS == PRIVATE_DIRECTORY_MODE
}

/// Attaches the rejected path to a directory safety failure.
fn unsafe_directory(path: &Path, reason: &'static str) -> Error {
    Error::UnsafeDirectory {
        path: path.to_path_buf(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use rustix::process::geteuid;

    use super::{locate_in, prepare_in, validate_private};
    use crate::{Error, Result};

    fn prepare(leaf: &Path) -> Result<PathBuf> {
        prepare_in(leaf, geteuid().as_raw())
    }

    fn locate(leaf: &Path) -> Result<PathBuf> {
        locate_in(leaf, geteuid().as_raw())
    }

    /// Prepares `leaf` and checks that its resolved path comes back.
    fn prepare_at(leaf: &Path) -> PathBuf {
        let directory = prepare(leaf).unwrap();
        assert_eq!(directory, leaf.canonicalize().unwrap());
        directory
    }

    fn assert_unsafe(leaf: &Path) {
        assert!(matches!(prepare(leaf), Err(Error::UnsafeDirectory { .. })));
    }

    /// The permission bits of `path` itself, without following a symlink.
    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().mode() & 0o7777
    }

    #[test]
    fn creates_the_leaf_private_and_reuses_it_without_changing_contents() {
        let parent = tempfile::tempdir().unwrap();
        let leaf = parent.path().join("spacecraft");

        let directory = prepare_at(&leaf);
        assert_eq!(fs::metadata(&directory).unwrap().uid(), geteuid().as_raw());
        assert_eq!(mode(&directory), 0o700);
        fs::write(directory.join("keep"), "keep").unwrap();

        assert_eq!(prepare_at(&leaf), directory);
        assert_eq!(fs::read_to_string(directory.join("keep")).unwrap(), "keep");
    }

    #[test]
    fn lays_out_the_daemon_directories() {
        let root = tempfile::tempdir().unwrap();

        let directory = prepare_at(&root.path().join("spacecraft"));

        for name in ["machines", "egress"] {
            assert_eq!(mode(&directory.join(name)), 0o700, "{name}");
        }

        // An existing directory keeps its permissions, as the leaf does.
        let leaf = root.path().join("other");
        fs::create_dir(&leaf).unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(leaf.join("machines")).unwrap();
        fs::set_permissions(leaf.join("machines"), fs::Permissions::from_mode(0o750)).unwrap();

        prepare_at(&leaf);

        assert_eq!(mode(&leaf.join("machines")), 0o750);
    }

    #[test]
    fn locate_creates_nothing_for_a_missing_directory() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join(".local/state/spacecraft");

        assert_eq!(locate(&leaf).unwrap(), leaf);
        assert!(!root.path().join(".local").exists());
    }

    #[test]
    fn locate_checks_an_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("spacecraft");
        fs::create_dir(&leaf).unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o750)).unwrap();

        assert!(matches!(locate(&leaf), Err(Error::UnsafeDirectory { .. })));
        assert_eq!(mode(&leaf), 0o750);

        // A directory the daemon prepared comes back resolved, as `prepare_in` returns it.
        let prepared = prepare_at(&root.path().join("prepared"));
        assert_eq!(locate(&root.path().join("prepared")).unwrap(), prepared);
    }

    #[test]
    fn creates_missing_parents_private() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join(".local/state/spacecraft");

        let directory = prepare_at(&leaf);

        assert_eq!(mode(&directory), 0o700);
        for parent in [".local", ".local/state"] {
            assert_eq!(mode(&root.path().join(parent)), 0o700, "{parent}");
        }
    }

    #[test]
    fn rejects_a_world_writable_ancestor_before_creating_the_leaf() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let leaf = root.path().join("spacecraft");

        assert_unsafe(&leaf);
        assert!(!leaf.exists());
    }

    #[test]
    fn rejects_an_insecure_existing_leaf_without_repairing_it() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("spacecraft");
        fs::create_dir(&leaf).unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o750)).unwrap();

        assert_unsafe(&leaf);
        assert_eq!(mode(&leaf), 0o750);
    }

    #[test]
    fn rejects_a_symlinked_leaf_including_dangling_ones() {
        for target_exists in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let target = root.path().join("target");
            if target_exists {
                // Private, so only the symlink check can reject it.
                fs::create_dir(&target).unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let leaf = root.path().join("spacecraft");
            symlink(&target, &leaf).unwrap();

            // A trailing `/` or `/.` makes a path lookup follow the link.
            for path in [leaf.clone(), leaf.join(""), leaf.join(".")] {
                assert_unsafe(&path);
            }
            assert_eq!(fs::read_link(&leaf).unwrap(), target);
            assert_eq!(target.exists(), target_exists);
        }
    }

    #[test]
    fn rejects_a_file_in_place_of_the_leaf() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("spacecraft");
        fs::write(&leaf, "keep").unwrap();

        assert_unsafe(&leaf);
        assert_eq!(fs::read_to_string(&leaf).unwrap(), "keep");
    }

    #[test]
    fn allows_symlinked_ancestors_and_returns_the_resolved_path() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let link = root.path().join("ancestor");
        symlink(target.path(), &link).unwrap();

        let directory = prepare_at(&link.join("spacecraft"));

        assert_eq!(
            directory,
            target.path().canonicalize().unwrap().join("spacecraft")
        );
    }

    #[test]
    fn rejects_the_filesystem_root() {
        assert_unsafe(Path::new("/"));
    }

    #[test]
    fn rejects_wrong_owner_without_needing_chown() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = fs::metadata(dir.path()).unwrap();
        let wrong_uid = geteuid().as_raw().wrapping_add(1);
        assert!(matches!(
            validate_private(&metadata, dir.path(), wrong_uid),
            Err(Error::UnsafeDirectory { .. })
        ));
    }
}
