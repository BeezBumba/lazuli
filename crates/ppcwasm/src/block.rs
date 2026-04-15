//! Compiled WebAssembly block type.

/// Import function indices within the generated WASM module's import section.
///
/// The import section lists hook functions in this exact order:
/// ```text
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
/// ```
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
    /// Total number of imported functions. `execute` is at function index `COUNT`.
    pub const COUNT:            u32 = 12;
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
/// ### Imported hooks
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
