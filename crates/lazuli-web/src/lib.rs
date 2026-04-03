//! Browser WebAssembly frontend for the Lazuli GameCube emulator.
//!
//! ## Architecture overview
//!
//! This crate bridges the [`ppcwasm`] JIT backend to the web browser and
//! provides the Rust-side foundations for full in-browser emulation,
//! mirroring the approach used by the [Play!] PS2 emulator:
//!
//! - **CPU:** PowerPC → WebAssembly dynarec via [`ppcwasm::WasmJit`]
//! - **GPU:** WebGPU via `wgpu` (feature `webgpu` on wasm32) — see [`check_webgpu_support`]
//! - **Audio:** Web Audio API `AudioWorkletNode` driven by GameCube DSP output
//! - **IO:** GameCube controller via the browser Gamepad API
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
//! instantiated.  The hook closures call back into the zero-copy RAM view
//! backed by the emulator's WASM linear memory.
//!
//! [Play!]: https://github.com/jpd002/Play-

use std::collections::HashMap;
use std::mem::size_of;

use gekko::disasm::{Extensions, Ins};
use js_sys::WebAssembly;
use ppcwasm::WasmJit;
use wasm_bindgen::prelude::*;

// ─── WebGPU support detection ─────────────────────────────────────────────────

/// Returns `true` if the browser exposes `navigator.gpu` (WebGPU is available).
///
/// WebGPU is the GPU API used by `wgpu` on `wasm32` targets (enabled via the
/// `webgpu` feature).  Call this from JavaScript before attempting to
/// initialise the GPU renderer; fall back to the canvas-based XFB blitter if
/// it returns `false`.
///
/// # Browser support
///
/// Browser WebGPU support is still evolving; check
/// [MDN](https://developer.mozilla.org/en-US/docs/Web/API/WebGPU_API) for the
/// current compatibility table.  Chrome and Edge have shipped WebGPU in stable
/// releases since 2023; Firefox and Safari have varying levels of support
/// depending on the platform and flags in use.
#[wasm_bindgen]
pub fn check_webgpu_support() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    // `navigator.gpu` is `undefined` on browsers without WebGPU support.
    js_sys::Reflect::get(&window.navigator(), &JsValue::from_str("gpu"))
        .map(|v| !v.is_undefined() && !v.is_null())
        .unwrap_or(false)
}

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

// ─── DiscImageDevice / DVD Interface ─────────────────────────────────────────

/// Physical base address of the GameCube DVD Interface (DI) registers.
///
/// The DI lives at MMIO offset 0x6000 from the GameCube's hardware base
/// (0x0C000000 uncached / 0xCC000000 cached), giving a virtual address of
/// `0xCC006000`.  See YAGCD §9.3 for the complete register map.
///
/// Note: the Processor Interface (PI) occupies offset 0x3000 (0xCC003000) and
/// must not be confused with the DVD Interface.
const DI_BASE: u32 = 0xCC00_6000;
/// Number of bytes covered by the DI register bank (10 × 4-byte registers).
const DI_SIZE: u32 = 0x28;

