//! The daemon's log: `daemon.log` and the previous logs kept beside it, their rotation at the
//! cap, and the writer of the decider's lines.
//!
//! In detached mode three writers reach `daemon.log`. `Writer` takes the decider's lines, and
//! only it rotates the log. The daemon's stdout and stderr point at the log as well, so the
//! daemon's own lines and a panic message land there without passing through this module.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::{ByteSize, Error, Result};

/// The name of the current log. Each previous log adds its number to it, as in `daemon.log.1`.
const LOG_FILE_NAME: &str = "daemon.log";

/// The name a fresh log opens under while a rotation makes room for it. The rotation's last step
/// renames it `daemon.log`.
const FRESH_LOG_FILE_NAME: &str = "daemon.log.new";

/// The mode of the log files.
const FILE_MODE: u32 = 0o600;

/// The size at which `daemon.log` rotates: a line that would cross it goes to a fresh log.
const LOG_ROTATE_BYTES: ByteSize = ByteSize::checked_from_mib(8).expect("8 MiB fits in a u64");

/// The number of previous logs kept, `daemon.log.1` through `daemon.log.15`. With `daemon.log`,
/// the 16 logs take at most 128 MiB.
const PREVIOUS_LOGS_MAX: u32 = 15;

/// The number `daemon.log` counts as when a rotation numbers the logs from the youngest. The
/// previous logs follow from 1.
const CURRENT_LOG: u32 = 0;

/// The separator between a previous log's name and its number, as in `daemon.log.1`.
const LOG_NUMBER_SEPARATOR: char = '.';

/// The size in bytes of the newline that ends each log line.
const NEWLINE_BYTES: u64 = 1;

/// The separator between an error's message and each of its causes.
const CAUSE_SEPARATOR: &str = ": ";

pub(super) fn open(dir: &Path) -> Result<File> {
    open_log_at(&dir.join(LOG_FILE_NAME))
}

/// Opens the log at `path` for appending, and creates it, private to the user, when it is
/// missing.
fn open_log_at(path: &Path) -> Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(FILE_MODE)
        .open(path)
        .map_err(|e| Error::io(path, e))
}

/// Rotates the logs in `dir`, as `logrotate` numbers them, and returns a fresh `daemon.log`,
/// opened as `open` opens it.
///
/// The fresh log opens first, as `daemon.log.new`, so a rotation that can't open it moves no
/// log. Then `daemon.log.14` moves over the oldest, `daemon.log.15`, and each younger one over
/// the next, down to `daemon.log`, which becomes `daemon.log.1`. The oldest moves first, so no
/// log is overwritten before it moved. The fresh log takes the name `daemon.log` last.
///
/// The previous logs come out numbered in sequence. A number missing among them, whose log was
/// deleted while the daemon ran, is filled: the logs younger than it move one step back, the
/// older ones close any other gap between them, and none is replaced. With `daemon.log` itself
/// missing, there is nothing to make room for, so no log moves one step back.
///
/// A rotation that fails stops where it is. One that stopped between two moves left a number
/// free, and one that stopped before the fresh log took its name left `daemon.log` missing:
/// either way the next rotation carries on from there. However many attempts a rotation takes,
/// the oldest log is the only one replaced, and once.
///
/// # Errors
///
/// Returns [`Error::Io`] if the fresh log can't be opened or renamed, or a log can't be moved.
/// The rotation stops at the first log that can't move, so no younger log moves over it.
fn rotate_log(dir: &Path) -> Result<File> {
    rotate_log_with(dir, |from, to| std::fs::rename(from, to))
}

/// Rotates the logs in `dir` as `rotate_log` describes, moving each file with `rename`.
/// `rotate_log` passes the file system's rename, and a test passes one that fails.
fn rotate_log_with(dir: &Path, rename: impl Fn(&Path, &Path) -> io::Result<()>) -> Result<File> {
    let fresh_path = dir.join(FRESH_LOG_FILE_NAME);
    let fresh = open_log_at(&fresh_path)?;

    let free = first_free_number(dir)?;
    close_gaps_past(dir, free, &rename)?;
    for n in (CURRENT_LOG..free).rev() {
        let next = n.saturating_add(1);
        move_log(
            &numbered_log_path(dir, n),
            &numbered_log_path(dir, next),
            &rename,
        )?;
    }

    rename(&fresh_path, &dir.join(LOG_FILE_NAME)).map_err(|err| Error::io(&fresh_path, err))?;
    Ok(fresh)
}

