//! Video Interface (VI) hardware register state.
//!
//! Tracks all VI control and frame-buffer registers at `0xCC002000–0xCC00207F`.
//! The most important register for rendering is `TFBL` (Top Field Base Left,
//! offset `0x1C`): it encodes the physical address of the XFB divided by 32.
//! JavaScript reads [`ViState::xfb_addr`] to obtain the actual XFB physical
//! address instead of using a heuristic scan of guest RAM.
//!
//! ## Register map (offsets from `VI_BASE`)
//!
//! | Offset | Name   | Width | Description                                     |
//! |--------|--------|-------|-------------------------------------------------|
//! | 0x00   | VTR    | 16    | Vertical Timing (equalization, active lines)    |
//! | 0x02   | DCR    | 16    | Display Configuration (enable, reset, format)   |
//! | 0x04   | HTR0   | 32    | Horizontal Timing 0                             |
//! | 0x08   | HTR1   | 32    | Horizontal Timing 1                             |
//! | 0x0C   | VTO    | 32    | Vertical Timing Odd Field                       |
//! | 0x10   | VTE    | 32    | Vertical Timing Even Field                      |
//! | 0x14   | BBOI   | 32    | Blanking/Border Odd Field                       |
//! | 0x18   | BBEI   | 32    | Blanking/Border Even Field                      |
//! | 0x1C   | TFBL   | 32    | Top Field Base Left (**XFB address >> 5**)      |
//! | 0x20   | TFBR   | 32    | Top Field Base Right                            |
//! | 0x24   | BFBL   | 32    | Bottom Field Base Left                          |
//! | 0x28   | BFBR   | 32    | Bottom Field Base Right                         |
//! | 0x2C   | VCOUNT | 16    | Current vertical scan-line counter              |
//! | 0x2E   | HCOUNT | 16    | Current horizontal sample counter               |
//! | 0x30   | DI0    | 32    | Display Interrupt 0                             |
//! | 0x34   | DI1    | 32    | Display Interrupt 1                             |
//! | 0x38   | DI2    | 32    | Display Interrupt 2                             |
//! | 0x3C   | DI3    | 32    | Display Interrupt 3                             |
//! | 0x4C   | HSR    | 16    | Horizontal Scaling                              |
//! | 0x6C   | VICLK  | 16    | VI Clock Select                                 |
//! | 0x6E   | VISEL  | 16    | VI DTV (progressive scan) status                |

/// Physical base address of the Video Interface registers.
pub(crate) const VI_BASE: u32 = 0xCC00_2000;
/// Byte span of the VI register block tracked by this module (128 bytes).
pub(crate) const VI_SIZE: u32 = 0x80;

/// Video Interface hardware register file.
///
/// Registers are stored in a flat array of 32-bit words indexed by
/// `offset / 4` (16-bit registers occupy the upper or lower half of the
/// containing word, so any 32-bit access covers both halves).
#[derive(Default)]
pub(crate) struct ViState {
    /// Raw 32-bit register storage.  Indexed by `(offset_from_VI_BASE) / 4`.
    /// Two consecutive entries cover an 8-byte range at offsets 0, 4, 8, …
    regs: [u32; 32],
}

impl ViState {
    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Read 32 bits from the register word at `offset` (must be 4-aligned).
    #[inline]
    fn read32(&self, offset: u32) -> u32 {
        let idx = (offset >> 2) as usize;
        if idx < self.regs.len() { self.regs[idx] } else { 0 }
    }

    /// Write 32 bits to the register word at `offset` (must be 4-aligned).
    #[inline]
    fn write32(&mut self, offset: u32, val: u32) {
        let idx = (offset >> 2) as usize;
        if idx < self.regs.len() {
            self.regs[idx] = val;
        }
    }

    // ── Public register I/O (called from hw/mod.rs) ───────────────────────────

    /// Read a 32-bit value from the VI register at `offset` bytes from `VI_BASE`.
    pub(crate) fn read_u32(&self, offset: u32) -> u32 {
        self.read32(offset & !3)
    }

    /// Read a 16-bit value from the VI register at `offset` bytes from `VI_BASE`.
    pub(crate) fn read_u16(&self, offset: u32) -> u16 {
        let word = self.read32(offset & !3);
        // Big-endian halfword: upper half at even 4-byte boundary, lower at +2.
        if (offset & 2) == 0 { (word >> 16) as u16 } else { word as u16 }
    }

