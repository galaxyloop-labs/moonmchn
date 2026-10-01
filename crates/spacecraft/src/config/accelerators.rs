//! Best-effort host accelerator probes and GPU shared-memory sizing.
//!
//! Graphics and CUDA are independent capabilities: smolvm uses virtio-gpu and
//! Vulkan for graphics and NVIDIA Driver-API remoting for CUDA. Each platform
//! module runs its own short-lived probe commands; neither installs software,
//! starts a workload, or fails setup when a probe is unavailable.
//!
//! Detection covers Apple Silicon macOS (Metal) and Linux (DRM render nodes
//! plus `nvidia-smi`). Every other platform reports no accelerators.

use super::{ByteSize, Mib};

/// Shared time limit for each platform's accelerator probe command.
///
/// Long enough for `nvidia-smi` to initialize the driver on a cold start, short
/// enough that a hung probe cannot make setup appear frozen.
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), target_os = "linux"))]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Host accelerator capabilities as reported by the platform probes.
///
/// The default value reports nothing, which is also the result on unsupported
/// platforms and whenever a probe fails. Values describe host hardware, not a
/// verified guest graphics or CUDA stack.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Accelerators {
    /// A graphics device the engine can use for virtio-gpu acceleration: Metal
    /// support on Apple Silicon, or an accessible DRM render node on Linux.
    pub gpu: bool,
    /// An NVIDIA device with a valid compute capability and an allowed compute
    /// mode, detected on Linux only. Independent of `gpu`: a compute-only card
    /// enables CUDA without graphics, and Apple Metal never counts as CUDA.
    pub cuda: bool,
    /// Smallest reported dedicated VRAM capacity among accessible graphics
    /// devices. None means unified/shared memory or an unavailable measurement
    /// (including values outside the range of `Mib`).
    pub dedicated_memory: Option<Mib>,
}

impl Accelerators {
    /// Use at most one eighth of host RAM for the graphics shared-memory region.
    const HOST_MEMORY_DIVISOR: u64 = 8;

    /// Use at most half of reported dedicated VRAM, leaving room for the host.
    const DEDICATED_MEMORY_DIVISOR: u64 = 2;

    /// Maximum default shared-memory region, regardless of host or GPU capacity.
    const MAX_VRAM: Mib = Mib::new(4096);

    /// Round larger budgets down to this block size; this is not a minimum.
    const VRAM_BLOCK_SIZE: Mib = Mib::new(256);

    /// Recommends a default size for the engine's graphics shared-memory region.
    ///
    /// # Policy
    ///
    /// 1. Without graphics, including CUDA-only hosts, return `None`: the
    ///    region is unused.
    /// 2. Start with one eighth of host RAM, capped at 4096 MiB.
    /// 3. When dedicated VRAM is known and positive, also cap at half of it.
    /// 4. Round budgets of at least 256 MiB down to a multiple of 256 MiB;
    ///    smaller positive budgets are kept as they are.
    /// 5. Return `None` instead of a zero-sized region.
    ///
    /// For example, 16 GiB of unified memory suggests 2048 MiB, a 64 GiB host
    /// with a 6144 MiB card suggests 3072 MiB, and a 4 GiB host suggests
    /// 512 MiB however large its card is.
    ///
    /// # Limitations
    ///
    /// This sizes a shared-memory region, not a physical VRAM allocation, and
    /// it uses total rather than free host RAM. With several GPUs the smallest
    /// reported capacity bounds the budget, so the region fits any device the
    /// engine may pick. These are default-sizing choices, not limits on an
    /// explicitly configured `gpu_vram`.
    pub(super) fn suggested_vram(self, host_memory: ByteSize) -> Option<Mib> {
        // CUDA-only devices do not use the graphics shared-memory region.
        if !self.gpu {
            return None;
        }

        // Reserve most RAM for the host and cap the recommendation even on
        // large machines. Keep MiB arithmetic wide until after applying limits.
        let mut budget_mib = (host_memory / Self::HOST_MEMORY_DIVISOR)
            .as_mib()
            .min(u64::from(u32::from(Self::MAX_VRAM)));

        // Unified/shared memory and missing or zero measurements have no
        // dedicated VRAM limit, so they use only the host-memory budget.
        if let Some(dedicated) = self.dedicated_memory.filter(|&memory| memory > Mib::new(0)) {
            let dedicated_mib = u64::from(u32::from(dedicated));
            budget_mib = budget_mib.min(dedicated_mib / Self::DEDICATED_MEMORY_DIVISOR);
        }

        // Round down, never up past a capacity limit. Preserve positive budgets
        // below one block rather than imposing an unsafe minimum allocation.
        let block_mib = u64::from(u32::from(Self::VRAM_BLOCK_SIZE));
        if budget_mib >= block_mib {
            budget_mib = budget_mib / block_mib * block_mib;
        }

        // Omit budgets smaller than one whole MiB rather than serializing zero.
        // Keep the conversion checked if the sizing policy changes in future.
        u32::try_from(budget_mib)
            .ok()
            .filter(|&mib| mib > 0)
            .map(Mib::new)
    }
}

