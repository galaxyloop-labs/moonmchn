//! The engine's gateway as the decider meets it: the `SMOLMEG2` preamble it writes before each
//! flow, the byte it reads back, the link every guest gets, and the host's own addresses.
//!
//! The preamble, big-endian: the magic (8 bytes), the machine's token (32), the machine id (16)
//! and the parent id (16), the destination's address family (1) and port (2), its address (4 or
//! 16), then the length of the opening bytes (2) and the bytes, at most 8 KiB. The decoder skips
//! both ids, since nothing reads them.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use spacecraft::machine::{self, Decoded, Flow, TOKEN_LEN};

/// The eight bytes a version 2 preamble opens with.
const PREAMBLE_MAGIC: [u8; 8] = *b"SMOLMEG2";

/// The length of each id after the token, in bytes: the machine id, then the parent id.
const ID_LEN: usize = 16;

/// The address family byte of an IPv4 destination, whose address takes four bytes.
const FAMILY_V4: u8 = 4;

/// The address family byte of an IPv6 destination, whose address takes sixteen bytes.
const FAMILY_V6: u8 = 6;

/// The length of the fields before the destination's address: the magic, the token, both ids,
/// the family and the port.
const HEAD_LEN: usize =
    PREAMBLE_MAGIC.len() + TOKEN_LEN + 2 * ID_LEN + size_of::<u8>() + size_of::<u16>();

/// The length of an IPv4 destination's address, in bytes.
const ADDRESS_V4_LEN: usize = 4;

/// The length of an IPv6 destination's address, in bytes.
const ADDRESS_V6_LEN: usize = 16;

/// The most opening bytes the engine sends in a preamble: a longer length is refused before any
/// of the bytes is asked for.
const OPENING_BYTES_MAX: usize = 8192;

/// The IPv4 half of the guest's own link, `100.96.0.0/30`: every machine gets `100.96.0.2`, with
/// its gateway at `100.96.0.1`.
const GUEST_LINK_V4: Ipv4Addr = Ipv4Addr::new(100, 96, 0, 0);

/// The prefix length of `GUEST_LINK_V4`, in bits.
const GUEST_LINK_V4_PREFIX: u32 = 30;

/// The IPv6 half of the guest's own link, `fd53:4d00::/64`: on a host with working IPv6, every
/// machine gets `fd53:4d00::2`, with its gateway at `fd53:4d00::1`.
const GUEST_LINK_V6: Ipv6Addr = Ipv6Addr::new(0xfd53, 0x4d00, 0, 0, 0, 0, 0, 0);

/// The prefix length of `GUEST_LINK_V6`, in bits.
const GUEST_LINK_V6_PREFIX: u32 = 64;

/// The port the host-address check binds: `0`, the operating system's "any free port", so a port
/// in use never fails the bind.
const ANY_PORT: u16 = 0;

/// The engine's gateway, which asks the decider about every flow before it dials one.
pub struct Gateway;

impl machine::Gateway for Gateway {
    type FlowError = PreambleError;

    /// The engine's `AllowDirect`: the gateway dials the destination itself.
    const ALLOW: u8 = 0;

    /// The engine's `Deny`: the gateway aborts the guest's flow.
    const DENY: u8 = 1;

    fn decode_flow(bytes: &[u8]) -> Result<Decoded, PreambleError> {
        let Some((magic, rest)) = bytes.split_first_chunk::<{ PREAMBLE_MAGIC.len() }>() else {
            return Ok(Decoded::Partial(PREAMBLE_MAGIC.len()));
        };
        if magic != &PREAMBLE_MAGIC {
            return Err(PreambleError::Magic);
        }

        let Some((token, rest)) = rest.split_first_chunk::<TOKEN_LEN>() else {
            return Ok(Decoded::Partial(HEAD_LEN));
        };
        let Some((_, [family, port_high, port_low, rest @ ..])) =
            rest.split_first_chunk::<{ 2 * ID_LEN }>()
        else {
            return Ok(Decoded::Partial(HEAD_LEN));
        };
        let port = u16::from_be_bytes([*port_high, *port_low]);

        let (ip, length, opening) = match *family {
            FAMILY_V4 => {
                let Some((address, [high, low, opening @ ..])) =
                    rest.split_first_chunk::<ADDRESS_V4_LEN>()
                else {
                    return Ok(Decoded::Partial(
                        HEAD_LEN + ADDRESS_V4_LEN + size_of::<u16>(),
                    ));
                };
                (
                    IpAddr::from(*address),
                    u16::from_be_bytes([*high, *low]),
                    opening,
                )
            }
            FAMILY_V6 => {
                let Some((address, [high, low, opening @ ..])) =
                    rest.split_first_chunk::<ADDRESS_V6_LEN>()
                else {
                    return Ok(Decoded::Partial(
                        HEAD_LEN + ADDRESS_V6_LEN + size_of::<u16>(),
                    ));
                };
                (
                    IpAddr::from(*address),
                    u16::from_be_bytes([*high, *low]),
                    opening,
                )
            }
            family => return Err(PreambleError::Family(family)),
        };
        if usize::from(length) > OPENING_BYTES_MAX {
            return Err(PreambleError::Oversize(length));
        }
        let Some(opening) = opening.get(..usize::from(length)) else {
            let opening_end = bytes.len() - opening.len() + usize::from(length);
            return Ok(Decoded::Partial(opening_end));
        };

        Ok(Decoded::Complete(Flow {
            token: *token,
            destination: SocketAddr::from((ip, port)),
            opening: opening.to_vec(),
        }))
    }

