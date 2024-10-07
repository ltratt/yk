pub(crate) trait SignExtend {
    fn sign_extend(&self, bits: u32) -> Self;
}

impl SignExtend for u64 {
    fn sign_extend(&self, bits: u32) -> Self {
        debug_assert!(
            bits > 0 && bits <= Self::BITS,
            "{bits} outside range 1..={}",
            Self::BITS
        );
        let shift = Self::BITS - bits;
        (*self << shift) >> shift
    }
}

pub(crate) trait Truncate {
    fn truncate(&self, bits: u32) -> Self;
}

impl Truncate for u64 {
    fn truncate(&self, bits: u32) -> Self {
        debug_assert!(
            bits > 0 && bits <= Self::BITS,
            "{bits} outside range 1..={}",
            Self::BITS
        );
        *self & ((1 as Self).wrapping_shl(bits) - 1)
    }
}
