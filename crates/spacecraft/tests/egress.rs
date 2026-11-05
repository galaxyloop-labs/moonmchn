//! End-to-end tests for the daemon's egress decider: a gateway's question over TCP, answered from
//! the lists of the machine whose token it carries.

use spacecraft::config::{Egress, Pattern};
use spacecraft::daemon::client::Client;
use spacecraft::machine::Spec;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

mod common;
use common::{DaemonThread, daemon, daemon_in, daemon_with_thread};

/// A marker as an earlier daemon left it: the token `0123456789abcdef` four times in its
/// `[egress]` table, above a spec whose image is pinned and whose `allow` is `["*"]`.
const MARKER: &str = r#"[egress]
token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

[spec.image]
name = "ghcr.io/org/name:latest"
digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[spec.resources]
cpus = 1

[spec.egress]
allow = ["*"]
"#;

#[test]
fn an_unlisted_host_is_denied() {
    let (dir, client) = daemon();
    client
        .machine_start("alpha", spec_with(&["localhost", "127.0.0.1"], &[]))
        .unwrap();
    let token = common::token_in(dir.path(), "alpha");
    let decider = decider(dir.path());

    let unlisted = request(&token, "192.0.2.1:443", &hello("other.invalid"));
    assert_eq!(ask(decider, &unlisted), b'n');
    // A forged claim: `localhost` is allowed, and resolves to another address.
    let forged = request(&token, "192.0.2.1:443", &hello("localhost"));
    assert_eq!(ask(decider, &forged), b'n');
}

#[test]
fn a_loopback_destination_is_denied() {
    let (dir, client) = daemon();
    client
        .machine_start("beta", spec_with(&["localhost"], &[]))
        .unwrap();
    let token = common::token_in(dir.path(), "beta");

    let flow = request(&token, "127.0.0.1:443", &hello("localhost"));
    assert_eq!(ask(decider(dir.path()), &flow), b'n');
}

#[test]
fn an_empty_list_denies_and_logs() {
    let (dir, client, daemon) = daemon_with_thread();
    client.machine_start("gamma", spec_with(&[], &[])).unwrap();
    let token = common::token_in(dir.path(), "gamma");
    let decider = decider(dir.path());

    let loopback = request(&token, "127.0.0.1:443", &hello("localhost"));
    assert_eq!(ask(decider, &loopback), b'n');
    // A flow that sends nothing first.
    let silent = request(&token, "192.0.2.1:443", &[]);
    assert_eq!(ask(decider, &silent), b'n');

    let out = shut_down(&client, daemon);
    // Step 1 turns loopback away before any list is asked.
    assert_denied(&out, "gamma", "127.0.0.1:443", "loopback");
    assert_denied(&out, "gamma", "192.0.2.1:443", "no list allows it");
}

#[test]
fn the_guest_link_is_denied() {
    let (dir, client, daemon) = daemon_with_thread();
    client
        .machine_start("alpha", spec_with(&["*"], &[]))
        .unwrap();
    let token = common::token_in(dir.path(), "alpha");

    // The fake gateway's link, under a bare `*`.
    let flow = request(&token, "198.51.100.1:8000", &[]);
    assert_eq!(ask(decider(dir.path()), &flow), b'n');

    let out = shut_down(&client, daemon);
    assert_denied(&out, "alpha", "198.51.100.1:8000", "guest link");
}

#[test]
fn a_listed_address_is_denied_under_a_wildcard() {
    let (dir, client, daemon) = daemon_with_thread();
    client
        .machine_start("delta", spec_with(&["*"], &["192.0.2.1"]))
        .unwrap();
    let token = common::token_in(dir.path(), "delta");
    let decider = decider(dir.path());

    let listed = request(&token, "192.0.2.1:443", &hello("other.invalid"));
    assert_eq!(ask(decider, &listed), b'n');
    // A deny of addresses alone turns away no other flow without a name.
    let elsewhere = request(&token, "203.0.113.9:443", &hello("other.invalid"));
    assert_eq!(ask(decider, &elsewhere), b'y');

    let out = shut_down(&client, daemon);
    assert_denied(&out, "delta", "192.0.2.1:443", "listed in deny");
}

#[test]
fn a_wrong_token_is_denied() {
    let (dir, client) = daemon();
    // Lists that let the flow through, so the token alone can turn it away.
    client
        .machine_start("alpha", spec_with(&["*"], &[]))
        .unwrap();

    let flow = request(&"0".repeat(64), "192.0.2.1:443", &hello("other.invalid"));
    assert_eq!(ask(decider(dir.path()), &flow), b'n');
}

#[test]
fn a_stopped_machine_no_longer_answers() {
    let (dir, client) = daemon();
    client
        .machine_start("alpha", spec_with(&["*"], &[]))
        .unwrap();
    let token = common::token_in(dir.path(), "alpha");
    let flow = request(&token, "192.0.2.1:443", &hello("other.invalid"));
    let decider = decider(dir.path());
    assert_eq!(ask(decider, &flow), b'y');

    client.machine_stop("alpha").unwrap();
    assert_eq!(ask(decider, &flow), b'n');

    // Nor does a machine whose boot failed: the fake engine fails `boom`'s.
    assert!(
        client
            .machine_start("boom", spec_with(&["*"], &[]))
            .is_err()
    );
    let flow = request(
        &common::bindings("boom")[0].token,
        "192.0.2.1:443",
        &hello("other.invalid"),
    );
    assert_eq!(ask(decider, &flow), b'n');
}

