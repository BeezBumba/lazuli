//! Hardware register I/O — bridges JavaScript hook calls to per-subsystem
//! register state structs.
//!
//! This module exposes `hw_read_u32`, `hw_write_u32`, `hw_read_u16`,
//! `hw_write_u16`, `hw_read_u8`, `hw_write_u8`, and `assert_vi_interrupt`
//! on [`WasmEmulator`].  These are called from JavaScript when the guest
//! accesses an MMIO address (prefix `0xCC` cached or `0xCD` uncached),
//! before the `PHYS_MASK` is applied.
//!
//! ## Dispatch table
//!
//! | Base address  | Module | Description                         |
//! |---------------|--------|-------------------------------------|
//! | `0xCC000000`  | `gx`   | GX Command Processor + Pixel Engine |
//! | `0xCC002000`  | `vi`   | Video Interface                     |
//! | `0xCC003000`  | `pi`   | Processor Interface                 |
//! | `0xCC005000`  | `dsp`  | DSP Interface / ARAM                |
//! | `0xCC006000`  | `di`   | DVD Interface                       |
//! | `0xCC006400`  | `si`   | Serial Interface (controllers)      |
//! | `0xCC006800`  | `exi`  | External Interface                  |
//! | `0xCC006C00`  | `ai`   | Audio Interface                     |

pub(crate) mod ai;
pub(crate) mod di;
pub(crate) mod dsp;
pub(crate) mod exi;
pub(crate) mod gx;
pub(crate) mod gx_fifo;
pub(crate) mod mi;
pub(crate) mod pi;
pub(crate) mod si;
pub(crate) mod vi;

pub(crate) use ai::{AiState, AI_BASE, AI_SIZE};
pub(crate) use di::{DiState, DI_BASE, DI_SIZE};
pub(crate) use dsp::{DspState, DSP_BASE, DSP_SIZE};
pub(crate) use exi::{ExiState, EXI_BASE, EXI_SIZE};
pub(crate) use gx::{GxState, GX_BASE, GX_SIZE};
pub(crate) use mi::{MiState, MI_BASE, MI_SIZE};
pub(crate) use pi::{
    PI_BASE, PI_BUSCLK_VAL, PI_CPUCLK_VAL, PI_INT_AI, PI_INT_DI, PI_INT_DSP, PI_INT_PE_FINISH,
    PI_INT_PE_TOKEN, PI_INT_SI as PI_SI, PI_INT_VI, PI_MEMSIZE_VAL, PI_SIZE,
};
pub(crate) use si::{SiState, SI_BASE, SI_SIZE};
pub(crate) use vi::{ViState, VI_BASE, VI_SIZE};

use wasm_bindgen::prelude::*;

use crate::WasmEmulator;

/// Bit that distinguishes the uncached (`0xCDxxxxxx`) MMIO mirror from the
/// cached (`0xCCxxxxxx`) mirror.  Both aliases map to the same physical
/// registers; clearing this bit normalises uncached addresses to the cached
/// base so a single set of `*_BASE` constants covers both variants.
const UNCACHED_MIRROR_BIT: u32 = 0x0100_0000;

macro_rules! console_log {
    ($($t:tt)*) => {
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!($($t)*)))
    };
}

