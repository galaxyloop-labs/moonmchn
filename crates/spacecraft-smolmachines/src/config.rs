//! Translate Spacecraft configuration into `smolmachines` SDK types.
//!
//! Conversion preserves configured values and leaves unset settings at their SDK
//! defaults, with two departures: a registry image boots pinned to its recorded
//! digest, and every machine gets networking. It does not validate target-specific
//! constraints or create machines; the SDK remains responsible for those operations.
//!
//! # Example
//!
//! ```
//! use spacecraft::config::Resources;
//! use spacecraft::image::{Digest, Image};
//! use spacecraft::machine::{Config, Record};
//!
//! let config = Config {
//!     image: Image::parse("ghcr.io/spacecraft-dev/spacecraft:latest").unwrap(),
//!     mounts: Vec::new(),
//!     resources: Resources {
//!         cpus: Some(4),
//!         memory: Some(4096),
//!         ..Resources::default()
//!     },
//! };
//! let record = Record {
//!     digest: Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap(),
//!     config,
//! };
//! let machine = spacecraft_smolmachines::machine_config(record, "my-machine");
//! assert_eq!(machine.name, "my-machine");
//! assert_eq!(machine.resources.memory_mib, Some(4096));
//! ```

use spacecraft::{
    config::{EgressInterceptor, Mount, Port, Resources},
    machine::Record,
};

/// Converts a machine's record into an SDK config with a caller-supplied name.
///
/// A registry image boots as `host/path@sha256:hex`, pinned to the record's digest, and an
/// archive from its path. Settings outside the image, mounts and resources keep their SDK
/// defaults. Conversion is infallible and does not perform validation or machine creation.
#[must_use]
pub fn machine_config(record: Record, name: impl Into<String>) -> smolmachines::MachineConfig {
    smolmachines::MachineConfig {
        image: Some(record.config.image.pinned(&record.digest)),
        mounts: record.config.mounts.into_iter().map(mount).collect(),
        resources: resources(&record.config.resources),
        ..smolmachines::MachineConfig::new(name)
    }
}

/// Maps VM resources into SDK fields without changing their units or values.
///
/// `smolmachines` 1.29.1 retains GPU shared-memory and CUDA settings in its config,
/// but does not forward them during local machine creation.
#[must_use]
pub fn resources(resources: &Resources) -> smolmachines::Resources {
    smolmachines::Resources {
        cpus: resources.cpus,
        memory_mib: resources.memory,
        // Every machine gets networking: the engine refuses a registry image without it,
        // and the egress policy builds on it.
        network: Some(true),
        storage_gib: resources.storage,
        overlay_gib: resources.overlay,
        gpu: resources.gpu,
        // FIXME: smolmachines 1.29.1 does not forward this as --gpu-vram
        // during local creation. Upgrade or fix its argument mapping before
        // relying on the suggested shared-memory size at launch.
        gpu_vram_mib: resources.gpu_vram,
        // FIXME: smolmachines 1.29.1 stores this value but does not forward
        // --cuda during local machine creation. Upgrade or fix the SDK's
        // argument mapping before relying on this flag to enable CUDA.
        cuda: resources.cuda,
    }
}

/// Converts a mount without validating its source, target, or mode.
#[must_use]
pub fn mount(mount: Mount) -> smolmachines::Mount {
    smolmachines::Mount {
        source: mount.source,
        target: mount.target,
        read_only: mount.read_only,
        staged: mount.staged,
    }
}

/// Converts an inbound port forward.
#[must_use]
pub fn port(port: Port) -> smolmachines::Port {
    smolmachines::Port::new(port.host, port.guest)
}

/// Converts a runtime egress binding, preserving its address and token verbatim.
#[must_use]
pub fn egress_interceptor(interceptor: EgressInterceptor) -> smolmachines::EgressInterceptor {
    smolmachines::EgressInterceptor::new(interceptor.address, interceptor.token)
}

#[cfg(test)]
mod tests {
    use spacecraft::{
        config::{EgressInterceptor, Mount, Port, Resources},
        image::{Digest, Image},
        machine::{Config, Record},
    };

    use super::{egress_interceptor, machine_config, mount, port, resources};

    #[test]
    fn every_setting_is_preserved_from_config_to_sdk_config() {
        let record = Record {
            digest: Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            config: Config {
                image: Image::parse("ghcr.io/org/name:latest").unwrap(),
                mounts: vec![
                    Mount {
                        source: "/host/path with spaces".into(),
                        target: "/workspace".into(),
                        read_only: false,
                        staged: true,
                    },
                    Mount {
                        source: "s3://bucket/models".into(),
                        target: "/models".into(),
                        read_only: true,
                        staged: false,
                    },
                ],
                resources: Resources {
                    cpus: Some(4),
                    memory: Some(4096),
                    storage: Some(20),
                    overlay: Some(10),
                    gpu: Some(false),
                    gpu_vram: Some(2048),
                    cuda: Some(false),
                },
            },
        };
        let machine = machine_config(record, String::from("unique-machine-name"));

        assert_eq!(machine.name, "unique-machine-name");
        // The registry image boots pinned to the digest its tag resolved to.
        assert_eq!(
            machine.image,
            Some(format!("ghcr.io/org/name@sha256:{}", "a".repeat(64)))
        );
        assert_eq!(
            machine.mounts,
            [
                smolmachines::Mount::new("/host/path with spaces", "/workspace").staged(),
                smolmachines::Mount::new("s3://bucket/models", "/models").read_only(),
            ]
        );
        assert_eq!(
            machine.resources,
            smolmachines::Resources {
                cpus: Some(4),
                memory_mib: Some(4096),
                // Every machine gets networking: the engine refuses a registry image without
                // it, and the egress policy builds on it.
                network: Some(true),
                storage_gib: Some(20),
                overlay_gib: Some(10),
                gpu: Some(false),
                gpu_vram_mib: Some(2048),
                cuda: Some(false),
            }
        );
    }

