//! DVD Interface (DI) hardware register state.
//!
//! Mirrors the ten memory-mapped I/O registers at `0xCC006000–0xCC006027`.
//! This is the Rust-side counterpart of Play!'s `Js_DiscImageDeviceStream`:
//! the emulated disc controller that reads sectors from the stored ISO image
//! and DMAs them into guest RAM when the game issues a DVD Read command.

/// Physical base address of the GameCube DVD Interface (DI) registers.
///
/// The DI lives at MMIO offset 0x6000 from the GameCube's hardware base
/// (0x0C000000 uncached / 0xCC000000 cached), giving a virtual address of
/// `0xCC006000`.  See YAGCD §9.3 for the complete register map.
///
/// Note: the Processor Interface (PI) occupies offset 0x3000 (0xCC003000) and
/// must not be confused with the DVD Interface.
pub(crate) const DI_BASE: u32 = 0xCC00_6000;
/// Number of bytes covered by the DI register bank (10 × 4-byte registers).
pub(crate) const DI_SIZE: u32 = 0x28;

/// DVD Interface hardware register file.
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
pub(crate) struct DiState {
    /// DISTATUS (0x00): status and interrupt flags.
    pub(crate) status: u32,
    /// DICOVER (0x04): lid/cover state.  Bit 2 = 1 → cover closed (disc present).
    pub(crate) cover: u32,
    /// DICMDBUF0 (0x08): command word; bits 31–24 hold the command code.
    pub(crate) cmd_buf0: u32,
    /// DICMDBUF1 (0x0C): disc address for the DVD Read command, in 4-byte units.
    ///
    /// Real hardware (and ipl-hle) store `byte_offset >> 2` here; callers must
    /// shift left by 2 to recover the actual byte offset into the disc image.
    pub(crate) cmd_buf1: u32,
    /// DICMDBUF2 (0x10): second command parameter (reserved for most commands).
    pub(crate) cmd_buf2: u32,
    /// DIMAR (0x14): physical RAM destination for DMA transfers.
    pub(crate) dma_addr: u32,
    /// DILENGTH (0x18): number of bytes to transfer.
    pub(crate) dma_len: u32,
    /// DICR (0x1C): control register.  Writing bit 0 (TSTART) begins the transfer.
    pub(crate) control: u32,
    /// DIIMMBUF (0x20): immediate data returned by non-DMA commands.
    pub(crate) imm_buf: u32,
    /// DICFG (0x24): drive configuration.
    pub(crate) config: u32,
}

impl DiState {
    /// Read a 32-bit value from the DI register at `offset` bytes from `DI_BASE`.
    pub(crate) fn read_reg(&self, offset: u32) -> u32 {
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
    pub(crate) fn write_reg(&mut self, offset: u32, val: u32) -> bool {
        match offset {
            // DISTATUS: interrupt-status bits (1–3: DEINT, TCINT, BRKINT) are
            // write-1-to-clear; all other bits (mask enables, etc.) are R/W.
            0x00 => {
                const W1C: u32 = (1 << 1) | (1 << 2) | (1 << 3); // DEINT | TCINT | BRKINT
                self.status = (self.status & !(val & W1C)) | (val & !W1C);
            }
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
