use std::num::TryFromIntError;

/// A byte quantity, independent of whether it describes memory or disk space.
///
/// Stored as a `u64` byte count. Binary-unit conversions use MiB (2²⁰ bytes)
/// and GiB (2³⁰ bytes), not decimal MB and GB. Division and conversions to
/// larger units round down.
///
/// ```
/// use spacecraft::ByteSize;
///
/// let host_memory = ByteSize::checked_from_gib(16).unwrap();
/// let guest_memory = host_memory.checked_div(4).unwrap();
/// assert_eq!(guest_memory.as_mib(), 4096);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[must_use]
pub struct ByteSize(u64);

impl ByteSize {
    const BYTES_PER_MIB: u64 = 1024 * 1024;
    const BYTES_PER_GIB: u64 = 1024 * Self::BYTES_PER_MIB;

    /// Creates a size from an exact byte count.
    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Creates a size from MiB, returning `None` if the byte count overflows.
    #[must_use]
    pub const fn checked_from_mib(mib: u64) -> Option<Self> {
        match mib.checked_mul(Self::BYTES_PER_MIB) {
            Some(bytes) => Some(Self(bytes)),
            None => None,
        }
    }

    /// Creates a size from GiB, returning `None` if the byte count overflows.
    #[must_use]
    pub const fn checked_from_gib(gib: u64) -> Option<Self> {
        match gib.checked_mul(Self::BYTES_PER_GIB) {
            Some(bytes) => Some(Self(bytes)),
            None => None,
        }
    }

    /// Returns the exact byte count.
    #[must_use]
    pub const fn as_bytes(self) -> u64 {
        self.0
    }

    /// Returns whole MiB, rounding down.
    #[must_use]
    pub const fn as_mib(self) -> u64 {
        self.0 / Self::BYTES_PER_MIB
    }

    /// Returns whole GiB, rounding down.
    #[must_use]
    pub const fn as_gib(self) -> u64 {
        self.0 / Self::BYTES_PER_GIB
    }

    /// Divides by a scalar, rounding down to whole bytes.
    ///
    /// Returns `None` if `divisor` is zero.
    pub fn checked_div(self, divisor: u64) -> Option<Self> {
        self.0.checked_div(divisor).map(Self)
    }
}

/// A count of whole mebibytes (MiB), distinct from a byte count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Mib(u32);

impl Mib {
    /// Creates a quantity from a count already expressed in MiB.
    pub(super) const fn new(mib: u32) -> Self {
        Self(mib)
    }
}

impl TryFrom<ByteSize> for Mib {
    type Error = TryFromIntError;

    /// Converts to whole MiB, rounding down.
    ///
    /// # Errors
    ///
    /// Returns an error if the whole-MiB count exceeds `u32::MAX`.
    fn try_from(size: ByteSize) -> Result<Self, Self::Error> {
        u32::try_from(size.as_mib()).map(Self::new)
    }
}

impl From<Mib> for u32 {
    fn from(mib: Mib) -> Self {
        mib.0
    }
}

#[cfg(test)]
mod tests {
    use super::ByteSize;
    use super::Mib;

    #[test]
    fn binary_units_share_a_canonical_byte_count() {
        let gib = ByteSize::checked_from_gib(1).unwrap();
        assert_eq!(gib.as_bytes(), 1_073_741_824);
        assert_eq!(gib.as_mib(), 1024);
        assert_eq!(gib.as_gib(), 1);
        assert_eq!(ByteSize::checked_from_mib(1024), Some(gib));
        assert_eq!(ByteSize::checked_from_mib(1).unwrap().as_bytes(), 1_048_576);
    }

    #[test]
    fn checked_unit_constructors_work_in_constants() {
        const MIB: ByteSize = ByteSize::checked_from_mib(1).unwrap();
        const GIB: ByteSize = ByteSize::checked_from_gib(1).unwrap();
        const MIB_OVERFLOW: Option<ByteSize> = ByteSize::checked_from_mib(u64::MAX);
        const GIB_OVERFLOW: Option<ByteSize> = ByteSize::checked_from_gib(u64::MAX);

        assert_eq!(MIB.as_bytes(), 1_048_576);
        assert_eq!(GIB.as_bytes(), 1_073_741_824);
        assert_eq!(MIB_OVERFLOW, None);
        assert_eq!(GIB_OVERFLOW, None);
    }