    fn is_guest_link(ip: IpAddr) -> bool {
        // Each mask is a `const` block, which the compiler evaluates when it builds this function:
        // a prefix the shift cannot take, 0 or longer than the address, fails the build (E0080),
        // so nothing is left to panic when the function runs.
        match ip {
            IpAddr::V4(ip) => {
                let mask = const { u32::MAX << (u32::BITS - GUEST_LINK_V4_PREFIX) };
                ip.to_bits() & mask == GUEST_LINK_V4.to_bits()
            }
            IpAddr::V6(ip) => {
                let mask = const { u128::MAX << (u128::BITS - GUEST_LINK_V6_PREFIX) };
                ip.to_bits() & mask == GUEST_LINK_V6.to_bits()
            }
        }
    }

    fn is_host_address(ip: IpAddr) -> bool {
        // An address the host can bind is one of its own; the socket drops at once.
        match UdpSocket::bind(SocketAddr::new(ip, ANY_PORT)) {
            Ok(_) => true,
            // `AddrNotAvailable` alone says that no interface holds the address. Any other error
            // counts as the host's, so a failed check denies the flow: running out of file
            // descriptors must never admit one.
            Err(error) => error.kind() != io::ErrorKind::AddrNotAvailable,
        }
    }
}

/// Why bytes start no `SMOLMEG2` preamble. `Display` gives the words that follow
/// `bad preamble,` in the decider's log line.
#[derive(Debug)]
pub enum PreambleError {
    /// The bytes open with other bytes than `PREAMBLE_MAGIC`.
    Magic,
    /// The address family byte is neither `FAMILY_V4` nor `FAMILY_V6`.
    Family(u8),
    /// The opening bytes length is over `OPENING_BYTES_MAX`.
    Oversize(u16),
}

