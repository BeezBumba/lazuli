//! Audio Interface (AI) hardware register state.
//!
//! The AI DMA engine reads 32-byte blocks of audio samples from guest RAM and
//! passes them to the GameCube's DSP for mixing and output.  This module tracks
//! the AI control, volume, sample-counter, and interrupt-timing registers so
//! that games can start/stop audio DMA and query the playback position without
//! hanging.
//!
//! ## Audio pipeline (GameCube)
//!
//! ```text
//!   Main RAM ──[DMA]──► AI register file ──► DSP ARAM ──► DAC ──► speakers
//! ```
//!
//! The emulator does not yet implement the full audio pipeline; however,
//! maintaining these registers allows `AI_SCNT` to increment (simulated by
//! `tick_sample_counter`) so that games waiting on the counter can proceed.
//!
//! ## Register map (offsets from `AI_BASE = 0xCC006C00`)
//!
//! | Offset | Width | Name    | Description                                    |
//! |--------|-------|---------|------------------------------------------------|
//! | 0x00   | 32    | AICR    | Control Register                               |
//! | 0x04   | 32    | AIVR    | Volume Register (some SDK headers use this)    |
//! | 0x08   | 32    | AISCNT  | Sample Counter (read-only; increments with DMA)|
//! | 0x0C   | 32    | AIIT    | Interrupt Timing (set interrupt at this count) |
//!
//! ## AICR bit layout
//!
//! | Bit | Field       | Description                                      |
//! |-----|-------------|--------------------------------------------------|
//! | 0   | PSTAT       | 1 = DMA playing                                  |
//! | 1   | AISFR       | Aux sample rate: 0=48 kHz, 1=32 kHz             |
//! | 2   | AIINTMSK    | Interrupt enable (AIINT fires when SCNT==AIIT)   |
//! | 3   | AIINT       | Interrupt pending — write 1 to clear             |
//! | 4   | AIINTVLD    | 1 = AIIT value is valid                          |
//! | 5   | AIOCNT      | Sample-counter reset (self-clearing)             |
//! | 6   | AFR         | DSP sample rate: 0=48 kHz, 1=32 kHz             |

/// Physical base address of the Audio Interface registers.
pub(crate) const AI_BASE: u32 = 0xCC00_6C00;
/// Byte span of the AI register block (4 × 4-byte registers).
pub(crate) const AI_SIZE: u32 = 0x10;

/// Audio Interface hardware register file.
#[derive(Default)]
pub(crate) struct AiState {
    /// AICR — Control Register.
    pub(crate) control: u32,
    /// AIVR — Volume Register.
    pub(crate) volume: u32,
    /// AISCNT — Sample Counter (incremented per DMA block completed).
    pub(crate) sample_count: u32,
    /// AIIT — Interrupt Timing: fire AIINT when AISCNT reaches this value.
    pub(crate) interrupt_sample: u32,
}

impl AiState {
    /// Read a 32-bit value from the AI register at `offset` bytes from `AI_BASE`.
    pub(crate) fn read_u32(&self, offset: u32) -> u32 {
        match offset {
            0x00 => self.control,
            0x04 => self.volume,
            0x08 => self.sample_count,
            0x0C => self.interrupt_sample,
            _ => 0,
        }
    }

    /// Write a 32-bit value to the AI register at `offset` bytes from `AI_BASE`.
    ///
    /// Special handling:
    /// - **AICR bit 3 (AIINT)**: write-1-to-clear (acknowledged by the ISR).
    /// - **AICR bit 5 (AIOCNT)**: when set, resets the sample counter to 0
    ///   (self-clearing; the hardware pulses this bit for one cycle).
    pub(crate) fn write_u32(&mut self, offset: u32, val: u32) {
        match offset {
            0x00 => {
                // AIINT (bit 3): write-1-to-clear.
                let aiint_cleared = if (val & (1 << 3)) != 0 {
                    self.control & !(1 << 3) // clear interrupt bit
                } else {
                    self.control
                };
                // AIOCNT (bit 5): if written 1, reset sample counter and self-clear.
                if (val & (1 << 5)) != 0 {
                    self.sample_count = 0;
                }
                // Store AICR without the W1C and self-clearing bits.
                let keep_mask = !(1u32 << 3); // don't let guest set AIINT directly
                self.control = (aiint_cleared & !(1 << 5)) // clear AIOCNT
                    | (val & keep_mask & !(1 << 5)); // merge new fields
            }
            0x04 => self.volume          = val,
            0x08 => {} // AISCNT is read-only; writes are ignored.
            0x0C => self.interrupt_sample = val,
            _ => {}
        }
    }

    /// Advance the sample counter by `samples` and fire an interrupt if the
    /// timing threshold has been reached.
    ///
    /// Returns `true` when AIINT should be asserted (i.e., the caller must
    /// merge the AI interrupt bit into PI_INTSR and potentially deliver an
    /// external interrupt to the CPU).
    pub(crate) fn tick_sample_counter(&mut self, samples: u32) -> bool {
        let was_below = self.sample_count < self.interrupt_sample;
        self.sample_count = self.sample_count.wrapping_add(samples);
        let crossed = was_below && self.sample_count >= self.interrupt_sample;

        let int_enabled = (self.control & (1 << 2)) != 0; // AIINTMSK
        let int_valid   = (self.control & (1 << 4)) != 0; // AIINTVLD
        let playing     = (self.control & 1) != 0;        // PSTAT

        if playing && int_enabled && int_valid && crossed {
            self.control |= 1 << 3; // set AIINT
            return true;
        }
        false
    }

    /// Whether the DMA engine is currently running (PSTAT bit in AICR).
    #[allow(dead_code)]
    pub(crate) fn is_playing(&self) -> bool {
        (self.control & 1) != 0
    }
}