#[wasm_bindgen]
impl WasmEmulator {
    /// Read a 32-bit value from a GameCube hardware register.
    ///
    /// Called by the JavaScript `read_u32` hook when the guest address has
    /// the prefix `0xCC` or `0xCD` (GameCube memory-mapped I/O space),
    /// **before** the `PHYS_MASK` is applied.  This is necessary because
    /// applying `addr & 0x01FFFFFF` to `0xCC006008` yields `0x00006008`,
    /// which would silently alias into guest RAM instead of the DVD Interface
    /// registers.
    ///
    /// Both `0xCCxxxxxx` (cached) and `0xCDxxxxxx` (uncached) aliases are
    /// normalised to the `0xCC` base before dispatching.
    pub fn hw_read_u32(&self, addr: u32) -> u32 {
        // Normalise uncached (0xCDxxxxxx) addresses to the cached (0xCCxxxxxx)
        // mirror so a single set of base-address constants covers both aliases.
        let addr = addr & !UNCACHED_MIRROR_BIT;

        // ── GX: Command Processor + Pixel Engine (16-bit registers) ───────
        // GX registers are 16-bit wide; reconstruct a 32-bit value from two
        // consecutive halfwords (big-endian: high halfword at the lower address).
        if addr >= GX_BASE && addr < GX_BASE + GX_SIZE {
            let offset = addr - GX_BASE;
            let hi = self.gx.read_u16(offset) as u32;
            let lo = self.gx.read_u16(offset + 2) as u32;
            return (hi << 16) | lo;
        }

        // ── Video Interface ────────────────────────────────────────────────
        if addr >= VI_BASE && addr < VI_BASE + VI_SIZE {
            return self.vi.read_u32(addr - VI_BASE);
        }

        // ── Processor Interface ────────────────────────────────────────────
        if addr >= PI_BASE && addr < PI_BASE + PI_SIZE {
            let offset = addr - PI_BASE;
            return match offset {
                0x00 => self.pi_intsr,
                0x04 => self.pi_intmsk,
                // PI_FIFO_BASE / PI_FIFO_END / PI_FIFO_WPTR — readable state.
                0x0C => self.pi_fifo_base,
                0x10 => self.pi_fifo_end,
                0x14 => self.pi_fifo_wptr,
                0x28 => PI_MEMSIZE_VAL,
                0x2C => PI_BUSCLK_VAL,
                0x30 => PI_CPUCLK_VAL,
                _ => 0,
            };
        }

        // ── DVD Interface ──────────────────────────────────────────────────
        if addr >= DI_BASE && addr < DI_BASE + DI_SIZE {
            return self.di.read_reg(addr - DI_BASE);
        }

        // ── Memory Interface ───────────────────────────────────────────────
        if addr >= MI_BASE && addr < MI_BASE + MI_SIZE {
            let offset = addr - MI_BASE;
            let hi = self.mi.read_u16(offset) as u32;
            let lo = self.mi.read_u16(offset + 2) as u32;
            return (hi << 16) | lo;
        }

        // ── Serial Interface ───────────────────────────────────────────────
        if addr >= SI_BASE && addr < SI_BASE + SI_SIZE {
            return self.si.read_u32(addr - SI_BASE);
        }

        // ── External Interface ─────────────────────────────────────────────
        if addr >= EXI_BASE && addr < EXI_BASE + EXI_SIZE {
            return self.exi.read_u32(addr - EXI_BASE);
        }

        // ── DSP Interface (32-bit reads) ───────────────────────────────────
        if addr >= DSP_BASE && addr < DSP_BASE + DSP_SIZE {
            let offset = addr - DSP_BASE;
            // Reconstruct a 32-bit value from two 16-bit register halves
            // (big-endian: high halfword at the lower address).
            let hi = self.dsp.read_u16(offset) as u32;
            let lo = self.dsp.read_u16(offset + 2) as u32;
            return (hi << 16) | lo;
        }

        // ── Audio Interface ────────────────────────────────────────────────
        if addr >= AI_BASE && addr < AI_BASE + AI_SIZE {
            return self.ai.read_u32(addr - AI_BASE);
        }

        0
    }

