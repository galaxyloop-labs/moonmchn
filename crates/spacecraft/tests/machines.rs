//! End-to-end tests for the machines the Spacecraft daemon starts, stops and reports.

use spacecraft::config::{Mount, Resources};
use spacecraft::daemon::api::{MachineStart, MachineStatus, MachineStop, Request};
use spacecraft::daemon::client::Client;
use spacecraft::machine::{Config, State, Status};
use spacecraft::{Error, Result};
use std::fmt::Debug;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

mod common;

/// What a marker written by hand holds: any valid configuration.
const MARKER: &str = "[resources]\ncpus = 1\n";

#[test]
fn starts_and_reports_a_machine() {
    let (dir, client) = daemon();

    let started = client.machine_start("alpha", config()).unwrap();

    assert_eq!(started, MachineStart::Started(running("alpha")));
    assert_eq!(
        client.machine_status("alpha").unwrap(),
        MachineStatus {
            status: running("alpha"),
            config: Some(config()),
        }
    );
    assert_eq!(
        read_marker(dir.path(), "alpha"),
        config().to_toml().unwrap()
    );
}

#[test]
fn start_is_idempotent() {
    let (dir, client) = daemon();
    client.machine_start("alpha", config()).unwrap();

    let again = client.machine_start("alpha", config()).unwrap();

    assert_eq!(again, MachineStart::AlreadyRunning(running("alpha")));
    assert_eq!(
        fs::read_dir(dir.path().join("machines")).unwrap().count(),
        1
    );
}

#[test]
fn start_refuses_a_different_configuration() {
    let (dir, client) = daemon();
    client.machine_start("alpha", config()).unwrap();

    let other = Config {
        resources: Resources {
            cpus: Some(1),
            ..Resources::default()
        },
        ..Config::default()
    };
    let message = error_message(client.machine_start("alpha", other));

    assert_eq!(
        message,
        "machine alpha is running with a different configuration; run `spacecraft machine stop alpha` to discard it, then start it again"
    );
    assert_eq!(
        read_marker(dir.path(), "alpha"),
        config().to_toml().unwrap()
    );
}

#[test]
fn status_of_an_unknown_machine_fails() {
    let (_dir, client) = daemon();

    let message = error_message(client.machine_status("nope"));

    assert_eq!(message, "machine nope not found");
}

#[test]
fn a_boot_failure_leaves_nothing_behind() {
    let (dir, client) = daemon();

    let message = error_message(client.machine_start("boom", config()));

    assert_eq!(message, "the engine failed: boom");
    assert!(!common::marker(dir.path(), "boom").exists());
    assert_eq!(
        error_message(client.machine_status("boom")),
        "machine boom not found"
    );
}

#[test]
fn concurrent_operations_on_one_name_are_refused() {
    let (dir, client) = daemon();
    let other = Client::new(dir.path().to_path_buf());
    let boot = thread::spawn(move || other.machine_start("slow", config()));
    // The fake boots `slow` in 200 ms, so 50 ms in, the boot is still in flight.
    thread::sleep(Duration::from_millis(50));

    let report = client.machine_status("slow").unwrap();
    assert_eq!(report.status.state, State::Starting);
    assert_eq!(report.status.pid, None);
    assert_eq!(report.config, Some(config()));
    assert_eq!(
        error_message(client.machine_start("slow", config())),
        "machine slow is starting"
    );

    let started = boot.join().unwrap().unwrap();
    assert_eq!(started, MachineStart::Started(running("slow")));
}

#[test]
fn start_heals_a_dead_machine() {
    let (dir, client) = daemon();
    write_marker(dir.path(), "dead");

    let started = client.machine_start("dead", config()).unwrap();

    // The fake reports `dead` as failed even after a boot, so only the variant counts.
    assert!(matches!(started, MachineStart::Started(_)));
    assert_eq!(read_marker(dir.path(), "dead"), config().to_toml().unwrap());
}

