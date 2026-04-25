//! Serial Interface (SI) hardware register state and controller auto-response.
//!
//! The SI manages communication with up to 4 GameCube controller ports via a
//! proprietary serial protocol.  This module provides a full register file
//! and an HLE (high-level emulation) stub that automatically builds controller
//! responses when the game starts an SI transfer.
//!
//! ## Protocol overview
//!
//! 1. The game writes a command byte sequence to the SI output buffer for the
//!    target channel (addresses `0xCC006400/0x640C/0x6418/0x6424`).
//! 2. The game writes SICOMCSR (`0xCC006434`) with:
//!    - `TSTART` (bit 0) = 1 → begin transfer
//!    - `CHANNEL` (bits 1–2) = port number (0–3)
//!    - `INLEN-1` (bits 8–14) = expected response bytes minus 1
//!    - `OUTLEN-1` (bits 16–22) = command bytes minus 1
//! 3. On TSTART=1, this module immediately builds the response in the
//!    corresponding input buffer and sets `TRANSFER_INT` (bit 31 of SICOMCSR).
//! 4. The game reads the response from the input buffer for the channel
//!    (addresses `0xCC006404+0x6408`, `0x6410+0x6414`, etc.).
//!
//! ## Controller response formats
//!
//! **Command 0x00 — GetDeviceType (3-byte response):**
//! ```text
//!   [0x09, 0x00, 0x20]  — standard GameCube controller
//! ```
//!
//! **Command 0x40/0x41/0x42 — Poll / GetOrigin / Calibrate (8-byte response):**
//! ```text
//!   Byte 0: [0, 0, 0, START, Y, X, B, A]
//!   Byte 1: [1, L_dig, R_dig, Z, D-Up, D-Dn, D-Rt, D-Lt]
//!   Byte 2: Joy-X  (0–255, centre = 128)
//!   Byte 3: Joy-Y  (0–255, centre = 128; up = larger value)
//!   Byte 4: C-Stick X  (centre = 128)
//!   Byte 5: C-Stick Y  (centre = 128)
//!   Byte 6: L trigger analog  (0–255)
//!   Byte 7: R trigger analog  (0–255)
//! ```
//!
//! Port 0 reports the pad state from [`WasmEmulator::pad_buttons`]; ports 1–3
//! report "no controller" (GetDeviceType returns an error pattern, Poll returns
//! all-zeroes).
//!
//! ## Register map (offsets from `SI_BASE = 0xCC006400`)
//!
//! | Offset | Name          | Description                                    |
//! |--------|---------------|------------------------------------------------|
//! | 0x00   | SIOUTBUF0     | Channel 0 output buffer (command)              |
//! | 0x04   | SIINBUF0H     | Channel 0 input buffer high 32 bits            |
//! | 0x08   | SIINBUF0L     | Channel 0 input buffer low 32 bits             |
//! | 0x0C   | SIOUTBUF1     | Channel 1 output buffer                        |
//! | 0x10   | SIINBUF1H     | Channel 1 input buffer high 32 bits            |
//! | 0x14   | SIINBUF1L     | Channel 1 input buffer low 32 bits             |
//! | 0x18   | SIOUTBUF2     | Channel 2 output buffer                        |
//! | 0x1C   | SIINBUF2H     | Channel 2 input buffer high 32 bits            |
//! | 0x20   | SIINBUF2L     | Channel 2 input buffer low 32 bits             |
//! | 0x24   | SIOUTBUF3     | Channel 3 output buffer                        |
//! | 0x28   | SIINBUF3H     | Channel 3 input buffer high 32 bits            |
//! | 0x2C   | SIINBUF3L     | Channel 3 input buffer low 32 bits             |
//! | 0x30   | SIPOLL        | Polling configuration                          |
//! | 0x34   | SICOMCSR      | Communication Control and Status               |
//! | 0x38   | SISR          | SI Status Register                             |
//! | 0x3C   | SIEXILK       | EXI/SI Clock Lock                              |
//! | 0x80   | SIBUFREG      | SI Buffer Register (128 bytes)                 |

/// Physical base address of the Serial Interface registers.
pub(crate) const SI_BASE: u32 = 0xCC00_6400;
/// Byte span of the SI register block (256 bytes covers all SI registers).
pub(crate) const SI_SIZE: u32 = 0x100;

