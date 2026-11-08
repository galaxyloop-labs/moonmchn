use serde::{Deserialize, Serialize};

/// VM sizing and optional devices.
///
/// Unset settings let the backend choose its defaults. Memory sizes are in MiB;
/// disk sizes are in GiB.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Resources {
    /// Virtual CPU count.
    pub cpus: Option<u8>,
    /// Guest memory in MiB.
    pub memory: Option<u32>,
    /// Persistent data disk size in GiB; unset keeps the engine default.
    pub storage: Option<u64>,
    /// Image overlay size in GiB; unset keeps the engine default.
    pub overlay: Option<u64>,
    /// Expose a GPU to the guest.
    pub gpu: Option<bool>,
    /// GPU shared-memory region size in MiB, not detected physical VRAM.
    pub gpu_vram: Option<u32>,
    /// Expose CUDA via API remoting.
    pub cuda: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::Resources;

    fn fully_configured() -> Resources {
        Resources {
            cpus: Some(4),
            memory: Some(4096),
            storage: Some(20),
            overlay: Some(10),
            gpu: Some(true),
            gpu_vram: Some(2048),
            cuda: Some(true),
        }
    }

    /// The on-disk form of [`fully_configured`]: the file format is a user-facing
    /// contract, so it is pinned verbatim and checked in both directions.
    const FULLY_CONFIGURED: &str = "\
cpus = 4
memory = 4096
storage = 20
overlay = 10
gpu = true
gpu_vram = 2048
cuda = true
";

    #[test]
    fn empty_table_leaves_every_setting_unset() {
        let resources: Resources = toml::from_str("").unwrap();
        assert_eq!(
            resources,
            Resources {
                cpus: None,
                memory: None,
                storage: None,
                overlay: None,
                gpu: None,
                gpu_vram: None,
                cuda: None,
            }
        );
        assert_eq!(resources, Resources::default());
    }

    #[test]
    fn each_setting_deserializes_independently_under_its_own_key() {
        for (contents, expected) in [
            (
                "cpus = 4",
                Resources {
                    cpus: Some(4),
                    ..Resources::default()
                },
            ),
            (
                "memory = 4096",
                Resources {
                    memory: Some(4096),
                    ..Resources::default()
                },
            ),
            (
                "storage = 20",
                Resources {
                    storage: Some(20),
                    ..Resources::default()
                },
            ),
            (
                "overlay = 10",
                Resources {
                    overlay: Some(10),
                    ..Resources::default()
                },
            ),
            (
                "gpu = true",
                Resources {
                    gpu: Some(true),
                    ..Resources::default()
                },
            ),
            (
                "gpu_vram = 2048",
                Resources {
                    gpu_vram: Some(2048),
                    ..Resources::default()
                },
            ),
            (
                "cuda = true",
                Resources {
                    cuda: Some(true),
                    ..Resources::default()
                },
            ),
        ] {
            assert_eq!(
                toml::from_str::<Resources>(contents).unwrap(),
                expected,
                "{contents}"
            );
        }
    }

    #[test]
    fn accepts_the_full_range_of_every_numeric_setting() {
        // No validation here: zero is accepted and left to the backend.
        let smallest: Resources =
            toml::from_str("cpus = 0\nmemory = 0\nstorage = 0\noverlay = 0\ngpu_vram = 0\n")
                .unwrap();
        assert_eq!(
            smallest,
            Resources {
                cpus: Some(0),
                memory: Some(0),
                storage: Some(0),
                overlay: Some(0),
                gpu_vram: Some(0),
                ..Resources::default()
            }
        );

        // Disk sizes cover the whole u64 range, beyond TOML's signed 64-bit
        // `Value`, so no clamping is ever applied on the way in.
        let largest: Resources = toml::from_str(
            "cpus = 255\nmemory = 4294967295\nstorage = 18446744073709551615\n\
             overlay = 18446744073709551615\ngpu_vram = 4294967295\n",
        )
        .unwrap();
        assert_eq!(
            largest,
            Resources {
                cpus: Some(255),
                memory: Some(4_294_967_295),
                storage: Some(u64::MAX),
                overlay: Some(u64::MAX),
                gpu_vram: Some(4_294_967_295),
                ..Resources::default()
            }
        );
    }

    #[test]
    fn rejects_values_outside_each_numeric_range() {
        for contents in [
            "cpus = -1",
            "cpus = 256",
            "memory = -1",
            "memory = 4294967296",
            "gpu_vram = -1",
            "gpu_vram = 4294967296",
            "storage = -1",
            "storage = 18446744073709551616",
            "overlay = -1",
            "overlay = 18446744073709551616",
        ] {
            assert!(
                toml::from_str::<Resources>(contents).is_err(),
                "accepted {contents}"
            );
        }
    }

    #[test]
    fn rejects_mistyped_values() {
        for contents in [
            "cpus = 4.0",
            "cpus = '4'",
            "memory = '4096'",
            "memory = true",
            "storage = '20'",
            "overlay = 10.5",
            "gpu = 'yes'",
            "gpu_vram = []",
            "cuda = 0",
        ] {
            // Check the reason, so a renamed key cannot keep this passing as
            // an unknown field.
            let err = toml::from_str::<Resources>(contents).unwrap_err();
            assert!(
                err.message().starts_with("invalid type"),
                "{contents}: {err}"
            );
        }
    }

    #[test]
    fn rejects_unknown_fields_including_backend_names() {
        // Backend field names must not silently pass as aliases.
        for contents in [
            "vcpus = 4",
            "memory_mb = 2048",
            "memory_mib = 2048",
            "storage_gib = 20",
            "overlay_gib = 10",
            "gpu_vram_mib = 1024",
            "image = 'ubuntu:24.04'",
        ] {
            assert!(
                toml::from_str::<Resources>(contents).is_err(),
                "accepted {contents}"
            );
        }
    }

    #[test]
    fn unset_settings_are_omitted_from_toml() {
        assert_eq!(toml::to_string(&Resources::default()).unwrap(), "");

        let partial = Resources {
            cpus: Some(4),
            gpu: Some(false),
            ..Resources::default()
        };
        let contents = toml::to_string(&partial).unwrap();
        assert_eq!(contents, "cpus = 4\ngpu = false\n");
        assert_eq!(toml::from_str::<Resources>(&contents).unwrap(), partial);
    }

    #[test]
    fn fully_configured_resources_round_trip_with_every_key() {
        let resources = fully_configured();
        assert_eq!(toml::to_string(&resources).unwrap(), FULLY_CONFIGURED);
        assert_eq!(
            toml::from_str::<Resources>(FULLY_CONFIGURED).unwrap(),
            resources
        );
    }

    #[test]
    fn disk_sizes_beyond_the_signed_64_bit_range_survive_a_typed_round_trip() {
        // The untyped `toml::Value` cannot hold these, so a serialized file is
        // only guaranteed to load back through the typed configuration.
        for size in [9_223_372_036_854_775_808, u64::MAX] {
            let resources = Resources {
                storage: Some(size),
                overlay: Some(size),
                ..Resources::default()
            };
            let contents = toml::to_string(&resources).unwrap();
            assert_eq!(contents, format!("storage = {size}\noverlay = {size}\n"));
            assert_eq!(toml::from_str::<Resources>(&contents).unwrap(), resources);
        }
    }
}
