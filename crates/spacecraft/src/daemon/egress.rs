//! The daemon's egress listener and the tokens its machines' gateways present.
//!
//! `<state>/egress/port` holds the listener's port. Tokens and their policies are loaded from
//! the machine markers at startup, registered after a marker is written, and retired when its
//! machine is removed. The token map lives only in memory.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::config::{Egress, EgressEndpoint};
use crate::daemon::machines::{MACHINE_DIR_NAME, Marker, machine_marker, read_marker};
use crate::machine::TOKEN_LEN;
use crate::{Error, Result};

/// The secret a machine's gateway presents to the decider, written as 64 hex digits and hidden
/// by `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(super) struct Token(pub(super) [u8; TOKEN_LEN]);

impl TryFrom<String> for Token {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let mut bytes = [0; TOKEN_LEN];
        unhex(&text, &mut bytes).ok_or("a token is 64 hexadecimal digits")?;
        Ok(Token(bytes))
    }
}

impl From<Token> for String {
    fn from(token: Token) -> Self {
        hex(&token.0)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl Token {
    /// Draws a new token from the operating system's random source. Fails with `TokenMint` only
    /// when that source does, its error carried as an `io::Error`, so `Error` names no
    /// `getrandom` type.
    pub(super) fn mint() -> Result<Self> {
        let mut bytes = [0; TOKEN_LEN];

        getrandom::fill(&mut bytes).map_err(|e| Error::TokenMint(io::Error::other(e)))?;

        Ok(Token(bytes))
    }
}

/// The directory holding the daemon's persisted egress port.
pub(super) const EGRESS_DIR_NAME: &str = "egress";
/// The file holding the listener's port as decimal followed by a newline.
const PORT_FILE_NAME: &str = "port";
/// The port file is readable and writable by its owner alone.
const FILE_MODE: u32 = 0o600;

/// The loopback listener and the token and policy registered for each machine.
pub(super) struct State {
    port: u16,
    listener: TcpListener,
    entries: Mutex<HashMap<String, Entry>>,
}

impl State {
    /// Loads readable markers and binds the persisted port, or chooses and persists a new one.
    ///
    /// `state::prepare_in` has already laid out `machines/` and `egress/`. An unreadable or
    /// malformed marker contributes no token, but still prevents moving away from a port its
    /// machine may use. A missing or malformed port file counts as absent.
    ///
    /// # Errors
    ///
    /// Returns `EgressListen` when the persisted port cannot be bound while any marker remains,
    /// when an ephemeral port cannot be bound, or when the listener cannot report its port.
    /// Returns `Io` when the port file or the markers directory cannot be read, or the port
    /// cannot be written.
    pub(super) fn bind(state_dir: &Path) -> Result<Self> {
        let port_path = state_dir.join(EGRESS_DIR_NAME).join(PORT_FILE_NAME);
        let saved_port = match fs::read_to_string(&port_path) {
            Ok(contents) => contents.trim().parse::<u16>().ok(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            // `read_to_string` reports bytes that are not UTF-8 this way: no port either.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => None,
            Err(error) => return Err(Error::io(&port_path, error)),
        };

        let dir = state_dir.join(MACHINE_DIR_NAME);
        let markers = fs::read_dir(&dir).map_err(|error| Error::io(&dir, error))?;
        let mut entries = HashMap::new();
        let mut has_markers = false;
        for entry in markers {
            let entry = entry.map_err(|error| Error::io(&dir, error))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(path) = machine_marker(&dir, &name) else {
                continue;
            };
            has_markers = true;
            if let Ok(Some(marker)) = read_marker(&path) {
                entries.insert(
                    name,
                    Entry {
                        token: marker.egress.token,
                        egress: marker.spec.egress,
                    },
                );
            }
        }

        let port = saved_port.unwrap_or(0);
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => listener,
            Err(_) if port != 0 && !has_markers => TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .map_err(|source| Error::EgressListen { port: 0, source })?,
            Err(source) => return Err(Error::EgressListen { port, source }),
        };
        let port = listener
            .local_addr()
            .map_err(|source| Error::EgressListen { port, source })?
            .port();
        if saved_port != Some(port) {
            write_file(&port_path, &format!("{port}\n"))?;
        }

        Ok(Self {
            port,
            listener,
            entries: Mutex::new(entries),
        })
    }

    /// Registers a written marker, replacing any earlier entry for the name, and returns the
    /// binding its machine must use at boot.
    pub(super) fn register(&self, name: &str, marker: &Marker) -> EgressEndpoint {
        self.entries().insert(
            name.to_string(),
            Entry {
                token: marker.egress.token.clone(),
                egress: marker.spec.egress.clone(),
            },
        );
        EgressEndpoint {
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, self.port())),
            token: marker.egress.token.clone().into(),
        }
    }