/// Probes the current host for accelerators.
///
/// Dispatches to the platform module compiled for this target. Probes are
/// best-effort: a missing tool, failed command, or inaccessible device leaves
/// the corresponding capability disabled rather than returning an error.
pub(super) fn detect() -> Accelerators {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        macos::detect()
    }

    #[cfg(target_os = "linux")]
    {
        linux::detect()
    }

    #[cfg(not(any(all(target_os = "macos", target_arch = "aarch64"), target_os = "linux")))]
    {
        // smolvm does not provide graphics acceleration on Windows, and local
        // Intel macOS machines are unsupported. Never infer support from OS alone.
        Accelerators::default()
    }
}

// Include platform helpers in tests on other hosts; command invocation remains
// gated to the supported target so fixture tests never need that platform's GPU.
// `linux` reads Unix device files and `probe` spawns `/bin/sh` in its tests, so
// both stay Unix-only; `macos` parses JSON fixtures and builds anywhere.
#[cfg(any(target_os = "linux", all(test, unix)))]
mod linux;

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), test))]
mod macos;

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    target_os = "linux",
    all(test, unix)
))]
mod probe;

#[cfg(test)]
mod tests {
    use super::{Accelerators, Mib};
    use crate::ByteSize;

    #[test]
    fn recommends_conservative_shared_memory_for_unified_and_dedicated_gpus() {
        // Host RAM MiB, reported dedicated VRAM MiB, expected region MiB.
        for (host_mib, dedicated_mib, expected) in [
            (8192, None, 1024),
            (16384, None, 2048),
            (49152, None, 4096),
            (65536, Some(2048), 1024),
            (65536, Some(6144), 3072),
            (4096, Some(32768), 512),
            (11000, None, 1280),
            (65536, Some(3000), 1280),
            (1024, Some(128), 64),
            (8192, Some(0), 1024),
        ] {
            let accelerators = Accelerators {
                gpu: true,
                dedicated_memory: dedicated_mib.map(Mib::new),
                ..Accelerators::default()
            };
            assert_eq!(
                accelerators.suggested_vram(ByteSize::checked_from_mib(host_mib).unwrap()),
                Some(Mib::new(expected))
            );
        }
    }

    #[test]
    fn memory_recommendation_rounds_down_at_byte_and_block_boundaries() {
        let accelerators = Accelerators {
            gpu: true,
            ..Accelerators::default()
        };
        for (host_mib, before, at) in [
            (8, None, Some(1)),
            (2048, Some(255), Some(256)),
            (4096, Some(256), Some(512)),
            (32768, Some(3840), Some(4096)),
        ] {
            let bytes = ByteSize::checked_from_mib(host_mib).unwrap().as_bytes();
            for (bytes, expected) in [(bytes - 1, before), (bytes, at), (bytes + 1, at)] {
                assert_eq!(
                    accelerators.suggested_vram(ByteSize::from_bytes(bytes)),
                    expected.map(Mib::new)
                );
            }
        }
    }

    #[test]
    fn dedicated_memory_limits_round_down_without_imposing_a_minimum() {
        for (dedicated_mib, expected) in [
            (1, None),
            (2, Some(1)),
            (3, Some(1)),
            (511, Some(255)),
            (512, Some(256)),
            (513, Some(256)),
            (1023, Some(256)),
            (1024, Some(512)),
        ] {
            let accelerators = Accelerators {
                gpu: true,
                dedicated_memory: Some(Mib::new(dedicated_mib)),
                ..Accelerators::default()
            };
            assert_eq!(
                accelerators.suggested_vram(ByteSize::checked_from_gib(64).unwrap()),
                expected.map(Mib::new),
            );
        }
    }

    #[test]
    fn memory_recommendation_is_omitted_without_graphics_including_cuda_only() {
        let cuda_only = Accelerators {
            gpu: false,
            cuda: true,
            dedicated_memory: Some(Mib::new(16384)),
        };
        let max = ByteSize::from_bytes(u64::MAX);
        assert_eq!(cuda_only.suggested_vram(max), None);
        assert_eq!(Accelerators::default().suggested_vram(max), None);
    }

    #[test]
    fn memory_recommendation_never_overflows_or_serializes_zero() {
        let accelerators = Accelerators {
            gpu: true,
            dedicated_memory: Some(Mib::new(u32::MAX)),
            ..Accelerators::default()
        };
        assert_eq!(
            accelerators.suggested_vram(ByteSize::from_bytes(u64::MAX)),
            Some(Mib::new(4096))
        );
        assert_eq!(accelerators.suggested_vram(ByteSize::from_bytes(0)), None);
        assert_eq!(
            accelerators.suggested_vram(ByteSize::from_bytes(1024)),
            None
        );
    }
}
