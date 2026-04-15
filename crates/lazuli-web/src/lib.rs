//! Browser WebAssembly frontend for the Lazuli GameCube emulator.
//!
//! ## Architecture overview
//!
//! This crate bridges the [`ppcwasm`] JIT backend to the web browser and
//! provides the Rust-side foundations for full in-browser emulation,
//! mirroring the approach used by the [Play!] PS2 emulator:
//!
//! - **CPU:** PowerPC → WebAssembly dynarec via [`ppcwasm::WasmJit`]
//! - **GPU:** WebGPU via `wgpu` (Phase 4 — see `lazuli-web/Cargo.toml`)
//! - **Audio:** Web Audio API `AudioWorkletNode` driven by GameCube DSP output
//! - **IO:** GameCube controller via the browser Gamepad API
//!
//! ## Crate layout
//!
//! | Module    | Contents                                                     |
//! |-----------|--------------------------------------------------------------|
//! | `hw/pi`   | Processor Interface register constants                       |
//! | `hw/dsp`  | DSP Interface register state (HLE mailbox stub)              |
//! | `hw/di`   | DVD Interface register state + DMA command decoder           |
//! | `hw`      | `WasmEmulator` hardware I/O methods                         |
//! | `jit`     | `WasmBlockCache` + compile / fetch methods                   |
//! | `cpu`     | CPU register access, timebase, decrementer, serialisation    |
//! | `loader`  | ISO / raw-bytes ROM loading                                  |
//!
//! [`WasmEmulator`] is the main struct exported to JavaScript via
//! [`wasm_bindgen`].
//!
//! [Play!]: https://github.com/jpd002/Play-

mod cpu;
mod hw;
mod jit;
mod loader;

use ppcwasm::WasmJit;
use wasm_bindgen::prelude::*;

use hw::{AiState, DiState, DspState, ExiState, GxState, SiState, ViState};
use jit::WasmBlockCache;

// Re-export the block type for JavaScript inspection.
pub use ppcwasm::WasmBlock;

// ─── WebGPU support detection ─────────────────────────────────────────────────

/// Returns `true` if the browser exposes `navigator.gpu` (WebGPU is available).
///
/// WebGPU is the GPU API used by `wgpu` on `wasm32` targets (enabled via the
/// `webgpu` feature).  Call this from JavaScript before attempting to
/// initialise the GPU renderer; fall back to the canvas-based XFB blitter if
/// it returns `false`.
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
/// ```
#[wasm_bindgen]
pub fn wasm_memory() -> JsValue {
    wasm_bindgen::memory()
}

// ─── WasmEmulator ─────────────────────────────────────────────────────────────

