use std::fmt;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// A trusted local egress service, configured only when one actually exists.
///
/// Tokens are serialized verbatim but redacted from debug output. Protect any
/// manually configured token as a secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressInterceptor {
    /// Host loopback listener, including its port.
    pub address: SocketAddr,
    /// Shared token consisting of 64 hexadecimal digits.
    pub token: String,
}

impl fmt::Debug for EgressInterceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressInterceptor")
            .field("address", &self.address)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::EgressInterceptor;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    /// A token of the documented shape: 64 hexadecimal digits.
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn interceptor(address: &str) -> EgressInterceptor {
        EgressInterceptor {
            address: address.parse().unwrap(),
            token: TOKEN.into(),
        }
    }

    fn toml_with_address(address: &str) -> String {
        format!("address = \"{address}\"\ntoken = \"{TOKEN}\"\n")
    }

    #[test]
    fn deserializes_ipv4_and_ipv6_socket_addresses() {
        for (address, ip) in [
            ("127.0.0.1:9000", IpAddr::V4(Ipv4Addr::LOCALHOST)),
            ("[::1]:9000", IpAddr::V6(Ipv6Addr::LOCALHOST)),
        ] {
            let interceptor: EgressInterceptor =
                toml::from_str(&toml_with_address(address)).unwrap();
            assert_eq!(interceptor.address.ip(), ip);
            assert_eq!(interceptor.address.port(), 9000);
            assert_eq!(interceptor.token, TOKEN);
        }
    }

    #[test]
    fn rejects_addresses_that_are_not_ip_socket_addresses() {
        // Host names are never resolved, IPv6 needs brackets, and the port is
        // mandatory: the listener address must be usable without lookups.
        for address in [
            "",
            "127.0.0.1",
            ":9000",
            "localhost:9000",
            "http://127.0.0.1:9000",
            "127.0.0.1:65536",
            "::1:9000",
        ] {
            assert!(
                toml::from_str::<EgressInterceptor>(&toml_with_address(address)).is_err(),
                "accepted {address:?}"
            );
        }
    }

    #[test]
    fn requires_both_address_and_token() {
        assert!(toml::from_str::<EgressInterceptor>("").is_err());
        assert!(toml::from_str::<EgressInterceptor>("address = \"127.0.0.1:9000\"\n").is_err());
        assert!(toml::from_str::<EgressInterceptor>(&format!("token = \"{TOKEN}\"\n")).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        for extra in [
            "port = 9000",
            "addr = \"127.0.0.1:9000\"",
            "secret = \"0123456789abcdef\"",
        ] {
            let contents = format!("{}{extra}\n", toml_with_address("127.0.0.1:9000"));
            assert!(
                toml::from_str::<EgressInterceptor>(&contents).is_err(),
                "accepted {extra:?}"
            );
        }
    }

    #[test]
    fn serializes_the_token_verbatim_and_round_trips() {
        // Redaction applies to debug output only; the file must keep the secret.
        for address in ["127.0.0.1:9000", "[::1]:9000"] {
            let interceptor = interceptor(address);
            let contents = toml::to_string(&interceptor).unwrap();
            let values: toml::Value = toml::from_str(&contents).unwrap();
            let keys: Vec<_> = values.as_table().unwrap().keys().collect();
            assert_eq!(keys, ["address", "token"]);
            assert_eq!(values["address"].as_str(), Some(address));
            assert_eq!(values["token"].as_str(), Some(TOKEN));
            assert_eq!(
                toml::from_str::<EgressInterceptor>(&contents).unwrap(),
                interceptor
            );
        }
    }

    #[test]
    fn debug_output_redacts_the_token_but_keeps_the_address() {
        let interceptor = interceptor("127.0.0.1:9000");
        for debug in [format!("{interceptor:?}"), format!("{interceptor:#?}")] {
            assert!(debug.contains("127.0.0.1:9000"), "{debug}");
            assert!(debug.contains("token"), "{debug}");
            assert!(debug.contains("<redacted>"), "{debug}");
            assert!(!debug.contains(TOKEN), "{debug}");
        }
    }
}
