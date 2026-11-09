//! Host RAM detection and guest-memory recommendations.

use std::num::NonZero;
use sysinfo::System;

use super::{ByteSize, Mib};
use crate::{Error, Result};

/// A nonzero total host RAM capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HostMemory(NonZero<u64>);

impl HostMemory {
    /// Minimum default guest size, even when the host's quarter-share is smaller.
    const MIN_GUEST_MEMORY: ByteSize =
        ByteSize::checked_from_mib(512).expect("512 MiB fits in a u64 byte count");

    /// Maximum default guest size, regardless of host capacity.
    const MAX_GUEST_MEMORY: ByteSize =
        ByteSize::checked_from_gib(8).expect("8 GiB fits in a u64 byte count");

    /// Use one quarter of total host RAM as the initial guest budget.
    const GUEST_MEMORY_DIVISOR: u64 = 4;

    /// Detects total host RAM, failing if sysinfo does not support the host or
    /// the host reports no memory.
    pub(super) fn detect() -> Result<Self> {
        // sysinfo fails here only on platforms it does not support; its error
        // says no more than that, so it maps to the same variant as zero memory.
        let mut system = System::new().map_err(|_| Error::HostMemory)?;
        system.refresh_memory();
        Self::from_size(ByteSize::from_bytes(system.total_memory()))
    }

    /// Wraps a reported capacity, rejecting zero as unavailable host memory.
    pub(super) fn from_size(size: ByteSize) -> Result<Self> {
        NonZero::new(size.as_bytes())
            .map(Self)
            .ok_or(Error::HostMemory)
    }

    /// Returns total host capacity, without applying guest sizing policy.
    pub(super) fn total(self) -> ByteSize {
        ByteSize::from_bytes(self.0.get())
    }

    /// Recommends a default guest RAM size from total host capacity.
    ///
    /// # Policy
    ///
    /// 1. Start with one quarter of total host RAM, leaving capacity for the host
    ///    and other workloads.
    /// 2. Clamp that budget to a minimum of 512 MiB and a maximum of 8 GiB
    ///    (8192 MiB). These are default-sizing choices, not limits on explicitly
    ///    configured guest memory.
    /// 3. Round down to whole MiB for the configuration value.
    ///
    /// For example, 1 GiB of host RAM suggests 512 MiB, 16 GiB suggests 4096 MiB,
    /// and 64 GiB suggests 8192 MiB.
    ///
    /// # Limitations
    ///
    /// This uses total RAM, not currently free or available RAM. It neither
    /// allocates memory nor accounts for other running guests, so the result
    /// does not guarantee that sufficient memory is available.
    ///
    /// The minimum overrides the quarter-share on hosts below 2 GiB and can
    /// even exceed total host RAM on hosts below 512 MiB.
    ///
    /// # Errors
    ///
    /// Returns [`Error::GuestMemorySize`] if the recommendation cannot be
    /// represented as a `u32` MiB count. The current 8 GiB cap prevents this,
    /// but the conversion remains checked if the sizing policy changes.
    pub(super) fn suggested(self) -> Result<Mib> {
        let memory = ByteSize::from_bytes(self.total().as_bytes() / Self::GUEST_MEMORY_DIVISOR)
            .clamp(Self::MIN_GUEST_MEMORY, Self::MAX_GUEST_MEMORY);
        Mib::try_from(memory).map_err(Error::GuestMemorySize)
    }
}

#[cfg(test)]
mod tests {
    use super::{HostMemory, Mib};
    use crate::{ByteSize, Error};

    #[test]
    fn zero_reported_memory_is_unavailable() {
        assert!(matches!(
            HostMemory::from_size(ByteSize::from_bytes(0)),
            Err(Error::HostMemory)
        ));
    }

    #[test]
    fn reported_memory_preserves_positive_byte_counts() {
        for bytes in [1, 1024, 17_179_869_184, u64::MAX] {
            let memory = HostMemory::from_size(ByteSize::from_bytes(bytes)).unwrap();
            assert_eq!(memory.total().as_bytes(), bytes);
        }
    }

    #[test]
    fn reserves_capacity_and_caps_large_hosts() {
        for (host_mib, expected_mib) in [
            (256, 512),
            (1024, 512),
            (2048, 512),
            (4096, 1024),
            (16384, 4096),
            (32768, 8192),
            (262_144, 8192),
        ] {
            let memory =
                HostMemory::from_size(ByteSize::checked_from_mib(host_mib).unwrap()).unwrap();
            assert_eq!(memory.suggested().unwrap(), Mib::new(expected_mib));
        }
    }

    #[test]
    fn rounds_down_at_byte_boundaries_and_keeps_limits() {
        let smallest = HostMemory::from_size(ByteSize::from_bytes(1)).unwrap();
        assert_eq!(smallest.suggested().unwrap(), Mib::new(512));
        for (host_mib, before, at) in [
            (2048, 512, 512),
            (2052, 512, 513),
            (32768, 8191, 8192),
            (32772, 8192, 8192),
        ] {
            let bytes = ByteSize::checked_from_mib(host_mib).unwrap().as_bytes();
            for (bytes, expected) in [(bytes - 1, before), (bytes, at), (bytes + 1, at)] {
                let memory = HostMemory::from_size(ByteSize::from_bytes(bytes)).unwrap();
                assert_eq!(memory.suggested().unwrap(), Mib::new(expected));
            }
        }
    }

    #[test]
    fn does_not_overflow_on_extreme_capacity() {
        let memory = HostMemory::from_size(ByteSize::from_bytes(u64::MAX)).unwrap();
        assert_eq!(memory.suggested().unwrap(), Mib::new(8192));
    }
}
