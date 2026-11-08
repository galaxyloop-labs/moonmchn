//! Moon MCHN's configuration.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

use crate::dirs;
use crate::{Error, Result};

mod accelerators;
mod byte_size;
mod egress;
mod egress_endpoint;
mod host_cpus;
mod host_memory;
mod mount;
mod port;
mod resources;

use accelerators::Accelerators;
pub use byte_size::ByteSize;
use byte_size::Mib;
pub use egress::{Egress, Pattern, Subject};
pub use egress_endpoint::EgressEndpoint;
use host_cpus::HostCPUs;
use host_memory::HostMemory;
pub use mount::Mount;
pub use port::Port;
pub use resources::Resources;

/// Configuration
///
/// Optional settings are omitted from TOML when unset, leaving backend defaults; `mounts` and
/// the `[egress]` table, both lists included, are always written. [`Self::default`] leaves
/// settings unset and allows no host; [`Self::recommended`] sizes resources for the current host
/// and allows every host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Directories exposed inside the guest; no host files are shared by default.
    pub mounts: Vec<Mount>,
    /// VM sizing and optional devices.
    pub resources: Resources,
    /// The hosts a machine's guest may reach, and those it may never reach. A file without
    /// `[egress]` allows nothing.
    pub egress: Egress,
}

impl Config {
    /// Chooses defaults using the current host's capacity.
    ///
    /// Assigns half the available logical CPUs (rounded down, bounded to 1–8)
    /// and one quarter of total host RAM (bounded to 512–8192 MiB).
    ///
    /// Networking is enabled.
    ///
    /// GPU graphics and NVIDIA CUDA support are detected separately
    /// on supported platforms; unavailable or failed probes leave them disabled.
    ///
    /// No mounts are configured; that is left to the user.
    ///
    /// Every host is allowed: `[egress]` holds `allow = ["*"]` and `deny = []`.
    ///
    /// GPU shared memory is capped at 4096 MiB, one eighth of host RAM, and
    /// half of reported dedicated VRAM, rounded down to 256 MiB blocks
    /// (smaller positive budgets are kept).
    /// Unified or unknown GPU memory uses the host RAM limit.
    /// Without a graphics GPU, sizing remains unset.
    ///
    /// # Errors
    ///
    /// Returns an error if CPU availability or total host memory is unavailable,
    /// if the reported available CPU count exceeds 255, or if the suggested guest
    /// memory cannot be represented in the configuration.
    pub fn recommended() -> Result<Self> {
        let cpus = HostCPUs::detect()?;
        let memory = HostMemory::detect()?;
        let acc = accelerators::detect();

        Self::for_host(cpus, memory, acc)
    }

    /// Loads the configuration from the default location.
    ///
    /// # Errors
    ///
    /// - [`Error::ConfigNotFound`] if the file does not exist.
    /// - [`Error::ConfigDeserialization`] if the file is not valid TOML.
    /// - [`Error::Io`] if the file cannot be read for any other reason.
    pub fn load() -> Result<Self> {
        let path = dirs::config_dir()?.join(dirs::CONFIG_FILE_NAME);
        Self::load_from(&path)
    }