    /// Write a 32-bit value to the VI register at `offset` bytes from `VI_BASE`.
    pub(crate) fn write_u32(&mut self, offset: u32, val: u32) {
        // DI0–DI3 (0x30–0x3C): bit 31 is the interrupt-status bit — clear on write-1.
        let idx = offset & !3;
        if (0x30..=0x3C).contains(&idx) {
            // Clear status bit 31 if the guest writes 1 to it.
            let current = self.read32(idx);
            self.write32(idx, current & !(val & 0x8000_0000));
            // Store the non-status fields normally.
            self.write32(idx, (self.read32(idx) & 0x8000_0000) | (val & !0x8000_0000));
        } else {
            self.write32(idx, val);
        }
    }

    /// Returns `true` if any of the four VI DisplayInterrupt sources (DI0–DI3)
    /// currently has both its status bit (bit 31) and enable bit (bit 28) set.
    ///
    /// Mirrors the native `get_active_interrupts` check:
    ///   `video |= i.enable() && i.status()` for each display interrupt.
    pub(crate) fn any_display_interrupt_active(&self) -> bool {
        for idx in [0x30u32, 0x34, 0x38, 0x3C] {
            let reg = self.read32(idx);
            let status = (reg >> 31) & 1 != 0;
            let enable = (reg >> 28) & 1 != 0;
            if status && enable {
                return true;
            }
        }
        false
    }

    /// Write a 16-bit value to the VI register at `offset` bytes from `VI_BASE`.
    pub(crate) fn write_u16(&mut self, offset: u32, val: u16) {
        let idx = offset & !3;
        let current = self.read32(idx);
        let merged = if (offset & 2) == 0 {
            // Upper halfword
            (current & 0x0000_FFFF) | ((val as u32) << 16)
        } else {
            // Lower halfword
            (current & 0xFFFF_0000) | (val as u32)
        };
        self.write32(idx, merged);
    }

    // ── XFB address extraction ────────────────────────────────────────────────

    /// Physical RAM address of the top-field External Frame Buffer.
    ///
    /// Decodes the TFBL register (offset `0x1C` from `VI_BASE`):
    ///
    /// ```text
    ///   TFBL bits[23:0] = physical_address >> 5
    ///   physical_address = TFBL[23:0] << 5
    /// ```
    ///
    /// Returns `0` if the VI has not been programmed yet (TFBL == 0).
    pub(crate) fn xfb_addr(&self) -> u32 {
        let tfbl = self.read32(0x1C);   // TFBL is at offset 0x1C
        (tfbl & 0x00FF_FFFF) << 5
    }

    /// Vertical count from VCOUNT register (offset 0x2C upper halfword).
    #[allow(dead_code)]
    pub(crate) fn vcount(&self) -> u16 {
        self.read_u16(0x2C)
    }

    /// Increment the vertical counter by 1 (called once per frame).
    ///
    /// The counter wraps at 525 lines (NTSC 525-line frame).
    pub(crate) fn advance_vcount(&mut self) {
        let current = self.read_u16(0x2C) as u32;
        let next = (current + 1) % 525;
        // Write back upper halfword of the VCOUNT word at offset 0x2C.
        self.write_u16(0x2C, next as u16);
    }

    /// Fire any enabled VI DisplayInterrupt sources by setting their status bit
    /// (bit 31).
    ///
    /// On real hardware the VI asserts the display interrupt (and sets the
    /// per-source status bit) when VCOUNT reaches the line configured in each
    /// DI register.  The OS interrupt handler reads the DI status bits to
    /// determine which source fired, calls the registered retrace callback, and
    /// increments `VRetraceCnt` — which is what `VIWaitForRetrace()` polls.
    ///
    /// The emulator calls this once per animation frame (from
    /// `assert_vi_interrupt`) so the status bits are visible to the handler
    /// even though we don't simulate per-scanline timing.  Only DI registers
    /// with their enable bit (bit 28) already set are touched; unconfigured
    /// sources are left alone.
    pub(crate) fn fire_display_interrupts(&mut self) {
        for idx in [0x30u32, 0x34, 0x38, 0x3C] {
            let reg = self.read32(idx);
            if (reg >> 28) & 1 != 0 {
                // Enable bit is set — assert status (bit 31) to simulate the
                // hardware firing the interrupt at the configured scan line.
                self.write32(idx, reg | 0x8000_0000);
            }
        }
    }
}