/// GameCube emulator running entirely in the browser via WebAssembly.
///
/// Exported to JavaScript via `wasm-bindgen`.  The emulator maintains a
/// [`gekko::Cpu`] register file and a flat RAM array.  Compiled PPC blocks are
/// cached as [`WebAssembly::Module`]s and instantiated on demand with
/// JavaScript hook closures for guest memory access.
#[wasm_bindgen]
pub struct WasmEmulator {
    pub(crate) cpu: gekko::Cpu,
    pub(crate) ram: Vec<u8>,
    pub(crate) jit: WasmJit,
    pub(crate) cache: WasmBlockCache,
    /// Number of blocks compiled since emulator creation.
    pub(crate) blocks_compiled: u64,
    /// Number of blocks executed since emulator creation.
    pub(crate) blocks_executed: u64,
    /// GameCube controller button bitmask (set by JavaScript keyboard handler).
    pub(crate) pad_buttons: u32,
    /// Guest PC of the most recently compiled block (0 if none yet).
    pub(crate) last_compiled_pc: u32,
    /// PPC instruction count of the most recently compiled block.
    pub(crate) last_compiled_ins_count: u32,
    /// WASM byte length of the most recently compiled block.
    pub(crate) last_compiled_wasm_bytes: u32,
    /// Number of blocks that contained at least one unimplemented opcode.
    pub(crate) unimplemented_block_count: u64,
    /// Number of `raise_exception` calls forwarded from the JS hook.
    pub(crate) raise_exception_count: u64,
    /// Raw ISO disc image bytes for the emulated DVD controller.  `None` until
    /// [`loader::load_disc_image`] is called.
    pub(crate) disc: Option<Vec<u8>>,
    /// DVD Interface (DI) hardware register state.
    pub(crate) di: DiState,
    /// DSP Interface hardware register state (mailbox echo HLE stub).
    pub(crate) dsp: DspState,
    /// Video Interface (VI) hardware register state.
    pub(crate) vi: ViState,
    /// Serial Interface (SI) hardware register state + controller auto-response.
    pub(crate) si: SiState,
    /// External Interface (EXI) hardware register state.
    pub(crate) exi: ExiState,
    /// Audio Interface (AI) hardware register state.
    pub(crate) ai: AiState,
    /// GX Command Processor (CP) and Pixel Engine (PE) register state.
    pub(crate) gx: GxState,
    /// 16 MiB Audio RAM (ARAM) buffer.
    ///
    /// Matches the native `dspi::Dsp::aram` field.  ARAM DMA transfers between
    /// this buffer and main [`WasmEmulator::ram`] are executed in `hw_write_u32`
    /// (which has access to both) rather than inside `DspState::write_u32`.
    pub(crate) aram: Vec<u8>,
    /// Processor Interface interrupt status register.
    pub(crate) pi_intsr: u32,
    /// Processor Interface interrupt mask register (PI_INTMSK at 0xCC003004).
    pub(crate) pi_intmsk: u32,
    /// PI FIFO start address (PI_FIFO_BASE at PI+0x0C, `0xCC00300C`).
    pub(crate) pi_fifo_base: u32,
    /// PI FIFO end address (PI_FIFO_END at PI+0x10, `0xCC003010`).
    pub(crate) pi_fifo_end: u32,
    /// PI FIFO write pointer (PI_FIFO_WPTR at PI+0x14, `0xCC003014`).
    pub(crate) pi_fifo_wptr: u32,
    /// True when the decrementer has transitioned negative but the exception
    /// has not yet been delivered because MSR.EE was clear at the time.
    pub(crate) decrementer_pending: bool,
    /// Set to `true` by `process_di_command` whenever a DVD Read (0xA8)
    /// successfully copies disc bytes into guest RAM.
    pub(crate) dma_dirty: bool,
    /// Estimated CPU cycle count of the most recently compiled block.
    /// Mirrors [`ppcwasm::WasmBlock::cycles`] and is used by JavaScript to
    /// drive cycle-accurate decrementer advancement (matching how ppcjit uses
    /// `Block::meta().cycles` to feed the native scheduler).
    pub(crate) last_compiled_cycles: u32,
    /// Running total of emulated CPU cycles since emulator creation.
    /// Advanced by `add_cpu_cycles()` after each block; JavaScript maintains
    /// this to implement the same cycle-counting that `Lazuli::exec` performs
    /// internally via its `Scheduler`.
    pub(crate) cpu_cycles: u64,
    /// Accumulated CPU cycles for the AI sample-counter tick.
    ///
    /// The AI sample counter increments at the audio sample rate (48 kHz or
    /// 32 kHz depending on `AICR.AISFR`).  Since one audio sample corresponds
    /// to many CPU cycles (10 125 at 48 kHz), this accumulator avoids losing
    /// fractional samples across consecutive JIT blocks.
    pub(crate) ai_cpu_cycles: u64,
    /// Physical start address of the most recent successful DVD DMA transfer.
    /// JavaScript reads this (alongside `last_dma_len`) to perform selective
    /// per-address JIT cache invalidation instead of a full `moduleCache.clear()`.
    pub(crate) last_dma_addr: u32,
    /// Byte length of the most recent successful DVD DMA transfer.
    pub(crate) last_dma_len: u32,
    /// Byte offset into the disc image of the most recent successful DVD Read
    /// (0xA8) command.  JavaScript reads this after `take_dma_dirty()` to
    /// format the `[lazuli] DI: DVD Read` log entry in the apploader-log panel.
    pub(crate) last_di_disc_offset: u32,
    /// 16 KiB Gekko L2 cache-as-RAM region.
    ///
    /// GameCube software can lock the L2 cache and use it as fast scratch RAM
    /// mapped at `0xE000_0000`–`0xE003_FFFF`.  Many games store physics
    /// scratch buffers, temporary vertex data, and thread stacks here.
    ///
    /// Matches the native emulator's 16 KiB L2 allocation.  JavaScript routes
    /// guest reads/writes to `0xE000_xxxx` to/from this buffer via the
    /// `l2c_ptr()` / `l2c_size()` exports.
    pub(crate) l2c: Vec<u8>,
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
            dsp: DspState::default(),
            vi: ViState::default(),
            si: SiState::default(),
            exi: ExiState::default(),
            ai: AiState::default(),
            gx: GxState::default(),
            aram: vec![0u8; 16 * 1024 * 1024],
            pi_intsr: 0,
            pi_intmsk: 0,
            pi_fifo_base: 0,
            pi_fifo_end: 0,
            pi_fifo_wptr: 0,
            decrementer_pending: false,
            dma_dirty: false,
            last_compiled_cycles: 0,
            cpu_cycles: 0,
            ai_cpu_cycles: 0,
            last_dma_addr: 0,
            last_dma_len: 0,
            last_di_disc_offset: 0,
            l2c: vec![0u8; 16 * 1024],
        }
    }
}

// ─── Shared utilities ─────────────────────────────────────────────────────────

/// Translate a guest virtual address to a physical RAM offset.
///
/// Strips the GameCube KSEG0 (`0x80000000`) and KSEG1 (`0xA0000000`)
/// segment bits.  The mask `0x01FF_FFFF` (25 bits = 32 MiB) is
/// intentionally one bit wider than the 24 MiB of main RAM so that the
/// upper MiB of the address space also maps cleanly to physical offsets
/// without wrapping.  Addresses already below 0x02000000 pass through
/// unchanged.
pub(crate) fn phys_addr(vaddr: u32) -> usize {
    (vaddr & 0x01FF_FFFF) as usize
}

// ─── Panic hook ───────────────────────────────────────────────────────────────

fn console_error_panic_hook_set() {
    // Forward Rust panics to the browser console via `console.error(...)` so
    // that panic messages are visible in DevTools instead of silently aborting.
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("[lazuli] Rust panic: {info}");
        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&msg));
    }));
}
