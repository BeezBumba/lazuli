//! DSP Interface hardware register state — HLE mailbox stub.
//!
//! The DSP uses 16-bit registers accessed with `lhz`/`sth` (halfword
//! load/store).  The OS boots the DSP by writing a boot-code address to the
//! CPU→DSP mailbox (`0xCC005004`/`0xCC005006`) and then polling the DSP→CPU
//! mailbox (`0xCC005000`/`0xCC005002`) until it sees a non-zero ACK.
//!
//! Without a real DSP implementation the OS would spin forever in
//! `__OSInitAudioSystem`.  [`DspState`] provides a minimal HLE stub: any
//! value written to the CPU→DSP mailbox is immediately echoed back to the
//! DSP→CPU mailbox, so the OS sees its own boot command as the ACK and
//! proceeds.

/// Physical base address of the DSP Interface registers.
pub(crate) const DSP_BASE: u32 = 0xCC00_5000;
/// Number of bytes covered by the DSP register bank (8 × 2-byte registers).
pub(crate) const DSP_SIZE: u32 = 0x10;

/// DSP Interface hardware register file.
///
/// All registers are 16-bit (halfword); they are accessed with `lhz`/`sth`
/// instructions by the OS.
///
/// ## Register map (offsets from `DSP_BASE`)
///
/// | Offset | Name        | Description                                |
/// |--------|-------------|--------------------------------------------|
/// | 0x00   | DSMAILBOX_H | DSP→CPU mailbox high 16 bits (R)           |
/// | 0x02   | DSMAILBOX_L | DSP→CPU mailbox low 16 bits (R)            |
/// | 0x04   | CSMAILBOX_H | CPU→DSP mailbox high 16 bits (W)           |
/// | 0x06   | CSMAILBOX_L | CPU→DSP mailbox low 16 bits (W)            |
/// | 0x08   | DSPCONTROL  | Control/status register (R/W)              |
#[derive(Default)]
pub(crate) struct DspState {
    /// DSP→CPU mailbox high 16 bits (returned on read from 0xCC005000).
    pub(crate) dsp2cpu_hi: u16,
    /// DSP→CPU mailbox low 16 bits (returned on read from 0xCC005002).
    pub(crate) dsp2cpu_lo: u16,
    /// DSP control/status register (read from 0xCC005008).
    pub(crate) control: u16,
}

impl DspState {
    /// Read a 16-bit value from the DSP register at `offset` bytes from `DSP_BASE`.
    pub(crate) fn read_u16(&self, offset: u32) -> u16 {
        match offset {
            0x00 => self.dsp2cpu_hi,
            0x02 => self.dsp2cpu_lo,
            0x08 => self.control,
            _ => 0,
        }
    }

    /// Write `val` to the DSP register at `offset` bytes from `DSP_BASE`.
    ///
    /// Writes to the CPU→DSP mailbox (`0x04`/`0x06`) are immediately echoed
    /// to the DSP→CPU mailbox (`0x00`/`0x02`) as a minimal HLE stub: the OS
    /// sends a boot command and polls for an ACK; by echoing we satisfy the
    /// poll without running real DSP microcode.
    ///
    /// Writes to DSPCONTROL (`0x08`) auto-clear bit 0 (DSP Reset), because
    /// in hardware the reset pulse is instantaneous and the bit self-clears
    /// once the DSP has been reset.  Without this, the OS's reset-complete
    /// polling loop would spin forever.
    pub(crate) fn write_u16(&mut self, offset: u32, val: u16) {
        match offset {
            // CPU→DSP mailbox: echo immediately to DSP→CPU (HLE DSP boot stub).
            0x04 => self.dsp2cpu_hi = val,
            0x06 => self.dsp2cpu_lo = val,
            // DSPCONTROL: store the value but auto-clear the Reset bit (bit 0)
            // so that the OS's "wait for reset complete" poll exits immediately.
            0x08 => self.control = val & !0x0001,
            _ => {}
        }
    }
}
