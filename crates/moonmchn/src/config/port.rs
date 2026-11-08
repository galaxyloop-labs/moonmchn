use serde::{Deserialize, Serialize};

/// An inbound port forward; the cloud allocates its own host port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    /// Port listened on by the host for local machines.
    pub host: u16,
    /// Port served by the guest.
    pub guest: u16,
}

#[cfg(test)]
mod tests {
    use super::Port;

    #[test]
    fn requires_both_host_and_guest() {
        for contents in ["", "host = 8080", "guest = 80"] {
            assert!(
                toml::from_str::<Port>(contents).is_err(),
                "accepted {contents:?}"
            );
        }
    }

    #[test]
    fn accepts_the_full_16_bit_port_range() {
        for (host, guest) in [(0, 0), (8080, 80), (65535, 65535)] {
            let port: Port = toml::from_str(&format!("host = {host}\nguest = {guest}\n")).unwrap();
            assert_eq!(port, Port { host, guest });
        }
    }

    #[test]
    fn rejects_ports_outside_the_16_bit_range() {
        for contents in [
            "host = 65536\nguest = 80",
            "host = -1\nguest = 80",
            "host = 8080\nguest = 65536",
            "host = 8080\nguest = -1",
        ] {
            assert!(
                toml::from_str::<Port>(contents).is_err(),
                "accepted {contents:?}"
            );
        }
    }

    #[test]
    fn rejects_non_integer_ports() {
        for contents in [
            "host = '8080'\nguest = 80",
            "host = 8080.0\nguest = 80",
            "host = 8080\nguest = '80'",
            "host = 8080\nguest = true",
        ] {
            assert!(
                toml::from_str::<Port>(contents).is_err(),
                "accepted {contents:?}"
            );
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        for extra in ["protocol = 'tcp'", "host_port = 8080", "port = 80"] {
            let contents = format!("host = 8080\nguest = 80\n{extra}\n");
            assert!(
                toml::from_str::<Port>(&contents).is_err(),
                "accepted {extra:?}"
            );
        }
    }

    #[test]
    fn serializes_host_and_guest_under_their_own_keys_and_round_trips() {
        // Asymmetric values prove the two sides are never swapped.
        let port = Port {
            host: 8080,
            guest: 80,
        };
        let contents = toml::to_string(&port).unwrap();
        let values: toml::Value = toml::from_str(&contents).unwrap();
        let keys: Vec<_> = values.as_table().unwrap().keys().collect();
        assert_eq!(keys, ["guest", "host"]);
        assert_eq!(values["host"].as_integer(), Some(8080));
        assert_eq!(values["guest"].as_integer(), Some(80));
        assert_eq!(toml::from_str::<Port>(&contents).unwrap(), port);
    }
}
