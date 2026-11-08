//! The decider reads each flow's request through the engine's gateway, extracts the destination
//! and any claimed name, and evaluates the egress lists to allow or deny the flow.
//!
//! A flow claims a name with a TLS `ClientHello`, whose SNI names the server it asks for. Any
//! other opening bytes claim nothing, plain HTTP included: the gateway asks once per connection,
//! and one connection can carry many requests, each naming its own host, so no request's host
//! can stand for the connection.
//!
//! The guest writes the claim, and the gateway dials the preamble's address whatever the claim
//! says: a `ClientHello` for an allowed name could carry a flow to any address. So a claimed
//! name counts only when the host's resolver maps it to the dialed address, and a name written
//! as an IP address never counts. A flow whose claim does not count is judged by its address
//! alone.

use rustls::server::{Accepted, Acceptor};
use std::collections::HashSet;
use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::sync::broadcast;

use crate::config::{Egress, Subject};
use crate::daemon::egress::{State, Token};
use crate::daemon::server::ACCEPT_RETRY_DELAY;
use crate::machine::{Decoded, Flow, Gateway};

/// The scheme a deny line gives a TLS flow, which it takes for HTTPS.
const HTTPS_SCHEME: &str = "https";

/// How long a gateway may take to send a flow's request. A request not complete by then is
/// denied as a short read, so a connection that sends nothing cannot hold its task.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the host's resolver may take to answer for a claimed name. The guest picks the
/// name, so one whose DNS server never answers would hold the flow, and the gateway's
/// connection, for as long as the resolver waits: past this deadline, the name binds no name.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The most lookups that may run at once, for every machine together. A lookup runs on a
/// blocking thread, which nothing stops: one past its deadline holds the thread and the
/// resolver's socket until the resolver gives up. Over the cap, a claimed name is not looked up
/// at all, so lookups that never end cannot use up the daemon's threads and open files.
const LOOKUPS_MAX: usize = 32;

/// Why a flow's request could not be read. `Display` gives the words that follow
/// `bad preamble,` in the log line.
#[derive(Debug)]
enum ReadError<E> {
    /// The stream ended before the request did.
    Short,
    /// Reading the stream failed. An early end is `Short`.
    Io(io::Error),
    /// The gateway refused the bytes read so far.
    Refused(E),
}

impl<E: fmt::Display> fmt::Display for ReadError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Short => write!(f, "short read"),
            ReadError::Io(error) => write!(f, "{error}"),
            ReadError::Refused(error) => write!(f, "{error}"),
        }
    }
}

impl<E> From<io::Error> for ReadError<E> {
    /// Returns `Short` for an early end of the stream, `UnexpectedEof`, and `Io` for any other
    /// error.
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            ReadError::Short
        } else {
            ReadError::Io(error)
        }
    }
}

/// The answer to one flow.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// The gateway dials the destination.
    Allow,
    /// The guest's connection is reset, and the reason is logged.
    Deny(Reason),
}

/// Why a flow is denied: one variant for each way the evaluation denies. `Display` gives the
/// words the log line ends with.
#[derive(Debug, PartialEq, Eq)]
enum Reason {
    /// A loopback or unspecified destination, which the host would dial as itself.
    Loopback,
    /// A destination on the guest's own link, as the engine's gateway names it.
    GuestLink,
    /// A destination among the host's own addresses, as the engine's gateway names them.
    HostAddress,
    /// A built-in deny: a link-local address, such as the cloud metadata address.
    BuiltIn,
    /// A `deny` entry equals the address or matches the name.
    Listed,
    /// A flow without a name, which a bare `*` would allow, while `deny` holds a hostname it
    /// cannot be checked against.
    Unnamed,
    /// Nothing in `allow` admits the flow.
    NotAllowed,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words = match self {
            Reason::Loopback => "loopback",
            Reason::GuestLink => "guest link",
            Reason::HostAddress => "host address",
            Reason::BuiltIn => "built-in deny",
            Reason::Listed => "listed in deny",
            Reason::Unnamed => "no name to check against deny",
            Reason::NotAllowed => "no list allows it",
        };
        write!(f, "{words}")
    }
}

/// Why a claimed name binds no name, so the flow is judged by its address alone. `Display` gives
/// the words that follow the name in the log line.
#[derive(Debug, PartialEq, Eq)]
enum Unbound {
    /// The lookup failed, or none of its answers is the dialed address, which it carries.
    Elsewhere(IpAddr),
    /// The lookup gave no answer within `LOOKUP_TIMEOUT`.
    Late,
    /// `LOOKUPS_MAX` lookups were running already, so this one never started.
    Busy,
}

impl fmt::Display for Unbound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unbound::Elsewhere(ip) => write!(f, "does not resolve to {ip}"),
            Unbound::Late => write!(f, "did not resolve within {} s", LOOKUP_TIMEOUT.as_secs()),
            Unbound::Busy => write!(f, "was not looked up: too many lookups running"),
        }
    }
}

