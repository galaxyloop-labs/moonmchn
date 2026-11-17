//! Daemon instance

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::{ByteSize, Error, Result};

/// The name of the lock file used to ensure a single daemon instance.
const LOCK_FILE_NAME: &str = "daemon.pid";

/// The name of the socket file
const SOCKET_FILE_NAME: &str = "daemon.sock";

/// The name of the current log. Each previous log adds its number to it, as in `daemon.log.1`.
const LOG_FILE_NAME: &str = "daemon.log";

/// The mode of the lock and log files.
const FILE_MODE: u32 = 0o600;

/// The size at which `daemon.log` rotates: a line that would cross it goes to a fresh log.
const LOG_ROTATE_BYTES: ByteSize = ByteSize::checked_from_mib(8).expect("8 MiB fits in a u64");

/// The number of previous logs kept, `daemon.log.1` through `daemon.log.15`. With `daemon.log`,
/// the 16 logs take at most 128 MiB.
const PREVIOUS_LOGS_MAX: u32 = 15;

/// The separator between a previous log's name and its number, as in `daemon.log.1`.
const LOG_NUMBER_SEPARATOR: char = '.';

/// The size in bytes of the newline that ends each log line.
const NEWLINE_BYTES: u64 = 1;

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

/// Moves every log in `dir` one step back, as `logrotate` numbers them, and opens a fresh
/// `daemon.log` as `open_log` does: `daemon.log.14` moves over the oldest, `daemon.log.15`,
/// then each younger one over the next, down to `daemon.log`, which becomes `daemon.log.1`.
/// The oldest moves first, so no log is overwritten before it moved. A log missing from the
/// sequence, deleted while the daemon ran, is skipped, and a fresh `daemon.log` opens all the
/// same, so the log keeps going.
///
/// # Errors
///
/// Returns [`Error::Io`] if a log can't be moved, or `daemon.log` can't be opened. The rotation
/// stops at the first log that can't move, so no younger log moves over it.
pub(super) fn rotate_log(dir: &Path) -> Result<File> {
    for n in (1..PREVIOUS_LOGS_MAX).rev() {
        let next = n.saturating_add(1);
        move_log(&numbered_log_path(dir, n), &numbered_log_path(dir, next))?;
    }
    move_log(&dir.join(LOG_FILE_NAME), &numbered_log_path(dir, 1))?;

    open_log(dir)
}

/// The path in `dir` of the log numbered `n`, as in `daemon.log.1`.
fn numbered_log_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{LOG_FILE_NAME}{LOG_NUMBER_SEPARATOR}{n}"))
}

/// Renames the log `from` to `to`, skipping a `from` deleted while the daemon ran.
fn move_log(from: &Path, to: &Path) -> Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::io(from, err)),
    }
}

/// Returns whether `line` and the newline after it fit in a log that holds `len` bytes, without
/// taking the log past `LOG_ROTATE_BYTES`.
pub(super) fn fits_in_log(len: u64, line: &str) -> bool {
    u64::try_from(line.len()).is_ok_and(|line_len| {
        len.saturating_add(line_len).saturating_add(NEWLINE_BYTES) <= LOG_ROTATE_BYTES.as_bytes()
    })
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

    #[test]
    fn rotation_shifts_the_previous_logs_and_drops_the_oldest() {
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();
        // Sixteen logs, each holding its own number: `daemon.log` holds 0, and the oldest,
        // `daemon.log.15`, holds 15.
        fs::write(dir.join("daemon.log"), "0\n").unwrap();
        for n in 1..=15 {
            fs::write(dir.join(format!("daemon.log.{n}")), format!("{n}\n")).unwrap();
        }

        let mut file = super::rotate_log(dir).unwrap();
        file.write_all(b"fresh\n").unwrap();

        // Each log moved one step back, over the next, and the oldest is gone.
        for n in 1..=15 {
            assert_eq!(
                read(dir, &format!("daemon.log.{n}")),
                Some(format!("{}\n", n - 1)),
                "daemon.log.{n}"
            );
        }
        assert_eq!(read(dir, "daemon.log.16"), None);
        // The file returned is a fresh, private `daemon.log`.
        assert_eq!(read(dir, "daemon.log").as_deref(), Some("fresh\n"));
        assert_eq!(file.metadata().unwrap().mode() & 0o7777, 0o600);
    }

    #[test]
    fn rotation_moves_the_logs_on_both_sides_of_a_gap() {
        // `daemon.log.1` was deleted, and the logs around it still move one step back.
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();
        fs::write(dir.join("daemon.log"), "current\n").unwrap();
        fs::write(dir.join("daemon.log.2"), "older\n").unwrap();

        super::rotate_log(dir).unwrap();

        assert_eq!(read(dir, "daemon.log.1").as_deref(), Some("current\n"));
        assert_eq!(read(dir, "daemon.log.2"), None);
        assert_eq!(read(dir, "daemon.log.3").as_deref(), Some("older\n"));
    }

    #[test]
    fn rotating_a_deleted_log_opens_a_fresh_one() {
        // Someone deleted `daemon.log` while the daemon ran, so there is nothing to move aside.
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();

        let mut file = super::rotate_log(dir).unwrap();
        file.write_all(b"fresh\n").unwrap();

        assert_eq!(read(dir, "daemon.log").as_deref(), Some("fresh\n"));
        assert_eq!(read(dir, "daemon.log.1"), None);
    }

    #[test]
    fn a_line_fits_with_its_newline_up_to_8_mib() {
        // `abc` and its newline take 4 bytes: they fill a log 4 bytes short of the cap, and
        // cross it from 3 bytes short.
        assert!(super::fits_in_log(8 * 1024 * 1024 - 4, "abc"));
        assert!(!super::fits_in_log(8 * 1024 * 1024 - 3, "abc"));
    }

    #[test]
    fn a_failed_move_stops_the_rotation() {
        // `daemon.log.15` is a directory with a file in it: `daemon.log.14` can't move over it.
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();
        fs::create_dir_all(dir.join("daemon.log.15/x")).unwrap();
        fs::write(dir.join("daemon.log.14"), "14\n").unwrap();
        fs::write(dir.join("daemon.log.13"), "13\n").unwrap();

        assert!(super::rotate_log(dir).is_err());
        // `daemon.log.13` didn't move over the log that stayed.
        assert_eq!(read(dir, "daemon.log.14").as_deref(), Some("14\n"));
    }

    /// The contents of the file `name` in `dir`, or `None` when there is no such file, so a
    /// missing log fails an assertion instead of an `unwrap`.
    fn read(dir: &Path, name: &str) -> Option<String> {
        fs::read_to_string(dir.join(name)).ok()
    }
}
