//! External Interface (EXI) hardware register state.
//!
//! The EXI bus connects the GameCube CPU to external serial peripherals via
//! three independent channels:
//!
//! - **Channel 0** — IPL ROM / RTC / SRAM (device 1) and Memory Card A (device 0)
//! - **Channel 1** — Memory Card B (device 0)
//! - **Channel 2** — Microphone / AD16 (device 0)
//!
//! This module provides a minimal HLE stub that responds correctly to SRAM
//! reads (including DMA mode), RTC reads/writes, and UART (IPL debug port)
//! writes.  Memory-card probes receive "no card inserted" responses.
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

// ─── Memory card (EXI channel 0 slot A / channel 1 slot B) ───────────────────

/// Byte capacity of a simulated Memory Card 59 (512 KiB = 59 usable 8 KiB blocks).
const MC_SIZE: usize = 512 * 1024;

/// Device ID returned when the memory card chip-select is asserted and the host
/// performs a read without sending a command first.  Left-aligned in the 32-bit
/// EXI DATA register:
///   Byte 0 = 0xC2  (Nintendo EXI manufacturer ID)
///   Byte 1 = 0x04  (512 KiB / Memory Card 59 type code)
///   Bytes 2–3 = 0x00 0x00 (not transmitted, transfer is 2 bytes)
const MC_DEVICE_ID: u32 = 0xC204_0000;

/// Memory card command bytes (sent MSB-first in the EXI DATA register).
const MC_CMD_READ: u8 = 0x52;  // Read 128-byte sector
const MC_CMD_WRITE: u8 = 0xF1; // Write 128-byte sector
const MC_CMD_ERASE: u8 = 0xF2; // Erase 16 KiB block (128 sectors)

/// Phase of the memory card EXI state machine.
#[derive(Default, Clone, Copy, PartialEq)]
enum McPhase {
    /// Idle: awaiting a command byte or a device-ID probe read.
    #[default]
    Idle,
    /// Received command byte; accumulating the 3-byte byte address.
    GotCmd,
    /// Address complete; performing the read, write, or erase data transfer.
    DataXfer,
}

/// State for one EXI memory card slot (slot A on channel 0, slot B on channel 1).
///
/// The GameCube memory card protocol uses the EXI bus in full-duplex SPI mode.
/// Commands, addresses, and data are sent/received as separate EXI immediate
/// transfers (each triggered by writing TSTART to the CR register) while the
/// chip-select (device_sel field in CPR) remains asserted.
///
/// ## Transfer sequence for a sector read (command 0x52):
/// ```text
/// CS↓  → TX: 0x52 (1 byte)
///       → TX: addr[23:16] (1 byte)
///       → TX: addr[15:8]  (1 byte)
///       → TX: addr[7:0]   (1 byte)
///       ← RX: sector data (128 bytes)
/// CS↑
/// ```
///
/// The 128-byte sectors are addressed in bytes; the caller must align the
/// address to a 128-byte boundary.
pub(crate) struct MemCard {
    /// Raw 512 KiB flash data.  Defaults to all `0xFF` (erased flash).
    pub(crate) data: Vec<u8>,
    phase:      McPhase,
    cmd:        u8,
    addr:       u32,  // accumulated 24-bit byte address
    addr_bytes: u8,   // 0–3: number of address bytes received
    xfer_pos:   u32,  // byte offset within the current sector transfer
}

impl Default for MemCard {
    fn default() -> Self {
        Self {
            data: vec![0xFF; MC_SIZE],
            phase: McPhase::Idle,
            cmd: 0,
            addr: 0,
            addr_bytes: 0,
            xfer_pos: 0,
        }
    }
}