/// The number in `dir` that the logs step back into, with `daemon.log` counting as 0: the
/// lowest one that no log holds. When every number below the oldest log's is taken, it is the
/// oldest log's, and that log is the one the rotation replaces.
fn first_free_number(dir: &Path) -> Result<u32> {
    for n in CURRENT_LOG..PREVIOUS_LOGS_MAX {
        if !log_exists(&numbered_log_path(dir, n))? {
            return Ok(n);
        }
    }
    Ok(PREVIOUS_LOGS_MAX)
}

/// Renumbers the logs in `dir` older than the number `free`, so that they follow it in
/// sequence. From the youngest, each takes the next number after `free`, which no log holds by
/// then, so none is overwritten. `rename` moves each one.
fn close_gaps_past(
    dir: &Path,
    free: u32,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<()> {
    let first = free.saturating_add(1);
    let mut next = first;
    for n in first..=PREVIOUS_LOGS_MAX {
        let path = numbered_log_path(dir, n);
        if !log_exists(&path)? {
            continue;
        }
        if n != next {
            move_log(&path, &numbered_log_path(dir, next), &rename)?;
        }
        next = next.saturating_add(1);
    }
    Ok(())
}

/// The path in `dir` of the log numbered `n`: `daemon.log` for the current log, and for a
/// previous one the same name with its number, as in `daemon.log.1`.
fn numbered_log_path(dir: &Path, n: u32) -> PathBuf {
    if n == CURRENT_LOG {
        return dir.join(LOG_FILE_NAME);
    }
    dir.join(format!("{LOG_FILE_NAME}{LOG_NUMBER_SEPARATOR}{n}"))
}

/// Returns whether a log is at `path`.
fn log_exists(path: &Path) -> Result<bool> {
    std::fs::exists(path).map_err(|err| Error::io(path, err))
}

/// Renames the log `from` to `to` with `rename`, skipping a `from` deleted while the daemon ran.
fn move_log(from: &Path, to: &Path, rename: impl Fn(&Path, &Path) -> io::Result<()>) -> Result<()> {
    match rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::io(from, err)),
    }
}

/// Returns whether `line` and the newline after it fit in a log that holds `len` bytes, without
/// taking the log past `LOG_ROTATE_BYTES`.
fn fits_in_log(len: u64, line: &str) -> bool {
    u64::try_from(line.len()).is_ok_and(|line_len| {
        len.saturating_add(line_len).saturating_add(NEWLINE_BYTES) <= LOG_ROTATE_BYTES.as_bytes()
    })
}

/// Writes the decider's lines: to the daemon's `out` in the foreground, and in detached mode to
/// the log, which it rotates at its cap.
///
/// It keeps what lasts from one line to the next: the log's handle, which each rotation
/// replaces, whether the last redirect failed, and whether the last line was dropped. `out` is
/// lent for each line, because the daemon writes its own lines to it in between.
pub(super) struct Writer<R> {
    /// The daemon's state directory, where the log rotates.
    dir: PathBuf,
    /// The log in detached mode, replaced by each rotation, and `None` in the foreground.
    log: Option<File>,
    /// Points the daemon's output at the log a rotation opened. `serve_in` passes
    /// `redirect_output`, and a test passes one that records the log it got, or one that fails.
    redirect: R,
    /// Whether the last `redirect` failed, so the next line tries it again, and a run of
    /// failures is reported once.
    redirect_pending: bool,
    /// Whether the last line was dropped, so a run of them is reported once.
    dropping: bool,
}

impl<R: Fn(&File) -> io::Result<()>> Writer<R> {
    /// A writer for the daemon whose state directory is `dir`, with no line dropped yet and no
    /// redirect pending: `announce_ready` has pointed the daemon's output at `log` already.
    /// `log` is the daemon's log in detached mode, and `None` in the foreground. `redirect`
    /// points the daemon's output at the log a rotation opens.
    pub(super) fn new(dir: &Path, log: Option<File>, redirect: R) -> Self {
        Self {
            dir: dir.to_path_buf(),
            log,
            redirect,
            redirect_pending: false,
            dropping: false,
        }
    }

    /// Writes `line`, one of the decider's, and reports on `out` a line it could not write.
    /// `try_write_line` and `report_dropped` describe the two steps.
    pub(super) fn write_line(&mut self, out: &mut impl Write, line: &str) {
        let written = self.try_write_line(out, line);
        self.report_dropped(written, out);
    }