    #[test]
    fn zero_is_valid_in_every_unit() {
        let zero = ByteSize::default();
        assert_eq!(ByteSize::from_bytes(0), zero);
        assert_eq!(ByteSize::checked_from_mib(0), Some(zero));
        assert_eq!(ByteSize::checked_from_gib(0), Some(zero));
        assert_eq!(zero.as_bytes(), 0);
        assert_eq!(zero.as_mib(), 0);
        assert_eq!(zero.as_gib(), 0);
    }

    #[test]
    fn conversions_preserve_bytes_and_round_down_larger_units() {
        for (bytes, mib, gib) in [
            (1, 0, 0),
            (1_048_575, 0, 0),
            (1_048_576, 1, 0),
            (1_048_577, 1, 0),
            (1_073_741_823, 1023, 0),
            (1_073_741_824, 1024, 1),
            (1_073_741_825, 1024, 1),
            (u64::MAX, 17_592_186_044_415, 17_179_869_183),
        ] {
            let size = ByteSize::from_bytes(bytes);
            assert_eq!(size.as_bytes(), bytes);
            assert_eq!(size.as_mib(), mib);
            assert_eq!(size.as_gib(), gib);
        }
    }

    #[test]
    fn checked_constructors_accept_the_largest_fitting_value_but_not_more() {
        let max = ByteSize::from_bytes(u64::MAX);
        let mib = max.as_mib();
        assert_eq!(ByteSize::checked_from_mib(mib).unwrap().as_mib(), mib);
        assert_eq!(ByteSize::checked_from_mib(mib + 1), None);
        assert_eq!(ByteSize::checked_from_mib(u64::MAX), None);

        let gib = max.as_gib();
        assert_eq!(ByteSize::checked_from_gib(gib).unwrap().as_gib(), gib);
        assert_eq!(ByteSize::checked_from_gib(gib + 1), None);
        assert_eq!(ByteSize::checked_from_gib(u64::MAX), None);
    }

    #[test]
    fn scalar_division_returns_a_size_and_rounds_down_to_bytes() {
        let memory = ByteSize::checked_from_gib(16).unwrap();
        assert_eq!(memory.checked_div(4).unwrap().as_gib(), 4);

        for (bytes, divisor, expected_bytes) in [
            (11, 4, 2),
            (1, 4, 0),
            (0, 4, 0),
            (u64::MAX, 1, u64::MAX),
            (u64::MAX, u64::MAX, 1),
        ] {
            let size = ByteSize::from_bytes(bytes);
            let expected = ByteSize::from_bytes(expected_bytes);
            assert_eq!(size.checked_div(divisor), Some(expected));
        }
    }

    #[test]
    fn checked_division_rejects_zero_divisors() {
        assert_eq!(ByteSize::from_bytes(0).checked_div(0), None);
        assert_eq!(ByteSize::from_bytes(u64::MAX).checked_div(0), None);
    }

    #[test]
    fn byte_size_to_mib_rounds_down() {
        for (bytes, expected_mib) in [
            (0, 0),
            (1, 0),
            (1_048_575, 0),
            (1_048_576, 1),
            (1_048_577, 1),
        ] {
            assert_eq!(
                Mib::try_from(ByteSize::from_bytes(bytes)).unwrap(),
                Mib::new(expected_mib)
            );
        }
    }

    #[test]
    fn byte_size_to_mib_rejects_overflow_without_panicking() {
        let largest = ByteSize::checked_from_mib(u64::from(u32::MAX)).unwrap();
        assert_eq!(Mib::try_from(largest).unwrap(), Mib::new(u32::MAX));

        let first_overflow = ByteSize::checked_from_mib(u64::from(u32::MAX) + 1).unwrap();
        let last_fitting = ByteSize::from_bytes(first_overflow.as_bytes() - 1);
        assert_eq!(Mib::try_from(last_fitting).unwrap(), Mib::new(u32::MAX));
        assert!(Mib::try_from(first_overflow).is_err());
        assert!(Mib::try_from(ByteSize::from_bytes(u64::MAX)).is_err());
    }

    #[test]
    fn conversion_preserves_mib_counts_without_rescaling() {
        for count in [0, 1, 512, 8192, u32::MAX] {
            assert_eq!(u32::from(Mib::new(count)), count);
        }
    }
}
