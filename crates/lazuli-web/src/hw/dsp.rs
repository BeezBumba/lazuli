//! DSP Interface hardware register state — HLE mailbox and DMA stub.
//!
//! The DSP uses 16-bit registers accessed with `lhz`/`sth` (halfword
//! load/store) and some 32-bit registers accessed with `lwz`/`stw`.
//!
//! ## Mailbox boot sequence (HLE)
//!
//! The OS boots the DSP by:
//! 1. Writing a boot-code address to the CPU→DSP mailbox (`0xCC005000`/`0xCC005002`)
//!    and polling bit 15 until the DSP clears it (indicating receipt).
//! 2. Un-halting the DSP (clearing bit 2 of `DSPCONTROL` at `0xCC00500A`).
//! 3. Polling bit 15 of the DSP→CPU mailbox (`0xCC005004`) until the DSP sets it,
//!    indicating the boot ROM has completed initialisation.
//!
//! HLE stubs:
//! - Reads from `0xCC005000`/`0xCC005002` (CSMAILBOX) always return 0 so "poll
//!   until bit 15 clears" loops exit immediately.
//! - Writes to CSMAILBOX are echoed to the DSP→CPU mailbox buffer.
//! - When `DSPCONTROL` bit 2 (halt) transitions 1→0 (OS un-halts the DSP), bit 15
//!   of `DSMAILBOX_H` is immediately set to simulate the DSP boot-ready signal.
//!   This unblocks the `__OSInitAudioSystem` / `OSInitAram` polling loop at
//!   `0xCC005004`.
//!
//! ## Audio DMA (AI DMA via DSP register space)
//!
//! The OS starts PCM audio DMA by writing to `AudioDmaBase` (`0xCC005030`,
//! 32-bit) and `AudioDmaControl` (`0xCC005036`, 16-bit, bit 15 = playing).
//! After starting the DMA the OS polls `DSPCONTROL.ai_dma_interrupt` (bit 3
//! at `0xCC00500A`) with EE=0.  Without a real DSP the interrupt bit is never
//! set and the game hangs forever in `__OSInitAudioSystem`.
//!
//! HLE stub: writing `AudioDmaControl` with bit 15 = 1 immediately sets bit 3
//! (`ai_dma_interrupt`) in `DSPCONTROL`.  `AudioDmaRemaining` (`0xCC00503A`)
//! always reads as 0 (DMA instantly complete).
//!
//! ## ARAM DMA (HLE)
//!
//! The OS probes ARAM expansion by initiating a DMA via `DspAramDmaControl`
//! (`0xCC005028`, 32-bit) and then polling `DSPCONTROL.aram_dma_interrupt`
//! (bit 5 at `0xCC00500A`) with EE=0.  HLE stub: any write to the ARAM DMA
//! control register immediately sets bit 5 (`aram_dma_interrupt`) and clears
//! bit 9 (`aram_dma_ongoing`) in `DSPCONTROL`.
//!
//! ## Register map (offsets from `DSP_BASE = 0xCC005000`)
//!
//! | Offset | Width | Name              | Description                          |
//! |--------|-------|-------------------|--------------------------------------|
//! | 0x00   | 16    | CSMAILBOX_H       | CPU→DSP mailbox hi (W; HLE echo→0x04)|
//! | 0x02   | 16    | CSMAILBOX_L       | CPU→DSP mailbox lo (W; HLE echo→0x06)|
//! | 0x04   | 16    | DSMAILBOX_H       | DSP→CPU mailbox hi (R; HLE echo)     |
//! | 0x06   | 16    | DSMAILBOX_L       | DSP→CPU mailbox lo (R; HLE echo)     |
//! | 0x0A   | 16    | DSPCONTROL        | Control/status register (R/W)        |
//! | 0x12   | 16    | DspAramSize       | ARAM size (R/W; always 0 = no ARAM)  |
//! | 0x20   | 32    | DspAramDmaRamBase | ARAM DMA RAM base address (W)        |
//! | 0x24   | 32    | DspAramDmaAramBase| ARAM DMA ARAM base address (W)       |
//! | 0x28   | 32    | DspAramDmaControl | ARAM DMA control — write triggers HLE|
//! | 0x30   | 32    | AudioDmaBase      | AI DMA RAM base address (W)          |
//! | 0x36   | 16    | AudioDmaControl   | AI DMA control (bit 15 = playing)    |
//! | 0x3A   | 16    | AudioDmaRemaining | Remaining DMA bytes (R; always 0)    |
//!
//! ## DSPCONTROL bit layout (offset 0x0A)
//!
//! | Bit | Field               | Description                               |
//! |-----|---------------------|-------------------------------------------|
//! |  0  | reset               | DSP reset pulse (auto-clears)             |
//! |  1  | interrupt           | CPU→DSP external interrupt                |
//! |  2  | halt                | Halt DSP                                  |
//! |  3  | ai_dma_interrupt    | AI DMA block done — W1C, set by HLE       |
//! |  4  | ai_dma_int_mask     | AI DMA interrupt enable                   |
//! |  5  | aram_dma_interrupt  | ARAM DMA done — W1C, set by HLE           |
//! |  6  | aram_dma_int_mask   | ARAM DMA interrupt enable                 |
//! |  7  | dsp_interrupt       | DSP→CPU interrupt — W1C                   |
//! |  8  | dsp_int_mask        | DSP interrupt enable                      |
//! |  9  | aram_dma_ongoing    | Read-only; 1 while ARAM DMA is in flight  |
//! | 10  | unknown             | Unknown                                   |
//! | 11  | reset_high          | Reset vector select (1 = high vector)     |

