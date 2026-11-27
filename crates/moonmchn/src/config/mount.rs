use serde::{Deserialize, Serialize};

/// A host directory exposed inside the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// Host directory, as an absolute path: `mchn start` refuses any other source.
    pub source: String,
    /// Absolute path inside the guest.
    pub target: String,
    /// Expose the guest view read-only.
    #[serde(default)]
    pub read_only: bool,
}

#[cfg(test)]
mod tests {
    use super::Mount;

    fn mount(read_only: bool) -> Mount {
        Mount {
            source: "/host/path with spaces".into(),
            target: "/workspace".into(),
            read_only,
        }
    }

    #[test]
    fn requires_source_and_target() {
        for contents in ["", "source = '/host'", "target = '/workspace'"] {
            assert!(
                toml::from_str::<Mount>(contents).is_err(),
                "accepted {contents:?}"
            );
        }
    }

    #[test]
    fn omitted_read_only_defaults_to_writable() {
        let mount: Mount = toml::from_str("source = '/host'\ntarget = '/workspace'").unwrap();
        assert_eq!(
            mount,
            Mount {
                source: "/host".into(),
                target: "/workspace".into(),
                read_only: false,
            }
        );
    }

    #[test]
    fn rejects_a_non_boolean_read_only() {
        for mode in ["read_only = 'true'", "read_only = 1"] {
            let contents = format!("source = '/host'\ntarget = '/workspace'\n{mode}\n");
            assert!(
                toml::from_str::<Mount>(&contents).is_err(),
                "accepted {mode:?}"
            );
        }
    }

    #[test]
    fn rejects_unknown_fields_including_mode_typos() {
        for extra in [
            "readonly = true",
            "read-only = true",
            "ro = true",
            "mode = 'ro'",
            "destination = '/workspace'",
        ] {
            let contents = format!("source = '/host'\ntarget = '/workspace'\n{extra}\n");
            assert!(
                toml::from_str::<Mount>(&contents).is_err(),
                "accepted {extra:?}"
            );
        }
    }

    #[test]
    fn sources_and_targets_round_trip_verbatim() {
        // Neither side is normalized, validated, or resolved against the host.
        for (source, target) in [
            ("/host/path with spaces", "/workspace"),
            ("s3://bucket/prefix", "/models"),
            ("relative/path", "/mnt/data"),
            ("C:\\Users\\me\\project", "/project"),
            ("/host/données", "/données"),
        ] {
            let mount = Mount {
                source: source.into(),
                target: target.into(),
                read_only: false,
            };
            let contents = toml::to_string(&mount).unwrap();
            let values: toml::Value = toml::from_str(&contents).unwrap();
            assert_eq!(values["source"].as_str(), Some(source));
            assert_eq!(values["target"].as_str(), Some(target));
            assert_eq!(toml::from_str::<Mount>(&contents).unwrap(), mount);
        }
    }

    #[test]
    fn serialization_writes_read_only_explicitly() {
        // Generated files spell out `read_only` so readers never rely on its default.
        for read_only in [false, true] {
            let mount = mount(read_only);
            let contents = toml::to_string(&mount).unwrap();
            let values: toml::Value = toml::from_str(&contents).unwrap();
            let keys: Vec<_> = values.as_table().unwrap().keys().collect();
            assert_eq!(keys, ["read_only", "source", "target"]);
            assert_eq!(values["read_only"].as_bool(), Some(read_only));
            assert_eq!(toml::from_str::<Mount>(&contents).unwrap(), mount);
        }
    }

    #[test]
    fn a_staged_key_is_refused() {
        let contents = "source = '/host'\ntarget = '/workspace'\nstaged = false";

        let error = toml::from_str::<Mount>(contents).unwrap_err();

        assert!(
            error.to_string().contains("unknown field `staged`"),
            "{error}"
        );
    }
}
