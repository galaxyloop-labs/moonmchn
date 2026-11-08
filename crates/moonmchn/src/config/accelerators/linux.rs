//! Linux graphics and CUDA detection.
//!
//! Graphics means an accessible DRM render node (`/dev/dri/renderD*`) that can
//! be opened read/write. CUDA means an `nvidia-smi` device row with a valid
//! compute capability and an allowed compute mode; no toolkit is required.
//! Dedicated VRAM comes from the node's sysfs `mem_info_vram_total` (bytes) or,
//! failing that, from `nvidia-smi` matched by PCI address.
//!
//! Memory measurements are associated with accessible render devices so a
//! compute-only or inaccessible NVIDIA card cannot set the graphics memory
//! budget. Capacities of different GPUs are never added together.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use super::{Accelerators, Mib};
#[cfg(target_os = "linux")]
use super::{PROBE_TIMEOUT, probe};
use crate::ByteSize;

/// Resolved through `PATH`; a missing binary means no usable NVIDIA driver.
#[cfg(target_os = "linux")]
const NVIDIA_SMI_PROGRAM: &str = "nvidia-smi";
/// One headerless CSV row per device: `compute_cap, compute_mode`.
#[cfg(target_os = "linux")]
const NVIDIA_COMPUTE_ARGS: &[&str] = &[
    "--query-gpu=compute_cap,compute_mode",
    "--format=csv,noheader,nounits",
];
/// One headerless CSV row per device: `pci.bus_id, memory.total` in MiB.
#[cfg(target_os = "linux")]
const NVIDIA_MEMORY_ARGS: &[&str] = &[
    "--query-gpu=pci.bus_id,memory.total",
    "--format=csv,noheader,nounits",
];
/// Device nodes; only the `renderD*` render nodes are considered.
#[cfg(target_os = "linux")]
const DRM_DEVICE_PATH: &str = "/dev/dri";
/// Per-node sysfs entries whose `device` link resolves to the PCI device.
#[cfg(target_os = "linux")]
const DRM_SYSFS_PATH: &str = "/sys/class/drm";

/// Compute modes that let this process create a CUDA context. `Prohibited`
/// disables CUDA, and unknown values are not trusted.
const NVIDIA_COMPUTE_MODES: [&str; 3] = ["Default", "Exclusive_Process", "Exclusive_Thread"];

/// A PCI address as (domain, bus, device, function), compared numerically so
/// `nvidia-smi`'s eight-digit domains match sysfs's four-digit ones.
type PciAddress = (u32, u8, u8, u8);

/// Probes graphics, dedicated VRAM, and CUDA on the current Linux host.
///
/// Each `nvidia-smi` query runs separately within `PROBE_TIMEOUT`; a missing
/// binary or failed query is treated as absent hardware, never as an error.
#[cfg(target_os = "linux")]
pub(super) fn detect() -> Accelerators {
    // Probe failures mean unavailable hardware, not failed configuration.
    let output = probe::output(NVIDIA_SMI_PROGRAM, NVIDIA_COMPUTE_ARGS, PROBE_TIMEOUT).ok();
    // Keep memory probing separate: an unsupported memory query must not
    // disable an otherwise working CUDA device or graphics render node.
    let memory_output = probe::output(NVIDIA_SMI_PROGRAM, NVIDIA_MEMORY_ARGS, PROBE_TIMEOUT).ok();
    let mut accelerators = detect_graphics(
        Path::new(DRM_DEVICE_PATH),
        Path::new(DRM_SYSFS_PATH),
        memory_output.as_deref(),
    );
    accelerators.cuda = output.as_deref().is_some_and(nvidia_compute_available);
    accelerators
}

/// Detects graphics devices with a real access check against `directory`.
///
/// `sysfs` and the optional `nvidia-smi` memory report supply dedicated VRAM.
/// Separated from [`inspect`] so tests can substitute the access check.
fn detect_graphics(directory: &Path, sysfs: &Path, nvidia_output: Option<&[u8]>) -> Accelerators {
    inspect(directory, sysfs, nvidia_output, |path| {
        // Opening read/write checks the access needed by graphics drivers,
        // without reading from or writing to the device. A display-only
        // card, regular file, or inaccessible render node is not sufficient.
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.metadata())
            .is_ok_and(|metadata| metadata.file_type().is_char_device())
    })
}