    #[test]
    fn a_local_image_is_passed_as_its_path() {
        let record = Record {
            digest: Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            config: Config {
                image: Image::parse("/abs/guest.tar").unwrap(),
                mounts: Vec::new(),
                resources: Resources::default(),
            },
        };

        let machine = machine_config(record, "local");

        // A path has no pinned form: the engine boots the file, and the record keeps the
        // digest it had at boot.
        assert_eq!(machine.image, Some("/abs/guest.tar".to_string()));
    }

    #[test]
    fn a_minimal_config_retains_sdk_defaults() {
        let record = Record {
            digest: Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            config: Config {
                image: Image::parse("ghcr.io/org/name:latest").unwrap(),
                mounts: Vec::new(),
                resources: Resources::default(),
            },
        };
        let machine = machine_config(record, "bare");
        let defaults = smolmachines::MachineConfig::new("bare");

        assert_eq!(machine.name, defaults.name);
        // Networking and the pinned image, which the test above checks, are the deliberate
        // departures from the SDK defaults.
        assert_eq!(
            machine.resources,
            smolmachines::Resources {
                network: Some(true),
                ..defaults.resources
            }
        );
        assert_eq!(machine.mounts, defaults.mounts);
        assert_eq!(machine.egress_interceptor, defaults.egress_interceptor);
        assert_eq!(machine.wait_for_ports, defaults.wait_for_ports);
        // No command: the image's own entrypoint runs.
        assert_eq!(machine.command, defaults.command);
        assert_eq!(machine.env, defaults.env);
        assert_eq!(machine.workdir, defaults.workdir);
        assert_eq!(machine.user, defaults.user);
        assert_eq!(machine.ports, defaults.ports);
        assert_eq!(machine.labels, defaults.labels);
        assert_eq!(machine.allowed_hosts, defaults.allowed_hosts);
        assert_eq!(machine.persistent, defaults.persistent);
        assert_eq!(machine.branchable, defaults.branchable);
        assert_eq!(machine.allowed_cidrs, defaults.allowed_cidrs);
        assert_eq!(machine.auto_stop_seconds, defaults.auto_stop_seconds);
        assert_eq!(machine.ttl_seconds, defaults.ttl_seconds);
        assert_eq!(machine.arch, defaults.arch);
    }

    #[test]
    fn optional_flags_remain_independent_in_sdk_config() {
        for gpu in [None, Some(false), Some(true)] {
            for cuda in [None, Some(false), Some(true)] {
                let converted = resources(&Resources {
                    gpu,
                    cuda,
                    gpu_vram: gpu.filter(|enabled| *enabled).map(|_| 1024),
                    ..Resources::default()
                });
                assert_eq!(converted.gpu, gpu);
                assert_eq!(converted.cuda, cuda);
                assert_eq!(
                    converted.gpu_vram_mib,
                    gpu.filter(|enabled| *enabled).map(|_| 1024)
                );
            }
        }
    }

    #[test]
    fn resource_conversion_preserves_numeric_values_without_validation() {
        for (cpus, memory, disk) in [(0, 0, 0), (u8::MAX, u32::MAX, u64::MAX)] {
            let converted = resources(&Resources {
                cpus: Some(cpus),
                memory: Some(memory),
                storage: Some(disk),
                overlay: Some(disk),
                gpu_vram: Some(memory),
                ..Resources::default()
            });
            assert_eq!(converted.cpus, Some(cpus));
            assert_eq!(converted.memory_mib, Some(memory));
            assert_eq!(converted.storage_gib, Some(disk));
            assert_eq!(converted.overlay_gib, Some(disk));
            assert_eq!(converted.gpu_vram_mib, Some(memory));
        }
    }

    #[test]
    fn mount_conversion_preserves_modes_without_validation() {
        for read_only in [false, true] {
            for staged in [false, true] {
                let converted = mount(Mount {
                    source: "/host/path with spaces".into(),
                    target: "/workspace".into(),
                    read_only,
                    staged,
                });
                assert_eq!(converted.source, "/host/path with spaces");
                assert_eq!(converted.target, "/workspace");
                assert_eq!(converted.read_only, read_only);
                assert_eq!(converted.staged, staged);
            }
        }
    }

    #[test]
    fn ports_and_interceptors_convert_to_sdk_types() {
        for (host, guest) in [(8080, 80), (0, u16::MAX)] {
            assert_eq!(
                port(Port { host, guest }),
                smolmachines::Port::new(host, guest)
            );
        }
        for address in ["127.0.0.1:9000", "[::1]:9000"] {
            let binding = egress_interceptor(EgressInterceptor {
                address: address.parse().unwrap(),
                token: "0123456789abcdef".repeat(4),
            });
            assert_eq!(binding.address, address.parse().unwrap());
            assert_eq!(binding.token, "0123456789abcdef".repeat(4));
        }
    }
}
