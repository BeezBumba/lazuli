//! Processor Interface (PI) hardware register constants.
//!
//! ## PI interrupt-status register (PI_INTSR at 0xCC003000) bit layout
//!
//! | Bit | Value      | Source              |
//! |-----|------------|---------------------|
//! |  0  | 0x00000001 | GP (GX FIFO error)  |
//! |  1  | 0x00000002 | RSW (Reset Switch)  |
//! |  2  | 0x00000004 | DI (DVD Interface)  |
//! |  3  | 0x00000008 | SI (Serial If.)     |
//! |  4  | 0x00000010 | EXI (External If.)  |
//! |  5  | 0x00000020 | AI (Audio If.)      |
//! |  6  | 0x00000040 | DSP Interface       |
//! |  7  | 0x00000080 | MI (Memory If.)     |
//! |  8  | 0x00000100 | VI (Video If.)      |
//! |  9  | 0x00000200 | PE Token            |
//! | 10  | 0x00000400 | PE Finish           |
//! | 11  | 0x00000800 | CP (Cmd Processor)  |
//! | 12  | 0x00001000 | DEBUG               |
//! | 13  | 0x00002000 | HSP (Hi-Speed Port) |

/// Physical base address of the Processor Interface registers.
pub(crate) const PI_BASE: u32 = 0xCC00_3000;
/// Number of bytes covered by the PI register bank.
pub(crate) const PI_SIZE: u32 = 0x40;

// ─── Individual PI interrupt source bits ─────────────────────────────────────

/// Bit 0: GX FIFO / GP error.
pub(crate) const PI_INT_GP:  u32 = 0x0000_0001;
/// Bit 1: Reset Switch.
pub(crate) const PI_INT_RSW: u32 = 0x0000_0002;
/// Bit 2: DVD Interface (DI) transfer complete.
pub(crate) const PI_INT_DI:  u32 = 0x0000_0004;
/// Bit 3: Serial Interface (SI) transfer complete.
pub(crate) const PI_INT_SI:  u32 = 0x0000_0008;
/// Bit 4: External Interface (EXI) transfer complete.
pub(crate) const PI_INT_EXI: u32 = 0x0000_0010;
/// Bit 5: Audio Interface (AI) sample-counter interrupt.
pub(crate) const PI_INT_AI:  u32 = 0x0000_0020;
/// Bit 6: DSP Interface mailbox / ARAM DMA complete.
pub(crate) const PI_INT_DSP: u32 = 0x0000_0040;
/// Bit 7: Memory Interface (MI) protection fault.
pub(crate) const PI_INT_MI:  u32 = 0x0000_0080;
/// Bit 8: Video Interface (VI) vertical retrace.
///
/// Corrected from the former erroneous value of `0x80` (bit 7 = MI) to
/// `0x100` (bit 8 = VI), matching Dolphin's `INT_CAUSE_VI = 0x00000100` and
/// the GameCube SDK's `PI_INTERRUPT_VI` constant.
pub(crate) const PI_INT_VI:  u32 = 0x0000_0100;
/// Bit 9: Pixel Engine token interrupt.
pub(crate) const PI_INT_PE_TOKEN:  u32 = 0x0000_0200;
/// Bit 10: Pixel Engine finish interrupt.
pub(crate) const PI_INT_PE_FINISH: u32 = 0x0000_0400;
/// Bit 11: Command Processor (CP) interrupt.
pub(crate) const PI_INT_CP:  u32 = 0x0000_0800;
/// Bit 12: DEBUG interrupt.
pub(crate) const PI_INT_DEBUG: u32 = 0x0000_1000;
/// Bit 13: High-Speed Port (HSP) interrupt.
pub(crate) const PI_INT_HSP: u32 = 0x0000_2000;

/// Value returned from PI_MEMSIZE (0xCC003028): 24 MiB of main RAM.
pub(crate) const PI_MEMSIZE_VAL: u32 = 24 * 1024 * 1024;

/// Value returned from PI_BUSCLK (0xCC00302C): GameCube bus clock in Hz.
pub(crate) const PI_BUSCLK_VAL: u32 = 162_000_000;

/// Value returned from PI_CPUCLK (0xCC003030): GameCube CPU clock in Hz.
pub(crate) const PI_CPUCLK_VAL: u32 = 486_000_000;