#[test]
fn start_refuses_a_machine_the_engine_cannot_report() {
    let (dir, client) = daemon();
    write_marker(dir.path(), "ghost");

    let report = client.machine_status("ghost").unwrap();
    assert_eq!(report.status.state, State::Unknown);
    assert_eq!(report.status.pid, None);

    assert_eq!(
        error_message(client.machine_start("ghost", config())),
        "the engine cannot report machine ghost; retry, or run `spacecraft machine stop ghost` to discard it"
    );
    assert_eq!(read_marker(dir.path(), "ghost"), MARKER);
}

#[test]
fn start_resumes_a_paused_machine() {
    let (dir, client) = daemon();
    // The marker holds `config()`, so the configurations match. Its modification time moves
    // into the past, so a marker written anew would show. `set_modified` works on an open file;
    // opening it for writing without `truncate` keeps its content.
    let marker = common::marker(dir.path(), "frozen");
    fs::write(&marker, config().to_toml().unwrap()).unwrap();
    let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    fs::File::options()
        .write(true)
        .open(&marker)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();

    let started = client.machine_start("frozen", config());

    // The fake has no memory, so it reports `frozen` paused even after the resume: only the
    // variant and the name count.
    assert!(
        matches!(&started, Ok(MachineStart::Started(status)) if status.name == "frozen"),
        "{started:?}"
    );
    // The `Failed` path would have deleted the machine and written a new marker, with the same
    // content and a new modification time. An untouched marker plus `Started` shows that
    // `start` resumed the machine.
    assert_eq!(
        read_marker(dir.path(), "frozen"),
        config().to_toml().unwrap()
    );
    assert_eq!(fs::metadata(&marker).unwrap().modified().unwrap(), long_ago);
}

#[test]
fn start_refuses_a_paused_machine_with_a_different_configuration() {
    let (dir, client) = daemon();
    // `MARKER` sets `cpus = 1`, and `config()` sets `cpus = 2`.
    write_marker(dir.path(), "frozen");

    assert_eq!(
        error_message(client.machine_start("frozen", config())),
        "machine frozen is running with a different configuration; run `spacecraft machine stop frozen` to discard it, then start it again"
    );
    assert_eq!(read_marker(dir.path(), "frozen"), MARKER);
}

#[test]
fn an_inspection_failure_destroys_nothing() {
    let (dir, client) = daemon();
    write_marker(dir.path(), "flaky");

    assert_eq!(
        error_message(client.machine_status("flaky")),
        "the engine failed: flaky"
    );
    assert_eq!(
        error_message(client.machine_start("flaky", config())),
        "the engine failed: flaky"
    );
    assert_eq!(read_marker(dir.path(), "flaky"), MARKER);
}

#[test]
fn a_failed_rollback_keeps_the_record() {
    let (dir, client) = daemon();

    assert_eq!(
        error_message(client.machine_start("wreck", config())),
        "the engine failed to boot wreck: boom; deleting it failed too: stuck; run `spacecraft machine stop wreck`"
    );
    assert_eq!(
        read_marker(dir.path(), "wreck"),
        config().to_toml().unwrap()
    );
    assert_eq!(
        client.machine_status("wreck").unwrap().status.state,
        State::Failed
    );

    // `stop` reaches the machine through the marker the failed rollback kept.
    assert_eq!(client.machine_stop("wreck").unwrap(), MachineStop::Stopped);
    assert!(!common::marker(dir.path(), "wreck").exists());
}

#[test]
fn rejects_invalid_names() {
    let (dir, client) = daemon();

    assert_eq!(
        error_message(client.machine_start("a/b", config())),
        r#"invalid machine name "a/b": use lowercase letters, digits, '_', '.' and '-', starting with a letter or digit"#
    );
    assert_eq!(
        fs::read_dir(dir.path().join("machines")).unwrap().count(),
        0
    );
}