/// Physical base address of the DSP Interface registers.
pub(crate) const DSP_BASE: u32 = 0xCC00_5000;
/// Byte span of the DSP register bank, covering through AudioDmaRemaining.
pub(crate) const DSP_SIZE: u32 = 0x40;

/// Mask of DSPCONTROL bits that are write-1-to-clear (interrupt pending flags).
const DSPCTRL_W1C: u16 = (1 << 3) | (1 << 5) | (1 << 7);
/// Mask of DSPCONTROL bits that are normally writable (not auto-clear or read-only).
/// Bits 1,2,4,6,8,10,11 — excludes bit 0 (auto-clear reset), bits 3/5/7 (W1C),
/// and bit 9 (read-only aram_dma_ongoing).
const DSPCTRL_NORMAL_WRITE: u16 = 0x0D56;
/// Default value of DSPCONTROL: bit 11 = reset_high (matches native `Control::default()`).
const DSPCTRL_DEFAULT: u16 = 1 << 11;

/// DSP Interface hardware register file (HLE stub).
pub(crate) struct DspState {
    /// DSP→CPU mailbox high/low — read from `0xCC005004`/`0xCC005006` (DSMAILBOX).
    /// Populated by echoing CPU→DSP writes so that any "poll for DSP ACK" loops
    /// at `0xCC005004` terminate without real DSP microcode.
    pub(crate) dsp2cpu_hi: u16,
    pub(crate) dsp2cpu_lo: u16,
    /// DSPCONTROL at offset **0x0A** (`0xCC00500A`).
    pub(crate) control: u16,
    /// AudioDmaControl at offset 0x36 (`0xCC005036`).
    /// Bit 15 = playing; bits 0–14 = length in 32-byte blocks.
    pub(crate) audio_dma_control: u16,
}

impl Default for DspState {
    fn default() -> Self {
        Self {
            dsp2cpu_hi: 0,
            dsp2cpu_lo: 0,
            control: DSPCTRL_DEFAULT,
            audio_dma_control: 0,
        }
    }
}

impl DspState {
    /// Read a 16-bit value from the DSP register at `offset` bytes from `DSP_BASE`.
    pub(crate) fn read_u16(&self, offset: u32) -> u16 {
        match offset {
            // CPU→DSP mailbox (0x00/0x02): always return 0 so that
            // "poll bit15 until it clears" boot loops exit immediately.
            0x00 | 0x02 => 0,
            // DSP→CPU mailbox (0x04/0x06): return the echo stored by writes.
            0x04 => self.dsp2cpu_hi,
            0x06 => self.dsp2cpu_lo,
            // DSPCONTROL at the CORRECT hardware offset 0x0A (0xCC00500A).
            0x0A => self.control,
            // AudioDmaControl: return the last written value.
            0x36 => self.audio_dma_control,
            // AudioDmaRemaining: HLE always returns 0 (DMA instantly complete).
            0x3A => 0,
            _ => 0,
        }
    }

