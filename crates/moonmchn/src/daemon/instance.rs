//! Daemon instance

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The name of the lock file used to ensure a single daemon instance.
const LOCK_FILE_NAME: &str = "daemon.pid";

/// The name of the socket file
const SOCKET_FILE_NAME: &str = "daemon.sock";

/// The name of the log file
const LOG_FILE_NAME: &str = "daemon.log";

/// The mode of the lock and log files.
const FILE_MODE: u32 = 0o600;

pub(super) fn lock(dir: &Path) -> Result<File> {
    let path = dir.join(LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(false)
        .mode(FILE_MODE)
        .create(true)
        .open(&path)
        .map_err(|err| Error::io(&path, err))?;

    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::DaemonAlreadyRunning), // Lock not acquired
        Err(TryLockError::Error(err)) => Err(Error::io(&path, err)),
    }
}

/// Returns whether a daemon holds the lock on `daemon.pid` in `dir`, creating nothing.
///
/// The file opens for reading, without `create`, so a missing file, or a missing `dir`, means
/// that no daemon runs there. A free lock is taken and dropped at once, as `lock` would take
/// it: a start racing that exact instant could see it held.
pub(super) fn lock_held(dir: &Path) -> Result<bool> {
    let path = dir.join(LOCK_FILE_NAME);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Error::io(&path, err)),
    };

    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(err)) => Err(Error::io(&path, err)),
    }
}

/// Claims `dir` for this daemon: takes the lock on `daemon.pid`, writes this process's pid into
/// the file, and removes a socket a dead daemon left behind.
///
/// The returned `File` holds the lock for as long as it, or any clone of it, lives. `Machines`
/// keeps it, behind an `Arc` that every worker able to change a marker clones, so the lock lasts
/// until the registry and the last of those workers are gone.
///
/// # Errors
///
/// Returns [`Error::DaemonAlreadyRunning`] if another daemon holds the lock, and [`Error::Io`]
/// if `daemon.pid` cannot be opened, locked or written.
pub(super) fn claim(dir: &Path) -> Result<File> {
    let mut file = lock(dir)?;
    let path = dir.join(LOCK_FILE_NAME);

    file.set_len(0).map_err(|err| Error::io(&path, err))?;
    writeln!(&mut file, "{}", std::process::id()).map_err(|err| Error::io(&path, err))?;

    // Remove the socket a dead daemon left behind. One that survives this makes the
    // daemon's `bind` fail with the socket path.
    drop(std::fs::remove_file(socket_path(dir)));

    Ok(file)
}

pub(super) fn open_log(dir: &Path) -> Result<File> {
    let path = dir.join(LOG_FILE_NAME);

    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(FILE_MODE)
        .open(&path)
        .map_err(|e| Error::io(&path, e))
}

pub(super) fn socket_path(dir: &Path) -> PathBuf {
    dir.join(SOCKET_FILE_NAME)
}

/// macOS refuses the connection while the backlog is full, so a blocking connect already
/// fails at once.
#[cfg(target_os = "macos")]
pub(super) fn connect(path: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// Linux queues the connection while the backlog is full, and a stalled daemon never
/// drains it. A non-blocking socket fails with `WouldBlock` at once.
#[cfg(target_os = "linux")]
pub(super) fn connect(path: &Path) -> io::Result<UnixStream> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

    let fd = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::connect(&fd, &SocketAddrUnix::new(path)?)?;

    // Back to blocking, so the callers' read timeouts apply.
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn exclusive_lock() {
        let lock_dir = tempfile::tempdir().unwrap();
        let file = super::lock(lock_dir.path()).unwrap();

        let expected_mode = file.metadata().unwrap().mode() & 0o7777;
        assert_eq!(expected_mode, 0o600);

        let err = super::lock(lock_dir.path());
        assert!(matches!(err, Err(Error::DaemonAlreadyRunning)));

        file.unlock().unwrap();
        drop(file);

        drop(super::lock(lock_dir.path()).unwrap());
    }

    #[test]
    fn lock_held_reports_the_lock_without_creating_its_file() {
        let lock_dir = tempfile::tempdir().unwrap();

        assert!(!super::lock_held(lock_dir.path()).unwrap());
        assert!(!lock_dir.path().join("daemon.pid").exists());

        let file = super::lock(lock_dir.path()).unwrap();
        assert!(super::lock_held(lock_dir.path()).unwrap());

        drop(file);
        assert!(!super::lock_held(lock_dir.path()).unwrap());
    }

    #[test]
    fn write_pid_in_lock_file() {
        let lock_dir = tempfile::tempdir().unwrap();
        let path = lock_dir.path().join(super::LOCK_FILE_NAME);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(true)
            .create(true)
            .open(&path)
            .unwrap();

        file.write_all("99999999999\n".as_bytes()).unwrap();

        super::claim(lock_dir.path()).unwrap();

        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(format!("{}\n", std::process::id()), contents);
    }

    #[test]
    fn stale_socket() {
        let lock_dir = tempfile::tempdir().unwrap();
        let path = lock_dir.path().join(super::LOCK_FILE_NAME);
        let first = super::claim(lock_dir.path()).unwrap();

        let socket_path = super::socket_path(lock_dir.path());
        fs::write(&socket_path, "").unwrap();

        // Test exclusive lock
        let err = super::claim(lock_dir.path());
        assert!(matches!(err, Err(Error::DaemonAlreadyRunning)));

        // Assert original contents (PID) are protected
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(format!("{}\n", std::process::id()), contents);

        // Assert socket still exists
        assert!(fs::exists(&socket_path).unwrap());

        first.unlock().unwrap();
        drop(first);

        // When unlocked it succeeds again
        super::claim(lock_dir.path()).unwrap();

        // Assert socket is gone
        assert!(!fs::exists(&socket_path).unwrap());
    }

    #[test]
    fn open_log_file() {
        let log_dir = tempfile::tempdir().unwrap();
        let mut file1 = super::open_log(log_dir.path()).unwrap();

        file1.write_all(b"test 1\n").unwrap();

        let mode = file1.metadata().unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            fs::read_to_string(log_dir.path().join(super::LOG_FILE_NAME)).unwrap(),
            "test 1\n"
        );

        // Open again and append
        let mut file2 = super::open_log(log_dir.path()).unwrap();
        file2.write_all(b"test 2\n").unwrap();

        assert_eq!(
            fs::read_to_string(log_dir.path().join(super::LOG_FILE_NAME)).unwrap(),
            "test 1\ntest 2\n"
        );
    }
}