impl MemCard {
    /// Process one EXI immediate transfer directed at this memory card.
    ///
    /// - `rw`:      0 = read from card, 1 = write to card
    /// - `bytes`:   number of bytes transferred (1–4)
    /// - `data_in`: the value in the DATA register for write transfers
    ///              (bytes are left-aligned: byte 0 in bits 31–24).
    ///
    /// Returns the value to place in the DATA register for read transfers.
    pub(crate) fn handle(&mut self, rw: u32, bytes: usize, data_in: u32) -> u32 {
        match self.phase {
            McPhase::Idle => {
                if rw == 0 {
                    // No prior command: the host is probing the device ID.
                    return MC_DEVICE_ID;
                }
                // First write byte is the command.
                self.cmd = (data_in >> 24) as u8;
                self.addr = 0;
                self.addr_bytes = 0;
                self.xfer_pos = 0;
                self.phase = McPhase::GotCmd;
                0
            }

            McPhase::GotCmd => {
                if rw == 1 {
                    // Accumulate address bytes from the MSB of the DATA register.
                    for i in 0..(bytes.min(3usize.saturating_sub(self.addr_bytes as usize))) {
                        let byte = (data_in >> (24u32 - i as u32 * 8)) as u8;
                        self.addr = (self.addr << 8) | byte as u32;
                        self.addr_bytes += 1;
                    }
                    if self.addr_bytes >= 3 {
                        self.xfer_pos = 0;
                        self.phase = McPhase::DataXfer;
                    }
                }
                0
            }

            McPhase::DataXfer => {
                let card_addr = (self.addr as usize).min(MC_SIZE.saturating_sub(1));

                match self.cmd {
                    MC_CMD_READ => {
                        // Return `bytes` bytes of card data, left-aligned.
                        let mut result = 0u32;
                        for i in 0..bytes {
                            let pos = (card_addr + self.xfer_pos as usize + i) % MC_SIZE;
                            result = (result << 8) | self.data[pos] as u32;
                        }
                        self.xfer_pos += bytes as u32;
                        // Left-align the result in the 32-bit DATA register.
                        result << (32u32.saturating_sub(bytes as u32 * 8))
                    }

                    MC_CMD_WRITE => {
                        // Store `bytes` bytes into card data.
                        for i in 0..bytes {
                            let pos = (card_addr + self.xfer_pos as usize + i) % MC_SIZE;
                            let byte = (data_in >> (24u32.saturating_sub(i as u32 * 8))) as u8;
                            self.data[pos] = byte;
                        }
                        self.xfer_pos += bytes as u32;
                        0
                    }

                    MC_CMD_ERASE => {
                        // Erase the 16 KiB block containing `card_addr` to 0xFF.
                        const ERASE_SIZE: usize = 16 * 1024;
                        let block_off = card_addr & !(ERASE_SIZE - 1);
                        let end = (block_off + ERASE_SIZE).min(MC_SIZE);
                        self.data[block_off..end].fill(0xFF);
                        self.phase = McPhase::Idle;
                        0
                    }

                    _ => 0,
                }
            }
        }
    }

    /// Reset the state machine.
    ///
    /// Called when the chip-select for this card is deasserted (i.e., when
    /// the EXI `device_sel` field transitions away from the card's device slot).
    pub(crate) fn reset_phase(&mut self) {
        self.phase = McPhase::Idle;
        self.addr_bytes = 0;
        self.xfer_pos = 0;
    }
}

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

/// Pending DMA request returned when `handle_transfer` performs a DMA-mode
/// SRAM read.  The caller (`hw_write_u32`) has access to the full RAM buffer
/// and executes the copy there.
pub(crate) struct ExiSramDma {
    /// Physical RAM destination address.
    pub(crate) ram_addr: u32,
    /// Number of bytes to copy.
    pub(crate) length: u32,
    /// SRAM byte offset to read from.
    pub(crate) sram_offset: usize,
}

