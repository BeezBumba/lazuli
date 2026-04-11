//! Graphics subsystem (GX) hardware register stubs.
//!
//! This module provides read/write stubs for the Command Processor (CP) and
//! Pixel Engine (PE) MMIO registers that games access when initialising the
//! GPU pipeline.  No actual rendering is performed — the stubs exist so that
//! register configuration writes are accepted, CP FIFO pointer reads return
//! consistent values, and the PE interrupt mechanism works correctly so that
//! games waiting on `PE_TOKEN` or `PE_FINISH` can proceed.
//!
//! The browser build fires `PE_FINISH` once per VI frame (inside
//! [`WasmEmulator::assert_vi_interrupt`]) to unblock games that use
//! `GXWaitForDrawDone()` as a frame-sync primitive.
//!
//! ## Register map (offsets from `GX_BASE = 0xCC000000`)
//!
//! | Offset | Width | Name                 | Description                         |
//! |--------|-------|----------------------|-------------------------------------|
//! | 0x0000 | 16    | CP_STATUS            | CP FIFO status flags (read-only)    |
//! | 0x0002 | 16    | CP_CONTROL           | CP FIFO enable / clear bits         |
//! | 0x0004 | 16    | CP_CLEAR             | CP FIFO clear register              |
//! | 0x0020 | 16    | CP_FIFO_START_LO     | FIFO start address, low 16 bits     |
//! | 0x0022 | 16    | CP_FIFO_START_HI     | FIFO start address, high 16 bits    |
//! | 0x0024 | 16    | CP_FIFO_END_LO       | FIFO end address, low 16 bits       |
//! | 0x0026 | 16    | CP_FIFO_END_HI       | FIFO end address, high 16 bits      |
//! | 0x0028 | 16    | CP_FIFO_HWMARK_LO    | High-watermark, low 16 bits         |
//! | 0x002A | 16    | CP_FIFO_HWMARK_HI    | High-watermark, high 16 bits        |
//! | 0x002C | 16    | CP_FIFO_LWMARK_LO    | Low-watermark, low 16 bits          |
//! | 0x002E | 16    | CP_FIFO_LWMARK_HI    | Low-watermark, high 16 bits         |
//! | 0x0030 | 16    | CP_FIFO_COUNT_LO     | FIFO token count, low 16 bits       |
//! | 0x0032 | 16    | CP_FIFO_COUNT_HI     | FIFO token count, high 16 bits      |
//! | 0x0034 | 16    | CP_FIFO_WPTR_LO      | FIFO write pointer, low 16 bits     |
//! | 0x0036 | 16    | CP_FIFO_WPTR_HI      | FIFO write pointer, high 16 bits    |
//! | 0x0038 | 16    | CP_FIFO_RPTR_LO      | FIFO read pointer, low 16 bits      |
//! | 0x003A | 16    | CP_FIFO_RPTR_HI      | FIFO read pointer, high 16 bits     |
//! | 0x003C | 16    | CP_FIFO_BPT_LO       | Breakpoint address, low 16 bits     |
//! | 0x003E | 16    | CP_FIFO_BPT_HI       | Breakpoint address, high 16 bits    |
//! | 0x100A | 16    | PE_INT_STATUS        | PE interrupt enable + pending bits  |
//! | 0x100E | 16    | PE_TOKEN             | PE token value                      |
//!
//! ## PE interrupt status register (at offset 0x100A) bit layout
//!
//! | Bit | Field              | Description                               |
//! |-----|--------------------|-------------------------------------------|
//! |  0  | token_enable       | Token interrupt enable (R/W)              |
//! |  1  | finish_enable      | Finish interrupt enable (R/W)             |
//! |  2  | token_pending      | Token interrupt pending — W1C             |
//! |  3  | finish_pending     | Finish interrupt pending — W1C            |
//!
//! Mirrors `pix::InterruptStatus` in the native build.

/// Physical base address of the GX Command Processor / Pixel Engine registers.
pub(crate) const GX_BASE: u32 = 0xCC00_0000;

/// Byte span of the GX register block.
///
/// Covers from `GX_BASE + 0x0000` (CP_STATUS) through `GX_BASE + 0x100F`
/// (PE_TOKEN + 2 bytes).
pub(crate) const GX_SIZE: u32 = 0x1010;

