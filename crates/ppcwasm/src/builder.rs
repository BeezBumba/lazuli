//! Compatibility shim: re-exports the `ppc_mask` helper used by tests in
//! `lib.rs` that test `builder::ppc_mask`.

/// Generate a PowerPC rotate/mask bitmask (MB, ME in PPC big-endian bit numbering).
#[cfg(test)]
pub(crate) fn ppc_mask(mb: u32, me: u32) -> u32 {
    ppcir::decode::ppc_mask(mb, me)
}

#[cfg(test)]
mod tests {
    use super::ppc_mask;

    #[test]
    fn ppc_mask_full_range() { assert_eq!(ppc_mask(0, 31), 0xFFFF_FFFF); }
    #[test]
    fn ppc_mask_lower_byte() { assert_eq!(ppc_mask(24, 31), 0x0000_00FF); }
    #[test]
    fn ppc_mask_upper_halfword() { assert_eq!(ppc_mask(0, 15), 0xFFFF_0000); }
}
