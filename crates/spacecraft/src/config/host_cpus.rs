//! Available host CPU detection and default guest CPU recommendations.

use std::num::NonZero;
use std::thread;

use crate::{Error, Result};

/// A nonzero count of logical CPUs available to the current process.
///
/// This is the parallelism reported by [`thread::available_parallelism`], not
/// necessarily the machine's physical core count or total logical CPU count.
/// The supported range is 1–255; larger reported counts are rejected rather
/// than truncated or saturated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HostCPUs(NonZero<u8>);

impl HostCPUs {
    /// Minimum default guest CPU count, including on single-CPU hosts.
    const MIN_GUEST_CPUS: u8 = 1;

    /// Maximum default guest CPU count, regardless of available host capacity.
    const MAX_GUEST_CPUS: u8 = 8;

    /// Use half of the reported available logical CPUs as the initial budget.
    const GUEST_CPUS_DIVISOR: u8 = 2;

    /// Detects the logical CPU parallelism available to the current process.
    ///
    /// Uses [`thread::available_parallelism`], whose estimate may reflect
    /// scheduling restrictions such as CPU affinity or container limits. It
    /// does not measure current CPU load or promise exclusive access to CPUs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HostCpu`] if the available parallelism cannot be
    /// determined, or [`Error::HostCpuCount`] if the reported count exceeds 255.
    pub(super) fn detect() -> Result<Self> {
        let detected = thread::available_parallelism().map_err(Error::HostCpu)?;
        Self::from_count(detected)
    }

    /// Constructs host capacity from an already reported nonzero CPU count.
    ///
    /// Preserves the count exactly without probing the host. The argument's
    /// type ensures that zero cannot be supplied.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HostCpuCount`] if the count exceeds 255.
    pub(super) fn from_count(count: NonZero<usize>) -> Result<Self> {
        let count = NonZero::<u8>::try_from(count).map_err(Error::HostCpuCount)?;
        Ok(Self(count))
    }

    /// Recommends a default guest CPU count from available host parallelism.
    ///
    /// # Policy
    ///
    /// 1. Divide the available logical CPU count by two, rounding down, to leave
    ///    capacity for the host and other workloads.
    /// 2. Clamp the result to 1–8 guest CPUs. The minimum overrides the half-share
    ///    on single-CPU hosts, ensuring the guest is never assigned zero CPUs.
    ///
    /// For example, 1 or 3 available CPUs suggest 1 guest CPU, 8 suggest 4, and
    /// 16 or more suggest 8. These are default-sizing choices, not limits on
    /// explicitly configured guest CPU counts.
    ///
    /// # Limitations
    ///
    /// This recommendation does not reserve or pin CPUs, measure current load,
    /// or account for other guests. In particular, the single-CPU minimum
    /// leaves no separate CPU for the host. The calculation cannot fail for
    /// any supported host count.
    pub(super) fn suggested(self) -> u8 {
        (self.0.get() / Self::GUEST_CPUS_DIVISOR).clamp(Self::MIN_GUEST_CPUS, Self::MAX_GUEST_CPUS)
    }
}

#[cfg(test)]
mod tests {
    use super::HostCPUs;
    use crate::Error;
    use std::num::NonZero;

    #[test]
    fn preserves_every_supported_reported_count() {
        for count in 1..=usize::from(u8::MAX) {
            let cpus = HostCPUs::from_count(NonZero::new(count).unwrap()).unwrap();
            assert_eq!(usize::from(cpus.0.get()), count);
        }
    }

    #[test]
    fn rejects_counts_above_u8_range_without_saturating() {
        for count in [256, 257, usize::MAX] {
            assert!(matches!(
                HostCPUs::from_count(NonZero::new(count).unwrap()),
                Err(Error::HostCpuCount(_))
            ));
        }
    }

    #[test]
    fn halves_capacity_rounds_down_and_applies_default_limits() {
        for (count, expected) in [
            (1, 1),
            (2, 1),
            (3, 1),
            (4, 2),
            (7, 3),
            (8, 4),
            (15, 7),
            (16, 8),
            (17, 8),
            (64, 8),
            (255, 8),
        ] {
            let cpus = HostCPUs::from_count(NonZero::new(count).unwrap()).unwrap();
            assert_eq!(cpus.suggested(), expected, "host count: {count}");
        }
    }

    #[test]
    fn recommendations_stay_bounded_and_monotonic_across_supported_counts() {
        let mut previous = 0;
        for count in 1..=usize::from(u8::MAX) {
            let cpus = HostCPUs::from_count(NonZero::new(count).unwrap()).unwrap();
            let suggested = cpus.suggested();
            assert!((1..=8).contains(&suggested), "host count: {count}");
            assert!(usize::from(suggested) <= count);
            assert!(suggested >= previous);
            previous = suggested;
        }
    }
}