/// DVD Interface hardware register file.
///
/// Mirrors the ten memory-mapped I/O registers at `0xCC006000–0xCC006027`.
/// This is the Rust-side counterpart of Play!'s `Js_DiscImageDeviceStream`:
/// the emulated disc controller that reads sectors from the stored ISO image
/// and DMAs them into guest RAM when the game issues a DVD Read command.
///
/// ## Register map
///
/// | Offset | Name       | Description                                   |
/// |--------|------------|-----------------------------------------------|
/// | 0x00   | DISTATUS   | Status & interrupt flags (TCINT at bit 1)     |
/// | 0x04   | DICOVER    | Lid/cover state (bit 2 = closed = disc present) |
/// | 0x08   | DICMDBUF0  | Command word (command byte in bits 31–24)     |
/// | 0x0C   | DICMDBUF1  | Disc byte offset for Read command             |
/// | 0x10   | DICMDBUF2  | Reserved / command parameter 2                |
/// | 0x14   | DIMAR      | DMA destination address in main RAM           |
/// | 0x18   | DILENGTH   | DMA transfer length in bytes                  |
/// | 0x1C   | DICR       | Control — bit 0 = TSTART (begin transfer)     |
/// | 0x20   | DIIMMBUF   | Immediate data buffer                         |
/// | 0x24   | DICFG      | Configuration register                        |
#[derive(Default)]
struct DiState {
    /// DISTATUS (0x00): status and interrupt flags.
    status: u32,
    /// DICOVER (0x04): lid/cover state.  Bit 2 = 1 → cover closed (disc present).
    cover: u32,
    /// DICMDBUF0 (0x08): command word; bits 31–24 hold the command code.
    cmd_buf0: u32,
    /// DICMDBUF1 (0x0C): disc byte offset used by the DVD Read command.
    cmd_buf1: u32,
    /// DICMDBUF2 (0x10): second command parameter (reserved for most commands).
    cmd_buf2: u32,
    /// DIMAR (0x14): physical RAM destination for DMA transfers.
    dma_addr: u32,
    /// DILENGTH (0x18): number of bytes to transfer.
    dma_len: u32,
    /// DICR (0x1C): control register.  Writing bit 0 (TSTART) begins the transfer.
    control: u32,
    /// DIIMMBUF (0x20): immediate data returned by non-DMA commands.
    imm_buf: u32,
    /// DICFG (0x24): drive configuration.
    config: u32,
}

impl DiState {
    /// Read a 32-bit value from the DI register at `offset` bytes from `DI_BASE`.
    fn read_reg(&self, offset: u32) -> u32 {
        match offset {
            0x00 => self.status,
            0x04 => self.cover,
            0x08 => self.cmd_buf0,
            0x0C => self.cmd_buf1,
            0x10 => self.cmd_buf2,
            0x14 => self.dma_addr,
            0x18 => self.dma_len,
            0x1C => self.control,
            0x20 => self.imm_buf,
            0x24 => self.config,
            _ => 0,
        }
    }

    /// Write `val` to the DI register at `offset` bytes from `DI_BASE`.
    ///
    /// Returns `true` if the write sets the TSTART bit in DICR, signalling
    /// that a disc command should be processed immediately.
    fn write_reg(&mut self, offset: u32, val: u32) -> bool {
        match offset {
            0x00 => self.status = val,
            0x04 => self.cover = val,
            0x08 => self.cmd_buf0 = val,
            0x0C => self.cmd_buf1 = val,
            0x10 => self.cmd_buf2 = val,
            0x14 => self.dma_addr = val,
            0x18 => self.dma_len = val,
            0x1C => {
                let tstart = (val & 0x1) != 0;
                self.control = val;
                if tstart {
                    return true;
                }
            }
            0x20 => self.imm_buf = val,
            0x24 => self.config = val,
            _ => {}
        }
        false
    }
}

// ─── WasmEmulator ────────────────────────────────────────────────────────────

