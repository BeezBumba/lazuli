//! Compiled WebAssembly block type.

/// Import function indices within the generated WASM module's import section.
///
/// The import section lists hook functions in this exact order, and these
/// constants serve as the function indices used in `call` instructions.
pub mod imports {
    /// `read_u8(addr: i32) -> i32`  —  load a byte from guest address space.
    pub const READ_U8: u32 = 0;
    /// `read_u16(addr: i32) -> i32`  —  load a half-word.
    pub const READ_U16: u32 = 1;
    /// `read_u32(addr: i32) -> i32`  —  load a word.
    pub const READ_U32: u32 = 2;
    /// `write_u8(addr: i32, val: i32)`  —  store a byte.
    pub const WRITE_U8: u32 = 3;
    /// `write_u16(addr: i32, val: i32)`  —  store a half-word.
    pub const WRITE_U16: u32 = 4;
    /// `write_u32(addr: i32, val: i32)`  —  store a word.
    pub const WRITE_U32: u32 = 5;
    /// `raise_exception(kind: i32)`  —  signal a CPU exception.
    pub const RAISE_EXCEPTION: u32 = 6;
    /// Number of imported functions.  The `execute` function index is `COUNT`.
    pub const COUNT: u32 = 7;
}

/// A PowerPC basic block compiled to WebAssembly bytecode.
///
/// ## WASM Module Interface
///
/// The generated module exports a single function:
///
/// ```text
/// (func "execute" (param $regs_ptr i32) (result i32))
/// ```
///
/// | Parameter / return | Meaning |
/// |--------------------|---------|
/// | `$regs_ptr` | Byte offset into WASM linear memory where the [`gekko::Cpu`] struct begins. |
/// | return value | The next PC address the emulator should jump to.  `0` indicates that the block already updated `Cpu::pc` in place (computed branches to LR / CTR). |
///
/// The module imports a linear memory and seven functions:
///
/// | namespace | name              | kind               |
/// |-----------|-------------------|--------------------|
/// | `"env"`   | `"memory"`        | memory (min 1 page)|
///
/// | index | name              | signature          |
/// |-------|-------------------|--------------------|
/// | 0     | `read_u8`         | `(i32) -> i32`     |
/// | 1     | `read_u16`        | `(i32) -> i32`     |
/// | 2     | `read_u32`        | `(i32) -> i32`     |
/// | 3     | `write_u8`        | `(i32, i32) -> ()` |
/// | 4     | `write_u16`       | `(i32, i32) -> ()` |
/// | 5     | `write_u32`       | `(i32, i32) -> ()` |
/// | 6     | `raise_exception` | `(i32) -> ()`      |
#[derive(Debug, Clone)]
pub struct WasmBlock {
    /// Raw WebAssembly binary module bytes.
    pub bytes: Vec<u8>,
    /// Number of PowerPC instructions compiled into this block.
    pub instruction_count: u32,
    /// Estimated cycle count (one cycle per instruction, matching the
    /// interpreter's default accounting).
    pub cycles: u32,
}
