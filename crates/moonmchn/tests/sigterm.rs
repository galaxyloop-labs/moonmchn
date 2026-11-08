//! End-to-end tests for Moon MCHN daemon SIGTERM handling.

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

use moonmchn::Error;
use moonmchn::daemon::client::Client;
use moonmchn::machine::Spec;
use rustix::process;

mod common;

#[test]
fn shuts_down_on_sigterm() {
    let dir = common::state_dir();
    let t = common::start_daemon(dir.path().to_path_buf());
    let client = Client::new(dir.path().to_path_buf());

    let _status = common::wait_for_status(&client).unwrap();
    client.machine_start("alpha", Spec::default()).unwrap();

    process::kill_process(process::getpid(), process::Signal::TERM).unwrap();

    let (handle_result, output) = t.join().unwrap();

    assert!(handle_result.is_ok());
    // `SIGTERM` tears the machines down, as a forced shutdown does.
    let output = String::from_utf8(output).unwrap();
    assert!(
        output.ends_with("\nstopped machine alpha\nstopped\n"),
        "{output}"
    );
    assert!(!common::marker(dir.path(), "alpha").exists());
    // The machine was told why it stopped.
    common::assert_announced("alpha", "the Moon MCHN daemon is stopping");

    assert!(matches!(client.status(), Err(Error::DaemonNotRunning)));
}
