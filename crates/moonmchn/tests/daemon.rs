//! End-to-end tests for the Moon MCHN daemon library.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "Tests may panic and print diagnostics"
)]

use std::fs::File;
use std::io::BufReader;
use std::io::prelude::*;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use moonmchn::daemon::client::Client;
use moonmchn::daemon::{Mode, serve_in};
use moonmchn::{Error, VERSION};

mod common;
use crate::common::FakeEngine;

#[test]
fn answers_status() {
    let dir = common::state_dir();
    common::start_daemon(dir.path().to_path_buf());
    let client = Client::new(dir.path().to_path_buf());

    let status = common::wait_for_status(&client).unwrap();

    assert_eq!(status.version, VERSION);
    assert_eq!(status.pid, std::process::id());
}

#[test]
fn creates_its_state_directory() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("state");
    common::start_daemon(dir.clone());
    let client = Client::new(dir.clone());

    common::wait_for_status(&client).unwrap();

    // The modes are the state directory module's to check.
    assert!(dir.join("machines").is_dir());
}

#[test]
fn errors_on_malformed_message() {
    let dir = common::state_dir();
    common::start_daemon(dir.path().to_path_buf());
    let socket_path = dir.path().join("daemon.sock");
    let client = Client::new(dir.path().to_path_buf());

    let _status = common::wait_for_status(&client).unwrap();

    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream.write_all(b"nope\n").unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert!(response.starts_with(r#"{"error":{"message":"invalid request: "#));
    assert!(response.ends_with('\n'));
}

#[test]
fn rejects_an_oversized_request() {
    let dir = common::state_dir();
    common::start_daemon(dir.path().to_path_buf());
    let socket_path = dir.path().join("daemon.sock");
    let client = Client::new(dir.path().to_path_buf());

    let _status = common::wait_for_status(&client).unwrap();

    // No newline, and the stream stays open: only the 64 KiB size cap ends the request.
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(&b"x".repeat(64 * 1024)).unwrap();

    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response).unwrap();

    assert!(response.starts_with(r#"{"error":{"message":"invalid request: "#));
    assert!(client.status().is_ok());
}

#[test]
fn drops_an_idle_connection() {
    let dir = common::state_dir();
    common::start_daemon(dir.path().to_path_buf());
    let socket_path = dir.path().join("daemon.sock");
    let client = Client::new(dir.path().to_path_buf());

    let _status = common::wait_for_status(&client).unwrap();

    // The read timeout outlasts the daemon's 5 second deadline, so EOF means the
    // daemon closed the connection.
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(7)))
        .unwrap();

    let mut response = String::new();
    assert_eq!(stream.read_to_string(&mut response).unwrap(), 0);
    assert!(client.status().is_ok());
}

#[test]
fn shutdowns_on_request() {
    let dir = common::state_dir();
    let t = common::start_daemon(dir.path().to_path_buf());
    // The daemon works in its state directory's resolved path, which is where it reports its
    // socket: macOS's `/var` is a link to `/private/var`.
    let socket_path = dir.path().canonicalize().unwrap().join("daemon.sock");
    let client = Client::new(dir.path().to_path_buf());

    let _status = common::wait_for_status(&client).unwrap();

    client.shutdown(false).unwrap();

    // The daemon released its lock before `shutdown` returned.
    let pid_file = File::open(dir.path().join("daemon.pid")).unwrap();
    assert!(pid_file.try_lock().is_ok());
    drop(pid_file);

    let (handle_result, output) = t.join().unwrap();

    assert!(handle_result.is_ok());
    assert_eq!(
        output,
        format!("listening on {}\nstopped\n", socket_path.display()).as_bytes()
    );

    // The daemon should be gone now.
    assert!(matches!(client.status(), Err(Error::DaemonNotRunning)));
}

#[test]
fn refuses_a_second_daemon() {
    let dir = common::state_dir();
    common::start_daemon(dir.path().to_path_buf());
    let client = Client::new(dir.path().to_path_buf());

    let _status = common::wait_for_status(&client).unwrap();

    let mut out = Vec::new();
    // This daemon fails before it serves, so its teardown bound never comes into play.
    let err = serve_in::<FakeEngine>(
        dir.path(),
        Mode::Foreground,
        &mut out,
        Duration::from_secs(60),
    )
    .unwrap_err();

    assert!(matches!(err, Error::DaemonAlreadyRunning));
    assert_eq!(String::from_utf8(out).unwrap(), "");
}