// ─── GameCube button bitmask constants (must match bootstrap.js GC_BTN) ──────

const BTN_A:           u32 = 0x0001;
const BTN_B:           u32 = 0x0002;
const BTN_X:           u32 = 0x0004;
const BTN_Y:           u32 = 0x0008;
const BTN_Z:           u32 = 0x0010;
const BTN_START:       u32 = 0x0020;
const BTN_UP:          u32 = 0x0040;
const BTN_DOWN:        u32 = 0x0080;
const BTN_LEFT:        u32 = 0x0100;
const BTN_RIGHT:       u32 = 0x0200;
const BTN_L:           u32 = 0x0400;
const BTN_R:           u32 = 0x0800;

/// Serial Interface hardware register file.
pub(crate) struct SiState {
    /// Per-channel output buffers (command written by the game).
    /// Channel `n` lives at offset `n * 0x0C`.
    out_buf: [u32; 4],
    /// Per-channel input buffer high word (response bytes 0–3).
    in_buf_hi: [u32; 4],
    /// Per-channel input buffer low word (response bytes 4–7).
    in_buf_lo: [u32; 4],
    /// SIPOLL — polling configuration register.
    poll: u32,
    /// SICOMCSR — Communication Control and Status Register.
    comm_ctrl: u32,
    /// SISR — SI Status Register.
    status: u32,
    /// SIEXILK — EXI/SI Clock Lock.
    exi_clock_lock: u32,
    /// SI Buffer (128 bytes at offset 0x80; used by some games for raw transfers).
    si_buf: [u8; 128],
    /// Analog main stick X axis (0–255, centre = 128).
    ///
    /// Updated by [`SiState::set_analog_axes`] from the JavaScript input layer.
    /// Takes precedence over the discrete `STICK_*` pseudo-buttons in the SI
    /// poll response so that gamepad analog input is reported with full 8-bit
    /// resolution rather than a fixed ±96 deflection.
    pub(crate) joy_x: u8,
    /// Analog main stick Y axis (0–255, centre = 128; up = larger value).
    pub(crate) joy_y: u8,
    /// Analog C-Stick X axis (0–255, centre = 128).
    pub(crate) c_stick_x: u8,
    /// Analog C-Stick Y axis (0–255, centre = 128).
    pub(crate) c_stick_y: u8,
    /// L trigger analog value (0–255; 0 = released, 255 = fully pressed).
    pub(crate) l_trig: u8,
    /// R trigger analog value (0–255; 0 = released, 255 = fully pressed).
    pub(crate) r_trig: u8,
}

impl Default for SiState {
    fn default() -> Self {
        Self {
            out_buf: [0u32; 4],
            in_buf_hi: [0u32; 4],
            in_buf_lo: [0u32; 4],
            poll: 0,
            comm_ctrl: 0,
            status: 0,
            exi_clock_lock: 0,
            si_buf: [0u8; 128],
            // Initialise analog axes to centred / released.
            joy_x: 128,
            joy_y: 128,
            c_stick_x: 128,
            c_stick_y: 128,
            l_trig: 0,
            r_trig: 0,
        }
    }
}