#[test]
fn rejects_case_aliases() {
    let (dir, client) = daemon();
    client.machine_start("alpha", config()).unwrap();

    // A different configuration, so an overwritten marker would show.
    assert_eq!(
        error_message(client.machine_start("Alpha", Config::default())),
        r#"invalid machine name "Alpha": use lowercase letters, digits, '_', '.' and '-', starting with a letter or digit"#
    );
    assert_eq!(
        read_marker(dir.path(), "alpha"),
        config().to_toml().unwrap()
    );
    assert_eq!(
        client.machine_status("alpha").unwrap().status,
        running("alpha")
    );
}

#[test]
fn start_rejects_a_relative_mount_source() {
    let (dir, client) = daemon();
    let relative = Config {
        mounts: vec![mount("./data", "/data")],
        ..config()
    };

    let message = error_message(client.machine_start("alpha", relative));

    assert_eq!(
        message,
        r#"invalid mount source "./data": a host directory must be an absolute path"#
    );
    assert!(!common::marker(dir.path(), "alpha").exists());
    let list = client.machine_list().unwrap();
    assert!(list.is_empty(), "{list:?}");

    // `http://` is none of the engine's remote schemes, so the engine would take this source
    // for a relative host path.
    let unknown_scheme = Config {
        mounts: vec![mount("http://host/x", "/data")],
        ..config()
    };
    assert_eq!(
        error_message(client.machine_start("alpha", unknown_scheme)),
        r#"invalid mount source "http://host/x": a host directory must be an absolute path"#
    );

    // The rule's boundary: an absolute host directory and a remote volume both pass. The fake
    // engine ignores the configuration, so the start succeeds.
    let valid = Config {
        mounts: vec![
            mount("/host/data", "/data"),
            mount("s3://bucket/prefix", "/models"),
        ],
        ..config()
    };
    assert_eq!(
        client.machine_start("alpha", valid).unwrap(),
        MachineStart::Started(running("alpha"))
    );
}

#[test]
fn an_unwritable_marker_leaves_nothing_in_flight() {
    let (_dir, client) = daemon();
    // Longer than any file name the filesystem accepts.
    let name = "a".repeat(300);

    let message = error_message(client.machine_start(&name, config()));

    assert!(message.contains("failed to access"));
    assert_eq!(
        error_message(client.machine_status(&name)),
        format!("machine {name} not found")
    );
}

#[test]
fn accepts_a_large_configuration() {
    let (_dir, client) = daemon();
    // 30 mounts of 60-character paths make a request over 4 KiB.
    let large = Config {
        mounts: (0..30)
            .map(|i| Mount {
                source: format!("/host/{i:054}"),
                target: format!("/guest/{i:053}"),
                read_only: false,
                staged: false,
            })
            .collect(),
        ..config()
    };

    let started = client.machine_start("large", large.clone()).unwrap();

    assert_eq!(started, MachineStart::Started(running("large")));
    assert_eq!(client.machine_status("large").unwrap().config, Some(large));
}

#[test]
fn a_departed_client_does_not_corrupt_the_record() {
    let (dir, client) = daemon();
    let request = Request::MachineStart {
        name: "slow".to_string(),
        config: config(),
    };

    // Hang up right after the request, long before the fake's 200 ms boot ends.
    let mut stream = UnixStream::connect(dir.path().join("daemon.sock")).unwrap();
    writeln!(stream, "{}", serde_json::to_string(&request).unwrap()).unwrap();
    drop(stream);
    thread::sleep(Duration::from_millis(300));

    assert_eq!(
        client.machine_status("slow").unwrap(),
        MachineStatus {
            status: running("slow"),
            config: Some(config()),
        }
    );
}

#[test]
fn stops_a_running_machine() {
    let (dir, client) = daemon();
    client.machine_start("alpha", config()).unwrap();

    let stopped = client.machine_stop("alpha").unwrap();

    assert_eq!(stopped, MachineStop::Stopped);
    assert!(!common::marker(dir.path(), "alpha").exists());
    assert_eq!(
        error_message(client.machine_status("alpha")),
        "machine alpha not found"
    );
    // A second stop finds no marker: there is nothing left to stop.
    assert_eq!(
        client.machine_stop("alpha").unwrap(),
        MachineStop::NotRunning
    );
}