    /// Stops answering for the token of a removed machine.
    pub(super) fn retire(&self, name: &str) {
        self.entries().remove(name);
    }

    /// Finds the machine and policy authenticated by `token`.
    #[must_use]
    pub(super) fn lookup(&self, token: &Token) -> Option<(String, Egress)> {
        // ponytail: a scan with a plain compare; one user per host, and dozens of machines at
        // most, so neither a token index nor a constant-time comparison is needed here.
        self.entries()
            .iter()
            .find(|(_, entry)| &entry.token == token)
            .map(|(name, entry)| (name.clone(), entry.egress.clone()))
    }

    /// Returns the bound loopback port.
    #[must_use]
    pub(super) fn port(&self) -> u16 {
        self.port
    }

    /// Clones the blocking listener; the decider makes its clone non-blocking.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the listener cannot be cloned.
    pub(super) fn listener(&self) -> io::Result<TcpListener> {
        self.listener.try_clone()
    }

    /// Locks the token map, poisoned or not.
    ///
    /// As with the in-flight claims, each mutation is one map operation, so a poisoned lock can
    /// be recovered without exposing a partially updated entry.
    fn entries(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Atomically replaces `path` with an owner-only file containing `contents`.
fn write_file(path: &Path, contents: &str) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::io(path, io::ErrorKind::InvalidInput.into()))?;
    let mut file = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(FILE_MODE))
        .tempfile_in(dir)
        .map_err(|error| Error::io(path, error))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| Error::io(path, error))?;
    file.persist(path)
        .map_err(|error| Error::io(path, error.error))?;
    Ok(())
}

/// The token and policy loaded from, or registered with, a machine's marker.
struct Entry {
    token: Token,
    egress: Egress,
}

/// Returns `bytes` as lowercase hex, two digits per byte.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0xf])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// Decodes `text` into `out`, two hex digits per byte. Returns `None` when `text` holds
/// anything else, or more or fewer digits than `out` needs.
fn unhex(text: &str, out: &mut [u8]) -> Option<()> {
    // Every byte is checked first: `u8::from_str_radix` also takes a leading `+`.
    if text.len() != out.len().saturating_mul(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }

    for (byte, pair) in out.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        *byte = u8::from_str_radix(str::from_utf8(pair).ok()?, 16).ok()?;
    }

    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Pattern;
    use crate::daemon::machines::MarkerEgress;
    use crate::machine;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A marker as `tests/machines.rs` writes one by hand: the token `a` 64 times in `[egress]`,
    /// above a spec with no `[spec.egress]`, its image pinned.
    const MARKER: &str = r#"[egress]
token = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[spec.image]
name = "ghcr.io/org/name:latest"
digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[spec.resources]
cpus = 1
"#;

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");

        let mut bytes = [0; 3];
        assert_eq!(unhex("00abff", &mut bytes), Some(()));
        assert_eq!(bytes, [0x00, 0xab, 0xff]);
    }

    #[test]
    fn unhex_refuses_anything_but_two_hex_digits_per_byte() {
        // A digit outside hexadecimal, a sign, which `u8::from_str_radix` takes, too few digits,
        // too many, an odd count, and a pair that would split `é`, two bytes long, which must
        // fail without a panic.
        for text in ["0g00", "+a+b", "ab", "abcdef", "abcde", "aéa"] {
            assert_eq!(unhex(text, &mut [0; 2]), None, "{text:?}");
        }
    }

    #[test]
    fn a_token_is_64_hex_digits() {
        let token = Token::try_from("ab".repeat(32)).unwrap();

        assert_eq!(token, Token([0xab; 32]));
        assert_eq!(String::from(token), "ab".repeat(32));
        assert_eq!(
            Token::try_from("short".to_string()),
            Err("a token is 64 hexadecimal digits")
        );
    }

    #[test]
    fn debug_output_hides_the_token() {
        assert_eq!(format!("{:?}", Token([0xab; 32])), "Token(<redacted>)");
    }

    #[test]
    fn minting_draws_a_new_token() {
        assert_ne!(Token::mint().unwrap(), Token::mint().unwrap());
    }

    #[test]
    fn bind_picks_an_ephemeral_port() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = laid_out(&tmp);

        let state = State::bind(&dir).unwrap();

        // `egress/` itself is the layout's, which `state.rs` checks.
        let egress = dir.join("egress");
        assert_eq!(
            fs::read_to_string(egress.join("port")).unwrap(),
            format!("{}\n", state.port())
        );
        assert_eq!(
            fs::metadata(egress.join("port")).unwrap().mode() & 0o7777,
            0o600
        );
        // The decider listens on loopback alone, on the port the file names. The operating
        // system picked that port, so it is never 0.
        assert_eq!(
            state.listener().unwrap().local_addr().unwrap(),
            SocketAddr::from(([127, 0, 0, 1], state.port()))
        );
    }

    #[test]
    fn bind_reuses_the_persisted_port() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = laid_out(&tmp);
        // A port free again, as an earlier daemon leaves it when it exits.
        let (listener, port) = held_port();
        drop(listener);
        port_file(&dir, format!("{port}\n"));

        let state = State::bind(&dir).unwrap();

        assert_eq!(state.port(), port);
    }

    #[test]
    fn bind_refuses_a_held_port_while_a_marker_remains() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = laid_out(&tmp);
        let (_held, port) = held_port();
        port_file(&dir, format!("{port}\n"));
        marker(&dir, "alpha", MARKER);

        let Err(error) = State::bind(&dir) else {
            panic!("bind took a port another socket holds");
        };

        assert_eq!(
            error.to_string(),
            format!("failed to listen for egress decisions on 127.0.0.1:{port}")
        );
    }

    #[test]
    fn bind_moves_to_a_new_port_when_no_machine_depends_on_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = laid_out(&tmp);
        let (_held, port) = held_port();
        port_file(&dir, format!("{port}\n"));

        let state = State::bind(&dir).unwrap();

        assert_ne!(state.port(), port);
        assert_eq!(
            fs::read_to_string(dir.join("egress").join("port")).unwrap(),
            format!("{}\n", state.port())
        );
    }

    #[test]
    fn bind_picks_a_new_port_when_the_port_file_holds_no_port() {
        // An empty file, as a power loss can leave it beside the markers of running machines,
        // then one whose bytes are not UTF-8.
        for contents in [&b""[..], b"\xff\n"] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = laid_out(&tmp);
            port_file(&dir, contents);
            marker(&dir, "alpha", MARKER);

            let state = State::bind(&dir).unwrap();

            assert_eq!(
                fs::read_to_string(dir.join("egress").join("port")).unwrap(),
                format!("{}\n", state.port())
            );
        }
    }

    #[test]
    fn bind_loads_the_token_of_every_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = laid_out(&tmp);
        marker(&dir, "alpha", MARKER);
        marker(&dir, "beta", &with_token(&"b".repeat(64)));
        // A temporary file a write left behind, its marker complete, under a name no machine
        // can have.
        marker(&dir, ".tmpK3jx9a", &with_token(&"c".repeat(64)));

        let state = State::bind(&dir).unwrap();

        // `MARKER` has no `[spec.egress]`, so its lists are the default ones.
        assert_eq!(
            state.lookup(&Token([0xaa; 32])),
            Some(("alpha".to_string(), Egress::default()))
        );
        assert_eq!(
            state.lookup(&Token([0xbb; 32])),
            Some(("beta".to_string(), Egress::default()))
        );
        assert_eq!(state.lookup(&Token([0xcc; 32])), None);
    }

    #[test]
    fn bind_skips_a_marker_it_cannot_read() {
        // No marker at all, then one whose token is no token.
        for text in ["not a marker\n".to_string(), with_token("short")] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = laid_out(&tmp);
            marker(&dir, "odd", &text);

            let state = State::bind(&dir).unwrap();

            // The marker stays as it was, and no entry holds a zeroed token in its place.
            assert_eq!(
                fs::read_to_string(dir.join("machines").join("odd")).unwrap(),
                text
            );
            assert_eq!(state.lookup(&Token([0; 32])), None);
        }
    }

    #[test]
    fn register_returns_the_binding_and_answers_for_its_token() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = laid_out(&tmp);
        let state = State::bind(&dir).unwrap();

        let binding = state.register("alpha", &minted(0xaa, &["api.github.com"]));

        assert_eq!(
            binding,
            EgressEndpoint {
                address: SocketAddr::from(([127, 0, 0, 1], state.port())),
                token: "aa".repeat(32),
            }
        );
        assert_eq!(
            state.lookup(&Token([0xaa; 32])),
            Some((
                "alpha".to_string(),
                Egress {
                    allow: vec![Pattern::parse("api.github.com").unwrap()],
                    deny: vec![],
                }
            ))
        );
    }

    #[test]
    fn registering_again_replaces_the_token() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = laid_out(&tmp);
        let state = State::bind(&dir).unwrap();

        state.register("alpha", &minted(0xaa, &[]));
        state.register("alpha", &minted(0xbb, &[]));

        assert_eq!(state.lookup(&Token([0xaa; 32])), None);
        assert_eq!(
            state.lookup(&Token([0xbb; 32])),
            Some(("alpha".to_string(), Egress::default()))
        );
    }

    #[test]
    fn retire_removes_the_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = laid_out(&tmp);
        let state = State::bind(&dir).unwrap();
        state.register("alpha", &minted(0xaa, &[]));

        state.retire("alpha");

        assert_eq!(state.lookup(&Token([0xaa; 32])), None);
    }

    /// Lays out a state directory as `serve_in` does, as `state` under the temporary `dir`:
    /// `prepare_in` creates it private, where `dir` keeps `tempfile`'s default permissions.
    fn laid_out(dir: &TempDir) -> PathBuf {
        let uid = rustix::process::geteuid().as_raw();
        crate::daemon::state::prepare_in(&dir.path().join("state"), uid).unwrap()
    }

    /// `MARKER` with `token` in place of its own: the first run of 64 `a`s, before the digest's.
    fn with_token(token: &str) -> String {
        MARKER.replacen(&"a".repeat(64), token, 1)
    }

    /// Writes `text` as the marker of `name` in the state directory `dir`.
    fn marker(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join("machines").join(name), text).unwrap();
    }

    /// Writes `contents` as `egress/port` in the state directory `dir`.
    fn port_file(dir: &Path, contents: impl AsRef<[u8]>) {
        fs::write(dir.join("egress").join("port"), contents).unwrap();
    }

    /// Binds `127.0.0.1:0`, and returns the listener, which holds the port while it lives, and
    /// the port.
    fn held_port() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    /// A marker whose token is `token` 32 times, around a spec from `machine::Spec::default()`
    /// whose `allow` holds `allow`.
    fn minted(token: u8, allow: &[&str]) -> Marker {
        Marker {
            egress: MarkerEgress {
                token: Token([token; 32]),
            },
            spec: machine::Spec {
                egress: Egress {
                    allow: allow.iter().map(|p| Pattern::parse(p).unwrap()).collect(),
                    deny: vec![],
                },
                ..machine::Spec::default()
            },
        }
    }
}