    /// Write `val` to the DSP register at `offset` bytes from `DSP_BASE`.
    ///
    /// Key HLE behaviours:
    /// - **Mailbox echo**: writes to `0x00`/`0x02` (CPU→DSP) *and* `0x04`/`0x06`
    ///   are stored in the DSP→CPU mailbox so any poll at `0x04`/`0x06` sees a
    ///   response.
    /// - **DSPCONTROL** (`0x0A`): interrupt flags (bits 3, 5, 7) are
    ///   write-1-to-clear; bit 0 (reset) auto-clears; bit 9 (aram_dma_ongoing)
    ///   is read-only.
    /// - **ARAM DMA** (`0x28`): write immediately sets `aram_dma_interrupt`
    ///   (bit 5) and clears `aram_dma_ongoing` (bit 9).
    /// - **AudioDmaControl** (`0x36`): writing with bit 15 = 1 immediately sets
    ///   `ai_dma_interrupt` (bit 3) in DSPCONTROL.
    pub(crate) fn write_u16(&mut self, offset: u32, val: u16) {
        match offset {
            // CPU→DSP mailbox (hardware offset 0x00/0x02, CSMAILBOX): echo the
            // written value into the DSP→CPU mailbox (dsp2cpu_hi/lo) so that
            // any "poll bit15 of 0xCC005004 for DSP ACK" loop exits immediately.
            0x00 => self.dsp2cpu_hi = val,
            0x02 => self.dsp2cpu_lo = val,
            // DSP→CPU mailbox (hardware offset 0x04/0x06, DSMAILBOX): normally
            // only the DSP writes here, but accept writes to the same echo buffer
            // as a broad HLE stub for alternative boot sequences.
            0x04 => self.dsp2cpu_hi = val,
            0x06 => self.dsp2cpu_lo = val,
            // DSPCONTROL at hardware offset 0x0A (0xCC00500A).
            0x0A => {
                // Snapshot the halt bit (bit 2) before applying the write so we can
                // detect the 1→0 transition that un-halts the DSP.
                let halt_was_set = (self.control & (1 << 2)) != 0;
                // W1C: bits 3, 5, 7 clear when the guest writes 1 to them.
                self.control &= !(val & DSPCTRL_W1C);
                // Normal writable bits.
                self.control =
                    (self.control & !DSPCTRL_NORMAL_WRITE) | (val & DSPCTRL_NORMAL_WRITE);
                // Bit 0 (reset) is a self-clearing pulse — never persist it.

                // HLE DSP boot-completion: when the OS clears the halt bit (bit 2),
                // the real DSP begins executing its boot ROM and shortly thereafter
                // writes to DSMAILBOX setting bit 15 ("message available") to signal
                // readiness.  Simulate this instantly so any "poll DSMAILBOX bit 15"
                // loops (e.g. __OSInitAudioSystem) exit immediately.
                let halt_now_clear = (self.control & (1 << 2)) == 0;
                if halt_was_set && halt_now_clear {
                    self.dsp2cpu_hi |= 0x8000; // DSP→CPU mailbox status = message ready
                }
            }
            // ARAM DMA control high halfword (0x28 = offset of DspAramDmaControl).
            // Writing this triggers a DMA in hardware; HLE completes it instantly.
            0x28 => {
                self.control |= 1 << 5; // aram_dma_interrupt = 1
                self.control &= !(1 << 9); // aram_dma_ongoing = 0
            }
            // AudioDmaControl (0x36): bit 15 = playing.
            // Starting the DMA (bit 15 → 1) immediately fires the AI DMA
            // interrupt (DSPCONTROL bit 3) in the HLE stub.
            0x36 => {
                self.audio_dma_control = val;
                if val & 0x8000 != 0 {
                    self.control |= 1 << 3; // ai_dma_interrupt = 1
                }
            }
            _ => {}
        }
    }

    /// Handle 32-bit writes to DSP-space registers.
    ///
    /// Several DSP registers (ARAM DMA base/control, AudioDmaBase) are 32-bit
    /// and written with `stw`.  The 32-bit dispatch path in `hw_write_u32`
    /// delegates here for the DSP address range.
    pub(crate) fn write_u32(&mut self, offset: u32, _val: u32) {
        match offset {
            // DspAramDmaControl (0x28, 4 bytes): write triggers ARAM DMA.
            // HLE: immediately complete — set aram_dma_interrupt (bit 5),
            // clear aram_dma_ongoing (bit 9).
            0x28 => {
                self.control |= 1 << 5; // aram_dma_interrupt = 1
                self.control &= !(1 << 9); // aram_dma_ongoing = 0
            }
            // AudioDmaBase (0x30, 4 bytes): store address for DMA source.
            // HLE doesn't need the actual base; ignore the write.
            // DspAramDmaRamBase (0x20) / DspAramDmaAramBase (0x24): ignored.
            _ => {}
        }
    }
}