    /// Write a 32-bit value to a GameCube hardware register.
    ///
    /// Called by the JavaScript `write_u32` hook when the guest address has
    /// the prefix `0xCC` or `0xCD`, before `PHYS_MASK` is applied.
    pub fn hw_write_u32(&mut self, addr: u32, val: u32) {
        let addr = addr & !UNCACHED_MIRROR_BIT;

        // ── PI Write-Gather Port (GX FIFO) — 0xCC008000–0xCC00801F ──────────
        // The CPU pushes GX commands here via STW/STWBRX.  Each 32-byte burst
        // is forwarded to the GX FIFO parser which:
        //   • tracks the CP register file (VCD / VAT) for vertex-size computation;
        //   • fires PE_FINISH when a LoadBP PixelDone (0x45) command is seen,
        //     replacing the coarse VI-rate PE_FINISH stub.
        // Mirrors native `pi::fifo_push` → CP FIFO buffer → `cmd::consume/process`.
        if addr >= 0xCC00_8000 && addr < 0xCC00_8020 {
            self.gx.fifo.push_u32(val);
            // Check if the FIFO parser fired any PE interrupt.
            if core::mem::replace(&mut self.gx.fifo.pe_finish_pending, false) {
                if self.gx.fire_pe_finish() {
                    self.pi_intsr |= PI_INT_PE_FINISH;
                    self.maybe_deliver_external_interrupt();
                }
            }
            if core::mem::replace(&mut self.gx.fifo.pe_token_pending, false) {
                let token = self.gx.fifo.pe_token;
                if self.gx.fire_pe_token(token) {
                    self.pi_intsr |= PI_INT_PE_TOKEN;
                    self.maybe_deliver_external_interrupt();
                }
            }
            return;
        }

        // ── GX: Command Processor + Pixel Engine ──────────────────────────
        // GX registers are 16-bit wide; split the 32-bit write into two
        // consecutive halfword writes (high halfword at the lower address).
        if addr >= GX_BASE && addr < GX_BASE + GX_SIZE {
            let offset = addr - GX_BASE;
            let changed_hi = self.gx.write_u16(offset, (val >> 16) as u16);
            let changed_lo = self.gx.write_u16(offset + 2, val as u16);
            if changed_hi || changed_lo {
                self.sync_pi_pe_interrupts();
                self.maybe_deliver_external_interrupt();
            }
            return;
        }

        // ── Video Interface ────────────────────────────────────────────────
        if addr >= VI_BASE && addr < VI_BASE + VI_SIZE {
            self.vi.write_u32(addr - VI_BASE, val);
            // DI0–DI3 (VI DisplayInterrupt registers, offsets 0x30–0x3C):
            // When the OS clears the interrupt status bit (bit 31) in a VI DI
            // register, sync the PI_INTSR VI bit with the actual hardware state.
            // This mirrors the native `get_active_interrupts` path which computes
            // the VI interrupt dynamically from the VI source registers rather
            // than latching it in a separate PI_INTSR VI bit.
            let vi_offset = addr - VI_BASE;
            if (0x30..=0x3C).contains(&(vi_offset & !3)) {
                if self.vi.any_display_interrupt_active() {
                    self.pi_intsr |= PI_INT_VI;
                } else {
                    self.pi_intsr &= !PI_INT_VI;
                }
            }
            return;
        }

        // ── Processor Interface ────────────────────────────────────────────
        if addr >= PI_BASE && addr < PI_BASE + PI_SIZE {
            let offset = addr - PI_BASE;
            match offset {
                // PI_INTSR: write-1-to-clear — the OS writes back the bits it
                // has handled to acknowledge the interrupt.
                0x00 => self.pi_intsr &= !val,
                // PI_INTMSK: interrupt enable mask (normal read/write).
                // Re-check pending interrupts immediately after the mask changes — the OS
                // may have just unmasked a source that was already pending in PI_INTSR,
                // mirroring the native bus.rs `schedule_now(pi::check_interrupts)` path.
                0x04 => {
                    self.pi_intmsk = val;
                    self.maybe_deliver_external_interrupt();
                }
                // PI_FIFO_BASE / PI_FIFO_END / PI_FIFO_WPTR: writable state
                // mirroring the CP FIFO shadow registers.
                0x0C => self.pi_fifo_base = val,
                0x10 => self.pi_fifo_end = val,
                0x14 => self.pi_fifo_wptr = val,
                _ => {}
            }
            return;
        }

        // ── DSP Interface (32-bit writes) ──────────────────────────────────
        // Handles ARAM DMA base/control (0x5020/0x5024/0x5028) and AudioDmaBase
        // (0x5030) which the OS writes with `stw`.
        //
        // DspState::write_u32 stores the DMA parameters and marks
        // `aram_dma_pending = true` when the control register (0x28) is written.
        // The actual byte copy is executed here where both `self.ram` and
        // `self.aram` are accessible.
        if addr >= DSP_BASE && addr < DSP_BASE + DSP_SIZE {
            self.dsp.write_u32(addr - DSP_BASE, val);
            // Execute pending ARAM DMA (triggered by write to DspAramDmaControl).
            if core::mem::replace(&mut self.dsp.aram_dma_pending, false) {
                self.execute_aram_dma();
            }
            // Sync PI_INT_DSP: set the bit if any DSP interrupt source is both
            // pending and enabled; clear it otherwise.  Mirrors the native
            // `get_active_interrupts` → `sources.set_dsp_interface(control.any_interrupt())`
            // path in pi.rs.
            self.sync_pi_dsp();
            self.maybe_deliver_external_interrupt();
            return;
        }

        // ── DVD Interface ──────────────────────────────────────────────────
        if addr >= DI_BASE && addr < DI_BASE + DI_SIZE {
            if self.di.write_reg(addr - DI_BASE, val) {
                self.process_di_command();
                // Assert the DI interrupt (PI_INT_DI = bit 2 = 0x0000_0004).
                // The OS handler will clear this bit by writing 1 to PI_INTSR.
                self.pi_intsr |= PI_INT_DI;
                self.maybe_deliver_external_interrupt();
            }
            return;
        }

        // ── Memory Interface ───────────────────────────────────────────────
        if addr >= MI_BASE && addr < MI_BASE + MI_SIZE {
            let offset = addr - MI_BASE;
            self.mi.write_u16(offset, (val >> 16) as u16);
            self.mi.write_u16(offset + 2, val as u16);
            return;
        }

        // ── Serial Interface ───────────────────────────────────────────────
        if addr >= SI_BASE && addr < SI_BASE + SI_SIZE {
            let triggered = self.si.write_u32(addr - SI_BASE, val, self.pad_buttons);
            if triggered {
                // Assert the SI interrupt (PI_INT_SI = bit 3 = 0x0008).
                self.pi_intsr |= PI_SI;
                self.maybe_deliver_external_interrupt();
            }
            return;
        }

        // ── External Interface ─────────────────────────────────────────────
        if addr >= EXI_BASE && addr < EXI_BASE + EXI_SIZE {
            if let Some(dma) = self.exi.write_u32(addr - EXI_BASE, val) {
                // DMA-mode SRAM read: copy SRAM bytes → guest RAM.
                let sram = &self.exi.sram;
                let src_end = (dma.sram_offset + dma.length as usize).min(sram.len());
                let src_len = src_end.saturating_sub(dma.sram_offset);
                let dst_addr = crate::phys_addr(dma.ram_addr);
                let dst_end  = dst_addr + src_len;
                if dst_end <= self.ram.len() {
                    self.ram[dst_addr..dst_end]
                        .copy_from_slice(&sram[dma.sram_offset..src_end]);
                }
            }
            return;
        }

        // ── Audio Interface ────────────────────────────────────────────────
        if addr >= AI_BASE && addr < AI_BASE + AI_SIZE {
            self.ai.write_u32(addr - AI_BASE, val);
            return;
        }
    }

