//! End-to-end tests for Spacecraft daemon SIGTERM handling.

use rustix::process;
use spacecraft::Error;
use spacecraft::daemon::client::Client;

mod common;

#[test]
fn shuts_down_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let t = common::start_daemon(dir.path().to_path_buf());
    let client = Client::new(dir.path().to_path_buf());

    let _status = common::wait_for_status(&client).unwrap();

    process::kill_process(process::getpid(), process::Signal::TERM).unwrap();

    let (handle_result, output) = t.join().unwrap();

    assert!(handle_result.is_ok());
    assert!(output.ends_with(b"stopped\n"));

    assert!(matches!(client.status(), Err(Error::DaemonNotRunning)));
}