#[test]
fn stop_rejects_invalid_names() {
    let (_dir, client) = daemon();

    assert_eq!(
        error_message(client.machine_stop("../x")),
        r#"invalid machine name "../x": use lowercase letters, digits, '_', '.' and '-', starting with a letter or digit"#
    );
}

#[test]
fn a_failed_stop_keeps_the_record() {
    let (dir, client) = daemon();
    client.machine_start("stubborn", config()).unwrap();

    assert_eq!(
        error_message(client.machine_stop("stubborn")),
        "the engine failed: stubborn"
    );
    // The machine may still exist, so its marker stays and `status` still reaches it.
    assert_eq!(
        read_marker(dir.path(), "stubborn"),
        config().to_toml().unwrap()
    );
    assert_eq!(
        client.machine_status("stubborn").unwrap().status,
        running("stubborn")
    );
}

#[test]
fn stopping_a_machine_the_engine_lost_cleans_up() {
    let (dir, client) = daemon();
    // The fake's engine has lost `ghost`: its `stop` fails with `MachineNotFound`.
    write_marker(dir.path(), "ghost");

    let stopped = client.machine_stop("ghost").unwrap();

    assert_eq!(stopped, MachineStop::NotRunning);
    assert!(!common::marker(dir.path(), "ghost").exists());
}

#[test]
fn stop_during_a_boot_is_refused() {
    let (dir, client) = daemon();
    let other = Client::new(dir.path().to_path_buf());
    let boot = thread::spawn(move || other.machine_start("slow", config()));
    // The fake boots `slow` in 200 ms, so 50 ms in, the boot is still in flight.
    thread::sleep(Duration::from_millis(50));

    assert_eq!(
        error_message(client.machine_stop("slow")),
        "machine slow is starting"
    );

    let started = boot.join().unwrap().unwrap();
    assert_eq!(started, MachineStart::Started(running("slow")));
    assert_eq!(client.machine_stop("slow").unwrap(), MachineStop::Stopped);
}

#[test]
fn a_corrupt_record_fails_status_but_not_stop() {
    let (dir, client) = daemon();
    client.machine_start("alpha", config()).unwrap();
    fs::write(common::marker(dir.path(), "alpha"), "cpus = [").unwrap();

    let message = error_message(client.machine_status("alpha"));
    assert!(message.starts_with("failed to parse "), "{message}");
    assert!(message.contains("machines/alpha"), "{message}");

    // `stop` never reads the record, so a corrupt one cannot block it.
    assert_eq!(client.machine_stop("alpha").unwrap(), MachineStop::Stopped);
    assert!(!common::marker(dir.path(), "alpha").exists());
}

#[test]
fn list_is_empty_without_machines() {
    let (_dir, client) = daemon();

    let list = client.machine_list().unwrap();

    assert!(list.is_empty(), "{list:?}");
}

#[test]
fn lists_machines_sorted_by_name() {
    let (_dir, client) = daemon();
    client.machine_start("beta", config()).unwrap();
    client.machine_start("alpha", config()).unwrap();

    let list = client.machine_list().unwrap();

    assert_eq!(list, vec![running("alpha"), running("beta")]);
}

