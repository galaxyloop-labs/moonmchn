//! Apple Silicon graphics detection from `system_profiler`'s Metal report.
//!
//! Graphics is available when any display adapter in the JSON report of
//! `system_profiler SPDisplaysDataType` states Metal support. CUDA is never
//! reported, since Apple Metal is not NVIDIA CUDA, and unified memory is never
//! reported as dedicated VRAM.

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use super::{Accelerators, PROBE_TIMEOUT, probe};

/// Absolute path, so detection does not depend on the caller's `PATH`.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SYSTEM_PROFILER_PATH: &str = "/usr/sbin/system_profiler";
/// JSON keeps parsing independent of localized text; `mini` detail still
/// includes the Metal support keys while keeping the report small.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SYSTEM_PROFILER_ARGS: &[&str] = &[DISPLAYS_DATA_TYPE, "-json", "-detailLevel", "mini"];

/// Top-level key holding one entry per display adapter.
const DISPLAYS_DATA_TYPE: &str = "SPDisplaysDataType";
/// Keys that have carried the Metal support value across macOS releases.
const METAL_SUPPORT_KEYS: [&str; 2] = ["spdisplays_mtlgpufamilysupport", "spdisplays_metal"];
/// Legacy value stating that Metal is supported without naming a version.
const METAL_SUPPORTED: &str = "spdisplays_supported";
/// Prefix of versioned support values such as `spdisplays_metal3`.
const METAL_VERSION_PREFIX: &str = "spdisplays_metal";

/// Probes graphics support on the current Apple Silicon host.
///
/// Runs `system_profiler` within `PROBE_TIMEOUT`; a failed command or a
/// malformed report leaves graphics disabled rather than returning an error.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(super) fn detect() -> Accelerators {
    // Probe failures mean unavailable hardware, not failed configuration.
    let output = probe::output(SYSTEM_PROFILER_PATH, SYSTEM_PROFILER_ARGS, PROBE_TIMEOUT).ok();
    Accelerators {
        gpu: output.as_deref().is_some_and(metal_available),
        // Apple Metal is not NVIDIA CUDA. The engine's CPU CUDA emulation
        // is test-only and must not be advertised as hardware support.
        cuda: false,
        // Apple Silicon shares host RAM; do not report it as dedicated VRAM.
        dedicated_memory: None,
    }
}

/// Reports whether the JSON display report shows Metal support on any adapter.
///
/// Anything other than a JSON object with an array under `SPDisplaysDataType`
/// is treated as no support, and entries that are not objects are skipped.
fn metal_available(output: &[u8]) -> bool {
    let Ok(report) = serde_json::from_slice::<serde_json::Value>(output) else {
        return false;
    };
    let Some(devices) = report
        .get(DISPLAYS_DATA_TYPE)
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };

    // system_profiler uses different support keys across macOS releases.
    // Require an explicit support value on at least one device; a model name
    // alone is not evidence of Metal support.
    devices.iter().any(|device| {
        METAL_SUPPORT_KEYS
            .iter()
            .filter_map(|key| device.get(key).and_then(serde_json::Value::as_str))
            .any(reports_metal_support)
    })
}

/// Interprets one support value taken from a display adapter entry.
fn reports_metal_support(value: &str) -> bool {
    // Older reports use a generic supported marker. Newer reports name a
    // Metal version (e.g. spdisplays_metal3); reject bare prefixes and suffixes
    // other than ASCII digits rather than guessing from an unknown value.
    value == METAL_SUPPORTED
        || value
            .strip_prefix(METAL_VERSION_PREFIX)
            .is_some_and(|version| {
                !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
            })
}

#[cfg(test)]
mod tests {
    use super::metal_available;

    #[test]
    fn detects_apple_metal_and_legacy_supported_reports() {
        for report in [
            serde_json::json!({
                "SPDisplaysDataType": [{
                    "sppci_model": "Apple M4 Pro",
                    "spdisplays_mtlgpufamilysupport": "spdisplays_metal3"
                }]
            }),
            serde_json::json!({
                "SPDisplaysDataType": [
                    { "sppci_model": "Unsupported adapter" },
                    { "spdisplays_metal": "spdisplays_supported" }
                ]
            }),
            serde_json::json!({
                "SPDisplaysDataType": [{
                    "spdisplays_mtlgpufamilysupport": "spdisplays_supported"
                }]
            }),
            serde_json::json!({
                "SPDisplaysDataType": [{
                    "spdisplays_mtlgpufamilysupport": "spdisplays_unsupported",
                    "spdisplays_metal": "spdisplays_metal12"
                }]
            }),
        ] {
            assert!(metal_available(&serde_json::to_vec(&report).unwrap()));
        }
    }

    #[test]
    fn malformed_or_unsupported_display_reports_do_not_enable_graphics() {
        for report in [
            b"not JSON".as_slice(),
            b"{}",
            b"[]",
            br#"{"SPDisplaysDataType": []}"#,
            br#"{"SPDisplaysDataType": {}}"#,
            br#"{"SPDisplaysDataType": [null, 1, "spdisplays_supported"]}"#,
            br#"{"SPDisplaysDataType": [{"sppci_model": "Apple M4 Pro"}]}"#,
            br#"{"SPDisplaysDataType": [{"spdisplays_metal": "spdisplays_unsupported"}]}"#,
            br#"{"SPDisplaysDataType": [{"spdisplays_metal": true}]}"#,
            br#"{"SPDisplaysDataType": [{"spdisplays_metal": ""}]}"#,
            br#"{"SPDisplaysDataType": [{"spdisplays_metal": "spdisplays_metal3.1"}]}"#,
            br#"{"SPDisplaysDataType": [{"spdisplays_metal": "spdisplays_metal3extra"}]}"#,
            br#"{"SPDisplaysDataType": [{"spdisplays_mtlgpufamilysupport": "spdisplays_metal"}]}"#,
        ] {
            assert!(!metal_available(report));
        }
    }
}