    /// Read a 16-bit value from a GameCube hardware register.
    ///
    /// Called by the JavaScript `read_u16` hook when the guest address has
    /// the prefix `0xCC` or `0xCD`.
    pub fn hw_read_u16(&self, addr: u32) -> u16 {
        let addr = addr & !UNCACHED_MIRROR_BIT;

        // ── GX: Command Processor + Pixel Engine ──────────────────────────
        if addr >= GX_BASE && addr < GX_BASE + GX_SIZE {
            return self.gx.read_u16(addr - GX_BASE);
        }

        // ── Video Interface (16-bit registers VTR, DCR, VCOUNT, HCOUNT, HSR, VICLK) ──
        if addr >= VI_BASE && addr < VI_BASE + VI_SIZE {
            return self.vi.read_u16(addr - VI_BASE);
        }

        // ── DSP Interface ──────────────────────────────────────────────────
        if addr >= DSP_BASE && addr < DSP_BASE + DSP_SIZE {
            return self.dsp.read_u16(addr - DSP_BASE);
        }

        // ── DVD Interface — 16-bit access to 32-bit DI register ───────────
        if addr >= DI_BASE && addr < DI_BASE + DI_SIZE {
            let offset = addr - DI_BASE;
            let word = self.di.read_reg(offset & !3);
            return if (offset & 2) == 0 {
                (word >> 16) as u16
            } else {
                word as u16
            };
        }

        // ── Memory Interface ───────────────────────────────────────────────
        if addr >= MI_BASE && addr < MI_BASE + MI_SIZE {
            return self.mi.read_u16(addr - MI_BASE);
        }

        // ── Serial Interface — 16-bit access to 32-bit SI register ────────
        if addr >= SI_BASE && addr < SI_BASE + SI_SIZE {
            let word = self.si.read_u32(addr - SI_BASE & !3);
            let offset = addr - SI_BASE;
            return if (offset & 2) == 0 { (word >> 16) as u16 } else { word as u16 };
        }

        0
    }

    /// Write a 16-bit value to a GameCube hardware register.
    ///
    /// Called by the JavaScript `write_u16` hook when the guest address has
    /// the prefix `0xCC` or `0xCD`.
    pub fn hw_write_u16(&mut self, addr: u32, val: u16) {
        let addr = addr & !UNCACHED_MIRROR_BIT;

        // ── GX: Command Processor + Pixel Engine ──────────────────────────
        if addr >= GX_BASE && addr < GX_BASE + GX_SIZE {
            let changed = self.gx.write_u16(addr - GX_BASE, val);
            if changed {
                self.sync_pi_pe_interrupts();
                self.maybe_deliver_external_interrupt();
            }
            return;
        }

        // ── Video Interface ────────────────────────────────────────────────
        if addr >= VI_BASE && addr < VI_BASE + VI_SIZE {
            self.vi.write_u16(addr - VI_BASE, val);
            // Sync PI_INTSR VI bit after DI register half-word writes, mirroring
            // the full-word path so that the rare sth case is handled correctly.
            let vi_offset = addr - VI_BASE;
            if (0x30..=0x3C).contains(&(vi_offset & !3)) {
                if self.vi.any_display_interrupt_active() {
                    self.pi_intsr |= PI_INT_VI;
                } else {
                    self.pi_intsr &= !PI_INT_VI;
                }
            }
            return;
        }

        // ── DSP Interface ──────────────────────────────────────────────────
        if addr >= DSP_BASE && addr < DSP_BASE + DSP_SIZE {
            self.dsp.write_u16(addr - DSP_BASE, val);
            // Sync PI_INT_DSP after any DSPCONTROL change (enable-mask bits may
            // have changed, or interrupt pending bits may have been W1C'd).
            self.sync_pi_dsp();
            self.maybe_deliver_external_interrupt();
            return;
        }

        // ── Memory Interface ───────────────────────────────────────────────
        if addr >= MI_BASE && addr < MI_BASE + MI_SIZE {
            self.mi.write_u16(addr - MI_BASE, val);
        }
    }

    /// Read an 8-bit value from a GameCube hardware register.
    ///
    /// Most MMIO registers are 16- or 32-bit wide; 8-bit reads are unusual
    /// but the OS sometimes uses `lbz` to check single-byte status fields.
    /// Returns the appropriate byte from the containing 32-bit register.
    pub fn hw_read_u8(&self, addr: u32) -> u8 {
        let addr_n = addr & !UNCACHED_MIRROR_BIT;
        // Align to the containing 32-bit word, preserving the high-address bits
        // needed for MMIO dispatch (0xCCxxxxxx or 0xCDxxxxxx prefix).
        let aligned = (addr_n & !3) | (addr & 0xFF00_0000);
        let word = self.hw_read_u32(aligned);
        // Extract the correct byte in big-endian order (byte 0 = MSB of word).
        let byte_lane = addr_n & 3;
        (word >> (24 - byte_lane * 8)) as u8
    }