/// External Interface hardware register file (3 channels).
pub(crate) struct ExiState {
    channels: [ExiChannel; 3],
    /// Previous device_sel value for each channel (used to detect CS transitions).
    prev_device_sel: [u8; 3],
    /// Memory card in EXI channel 0 slot A (device_sel = 0b001).
    pub(crate) mc_a: MemCard,
    /// Memory card in EXI channel 1 slot A (device_sel = 0b001).
    pub(crate) mc_b: MemCard,
    /// 64-byte stub SRAM.
    ///
    /// Initialised with sensible GameCube defaults:
    /// - Byte 0x11 (NTD): bit 2 = stereo sound (0x04), NTSC (bits 0-1 = 0).
    /// - Byte 0x12 (language): 0x00 = English.
    /// - Byte 0x13 (flags): 0x6C (fixed boot-complete marker).
    ///
    /// Checksums at bytes 0x00–0x03 are recomputed before every SRAM read
    /// (matching the native `update_sram_checksum` path).
    ///
    /// Made `pub(crate)` so `WasmEmulator::get_sram` / `set_sram` can provide
    /// JavaScript with persistent access (stored in `localStorage`).
    pub(crate) sram: [u8; 64],
    /// GameCube RTC value — seconds since 2000-01-01 00:00:00 UTC.
    ///
    /// Initialised from the browser clock on first RTC read.  Games write the
    /// RTC at boot to set it; subsequent reads return the stored value.
    pub(crate) rtc: u32,
    /// True after the IPL/UART device (channel 0, device_sel=0b010) receives
    /// the UART-write command (`0xA001_0000`).  Subsequent writes on that
    /// channel are treated as UART data bytes rather than new commands.
    uart_pending: bool,
    /// Bytes emitted via the EXI UART (channel 0, IPL chip) since the last
    /// call to [`ExiState::take_uart_output`].
    uart_output: Vec<u8>,
}

/// Difference in seconds between the GameCube RTC epoch (2000-01-01 UTC)
/// and the Unix epoch (1970-01-01 UTC).
const GC_EPOCH_OFFSET_S: f64 = 946_684_800.0;

impl Default for ExiState {
    fn default() -> Self {
        let mut state = Self {
            channels: [ExiChannel::default(); 3],
            prev_device_sel: [0u8; 3],
            mc_a: MemCard::default(),
            mc_b: MemCard::default(),
            sram: [0u8; 64],
            rtc: 0,
            uart_pending: false,
            uart_output: Vec::new(),
        };
        // Set sensible GameCube SRAM defaults.
        // Byte 0x11 (NTD): bit 2 = stereo sound.
        state.sram[0x11] = 0x04;
        // Byte 0x12: language = 0x00 (English).
        state.sram[0x12] = 0x00;
        // Byte 0x13: fixed boot-complete marker (forced to 0x6C by checksum code).
        state.sram[0x13] = 0x6C;
        state
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
    /// Returns `Some(ExiSramDma)` when a DMA-mode SRAM read is requested so
    /// the caller (which has access to the full RAM buffer) can execute the
    /// copy.  Returns `None` for all other transfers.
    ///
    /// When a transfer is started (TSTART set in CR), the module immediately
    /// completes it (auto-responds with stub data), then clears TSTART and sets
    /// the transfer-complete interrupt bit in CPR.
    pub(crate) fn write_u32(&mut self, offset: u32, val: u32) -> Option<ExiSramDma> {
        let (ch, reg) = channel_and_reg(offset);
        let Some(ch) = ch else { return None };
        match reg {
            0x00 => {
                // CPR: detect chip-select transitions before applying the write.
                let old_sel = self.prev_device_sel[ch];
                let new_sel = ((val >> 7) & 0x7) as u8;
                if old_sel != new_sel {
                    // CS deasserted for old device — reset its state machine.
                    match (ch, old_sel) {
                        (0, 0b001) => self.mc_a.reset_phase(),
                        (1, 0b001) => self.mc_b.reset_phase(),
                        _ => {}
                    }
                }
                self.prev_device_sel[ch] = new_sel;

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
                    return self.handle_transfer(ch, val);
                }
            }
            0x10 => self.channels[ch].data = val,
            _ => {}
        }
        None
    }