    /// Writes `line`: to `out` in the foreground, and in detached mode to the log, in three
    /// steps.
    ///
    /// 1. A line that would take the log past its cap rotates it first, and the fresh log takes
    ///    the handle's place.
    /// 2. After a rotation, `redirect` points the daemon's output at the fresh log. When it
    ///    fails, the failure is reported in that log, once for a run of them, and `redirect`
    ///    runs again ahead of each next line, with no rotation, until it succeeds.
    /// 3. The line is written, whether or not the redirect succeeded.
    ///
    /// # Errors
    ///
    /// Returns the error of the log's length check, of the rotation, or of the write. The line
    /// is then dropped, and `report_dropped` reports it. A failed redirect is no error here.
    fn try_write_line(&mut self, out: &mut impl Write, line: &str) -> io::Result<()> {
        let Some(log) = self.log.as_mut() else {
            return writeln!(out, "{line}");
        };

        let rotated = !fits_in_log(log.metadata()?.len(), line);
        if rotated {
            // `?` converts errors with `From`, and no `From` turns the crate's `Error` into the
            // `io::Error` this function returns: `io::Error::other` wraps it, message included.
            *log = rotate_log(&self.dir).map_err(io::Error::other)?;
        }

        // A redirect is owed after a rotation, and again while the last one failed. The retry
        // runs on the log the writer already holds, so it moves no log.
        if rotated || self.redirect_pending {
            match (self.redirect)(log) {
                Ok(()) => self.redirect_pending = false,
                Err(error) => {
                    // Only the first failure of a run is reported. The report goes through the
                    // log's handle, because `out` is the daemon's stdout, which still points at
                    // the previous log.
                    if !self.redirect_pending {
                        let report = format!(
                            "egress: the daemon's own lines still go to the previous log: {error}\n"
                        );
                        drop(log.write_all(report.as_bytes()));
                    }
                    self.redirect_pending = true;
                }
            }
        }

        // The line is written whether or not the redirect succeeded: the cap is checked on the
        // fresh log, so it holds either way.
        //
        // One write per line: `writeln!` writes the line and then its newline, and a write from
        // another thread, such as a panic message on stderr, could land between the two.
        log.write_all(format!("{line}\n").as_bytes())
    }

    /// Reports on `out` a line that `try_write_line` could not write, as when the log fails to
    /// rotate: such a line is dropped. Only the first of a run is reported, with its error, and
    /// the next line written ends the run, so a failure that lasts adds one line to a log that
    /// may be at its cap already. In detached mode `out` is the daemon's stdout, which points at
    /// the log the daemon still holds. While a redirect is pending it points at the previous
    /// log, so the report lands there.
    fn report_dropped(&mut self, written: io::Result<()>, out: &mut impl Write) {
        match written {
            Ok(()) => self.dropping = false,
            Err(_) if self.dropping => {}
            Err(error) => {
                self.dropping = true;
                let error = with_causes(&error);
                drop(writeln!(out, "egress: lines dropped: {error}"));
            }
        }
    }
}