    /// Loads a configuration from a file.
    ///
    /// # Errors
    ///
    /// - [`Error::ConfigNotFound`] if the file does not exist.
    /// - [`Error::ConfigDeserialization`] if the file is not valid TOML.
    /// - [`Error::Io`] if the file cannot be read for any other reason.
    pub(crate) fn load_from(path: &Path) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(Error::ConfigNotFound(path.to_path_buf()));
            }
            Err(source) => return Err(Error::io(path, source)),
        };

        toml::from_str(&contents).map_err(|source| Error::ConfigDeserialization {
            path: path.to_path_buf(),
            source,
        })
    }

    fn for_host(cpus: HostCPUs, memory: HostMemory, accelerators: Accelerators) -> Result<Self> {
        Ok(Self {
            resources: Resources {
                cpus: Some(cpus.suggested()),
                memory: Some(u32::from(memory.suggested()?)),
                gpu: Some(accelerators.gpu),
                gpu_vram: accelerators.suggested_vram(memory.total()).map(u32::from),
                cuda: Some(accelerators.cuda),
                ..Resources::default()
            },
            // ponytail: every host until the approver lands. Without prompts, an empty `allow`
            // denies every flow; with them, `setup` can write `Egress::default()` and ask about
            // every flow (sandboxing spec, Sequencing step 3).
            egress: Egress::unrestricted(),
            ..Self::default()
        })
    }

    /// Serializes these defaults as readable TOML.
    ///
    /// # Errors
    ///
    /// Returns an error if a value cannot be represented in TOML.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(Error::ConfigSerialization)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Accelerators, Config, Egress, HostCPUs, HostMemory, Mib, Mount, Pattern, Resources,
    };
    use crate::ByteSize;
    use crate::Error;
    use std::num::NonZero;

    fn host_cpus(count: usize) -> HostCPUs {
        HostCPUs::from_count(NonZero::new(count).unwrap()).unwrap()
    }

    #[test]
    fn host_sizing_reserves_capacity_and_caps_large_hosts() {
        // Host CPUs, host RAM in MiB, guest CPUs, guest RAM in MiB.
        for (cpus, memory, expected_cpus, expected_memory) in [
            (1, 1024, 1, 512),
            (3, 4096, 1, 1024),
            (8, 16384, 4, 4096),
            (16, 32768, 8, 8192),
            (64, 262_144, 8, 8192),
        ] {
            let config = Config::for_host(
                host_cpus(cpus),
                HostMemory::from_size(ByteSize::checked_from_mib(memory).unwrap()).unwrap(),
                Accelerators::default(),
            )
            .unwrap();
            assert_eq!(config.resources.cpus, Some(expected_cpus));
            assert_eq!(config.resources.memory, Some(expected_memory));
        }
    }

    #[test]
    fn sizing_does_not_overflow_on_extreme_capacity() {
        let config = Config::for_host(
            host_cpus(usize::from(u8::MAX)),
            HostMemory::from_size(ByteSize::from_bytes(u64::MAX)).unwrap(),
            Accelerators::default(),
        )
        .unwrap();
        assert_eq!(config.resources.cpus, Some(8));
        assert_eq!(config.resources.memory, Some(8192));
    }

    #[test]
    fn recommendations_serialize_as_real_toml_without_runtime_bindings() {
        let config = Config::for_host(
            host_cpus(8),
            HostMemory::from_size(ByteSize::checked_from_gib(16).unwrap()).unwrap(),
            Accelerators::default(),
        )
        .unwrap();
        let contents = config.to_toml().unwrap();

        // `mounts`, `resources` and `egress` at the top level, unset settings omitted, and every
        // host allowed: what `setup` writes.
        assert_eq!(
            contents,
            concat!(
                "mounts = []\n\n",
                "[resources]\ncpus = 4\nmemory = 4096\ngpu = false\ncuda = false\n\n",
                "[egress]\nallow = [\"*\"]\ndeny = []\n",
            )
        );
        assert_eq!(toml::from_str::<Config>(&contents).unwrap(), config);
    }

    #[test]
    fn graphics_and_cuda_are_independent_in_toml() {
        for (gpu, cuda) in [(false, false), (true, false), (false, true), (true, true)] {
            let config = Config::for_host(
                host_cpus(8),
                HostMemory::from_size(ByteSize::checked_from_gib(16).unwrap()).unwrap(),
                Accelerators {
                    gpu,
                    cuda,
                    ..Accelerators::default()
                },
            )
            .unwrap();
            let contents = config.to_toml().unwrap();
            let decoded: Config = toml::from_str(&contents).unwrap();
            assert_eq!(decoded, config);
            assert_eq!(decoded.resources.gpu, Some(gpu));
            assert_eq!(decoded.resources.cuda, Some(cuda));
            assert_eq!(decoded.resources.gpu_vram, gpu.then_some(2048));
        }
    }

    #[test]
    fn dedicated_vram_limits_survive_toml_serialization() {
        let config = Config::for_host(
            host_cpus(16),
            HostMemory::from_size(ByteSize::checked_from_gib(64).unwrap()).unwrap(),
            Accelerators {
                gpu: true,
                cuda: true,
                dedicated_memory: Some(Mib::new(2048)),
            },
        )
        .unwrap();
        let contents = config.to_toml().unwrap();
        let decoded: Config = toml::from_str(&contents).unwrap();
        assert_eq!(decoded.resources.gpu_vram, Some(1024));
        assert_eq!(decoded, config);
    }

    fn fully_configured() -> Config {
        Config {
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
            egress: Egress {
                allow: vec![
                    Pattern::parse("api.github.com").unwrap(),
                    Pattern::parse("*.pypi.org").unwrap(),
                    Pattern::parse("10.0.0.5").unwrap(),
                ],
                deny: vec![Pattern::parse("upload.pypi.org").unwrap()],
            },
        }
    }

    /// The on-disk form of [`fully_configured`]: the file format is a user-facing
    /// contract, so it is pinned verbatim and checked in both directions.
    const FULLY_CONFIGURED: &str = r#"[[mounts]]
