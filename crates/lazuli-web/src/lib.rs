//! Browser WebAssembly frontend for the Lazuli GameCube emulator.
//!
//! ## Architecture overview
//!
//! This crate bridges the [`ppcwasm`] JIT backend to the web browser.  The
//! overall data-flow mirrors the approach used by the Play! PS2 emulator:
//!
//! ```text
//!  PowerPC instructions
//!         │
//!         ▼
//!   ppcwasm::WasmJit                ← compiled dynarec-to-WASM JIT
//!         │ WasmBlock { bytes: Vec<u8> }
//!         ▼
//!  WebAssembly.compile(bytes)       ← browser WASM compiler
//!         │ WebAssembly::Module
//!         ▼
//!  WebAssembly.instantiate(module,  ← bind hook functions
//!    { hooks: { read_u32, … } })
//!         │ WebAssembly::Instance
//!         ▼
//!   instance.execute(regs_ptr)      ← execute compiled block
//! ```
//!
//! [`WasmEmulator`] is the main struct exported to JavaScript via
//! [`wasm_bindgen`].  It owns a [`ppcwasm::WasmJit`] instance and a
//! cache that maps guest PC values to compiled [`js_sys::WebAssembly::Module`]
//! objects so that each block is compiled at most once.
//!
//! ## Hook functions
//!
//! Compiled blocks import memory-access functions from the `"hooks"` module.
//! These are provided by JavaScript closures captured when the block is
//! instantiated.  In a full emulator integration the hook closures would call
//! back into Rust through [`wasm_bindgen`]; in this initial implementation
//! they are thin JavaScript shims that forward to a typed array view of the
//! emulator's RAM.

use std::collections::HashMap;

use gekko::disasm::{Extensions, Ins};
use js_sys::WebAssembly;
use ppcwasm::WasmJit;
use wasm_bindgen::prelude::*;

// Re-export the block type for JavaScript inspection.
pub use ppcwasm::WasmBlock;

// ─── Console logging helper ───────────────────────────────────────────────────

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

macro_rules! console_log {
    ($($t:tt)*) => (log(&format!($($t)*)))
}

// ─── WasmBlockCache ──────────────────────────────────────────────────────────

/// Compiled and cached WASM modules, keyed by guest PC.
struct WasmBlockCache {
    modules: HashMap<u32, WebAssembly::Module>,
}

impl WasmBlockCache {
    fn new() -> Self {
        Self { modules: HashMap::new() }
    }

    /// Compile a [`WasmBlock`] into a [`WebAssembly::Module`] and cache it.
    ///
    /// `new WebAssembly.Module(bytes)` is synchronous and permitted inside
    /// WebWorker contexts (where the Lazuli emulator loop is intended to run).
    fn compile_and_cache(
        &mut self,
        pc: u32,
        block: &WasmBlock,
    ) -> Result<&WebAssembly::Module, JsValue> {
        if !self.modules.contains_key(&pc) {
            let bytes = js_sys::Uint8Array::from(block.bytes.as_slice());
            let module = WebAssembly::Module::new(&bytes)?;
            self.modules.insert(pc, module);
        }
        Ok(self.modules.get(&pc).unwrap())
    }

    #[allow(dead_code)]
    fn get(&self, pc: u32) -> Option<&WebAssembly::Module> {
        self.modules.get(&pc)
    }

    fn invalidate(&mut self, pc: u32) {
        self.modules.remove(&pc);
    }

    fn len(&self) -> usize {
        self.modules.len()
    }
}

// ─── WasmEmulator ────────────────────────────────────────────────────────────

/// A minimal GameCube CPU emulator that runs in the browser using a
/// PowerPC → WebAssembly dynarec.
///
/// Exported to JavaScript via `wasm-bindgen`.  The emulator maintains a
/// [`gekko::Cpu`] register file and a flat RAM array.  Compiled PPC blocks are
/// cached as [`WebAssembly::Module`]s and instantiated on demand with
/// JavaScript hook closures for guest memory access.
#[wasm_bindgen]
pub struct WasmEmulator {
    cpu: gekko::Cpu,
    ram: Vec<u8>,
    jit: WasmJit,
    cache: WasmBlockCache,
    /// Number of blocks compiled since emulator creation.
    blocks_compiled: u64,
    /// Number of blocks executed since emulator creation.
    blocks_executed: u64,
}

