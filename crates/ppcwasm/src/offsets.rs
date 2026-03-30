//! CPU register offsets within the [`gekko::Cpu`] struct layout.
//!
//! The compiled WebAssembly blocks access CPU registers via direct memory loads
//! and stores using these pre-computed values as the `offset` immediate of WASM
//! `i32.load` / `i32.store` instructions.  This is the same strategy used by
//! the Play! PS2 emulator's Nuanceur-to-WebAssembly backend: guest CPU
//! registers reside at known, statically computable byte offsets inside the
//! host's (WASM) linear memory, so JIT-emitted code can reach them with a
//! single load/store without any runtime address arithmetic.

use std::mem::{offset_of, size_of};

use gekko::{Cpu, FloatPair};

/// Pre-computed byte offsets of CPU register fields within [`Cpu`].
///
/// These constants are embedded as immediate `MemArg::offset` values in the
/// WASM `i32.load` / `i32.store` instructions that are emitted for every guest
/// register access.  Because [`Cpu`] and [`gekko::User`] are `#[repr(C)]` the
/// offsets are stable across builds with the same Rust toolchain version.
#[derive(Debug, Clone, Copy)]
pub struct RegOffsets {
    /// Byte offset of `Cpu::pc`.
    pub pc: u64,
    /// Byte offsets of `Cpu::user.gpr[0..32]`.
    pub gpr: [u64; 32],
    /// Byte offsets of the first element (`ps0`) of each `Cpu::user.fpr[i]`.
    pub fpr_ps0: [u64; 32],
    /// Byte offsets of the second element (`ps1`) of each `Cpu::user.fpr[i]`.
    pub fpr_ps1: [u64; 32],
    /// Byte offset of `Cpu::user.cr`.
    pub cr: u64,
    /// Byte offset of `Cpu::user.lr`.
    pub lr: u64,
    /// Byte offset of `Cpu::user.ctr`.
    pub ctr: u64,
    /// Byte offset of `Cpu::user.xer`.
    pub xer: u64,
    /// Byte offset of the low 32 bits of `Cpu::supervisor.misc.tb` (TBL, TBR 268).
    pub tb_lo: u64,
    /// Byte offset of the high 32 bits of `Cpu::supervisor.misc.tb` (TBU, TBR 269).
    pub tb_hi: u64,
    /// Byte offset of `Cpu::supervisor.config.msr`.
    pub msr: u64,
}

impl RegOffsets {
    /// Derives register offsets from the actual `#[repr(C)]` layout of
    /// [`Cpu`] / [`gekko::User`] at the current compilation target.
    pub fn compute() -> Self {
        let pc = offset_of!(Cpu, pc) as u64;
        let gpr_base = offset_of!(Cpu, user.gpr) as u64;
        let fpr_base = offset_of!(Cpu, user.fpr) as u64;
        let float_pair_size = size_of::<FloatPair>() as u64; // [f64; 2] = 16 bytes

        let mut gpr = [0u64; 32];
        let mut fpr_ps0 = [0u64; 32];
        let mut fpr_ps1 = [0u64; 32];

        for i in 0..32usize {
            gpr[i] = gpr_base + (i as u64) * 4;
            fpr_ps0[i] = fpr_base + (i as u64) * float_pair_size;
            fpr_ps1[i] = fpr_base + (i as u64) * float_pair_size + 8;
        }

        RegOffsets {
            pc,
            gpr,
            fpr_ps0,
            fpr_ps1,
            cr: offset_of!(Cpu, user.cr) as u64,
            lr: offset_of!(Cpu, user.lr) as u64,
            ctr: offset_of!(Cpu, user.ctr) as u64,
            xer: offset_of!(Cpu, user.xer) as u64,
            tb_lo: offset_of!(Cpu, supervisor.misc.tb) as u64,
            tb_hi: offset_of!(Cpu, supervisor.misc.tb) as u64 + 4,
            msr: offset_of!(Cpu, supervisor.config.msr) as u64,
        }
    }
}