/// GX Command Processor and Pixel Engine hardware register file (stub).
///
/// CP registers accept reads and writes so that the OS/game FIFO configuration
/// code proceeds without hanging.  PE interrupt registers implement the full
/// enable/W1C protocol so that `GXWaitForDrawDone()` can be unblocked by
/// [`GxState::fire_pe_finish`].
#[derive(Default)]
pub(crate) struct GxState {
    // ── Command Processor (CP) FIFO register file ─────────────────────────
    /// CP_STATUS (0x0000): FIFO overflow / underflow / active flags.
    pub(crate) cp_status: u16,
    /// CP_CONTROL (0x0002): FIFO reading enable, GP link mode enable, etc.
    pub(crate) cp_control: u16,
    /// CP_CLEAR (0x0004): write-to-clear bits.
    pub(crate) cp_clear: u16,
    /// CP FIFO start address (0x0020/0x0022 combined into 32 bits).
    pub(crate) cp_fifo_start: u32,
    /// CP FIFO end address (0x0024/0x0026 combined into 32 bits).
    pub(crate) cp_fifo_end: u32,
    /// CP FIFO high watermark (0x0028/0x002A combined into 32 bits).
    pub(crate) cp_fifo_hwmark: u32,
    /// CP FIFO low watermark (0x002C/0x002E combined into 32 bits).
    pub(crate) cp_fifo_lwmark: u32,
    /// CP FIFO token count (0x0030/0x0032 combined into 32 bits).
    pub(crate) cp_fifo_count: u32,
    /// CP FIFO write pointer (0x0034/0x0036 combined into 32 bits).
    pub(crate) cp_fifo_wptr: u32,
    /// CP FIFO read pointer (0x0038/0x003A combined into 32 bits).
    pub(crate) cp_fifo_rptr: u32,
    /// CP FIFO breakpoint address (0x003C/0x003E combined into 32 bits).
    pub(crate) cp_fifo_breakpoint: u32,

    // ── Pixel Engine (PE) interrupt registers ──────────────────────────────
    /// PE interrupt status register (0x100A).
    ///
    /// Bit layout:
    /// - bit 0: token interrupt enable (R/W)
    /// - bit 1: finish interrupt enable (R/W)
    /// - bit 2: token interrupt pending (W1C — hardware sets, OS clears)
    /// - bit 3: finish interrupt pending (W1C — hardware sets, OS clears)
    pub(crate) pe_int_status: u16,

    /// PE token value (0x100E): the 16-bit token written by the GX command.
    pub(crate) pe_token: u16,
}

impl GxState {
    /// Read a 16-bit value from the GX register at `offset` bytes from `GX_BASE`.
    pub(crate) fn read_u16(&self, offset: u32) -> u16 {
        match offset {
            0x0000 => self.cp_status,
            0x0002 => self.cp_control,
            0x0004 => self.cp_clear,
            // CP FIFO start (low/high halfwords)
            0x0020 => self.cp_fifo_start as u16,
            0x0022 => (self.cp_fifo_start >> 16) as u16,
            // CP FIFO end (low/high halfwords)
            0x0024 => self.cp_fifo_end as u16,
            0x0026 => (self.cp_fifo_end >> 16) as u16,
            // CP FIFO high watermark (low/high halfwords)
            0x0028 => self.cp_fifo_hwmark as u16,
            0x002A => (self.cp_fifo_hwmark >> 16) as u16,
            // CP FIFO low watermark (low/high halfwords)
            0x002C => self.cp_fifo_lwmark as u16,
            0x002E => (self.cp_fifo_lwmark >> 16) as u16,
            // CP FIFO count (low/high halfwords)
            0x0030 => self.cp_fifo_count as u16,
            0x0032 => (self.cp_fifo_count >> 16) as u16,
            // CP FIFO write pointer (low/high halfwords)
            0x0034 => self.cp_fifo_wptr as u16,
            0x0036 => (self.cp_fifo_wptr >> 16) as u16,
            // CP FIFO read pointer (low/high halfwords)
            0x0038 => self.cp_fifo_rptr as u16,
            0x003A => (self.cp_fifo_rptr >> 16) as u16,
            // CP FIFO breakpoint (low/high halfwords)
            0x003C => self.cp_fifo_breakpoint as u16,
            0x003E => (self.cp_fifo_breakpoint >> 16) as u16,
            // PE interrupt status
            0x100A => self.pe_int_status,
            // PE token value
            0x100E => self.pe_token,
            _ => 0,
        }
    }

