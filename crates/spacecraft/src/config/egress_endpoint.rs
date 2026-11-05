use std::fmt;
use std::net::SocketAddr;

/// Where a machine's gateway reaches the daemon's decider, which it asks about every flow, and
/// the token the machine presents there.
///
/// `address` is the same for every machine. `token` is the machine's: 64 hex digits minted when
/// it boots, handed to the engine through its environment, written nowhere but the machine's
/// marker, and redacted by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct EgressEndpoint {
    /// The decider's listener on the host's loopback, with its port.
    pub address: SocketAddr,
    /// The machine's token: 64 hexadecimal digits.
    pub token: String,
}

impl fmt::Debug for EgressEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressEndpoint")
            .field("address", &self.address)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::EgressEndpoint;

    /// A token of the documented shape: 64 hexadecimal digits.
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn endpoint(address: &str) -> EgressEndpoint {
        EgressEndpoint {
            address: address.parse().unwrap(),
            token: TOKEN.into(),
        }
    }

    #[test]
    fn debug_output_redacts_the_token_but_keeps_the_address() {
        let endpoint = endpoint("127.0.0.1:9000");
        for debug in [format!("{endpoint:?}"), format!("{endpoint:#?}")] {
            assert!(debug.contains("127.0.0.1:9000"), "{debug}");
            assert!(debug.contains("token"), "{debug}");
            assert!(debug.contains("<redacted>"), "{debug}");
            assert!(!debug.contains(TOKEN), "{debug}");
        }
    }
}