#[test]
fn a_daemon_restart_answers_for_a_machine_started_before() {
    let dir = common::state_dir();
    let port = earlier_daemon(dir.path(), MARKER);

    let (dir, _client) = daemon_in(dir);

    // The port the earlier daemon persisted, which the machine's gateway still dials.
    assert_eq!(decider(dir.path()).port(), port);
    // The marker's token, and its `allow = ["*"]`.
    let flow = request(
        &"0123456789abcdef".repeat(4),
        "192.0.2.1:443",
        &hello("other.invalid"),
    );
    assert_eq!(ask(decider(dir.path()), &flow), b'y');
}

#[test]
fn an_unreadable_marker_denies_its_token() {
    let dir = common::state_dir();
    let token = "0123456789abcdef".repeat(4);
    earlier_daemon(dir.path(), &MARKER.replace(&token, "short"));

    let (dir, _client) = daemon_in(dir);

    let flow = request(&token, "192.0.2.1:443", &hello("other.invalid"));
    assert_eq!(ask(decider(dir.path()), &flow), b'n');
    assert!(common::marker(dir.path(), "alpha").exists());
}

/// The decider's address for the daemon in `dir`: the loopback, at the port in `egress/port`.
fn decider(dir: &Path) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, common::egress_port(dir)))
}

/// Asks the decider at `address` about one flow, as a gateway does: sends `bytes`, then reads
/// the byte of the answer, within 5 seconds.
fn ask(address: SocketAddr, bytes: &[u8]) -> u8 {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(bytes).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut answer = [0];
    stream.read_exact(&mut answer).unwrap();
    answer[0]
}

/// A flow's request as `common::FakeGateway` reads one: `token`, its 64 hex digits decoded,
/// the address and port of `destination`, then the length of `opening` and `opening` itself.
fn request(token: &str, destination: &str, opening: &[u8]) -> Vec<u8> {
    let destination: SocketAddrV4 = destination.parse().unwrap();
    let mut bytes: Vec<u8> = (0..token.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&token[at..at + 2], 16).unwrap())
        .collect();
    bytes.extend(destination.ip().octets());
    bytes.extend(destination.port().to_be_bytes());
    bytes.push(u8::try_from(opening.len()).unwrap());
    bytes.extend(opening);
    bytes
}

/// A TLS `ClientHello` whose SNI is `name`, in one record, with no more than rustls asks of a
/// hello: one cipher suite, null compression, and a `signature_algorithms` extension.
fn hello(name: &str) -> Vec<u8> {
    let name = name.as_bytes();
    let name_len = u16::try_from(name.len()).unwrap();

    // TLS 1.2, a zero random, no session id, TLS_AES_128_GCM_SHA256, null compression.
    let mut body = vec![0x03, 0x03];
    body.extend([0; 32]);
    body.extend([0x00, 0x00, 0x02, 0x13, 0x01, 0x01, 0x00]);
    // The extensions: `server_name` holding one host name, then `signature_algorithms` holding
    // ed25519.
    body.extend((name_len + 17).to_be_bytes());
    body.extend([0x00, 0x00]);
    body.extend((name_len + 5).to_be_bytes());
    body.extend((name_len + 3).to_be_bytes());
    body.push(0x00);
    body.extend(name_len.to_be_bytes());
    body.extend(name);
    body.extend([0x00, 0x0d, 0x00, 0x04, 0x00, 0x02, 0x08, 0x07]);

    // A handshake record holding the hello, whose length takes 3 bytes.
    let body_len = u16::try_from(body.len()).unwrap();
    let mut record = vec![0x16, 0x03, 0x01];
    record.extend((body_len + 4).to_be_bytes());
    record.extend([0x01, 0x00]);
    record.extend(body_len.to_be_bytes());
    record.extend(body);
    record
}

/// A spec from `Spec::default()` whose `[egress]` lists are `allow` and `deny`.
fn spec_with(allow: &[&str], deny: &[&str]) -> Spec {
    let parse = |entries: &[&str]| -> Vec<Pattern> {
        entries
            .iter()
            .map(|entry| Pattern::parse(entry).unwrap())
            .collect()
    };

    Spec {
        egress: Egress {
            allow: parse(allow),
            deny: parse(deny),
        },
        ..Spec::default()
    }
}

/// Shuts the daemon down, waits for its thread, and returns everything it wrote.
fn shut_down(client: &Client, daemon: DaemonThread) -> String {
    client.shutdown(true).unwrap();
    let (result, out) = daemon.join().unwrap();
    result.unwrap();
    String::from_utf8(out).unwrap()
}

/// Asserts that the daemon's output names the deny of a flow from `machine` to `destination`,
/// and its reason. The line's wording is the decider's unit tests' to pin.
fn assert_denied(out: &str, machine: &str, destination: &str, reason: &str) {
    assert!(
        out.lines().any(|line| [machine, destination, reason]
            .iter()
            .all(|word| line.contains(word))),
        "{out}"
    );
}

/// Leaves `dir` as an earlier daemon would have: `marker` as the marker of `alpha`, and in
/// `egress/port` a port no listener holds now. Returns that port.
fn earlier_daemon(dir: &Path, marker: &str) -> u16 {
    fs::create_dir_all(dir.join("machines")).unwrap();
    fs::write(common::marker(dir, "alpha"), marker).unwrap();

    // A port the system handed out a moment ago, free again once its listener drops.
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    fs::create_dir_all(dir.join("egress")).unwrap();
    fs::write(dir.join("egress").join("port"), format!("{port}\n")).unwrap();
    port
}
