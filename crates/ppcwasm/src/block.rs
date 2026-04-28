//! Compiled WebAssembly block type.

/// Import function indices within the generated WASM module's import section.
///
/// The import section lists one memory import, one global import, then the hook
/// functions in this exact order:
/// ```text
/// (import "env" "memory" (memory 1))            — memory index 0
/// (import "env" "ram_base" (global i32))         — global index 0
/// 0  read_u8            (i32) -> i32
/// 1  read_u16           (i32) -> i32
/// 2  read_u32           (i32) -> i32
/// 3  read_f64           (i32) -> f64
/// 4  write_u8           (i32, i32) -> ()
/// 5  write_u16          (i32, i32) -> ()
/// 6  write_u32          (i32, i32) -> ()
/// 7  write_f64          (i32, f64) -> ()
/// 8  raise_exception    (i32) -> ()
/// 9  psq_load           (i32, i32) -> f64
/// 10 psq_store          (i32, i32, f64) -> i32
/// 11 psq_load_size      (i32) -> i32
/// 12 icbi               (i32) -> ()
/// 13 isync              () -> ()
/// ```
///
/// The `ram_base` global (index 0) is the byte offset of guest RAM inside the
/// WASM linear memory.  JIT blocks use it with direct `i32.load` / `i32.store`
/// instructions instead of hook calls for normal RAM addresses, avoiding the
/// JavaScript boundary crossing overhead on every memory operation.
/// Hook calls for `read_u32` / `write_u32` etc. are emitted only for MMIO
/// (`0xCCxxxxxx`) and L2C (`0xE0xxxxxx`) addresses.
pub mod imports {
    pub const READ_U8:          u32 = 0;
    pub const READ_U16:         u32 = 1;
    pub const READ_U32:         u32 = 2;
    pub const READ_F64:         u32 = 3;
    pub const WRITE_U8:         u32 = 4;
    pub const WRITE_U16:        u32 = 5;
    pub const WRITE_U32:        u32 = 6;
    pub const WRITE_F64:        u32 = 7;
    pub const RAISE_EXCEPTION:  u32 = 8;
    pub const PSQ_LOAD:         u32 = 9;
    pub const PSQ_STORE:        u32 = 10;
    pub const PSQ_LOAD_SIZE:    u32 = 11;
    /// Invalidate the JIT block at the given guest address (icbi).
    pub const ICBI:             u32 = 12;
    /// Flush the entire instruction-cache (isync / sync).
    pub const ISYNC:            u32 = 13;
    /// Total number of imported functions. `execute` is at function index `COUNT`.
    pub const COUNT:            u32 = 14;
}

/// A PowerPC basic block compiled to WebAssembly bytecode.
///
/// ## WASM module interface
///
/// The generated module exports a single function:
///
/// ```text
/// (func "execute" (param $regs_ptr i32) (result i32))
/// ```
///
/// | Parameter / return | Meaning |
/// |--------------------|---------|
/// | `$regs_ptr`        | Byte offset into WASM linear memory where [`gekko::Cpu`] begins. |
/// | return value       | Next PC to execute. `0` means the block already wrote `Cpu::pc`. |
///
/// ### Imported globals
///
/// | name       | type | meaning                                              |
/// |------------|------|------------------------------------------------------|
/// | `ram_base` | i32  | Byte offset of guest RAM in WASM linear memory.      |
///
/// ### Imported hooks
///
/// These are called only for MMIO (`0xCCxxxxxx`) and L2C (`0xE0xxxxxx`)
/// addresses; all other addresses use direct `i32.load` / `i32.store`.
///
/// | name              | signature              |
/// |-------------------|------------------------|
/// | `read_u8`         | `(i32) -> i32`         |
/// | `read_u16`        | `(i32) -> i32`         |
/// | `read_u32`        | `(i32) -> i32`         |
/// | `read_f64`        | `(i32) -> f64`         |
/// | `write_u8`        | `(i32, i32) -> ()`     |
/// | `write_u16`       | `(i32, i32) -> ()`     |
/// | `write_u32`       | `(i32, i32) -> ()`     |
/// | `write_f64`       | `(i32, f64) -> ()`     |
/// | `raise_exception` | `(i32) -> ()`          |
/// | `psq_load`        | `(i32, i32) -> f64`    |
/// | `psq_store`       | `(i32, i32, f64) -> i32` |
/// | `psq_load_size`   | `(i32) -> i32`         |
/// | `icbi`            | `(i32) -> ()`          |
/// | `isync`           | `() -> ()`             |
#[derive(Debug, Clone)]
pub struct WasmBlock {
    /// Raw WebAssembly binary module bytes.
    pub bytes: Vec<u8>,
    /// Number of PowerPC instructions compiled into this block.
    pub instruction_count: u32,
    /// Estimated cycle count (one per instruction).
    pub cycles: u32,
    /// Opcode names for unimplemented instructions that fell through to
    /// `raise_exception`.
    pub unimplemented_ops: Vec<String>,
}