/// GameCube emulator running entirely in the browser via WebAssembly.
///
/// Exported to JavaScript via `wasm-bindgen`.  The emulator maintains a
/// [`gekko::Cpu`] register file and a flat RAM array.  Compiled PPC blocks are
/// cached as [`WebAssembly::Module`]s and instantiated on demand with
/// JavaScript hook closures for guest memory access.
///
/// This is the Rust counterpart to the JavaScript game loop in `bootstrap.js`.
/// Together they implement the same architecture used by the [Play!] PS2
/// emulator in the browser: CPU dynarec via WASM + GPU via WebGPU +
/// audio via `AudioWorkletNode` + IO via the Gamepad API.
///
/// [Play!]: https://github.com/jpd002/Play-
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
    /// Guest PC of the most recently compiled block (0 if none yet).
    last_compiled_pc: u32,
    /// PPC instruction count of the most recently compiled block.
    last_compiled_ins_count: u32,
    /// WASM byte length of the most recently compiled block.
    last_compiled_wasm_bytes: u32,
    /// Number of blocks that contained at least one unimplemented opcode.
    unimplemented_block_count: u64,
    /// Number of `raise_exception` calls forwarded from the JS hook.
    raise_exception_count: u64,
    /// Raw ISO disc image bytes, stored so the emulated DVD controller can
    /// service in-game sector reads.  `None` until [`load_disc_image`] is called.
    disc: Option<Vec<u8>>,
    /// DVD Interface (DI) hardware register state.
    di: DiState,
    /// True when the decrementer has transitioned from non-negative to negative
    /// but the exception has not yet been delivered because MSR.EE was clear at
    /// the time of the transition.  The exception is held pending and will be
    /// raised on the next `advance_decrementer` call that finds MSR.EE set.
    decrementer_pending: bool,
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
            last_compiled_pc: 0,
            last_compiled_ins_count: 0,
            last_compiled_wasm_bytes: 0,
            unimplemented_block_count: 0,
            raise_exception_count: 0,
            disc: None,
            di: DiState::default(),
            decrementer_pending: false,
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

    // ── DiscImageDevice — Play!-style streaming disc reads ────────────────────

    /// Store a raw GameCube ISO disc image for runtime sector reads.
    ///
    /// This is the Rust counterpart of Play!'s `DiscImageDevice.ts` +
    /// `Js_DiscImageDeviceStream.cpp`: once the ISO bytes are stored here the
    /// emulated DVD Interface can service `A8h` (DVD Read) DMA commands during
    /// gameplay, so that games that stream textures, audio, or level data from
    /// disc continue to work after the initial boot DOL has been loaded.
    ///
    /// Call this in addition to (not instead of) the DOL-loading path in
    /// `parseAndLoadIso` / `load_bytes`.
    pub fn load_disc_image(&mut self, data: &[u8]) {
        let disc_size_mib = data.len() / (1024 * 1024);
        self.disc = Some(data.to_vec());
        // Mark the cover as closed (bit 2 = DICVR_STATE = 1 → cover closed,
        // disc present) so games that poll DICOVER before issuing commands see
        // a valid disc in the drive.
        self.di.cover = 0x4;
        console_log!("[lazuli] DiscImageDevice: stored {} MiB disc image for runtime reads", disc_size_mib);
    }

    // ── Hardware-register I/O (hook bridge) ───────────────────────────────────

    /// Read a 32-bit value from a GameCube hardware register.
    ///
    /// Called by the JavaScript `read_u32` hook when the guest address has
    /// the prefix `0xCC` (GameCube memory-mapped I/O space), **before** the
    /// `PHYS_MASK` is applied.  This is necessary because applying
    /// `addr & 0x01FFFFFF` to `0xCC006008` yields `0x00006008`, which would
    /// silently alias into guest RAM instead of the DVD Interface registers.
    ///
    /// Currently handles:
    /// - **DVD Interface** (`0xCC006000–0xCC006027`): full register read
    /// - All other hardware registers: returns `0`
    pub fn hw_read_u32(&self, addr: u32) -> u32 {
        if addr >= DI_BASE && addr < DI_BASE + DI_SIZE {
            return self.di.read_reg(addr - DI_BASE);
        }
        0
    }

    /// Write a 32-bit value to a GameCube hardware register.
    ///
    /// Called by the JavaScript `write_u32` hook when the guest address has
    /// the prefix `0xCC`, before `PHYS_MASK` is applied (for the same reason
    /// as documented on [`hw_read_u32`]).
    ///
    /// Writing `DICR` (offset `0x1C`) with bit 0 set triggers an immediate
    /// disc DMA: the stored [`DiState`] command registers are decoded and the
    /// requested bytes are copied from `disc` into guest RAM.
    ///
    /// Currently handles:
    /// - **DVD Interface** (`0xCC006000–0xCC006027`): full register write + DMA
    /// - All other hardware registers: silently ignored
    pub fn hw_write_u32(&mut self, addr: u32, val: u32) {
        if addr >= DI_BASE && addr < DI_BASE + DI_SIZE {
            if self.di.write_reg(addr - DI_BASE, val) {
                self.process_di_command();
            }
        }
        // All other hardware addresses are ignored (reads return 0 via hw_read_u32).
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

    /// Guest PC of the most recently JIT-compiled block (0 if none compiled yet).
    pub fn last_compiled_pc(&self) -> u32 {
        self.last_compiled_pc
    }

    /// PPC instruction count of the most recently compiled block.
    pub fn last_compiled_ins_count(&self) -> u32 {
        self.last_compiled_ins_count
    }

    /// WASM byte length of the most recently compiled block.
    pub fn last_compiled_wasm_bytes(&self) -> u32 {
        self.last_compiled_wasm_bytes
    }

    /// Number of compiled blocks that contained at least one unimplemented opcode.
    pub fn unimplemented_block_count(&self) -> u32 {
        self.unimplemented_block_count as u32
    }

    /// Increment the `raise_exception` counter.
    ///
    /// Call this from the JavaScript `raise_exception` hook each time an
    /// exception is raised by a compiled block, so the Rust side can expose
    /// the total count to the stats panel.
    pub fn record_raise_exception(&mut self) {
        self.raise_exception_count += 1;
    }

    /// Total number of `raise_exception` calls since emulator creation.
    pub fn raise_exception_count(&self) -> u32 {
        self.raise_exception_count as u32
    }

    /// Current Link Register value.
    pub fn get_lr(&self) -> u32 {
        self.cpu.user.lr
    }

    /// Current Counter Register (CTR) value.
    pub fn get_ctr(&self) -> u32 {
        self.cpu.user.ctr
    }

    /// Current Machine State Register (MSR) as a raw 32-bit word.
    ///
    /// Bit 15 (`interrupts` / `EE`) is the external-interrupt enable flag.
    /// Check `(msr >> 15) & 1` to see if external interrupts are enabled.
    pub fn get_msr(&self) -> u32 {
        self.cpu.supervisor.config.msr.to_bits()
    }

    /// Saved Restore Register 0 (SRR0) — the PC saved when the last exception fired.
    pub fn get_srr0(&self) -> u32 {
        self.cpu.supervisor.exception.srr[0]
    }

    /// Saved Restore Register 1 (SRR1) — the MSR saved when the last exception fired.
    pub fn get_srr1(&self) -> u32 {
        self.cpu.supervisor.exception.srr[1]
    }

    /// Current Decrementer (DEC) value (signed; goes negative when it expires).
    pub fn get_dec(&self) -> u32 {
        self.cpu.supervisor.misc.dec
    }

    /// Return a [`js_sys::Array`] containing the guest PC of every block
    /// currently held in the compiled-block cache (module cache).
    ///
    /// Useful for debugging: shows which basic-block addresses have been JIT-
    /// compiled so far.
    pub fn get_compiled_block_pcs(&self) -> js_sys::Array {
        let arr = js_sys::Array::new();
        for &pc in self.cache.modules.keys() {
            arr.push(&JsValue::from(pc));
        }
        arr
    }

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
    /// If the transition occurs while `EE` is clear (interrupts disabled), the
    /// exception is held pending in `decrementer_pending`.  It will be delivered
    /// the next time this function is called with `EE` set, matching real
    /// PowerPC hardware where the decrementer interrupt is level-sensitive once
    /// the sign bit is set: enabling `EE` while `DEC < 0` triggers the exception
    /// immediately.
    ///
    /// Call this once per JIT block (not just once per animation frame) so that
    /// the exception fires as soon as the guest enables `EE` inside a spin-wait
    /// loop.
    pub fn advance_decrementer(&mut self, delta: u32) {
        let old_dec = self.cpu.supervisor.misc.dec;
        let new_dec = old_dec.wrapping_sub(delta);
        self.cpu.supervisor.misc.dec = new_dec;

        // Latch the pending flag whenever DEC transitions non-negative → negative.
        if (old_dec as i32) >= 0 && (new_dec as i32) < 0 {
            self.decrementer_pending = true;
        }

        // Deliver the exception as soon as external interrupts are enabled.
        // This correctly handles the case where DEC went negative while EE=0
        // (e.g. at emulator startup before the guest OS has initialised): the
        // exception is held until the guest runs `mtmsr` to enable EE, after
        // which the very next `advance_decrementer` call fires it.
        if self.decrementer_pending && self.cpu.supervisor.config.msr.interrupts() {
            self.decrementer_pending = false;
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
        self.last_compiled_pc = guest_pc;
        self.last_compiled_ins_count = block.instruction_count;
        self.last_compiled_wasm_bytes = block.bytes.len() as u32;
        if !block.unimplemented_ops.is_empty() {
            self.unimplemented_block_count += 1;
        }
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
        self.last_compiled_pc = guest_pc;
        self.last_compiled_ins_count = ins_count;
        self.last_compiled_wasm_bytes = byte_len;
        if !block.unimplemented_ops.is_empty() {
            self.unimplemented_block_count += 1;
        }

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
                    "read_f64",
                    "write_u8",
                    "write_u16",
                    "write_u32",
                    "write_f64",
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

    /// Process a DVD Interface DMA command (called when DICR bit 0 is written 1).
    ///
    /// Decodes the command in `DICMDBUF0` bits 31–24 and acts on it:
    ///
    /// - **`0xA8` — DVD Read**: copies `DILENGTH` bytes from the stored disc
    ///   image at byte offset `DICMDBUF1` into guest RAM at `DIMAR`.
    /// - **`0xAB` — Seek**: no-op (block device, seek has no physical effect).
    /// - **`0xE0` — Request Error**: zeroes `DIIMMBUF` and completes.
    /// - **`0xE3` — Stop Motor**: no-op.
    /// - Any other command: logged and completed without action.
    ///
    /// On completion (all paths), `DICR` bit 0 (TSTART) is cleared and
    /// `DISTATUS` bit 1 (TCINT — transfer complete) is set.  The CPU decrementer
    /// interrupt path is not currently modelled; games that spin on TCINT via
    /// polling (rather than an interrupt handler) will see the bit immediately.
    fn process_di_command(&mut self) {
        let cmd = (self.di.cmd_buf0 >> 24) as u8;
        let disc_offset = self.di.cmd_buf1 as usize; // byte offset in disc image
        let dma_len = self.di.dma_len as usize;
        let dma_dest = Self::phys_addr(self.di.dma_addr);

        match cmd {
            0xA8 => {
                // DVD Read: copy `dma_len` bytes from disc at `disc_offset` to RAM.
                // `self.disc` and `self.ram` are separate fields with disjoint heap
                // allocations; `as_deref()` borrows only the former, so Rust allows
                // a simultaneous mutable borrow of the latter.
                if let Some(disc) = self.disc.as_deref() {
                    let src_end = disc_offset.saturating_add(dma_len);
                    if src_end <= disc.len() && dma_dest + dma_len <= self.ram.len() {
                        self.ram[dma_dest..dma_dest + dma_len]
                            .copy_from_slice(&disc[disc_offset..src_end]);
                    } else {
                        console_log!(
                            "[lazuli] DI: DVD Read out of bounds \
                             (disc_off={:#010x}, len={}, disc_len={}, \
                             ram_dest={:#010x}, ram_len={})",
                            disc_offset,
                            dma_len,
                            disc.len(),
                            dma_dest,
                            self.ram.len()
                        );
                    }
                } else {
                    console_log!("[lazuli] DI: DVD Read with no disc loaded — call load_disc_image() first");
                }
            }
            0xAB => { /* Seek — no-op on a block device */ }
            0xE0 => {
                // Request Error — return 0 via DIIMMBUF
                self.di.imm_buf = 0;
            }
            0xE3 => { /* Stop Motor — no-op in emulation */ }
            other => {
                console_log!("[lazuli] DI: unrecognised command {:#04x}", other);
            }
        }

        // Mark transfer complete: clear TSTART, set TCINT.
        self.di.control &= !0x1; // clear TSTART
        self.di.status  |=  0x2; // set   TCINT
    }
}

// ─── Panic hook ───────────────────────────────────────────────────────────────

fn console_error_panic_hook_set() {
    // Forward Rust panics to the browser console for easier debugging.
    // Enabled when the `console_error_panic_hook` crate is available.
}