impl fmt::Display for PreambleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PreambleError::Magic => write!(f, "wrong magic"),
            PreambleError::Family(family) => write!(f, "unknown address family {family}"),
            PreambleError::Oversize(length) => {
                write!(f, "opening bytes length {length} over {OPENING_BYTES_MAX}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spacecraft::machine::Gateway as _;

    #[test]
    fn decode_flow_reads_the_captured_header() {
        // The header the spike recorded on 2026-10-27: the magic, the token, the machine id, a
        // zero parent id, family 4, port 443, the address, and the length, 1544.
        let token: [u8; 32] = [
            0x78, 0xb1, 0x6e, 0x39, 0x7b, 0x0e, 0x47, 0xc2, 0xfc, 0xda, 0xf0, 0x2f, 0x79, 0xb8,
            0xb0, 0xe9, 0x67, 0x62, 0x5c, 0xb2, 0xd8, 0x8e, 0x19, 0xb2, 0xa3, 0x34, 0x62, 0xc4,
            0x9b, 0x34, 0xdc, 0x3f,
        ];
        let machine: [u8; 16] = [
            0xbf, 0xf6, 0x8d, 0x92, 0x8f, 0x81, 0xab, 0x32, 0xf7, 0x2d, 0xc9, 0x2e, 0xd8, 0x70,
            0x89, 0xf4,
        ];
        let mut bytes = b"SMOLMEG2".to_vec();
        bytes.extend(token);
        bytes.extend(machine);
        bytes.extend([0; 16]);
        bytes.push(4);
        bytes.extend([0x01, 0xbb]);
        bytes.extend([104, 20, 23, 154]);
        bytes.extend([0x06, 0x08]);
        // The decoder never looks inside the opening bytes, so any 1544 stand in for the
        // `ClientHello` that followed.
        bytes.extend([0x16; 1544]);

        let Ok(Decoded::Complete(flow)) = Gateway::decode_flow(&bytes) else {
            panic!("decode the captured preamble");
        };

        assert_eq!(flow.token, token);
        // The two ids are skipped: skipping more or fewer bytes shifts every field after them.
        assert_eq!(
            flow.destination,
            SocketAddr::from(([104, 20, 23, 154], 443))
        );
        assert_eq!(flow.opening, [0x16; 1544]);

        // Family 6 takes sixteen address bytes.
        let bytes = preamble_bytes(&[0xaa; 32], 6, 443, &Ipv6Addr::LOCALHOST.octets(), &[]);

        let Ok(Decoded::Complete(flow)) = Gateway::decode_flow(&bytes) else {
            panic!("decode an IPv6 preamble");
        };

        assert_eq!(
            flow.destination,
            SocketAddr::from((Ipv6Addr::LOCALHOST, 443))
        );
        assert!(flow.opening.is_empty());
    }

    #[test]
    fn decoding_asks_for_each_field_in_turn() {
        let bytes = preamble_bytes(&[0xaa; 32], 4, 443, &[192, 0, 2, 1], b"GET");

        // The magic, then the fields through the port, then the address and the length, then
        // the opening bytes.
        for (read, wanted) in [(0, 8), (8, 75), (75, 81), (81, 84)] {
            assert!(
                matches!(
                    Gateway::decode_flow(&bytes[..read]),
                    Ok(Decoded::Partial(len)) if len == wanted
                ),
                "after {read} bytes"
            );
        }
        assert!(matches!(
            Gateway::decode_flow(&bytes),
            Ok(Decoded::Complete(_))
        ));

        // An IPv6 address takes sixteen bytes.
        let bytes = preamble_bytes(&[0xaa; 32], 6, 443, &Ipv6Addr::LOCALHOST.octets(), &[]);

        assert!(matches!(
            Gateway::decode_flow(&bytes[..75]),
            Ok(Decoded::Partial(93))
        ));
    }

    #[test]
    fn wrong_magic_unknown_family_and_oversize_are_errors() {
        // A length over the limit with nothing after it: the decoder refuses it before it asks
        // for any opening byte.
        let mut oversize = preamble_bytes(&[0xaa; 32], 4, 443, &[192, 0, 2, 1], &[]);
        let length = oversize.len() - 2;
        oversize[length..].copy_from_slice(&8193_u16.to_be_bytes());

        for (bytes, what) in [
            // The magic alone.
            (b"SMOLMEG1".to_vec(), "wrong magic"),
            (
                preamble_bytes(&[0xaa; 32], 7, 443, &[192, 0, 2, 1], &[]),
                "unknown address family 7",
            ),
            (oversize, "opening bytes length 8193 over 8192"),
        ] {
            let Err(error) = Gateway::decode_flow(&bytes) else {
                panic!("decoded a flow from {bytes:?}");
            };
            assert_eq!(error.to_string(), what);
        }

        // 8192 bytes, the most the engine sends, are no error.
        let longest = preamble_bytes(&[0xaa; 32], 4, 443, &[192, 0, 2, 1], &[0; 8192]);
        assert!(matches!(
            Gateway::decode_flow(&longest),
            Ok(Decoded::Complete(_))
        ));
    }

    #[test]
    fn the_guest_link_has_both_halves() {
        for (ip, on_the_link) in [
            ("100.96.0.0", true),
            ("100.96.0.1", true),
            ("100.96.0.3", true),
            ("100.96.0.4", false),
            ("100.95.255.255", false),
            ("fd53:4d00::1", true),
            ("fd53:4d00::ffff:ffff:ffff:ffff", true),
            ("fd53:4d00:0:1::1", false),
            ("fd53:4d01::1", false),
        ] {
            assert_eq!(
                Gateway::is_guest_link(ip.parse().unwrap()),
                on_the_link,
                "{ip}"
            );
        }
    }

    #[test]
    fn the_host_owns_the_addresses_it_can_bind() {
        // Every host holds loopback, and none holds `192.0.2.1`, which RFC 5737 reserves for
        // documentation.
        assert!(Gateway::is_host_address(IpAddr::from([127, 0, 0, 1])));
        assert!(!Gateway::is_host_address(IpAddr::from([192, 0, 2, 1])));
    }

    #[test]
    fn replies_are_the_engines_bytes() {
        // The engine's `AllowDirect` and `Deny`.
        assert_eq!(Gateway::ALLOW, 0);
        assert_eq!(Gateway::DENY, 1);
    }

    /// A preamble as the engine writes one: the magic, `token`, a zero machine id and parent id,
    /// `family`, `port`, `address`, then the length of `opening` and `opening` itself. Each
    /// length comes from its slice, so a test that needs a wrong one edits the bytes.
    fn preamble_bytes(
        token: &[u8; 32],
        family: u8,
        port: u16,
        address: &[u8],
        opening: &[u8],
    ) -> Vec<u8> {
        let mut bytes = b"SMOLMEG2".to_vec();
        bytes.extend(token);
        bytes.extend([0; 32]);
        bytes.push(family);
        bytes.extend(port.to_be_bytes());
        bytes.extend(address);
        bytes.extend(u16::try_from(opening.len()).unwrap().to_be_bytes());
        bytes.extend(opening);
        bytes
    }
}