    /// Write a 16-bit value to the GX register at `offset` bytes from `GX_BASE`.
    ///
    /// Returns `true` when the PE interrupt status changes (caller should
    /// re-evaluate `PI_INT_PE_TOKEN` / `PI_INT_PE_FINISH` in `PI_INTSR`).
    pub(crate) fn write_u16(&mut self, offset: u32, val: u16) -> bool {
        match offset {
            0x0000 => self.cp_status = val,
            0x0002 => self.cp_control = val,
            0x0004 => self.cp_clear = val,
            // CP FIFO start
            0x0020 => self.cp_fifo_start = (self.cp_fifo_start & 0xFFFF_0000) | val as u32,
            0x0022 => {
                self.cp_fifo_start =
                    (self.cp_fifo_start & 0x0000_FFFF) | ((val as u32) << 16);
            }
            // CP FIFO end
            0x0024 => self.cp_fifo_end = (self.cp_fifo_end & 0xFFFF_0000) | val as u32,
            0x0026 => {
                self.cp_fifo_end =
                    (self.cp_fifo_end & 0x0000_FFFF) | ((val as u32) << 16);
            }
            // CP FIFO high watermark
            0x0028 => self.cp_fifo_hwmark = (self.cp_fifo_hwmark & 0xFFFF_0000) | val as u32,
            0x002A => {
                self.cp_fifo_hwmark =
                    (self.cp_fifo_hwmark & 0x0000_FFFF) | ((val as u32) << 16);
            }
            // CP FIFO low watermark
            0x002C => self.cp_fifo_lwmark = (self.cp_fifo_lwmark & 0xFFFF_0000) | val as u32,
            0x002E => {
                self.cp_fifo_lwmark =
                    (self.cp_fifo_lwmark & 0x0000_FFFF) | ((val as u32) << 16);
            }
            // CP FIFO count (read-only in hardware; accept writes without action)
            0x0030 => self.cp_fifo_count = (self.cp_fifo_count & 0xFFFF_0000) | val as u32,
            0x0032 => {
                self.cp_fifo_count =
                    (self.cp_fifo_count & 0x0000_FFFF) | ((val as u32) << 16);
            }
            // CP FIFO write pointer
            0x0034 => self.cp_fifo_wptr = (self.cp_fifo_wptr & 0xFFFF_0000) | val as u32,
            0x0036 => {
                self.cp_fifo_wptr =
                    (self.cp_fifo_wptr & 0x0000_FFFF) | ((val as u32) << 16);
            }
            // CP FIFO read pointer
            0x0038 => self.cp_fifo_rptr = (self.cp_fifo_rptr & 0xFFFF_0000) | val as u32,
            0x003A => {
                self.cp_fifo_rptr =
                    (self.cp_fifo_rptr & 0x0000_FFFF) | ((val as u32) << 16);
            }
            // CP FIFO breakpoint
            0x003C => {
                self.cp_fifo_breakpoint =
                    (self.cp_fifo_breakpoint & 0xFFFF_0000) | val as u32;
            }
            0x003E => {
                self.cp_fifo_breakpoint =
                    (self.cp_fifo_breakpoint & 0x0000_FFFF) | ((val as u32) << 16);
            }
            // PE interrupt status (0x100A): bits 0-1 R/W enable, bits 2-3 W1C pending.
            0x100A => {
                let old = self.pe_int_status;
                // Update R/W enable bits (0 = token_enable, 1 = finish_enable).
                self.pe_int_status = (self.pe_int_status & !0x0003) | (val & 0x0003);
                // W1C pending bits (2 = token_pending, 3 = finish_pending).
                self.pe_int_status &= !(val & 0x000C);
                return self.pe_int_status != old;
            }
            // PE token value
            0x100E => self.pe_token = val,
            _ => {}
        }
        false
    }

    /// Assert the PE_FINISH interrupt (sets bit 3 of `PE_INT_STATUS`).
    ///
    /// Called once per VI frame from [`WasmEmulator::assert_vi_interrupt`]
    /// to simulate the GPU completing a frame — unblocking games that call
    /// `GXWaitForDrawDone()` with EE=1.
    ///
    /// Returns `true` if the finish interrupt is also **enabled** (bit 1),
    /// indicating the caller should assert `PI_INT_PE_FINISH` in `PI_INTSR`.
    pub(crate) fn fire_pe_finish(&mut self) -> bool {
        self.pe_int_status |= 1 << 3; // set finish_pending
        (self.pe_int_status & (1 << 1)) != 0 // finish_enable set?
    }

    /// Assert the PE_TOKEN interrupt (sets bit 2 of `PE_INT_STATUS`).
    ///
    /// Returns `true` if the token interrupt is also **enabled** (bit 0),
    /// indicating the caller should assert `PI_INT_PE_TOKEN` in `PI_INTSR`.
    #[allow(dead_code)]
    pub(crate) fn fire_pe_token(&mut self, token: u16) -> bool {
        self.pe_token = token;
        self.pe_int_status |= 1 << 2; // set token_pending
        (self.pe_int_status & (1 << 0)) != 0 // token_enable set?
    }

    /// Whether a PE token interrupt is currently active (pending AND enabled).
    ///
    /// Mirrors `pix::InterruptStatus::token() && pix::InterruptStatus::token_enabled()`
    /// in the native build.
    pub(crate) fn pe_token_active(&self) -> bool {
        // bit 2 (token_pending) AND bit 0 (token_enable)
        (self.pe_int_status & 0x0005) == 0x0005
    }

    /// Whether a PE finish interrupt is currently active (pending AND enabled).
    ///
    /// Mirrors `pix::InterruptStatus::finish() && pix::InterruptStatus::finish_enabled()`
    /// in the native build.
    pub(crate) fn pe_finish_active(&self) -> bool {
        // bit 3 (finish_pending) AND bit 1 (finish_enable)
        (self.pe_int_status & 0x000A) == 0x000A
    }
}