/// Answers the engine's gateways on the egress listener in `state` until the daemon ends: each
/// connection the listener accepts gets a task of its own, which `handle_flow` runs. The lines
/// they log go to `log`.
///
/// A failed accept is logged, `egress: accept failed: <error>`, and retried after
/// `ACCEPT_RETRY_DELAY`, so an error such as `EMFILE` cannot spin the loop; it never ends it.
/// A listener that cannot be made non-blocking, or handed to tokio, is logged once,
/// `egress: listener failed: <error>`, and `serve` returns: the error of a spawned task reaches
/// nobody.
///
/// Every connection shares two things: the addresses logged already, of the peers with a bad
/// preamble and of the destinations with an unknown token, so each address is logged once; and
/// the cap of `LOOKUPS_MAX` lookups running at once.
pub(super) async fn serve<G: Gateway>(state: Arc<State>, log: broadcast::Sender<String>) {
    let listener = state.listener().and_then(|listener| {
        listener.set_nonblocking(true)?;
        TcpListener::from_std(listener)
    });
    let listener = match listener {
        Ok(listener) => listener,
        Err(error) => {
            drop(log.send(format!("egress: listener failed: {error}")));
            return;
        }
    };

    // ponytail: one entry per distinct address, never cleared; add a cap if logs show it
    // growing large.
    let logged = Arc::new(Mutex::new(HashSet::new()));
    // ponytail: one cap for every machine, so one guest's stuck lookups turn away the names of
    // all; a cap per machine if that bites.
    let lookups = Arc::new(Semaphore::new(LOOKUPS_MAX));
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tokio::spawn(handle_flow::<G, _>(
                    stream,
                    peer,
                    Arc::clone(&state),
                    log.clone(),
                    Arc::clone(&logged),
                    Arc::clone(&lookups),
                ));
            }
            Err(error) => {
                drop(log.send(format!("egress: accept failed: {error}")));
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
            }
        }
    }
}

/// Answers one flow on `stream`, a connection from the gateway at `peer`: reads its request as
/// the gateway `G` speaks it, within `READ_TIMEOUT`, finds the machine by the token in
/// `state`, binds the name the opening bytes claim, under the cap of `lookups`, judges the
/// flow by the machine's lists, and writes `G::ALLOW` or `G::DENY`.
///
/// Every deny is logged through `log`, and so is a claimed name that binds no name; an allowed
/// flow whose claim bound, or that claimed nothing, leaves no line. The lines, each after
/// `egress: `:
///
/// - `denied <peer>: bad preamble, <why>`, once per peer address, whatever its port, as
///   `logged` records; a request not complete by the deadline is a `short read`;
/// - `denied <ip>:<port>: unknown token`, once per destination address, as `logged` records;
/// - `<name> <unbound>`, with the words of `Unbound`, before the flow is judged without the
///   name;
/// - `denied <machine> to https://<name> (<ip>:<port>): <reason>`, for a deny of a flow with a
///   bound name, and `denied <machine> to <ip>:<port>: <reason>` for one without.
///
/// A line is sent before the byte is written, so a gateway that has read the byte finds the
/// line sent already. An error writing the byte, or sending a line, is dropped: the gateway may
/// be gone, and a daemon past its teardown drains no more lines.
async fn handle_flow<G: Gateway, S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    peer: SocketAddr,
    state: Arc<State>,
    log: broadcast::Sender<String>,
    logged: Arc<Mutex<HashSet<IpAddr>>>,
    lookups: Arc<Semaphore>,
) {
    let flow = tokio::time::timeout(READ_TIMEOUT, read_flow::<G>(&mut stream))
        .await
        .unwrap_or(Err(ReadError::Short));
    let flow = match flow {
        Ok(flow) => flow,
        Err(error) => {
            // A gateway connects from a new port for every flow, so the peer counts by its
            // address alone.
            if logged
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(peer.ip())
            {
                drop(log.send(format!("egress: denied {peer}: bad preamble, {error}")));
            }
            drop(stream.write_all(&[G::DENY]).await);
            return;
        }
    };

    let destination = flow.destination;
    let Some((machine, egress)) = state.lookup(&Token(flow.token)) else {
        if logged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(destination.ip())
        {
            drop(log.send(format!("egress: denied {destination}: unknown token")));
        }
        drop(stream.write_all(&[G::DENY]).await);
        return;
    };

    let name = match claimed_name(&flow.opening) {
        Some(claimed) => match bind_name(&claimed, destination, &lookups).await {
            Ok(name) => name,
            Err(unbound) => {
                drop(log.send(format!("egress: {claimed} {unbound}")));
                None
            }
        },
        None => None,
    };

    let subject = Subject {
        address: destination.ip(),
        name,
    };
    let reply = match evaluate::<G>(&egress, &subject) {
        Verdict::Allow => G::ALLOW,
        Verdict::Deny(reason) => {
            // A name bound to the address leads the line, as the URL the guest asked for; the
            // address follows it. Only a TLS hello names a flow, and the path travels encrypted.
            // ponytail: every TLS flow counts as HTTPS, IMAPS and its like included; read the
            // hello's ALPN once the logs show another protocol over TLS.
            let target = match &subject.name {
                Some(name) => format!("{HTTPS_SCHEME}://{name} ({destination})"),
                None => destination.to_string(),
            };
            drop(log.send(format!("egress: denied {machine} to {target}: {reason}")));
            G::DENY
        }
    };
    drop(stream.write_all(&[reply]).await);
}