/// Builds the graphics report from render nodes that pass `accessible`.
///
/// `gpu` is set when at least one node is accessible. `dedicated_memory` is the
/// smallest capacity among those nodes, taking each node's sysfs value first
/// and its `nvidia-smi` row (matched by PCI address) second; nodes without a
/// measurement are skipped. CUDA is left unset for the caller to decide.
fn inspect(
    directory: &Path,
    sysfs: &Path,
    nvidia_output: Option<&[u8]>,
    accessible: impl Fn(&Path) -> bool,
) -> Accelerators {
    let nodes = render_devices(directory, accessible);
    let nvidia_memory = nvidia_memory(nvidia_output.unwrap_or_default());
    let dedicated_memory = nodes
        .iter()
        .filter_map(|node| node_memory(sysfs, node, &nvidia_memory))
        // Smallest capacity, never the sum: the budget must fit whichever
        // device the engine ends up using.
        .min();

    Accelerators {
        gpu: !nodes.is_empty(),
        dedicated_memory,
        ..Accelerators::default()
    }
}

/// Reads one render node's dedicated VRAM: from sysfs first, then from the
/// `nvidia-smi` report through the node's PCI address.
///
/// sysfs gives exact bytes but NVIDIA's driver does not expose it, so the
/// fallback resolves the node's `device` link to its PCI address and looks
/// that up in the report. Only accessible nodes are ever looked up.
fn node_memory(
    sysfs: &Path,
    node: &Path,
    nvidia_memory: &BTreeMap<PciAddress, Mib>,
) -> Option<Mib> {
    let device = sysfs.join(node.file_name()?).join("device");
    sysfs_memory(&device).or_else(|| {
        let pci_device = device.canonicalize().ok()?;
        let address = pci_address(pci_device.file_name()?.to_str()?)?;
        nvidia_memory.get(&address).copied()
    })
}

/// Lists the `renderD<number>` nodes in `directory` that pass `accessible`.
///
/// Primary nodes (`card*`) need display privileges and are not used for
/// headless acceleration, so only render nodes count. A missing or unreadable
/// directory yields no devices.
fn render_devices(directory: &Path, accessible: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            // A bare `renderD` or a non-numeric suffix is not a render node.
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("renderD"))
                .is_some_and(|number| {
                    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                })
                && accessible(&entry.path())
        })
        .map(|entry| entry.path())
        .collect()
}