#[test]
fn list_reports_in_flight_dead_and_unknown_machines() {
    let (dir, client) = daemon();
    // The fake reports `dead` as failed, `frozen` as paused and `ghost` as unknown, and fails to
    // report `flaky`.
    for name in ["dead", "flaky", "frozen", "ghost"] {
        write_marker(dir.path(), name);
    }
    let other = Client::new(dir.path().to_path_buf());
    let boot = thread::spawn(move || other.machine_start("slow", config()));
    // The fake boots `slow` in 200 ms, so 50 ms in, the boot is still in flight.
    thread::sleep(Duration::from_millis(50));

    let list = client.machine_list().unwrap();

    let row = |name: &str, state| Status {
        name: name.to_string(),
        state,
        pid: None,
    };
    // `flaky`'s engine error becomes an `unknown` row, so the other rows still show.
    assert_eq!(
        list,
        vec![
            row("dead", State::Failed),
            row("flaky", State::Unknown),
            row("frozen", State::Paused),
            row("ghost", State::Unknown),
            row("slow", State::Starting),
        ]
    );

    let started = boot.join().unwrap().unwrap();
    assert_eq!(started, MachineStart::Started(running("slow")));
}

#[test]
fn list_skips_foreign_entries() {
    let (dir, client) = daemon();
    // `start` refuses these names, so no machine owns these files.
    let foreign = [".hidden", "bad name", "Alpha"];
    for name in foreign {
        write_marker(dir.path(), name);
    }

    let list = client.machine_list().unwrap();

    assert!(list.is_empty(), "{list:?}");
    for name in foreign {
        assert!(common::marker(dir.path(), name).exists(), "{name}");
    }
}

#[test]
fn records_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    // Written before the daemon starts: the new daemon learns of `alpha` from the disk alone.
    write_marker(dir.path(), "alpha");
    common::start_daemon(dir.path().to_path_buf());
    let client = Client::new(dir.path().to_path_buf());
    common::wait_for_status(&client).unwrap();

    let list = client.machine_list().unwrap();

    assert_eq!(list, vec![running("alpha")]);
    assert_eq!(client.machine_stop("alpha").unwrap(), MachineStop::Stopped);
    assert!(!common::marker(dir.path(), "alpha").exists());
}

#[test]
fn shutdown_is_refused_while_machines_run() {
    let (_dir, client) = daemon();
    client.machine_start("alpha", config()).unwrap();

    let message = error_message(client.shutdown(false));

    assert_eq!(message, "1 running machine(s); pass `--force` to stop them");
    // The daemon stays up, and keeps admitting operations.
    assert!(client.status().is_ok());
    assert_eq!(
        client.machine_status("alpha").unwrap().status,
        running("alpha")
    );
    assert_eq!(
        client.machine_start("beta", config()).unwrap(),
        MachineStart::Started(running("beta"))
    );
}

#[test]
fn shutdown_is_refused_while_a_boot_is_in_flight() {
    let (dir, client) = daemon();
    let other = Client::new(dir.path().to_path_buf());
    let boot = thread::spawn(move || other.machine_start("slow", config()));
    // The fake boots `slow` in 200 ms, so 50 ms in, the boot is still in flight.
    thread::sleep(Duration::from_millis(50));

    assert_eq!(
        error_message(client.shutdown(false)),
        "1 running machine(s); pass `--force` to stop them"
    );

    let started = boot.join().unwrap().unwrap();
    assert_eq!(started, MachineStart::Started(running("slow")));
}

#[test]
fn forced_shutdown_tears_machines_down() {
    let (dir, client, daemon) = daemon_with_thread();
    client.machine_start("alpha", config()).unwrap();
    client.machine_start("beta", config()).unwrap();

    client.shutdown(true).unwrap();

    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("\nstopped machine alpha\n"), "{out}");
    assert!(out.contains("\nstopped machine beta\n"), "{out}");
    assert!(out.ends_with("\nstopped\n"), "{out}");
    assert!(!common::marker(dir.path(), "alpha").exists());
    assert!(!common::marker(dir.path(), "beta").exists());
}

#[test]
fn forced_shutdown_reports_a_stubborn_machine() {
    let (dir, client, daemon) = daemon_with_thread();
    client.machine_start("stubborn", config()).unwrap();

    client.shutdown(true).unwrap();

    // Joining proves the daemon exited, even though one machine refused to stop.
    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("\nfailed to stop machine stubborn: the engine failed: stubborn\n"),
        "{out}"
    );
    // The machine may still exist, so its marker stays for a later `stop`.
    assert_eq!(
        read_marker(dir.path(), "stubborn"),
        config().to_toml().unwrap()
    );
}