#[wasm_bindgen]
impl WasmEmulator {
    /// Create a new emulator with `ram_size` bytes of guest RAM.
    ///
    /// `ram_size` must be a multiple of 65536 (one WASM memory page).
    #[wasm_bindgen(constructor)]
    pub fn new(ram_size: u32) -> WasmEmulator {
        console_error_panic_hook_set();
        WasmEmulator {
            cpu: gekko::Cpu::default(),
            ram: vec![0u8; ram_size as usize],
            jit: WasmJit::new(),
            cache: WasmBlockCache::new(),
            blocks_compiled: 0,
            blocks_executed: 0,
        }
    }

    // ── ROM loading ───────────────────────────────────────────────────────────

    /// Copy `data` into guest RAM starting at `guest_addr`.
    ///
    /// Clears the block cache for any PC that overlaps the written region, so
    /// that stale compiled blocks are not executed after a ROM reload.
    pub fn load_bytes(&mut self, guest_addr: u32, data: &[u8]) {
        let start = guest_addr as usize;
        let end = start + data.len();
        if end > self.ram.len() {
            console_log!(
                "[lazuli-web] load_bytes: data range 0x{:08X}..0x{:08X} exceeds RAM size 0x{:08X}",
                start,
                end,
                self.ram.len()
            );
            return;
        }
        self.ram[start..end].copy_from_slice(data);
        // Invalidate compiled blocks in the written range
        let start_page = (guest_addr & !3) as u32;
        let end_page = (guest_addr + data.len() as u32 + 3) & !3;
        for pc in (start_page..end_page).step_by(4) {
            self.cache.invalidate(pc);
        }
        console_log!("[lazuli-web] loaded {} bytes at guest 0x{:08X}", data.len(), guest_addr);
    }

    // ── Register access (for JS debugging) ───────────────────────────────────

    /// Set the program counter.
    pub fn set_pc(&mut self, pc: u32) {
        self.cpu.pc = gekko::Address(pc);
    }

    /// Get the current program counter.
    pub fn get_pc(&self) -> u32 {
        self.cpu.pc.0
    }

    /// Get GPR[i].
    pub fn get_gpr(&self, i: u8) -> u32 {
        self.cpu.user.gpr[i as usize]
    }

    /// Set GPR[i].
    pub fn set_gpr(&mut self, i: u8, value: u32) {
        self.cpu.user.gpr[i as usize] = value;
    }

    // ── Compilation stats ─────────────────────────────────────────────────────

    /// Number of distinct blocks that have been JIT-compiled to WASM.
    pub fn blocks_compiled(&self) -> u32 {
        self.blocks_compiled as u32
    }

    /// Number of blocks that have been executed.
    pub fn blocks_executed(&self) -> u32 {
        self.blocks_executed as u32
    }

    /// Number of blocks currently in the module cache.
    pub fn cache_size(&self) -> u32 {
        self.cache.len() as u32
    }

    // ── Block compilation ─────────────────────────────────────────────────────

    /// Compile the PowerPC basic block starting at `guest_pc` and return its
    /// WASM bytecode as a [`Uint8Array`].
    ///
    /// This is the key step that mirrors the Play! emulator's dynarec-to-WASM
    /// pipeline: raw guest machine code is translated into a self-contained
    /// WASM binary module.
    ///
    /// The returned bytes can be passed directly to `WebAssembly.instantiate()`
    /// in JavaScript to obtain a callable block.
    pub fn compile_block(&mut self, guest_pc: u32) -> Result<js_sys::Uint8Array, JsValue> {
        let instructions = self.fetch_instructions(guest_pc);
        if instructions.is_empty() {
            return Err(JsValue::from_str("no instructions at guest PC"));
        }

        let block = self
            .jit
            .build(instructions.into_iter())
            .ok_or_else(|| JsValue::from_str("JIT produced no block"))?;

        self.blocks_compiled += 1;
        let bytes = js_sys::Uint8Array::from(block.bytes.as_slice());
        Ok(bytes)
    }