impl SiState {
    /// Read a 32-bit value from the SI register at `offset` bytes from `SI_BASE`.
    pub(crate) fn read_u32(&self, offset: u32) -> u32 {
        match offset {
            0x00 => self.out_buf[0],
            0x04 => self.in_buf_hi[0],
            0x08 => self.in_buf_lo[0],
            0x0C => self.out_buf[1],
            0x10 => self.in_buf_hi[1],
            0x14 => self.in_buf_lo[1],
            0x18 => self.out_buf[2],
            0x1C => self.in_buf_hi[2],
            0x20 => self.in_buf_lo[2],
            0x24 => self.out_buf[3],
            0x28 => self.in_buf_hi[3],
            0x2C => self.in_buf_lo[3],
            0x30 => self.poll,
            0x34 => self.comm_ctrl,
            0x38 => self.status,
            0x3C => self.exi_clock_lock,
            0x80..=0xFF => {
                // 128-byte SI buffer at offset 0x80: return 4 bytes big-endian.
                let buf_off = (offset - 0x80) as usize;
                if buf_off + 4 <= self.si_buf.len() {
                    u32::from_be_bytes(self.si_buf[buf_off..buf_off + 4].try_into().unwrap_or([0; 4]))
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// Write a 32-bit value to the SI register at `offset` bytes from `SI_BASE`.
    ///
    /// Returns `true` if an SI transfer was triggered (TSTART set in SICOMCSR),
    /// so the caller can assert the SI interrupt after building the response.
    pub(crate) fn write_u32(&mut self, offset: u32, val: u32, pad_buttons: u32) -> bool {
        match offset {
            0x00 => self.out_buf[0] = val,
            0x04 => self.in_buf_hi[0] = val,
            0x08 => self.in_buf_lo[0] = val,
            0x0C => self.out_buf[1] = val,
            0x10 => self.in_buf_hi[1] = val,
            0x14 => self.in_buf_lo[1] = val,
            0x18 => self.out_buf[2] = val,
            0x1C => self.in_buf_hi[2] = val,
            0x20 => self.in_buf_lo[2] = val,
            0x24 => self.out_buf[3] = val,
            0x28 => self.in_buf_hi[3] = val,
            0x2C => self.in_buf_lo[3] = val,
            0x30 => self.poll = val,
            0x34 => {
                // SICOMCSR: write-1-to-clear TRANSFER_INT (bit 31) and READ_INT (bit 28).
                let cleared = self.comm_ctrl
                    & !(val & 0x9000_0000); // clear bits 31 and 28 if written 1
                // Merge the non-status fields from the new value.
                self.comm_ctrl = (cleared & 0x9000_0000)
                    | (val & !0x9000_0000);

                // Check if TSTART (bit 0) is set → start a new transfer.
                if (val & 1) != 0 {
                    let channel = ((val >> 1) & 0x3) as usize;
                    self.auto_respond(channel, pad_buttons);
                    // Clear TSTART, set TRANSFER_INT (bit 31).
                    self.comm_ctrl = (self.comm_ctrl & !1) | 0x8000_0000;
                    return true; // caller should assert SI interrupt
                }
            }
            0x38 => {
                // SISR: write-1-to-clear
                self.status &= !val;
            }
            0x3C => self.exi_clock_lock = val,
            0x80..=0xFF => {
                // SI buffer write
                let buf_off = (offset - 0x80) as usize;
                if buf_off + 4 <= self.si_buf.len() {
                    let bytes = val.to_be_bytes();
                    self.si_buf[buf_off..buf_off + 4].copy_from_slice(&bytes);
                }
            }
            _ => {}
        }
        false
    }

    /// Build an automatic controller response for `channel` using `pad_buttons`.
    ///
    /// Reads the command byte from `out_buf[channel]` and writes the appropriate
    /// response into `in_buf_hi[channel]` / `in_buf_lo[channel]`.
    fn auto_respond(&mut self, channel: usize, pad_buttons: u32) {
        if channel >= 4 {
            return;
        }
        // Extract the command byte from the high byte of the output buffer.
        let cmd = (self.out_buf[channel] >> 24) as u8;

        match cmd {
            // ── 0x00 — GetDeviceType ─────────────────────────────────────────
            0x00 => {
                if channel == 0 {
                    // Standard GameCube controller: type=0x09, status=0x00, motor=0x20
                    self.in_buf_hi[0] = 0x0900_2000;
                    self.in_buf_lo[0] = 0;
                } else {
                    // No controller on ports 1–3: return error pattern.
                    self.in_buf_hi[channel] = 0x0000_0000;
                    self.in_buf_lo[channel] = 0;
                }
            }

            // ── 0x40 — Poll / 0x41 — GetOrigin / 0x42 — Calibrate ───────────
            0x40 | 0x41 | 0x42 => {
                if channel == 0 {
                    let response = build_poll_response(
                        pad_buttons,
                        self.joy_x, self.joy_y,
                        self.c_stick_x, self.c_stick_y,
                        self.l_trig, self.r_trig,
                    );
                    // Pack 8 bytes into two u32s (big-endian byte order).
                    self.in_buf_hi[0] = u32::from_be_bytes(response[0..4].try_into().unwrap_or([0; 4]));
                    self.in_buf_lo[0] = u32::from_be_bytes(response[4..8].try_into().unwrap_or([0; 4]));
                } else {
                    // Ports 1–3: disconnected controller response (all zeroes).
                    self.in_buf_hi[channel] = 0;
                    self.in_buf_lo[channel] = 0;
                }
            }

            // ── Unknown command ───────────────────────────────────────────────
            _ => {
                // Return an empty response; mark no error.
                self.in_buf_hi[channel] = 0;
                self.in_buf_lo[channel] = 0;
            }
        }
    }

    /// Store analog axis values reported in subsequent controller poll responses.
    ///
    /// Called by the JavaScript input layer after polling the Gamepad API or
    /// converting keyboard `STICK_*` pseudo-buttons to axis values.
    ///
    /// - `joy_x` / `joy_y`: main stick (0–255, centre = 128; Y: up = larger).
    /// - `c_stick_x` / `c_stick_y`: C-Stick (0–255, centre = 128).
    /// - `l_trig` / `r_trig`: analog trigger depth (0 = released, 255 = full).
    pub(crate) fn set_analog_axes(
        &mut self,
        joy_x: u8, joy_y: u8,
        c_stick_x: u8, c_stick_y: u8,
        l_trig: u8, r_trig: u8,
    ) {
        self.joy_x     = joy_x;
        self.joy_y     = joy_y;
        self.c_stick_x = c_stick_x;
        self.c_stick_y = c_stick_y;
        self.l_trig    = l_trig;
        self.r_trig    = r_trig;
    }
}

/// Build the 8-byte SI poll response from digital `pad_buttons` and analog axes.
///
/// Maps the JavaScript `GC_BTN` bitmask to the GameCube controller wire format:
///
/// ```text
///   Byte 0: [0, 0, 0, START, Y, X, B, A]
///   Byte 1: [1, L_dig, R_dig, Z, D-Up, D-Down, D-Right, D-Left]
///   Byte 2: Joystick X  (0–255, centre=128)
///   Byte 3: Joystick Y  (0–255, centre=128; up = larger)
///   Byte 4: C-Stick X   (centre=128)
///   Byte 5: C-Stick Y   (centre=128)
///   Byte 6: L analog    (0–255)
///   Byte 7: R analog    (0–255)
/// ```
///
/// The analog values (`joy_x`, `joy_y`, `c_stick_x`, `c_stick_y`, `l_trig`,
/// `r_trig`) are provided directly by the caller (already converted from
/// Gamepad API axes or keyboard STICK_* deflection) rather than being derived
/// from the button bitmask, giving full 8-bit axis resolution.
fn build_poll_response(
    pad: u32,
    joy_x: u8, joy_y: u8,
    c_stick_x: u8, c_stick_y: u8,
    l_trig: u8, r_trig: u8,
) -> [u8; 8] {
    // Byte 0: high button byte
    let b0 = ((pad & BTN_A)     != 0) as u8        // bit 0
           | (((pad & BTN_B)    != 0) as u8) << 1  // bit 1
           | (((pad & BTN_X)    != 0) as u8) << 2  // bit 2
           | (((pad & BTN_Y)    != 0) as u8) << 3  // bit 3
           | (((pad & BTN_START)!= 0) as u8) << 4; // bit 4

    // Byte 1: low button byte — bit 7 = always 1 (valid controller)
    let b1 = 0x80_u8
           | (((pad & BTN_LEFT) != 0) as u8)       // bit 0 = D-Left
           | (((pad & BTN_RIGHT)!= 0) as u8) << 1  // bit 1 = D-Right
           | (((pad & BTN_DOWN) != 0) as u8) << 2  // bit 2 = D-Down
           | (((pad & BTN_UP)   != 0) as u8) << 3  // bit 3 = D-Up
           | (((pad & BTN_Z)    != 0) as u8) << 4  // bit 4 = Z
           | (((pad & BTN_R)    != 0) as u8) << 5  // bit 5 = R digital
           | (((pad & BTN_L)    != 0) as u8) << 6; // bit 6 = L digital

    [b0, b1, joy_x, joy_y, c_stick_x, c_stick_y, l_trig, r_trig]
}