#[test]
fn forced_shutdown_waits_for_a_boot_in_flight() {
    let (dir, client, daemon) = daemon_with_thread();
    let other = Client::new(dir.path().to_path_buf());
    let boot = thread::spawn(move || other.machine_start("slow", config()));
    // The fake boots `slow` in 200 ms, so 50 ms in, the boot is claimed before the shutdown
    // closes admission.
    thread::sleep(Duration::from_millis(50));

    client.shutdown(true).unwrap();

    // The daemon let the boot finish and reply before tearing the machine down.
    let started = boot.join().unwrap().unwrap();
    assert_eq!(started, MachineStart::Started(running("slow")));
    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("\nstopped machine slow\n"), "{out}");
    assert!(!common::marker(dir.path(), "slow").exists());
}

#[test]
fn a_mutation_after_shutdown_is_refused() {
    let (dir, _client, daemon) = daemon_with_thread();
    let socket = dir.path().join("daemon.sock");
    // `a` connects first, so the daemon accepts it before the shutdown ends the accept loop.
    let mut a = UnixStream::connect(&socket).unwrap();
    let mut b = UnixStream::connect(&socket).unwrap();

    let shutdown = Request::Shutdown { force: true };
    writeln!(b, "{}", serde_json::to_string(&shutdown).unwrap()).unwrap();
    let mut reply = String::new();
    BufReader::new(&b).read_line(&mut reply).unwrap();
    assert_eq!(reply, "{\"done\":null}\n");

    // The daemon waits for `a`'s request before tearing down, and refuses it.
    let start = Request::MachineStart {
        name: "alpha".to_string(),
        config: config(),
    };
    writeln!(a, "{}", serde_json::to_string(&start).unwrap()).unwrap();
    let mut reply = String::new();
    BufReader::new(&a).read_line(&mut reply).unwrap();
    assert_eq!(
        reply,
        "{\"error\":{\"message\":\"the daemon is stopping\"}}\n"
    );

    let (result, _) = daemon.join().unwrap();
    result.unwrap();
    assert!(!common::marker(dir.path(), "alpha").exists());
}

#[test]
fn a_client_that_never_reads_its_reply_does_not_block_shutdown() {
    let (dir, client, daemon) = daemon_with_thread();
    // The name is invalid, and the error message echoes it, so the reply is about 60 KB. A
    // reply larger than the socket's send buffer stalls the daemon's write until the client
    // reads, and this client never does. The buffer is 8 KiB for unix sockets on macOS. On
    // Linux it is larger, so the reply fits and the test passes without exercising the stall.
    let request = Request::MachineStart {
        name: "A".repeat(60_000),
        config: config(),
    };

    // `stream` stays open until the test returns, and nothing ever reads from it.
    let mut stream = UnixStream::connect(dir.path().join("daemon.sock")).unwrap();
    writeln!(stream, "{}", serde_json::to_string(&request).unwrap()).unwrap();

    // A forced shutdown waits up to 5 minutes for the daemon to exit, well past the daemon's
    // 5-second reply deadline.
    client.shutdown(true).unwrap();

    // Joining proves the daemon exited: the teardown got past the stalled reply.
    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.ends_with("\nstopped\n"), "{out}");
    assert!(matches!(client.status(), Err(Error::DaemonNotRunning)));
}