    /// Write an 8-bit value to a GameCube hardware register.
    ///
    /// Reads the containing 32-bit register, merges the byte, and writes back.
    pub fn hw_write_u8(&mut self, addr: u32, val: u8) {
        let addr_n = addr & !UNCACHED_MIRROR_BIT;
        let aligned = (addr_n & !3) | (addr & 0xFF00_0000);
        let byte_lane = addr_n & 3;
        let shift = 24 - byte_lane * 8;
        let mask = 0xFFu32 << shift;
        let current = self.hw_read_u32(aligned);
        let merged = (current & !mask) | ((val as u32) << shift);
        self.hw_write_u32(aligned, merged);
    }

    /// Assert a Video Interface (VI) vertical-retrace interrupt.
    ///
    /// Call this **once per animation frame** (~60 Hz) from the JavaScript
    /// game loop.  The function advances the vertical counter, fires any
    /// enabled VI DisplayInterrupt sources (setting their status bits so the
    /// OS handler can identify which source fired), sets the VI bit in
    /// `PI_INTSR`, and — if the guest CPU has external interrupts enabled
    /// (`MSR.EE = 1`) and the OS has unmasked the VI interrupt in `PI_INTMSK`
    /// — delivers an `Exception::Interrupt` (vector `0x00000500`) to the CPU.
    ///
    /// Additionally fires the PE_FINISH interrupt once per frame.  On real
    /// hardware PE_FINISH is generated by the GPU when it finishes processing
    /// a draw-done command from the GX FIFO.  Without a full GX pipeline the
    /// browser build fires it alongside the VI retrace to unblock games that
    /// call `GXWaitForDrawDone()` as a frame-sync primitive.
    pub fn assert_vi_interrupt(&mut self) {
        // Advance the VI vertical counter by one field (called each RAF frame).
        self.vi.advance_vcount();

        // Assert the status bit (bit 31) in every VI DisplayInterrupt register
        // that has its enable bit (bit 28) set.  On real hardware the VI
        // hardware sets these status bits when VCOUNT reaches the configured
        // scan line.  The OS interrupt handler reads them to dispatch to the
        // per-source retrace callback and increment VRetraceCnt — which is what
        // VIWaitForRetrace() polls.  Without this, the handler sees no active
        // DI source, never calls the callback, and VIWaitForRetrace() spins
        // forever with EE=1, producing an endless 0x00000500 interrupt loop.
        self.vi.fire_display_interrupts();

        // Sync PI_INTSR VI bit with the actual DI register state.
        // Native `get_active_interrupts` only sets the VI bit when at least
        // one VI DisplayInterrupt source has both its enable (bit 28) and
        // status (bit 31) bits set.  If no source is active the bit is
        // cleared, matching the native write to pi_intsr from pi.rs.
        if self.vi.any_display_interrupt_active() {
            self.pi_intsr |= PI_INT_VI;
        } else {
            self.pi_intsr &= !PI_INT_VI;
        }

        // Fire PE_FINISH once per frame as a fallback for games whose draw-done
        // commands were not captured by the GX FIFO parser (e.g. games that use
        // a linked FIFO mode or direct ARAM writes).  For games that properly
        // push a LoadBP PixelDone (0x45) command through the PI Write-Gather
        // Port (0xCC008000), the FIFO parser in `hw_write_u32` already fires
        // PE_FINISH at the precise moment the command is processed, which is
        // more accurate than this VI-rate approximation.
        if self.gx.fire_pe_finish() {
            self.pi_intsr |= PI_INT_PE_FINISH;
        }
        // Sync any other PE interrupt changes resulting from the frame tick.
        self.sync_pi_pe_interrupts();

        // Deliver the external interrupt immediately if EE=1 and the OS has
        // unmasked any pending interrupt in PI_INTMSK.  Do not pre-clear
        // PI_INTSR: the ISR must be able to read the bit to know which
        // interrupt fired.
        self.maybe_deliver_external_interrupt();
    }

    /// Advance the Audio Interface (AI) sample counter by `cpu_cycles`.
    ///
    /// Call this from the JavaScript game loop after every executed block,
    /// passing the block's CPU cycle count (`blockMeta.cycles`).  Internally
    /// the method accumulates cycles across blocks and increments the AI sample
    /// counter (`AISCNT`) once per audio sample period (10 125 CPU cycles at
    /// 48 kHz; 15 187 at 32 kHz, selected by `AICR.AISFR`).
    ///
    /// When `AISCNT` crosses `AIIT` (the interrupt threshold), the AI
    /// interrupt fires: `PI_INT_AI` is set in `PI_INTSR` and
    /// [`maybe_deliver_external_interrupt`] is called immediately.
    ///
    /// Returns `true` when the AI interrupt fires (informational only —
    /// interrupt delivery is handled automatically).
    ///
    /// Mirrors the native `ai::push_streaming_frame` scheduler event which
    /// increments `sample_counter` and calls `pi::check_interrupts` at the
    /// audio sample rate.
    pub fn advance_ai(&mut self, cpu_cycles: u32) -> bool {
        self.ai_cpu_cycles += cpu_cycles as u64;
        // Determine the sample rate from AICR bit 1 (AISFR):
        // 0 = 48 kHz (486_000_000 / 48_000 = 10_125 cycles/sample)
        // 1 = 32 kHz (486_000_000 / 32_000 ≈ 15_187 cycles/sample)
        let sample_rate_32k = (self.ai.control & (1 << 1)) != 0;
        let cycles_per_sample: u64 = if sample_rate_32k { 15_187 } else { 10_125 };

        let samples = self.ai_cpu_cycles / cycles_per_sample;
        if samples == 0 {
            return false;
        }
        self.ai_cpu_cycles -= samples * cycles_per_sample;

        if self.ai.tick_sample_counter(samples as u32) {
            self.pi_intsr |= PI_INT_AI;
            self.maybe_deliver_external_interrupt();
            return true;
        }
        false
    }