/// Reads one flow's request from `stream` through the gateway `G`: asks it what the bytes so far
/// make, and reads exactly as many more as it asks for, so a request it refuses leaves everything
/// after the field that gave it away unread. A gateway that asks for no more bytes than it holds
/// breaks `Partial`'s promise, and gets one more byte at a time.
///
/// Returns `Short` when the stream ends before the request does, `Io` when reading it fails, and
/// `Refused`, carrying the gateway's error, when the bytes so far start no request.
async fn read_flow<G: Gateway>(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<Flow, ReadError<G::FlowError>> {
    let mut bytes = Vec::new();
    loop {
        match G::decode_flow(&bytes).map_err(ReadError::Refused)? {
            Decoded::Complete(flow) => return Ok(flow),
            Decoded::Partial(len) => {
                // The part the gateway asks for, and at least one byte, so a gateway that breaks
                // `Partial`'s promise neither shrinks `bytes` nor gets asked forever.
                let mut rest = vec![0; len.saturating_sub(bytes.len()).max(1)];
                stream.read_exact(&mut rest).await?;
                bytes.extend(rest);
            }
        }
    }
}

/// Returns the name the opening bytes claim, as the module documentation has it: the SNI of
/// the TLS `ClientHello` they hold, which `client_hello` reads, as rustls gives it; `bind_name`
/// normalizes it. A hello without SNI, and bytes that hold no hello, plain HTTP included, claim
/// nothing.
fn claimed_name(opening: &[u8]) -> Option<String> {
    let accepted = client_hello(opening)?;
    accepted.client_hello().server_name().map(str::to_owned)
}

/// Returns the TLS `ClientHello` the opening bytes hold, or `None` when they hold no complete
/// one: a hello cut short, or bytes that are no TLS at all.
fn client_hello(opening: &[u8]) -> Option<Accepted> {
    let mut acceptor = Acceptor::default();
    let mut tls_bytes = opening;
    // One `read_tls` call takes at most 4 KiB, and a hello may span many records, so the bytes
    // go in a call at a time until they run out or the acceptor holds the whole hello. An `Err`
    // from `accept` spends the acceptor, so the next `read_tls` fails and ends the loop.
    while let Ok(1..) = acceptor.read_tls(&mut tls_bytes) {
        if let Ok(Some(accepted)) = acceptor.accept() {
            return Some(accepted);
        }
    }
    None
}

/// Returns the claimed name, normalized, when the host's resolver maps it to the dialed
/// address, or `None` when the claim is no name to look up: an IP address, which never counts
/// as one, or a claim that fails to normalize. The lookup runs as `resolve` has it, under the
/// cap of `lookups` and within `LOOKUP_TIMEOUT`.
///
/// Returns why the name binds no name otherwise, as `Unbound` lists. Either way, the flow is
/// judged by its address alone.
async fn bind_name(
    claimed: &str,
    destination: SocketAddr,
    lookups: &Arc<Semaphore>,
) -> Result<Option<String>, Unbound> {
    let Some(normalized) = Subject::normalize_name(claimed) else {
        return Ok(None);
    };
    let ip = destination.ip();
    let host = (normalized.clone(), destination.port());
    // The thread compares the answers itself, and hands back only whether one is the dialed
    // address.
    let binds = resolve(lookups, move || {
        host.to_socket_addrs()
            .is_ok_and(|mut answers| answers.any(|answer| answer.ip() == ip))
    })
    .await?;
    if binds {
        Ok(Some(normalized))
    } else {
        Err(Unbound::Elsewhere(ip))
    }
}

/// Runs `lookup`, which answers whether the name binds, on a blocking thread, as
/// `tokio::net::lookup_host` runs the resolver, and returns its answer: `Busy`, without running
/// it, when every permit of `lookups` is taken, and `Late` when no answer comes within
/// `LOOKUP_TIMEOUT`. The lookup comes as a closure, so a test can hand in one that sticks.
///
/// Nothing stops a blocking thread, so a lookup past its deadline runs on until the resolver
/// gives up. Its permit goes with the thread and comes back only then, so the cap counts every
/// lookup still running, the late ones included.
async fn resolve(
    lookups: &Arc<Semaphore>,
    lookup: impl FnOnce() -> bool + Send + 'static,
) -> Result<bool, Unbound> {
    let Ok(permit) = Arc::clone(lookups).try_acquire_owned() else {
        return Err(Unbound::Busy);
    };
    let thread = tokio::task::spawn_blocking(move || {
        let answer = lookup();
        // Only now, however long after the deadline: the cap counts the threads still running.
        drop(permit);
        answer
    });
    // A thread that ends without an answer, which only a panic does, is no answer either.
    tokio::time::timeout(LOOKUP_TIMEOUT, thread)
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(Unbound::Late)
}

/// Judges a flow by `subject`, its destination and the name bound to it, if any, in order:
///
/// 1. A flow to a loopback, unspecified or guest-link destination, or to another of the host's
///    own addresses, is denied, whatever `allow` lists: no entry opens the host itself. The
///    gateway `G` says which addresses are on the guest's own link, and which others are the
///    host's own.
/// 2. A flow is denied when a built-in deny covers its destination, or a `deny` entry equals
///    the destination or matches the name.
/// 3. A flow with a name is allowed when an `allow` entry matches the name, or an IP literal in
///    `allow` equals the destination; a bare `*` matches every name.
/// 4. A flow without a name is allowed by an IP literal in `allow` equal to its destination, or
///    by a bare `*` while `deny` holds no hostname.
/// 5. Any other flow is denied.
///
/// The destination counts in its canonical form, so an IPv4-mapped address counts as its IPv4
/// one, for the gateway too.
fn evaluate<G: Gateway>(egress: &Egress, subject: &Subject) -> Verdict {
    // Every check below, the gateway's included, sees `::ffff:127.0.0.1` as `127.0.0.1`. `Egress`
    // canonicalizes the addresses it compares, so `subject` goes to it unchanged.
    let address = subject.address.to_canonical();

    // Step 1: the host itself, whatever `allow` lists: its own addresses and the guest's own link.
    // The host's services often trust whatever connects from the host, the gateway included, and
    // an IP entry can come to name one of the host's addresses.
    if let Some(reason) = own_address::<G>(address) {
        return Verdict::Deny(reason);
    }

    // Step 2: every deny, before any allow. A deny on the address holds whatever name the flow
    // carries, and a deny on the name holds for every address it resolves to.
    if Egress::built_in_denies(address) {
        return Verdict::Deny(Reason::BuiltIn);
    }
    if egress.denies(subject) {
        return Verdict::Deny(Reason::Listed);
    }

    // Steps 3 and 4: a flow with a name is allowed by its name or its address, and one without by
    // its address; `Egress::allows` holds both rules.
    if egress.allows(subject) {
        return Verdict::Allow;
    }

    // Step 5: nothing allows the flow. A flow without a name that a bare `*` would have let
    // through was turned away by a hostname in `deny`, which it has no name to be checked
    // against: its reason says so.
    if subject.name.is_none() && egress.has_wildcard() && egress.has_name_deny() {
        return Verdict::Deny(Reason::Unnamed);
    }
    Verdict::Deny(Reason::NotAllowed)
}

/// Returns why a destination is the host's own or the guest's: `Loopback` for a loopback or
/// unspecified address, which the host dials as itself; `GuestLink` for an address on the
/// guest's own link, and `HostAddress` for another of the host's own addresses, as the gateway
/// `G` names each; and `None` for any other address.
fn own_address<G: Gateway>(ip: IpAddr) -> Option<Reason> {
    if ip.is_loopback() || ip.is_unspecified() {
        Some(Reason::Loopback)
    } else if G::is_guest_link(ip) {
        Some(Reason::GuestLink)
    } else if G::is_host_address(ip) {
        Some(Reason::HostAddress)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Pattern;
    use crate::daemon::egress::Token;
    use crate::daemon::machines::{Marker, MarkerEgress};
    use crate::machine::Spec;
    use std::cell::Cell;
    use std::net::Ipv6Addr;
    use tempfile::TempDir;
    use tokio::io::{AsyncWriteExt, duplex};

    /// The opening bytes of the spike's first flow, captured on 2026-10-27: a 1544-byte
    /// `ClientHello` whose SNI is `example.com`.
    const CLIENT_HELLO: &[u8] = include_bytes!("decider/testdata/client_hello_example_com.bin");

    /// A gateway that speaks the tests' own request, so no test here spells out an engine's: the
    /// machine's token (32 bytes), an IPv4 destination (4) and its port (2, big-endian), a length
    /// (1), then that many opening bytes, at most 128. It answers `y` or `n`, which no engine
    /// sends; its guest link is `198.51.100.1` alone, and its host's one address is
    /// `198.51.100.2`.
    struct FakeGateway;

    impl Gateway for FakeGateway {
        type FlowError = &'static str;

        const ALLOW: u8 = b'y';
        const DENY: u8 = b'n';

        fn decode_flow(bytes: &[u8]) -> Result<Decoded, &'static str> {
            let Some(head) = bytes.get(..39) else {
                return Ok(Decoded::Partial(39));
            };
            let length = usize::from(head[38]);
            if length > 128 {
                return Err("more than 128 opening bytes");
            }
            let Some(opening) = bytes.get(39..39 + length) else {
                return Ok(Decoded::Partial(39 + length));
            };

            Ok(Decoded::Complete(Flow {
                token: head[..32].try_into().unwrap(),
                destination: SocketAddr::from((
                    [head[32], head[33], head[34], head[35]],
                    u16::from_be_bytes([head[36], head[37]]),
                )),
                opening: opening.to_vec(),
            }))
        }

        fn is_guest_link(ip: IpAddr) -> bool {
            ip == IpAddr::from([198, 51, 100, 1])
        }

        fn is_host_address(ip: IpAddr) -> bool {
            ip == IpAddr::from([198, 51, 100, 2])
        }
    }

    thread_local! {
        /// How many times `StubbornGateway` was asked on this test's thread.
        static QUESTIONS: Cell<u32> = const { Cell::new(0) };
    }

    /// A gateway that breaks `Partial`'s promise once it holds 4 bytes: it asks for the 4 it
    /// has when they open with `=`, and for 2 when they open with `<`. It refuses 5 bytes, and
    /// it errs differently on its eleventh question, so a `read_flow` that asks again and again
    /// fails instead of hanging.
    struct StubbornGateway;

    impl Gateway for StubbornGateway {
        type FlowError = &'static str;

        const ALLOW: u8 = b'y';
        const DENY: u8 = b'n';

        fn decode_flow(bytes: &[u8]) -> Result<Decoded, &'static str> {
            QUESTIONS.set(QUESTIONS.get() + 1);
            if QUESTIONS.get() > 10 {
                return Err("read_flow asked more than ten times");
            }

            match bytes {
                [] | [b'=', _, _, _] => Ok(Decoded::Partial(4)),
                [b'<', _, _, _] => Ok(Decoded::Partial(2)),
                _ => Err("refused"),
            }
        }

        fn is_guest_link(_: IpAddr) -> bool {
            false
        }

        fn is_host_address(_: IpAddr) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn read_flow_reads_what_the_gateway_asks_for() {
        let mut bytes = request(&[0xaa; 32], [192, 0, 2, 1], 443, b"GET");
        bytes.extend(b"rest");
        let mut stream = &bytes[..];

        let flow = read_flow::<FakeGateway>(&mut stream).await.unwrap();

        assert_eq!(flow.token, [0xaa; 32]);
        assert_eq!(flow.destination, SocketAddr::from(([192, 0, 2, 1], 443)));
        assert_eq!(flow.opening, b"GET");
        // Exactly the request was read: what follows it is still there.
        assert_eq!(stream, b"rest");
    }

    #[tokio::test]
    async fn short_and_refused_requests_are_errors() {
        // The stream ends before the request does.
        let bytes = request(&[0xaa; 32], [192, 0, 2, 1], 443, b"GET");

        let Err(error) = read_flow::<FakeGateway>(&mut &bytes[..bytes.len() - 1]).await else {
            panic!("read a request cut short");
        };

        assert_eq!(error.to_string(), "short read");

        // A length over the gateway's limit is refused before the bytes it announces are read.
        let mut bytes = request(&[0xaa; 32], [192, 0, 2, 1], 443, &[]);
        *bytes.last_mut().unwrap() = 129;
        bytes.extend([b'x'; 129]);
        let mut stream = &bytes[..];

        let Err(error) = read_flow::<FakeGateway>(&mut stream).await else {
            panic!("read a request over the gateway's limit");
        };

        assert_eq!(error.to_string(), "more than 128 opening bytes");
        assert_eq!(stream.len(), 129);
    }

    #[tokio::test]
    async fn a_gateway_asking_for_no_more_bytes_still_ends_the_read() {
        // Asked for the bytes it already holds, or for fewer, `read_flow` still ends with the
        // gateway's refusal, without asking forever.
        for bytes in [b"====rest", b"<<<<rest"] {
            assert!(
                matches!(
                    read_flow::<StubbornGateway>(&mut &bytes[..]).await,
                    Err(ReadError::Refused("refused"))
                ),
                "{}",
                bytes.escape_ascii()
            );
        }
    }

    #[test]
    fn sni_is_read_from_the_client_hello() {
        assert_eq!(claimed_name(CLIENT_HELLO), Some("example.com".to_string()));
        // A hello cut short claims nothing yet, and neither does a record that holds no hello.
        assert_eq!(claimed_name(&CLIENT_HELLO[..20]), None);
        assert_eq!(
            claimed_name(&[0x16, 0x03, 0x01, 0x00, 0x05, 1, 2, 3, 4, 5]),
            None
        );
    }

    #[test]
    fn a_hello_in_many_small_records_claims_its_sni() {
        // The same hello, its handshake bytes after the 5-byte record header split into records
        // of 2 bytes each, a way to slip past SNI filters: 5389 bytes, more than the 4 KiB rustls
        // takes in one read.
        let mut records = Vec::new();
        for piece in CLIENT_HELLO[5..].chunks(2) {
            records.extend([0x16, 0x03, 0x01, 0x00, u8::try_from(piece.len()).unwrap()]);
            records.extend(piece);
        }
        assert_eq!(records.len(), 5389);

        assert_eq!(claimed_name(&records), Some("example.com".to_string()));
    }

    #[test]
    fn a_plain_http_request_claims_no_name() {
        // A connection carries many requests, each naming its own host, and the gateway asks
        // once: no request's host can stand for the connection, whatever its form.
        for head in [
            "GET / HTTP/1.1\r\nHost: api.github.com\r\n\r\n",
            "GET http://api.github.com/ HTTP/1.1\r\nHost: api.github.com\r\n\r\n",
            "CONNECT api.github.com:443 HTTP/1.1\r\nHost: api.github.com:443\r\n\r\n",
        ] {
            assert_eq!(claimed_name(head.as_bytes()), None, "{head:?}");
        }
    }

    #[tokio::test]
    async fn an_ip_literal_claim_is_no_name() {
        let lookups = Arc::new(Semaphore::new(1));

        // An address claimed as a name binds no name, even to itself, and is no name to report.
        assert_eq!(
            bind_name(
                "169.254.169.254",
                SocketAddr::from(([169, 254, 169, 254], 80)),
                &lookups
            )
            .await,
            Ok(None)
        );
        assert_eq!(
            bind_name(
                "[::1]",
                SocketAddr::from((Ipv6Addr::LOCALHOST, 80)),
                &lookups
            )
            .await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn bind_name_requires_the_dialed_address_among_the_answers() {
        let lookups = Arc::new(Semaphore::new(1));
        let loopback = SocketAddr::from(([127, 0, 0, 1], 443));
        let elsewhere = SocketAddr::from(([192, 0, 2, 1], 443));

        // `localhost` resolves through the hosts file; `nope.invalid` never resolves.
        assert_eq!(
            bind_name("localhost", loopback, &lookups).await,
            Ok(Some("localhost".to_string()))
        );
        assert_eq!(
            bind_name("LOCALHOST.", loopback, &lookups).await,
            Ok(Some("localhost".to_string()))
        );
        assert_eq!(
            bind_name("localhost", elsewhere, &lookups).await,
            Err(Unbound::Elsewhere(elsewhere.ip()))
        );
        assert_eq!(
            bind_name("nope.invalid", elsewhere, &lookups).await,
            Err(Unbound::Elsewhere(elsewhere.ip()))
        );
    }

    #[tokio::test]
    async fn a_lookup_over_the_cap_is_not_made() {
        // No permit is left, so `localhost` is not looked up, though the hosts file binds it.
        let busy = bind_name(
            "localhost",
            SocketAddr::from(([127, 0, 0, 1], 443)),
            &Arc::new(Semaphore::new(0)),
        )
        .await;

        assert_eq!(busy, Err(Unbound::Busy));
        assert_eq!(
            busy.unwrap_err().to_string(),
            "was not looked up: too many lookups running"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stuck_lookup_is_late_and_keeps_its_permit_until_it_ends() {
        let lookups = Arc::new(Semaphore::new(1));
        // A lookup stuck until the test lets it go, or for 10 s at most: without a deadline, the
        // test fails once those 10 s pass.
        let (release, released) = std::sync::mpsc::channel::<()>();
        let lookup = move || released.recv_timeout(Duration::from_secs(10)).is_ok();

        // The lookup starts first, then the paused clock jumps to its deadline, 5 s on.
        let (late, ()) = tokio::join!(
            resolve(&lookups, lookup),
            tokio::time::advance(Duration::from_secs(5))
        );

        assert_eq!(late, Err(Unbound::Late));
        assert_eq!(late.unwrap_err().to_string(), "did not resolve within 5 s");
        // The thread runs on past the deadline, and holds the only permit until its lookup
        // ends.
        assert!(lookups.try_acquire().is_err());
        release.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(10), lookups.acquire())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_name_that_resolves_elsewhere_is_dropped() {
        let destination = SocketAddr::from(([203, 0, 113, 9], 443));

        assert_eq!(
            bind_name("localhost", destination, &Arc::new(Semaphore::new(1))).await,
            Err(Unbound::Elsewhere(destination.ip()))
        );
        // The allowed name buys nothing for an address it does not resolve to.
        let subject = Subject {
            address: destination.ip(),
            name: None,
        };
        assert_eq!(
            evaluate::<FakeGateway>(&egress(&["localhost"], &[]), &subject),
            Verdict::Deny(Reason::NotAllowed)
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the length is the table's rows, one per case, grouped by step"
    )]
    fn evaluation_order() {
        use Reason::{BuiltIn, GuestLink, HostAddress, Listed, Loopback, NotAllowed, Unnamed};
        use Verdict::{Allow, Deny};

        /// `allow`, `deny`, the destination, the bound name, and the verdict.
        type Case<'a> = (
            &'a [&'a str],
            &'a [&'a str],
            &'a str,
            Option<&'a str>,
            Verdict,
        );

        let cases: &[Case<'_>] = &[
            // The host itself, whatever `allow` lists: loopback, the unspecified addresses, the
            // guest link and the host's own addresses. `FakeGateway`'s link is `198.51.100.1`,
            // asked in canonical form, and its host's address is `198.51.100.2`.
            (&[], &[], "127.0.0.1", Some("localhost"), Deny(Loopback)),
            (&["127.0.0.1"], &[], "127.0.0.1", None, Deny(Loopback)),
            (&["*"], &[], "::ffff:127.0.0.1", None, Deny(Loopback)),
            (&["*"], &[], "0.0.0.0", None, Deny(Loopback)),
            (&["*"], &[], "::", None, Deny(Loopback)),
            (&[], &[], "198.51.100.1", None, Deny(GuestLink)),
            (&["*"], &[], "198.51.100.1", None, Deny(GuestLink)),
            (&["*"], &[], "::ffff:198.51.100.1", None, Deny(GuestLink)),
            (
                &["198.51.100.1"],
                &[],
                "198.51.100.1",
                None,
                Deny(GuestLink),
            ),
            (&["*"], &[], "198.51.100.2", None, Deny(HostAddress)),
            // The built-in denies, under every list.
            (&["*"], &[], "169.254.169.254", None, Deny(BuiltIn)),
            // A name needs a hostname entry, or an IP literal for the destination. Without a name,
            // an address needs an IP literal, or a bare `*` while `deny` holds no hostname.
            (
                &["*.example"],
                &[],
                "93.184.216.34",
                Some("a.example"),
                Allow,
            ),
            (
                &["*.example"],
                &[],
                "93.184.216.34",
                Some("example.com"),
                Deny(NotAllowed),
            ),
            (&["*.example"], &[], "93.184.216.34", None, Deny(NotAllowed)),
            (
                &["*.example", "93.184.216.34"],
                &[],
                "93.184.216.34",
                None,
                Allow,
            ),
            (&["*"], &[], "93.184.216.34", None, Allow),
            (&["*", "api.github.com"], &[], "93.184.216.34", None, Allow),
            (
                &[],
                &[],
                "93.184.216.34",
                Some("example.com"),
                Deny(NotAllowed),
            ),
            (&[], &[], "93.184.216.34", None, Deny(NotAllowed)),
            // `deny` before `allow`.
            (
                &["*.example"],
                &["evil.example"],
                "93.184.216.34",
                Some("evil.example"),
                Deny(Listed),
            ),
            (
                &["*"],
                &["evil.example"],
                "93.184.216.34",
                Some("a.example"),
                Allow,
            ),
            (
                &["*"],
                &["evil.example"],
                "93.184.216.34",
                None,
                Deny(Unnamed),
            ),
            (
                &["*", "93.184.216.34"],
                &["evil.example"],
                "93.184.216.34",
                None,
                Allow,
            ),
            (
                &["*"],
                &["93.184.216.34"],
                "93.184.216.34",
                Some("a.example"),
                Deny(Listed),
            ),
            (&["*"], &["93.184.216.34"], "203.0.113.9", None, Allow),
        ];

        for (allow, deny, destination, name, verdict) in cases {
            let subject = Subject {
                address: destination.parse().unwrap(),
                name: name.map(str::to_owned),
            };
            assert_eq!(
                &evaluate::<FakeGateway>(&egress(allow, deny), &subject),
                verdict,
                "allow {allow:?}, deny {deny:?}, {destination}, {name:?}"
            );
        }
    }

    #[test]
    fn reasons_display_their_log_words() {
        for (reason, words) in [
            (Reason::Loopback, "loopback"),
            (Reason::GuestLink, "guest link"),
            (Reason::HostAddress, "host address"),
            (Reason::BuiltIn, "built-in deny"),
            (Reason::Listed, "listed in deny"),
            (Reason::Unnamed, "no name to check against deny"),
            (Reason::NotAllowed, "no list allows it"),
        ] {
            assert_eq!(reason.to_string(), words);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_client_is_dropped_at_the_deadline() {
        let (_dir, state) = alpha(&["*"], &[]);
        let (log, mut sent) = broadcast::channel(16);

        // The gateway sends nothing and stays open; with time paused, the 5 s pass at once.
        assert_eq!(answer(&state, &log, &Arc::default(), &[]).await, b'n');

        assert_eq!(
            lines(&mut sent),
            ["egress: denied 127.0.0.1:9: bad preamble, short read"]
        );
    }

    #[tokio::test]
    async fn a_bad_preamble_is_logged_once_per_peer_address() {
        let (_dir, state) = alpha(&["*"], &[]);
        let (log, mut sent) = broadcast::channel(16);
        let logged = Arc::default();
        // A length over the gateway's limit, which it refuses before any opening byte.
        let mut bytes = request(&[0xaa; 32], [192, 0, 2, 1], 443, &[]);
        *bytes.last_mut().unwrap() = 129;

        assert_eq!(answer(&state, &log, &logged, &bytes).await, b'n');
        // A gateway connects from a new port for every flow.
        let next_port = SocketAddr::from(([127, 0, 0, 1], 10));
        assert_eq!(
            answer_from(next_port, &state, &log, &logged, &bytes).await,
            b'n'
        );

        assert_eq!(
            lines(&mut sent),
            ["egress: denied 127.0.0.1:9: bad preamble, more than 128 opening bytes"]
        );
    }

    #[tokio::test]
    async fn an_unknown_token_is_denied_and_logged_once_per_destination() {
        let (_dir, state) = alpha(&["*"], &[]);
        let (log, mut sent) = broadcast::channel(16);
        let logged = Arc::default();
        let bytes = request(&[0xbb; 32], [192, 0, 2, 1], 443, &[]);

        assert_eq!(answer(&state, &log, &logged, &bytes).await, b'n');
        assert_eq!(answer(&state, &log, &logged, &bytes).await, b'n');

        assert_eq!(
            lines(&mut sent),
            ["egress: denied 192.0.2.1:443: unknown token"]
        );
    }

    #[tokio::test]
    async fn an_allowed_flow_is_not_logged() {
        let (_dir, state) = alpha(&["*"], &[]);
        let (log, mut sent) = broadcast::channel(16);

        // The flow sends nothing first, so it claims no name that could fail to bind and log a
        // line, and a bare `*` admits it while `deny` holds no hostname.
        let bytes = request(&[0xaa; 32], [192, 0, 2, 1], 443, &[]);
        assert_eq!(answer(&state, &log, &Arc::default(), &bytes).await, b'y');

        assert_eq!(lines(&mut sent), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_dropped_name_is_logged_then_judged() {
        let (_dir, state) = alpha(&["*"], &[]);
        let (log, mut sent) = broadcast::channel(16);

        // Without its name, the flow is judged by its address, which a bare `*` admits while
        // `deny` holds no hostname.
        let bytes = request(&[0xaa; 32], [192, 0, 2, 1], 443, &hello("other.invalid"));
        assert_eq!(answer(&state, &log, &Arc::default(), &bytes).await, b'y');

        assert_eq!(
            lines(&mut sent),
            ["egress: other.invalid does not resolve to 192.0.2.1"]
        );
    }

    #[tokio::test]
    async fn a_name_deny_turns_away_a_dropped_name() {
        let (_dir, state) = alpha(&["*"], &["evil.example"]);
        let (log, mut sent) = broadcast::channel(16);

        let bytes = request(&[0xaa; 32], [192, 0, 2, 1], 443, &hello("other.invalid"));
        assert_eq!(answer(&state, &log, &Arc::default(), &bytes).await, b'n');

        assert_eq!(
            lines(&mut sent),
            [
                "egress: other.invalid does not resolve to 192.0.2.1",
                "egress: denied alpha to 192.0.2.1:443: no name to check against deny",
            ]
        );
    }

    #[tokio::test]
    async fn a_named_deny_names_the_url() {
        // `localhost` binds to `127.0.0.1`, which step 1 denies, and the line names the URL the
        // guest asked for.
        let (_dir, state) = alpha(&[], &[]);
        let (log, mut sent) = broadcast::channel(16);

        let bytes = request(&[0xaa; 32], [127, 0, 0, 1], 443, &hello("localhost"));
        assert_eq!(answer(&state, &log, &Arc::default(), &bytes).await, b'n');

        assert_eq!(
            lines(&mut sent),
            ["egress: denied alpha to https://localhost (127.0.0.1:443): loopback"]
        );
    }

    /// A request as `FakeGateway` reads one: `token`, `ip`, `port`, then the length of `opening`
    /// and `opening` itself. The length comes from the slice, so a test that needs a wrong one
    /// edits the bytes.
    fn request(token: &[u8; 32], ip: [u8; 4], port: u16, opening: &[u8]) -> Vec<u8> {
        let mut bytes = token.to_vec();
        bytes.extend(ip);
        bytes.extend(port.to_be_bytes());
        bytes.push(u8::try_from(opening.len()).unwrap());
        bytes.extend(opening);
        bytes
    }

    /// A TLS `ClientHello` whose SNI is `name`, in one record, with no more than rustls asks of
    /// a hello: one cipher suite, null compression, and a `signature_algorithms` extension.
    fn hello(name: &str) -> Vec<u8> {
        let name = name.as_bytes();
        let name_len = u16::try_from(name.len()).unwrap();

        // TLS 1.2, a zero random, no session id, TLS_AES_128_GCM_SHA256, null compression.
        let mut body = vec![0x03, 0x03];
        body.extend([0; 32]);
        body.extend([0x00, 0x00, 0x02, 0x13, 0x01, 0x01, 0x00]);
        // The extensions: `server_name` holding one host name, then `signature_algorithms`
        // holding ed25519.
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

    /// The `[egress]` lists, each entry parsed as a pattern.
    fn egress(allow: &[&str], deny: &[&str]) -> Egress {
        let parse = |entries: &[&str]| -> Vec<Pattern> {
            entries
                .iter()
                .map(|entry| Pattern::parse(entry).unwrap())
                .collect()
        };

        Egress {
            allow: parse(allow),
            deny: parse(deny),
        }
    }

    /// The decider's state in a fresh state directory, answering for `alpha`, whose token is
    /// `[0xaa; 32]` and whose spec's lists are `allow` and `deny`. The directory lives as long
    /// as the returned guard.
    fn alpha(allow: &[&str], deny: &[&str]) -> (TempDir, Arc<State>) {
        let tmp = tempfile::tempdir().unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let dir = crate::daemon::state::prepare_in(&tmp.path().join("state"), uid).unwrap();
        let state = State::bind(&dir).unwrap();
        state.register(
            "alpha",
            &Marker {
                egress: MarkerEgress {
                    token: Token([0xaa; 32]),
                },
                spec: Spec {
                    egress: egress(allow, deny),
                    ..Spec::default()
                },
            },
        );

        (tmp, Arc::new(state))
    }

    /// Runs `handle_flow` on one connection from the gateway at `127.0.0.1:9`, which sends
    /// `bytes` and stays open, and returns the byte the decider answered.
    async fn answer(
        state: &Arc<State>,
        log: &broadcast::Sender<String>,
        logged: &Arc<Mutex<HashSet<IpAddr>>>,
        bytes: &[u8],
    ) -> u8 {
        answer_from(
            SocketAddr::from(([127, 0, 0, 1], 9)),
            state,
            log,
            logged,
            bytes,
        )
        .await
    }

    /// Like `answer`, for a connection from the gateway at `peer`.
    async fn answer_from(
        peer: SocketAddr,
        state: &Arc<State>,
        log: &broadcast::Sender<String>,
        logged: &Arc<Mutex<HashSet<IpAddr>>>,
        bytes: &[u8],
    ) -> u8 {
        let (mut gateway, decider) = duplex(1024);
        gateway.write_all(bytes).await.unwrap();

        handle_flow::<FakeGateway, _>(
            decider,
            peer,
            Arc::clone(state),
            log.clone(),
            Arc::clone(logged),
            Arc::new(Semaphore::new(1)),
        )
        .await;

        let mut answer = [0];
        gateway.read_exact(&mut answer).await.unwrap();
        answer[0]
    }

    /// The lines sent through `log` so far, in order.
    fn lines(log: &mut broadcast::Receiver<String>) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(line) = log.try_recv() {
            lines.push(line);
        }
        lines
    }
}