#[test]
fn a_hung_engine_call_does_not_block_shutdown() {
    let (dir, client, daemon) = daemon_with_teardown(Duration::from_millis(200));
    client.machine_start("alpha", config()).unwrap();
    // The fake never answers about `hung`, so the engine hangs on any call for this machine.
    write_marker(dir.path(), "hung");
    let other = Client::new(dir.path().to_path_buf());
    // The request hangs in the engine. The thread is never joined: it ends when the daemon
    // closes the connection.
    thread::spawn(move || other.machine_status("hung"));
    // 50 ms in, the request is in flight before the shutdown closes admission.
    thread::sleep(Duration::from_millis(50));

    force_shutdown(dir.path());

    // Joining proves the daemon exited, although two threads are still stuck in the engine: the
    // request's and the stop's.
    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("\ngave up waiting for 1 request(s) after 200ms\n"),
        "{out}"
    );
    assert!(out.contains("\nstopped machine alpha\n"), "{out}");
    assert!(
        out.contains("\nfailed to stop machine hung: machine hung is stopping\n"),
        "{out}"
    );
    assert!(out.ends_with("\nstopped\n"), "{out}");
    assert!(!common::marker(dir.path(), "alpha").exists());
    // The stop is still running, so the marker stays for the next daemon.
    assert_eq!(read_marker(dir.path(), "hung"), MARKER);
    assert!(matches!(client.status(), Err(Error::DaemonNotRunning)));
}

#[test]
fn a_hung_boot_keeps_its_record_at_shutdown() {
    let (dir, _client, daemon) = daemon_with_teardown(Duration::from_millis(200));
    let other = Client::new(dir.path().to_path_buf());
    // The fake's boot of `hung` hangs after the daemon has claimed the name and written the
    // marker. The thread is never joined, as above.
    thread::spawn(move || other.machine_start("hung", config()));
    // 50 ms in, the boot is claimed before the shutdown closes admission.
    thread::sleep(Duration::from_millis(50));

    force_shutdown(dir.path());

    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("\ngave up waiting for 1 request(s) after 200ms\n"),
        "{out}"
    );
    assert!(
        out.contains("\nfailed to stop machine hung: machine hung is starting\n"),
        "{out}"
    );
    assert!(out.ends_with("\nstopped\n"), "{out}");
    // The boot may still finish, so the marker stays for the next daemon.
    assert_eq!(read_marker(dir.path(), "hung"), config().to_toml().unwrap());
}

#[test]
fn an_abandoned_stop_keeps_the_daemon_lock() {
    let (dir, _client, daemon) = daemon_with_teardown(Duration::from_millis(200));
    // The fake never answers about `hung`, so the teardown's stop of it hangs, and the daemon
    // gives up on it after 200 ms.
    write_marker(dir.path(), "hung");

    force_shutdown(dir.path());

    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("\nfailed to stop machine hung: machine hung is stopping\n"),
        "{out}"
    );
    // The stop still runs, and may yet remove the marker, so the directory must stay locked:
    // a second daemon fails at once, before it writes anything. Were the lock free, that
    // daemon would serve, and this join would never return.
    let (result, out) = common::start_daemon(dir.path().to_path_buf())
        .join()
        .unwrap();
    assert!(
        matches!(result, Err(Error::DaemonAlreadyRunning)),
        "{result:?}"
    );
    assert_eq!(String::from_utf8(out).unwrap(), "");
}

#[test]
fn an_abandoned_boot_keeps_the_daemon_lock() {
    let (dir, _client, daemon) = daemon_with_teardown(Duration::from_millis(200));
    let other = Client::new(dir.path().to_path_buf());
    // The fake's boot of `hung` never returns. The thread is never joined, as in
    // `a_hung_boot_keeps_its_record_at_shutdown`.
    thread::spawn(move || other.machine_start("hung", config()));
    // 50 ms in, the boot is claimed before the shutdown closes admission.
    thread::sleep(Duration::from_millis(50));

    force_shutdown(dir.path());

    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("\nfailed to stop machine hung: machine hung is starting\n"),
        "{out}"
    );
    // The boot still runs, and may yet remove the marker, so the directory must stay locked,
    // as in `an_abandoned_stop_keeps_the_daemon_lock`.
    let (result, out) = common::start_daemon(dir.path().to_path_buf())
        .join()
        .unwrap();
    assert!(
        matches!(result, Err(Error::DaemonAlreadyRunning)),
        "{result:?}"
    );
    assert_eq!(String::from_utf8(out).unwrap(), "");
}