/// Reads a node's dedicated VRAM from the sysfs `mem_info_vram_total` file.
///
/// Exposed by amdgpu and some other drivers; NVIDIA's proprietary driver does
/// not provide it. Returns `None` when the file is missing or the value is
/// zero, malformed, or outside the range of `Mib`.
fn sysfs_memory(device: &Path) -> Option<Mib> {
    // amdgpu reports bytes here; missing, zero, or invalid values mean unknown.
    // Round down to whole MiB and reject out-of-range measurements, never wrap.
    let bytes = fs::read_to_string(device.join("mem_info_vram_total"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let memory = Mib::try_from(ByteSize::from_bytes(bytes)).ok()?;
    (memory > Mib::new(0)).then_some(memory)
}

/// Reports whether any device row of the compute query enables CUDA.
///
/// Non-UTF-8 output, driver error messages, and empty output all mean no CUDA.
fn nvidia_compute_available(output: &[u8]) -> bool {
    let Ok(output) = std::str::from_utf8(output) else {
        return false;
    };

    // Each headerless CSV row describes one GPU. Ignore unusable rows without
    // hiding another device that reports an enabled compute mode.
    output.lines().any(nvidia_device_compute_available)
}

/// Parses one `compute_cap, compute_mode` row of the compute query.
fn nvidia_device_compute_available(line: &str) -> bool {
    // The query returns compute capability as major.minor, then compute mode.
    // Missing fields and values such as [N/A] are not evidence of CUDA support.
    let Some((capability, mode)) = line.split_once(',') else {
        return false;
    };
    let Some((major, minor)) = capability.trim().split_once('.') else {
        return false;
    };

    // Require a valid nonzero major capability and an explicitly allowed mode.
    // Prohibited, unknown, and malformed modes (including extra CSV columns)
    // must not enable CUDA. No toolkit installation or workload is required.
    major.parse::<u32>().is_ok_and(|major| major > 0)
        && minor.parse::<u32>().is_ok()
        && NVIDIA_COMPUTE_MODES.contains(&mode.trim())
}

/// Parses `pci.bus_id, memory.total` rows into per-device capacities in MiB.
///
/// Rows with an unparsable address, a non-numeric or zero capacity, or extra
/// columns are skipped. A repeated address keeps its smaller capacity.
fn nvidia_memory(output: &[u8]) -> BTreeMap<PciAddress, Mib> {
    let Ok(output) = std::str::from_utf8(output) else {
        return BTreeMap::new();
    };
    let mut devices = BTreeMap::new();
    for (address, memory) in output.lines().filter_map(nvidia_memory_row) {
        // nvidia-smi lists each address once; if it ever does not, keep the
        // conservative figure rather than the last one seen.
        devices
            .entry(address)
            .and_modify(|existing: &mut Mib| *existing = (*existing).min(memory))
            .or_insert(memory);
    }
    devices
}

/// Parses one `pci.bus_id, memory.total` row into its address and capacity.
///
/// NVIDIA reports whole MiB already. The capacity is parsed within Mib's
/// range so oversized measurements cannot be truncated into a smaller budget;
/// extra columns make it unparsable. An unparsable address, a non-numeric or
/// zero capacity, and extra columns all yield `None`.
fn nvidia_memory_row(line: &str) -> Option<(PciAddress, Mib)> {
    let (address, memory) = line.split_once(',')?;
    let address = pci_address(address.trim())?;
    let memory = memory.trim().parse::<u32>().ok()?;
    (memory > 0).then_some((address, Mib::new(memory)))
}

/// Parses a hexadecimal `domain:bus:device.function` address, as printed by
/// both `nvidia-smi` and sysfs.
///
/// The domain may have any width. Device numbers above 31, function numbers
/// above 7, and trailing components are rejected.
fn pci_address(value: &str) -> Option<PciAddress> {
    // NVIDIA pads the domain to eight hex digits; Linux sysfs commonly uses
    // four. Normalize numerically, while retaining domains to avoid collisions.
    let mut parts = value.split(':');
    let domain = u32::from_str_radix(parts.next()?, 16).ok()?;
    let bus = u8::from_str_radix(parts.next()?, 16).ok()?;
    let (device, function) = parts.next()?.split_once('.')?;
    let device = u8::from_str_radix(device, 16).ok()?;
    let function = u8::from_str_radix(function, 16).ok()?;
    // PCI encodes a 5-bit device number and a 3-bit function number.
    if parts.next().is_some() || device > 31 || function > 7 {
        return None;
    }
    Some((domain, bus, device, function))
}

#[cfg(test)]
mod tests {
    use super::{
        ByteSize, Mib, detect_graphics, inspect, nvidia_compute_available, nvidia_memory,
        pci_address, render_devices, sysfs_memory,
    };
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    fn add_device(root: &Path, node: &str, pci: &str, memory_bytes: Option<&str>) {
        let dri = root.join("dri");
        fs::create_dir_all(&dri).unwrap();
        fs::write(dri.join(node), "").unwrap();
        let entry = root.join("sysfs").join(node);
        fs::create_dir_all(&entry).unwrap();
        let device = root.join("pci").join(pci);
        fs::create_dir_all(&device).unwrap();
        symlink(&device, entry.join("device")).unwrap();
        if let Some(bytes) = memory_bytes {
            fs::write(device.join("mem_info_vram_total"), bytes).unwrap();
        }
    }

    #[test]
    fn accepts_only_accessible_render_nodes() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["card0", "renderD", "renderDnot-a-device", "renderD128"] {
            fs::write(directory.path().join(name), "").unwrap();
        }
        assert_eq!(
            render_devices(directory.path(), |path| path.file_name().unwrap()
                == "renderD128"),
            vec![directory.path().join("renderD128")],
        );
        assert_eq!(
            render_devices(directory.path(), |_| false),
            Vec::<PathBuf>::new()
        );
        fs::remove_file(directory.path().join("renderD128")).unwrap();
        assert_eq!(
            render_devices(directory.path(), |_| true),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn missing_paths_and_regular_files_do_not_count_as_gpus() {
        let directory = tempfile::tempdir().unwrap();
        let sysfs = directory.path().join("sysfs");
        let missing = detect_graphics(&directory.path().join("missing"), &sysfs, None);
        assert!(!missing.gpu);
        assert_eq!(missing.dedicated_memory, None);
        fs::write(directory.path().join("renderD128"), "").unwrap();
        assert!(!detect_graphics(directory.path(), &sysfs, None).gpu);
    }

    #[test]
    fn reads_sysfs_bytes_and_uses_smallest_accessible_capacity_without_summing() {
        let root = tempfile::tempdir().unwrap();
        add_device(
            root.path(),
            "renderD128",
            "0000:01:00.0",
            Some("8589934592\n"),
        );
        add_device(
            root.path(),
            "renderD129",
            "0000:02:00.0",
            Some("4294967296\n"),
        );
        add_device(
            root.path(),
            "renderD130",
            "0000:03:00.0",
            Some("1073741824\n"),
        );

        let detected = inspect(
            &root.path().join("dri"),
            &root.path().join("sysfs"),
            None,
            |path| path.file_name().unwrap() != "renderD130",
        );
        assert!(detected.gpu);
        assert_eq!(detected.dedicated_memory, Some(Mib::new(4096)));
    }

    #[test]
    fn matches_nvidia_memory_to_render_device_pci_address_including_domain() {
        let root = tempfile::tempdir().unwrap();
        add_device(root.path(), "renderD128", "0000:0a:00.0", None);
        add_device(root.path(), "renderD129", "0000:0b:00.0", None);
        let report = b"00000000:0A:00.0, 8192\n00000000:0B:00.0, 4096\n\
                       00000001:0A:00.0, 512\n00000000:FF:00.0, 1024\n";

        let detected = inspect(
            &root.path().join("dri"),
            &root.path().join("sysfs"),
            Some(report),
            |_| true,
        );
        assert!(detected.gpu);
        assert_eq!(detected.dedicated_memory, Some(Mib::new(4096)));
    }

    #[test]
    fn ignores_nvidia_memory_when_there_is_no_accessible_graphics_device() {
        let root = tempfile::tempdir().unwrap();
        add_device(root.path(), "renderD128", "0000:01:00.0", None);
        let detected = inspect(
            &root.path().join("dri"),
            &root.path().join("sysfs"),
            Some(b"00000000:01:00.0, 8192\n"),
            |_| false,
        );
        assert!(!detected.gpu);
        assert_eq!(detected.dedicated_memory, None);
    }

    #[test]
    fn missing_or_invalid_memory_preserves_graphics_detection() {
        for memory in [
            None,
            Some("0"),
            Some("garbage"),
            Some("-1"),
            Some("1023"),
            Some("18446744073709551615"),
        ] {
            let root = tempfile::tempdir().unwrap();
            add_device(root.path(), "renderD128", "0000:01:00.0", memory);
            let detected = inspect(
                &root.path().join("dri"),
                &root.path().join("sysfs"),
                Some(b"00000000:01:00.0, [N/A]\n"),
                |_| true,
            );
            assert!(detected.gpu);
            assert_eq!(detected.dedicated_memory, None);
        }
    }

    #[test]
    fn sysfs_memory_rounds_down_and_rejects_values_outside_mib_range() {
        let device = tempfile::tempdir().unwrap();
        let one_mib = ByteSize::checked_from_mib(1).unwrap().as_bytes();
        let max = ByteSize::checked_from_mib(u64::from(u32::MAX))
            .unwrap()
            .as_bytes();
        for (bytes, expected) in [
            (0, None),
            (one_mib - 1, None),
            (one_mib, Some(1)),
            (one_mib + 1, Some(1)),
            (max, Some(u32::MAX)),
            (max + one_mib - 1, Some(u32::MAX)),
            (max + one_mib, None),
            (u64::MAX, None),
        ] {
            fs::write(device.path().join("mem_info_vram_total"), bytes.to_string()).unwrap();
            assert_eq!(sysfs_memory(device.path()), expected.map(Mib::new));
        }
    }

    #[test]
    fn oversized_sysfs_memory_falls_back_to_nvidia_memory() {
        let root = tempfile::tempdir().unwrap();
        add_device(
            root.path(),
            "renderD128",
            "0000:01:00.0",
            Some("18446744073709551615"),
        );
        let detected = inspect(
            &root.path().join("dri"),
            &root.path().join("sysfs"),
            Some(b"00000000:01:00.0, 8192\n"),
            |_| true,
        );
        assert!(detected.gpu);
        assert_eq!(detected.dedicated_memory, Some(Mib::new(8192)));
    }

    #[test]
    fn detects_nvidia_compute_without_requiring_a_toolkit() {
        for report in [
            b"8.6, Default\n".as_slice(),
            b"9.0, Exclusive_Process\r\n",
            b"3.5, Exclusive_Thread\n",
            b"[N/A], Prohibited\n8.0, Default\n",
            b" 12.0 , Default \n",
        ] {
            assert!(nvidia_compute_available(report));
        }
    }

    #[test]
    fn no_device_bad_output_or_prohibited_compute_does_not_enable_cuda() {
        for report in [
            b"".as_slice(),
            b"No devices were found",
            b"NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver.",
            b"8.6, Prohibited\n",
            b"[N/A], Default\n",
            b"0.0, Default\n",
            b"8, Default\n",
            b".6, Default\n",
            b"8., Default\n",
            b"-8.6, Default\n",
            b"bad.6, Default\n",
            b"8.bad, Default\n",
            b"8.6.1, Default\n",
            b"4294967296.0, Default\n",
            b"8.4294967296, Default\n",
            b"8.6,\n",
            b"8.6, Unknown\n",
            b"8.6, Default, unexpected\n",
            b"nvcc: release 12.4",
            b"\xff",
        ] {
            assert!(!nvidia_compute_available(report));
        }
    }

    #[test]
    fn nvidia_memory_accepts_mib_maximum_and_rejects_larger_values() {
        let memory = nvidia_memory(
            b"0000:01:00.0, 4294967295\n0000:02:00.0, 4294967296\n\
              0000:03:00.0, 18446744073709551615\n",
        );
        assert_eq!(memory.len(), 1);
        assert_eq!(
            memory[&pci_address("0000:01:00.0").unwrap()],
            Mib::new(u32::MAX)
        );
    }

    #[test]
    fn parses_nvidia_mib_and_ignores_malformed_values() {
        let memory = nvidia_memory(
            b"00000000:01:00.0, 8192\r\n00000000:01:00.0, 4096\n\
              0000:02:00.0, 0\n0000:03:00.0, -1\n0000:04:00.0, [N/A]\n\
              0000:05:00.0, 4096 MiB\n0000:06:00.0, 18446744073709551616\n\
              bad address, 8192\n0000:07:00.0, 4096, extra\n",
        );
        assert_eq!(memory.len(), 1);
        assert_eq!(
            memory[&pci_address("0000:01:00.0").unwrap()],
            Mib::new(4096)
        );
        assert!(nvidia_memory(b"\xff").is_empty());
        assert!(nvidia_memory(b"No devices were found").is_empty());
    }

    #[test]
    fn rejects_invalid_pci_addresses() {
        for address in [
            "",
            "01:00.0",
            "0000:gg:00.0",
            "0000:01:20.0",
            "0000:01:00.8",
            "0000:01:00.0:extra",
        ] {
            assert!(pci_address(address).is_none(), "accepted {address}");
        }
    }
}
