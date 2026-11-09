//! Bounded subprocess probes with recoverable errors and limits on output and time.
//!
//! Platform probes run tools such as `nvidia-smi` and `system_profiler` through
//! [`output`], which guarantees that a hung, chatty, or failing command cannot
//! block setup, exhaust memory, or leave a stray process behind.

use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::ByteSize;

/// Upper bound on captured stdout. Probe reports are a few KiB at most, so
/// larger output indicates the wrong program or a runaway and is rejected.
const MAX_OUTPUT_SIZE: ByteSize = ByteSize::from_bytes(64 * 1024);
/// Pause between non-blocking status checks; `Child::wait` has no timeout.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Captures stdout from a successful command within the size and time limits.
///
/// The command runs with `LC_ALL=C` for stable, parseable output, with no stdin
/// and discarded stderr. Callers may ignore errors when a probe is only a
/// best-effort hint.
///
/// # Errors
///
/// Returns an error if the program cannot be spawned, exits with a failure
/// status, exceeds `timeout` (`ErrorKind::TimedOut`), produces more than
/// `MAX_OUTPUT_SIZE` bytes (`ErrorKind::InvalidData`), or its stdout cannot be
/// read. On every error the child is killed and reaped before returning.
pub(super) fn output(program: &str, args: &[&str], timeout: Duration) -> io::Result<Vec<u8>> {
    let start = Instant::now();
    // Use a stable locale for parsing and suppress stdin and diagnostics.
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let result = collect_output(&mut child, start, timeout);
    if result.is_err() {
        // Dropping Child does not stop or reap it. Clean up on every error,
        // including a missing stdout pipe or failure to start the reader.
        drop(child.kill());
        drop(child.wait());
    }
    result
}

/// Reads stdout to completion while waiting for the child to exit successfully.
///
/// A helper thread drains the pipe so a full buffer cannot block the child.
/// The child is never killed here; [`output`] cleans up on error.
///
/// # Errors
///
/// Returns an error if stdout was not piped, the reader thread cannot start,
/// the child fails or times out, or the output exceeds `MAX_OUTPUT_SIZE`.
fn collect_output(child: &mut Child, start: Instant, timeout: Duration) -> io::Result<Vec<u8>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("probe stdout was not piped"))?;
    // The reader sends exactly one result, even if the caller has stopped waiting.
    let (sender, receiver) = mpsc::sync_channel(1);

    // Drain stdout concurrently so a full pipe cannot block the child.
    // Read one extra byte to distinguish output at the limit from oversized output.
    thread::Builder::new().spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(MAX_OUTPUT_SIZE.as_bytes().saturating_add(1))
            .read_to_end(&mut bytes);
        drop(sender.send(result.map(|_| bytes)));
    })?;

    wait_for_success(child, start, timeout)?;
    // Share the original time budget with the child wait. Do not join the reader:
    // a descendant may keep stdout open even after the direct child has exited.
    // The first `?` covers the channel, the second the read itself.
    let bytes = receiver
        .recv_timeout(timeout.saturating_sub(start.elapsed()))
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                io::Error::new(io::ErrorKind::TimedOut, "probe stdout timed out")
            }
            mpsc::RecvTimeoutError::Disconnected => io::Error::other("probe stdout reader stopped"),
        })??;
    if ByteSize::from_bytes(bytes.len() as u64) > MAX_OUTPUT_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "probe stdout exceeded the size limit",
        ));
    }
    Ok(bytes)
}

/// Polls the child until it exits or the deadline measured from `start` passes.
///
/// # Errors
///
/// Returns an error if the child exits with a failure status, is still running
/// at the deadline (`ErrorKind::TimedOut`), or its status cannot be queried.
fn wait_for_success(child: &mut Child, start: Instant, timeout: Duration) -> io::Result<()> {
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("probe command failed: {status}")))
            };
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "probe command timed out",
            ));
        }
        // Never sleep past the deadline.
        thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{MAX_OUTPUT_SIZE, collect_output, output};
    use std::io::ErrorKind;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const TEST_TIMEOUT: Duration = Duration::from_secs(1);
    const SHORT_TIMEOUT: Duration = Duration::from_millis(50);
    const COMPLETION_DEADLINE: Duration = Duration::from_secs(2);
    const SLOW_COMMAND_DURATION: Duration = Duration::from_secs(5);

    #[test]
    fn captures_successful_output_but_not_stderr() {
        assert_eq!(
            output(
                "/bin/sh",
                &["-c", "printf '8.6, Default\\n'; printf diagnostic >&2"],
                TEST_TIMEOUT,
            )
            .unwrap(),
            b"8.6, Default\n"
        );
    }

    #[test]
    fn failed_and_missing_commands_return_errors() {
        let error = output(
            "/bin/sh",
            &["-c", "printf '8.6, Default\\n'; exit 1"],
            TEST_TIMEOUT,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);

        let directory = tempfile::tempdir().unwrap();
        let error = output(
            directory.path().join("missing-probe").to_str().unwrap(),
            &[],
            TEST_TIMEOUT,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn terminates_a_probe_that_does_not_finish() {
        let command = format!("exec sleep {}", SLOW_COMMAND_DURATION.as_secs());
        let start = Instant::now();
        let error = output("/bin/sh", &["-c", &command], SHORT_TIMEOUT).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(start.elapsed() < COMPLETION_DEADLINE);
    }

    #[test]
    fn accepts_empty_output_and_output_up_to_the_limit() {
        for size in [
            0,
            MAX_OUTPUT_SIZE.as_bytes() - 1,
            MAX_OUTPUT_SIZE.as_bytes(),
        ] {
            let command = format!("printf '%{size}s' ''");
            let bytes = output("/bin/sh", &["-c", &command], TEST_TIMEOUT).unwrap();
            assert_eq!(bytes, vec![b' '; usize::try_from(size).unwrap()]);
        }
    }

    #[test]
    fn rejects_output_one_byte_over_the_limit() {
        let size = MAX_OUTPUT_SIZE.as_bytes() + 1;
        let command = format!("printf '%{size}s' ''");
        let error = output("/bin/sh", &["-c", &command], TEST_TIMEOUT).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn missing_stdout_returns_an_error_instead_of_panicking() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let result = collect_output(&mut child, Instant::now(), TEST_TIMEOUT);
        child.wait().unwrap();
        let error = result.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert_eq!(error.to_string(), "probe stdout was not piped");
    }

    #[test]
    fn times_out_when_a_descendant_keeps_stdout_open() {
        // The shell has exited before the deadline starts, however slowly it
        // started, so only the reader can time out: the short-lived descendant
        // holds the pipe open beyond the deadline and then exits on its own.
        let command = format!("sleep {} &", TEST_TIMEOUT.as_secs());
        let mut child = Command::new("/bin/sh")
            .args(["-c", &command])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.wait().unwrap();
        let start = Instant::now();
        let error = collect_output(&mut child, start, SHORT_TIMEOUT).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "probe stdout timed out");
        assert!(start.elapsed() < COMPLETION_DEADLINE);
    }
}