#[test]
fn a_teardown_bound_too_large_to_add_still_stops_the_machines() {
    // `Duration::MAX` past the current time does not fit an `Instant`, so the teardown must
    // saturate its deadline to get through.
    let (dir, client, daemon) = daemon_with_teardown(Duration::MAX);
    client.machine_start("alpha", config()).unwrap();

    client.shutdown(true).unwrap();

    // Joining fails if the daemon's thread panicked.
    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("\nstopped machine alpha\n"), "{out}");
    assert!(out.ends_with("\nstopped\n"), "{out}");
    assert!(!common::marker(dir.path(), "alpha").exists());
}

/// Starts a daemon in a fresh state directory and returns the directory and a client.
fn daemon() -> (TempDir, Client) {
    let (dir, client, _) = daemon_with_thread();

    (dir, client)
}

/// A daemon's thread: `join` yields the daemon's result and output once it exits.
type DaemonThread = thread::JoinHandle<(Result<()>, Vec<u8>)>;

/// Like `daemon`, and also returns the daemon's thread.
fn daemon_with_thread() -> (TempDir, Client, DaemonThread) {
    daemon_with_teardown(common::TEARDOWN_TIMEOUT)
}

/// Like `daemon_with_thread`, with `timeout` as the bound on each step of the daemon's teardown.
fn daemon_with_teardown(timeout: Duration) -> (TempDir, Client, DaemonThread) {
    let dir = tempfile::tempdir().unwrap();
    let thread = common::start_daemon_with(dir.path().to_path_buf(), timeout);
    let client = Client::new(dir.path().to_path_buf());
    common::wait_for_status(&client).unwrap();

    (dir, client, thread)
}

/// Sends a forced shutdown request by hand, and returns once the daemon has acknowledged it.
///
/// `Client::shutdown` would go on to wait for the daemon's lock, for up to 5 minutes. A start
/// or a stop the daemon abandoned at the teardown keeps that lock while it runs, an hour for
/// the fake's `hung`, so the client would wait out its timeout and fail.
fn force_shutdown(dir: &Path) {
    let mut stream = UnixStream::connect(dir.join("daemon.sock")).unwrap();
    let shutdown = Request::Shutdown { force: true };
    writeln!(stream, "{}", serde_json::to_string(&shutdown).unwrap()).unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    assert_eq!(reply, "{\"done\":null}\n");
}

/// The configuration the tests start machines with.
fn config() -> Config {
    Config {
        resources: Resources {
            cpus: Some(2),
            ..Resources::default()
        },
        ..Config::default()
    }
}

/// A writable, unstaged mount of `source` at `target`.
fn mount(source: &str, target: &str) -> Mount {
    Mount {
        source: source.to_string(),
        target: target.to_string(),
        read_only: false,
        staged: false,
    }
}

/// The status the fake engine reports for a machine it booted.
fn running(name: &str) -> Status {
    Status {
        name: name.to_string(),
        state: State::Running,
        pid: Some(1),
    }
}

/// Writes the marker of `name` by hand, as an earlier daemon would have left it.
fn write_marker(dir: &Path, name: &str) {
    fs::create_dir_all(dir.join("machines")).unwrap();
    fs::write(common::marker(dir, name), MARKER).unwrap();
}

/// Reads the marker of `name`.
fn read_marker(dir: &Path, name: &str) -> String {
    fs::read_to_string(common::marker(dir, name)).unwrap()
}
//
/// Returns the message of the error the daemon replied with.
fn error_message<T: Debug>(result: Result<T>) -> String {
    match result {
        Err(Error::DaemonRequest { message }) => message,
        other => panic!("expected an error reply, got {other:?}"),
    }
}
