//! External Interface (EXI) hardware register state.
//!
//! The EXI bus connects the GameCube CPU to external serial peripherals via
//! three independent channels:
//!
//! - **Channel 0** — IPL ROM / RTC / SRAM (device 1) and Memory Card A (device 0)
//! - **Channel 1** — Memory Card B (device 0)
//! - **Channel 2** — Microphone / AD16 (device 0)
//!
//! This module provides a minimal HLE stub that makes games think no memory
//! cards are inserted and no expansion devices are attached, while still
//! responding to SRAM reads with a plausible all-zero payload (the OS uses
//! SRAM byte 0 as a "boot complete" flag which games check during init).
//!
//! ## Register map (offset from `EXI_BASE = 0xCC006800`)
//!
//! Each channel has 5 × 4-byte registers at `channel * 0x14`:
//!
//! | Offset | Name           | Description                                         |
//! |--------|----------------|-----------------------------------------------------|
//! | +0x00  | EXICxCPR       | Channel Parameter Register                          |
//! | +0x04  | EXICxMAR       | DMA Memory Address                                  |
//! | +0x08  | EXICxLENGTH    | DMA Transfer Length                                 |
//! | +0x0C  | EXICxCR        | Control Register (TSTART, MODE, TYPE)               |
//! | +0x10  | EXICxDATA      | Immediate Data Register                             |
//!
//! ## Channel Parameter Register (CPR) bit layout
//!
//! | Bits   | Field           | Description                                    |
//! |--------|-----------------|------------------------------------------------|
//! | 0      | device_int_mask | Device interrupt enable                        |
//! | 1      | device_int      | Device interrupt pending (W1C)                 |
//! | 2      | xfer_int_mask   | Transfer-complete interrupt enable             |
//! | 3      | xfer_int        | Transfer-complete interrupt pending (W1C)      |
//! | 4–6    | clock_mult      | Clock multiplier                               |
//! | 7–9    | device_sel      | Chip-select: 001=dev0, 010=dev1, 100=dev2      |
//! | 10     | attach_int_mask | Device-attach interrupt enable                 |
//! | 11     | attach_int      | Device attached (W1C)                          |
//! | 12     | connected       | 1 = device present on bus                      |
//!
//! ## Control Register (CR) bit layout
//!
//! | Bits | Field  | Description                                           |
//! |------|--------|-------------------------------------------------------|
//! | 0    | TSTART | Write 1 to start a transfer (self-clears on complete) |
//! | 1    | DMA    | 0 = immediate, 1 = DMA transfer                      |
//! | 2–3  | RW     | 0 = read, 1 = write, 2 = read-write                  |
//! | 4–7  | len    | Transfer length – 1 (0–3 → 1–4 bytes immediate)      |

/// Physical base address of the External Interface registers.
pub(crate) const EXI_BASE: u32 = 0xCC00_6800;
/// Byte span of the EXI register block (3 channels × 0x14 bytes = 0x3C bytes).
pub(crate) const EXI_SIZE: u32 = 0x40;

/// Per-channel EXI register state.
#[derive(Default, Clone, Copy)]
struct ExiChannel {
    /// EXICxCPR — Channel Parameter Register.
    param: u32,
    /// EXICxMAR — DMA Memory Address.
    dma_base: u32,
    /// EXICxLENGTH — DMA Transfer Length.
    dma_length: u32,
    /// EXICxCR — Control Register.
    control: u32,
    /// EXICxDATA — Immediate Data Register.
    data: u32,
}

/// External Interface hardware register file (3 channels).
pub(crate) struct ExiState {
    channels: [ExiChannel; 3],
    /// 64-byte stub SRAM (all zeroes; games fall back to defaults when read).
    sram: [u8; 64],
}

impl Default for ExiState {
    fn default() -> Self {
        Self {
            channels: [ExiChannel::default(); 3],
            sram: [0u8; 64],
        }
    }
}