/// Writes `error` with its causes, which its own message leaves out: the crate's errors name
/// what failed, and the operating system's error is their cause.
fn with_causes(error: &io::Error) -> String {
    std::iter::successors(Some(error as &dyn std::error::Error), |error| {
        error.source()
    })
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join(CAUSE_SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn open_log_file() {
        let log_dir = tempfile::tempdir().unwrap();
        let mut file1 = super::open(log_dir.path()).unwrap();

        file1.write_all(b"test 1\n").unwrap();

        let mode = file1.metadata().unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            fs::read_to_string(log_dir.path().join(super::LOG_FILE_NAME)).unwrap(),
            "test 1\n"
        );

        // Open again and append
        let mut file2 = super::open(log_dir.path()).unwrap();
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
    fn rotation_leaves_the_previous_logs_numbered_in_sequence() {
        // Two gaps in the numbering, after `daemon.log.1` and after `daemon.log.3`. A log
        // deleted while the daemon ran leaves one, and so does a rotation that failed before it
        // moved the next log there.
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();
        fs::write(dir.join("daemon.log"), "current\n").unwrap();
        fs::write(dir.join("daemon.log.1"), "1\n").unwrap();
        fs::write(dir.join("daemon.log.3"), "3\n").unwrap();
        fs::write(dir.join("daemon.log.15"), "15\n").unwrap();

        super::rotate_log(dir).unwrap();

        // The two most recent logs moved one step back, into the first gap. `daemon.log.3`
        // stayed, and the oldest moved down next to it: no log was replaced.
        assert_eq!(read(dir, "daemon.log.1").as_deref(), Some("current\n"));
        assert_eq!(read(dir, "daemon.log.2").as_deref(), Some("1\n"));
        assert_eq!(read(dir, "daemon.log.3").as_deref(), Some("3\n"));
        assert_eq!(read(dir, "daemon.log.4").as_deref(), Some("15\n"));
        assert_eq!(read(dir, "daemon.log.15"), None);
    }

    #[test]
    fn rotating_a_deleted_log_leaves_the_previous_logs_in_place() {
        // `daemon.log` is gone: someone deleted it while the daemon ran, or a rotation moved it
        // aside and then failed. There is nothing to move aside, so no previous log moves.
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();
        fs::write(dir.join("daemon.log.1"), "1\n").unwrap();

        let mut file = super::rotate_log(dir).unwrap();
        file.write_all(b"fresh\n").unwrap();

        assert_eq!(read(dir, "daemon.log").as_deref(), Some("fresh\n"));
        assert_eq!(read(dir, "daemon.log.1").as_deref(), Some("1\n"));
        assert_eq!(read(dir, "daemon.log.2"), None);
    }

    #[test]
    fn a_rotation_that_cannot_open_the_fresh_log_moves_no_log() {
        // A directory holds the name the fresh log opens under, so it can't open, as when the
        // daemon has no file descriptor left.
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();
        fs::create_dir(dir.join("daemon.log.new")).unwrap();
        fs::write(dir.join("daemon.log"), "0\n").unwrap();
        for n in 1..=15 {
            fs::write(dir.join(format!("daemon.log.{n}")), format!("{n}\n")).unwrap();
        }

        // Each line the decider sends tries the rotation again. Were a failed attempt to move
        // the logs, 16 of them would push every one of the 16 logs out.
        for _ in 0..16 {
            assert!(super::rotate_log(dir).is_err());
        }

        assert_eq!(read(dir, "daemon.log").as_deref(), Some("0\n"));
        for n in 1..=15 {
            assert_eq!(
                read(dir, &format!("daemon.log.{n}")),
                Some(format!("{n}\n")),
                "daemon.log.{n}"
            );
        }
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
        // Every number is taken, and `daemon.log.15` is a directory with a file in it:
        // `daemon.log.14` can't move over it.
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();
        fs::create_dir_all(dir.join("daemon.log.15/x")).unwrap();
        fs::write(dir.join("daemon.log"), "0\n").unwrap();
        for n in 1..=14 {
            fs::write(dir.join(format!("daemon.log.{n}")), format!("{n}\n")).unwrap();
        }

        assert!(super::rotate_log(dir).is_err());
        // `daemon.log.13` didn't move over the log that stayed.
        assert_eq!(read(dir, "daemon.log.14").as_deref(), Some("14\n"));
    }

    #[test]
    fn a_rotation_carries_on_from_where_a_failed_one_stopped() {
        // Every number is taken, and `daemon.log.7` can't move, however often the rotation is
        // tried.
        let log_dir = tempfile::tempdir().unwrap();
        let dir = log_dir.path();
        fs::write(dir.join("daemon.log"), "0\n").unwrap();
        for n in 1..=15 {
            fs::write(dir.join(format!("daemon.log.{n}")), format!("{n}\n")).unwrap();
        }
        let stuck = dir.join("daemon.log.7");
        let rename = |from: &Path, to: &Path| {
            if from == stuck {
                return Err(io::Error::other("stuck"));
            }
            fs::rename(from, to)
        };

        // Each line the decider sends tries the rotation again. Were every attempt to move the
        // logs, 16 of them would push every log older than the stuck one out.
        for _ in 0..16 {
            assert!(super::rotate_log_with(dir, rename).is_err());
        }

        // The first attempt moved the logs older than the stuck one, `daemon.log.14` over the
        // oldest, and left the stuck one's next number free. No attempt after it moved them again.
        assert_eq!(read(dir, "daemon.log.15").as_deref(), Some("14\n"));
        assert_eq!(read(dir, "daemon.log.8"), None);

        // Once the log can move, the rotation ends as a single one would have: each log one step
        // back, and the oldest alone replaced.
        let mut file = super::rotate_log(dir).unwrap();
        file.write_all(b"fresh\n").unwrap();

        for n in 1..=15 {
            assert_eq!(
                read(dir, &format!("daemon.log.{n}")),
                Some(format!("{}\n", n - 1)),
                "daemon.log.{n}"
            );
        }
        assert_eq!(read(dir, "daemon.log").as_deref(), Some("fresh\n"));
    }

    #[test]
    fn a_line_that_would_cross_the_cap_goes_to_a_fresh_log() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path();
        let log = open(dir).unwrap();
        // `abc` and its newline take 4 bytes, one more than the log has room for.
        log.set_len(8 * 1024 * 1024 - 3).unwrap();
        let redirected = Cell::new(None);
        let mut writer = Writer::new(dir, Some(log), |file: &File| {
            redirected.set(Some(file.metadata()?.ino()));
            Ok(())
        });

        writer.try_write_line(&mut io::sink(), "abc").unwrap();

        // The full log moved aside whole, and the line went to a fresh `daemon.log`, which the
        // writer's handle and the daemon's output now point at.
        let fresh = fs::metadata(dir.join("daemon.log")).unwrap().ino();
        assert_eq!(
            fs::metadata(dir.join("daemon.log.1")).unwrap().len(),
            8 * 1024 * 1024 - 3
        );
        assert_eq!(fs::read_to_string(dir.join("daemon.log")).unwrap(), "abc\n");
        let log = writer.log.as_ref().unwrap();
        assert_eq!(log.metadata().unwrap().ino(), fresh);
        assert_eq!(redirected.get(), Some(fresh));
    }

    #[test]
    fn a_failed_redirect_is_retried_ahead_of_the_next_line() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path();
        let log = open(dir).unwrap();
        // `abc` and its newline take 4 bytes, one more than the log has room for.
        log.set_len(8 * 1024 * 1024 - 3).unwrap();
        // A redirect that counts its calls, fails the first two, then succeeds.
        let calls = Cell::new(0);
        let mut writer = Writer::new(dir, Some(log), |_: &File| {
            calls.set(calls.get() + 1);
            if calls.get() <= 2 {
                return Err(io::Error::other("stuck"));
            }
            Ok(())
        });

        // `abc` rotates the log, and each of the other lines fits in the fresh one.
        for line in ["abc", "def", "ghi", "jkl"] {
            let written = writer.try_write_line(&mut io::sink(), line);
            assert!(written.is_ok(), "{line}: {written:?}");
        }

        // Once after the rotation, then ahead of `def` and of `ghi`. The third call succeeds,
        // so `jkl` needs none.
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn a_run_of_failed_redirects_is_reported_once() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path();
        let log = open(dir).unwrap();
        // `abc` and its newline take 4 bytes, one more than the log has room for.
        log.set_len(8 * 1024 * 1024 - 3).unwrap();
        // A redirect that never succeeds.
        let mut writer = Writer::new(dir, Some(log), |_: &File| Err(io::Error::other("stuck")));

        writer.write_line(&mut io::sink(), "abc");
        writer.write_line(&mut io::sink(), "def");

        // The fresh log holds the report of the first failure, then both lines. The second
        // failure adds no report, and the retry moves no log.
        assert_eq!(
            read(dir, "daemon.log").as_deref(),
            Some("egress: the daemon's own lines still go to the previous log: stuck\nabc\ndef\n")
        );
    }

    #[test]
    fn a_line_dropped_by_a_failed_rotation_is_reported() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path();
        let log = open(dir).unwrap();
        // `abc` and its newline take 4 bytes, one more than the log has room for.
        log.set_len(8 * 1024 * 1024 - 3).unwrap();
        // A directory where the fresh log would open, so the rotation fails.
        fs::create_dir(dir.join("daemon.log.new")).unwrap();
        let mut writer = Writer::new(dir, Some(log), |_: &File| Ok(()));
        let mut out = Vec::new();

        writer.write_line(&mut out, "abc");

        // The report names the log that could not open.
        let reported = String::from_utf8(out).unwrap();
        assert!(reported.contains("daemon.log.new"), "{reported}");
    }

    #[test]
    fn a_run_of_dropped_lines_is_reported_once() {
        // What `try_write_line` returns when a rotation fails: the crate's error inside an
        // `io::Error`, with the operating system's error as its cause.
        let failed = || {
            let cause = io::Error::other("too many open files");
            let path = Path::new("/state/daemon.log.new");
            Err(io::Error::other(Error::io(path, cause)))
        };
        let mut writer = Writer::new(Path::new("/state"), None, |_: &File| Ok(()));
        let mut out = Vec::new();

        writer.report_dropped(failed(), &mut out);
        writer.report_dropped(failed(), &mut out);
        // A line that gets through ends the run, so the next line dropped is reported again.
        writer.report_dropped(Ok(()), &mut out);
        writer.report_dropped(failed(), &mut out);

        let line = "egress: lines dropped: \
                    failed to access /state/daemon.log.new: too many open files\n";
        assert_eq!(String::from_utf8(out).unwrap(), format!("{line}{line}"));
    }

    /// The contents of the file `name` in `dir`, or `None` when there is no such file, so a
    /// missing log fails an assertion instead of an `unwrap`.
    fn read(dir: &Path, name: &str) -> Option<String> {
        fs::read_to_string(dir.join(name)).ok()
    }
}