    /// Drain and return all EXI UART output bytes accumulated since the last
    /// call.  The internal buffer is cleared on each call.
    ///
    /// JavaScript should call this after every emulated block and pipe the
    /// returned bytes through the same `stdoutLineBuffer` → `appendApploaderLog`
    /// pipeline used for ipl-hle `0xCC007000` writes.
    pub(crate) fn take_uart_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.uart_output)
    }

    /// Recompute the SRAM integrity checksums, mirroring `update_sram_checksum`
    /// in the native `exi.rs`.
    ///
    /// The native IPL ROM writes a pair of 16-bit checksums to SRAM bytes
    /// 0x00–0x01 (c1) and 0x02–0x03 (c2) before handing control to the OS.
    /// `c1` is the wrapping sum of the four 16-bit words at SRAM offsets
    /// 0x0C–0x13; `c2` is the wrapping sum of their bitwise complements.
    /// Byte 0x13 is forced to `0x6C` first, as the native does.
    ///
    /// Games / the Dolphin OS check these checksums to decide whether SRAM
    /// contains valid data; computing them here ensures those checks pass even
    /// when the rest of the SRAM is all-zero.
    fn update_sram_checksum(&mut self) {
        self.sram[0x13] = 0b0110_1100; // fixed value required by the native checksum algorithm

        let mut c1: u16 = 0;
        let mut c2: u16 = 0;
        for i in 0..4usize {
            let off = 0x0C + 2 * i;
            let word = u16::from_be_bytes([self.sram[off], self.sram[off + 1]]);
            c1 = c1.wrapping_add(word);
            c2 = c2.wrapping_add(word ^ 0xFFFF);
        }

        let [h1, l1] = c1.to_be_bytes();
        let [h2, l2] = c2.to_be_bytes();
        self.sram[0x00] = h1;
        self.sram[0x01] = l1;
        self.sram[0x02] = h2;
        self.sram[0x03] = l2;
    }

    /// Return the current GameCube RTC value.
    ///
    /// If `self.rtc == 0` (never been written), initialise it from the
    /// browser clock so games get a plausible timestamp.
    fn rtc_value(&mut self) -> u32 {
        if self.rtc == 0 {
            // js_sys::Date::now() returns milliseconds since Unix epoch (f64).
            let unix_ms = js_sys::Date::now();
            let gc_secs = ((unix_ms / 1000.0) - GC_EPOCH_OFFSET_S).max(0.0) as u32;
            self.rtc = gc_secs;
        }
        self.rtc
    }

    /// Decode the EXI command type from the DATA register value and dispatch
    /// to the appropriate handler.
    ///
    /// The DATA register contains the IPL/SRAM/RTC command word when a
    /// transfer starts.  The encoding (from native `ipl_rtc_sram_transfer`):
    ///
    /// | DATA value range         | Command                         |
    /// |--------------------------|---------------------------------|
    /// | `0x2000_0000`            | RTC read (exact)                |
    /// | `0x2000_0100..0x2001_00` | SRAM read at computed offset    |
    /// | `0x2001_0000`            | UART read (ignored)             |
    /// | `0xA000_0000`            | RTC write                       |
    /// | `0xA000_0100..0xA001_00` | SRAM write at computed offset   |
    /// | `0xA001_0000`            | UART write mode enable          |
    ///
    /// The SRAM byte offset is: `((data & !0xA000_0000) - 0x100) >> 6`
    ///
    /// Returns `Some(ExiSramDma)` for DMA-mode SRAM reads, `None` otherwise.
    fn handle_transfer(&mut self, ch: usize, cr_val: u32) -> Option<ExiSramDma> {
        // Decode transfer type: bits 2–3 of CR.
        let rw    = (cr_val >> 2) & 0x3;
        // Decode byte count: bits 4–7, value+1 bytes (for immediate mode).
        let bytes = (((cr_val >> 4) & 0xF) + 1) as usize;
        // Bit 1 of CR: DMA mode (0 = immediate, 1 = DMA).
        let dma   = (cr_val >> 1) & 0x1;

        let device_sel = (self.channels[ch].param >> 7) & 0x7;
        let data = self.channels[ch].data;

        // ── Memory card (device_sel = 0b001, channels 0 and 1) ──────────────
        if device_sel == 0b001 {
            let mc = match ch {
                0 => &mut self.mc_a,
                1 => &mut self.mc_b,
                _ => {
                    self.channels[ch].control &= !1;
                    self.channels[ch].param |= 1 << 3;
                    return None;
                }
            };
            let result = mc.handle(rw, bytes, data);
            self.channels[ch].data = result;
            self.channels[ch].control &= !1; // clear TSTART
            self.channels[ch].param |= 1 << 3; // set XFER_INT
            return None;
        }

        let result = if ch == 0 && device_sel == 0b010 {
            // ── Channel 0, IPL/SRAM/RTC device ───────────────────────────
            match data {
                // ── RTC read: return seconds since GC epoch ───────────────
                0x2000_0000 if rw == 0 => {
                    self.uart_pending = false;
                    let rtc = self.rtc_value();
                    // Left-align RTC value in the immediate data register.
                    self.channels[ch].data = rtc;
                    None
                }
                // ── RTC write: store new timestamp ────────────────────────
                0xA000_0000 if rw == 1 => {
                    // In DMA mode the value to write is at the DMA address in
                    // RAM; in immediate mode it is in the DATA register itself.
                    // We cannot access RAM here for DMA writes, but the OS
                    // rarely writes the RTC in DMA mode, so just update rtc
                    // from the DATA register (matches immediate-mode writes
                    // from the IPL ROM).
                    self.rtc = data;
                    None
                }
                // ── SRAM read ─────────────────────────────────────────────
                cmd if (0x2000_0100..=0x2001_00FF).contains(&cmd) && rw == 0 => {
                    self.uart_pending = false;
                    self.update_sram_checksum();
                    // Compute SRAM byte offset from the command word.
                    // Formula from native `sram_transfer_read`:
                    //   sram_base = ((cmd & !0xA000_0000) - 0x100) >> 6
                    let sram_offset =
                        (((cmd & !0xA000_0000).wrapping_sub(0x100)) >> 6) as usize;

                    if dma == 0 {
                        // Immediate mode: fill data register with up to 4 SRAM bytes.
                        let mut result = 0u32;
                        for i in 0..bytes.min(4) {
                            let b = self.sram.get(sram_offset + i).copied().unwrap_or(0);
                            result = (result << 8) | b as u32;
                        }
                        // Left-align the result.
                        self.channels[ch].data = result << (32 - 8 * bytes);
                        None
                    } else {
                        // DMA mode: request that the caller copies SRAM → RAM.
                        let ram_addr = self.channels[ch].dma_base;
                        let length   = self.channels[ch].dma_length;
                        Some(ExiSramDma { ram_addr, length, sram_offset })
                    }
                }
                // ── SRAM write (immediate and DMA) ────────────────────────
                cmd if (0xA000_0100..=0xA001_00FF).contains(&cmd) && rw == 1 => {
                    let sram_offset =
                        (((cmd & !0xA000_0000).wrapping_sub(0x100)) >> 6) as usize;
                    if dma == 0 {
                        // Immediate mode: write up to 4 bytes into SRAM.
                        let data_be = data.to_be_bytes();
                        for i in 0..bytes.min(4) {
                            if sram_offset + i < self.sram.len() {
                                self.sram[sram_offset + i] = data_be[i];
                            }
                        }
                    }
                    // DMA SRAM write is rare and requires RAM access; skip.
                    None
                }
                // ── UART write mode enable ────────────────────────────────
                0xA001_0000 if rw == 1 => {
                    self.uart_pending = true;
                    None
                }
                // ── UART data bytes ───────────────────────────────────────
                _ if rw == 1 && self.uart_pending => {
                    let data_bytes = data.to_be_bytes();
                    for &byte in &data_bytes[..bytes.min(4)] {
                        if byte != 0x1B && byte != 0x00 {
                            self.uart_output.push(byte);
                        }
                    }
                    None
                }
                _ => None,
            }
        } else {
            None
        };

        // Complete transfer: clear TSTART, set XFER_INT in CPR.
        self.channels[ch].control &= !1; // clear TSTART
        self.channels[ch].param |= 1 << 3; // set XFER_INT

        result
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
