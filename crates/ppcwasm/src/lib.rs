//! PowerPC → WebAssembly JIT compiler.
//!
//! This crate implements the same "compiled dynarec to WASM" pattern used by
//! the [Play!] PS2 emulator: instead of emitting native machine code via a
//! backend like Cranelift, it emits **WebAssembly bytecode** that can be
//! compiled and executed by any WASM runtime — in particular by a web browser
//! via `WebAssembly.instantiate()`.
//!
//! ## How it works
//!
//! 1. The caller feeds a sequence of [`gekko::disasm::Ins`] (raw PowerPC
//!    instruction words) to [`WasmJit::build`].
//! 2. [`WasmJit`] translates each instruction into a sequence of typed WASM
//!    stack-machine opcodes using [`builder::BlockBuilder`].
//! 3. The opcodes are assembled into a self-contained WASM binary module
//!    (type / import / function / export / code sections) and returned as a
//!    [`WasmBlock`].
//! 4. The caller (e.g. `lazuli-web`) compiles the WASM binary with the
//!    browser's `WebAssembly.compile()` API and instantiates it with the hook
//!    functions that implement guest memory reads/writes.
//!
//! ## Relationship to `ppcjit`
//!
//! [`ppcjit`] uses Cranelift to emit native machine code for x86-64 and
//! AArch64.  `ppcwasm` is a *parallel* backend that targets WebAssembly
//! instead of native ISAs.  Both share the same guest-register layout (the
//! `#[repr(C)]` [`gekko::Cpu`] struct) and the same hook interface, so the
//! same [`cores`] dispatcher can route blocks to whichever backend is
//! appropriate for the current execution environment.
//!
//! [Play!]: https://github.com/jpd002/Play-

pub mod block;
pub mod offsets;

pub(crate) mod builder;

pub use block::WasmBlock;
pub use offsets::RegOffsets;

use gekko::disasm::Ins;

/// PowerPC → WebAssembly JIT compiler.
///
/// Translates PowerPC instruction sequences into [`WasmBlock`]s — WASM binary
/// modules that implement `execute(regs_ptr: i32) -> i32`.
///
/// One `WasmJit` instance can be shared across the lifetime of the emulator;
/// it holds only the pre-computed [`RegOffsets`] and therefore has no mutable
/// state during compilation.
pub struct WasmJit {
    offsets: RegOffsets,
}

impl WasmJit {
    /// Creates a new [`WasmJit`] instance.
    ///
    /// The [`RegOffsets`] are derived once from the `#[repr(C)]` layout of
    /// [`gekko::Cpu`] and reused for every subsequent compilation.
    pub fn new() -> Self {
        Self { offsets: RegOffsets::compute() }
    }

    /// Returns the register offsets used by this JIT instance.
    pub fn offsets(&self) -> &RegOffsets {
        &self.offsets
    }

    /// Compiles a PowerPC instruction sequence into a [`WasmBlock`].
    ///
    /// Instructions are consumed until either:
    /// - a terminal instruction is encountered (unconditional / conditional
    ///   branch, `blr`, `bcctr`), or
    /// - the iterator is exhausted.
    ///
    /// Returns `None` if the iterator yields no instructions.
    ///
    /// # Parameters
    ///
    /// * `instructions` — iterator yielding `(pc, instruction)` pairs.  The
    ///   PC of each instruction is required to compute branch targets.
    pub fn build(
        &self,
        instructions: impl Iterator<Item = (u32, Ins)>,
    ) -> Option<WasmBlock> {
        let mut builder = builder::BlockBuilder::new(&self.offsets);
        let mut last_pc = 0u32;
        let mut count = 0u32;

        for (pc, ins) in instructions {
            last_pc = pc;
            count += 1;
            let terminal = builder.emit(ins, pc);
            if terminal {
                return Some(builder.finish(pc + 4));
            }
        }

        if count == 0 {
            return None;
        }

        Some(builder.finish(last_pc + 4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gekko::disasm::{Extensions, Ins};

    fn ins(code: u32) -> Ins {
        Ins::new(code, Extensions::gekko_broadway())
    }

    #[test]
    fn build_empty_returns_none() {
        let jit = WasmJit::new();
        assert!(jit.build(std::iter::empty()).is_none());
    }

    #[test]
    fn build_single_nop_produces_valid_wasm() {
        let jit = WasmJit::new();
        // ori r0, r0, 0  (the canonical PPC nop)
        let nop = ins(0x6000_0000);
        let block = jit.build([(0x8000_0000u32, nop)].into_iter()).unwrap();

        assert_eq!(block.instruction_count, 1);
        // The output must start with the WASM magic number 0x00 0x61 0x73 0x6D
        assert_eq!(&block.bytes[..4], b"\0asm");
    }

    #[test]
    fn build_addi_block_is_valid_wasm() {
        let jit = WasmJit::new();
        // addi r3, r0, 42   (li r3, 42)
        let addi = ins(0x3860_002A);
        let block = jit.build([(0x8000_0000u32, addi)].into_iter()).unwrap();
        assert_eq!(&block.bytes[..4], b"\0asm");
        assert_eq!(block.instruction_count, 1);
    }

    #[test]
    fn build_branch_terminates_block() {
        let jit = WasmJit::new();
        // b 0x10  (unconditional branch forward by 16)
        let b_ins = ins(0x4800_0010);
        // addi r3, r0, 1  (should NOT be included)
        let addi = ins(0x3860_0001);

        let block = jit
            .build([(0x8000_0000u32, b_ins), (0x8000_0004u32, addi)].into_iter())
            .unwrap();

        // Only the branch instruction should be compiled
        assert_eq!(block.instruction_count, 1);
        assert_eq!(&block.bytes[..4], b"\0asm");
    }

    #[test]
    fn ppc_mask_sanity() {
        // Full word
        assert_eq!(super::builder::ppc_mask(0, 31), 0xFFFF_FFFFu32);
        // Low byte
        assert_eq!(super::builder::ppc_mask(24, 31), 0x0000_00FFu32);
    }
}