impl ExiState {
    /// Read a 32-bit value from the EXI register at `offset` bytes from `EXI_BASE`.
    pub(crate) fn read_u32(&self, offset: u32) -> u32 {
        let (ch, reg) = channel_and_reg(offset);
        let Some(ch) = ch else { return 0 };
        match reg {
            0x00 => self.channels[ch].param,
            0x04 => self.channels[ch].dma_base,
            0x08 => self.channels[ch].dma_length,
            0x0C => self.channels[ch].control,
            0x10 => self.channels[ch].data,
            _ => 0,
        }
    }

    /// Write a 32-bit value to the EXI register at `offset` bytes from `EXI_BASE`.
    ///
    /// When a transfer is started (TSTART set in CR), the module immediately
    /// completes it (auto-responds with stub data), then clears TSTART and sets
    /// the transfer-complete interrupt bit in CPR.
    pub(crate) fn write_u32(&mut self, offset: u32, val: u32) {
        let (ch, reg) = channel_and_reg(offset);
        let Some(ch) = ch else { return };
        match reg {
            0x00 => {
                // CPR: write-1-to-clear interrupt bits (1 = device_int, 3 = xfer_int, 11 = attach_int).
                let w1c_mask = (1 << 1) | (1 << 3) | (1 << 11);
                let cleared = self.channels[ch].param & !(val & w1c_mask);
                // Merge non-W1C fields from the new value.
                self.channels[ch].param = (cleared & w1c_mask) | (val & !w1c_mask);
            }
            0x04 => self.channels[ch].dma_base   = val,
            0x08 => self.channels[ch].dma_length  = val,
            0x0C => {
                self.channels[ch].control = val;
                if (val & 1) != 0 {
                    // TSTART: immediately complete the transfer.
                    self.handle_transfer(ch, val);
                }
            }
            0x10 => self.channels[ch].data = val,
            _ => {}
        }
    }

    /// Handle an EXI transfer request for `channel`.
    ///
    /// For the SRAM/RTC device (channel 0, device_sel bit 1 = 0b010), the
    /// first immediate write sets an address then reads from the stub SRAM.
    /// All other devices return zeroes (no card / no device).
    fn handle_transfer(&mut self, ch: usize, cr_val: u32) {
        // Decode transfer type: bits 2–3 of CR.
        let rw    = (cr_val >> 2) & 0x3;
        // Decode byte count: bits 4–7, value+1 bytes (for immediate mode).
        let bytes = (((cr_val >> 4) & 0xF) + 1) as usize;

        // For immediate read transfers (rw == 0), return data from stub SRAM
        // if channel 0 and the SRAM device is selected (device_sel = 0b010).
        let device_sel = (self.channels[ch].param >> 7) & 0x7;
        if ch == 0 && device_sel == 0b010 && rw == 0 {
            // The data register's high byte is used as an SRAM address by the OS;
            // return the byte at that address (all zeroes from the stub SRAM).
            let sram_addr = ((self.channels[ch].data >> 24) & 0x3F) as usize;
            let mut result = 0u32;
            for i in 0..bytes.min(4) {
                let byte = if sram_addr + i < self.sram.len() { self.sram[sram_addr + i] } else { 0 };
                result = (result << 8) | byte as u32;
            }
            // Left-align the result in the 32-bit data register.
            self.channels[ch].data = result << (32 - 8 * bytes);
        }

        // Complete transfer: clear TSTART, set XFER_INT.
        self.channels[ch].control &= !1; // clear TSTART
        self.channels[ch].param |= 1 << 3; // set XFER_INT
    }
}

/// Decode an EXI register offset into `(Some(channel_index), register_offset)`.
///
/// Each channel occupies 0x14 bytes starting at `channel * 0x14`.
#[inline]
fn channel_and_reg(offset: u32) -> (Option<usize>, u32) {
    let ch = (offset / 0x14) as usize;
    if ch < 3 {
        (Some(ch), offset % 0x14)
    } else {
        (None, 0)
    }
}