    /// Physical RAM address of the External Frame Buffer as programmed by the
    /// game into the VI TFBL register.
    ///
    /// Returns `0` if the game has not yet configured the VI (e.g. during
    /// early boot).  JavaScript falls back to heuristic XFB detection when
    /// this returns `0`.
    pub fn vi_xfb_addr(&self) -> u32 {
        self.vi.xfb_addr()
    }

    /// Return and clear any EXI UART bytes emitted by the game since the last
    /// call.
    ///
    /// The GameCube OS (`OSReport` → `__OSConsoleWrite`) writes console output
    /// via the EXI UART protocol on channel 0 (command `0xA001_0000` then
    /// immediate-mode data writes), not via the virtual `0xCC007000` byte port
    /// used by ipl-hle.  JavaScript should call this after each emulated block
    /// and pipe the returned bytes through the same `stdoutLineBuffer` →
    /// `appendApploaderLog` path used for ipl-hle output.
    pub fn take_uart_output(&mut self) -> Vec<u8> {
        self.exi.take_uart_output()
    }

    /// Return `true` if a DVD DMA has written new data into guest RAM since the
    /// last call, and reset the flag to `false`.
    pub fn take_dma_dirty(&mut self) -> bool {
        let dirty = self.dma_dirty;
        self.dma_dirty = false;
        dirty
    }

    /// Physical start address (in emulator RAM) of the most recent successful
    /// DVD DMA transfer.
    pub fn last_dma_addr(&self) -> u32 {
        self.last_dma_addr
    }

    /// Byte length of the most recent successful DVD DMA transfer.
    pub fn last_dma_len(&self) -> u32 {
        self.last_dma_len
    }

