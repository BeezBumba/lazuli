//! Hardware register I/O — bridges JavaScript hook calls to per-subsystem
//! register state structs.
//!
//! This module exposes `hw_read_u32`, `hw_write_u32`, `hw_read_u16`,
//! `hw_write_u16`, and `assert_vi_interrupt` on [`WasmEmulator`].  These are
//! called from JavaScript when the guest accesses an MMIO address (prefix
//! `0xCC` cached or `0xCD` uncached), before the `PHYS_MASK` is applied.

pub(crate) mod di;
pub(crate) mod dsp;
pub(crate) mod pi;

pub(crate) use di::{DiState, DI_BASE, DI_SIZE};
pub(crate) use dsp::{DspState, DSP_BASE, DSP_SIZE};
pub(crate) use pi::{
    PI_BASE, PI_BUSCLK_VAL, PI_CPUCLK_VAL, PI_INT_VI, PI_MEMSIZE_VAL, PI_SIZE,
};

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
    /// The GameCube exposes the same hardware registers at both:
    ///   - `0xCCxxxxxx` — cached I/O mirror (used by most SDK code)
    ///   - `0xCDxxxxxx` — uncached I/O mirror (used by DMA paths)
    /// Both aliases are normalised to the `0xCC` base before dispatching.
    ///
    /// Currently handles:
    /// - **Processor Interface** (`0xCC003000–0xCC00303F`): INTSR, INTMSK,
    ///   MEMSIZE, BUSCLK, CPUCLK
    /// - **DVD Interface** (`0xCC006000–0xCC006027`): full register read
    /// - All other hardware registers: returns `0`
    pub fn hw_read_u32(&self, addr: u32) -> u32 {
        // Normalise uncached (0xCDxxxxxx) addresses to the cached (0xCCxxxxxx)
        // mirror so a single set of base-address constants covers both aliases.
        let addr = addr & !UNCACHED_MIRROR_BIT;
        if addr >= PI_BASE && addr < PI_BASE + PI_SIZE {
            let offset = addr - PI_BASE;
            return match offset {
                0x00 => self.pi_intsr,
                0x04 => self.pi_intmsk,
                0x28 => PI_MEMSIZE_VAL,
                0x2C => PI_BUSCLK_VAL,
                0x30 => PI_CPUCLK_VAL,
                _ => 0,
            };
        }
        if addr >= DI_BASE && addr < DI_BASE + DI_SIZE {
            return self.di.read_reg(addr - DI_BASE);
        }
        0
    }

    /// Write a 32-bit value to a GameCube hardware register.
    ///
    /// Called by the JavaScript `write_u32` hook when the guest address has
    /// the prefix `0xCC` or `0xCD`, before `PHYS_MASK` is applied.
    /// Both cached (`0xCC`) and uncached (`0xCD`) mirrors are accepted.
    ///
    /// Writing `DICR` (offset `0x1C`) with bit 0 set triggers an immediate
    /// disc DMA: the stored [`DiState`] command registers are decoded and the
    /// requested bytes are copied from `disc` into guest RAM.
    ///
    /// Currently handles:
    /// - **Processor Interface** (`0xCC003000–0xCC00303F`): INTSR (W1C),
    ///   INTMSK
    /// - **DVD Interface** (`0xCC006000–0xCC006027`): full register write + DMA
    /// - All other hardware registers: silently ignored
    pub fn hw_write_u32(&mut self, addr: u32, val: u32) {
        let addr = addr & !UNCACHED_MIRROR_BIT;
        if addr >= PI_BASE && addr < PI_BASE + PI_SIZE {
            let offset = addr - PI_BASE;
            match offset {
                // PI_INTSR: write-1-to-clear — guest OS writes back the bits
                // it has handled to acknowledge the interrupt.
                0x00 => self.pi_intsr &= !val,
                // PI_INTMSK: interrupt enable mask (normal read/write).
                0x04 => self.pi_intmsk = val,
                _ => {}
            }
            return;
        }
        if addr >= DI_BASE && addr < DI_BASE + DI_SIZE {
            if self.di.write_reg(addr - DI_BASE, val) {
                self.process_di_command();
            }
        }
        // All other hardware addresses are ignored (reads return 0 via hw_read_u32).
    }

    /// Read a 16-bit value from a GameCube hardware register.
    ///
    /// Called by the JavaScript `read_u16` hook when the guest address has the
    /// prefix `0xCC` or `0xCD`.  Currently handles:
    ///
    /// - **DSP Interface** (`0xCC005000–0xCC00500F`): DSP→CPU mailbox and
    ///   control register.
    /// - **DVD Interface** (`0xCC006000–0xCC006027`): 16-bit half of any 32-bit
    ///   DI register.  The guest OS occasionally reads DISTATUS and other DI
    ///   registers with `lhz` (halfword load), so we return the correct
    ///   big-endian half of the underlying 32-bit register value.
    /// - All other hardware addresses: returns `0`
    pub fn hw_read_u16(&self, addr: u32) -> u16 {
        let addr = addr & !UNCACHED_MIRROR_BIT;
        if addr >= DSP_BASE && addr < DSP_BASE + DSP_SIZE {
            return self.dsp.read_u16(addr - DSP_BASE);
        }
        if addr >= DI_BASE && addr < DI_BASE + DI_SIZE {
            let offset = addr - DI_BASE;
            // DI registers are 32-bit; align to the containing 32-bit word
            // and return the correct big-endian halfword.
            let word_offset = offset & !3;
            let word = self.di.read_reg(word_offset);
            return if (offset & 2) == 0 {
                (word >> 16) as u16  // upper (most-significant) halfword
            } else {
                word as u16          // lower (least-significant) halfword
            };
        }
        0
    }

    /// Write a 16-bit value to a GameCube hardware register.
    ///
    /// Called by the JavaScript `write_u16` hook when the guest address has the
    /// prefix `0xCC` or `0xCD`.  Currently handles:
    ///
    /// - **DSP Interface** (`0xCC005000–0xCC00500F`): CPU→DSP mailbox.  Values
    ///   written to `0xCC005004`/`0xCC005006` are echoed to the DSP→CPU
    ///   mailbox so the OS's boot-ACK polling loop exits immediately.
    /// - All other hardware addresses: silently ignored
    pub fn hw_write_u16(&mut self, addr: u32, val: u16) {
        let addr = addr & !UNCACHED_MIRROR_BIT;
        if addr >= DSP_BASE && addr < DSP_BASE + DSP_SIZE {
            self.dsp.write_u16(addr - DSP_BASE, val);
        }
        // All other 16-bit hardware writes are silently ignored.
    }

    /// Assert a Video Interface (VI) vertical-retrace interrupt.
    ///
    /// Call this **once per animation frame** (~60 Hz) from the JavaScript
    /// game loop.  The function sets the VI bit in `PI_INTSR` and, if the
    /// guest CPU currently has external interrupts enabled (`MSR.EE = 1`) and
    /// the OS has unmasked the VI interrupt in `PI_INTMSK`, delivers an
    /// `Exception::Interrupt` (vector `0x00000500`) to the CPU.
    pub fn assert_vi_interrupt(&mut self) {
        // Set the VI pending bit in PI_INTSR.
        self.pi_intsr |= PI_INT_VI;

        // Deliver the external interrupt immediately if EE=1 and the OS has
        // unmasked the VI interrupt in PI_INTMSK.  Do not pre-clear PI_INTSR:
        // the ISR must be able to read the bit to know which interrupt fired.
        let ee = self.cpu.supervisor.config.msr.interrupts();
        let vi_enabled = (self.pi_intmsk & PI_INT_VI) != 0;
        if ee && vi_enabled {
            self.cpu.raise_exception(gekko::Exception::Interrupt);
        }
    }

    /// Return `true` if a DVD DMA has written new data into guest RAM since the
    /// last call, and reset the flag to `false`.
    ///
    /// JavaScript must call this after every `hw_write_u32` and, when it returns
    /// `true`, invalidate any JIT modules whose code may have been overwritten.
    /// Use [`last_dma_addr`] and [`last_dma_len`] to perform selective
    /// per-address invalidation rather than flushing the entire cache.
    pub fn take_dma_dirty(&mut self) -> bool {
        let dirty = self.dma_dirty;
        self.dma_dirty = false;
        dirty
    }

    /// Physical start address (in emulator RAM) of the most recent successful
    /// DVD DMA transfer.
    ///
    /// Valid only immediately after [`take_dma_dirty`] returns `true`.
    /// JavaScript compares this against `(pc & PHYS_MASK)` for each cached
    /// module to determine which blocks were overwritten by the DMA.
    pub fn last_dma_addr(&self) -> u32 {
        self.last_dma_addr
    }

    /// Byte length of the most recent successful DVD DMA transfer.
    ///
    /// Together with [`last_dma_addr`] this defines the half-open byte interval
    /// `[last_dma_addr, last_dma_addr + last_dma_len)` that was overwritten.
    pub fn last_dma_len(&self) -> u32 {
        self.last_dma_len
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

impl WasmEmulator {
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
        let disc_offset = self.di.cmd_buf1 as usize;
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
