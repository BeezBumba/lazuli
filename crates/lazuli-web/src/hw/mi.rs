//! Memory Interface (MI) hardware register stubs.
//!
//! The Memory Interface sits at `0xCC004000`–`0xCC00403F` and provides
//! memory-protection and memory-exception signalling on real hardware.  In
//! software emulation these functions are not needed; the stubs here exist
//! solely so that OS boot code that reads or writes MI registers does not
//! fall through to the silent default handler, which would cause subtle
//! breakage if the OS used the read-back value to decide whether protection
//! was enabled.
//!
//! ## Register map (offsets from `MI_BASE = 0xCC004000`)
//!
//! | Offset | Width | Name              | Description                        |
//! |--------|-------|-------------------|------------------------------------|
//! | 0x010  | 16    | MemoryProtection  | Memory-protection enable bits (R/W)|
//! | 0x01C  | 16    | MemoryIntMask     | Memory-interrupt enable mask (R/W) |
//! | 0x020  | 16    | MemoryInterrupt   | Memory-interrupt status (W1C)      |

/// Physical base address of the Memory Interface (MI) registers.
pub(crate) const MI_BASE: u32 = 0xCC00_4000;
/// Byte span of the MI register bank (covers through `MemoryInterrupt`).
pub(crate) const MI_SIZE: u32 = 0x40;

/// Memory Interface register file.
#[derive(Default)]
pub(crate) struct MiState {
    /// `MemoryProtection` (offset 0x010) — memory-range protection control.
    pub(crate) protection: u16,
    /// `MemoryIntMask` (offset 0x01C) — interrupt enable mask.
    pub(crate) int_mask: u16,
    /// `MemoryInterrupt` (offset 0x020) — interrupt status (W1C).
    pub(crate) interrupt: u16,
}

impl MiState {
    /// Read a 16-bit value from the MI register at `offset` bytes from `MI_BASE`.
    pub(crate) fn read_u16(&self, offset: u32) -> u16 {
        match offset {
            0x10 => self.protection,
            0x1C => self.int_mask,
            0x20 => self.interrupt,
            _ => 0,
        }
    }

    /// Write a 16-bit value to the MI register at `offset` bytes from `MI_BASE`.
    pub(crate) fn write_u16(&mut self, offset: u32, val: u16) {
        match offset {
            0x10 => self.protection = val,
            0x1C => self.int_mask = val,
            // MemoryInterrupt is write-1-to-clear: clear any bits where the
            // guest wrote a 1.
            0x20 => self.interrupt &= !val,
            _ => {}
        }
    }
}
