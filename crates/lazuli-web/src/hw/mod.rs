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
//! | Base address  | Module | Description                    |
//! |---------------|--------|--------------------------------|
//! | `0xCC002000`  | `vi`   | Video Interface                |
//! | `0xCC003000`  | `pi`   | Processor Interface            |
//! | `0xCC005000`  | `dsp`  | DSP Interface / ARAM           |
//! | `0xCC006000`  | `di`   | DVD Interface                  |
//! | `0xCC006400`  | `si`   | Serial Interface (controllers) |
//! | `0xCC006800`  | `exi`  | External Interface             |
//! | `0xCC006C00`  | `ai`   | Audio Interface                |

pub(crate) mod ai;
pub(crate) mod di;
pub(crate) mod dsp;
pub(crate) mod exi;
pub(crate) mod pi;
pub(crate) mod si;
pub(crate) mod vi;

pub(crate) use ai::{AiState, AI_BASE, AI_SIZE};
pub(crate) use di::{DiState, DI_BASE, DI_SIZE};
pub(crate) use dsp::{DspState, DSP_BASE, DSP_SIZE};
pub(crate) use exi::{ExiState, EXI_BASE, EXI_SIZE};
pub(crate) use pi::{
    PI_BASE, PI_BUSCLK_VAL, PI_CPUCLK_VAL, PI_INT_DI, PI_INT_SI as PI_SI, PI_INT_VI,
    PI_MEMSIZE_VAL, PI_SIZE,
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
                // PI_FIFO_BASE / PI_FIFO_END / PI_FIFO_WPTR (0x0C/0x10/0x14): return 0
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

        // ── Serial Interface ───────────────────────────────────────────────
        if addr >= SI_BASE && addr < SI_BASE + SI_SIZE {
            return self.si.read_u32(addr - SI_BASE);
        }

        // ── External Interface ─────────────────────────────────────────────
        if addr >= EXI_BASE && addr < EXI_BASE + EXI_SIZE {
            return self.exi.read_u32(addr - EXI_BASE);
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

        // ── Video Interface ────────────────────────────────────────────────
        if addr >= VI_BASE && addr < VI_BASE + VI_SIZE {
            self.vi.write_u32(addr - VI_BASE, val);
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
                0x04 => self.pi_intmsk = val,
                _ => {}
            }
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
            self.exi.write_u32(addr - EXI_BASE, val);
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

        // ── Video Interface ────────────────────────────────────────────────
        if addr >= VI_BASE && addr < VI_BASE + VI_SIZE {
            self.vi.write_u16(addr - VI_BASE, val);
            return;
        }

        // ── DSP Interface ──────────────────────────────────────────────────
        if addr >= DSP_BASE && addr < DSP_BASE + DSP_SIZE {
            self.dsp.write_u16(addr - DSP_BASE, val);
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
    /// game loop.  The function sets the VI bit in `PI_INTSR` and, if the
    /// guest CPU currently has external interrupts enabled (`MSR.EE = 1`) and
    /// the OS has unmasked the VI interrupt in `PI_INTMSK`, delivers an
    /// `Exception::Interrupt` (vector `0x00000500`) to the CPU.
    pub fn assert_vi_interrupt(&mut self) {
        // Advance the VI vertical counter by one field (called each RAF frame).
        self.vi.advance_vcount();

        // Set the VI pending bit in PI_INTSR.
        self.pi_intsr |= PI_INT_VI;

        // Deliver the external interrupt immediately if EE=1 and the OS has
        // unmasked the VI interrupt in PI_INTMSK.  Do not pre-clear PI_INTSR:
        // the ISR must be able to read the bit to know which interrupt fired.
        self.maybe_deliver_external_interrupt();
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
}

// ─── Private helpers ──────────────────────────────────────────────────────────

impl WasmEmulator {
    /// Deliver an external interrupt to the CPU if EE=1 and any enabled
    /// interrupt is pending in PI_INTSR & PI_INTMSK.
    ///
    /// Called after setting any PI_INTSR bit (VI, SI, AI, EXI, DI, …) so that
    /// the interrupt reaches the CPU as soon as the OS re-enables EE.
    pub(crate) fn maybe_deliver_external_interrupt(&mut self) {
        let pending_and_enabled = self.pi_intsr & self.pi_intmsk;
        if pending_and_enabled != 0 && self.cpu.supervisor.config.msr.interrupts() {
            self.cpu.raise_exception(gekko::Exception::Interrupt);
        }
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
    /// On completion, `DICR` bit 0 (TSTART) is cleared and `DISTATUS` bit 1
    /// (TCINT — transfer complete) is set.
    fn process_di_command(&mut self) {
        let cmd = (self.di.cmd_buf0 >> 24) as u8;
        // DICMDBUF1 stores the disc address in 4-byte units, not bytes.
        // ipl-hle (and the real apploader) write `byte_offset >> 2` to this
        // register, so we must shift left by 2 to recover the byte offset.
        let disc_offset = (self.di.cmd_buf1 as usize) << 2;
        let dma_len = self.di.dma_len as usize;
        let dma_dest = crate::phys_addr(self.di.dma_addr);

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
                        // New code may have been written into guest RAM; signal JS
                        // to perform selective JIT cache invalidation for the
                        // affected address range.
                        self.dma_dirty = true;
                        self.last_dma_addr = dma_dest as u32;
                        self.last_dma_len  = dma_len  as u32;
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
                    console_log!(
                        "[lazuli] DI: DVD Read with no disc loaded — call load_disc_image() first"
                    );
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
        self.di.status |= 0x2; // set   TCINT
    }
}