source = "/host/path with spaces"
target = "/workspace"
read_only = false
staged = true

[[mounts]]
source = "s3://bucket/models"
target = "/models"
read_only = true
staged = false

[resources]
cpus = 4
memory = 4096
storage = 20
overlay = 10
gpu = false
gpu_vram = 2048
cuda = false

[egress]
allow = [
    "api.github.com",
    "*.pypi.org",
    "10.0.0.5",
]
deny = ["upload.pypi.org"]
"#;

    #[test]
    fn every_setting_deserializes_from_toml() {
        assert_eq!(
            toml::from_str::<Config>(FULLY_CONFIGURED).unwrap(),
            fully_configured()
        );
    }

    #[test]
    fn all_nested_configuration_round_trips() {
        assert_eq!(fully_configured().to_toml().unwrap(), FULLY_CONFIGURED);
    }

    #[test]
    fn omitted_settings_leave_backend_choices_unset() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.mounts, []);
        assert_eq!(config.resources, Resources::default());

        // A file without `[egress]` allows nothing, and the table is written out all the same.
        let contents = config.to_toml().unwrap();
        assert_eq!(
            contents,
            "mounts = []\n\n[resources]\n\n[egress]\nallow = []\ndeny = []\n"
        );
        assert_eq!(toml::from_str::<Config>(&contents).unwrap(), config);
    }

    #[test]
    fn omitted_mount_modes_remain_writable_and_unstaged() {
        let config: Config =
            toml::from_str("[[mounts]]\nsource = '/host'\ntarget = '/workspace'").unwrap();
        assert_eq!(
            config.mounts,
            [Mount {
                source: "/host".into(),
                target: "/workspace".into(),
                read_only: false,
                staged: false,
            }]
        );
        assert_eq!(config.resources, Resources::default());
    }

    #[test]
    fn rejects_unknown_fields_and_incomplete_mounts() {
        for contents in [
            "image = 'ubuntu:24.04'",
            "[resources]\nmemory_mb = 2048",
            "[[mounts]]\ntarget = '/workspace'",
            "[[mounts]]\nsource = '/host'\ntarget = '/workspace'\nreadonly = true",
        ] {
            assert!(toml::from_str::<Config>(contents).is_err());
        }
    }

    #[test]
    fn load_from_reads_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, FULLY_CONFIGURED).unwrap();
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config, fully_configured());
    }

    #[test]
    fn load_from_reports_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.toml");

        let error = Config::load_from(&path).unwrap_err();

        assert!(matches!(error, Error::ConfigNotFound(missing) if missing == path));
    }

    #[test]
    fn load_from_reports_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.toml");
        std::fs::write(&path, "cpus = [").unwrap();

        let error = Config::load_from(&path).unwrap_err();

        assert!(error.to_string().contains(&path.display().to_string()));
        assert!(matches!(
            error,
            Error::ConfigDeserialization { path: failed, .. } if failed == path
        ));
    }

    #[test]
    fn a_bad_pattern_fails_to_load_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[egress]\nallow = [\"foo bar\"]\n").unwrap();

        let error = Config::load_from(&path).unwrap_err();

        assert!(matches!(error, Error::ConfigDeserialization { .. }));
        // The pattern is named by the TOML error underneath, the way the CLI prints the chain.
        let chain = format!("{error}: {}", std::error::Error::source(&error).unwrap());
        assert!(
            chain.contains(r#"invalid egress pattern "foo bar""#),
            "{chain}"
        );
    }
}
