use serde::{Deserialize, Serialize};

/// A host directory or remote volume exposed inside the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// Host directory or remote URL, such as `s3://bucket/prefix`.
    pub source: String,
    /// Absolute path inside the guest.
    pub target: String,
    /// Expose the guest view read-only.
    #[serde(default)]
    pub read_only: bool,
    /// Copy a local source into guest storage; requires writable mode.
    #[serde(default)]
    pub staged: bool,
}

#[cfg(test)]
mod tests {
    use super::Mount;

    fn mount(read_only: bool, staged: bool) -> Mount {
        Mount {
            source: "/host/path with spaces".into(),
            target: "/workspace".into(),
            read_only,
            staged,
        }
    }

    #[test]
    fn requires_source_and_target() {
        for contents in [
            "",
            "source = '/host'",
            "target = '/workspace'",
            "read_only = true\nstaged = false",
        ] {
            assert!(
                toml::from_str::<Mount>(contents).is_err(),
                "accepted {contents:?}"
            );
        }
    }

    #[test]
    fn omitted_modes_default_to_writable_and_unstaged() {
        let mount: Mount = toml::from_str("source = '/host'\ntarget = '/workspace'").unwrap();
        assert_eq!(
            mount,
            Mount {
                source: "/host".into(),
                target: "/workspace".into(),
                read_only: false,
                staged: false,
            }
        );
    }

    #[test]
    fn every_mode_combination_deserializes_without_validation() {
        // Mode conflicts such as a staged read-only mount are the backend's
        // concern; the configuration type preserves what was written.
        for (read_only, staged) in [(false, false), (true, false), (false, true), (true, true)] {
            let contents = format!(
                "source = '/host/path with spaces'\ntarget = '/workspace'\n\
                 read_only = {read_only}\nstaged = {staged}\n"
            );
            assert_eq!(
                toml::from_str::<Mount>(&contents).unwrap(),
                mount(read_only, staged)
            );
        }
    }

    #[test]
    fn rejects_non_boolean_modes() {
        for mode in [
            "read_only = 'true'",
            "read_only = 1",
            "staged = 'yes'",
            "staged = 0",
        ] {
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
            "stage = true",
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
                staged: false,
            };
            let contents = toml::to_string(&mount).unwrap();
            let values: toml::Value = toml::from_str(&contents).unwrap();
            assert_eq!(values["source"].as_str(), Some(source));
            assert_eq!(values["target"].as_str(), Some(target));
            assert_eq!(toml::from_str::<Mount>(&contents).unwrap(), mount);
        }
    }

    #[test]
    fn serialization_writes_both_modes_explicitly() {
        // Generated files spell out each mode so readers never rely on defaults.
        for (read_only, staged) in [(false, false), (true, true)] {
            let mount = mount(read_only, staged);
            let contents = toml::to_string(&mount).unwrap();
            let values: toml::Value = toml::from_str(&contents).unwrap();
            let keys: Vec<_> = values.as_table().unwrap().keys().collect();
            assert_eq!(keys, ["read_only", "source", "staged", "target"]);
            assert_eq!(values["read_only"].as_bool(), Some(read_only));
            assert_eq!(values["staged"].as_bool(), Some(staged));
            assert_eq!(toml::from_str::<Mount>(&contents).unwrap(), mount);
        }
    }
}
