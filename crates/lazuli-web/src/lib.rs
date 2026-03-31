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
use std::mem::size_of;

use gekko::disasm::{Extensions, Ins};
use js_sys::WebAssembly;
use ppcwasm::WasmJit;
use wasm_bindgen::prelude::*;

// ─── WASM memory export ───────────────────────────────────────────────────────

/// Returns the WebAssembly linear memory of this module.
///
/// JavaScript can use the returned [`WebAssembly::Memory`] together with
/// [`WasmEmulator::ram_ptr`] and [`WasmEmulator::ram_size`] to create a
/// **zero-copy live view** over the emulator's guest RAM:
///
/// ```js
/// const mem   = wasm_memory();
/// const ptr   = emu.ram_ptr();
/// const size  = emu.ram_size();
/// const ram   = new Uint8Array(mem.buffer, ptr, size);
/// // `ram` is now a live, zero-copy view — any write from a compiled block
/// // is instantly visible without calling get_ram_copy() / sync_ram().
/// ```
#[wasm_bindgen]
pub fn wasm_memory() -> JsValue {
    wasm_bindgen::memory()
}

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
    /// GameCube controller button bitmask (set by JavaScript keyboard handler).
    pad_buttons: u32,
}

#[wasm_bindgen]
impl WasmEmulator {
    /// Create a new emulator with `ram_size` bytes of guest RAM.
    ///
    /// `ram_size` must be a multiple of 65536 (one WASM memory page).
    /// For a full GameCube emulation pass `24 * 1024 * 1024` (24 MiB).
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
            pad_buttons: 0,
        }
    }

    // ── ROM loading ───────────────────────────────────────────────────────────

    /// Copy `data` into guest RAM starting at `guest_addr`.
    ///
    /// `guest_addr` may be a GameCube virtual address (`0x8xxxxxxx`) or a raw
    /// physical offset; both are handled transparently via the same
    /// `0x01FF_FFFF` mask used by [`Self::phys_addr`].
    ///
    /// Clears the block cache for any PC that overlaps the written region, so
    /// that stale compiled blocks are not executed after a ROM reload.
    pub fn load_bytes(&mut self, guest_addr: u32, data: &[u8]) {
        let start = Self::phys_addr(guest_addr);
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
        // Invalidate compiled blocks in the written range.
        // PPC instructions are always 4 bytes wide; we align start/end to the
        // nearest 4-byte boundary with `& !3` (clear the low two bits) so
        // every overlapping instruction address is evicted from the cache.
        let start_page = (start & !3) as u32;
        let end_page = (end as u32 + 3) & !3;
        for pc in (start_page..end_page).step_by(4) {
            self.cache.invalidate(pc);
        }
        console_log!("[lazuli-web] loaded {} bytes at physical 0x{:08X}", data.len(), start);
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

    /// Notify the emulator that one compiled block has just been executed.
    ///
    /// Call this from JavaScript after a successful `instance.exports.execute()`
    /// call to keep the execution counter in sync with the host.
    pub fn record_block_executed(&mut self) {
        self.blocks_executed += 1;
    }

    /// Number of blocks currently in the module cache.
    pub fn cache_size(&self) -> u32 {
        self.cache.len() as u32
    }

    // ── Zero-copy RAM access ──────────────────────────────────────────────────

    /// Returns a raw pointer (WASM linear memory offset) to the start of the
    /// guest RAM buffer.
    ///
    /// Combine with [`wasm_memory`] and [`ram_size`] to create a live,
    /// zero-copy JavaScript view that avoids per-block `get_ram_copy()` calls:
    ///
    /// ```js
    /// const ram = new Uint8Array(wasm_memory().buffer, emu.ram_ptr(), emu.ram_size());
    /// ```
    pub fn ram_ptr(&self) -> u32 {
        self.ram.as_ptr() as u32
    }

    /// Returns the size of the guest RAM buffer in bytes.
    pub fn ram_size(&self) -> u32 {
        self.ram.len() as u32
    }

    // ── Controller input ──────────────────────────────────────────────────────

    /// Set the GameCube controller button bitmask.
    ///
    /// Called by the JavaScript keyboard handler on every `keydown` / `keyup`
    /// event.  The bitmask layout matches the `GC_BTN` constants defined in
    /// `bootstrap.js`.
    pub fn set_pad_buttons(&mut self, buttons: u32) {
        self.pad_buttons = buttons;
    }

    /// Get the current GameCube controller button bitmask.
    pub fn get_pad_buttons(&self) -> u32 {
        self.pad_buttons
    }

    // ── Timebase ──────────────────────────────────────────────────────────────

    /// Advance the CPU time-base register by `delta` ticks.
    ///
    /// The GameCube's Gekko time base increments at approximately 40.5 MHz
    /// (CPU clock / 12).  Call this once per animation frame so that
    /// time-base polling loops (`mftb` / `OSWaitVBlank`) see a monotonically
    /// increasing counter and do not spin forever.
    ///
    /// Suggested value: `675_000` ticks per frame (= 40.5 MHz / 60 fps).
    pub fn advance_timebase(&mut self, delta: u32) {
        self.cpu.supervisor.misc.tb =
            self.cpu.supervisor.misc.tb.wrapping_add(delta as u64);
    }

    /// Tick the decrementer down by `delta` ticks and deliver a decrementer
    /// exception if it wraps through zero.
    ///
    /// The GameCube decrementer counts down at the same 40.5 MHz rate as the
    /// time base.  When it transitions from a non-negative value to a negative
    /// value (i.e. bit 31 becomes set), the CPU fires the decrementer exception
    /// at vector `0x00000900` — provided external interrupts are enabled in the
    /// MSR (the `EE` / `interrupts` bit).
    ///
    /// Call this once per animation frame with the same `delta` as
    /// `advance_timebase` so that decrementer-driven timing loops and OS timer
    /// callbacks (`OSWaitVBlank`, alarms, etc.) see a ticking counter and do not
    /// spin forever.
    ///
    /// Suggested value: `675_000` ticks per frame (= 40.5 MHz / 60 fps).
    pub fn advance_decrementer(&mut self, delta: u32) {
        let old_dec = self.cpu.supervisor.misc.dec;
        let new_dec = old_dec.wrapping_sub(delta);
        self.cpu.supervisor.misc.dec = new_dec;

        // The decrementer exception fires on the transition from non-negative
        // (bit 31 clear) to negative (bit 31 set), but only when external
        // interrupts are enabled in the MSR.
        let fired = (old_dec as i32) >= 0 && (new_dec as i32) < 0;
        if fired && self.cpu.supervisor.config.msr.interrupts() {
            self.cpu.raise_exception(gekko::Exception::Decrementer);
        }
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
            console_log!("[lazuli] compile_block 0x{:08X}: no instructions in RAM", guest_pc);
            return Err(JsValue::from_str("no instructions at guest PC"));
        }

        // Build a human-readable disassembly string for the console.
        let disasm: Vec<String> = instructions
            .iter()
            .map(|(pc, ins)| format!("0x{:08X}: {:?}", pc, ins.op))
            .collect();

        let block = self
            .jit
            .build(instructions.into_iter())
            .ok_or_else(|| JsValue::from_str("JIT produced no block"))?;

        // Log every block with its disassembly so unimplemented instructions
        // are visible immediately in the browser console.
        if block.unimplemented_ops.is_empty() {
            console_log!(
                "[lazuli] block #{} @ 0x{:08X} ({} insns, {} WASM bytes): {}",
                self.blocks_compiled + 1,
                guest_pc,
                block.instruction_count,
                block.bytes.len(),
                disasm.join(" | "),
            );
        } else {
            console_log!(
                "[lazuli] block #{} @ 0x{:08X} ({} insns, {} WASM bytes) \
                 ⚠ UNIMPLEMENTED [{}]: {}",
                self.blocks_compiled + 1,
                guest_pc,
                block.instruction_count,
                block.bytes.len(),
                block.unimplemented_ops.join(", "),
                disasm.join(" | "),
            );
        }

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

    // ── CPU struct serialisation ──────────────────────────────────────────────

    /// Size in bytes of the [`gekko::Cpu`] struct.
    ///
    /// JavaScript should allocate at least this many bytes in the WASM memory
    /// that it passes as the `env.memory` import when instantiating a compiled
    /// block.  The WASM memory must be at least one page (65536 bytes), which
    /// is always enough to hold the CPU struct.
    pub fn cpu_struct_size(&self) -> u32 {
        size_of::<gekko::Cpu>() as u32
    }

    /// Serialise the current CPU register state into a [`js_sys::Uint8Array`].
    ///
    /// The returned bytes match the `#[repr(C)]` in-memory layout of
    /// [`gekko::Cpu`].  Write them to offset 0 of the `env.memory` WASM
    /// memory before calling `execute(0)` on a compiled block.
    pub fn get_cpu_bytes(&self) -> js_sys::Uint8Array {
        // SAFETY: `gekko::Cpu` is `#[repr(C)]` and contains only plain integer /
        // float fields.  We borrow it as a byte slice for the duration of this
        // call, which is safe.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&self.cpu as *const gekko::Cpu).cast::<u8>(),
                size_of::<gekko::Cpu>(),
            )
        };
        js_sys::Uint8Array::from(bytes)
    }

    /// Restore the CPU register state from raw bytes.
    ///
    /// `data` must have been produced by a previous call to [`get_cpu_bytes`]
    /// and must therefore have length exactly [`cpu_struct_size`] bytes.  Call
    /// this after `execute()` returns to sync the register changes made by the
    /// compiled block back into the Rust emulator.
    pub fn set_cpu_bytes(&mut self, data: &[u8]) {
        let expected = size_of::<gekko::Cpu>();
        if data.len() != expected {
            console_log!(
                "[lazuli-web] set_cpu_bytes: expected {} bytes, got {}",
                expected,
                data.len()
            );
            return;
        }
        // SAFETY: `data` has the correct size and alignment is guaranteed by
        // the `#[repr(C)]` layout.  The source slice is valid for the duration
        // of the copy.
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (&mut self.cpu as *mut gekko::Cpu).cast::<u8>(),
                expected,
            );
        }
    }

    /// Return a snapshot of the emulator's guest RAM as a [`js_sys::Uint8Array`].
    ///
    /// JavaScript hook closures (`read_u8`, `read_u32`, etc.) passed to
    /// `WebAssembly.instantiate()` can read from this buffer to implement
    /// guest memory loads.  For writes, call [`sync_ram`] after execution.
    pub fn get_ram_copy(&self) -> js_sys::Uint8Array {
        js_sys::Uint8Array::from(self.ram.as_slice())
    }

    /// Copy `data` back over the emulator's guest RAM.
    ///
    /// Call this after block execution if the block wrote to guest memory and
    /// you want those writes to be reflected in the Rust emulator state.
    pub fn sync_ram(&mut self, data: &[u8]) {
        let len = data.len().min(self.ram.len());
        self.ram[..len].copy_from_slice(&data[..len]);
    }

    /// Return the byte offsets of key CPU registers within the [`gekko::Cpu`]
    /// struct as a JavaScript object.
    ///
    /// JavaScript can use these offsets to directly read / write individual
    /// registers in the WASM memory buffer that holds the serialised CPU state.
    pub fn get_reg_offsets(&self) -> JsValue {
        let offsets = self.jit.offsets();
        let obj = js_sys::Object::new();
        let set = |key: &str, val: u64| {
            let _ = js_sys::Reflect::set(&obj, &key.into(), &JsValue::from(val as u32));
        };
        set("pc", offsets.pc);
        set("lr", offsets.lr);
        set("ctr", offsets.ctr);
        set("cr", offsets.cr);
        set("xer", offsets.xer);
        set("srr0", offsets.srr0);
        set("srr1", offsets.srr1);
        set("dec", offsets.dec);
        let gpr_arr = js_sys::Array::new();
        for &off in &offsets.gpr {
            gpr_arr.push(&JsValue::from(off as u32));
        }
        let _ = js_sys::Reflect::set(&obj, &"gpr".into(), &gpr_arr.into());
        let sprg_arr = js_sys::Array::new();
        for &off in &offsets.sprg {
            sprg_arr.push(&JsValue::from(off as u32));
        }
        let _ = js_sys::Reflect::set(&obj, &"sprg".into(), &sprg_arr.into());
        obj.into()
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

impl WasmEmulator {
    /// Translate a guest virtual address to a physical RAM offset.
    ///
    /// Strips the GameCube KSEG0 (`0x80000000`) and KSEG1 (`0xA0000000`)
    /// segment bits.  The mask `0x01FF_FFFF` (25 bits = 32 MiB) is
    /// intentionally one bit wider than the 24 MiB of main RAM so that the
    /// upper MiB of the address space — used by the GameCube for cache-locked
    /// L2 data and some hardware registers — also maps cleanly to physical
    /// offsets without wrapping.  Addresses already below 0x02000000 pass
    /// through unchanged, keeping backward compatibility with demo programs
    /// that use raw RAM offsets starting at 0.
    fn phys_addr(vaddr: u32) -> usize {
        (vaddr & 0x01FF_FFFF) as usize
    }

    /// Fetch PowerPC instructions from guest RAM at `guest_pc`, stopping at
    /// the first terminal instruction or after at most 32 instructions.
    ///
    /// `guest_pc` may be a GameCube virtual address (`0x8xxxxxxx`) or a raw
    /// physical RAM offset; both are handled via [`Self::phys_addr`].
    fn fetch_instructions(&self, guest_pc: u32) -> Vec<(u32, Ins)> {
        let exts = Extensions::gekko_broadway();
        let mut out = Vec::new();

        for i in 0..32usize {
            let pc = guest_pc.wrapping_add((i * 4) as u32);
            let addr = Self::phys_addr(pc);
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
            let is_terminal = matches!(
                ins.op,
                gekko::disasm::Opcode::B
                    | gekko::disasm::Opcode::Bc
                    | gekko::disasm::Opcode::Bclr
                    | gekko::disasm::Opcode::Bcctr
                    | gekko::disasm::Opcode::Rfi
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