    /// Compile the block at `guest_pc` into the internal [`WasmBlockCache`]
    /// and return a JS object with metadata.
    pub fn compile_and_cache_block(&mut self, guest_pc: u32) -> Result<JsValue, JsValue> {
        let instructions = self.fetch_instructions(guest_pc);
        if instructions.is_empty() {
            return Err(JsValue::from_str("no instructions at guest PC"));
        }

        let block = self
            .jit
            .build(instructions.into_iter())
            .ok_or_else(|| JsValue::from_str("JIT produced no block"))?;

        let ins_count = block.instruction_count;
        let byte_len = block.bytes.len() as u32;

        self.cache.compile_and_cache(guest_pc, &block)?;
        self.blocks_compiled += 1;

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"pc".into(), &JsValue::from(guest_pc))?;
        js_sys::Reflect::set(&obj, &"instructionCount".into(), &JsValue::from(ins_count))?;
        js_sys::Reflect::set(&obj, &"wasmByteLength".into(), &JsValue::from(byte_len))?;
        Ok(obj.into())
    }

    // ── JavaScript hook import object ─────────────────────────────────────────

    /// Build the import object required to instantiate a compiled block.
    ///
    /// Returns a JavaScript object of the form:
    /// ```js
    /// {
    ///   hooks: {
    ///     read_u8:          (addr) => …,
    ///     read_u16:         (addr) => …,
    ///     read_u32:         (addr) => …,
    ///     write_u8:         (addr, val) => …,
    ///     write_u16:        (addr, val) => …,
    ///     write_u32:        (addr, val) => …,
    ///     raise_exception:  (kind) => …,
    ///   }
    /// }
    /// ```
    ///
    /// The hook closures read from and write to the emulator's RAM array.
    /// Note: this method returns a description object; the actual closures are
    /// created on the JavaScript side using the pattern shown in `index.html`.
    pub fn make_import_descriptor(&self) -> JsValue {
        // Return metadata that JS can use to wire up its own closures against
        // the emulator's WASM linear memory.
        let obj = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &obj,
            &"ramSize".into(),
            &JsValue::from(self.ram.len() as u32),
        );
        let _ = js_sys::Reflect::set(
            &obj,
            &"hookNames".into(),
            &{
                let arr = js_sys::Array::new();
                for name in &[
                    "read_u8",
                    "read_u16",
                    "read_u32",
                    "write_u8",
                    "write_u16",
                    "write_u32",
                    "raise_exception",
                ] {
                    arr.push(&JsValue::from_str(name));
                }
                arr.into()
            },
        );
        obj.into()
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

impl WasmEmulator {
    /// Fetch PowerPC instructions from guest RAM at `guest_pc`, stopping at
    /// the first terminal instruction or after at most 32 instructions.
    fn fetch_instructions(&self, guest_pc: u32) -> Vec<(u32, Ins)> {
        let exts = Extensions::gekko_broadway();
        let mut out = Vec::new();

        for i in 0..32usize {
            let addr = (guest_pc as usize).wrapping_add(i * 4);
            if addr + 4 > self.ram.len() {
                break;
            }
            let word = u32::from_be_bytes([
                self.ram[addr],
                self.ram[addr + 1],
                self.ram[addr + 2],
                self.ram[addr + 3],
            ]);
            let ins = Ins::new(word, exts);
            let pc = guest_pc.wrapping_add((i * 4) as u32);
            let is_terminal = matches!(
                ins.op,
                gekko::disasm::Opcode::B
                    | gekko::disasm::Opcode::Bc
                    | gekko::disasm::Opcode::Bclr
                    | gekko::disasm::Opcode::Bcctr
            );
            out.push((pc, ins));
            if is_terminal {
                break;
            }
        }

        out
    }
}

// ─── Panic hook ───────────────────────────────────────────────────────────────

fn console_error_panic_hook_set() {
    // Forward Rust panics to the browser console for easier debugging.
    // Enabled when the `console_error_panic_hook` crate is available.
}
