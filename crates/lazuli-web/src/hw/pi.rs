//! Processor Interface (PI) hardware register constants.

/// Physical base address of the Processor Interface registers.
pub(crate) const PI_BASE: u32 = 0xCC00_3000;
/// Number of bytes covered by the PI register bank.
pub(crate) const PI_SIZE: u32 = 0x40;

/// PI interrupt-status bit for the Video Interface (VI) retrace interrupt.
///
/// This is bit 7 of PI_INTSR/PI_INTMSK (value `0x80`), matching the GameCube
/// SDK constant `PI_INTERRUPT_VI`.
pub(crate) const PI_INT_VI: u32 = 0x0000_0080;

/// Value returned from PI_MEMSIZE (0xCC003028): 24 MiB of main RAM.
pub(crate) const PI_MEMSIZE_VAL: u32 = 24 * 1024 * 1024;

/// Value returned from PI_BUSCLK (0xCC00302C): GameCube bus clock in Hz.
pub(crate) const PI_BUSCLK_VAL: u32 = 162_000_000;

/// Value returned from PI_CPUCLK (0xCC003030): GameCube CPU clock in Hz.
pub(crate) const PI_CPUCLK_VAL: u32 = 486_000_000;