    /// Disc byte offset of the most recent successful DVD Read (0xA8) DMA.
    ///
    /// JavaScript reads this after [`take_dma_dirty`] returns `true` to
    /// format the `[lazuli] DI: DVD Read` diagnostic line in the apploader-log
    /// panel, mirroring the same message logged to the browser console by
    /// `process_di_command`.
    pub fn last_di_disc_offset(&self) -> u32 {
        self.last_di_disc_offset
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

#[wasm_bindgen]
impl WasmEmulator {
    /// Deliver an external interrupt to the CPU if EE=1 and any enabled
    /// interrupt is pending in `PI_INTSR & PI_INTMSK`.
    ///
    /// Call this from JavaScript whenever MSR.EE transitions from 0 to 1
    /// (e.g. after `rfi` or `mtmsr` restores EE).  This mirrors the native
    /// JIT's `msr_changed` → `schedule_now(pi::check_interrupts)` hook, which
    /// only re-checks pending interrupts on an actual MSR-change event — not
    /// on every single JIT block.
    ///
    /// Also called internally whenever a PI_INTSR bit is asserted (VI, DI, SI,
    /// …) so the interrupt fires immediately if EE is already enabled.
    pub fn maybe_deliver_external_interrupt(&mut self) {
        let pending_and_enabled = self.pi_intsr & self.pi_intmsk;
        if pending_and_enabled != 0 && self.cpu.supervisor.config.msr.interrupts() {
            self.cpu.raise_exception(gekko::Exception::Interrupt);
        }
    }
}

impl WasmEmulator {
    /// Synchronise `PI_INT_DSP` in `PI_INTSR` with the current DSPCONTROL state.
    ///
    /// Sets the DSP bit when any interrupt source is both pending and masked-on,
    /// and clears it when none are active.  Mirrors the native
    /// `get_active_interrupts` → `sources.set_dsp_interface(control.any_interrupt())`
    /// path in `pi.rs`.
    ///
    /// Must be called after every write to a DSPCONTROL-related register so that
    /// the OS can observe the correct PI_INTSR state via `__OSInitAudioSystem`
    /// and related polling loops.
    fn sync_pi_dsp(&mut self) {
        if self.dsp.any_interrupt() {
            self.pi_intsr |= PI_INT_DSP;
        } else {
            self.pi_intsr &= !PI_INT_DSP;
        }
    }

    /// Synchronise `PI_INT_PE_TOKEN` and `PI_INT_PE_FINISH` in `PI_INTSR` with
    /// the current GX Pixel Engine interrupt state.
    ///
    /// Mirrors the native `get_active_interrupts` path:
    /// ```text
    /// sources.set_pe_token (pix.interrupt.token()  && pix.interrupt.token_enabled());
    /// sources.set_pe_finish(pix.interrupt.finish() && pix.interrupt.finish_enabled());
    /// ```
    fn sync_pi_pe_interrupts(&mut self) {
        if self.gx.pe_token_active() {
            self.pi_intsr |= PI_INT_PE_TOKEN;
        } else {
            self.pi_intsr &= !PI_INT_PE_TOKEN;
        }
        if self.gx.pe_finish_active() {
            self.pi_intsr |= PI_INT_PE_FINISH;
        } else {
            self.pi_intsr &= !PI_INT_PE_FINISH;
        }
    }

    /// Execute a pending ARAM DMA transfer between main RAM and the ARAM buffer.
    ///
    /// Called from `hw_write_u32` after [`DspState::write_u32`] sets
    /// `aram_dma_pending = true` (triggered by a write to `DspAramDmaControl`
    /// at offset 0x28).  Having access to both `self.ram` and `self.aram` here
    /// allows the actual byte copy that `DspState::write_u32` cannot perform.
    ///
    /// Mirrors `dspi::aram_dma()` in the native build:
    /// - Reads `aram_dma_ram_base` (main RAM address, segment bits stripped).
    /// - Reads `aram_dma_aram_base` (ARAM byte offset).
    /// - Reads `aram_dma_control` (bit 31 = direction, bits 0–30 = length).
    /// - If `aram_base >= ARAM_LEN`, the transfer is silently dropped (the OS
    ///   uses out-of-bounds DMA to probe ARAM size; the interrupt is already set
    ///   by `DspState::write_u32`).
    /// - Otherwise performs the byte copy in the requested direction.
    fn execute_aram_dma(&mut self) {
        const ARAM_LEN: usize = 16 * 1024 * 1024;

        let aram_base = self.dsp.aram_dma_aram_base as usize;
        let ram_base  = crate::phys_addr(self.dsp.aram_dma_ram_base);
        let ctrl      = self.dsp.aram_dma_control;
        let len       = (ctrl & 0x7FFF_FFFF) as usize;
        let to_aram   = (ctrl >> 31) == 0; // 0 = RAM→ARAM, 1 = ARAM→RAM

        // Out-of-bounds ARAM address: interrupt already set; nothing to copy.
        // Mirrors native dspi::aram_dma() early-return for aram_base >= ARAM_LEN.
        if aram_base >= ARAM_LEN || len == 0 {
            return;
        }

        let eff_len = len.min(ARAM_LEN - aram_base);

        if to_aram {
            // RAM → ARAM
            if ram_base + eff_len <= self.ram.len() {
                self.aram[aram_base..aram_base + eff_len]
                    .copy_from_slice(&self.ram[ram_base..ram_base + eff_len]);
            }
        } else {
            // ARAM → RAM
            if ram_base + eff_len <= self.ram.len() {
                self.ram[ram_base..ram_base + eff_len]
                    .copy_from_slice(&self.aram[aram_base..aram_base + eff_len]);
            }
        }
    }

    /// Process a DVD Interface DMA command (called when DICR bit 0 is written 1).
    ///
    /// Decodes the command opcode in `DICMDBUF0` bits 31–24 and acts on it.
    /// Supported opcodes match the native `di::Command` set:
    ///
    /// | Opcode | Name            | Behaviour                                       |
    /// |--------|-----------------|-------------------------------------------------|
    /// | `0x12` | Identify        | Write 32 bytes of stub disc-ID data to DMA buf  |
    /// | `0xA8` | DVD Read        | Copy `DILENGTH` bytes from disc into RAM         |
    /// | `0xAB` | Seek            | No-op (block device)                            |
    /// | `0xE0` | Request Error   | Clear `DIIMMBUF` and complete                   |
    /// | `0xE1` | AudioStream     | No-op stub (native also stubs this)             |
    /// | `0xE2` | AudioStatus     | Clear `DIIMMBUF` and complete                   |
    /// | `0xE3` | Stop Motor      | No-op                                           |
    /// | `0xE4` | AudioConfig     | No-op stub                                      |
    /// | `0xFE` | Debug           | No-op                                           |
    /// | `0xFF` | DebugEnable     | No-op                                           |
    ///
    /// On completion, `DICR` bit 0 (TSTART) is cleared and `DISTATUS` bit 1
    /// (TCINT — transfer complete) is set, mirroring `di::complete_transfer`.
    fn process_di_command(&mut self) {
        let cmd     = (self.di.cmd_buf0 >> 24) as u8;
        let sub_cmd = ((self.di.cmd_buf0 >> 16) & 0xFF) as u8;
        // DICMDBUF1 stores the disc address in 4-byte units, not bytes.
        // ipl-hle (and the real apploader) write `byte_offset >> 2` to this
        // register, so we must shift left by 2 to recover the byte offset.
        let disc_offset = (self.di.cmd_buf1 as usize) << 2;
        let dma_len  = self.di.dma_len as usize;
        let dma_dest = crate::phys_addr(self.di.dma_addr);

        match cmd {
            // ── 0x12: Identify ────────────────────────────────────────────
            // Write a 32-byte disc identification structure into the DMA
            // destination.  Mirrors native di.rs Command::Identify handling.
            0x12 => {
                if dma_dest + 32 <= self.ram.len() {
                    // Date / version bytes matching native stub output.
                    let stub: [u8; 32] = [
                        0x00, 0x00, 0x00, 0x00, // zeros
                        0x20, 0x02, 0x04, 0x02, // date
                        0x61, 0x00, 0x00, 0x00, // version
                        0x00, 0x00, 0x00, 0x00,
                        0x00, 0x00, 0x00, 0x00,
                        0x00, 0x00, 0x00, 0x00,
                        0x00, 0x00, 0x00, 0x00,
                        0x00, 0x00, 0x00, 0x00,
                    ];
                    self.ram[dma_dest..dma_dest + 32].copy_from_slice(&stub);
                }
            }

            // ── 0xA8: DVD Read ────────────────────────────────────────────
            0xA8 => {
                // `self.disc` and `self.ram` are separate fields with disjoint heap
                // allocations; `as_deref()` borrows only the former, so Rust allows
                // a simultaneous mutable borrow of the latter.
                if let Some(disc) = self.disc.as_deref() {
                    let src_end = disc_offset.saturating_add(dma_len);
                    if src_end <= disc.len() && dma_dest + dma_len <= self.ram.len() {
                        // Log BEFORE the copy so that any format!() heap allocation
                        // (which may trigger WASM linear-memory growth and detach the
                        // JS RAM Uint8Array) happens before the disc data is written.
                        let preview_len = dma_len.min(8);
                        let mut preview = [0u8; 8];
                        preview[..preview_len].copy_from_slice(&disc[disc_offset..disc_offset + preview_len]);
                        console_log!(
                            "[lazuli] DI: DVD Read disc_off={:#010x} len={:#x} \
                             ram_dest={:#010x} data=[{:02x} {:02x} {:02x} {:02x} \
                             {:02x} {:02x} {:02x} {:02x}]",
                            disc_offset,
                            dma_len,
                            self.di.dma_addr,
                            preview[0], preview[1], preview[2], preview[3],
                            preview[4], preview[5], preview[6], preview[7],
                        );
                        self.ram[dma_dest..dma_dest + dma_len]
                            .copy_from_slice(&disc[disc_offset..src_end]);
                        // New code may have been written into guest RAM; signal JS
                        // to perform selective JIT cache invalidation for the
                        // affected address range.
                        self.dma_dirty = true;
                        self.last_dma_addr = dma_dest as u32;
                        self.last_dma_len  = dma_len  as u32;
                        self.last_di_disc_offset = disc_offset as u32;
                    } else {
                        console_log!(
                            "[lazuli] DI: DVD Read out of bounds \
                             (disc_off={:#010x}, len={:#x}, disc_len={:#x}, \
                             ram_dest={:#010x}, ram_len={:#x})",
                            disc_offset,
                            dma_len,
                            disc.len(),
                            dma_dest,
                            self.ram.len()
                        );
                    }
                } else {
                    console_log!(
                        "[lazuli] DI: DVD Read disc_off={:#010x} len={:#x} ram_dest={:#010x} \
                         — NO DISC LOADED (call load_disc_image() first)",
                        disc_offset,
                        dma_len,
                        self.di.dma_addr,
                    );
                }
            }

            // ── 0xAB: Seek — no-op on a block device ─────────────────────
            0xAB => {}

            // ── 0xE0: Request Error / Status ──────────────────────────────
            0xE0 => {
                // Return 0 via DIIMMBUF (no error).
                self.di.imm_buf = 0;
            }

            // ── 0xE1: AudioStream — start/stop/status ─────────────────────
            // Mirrors native Command::StartAudioStream / StopAudioStream /
            // AudioStreamStatus stubs (all set transfer_interrupt and complete).
            0xE1 => {
                let sub_name = match sub_cmd {
                    0x00 => "StartAudioStream",
                    0x01 => "StopAudioStream",
                    _    => "AudioStreamStatus",
                };
                console_log!("[lazuli] DI: {} (0xE1/{:#04x}) — stub", sub_name, sub_cmd);
                self.di.imm_buf = 0;
            }

            // ── 0xE2: AudioStatus ─────────────────────────────────────────
            0xE2 => {
                console_log!("[lazuli] DI: AudioStatus (0xE2/{:#04x}) — stub", sub_cmd);
                self.di.imm_buf = 0;
            }

            // ── 0xE3: Stop Motor — no-op ──────────────────────────────────
            0xE3 => {}

            // ── 0xE4: AudioConfig (enable/disable audio stream) ───────────
            0xE4 => {
                console_log!("[lazuli] DI: AudioConfig (0xE4/{:#04x}) — stub", sub_cmd);
                self.di.imm_buf = 0;
            }

            // ── 0xFE / 0xFF: Debug / DebugEnable — no-op ─────────────────
            0xFE | 0xFF => {}

            other => {
                console_log!("[lazuli] DI: unrecognised command {:#04x}", other);
            }
        }

        // Mark transfer complete: clear TSTART (bit 0), set TCINT (bit 1).
        // Mirrors di::complete_transfer in the native build.
        self.di.control &= !0x1; // clear TSTART
        self.di.status |= 0x2;   // set   TCINT
    }
}
