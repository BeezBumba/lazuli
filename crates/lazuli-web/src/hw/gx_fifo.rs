//! GX Command-Processor FIFO byte-stream parser.
//!
//! In native Lazuli the CPU sends draw commands to the Geometry Processor
//! (GP) by writing 32-byte bursts to the PI Write-Gather Port at
//! `0xCC008000`.  The GP reads from a circular ring-buffer in main RAM and
//! interprets a simple variable-length command language.
//!
//! The browser build cannot use the same circular-buffer mechanism (there is
//! no autonomous GP thread), but it must still parse enough of the command
//! stream to:
//!
//! 1. Update the CP register file (VCD / VAT tables) so subsequent vertex-size
//!    calculations are correct.
//! 2. Detect `SetBP` writes to `PixelDone` (register `0x45`) and set the
//!    `pe_finish_pending` flag — which unblocks games calling
//!    `GXWaitForDrawDone()` far more accurately than the previous VI-rate stub.
//! 3. Detect `SetBP` writes to `PixelToken` (register `0x48`) and capture the
//!    token value, so `PE_TOKEN` interrupts fire at the right moment.
//! 4. Skip draw-command payloads (vertex data) so the parser stays in sync.
//!
//! ## Command encoding
//!
//! Each command starts with a 1-byte opcode.  The high 5 bits encode the
//! `Operation`; the low 3 bits carry the VAT index (relevant for draw
//! commands only).
//!
//! | Opcode range | Operation              | Payload after opcode          |
//! |--------------|------------------------|-------------------------------|
//! | 0x00         | NOP                    | none                          |
//! | 0x08         | LoadCP                 | 1-byte reg + 4-byte value     |
//! | 0x10         | LoadXF                 | 2-byte (len,base) + N×4 bytes |
//! | 0x20/28/30/38| IndexedSetXF A/B/C/D   | 4-byte value                  |
//! | 0x40         | Call                   | 4-byte addr + 4-byte length   |
//! | 0x48         | InvalidateVertexCache  | none                          |
//! | 0x61         | LoadBP                 | 1-byte reg + 3-byte value     |
//! | 0x80–0xBF    | Draw*                  | 2-byte count + count×vtx bytes|
//!
//! ## CP register file
//!
//! The CP register file needed for vertex-size computation is tracked as raw
//! `u32` values matching the on-wire format:
//!
//! | CP register | CP addr range | Contents                     |
//! |-------------|---------------|------------------------------|
//! | VCD Low     | 0x50          | Vertex Component Descriptor  |
//! | VCD High    | 0x60          | Vertex Component Descriptor  |
//! | VAT A       | 0x70–0x77     | Vertex Attribute Tables A    |
//! | VAT B       | 0x80–0x87     | Vertex Attribute Tables B    |
//! | VAT C       | 0x90–0x97     | Vertex Attribute Tables C    |

/// BP register address for the draw-done signal (matches native `Reg::PixelDone = 0x45`).
const BP_PIXEL_DONE: u8 = 0x45;
/// BP register address for the PE token (matches native `Reg::PixelToken = 0x47`).
const BP_PIXEL_TOKEN: u8 = 0x47;
/// BP register address for the PE token interrupt (matches native `Reg::PixelTokenInt = 0x48`).
const BP_PIXEL_TOKEN_INT: u8 = 0x48;

/// GX command-processor FIFO parser and CP register file.
///
/// Accumulates raw bytes written to `0xCC008000` (the PI Write-Gather Port),
/// then parses and processes complete GX commands whenever new data arrives.
#[derive(Default)]
pub(crate) struct GxFifo {
    /// Byte accumulator.  Bytes are pushed by `push_u8` / `push_u32` and
    /// consumed by `process_commands`.
    buf: Vec<u8>,
    /// Current read cursor into `buf`.  Data before `read_pos` has been
    /// consumed; `buf` is compacted on demand.
    read_pos: usize,

    // ── CP register file ──────────────────────────────────────────────────
    /// VCD Low register (CP address 0x50).
    pub(crate) vcd_low: u32,
    /// VCD High register (CP address 0x60).
    pub(crate) vcd_high: u32,
    /// VAT A registers for the 8 VAT slots (CP addresses 0x70–0x77).
    pub(crate) vat_a: [u32; 8],
    /// VAT B registers for the 8 VAT slots (CP addresses 0x80–0x87).
    pub(crate) vat_b: [u32; 8],
    /// VAT C registers for the 8 VAT slots (CP addresses 0x90–0x97).
    pub(crate) vat_c: [u32; 8],

    // ── Interrupt outputs ─────────────────────────────────────────────────
    /// Set to `true` by `process_commands` when a `LoadBP PixelDone` command
    /// is seen.  The caller reads and clears this flag to assert `PE_FINISH`.
    pub(crate) pe_finish_pending: bool,
    /// Set to `true` by `process_commands` when a `LoadBP PixelToken` (with
    /// interrupt) command is seen.  Cleared by the caller.
    pub(crate) pe_token_pending: bool,
    /// The token value from the most recent `LoadBP PixelTokenInt`.
    pub(crate) pe_token: u16,
}

impl GxFifo {
    // ─── Data ingestion ───────────────────────────────────────────────────

    /// Push a single byte into the accumulator.
    #[inline]
    pub(crate) fn push_u8(&mut self, byte: u8) {
        self.buf.push(byte);
        self.process_commands();
    }

    /// Push four bytes from a big-endian 32-bit word into the accumulator.
    #[inline]
    pub(crate) fn push_u32(&mut self, word: u32) {
        let bytes = word.to_be_bytes();
        self.buf.extend_from_slice(&bytes);
        self.process_commands();
    }

    // ─── Command parser ───────────────────────────────────────────────────

    /// Consume as many complete commands as possible from the accumulator.
    ///
    /// Stops when there is not enough data to complete the next command or
    /// when no data remains.  Compacts the buffer every 64 KiB of consumed
    /// data to prevent unbounded growth.
    pub(crate) fn process_commands(&mut self) {
        loop {
            let avail = self.buf.len() - self.read_pos;
            if avail == 0 {
                break;
            }

            let opcode = self.buf[self.read_pos];

            // High 5 bits = operation, low 3 bits = VAT index.
            let operation = opcode >> 3;
            let vat_index = (opcode & 0x7) as usize;

            // Minimum remaining bytes needed (excluding opcode byte):
            let payload_needed = match operation {
                0b0_0000 => Some(0),  // NOP
                0b0_0001 => Some(5), // LoadCP: 1-byte reg + 4-byte value
                0b0_0010 => {
                    // LoadXF: 2-byte header (length+base) → then count×4 data
                    if avail < 3 {
                        break; // need opcode + 2 bytes for length_field
                    }
                    let lf = u16::from_be_bytes([
                        self.buf[self.read_pos + 1],
                        self.buf[self.read_pos + 2],
                    ]);
                    let count = ((lf & 0xF) as usize) + 1;
                    Some(4 + count * 4) // 2 (lf) + 2 (base) + count*4
                }
                0b0_0100 | 0b0_0101 | 0b0_0110 | 0b0_0111 => Some(4), // IndexedSetXF A/B/C/D
                0b0_1000 => Some(8), // Call: 4-byte addr + 4-byte length
                0b0_1001 => Some(0), // InvalidateVertexCache
                0b0_1100 => Some(4), // LoadBP: 1-byte reg + 3-byte value
                op if op & 0b1_0000 != 0 => {
                    // Draw command (0b1_xxxx): 2-byte vertex count + vertex data
                    if avail < 3 {
                        break; // need opcode + 2-byte count
                    }
                    let count = u16::from_be_bytes([
                        self.buf[self.read_pos + 1],
                        self.buf[self.read_pos + 2],
                    ]) as usize;
                    let vs = self.vertex_size(vat_index);
                    Some(2 + count * vs) // 2-byte count + vertex data
                }
                _ => Some(0), // unknown — skip opcode only
            };

            let Some(payload) = payload_needed else { break };

            if avail < 1 + payload {
                break; // not enough data yet
            }

            // We have a complete command — parse and process it.
            self.execute_command(opcode, operation, vat_index, payload);
            self.read_pos += 1 + payload;

            // Compact buffer once we've consumed ≥64 KiB.
            if self.read_pos >= 65536 {
                self.buf.drain(..self.read_pos);
                self.read_pos = 0;
            }
        }
    }

    /// Execute a single fully-buffered command.
    fn execute_command(&mut self, opcode: u8, operation: u8, vat_index: usize, _payload: usize) {
        let base = self.read_pos + 1; // byte after the opcode

        match operation {
            0b0_0000 => {} // NOP
            0b0_0001 => {
                // LoadCP: 1-byte register address + 4-byte value
                let reg = self.buf[base];
                let val = u32::from_be_bytes(self.buf[base + 1..base + 5].try_into().unwrap_or([0; 4]));
                self.apply_cp_register(reg, val);
            }
            0b0_0010 => {
                // LoadXF: 2-byte (length_field) + 2-byte base + count×4 values
                // We track the XF register file here only if we need to; for
                // now we only care about the CP registers that affect vertex size.
            }
            0b0_0100..=0b0_0111 => {} // IndexedSetXF A/B/C/D — ignore
            0b0_1000 => {}            // Call — ignore (display lists)
            0b0_1001 => {}            // InvalidateVertexCache — ignore
            0b0_1100 => {
                // LoadBP: opcode encodes register (low bits of opcode byte = 0
                // for BP, but actual register is the first payload byte).
                // BP payload = 1-byte register + 3-byte value (big-endian, MSB = reg).
                // Actually: the 4-byte payload is [reg, hi, mid, lo].
                let reg = self.buf[base];
                let val = u32::from_be_bytes([0, self.buf[base + 1], self.buf[base + 2], self.buf[base + 3]]);
                self.apply_bp_register(reg, val);
            }
            op if op & 0b1_0000 != 0 => {
                // Draw command — vertex data already skipped by payload_needed.
                let _ = (opcode, vat_index);
            }
            _ => {}
        }
    }

    /// Apply a write to a CP register.
    fn apply_cp_register(&mut self, reg: u8, val: u32) {
        match reg {
            0x50 => self.vcd_low  = val,
            0x60 => self.vcd_high = val,
            0x70..=0x77 => self.vat_a[(reg - 0x70) as usize] = val,
            0x80..=0x87 => self.vat_b[(reg - 0x80) as usize] = val,
            0x90..=0x97 => self.vat_c[(reg - 0x90) as usize] = val,
            _ => {}
        }
    }

    /// Apply a write to a BP (Blitter/PE) register.
    ///
    /// Sets `pe_finish_pending` when register `0x45` (`PixelDone`) is written,
    /// mirroring native `gx.rs set_register(Reg::PixelDone)`.
    fn apply_bp_register(&mut self, reg: u8, val: u32) {
        match reg {
            BP_PIXEL_DONE => {
                self.pe_finish_pending = true;
            }
            BP_PIXEL_TOKEN => {
                self.pe_token = (val & 0xFFFF) as u16;
            }
            BP_PIXEL_TOKEN_INT => {
                self.pe_token = (val & 0xFFFF) as u16;
                self.pe_token_pending = true;
            }
            _ => {}
        }
    }

    // ─── Vertex size computation ──────────────────────────────────────────

    /// Compute the byte size of a single vertex in the stream for VAT slot `vat`.
    ///
    /// This mirrors `Internal::vertex_size` in the native build.
    pub(crate) fn vertex_size(&self, vat: usize) -> usize {
        let vat = vat.min(7);
        let vcd_l = self.vcd_low;
        let vcd_h = self.vcd_high;
        let va    = self.vat_a[vat];
        let vb    = self.vat_b[vat];
        let vc    = self.vat_c[vat];

        let mut size = 0usize;

        // ── Position matrix index (1 bit, no mode, just enabled/disabled) ──
        if (vcd_l >> 0) & 1 != 0 {
            size += 1;
        }

        // ── Texture coord matrix indices 0–7 (1 bit each) ─────────────────
        for i in 0..8usize {
            if (vcd_l >> (1 + i)) & 1 != 0 {
                size += 1;
            }
        }

        // ── Position (bits 9–10 of VCD Low) ───────────────────────────────
        let pos_mode = (vcd_l >> 9) & 3;
        size += attr_size(pos_mode, pos_direct_size(va));

        // ── Normal (bits 11–12) ────────────────────────────────────────────
        let norm_mode = (vcd_l >> 11) & 3;
        size += attr_size(norm_mode, norm_direct_size(va));

        // ── Color0 (bits 13–14) ────────────────────────────────────────────
        let col0_mode = (vcd_l >> 13) & 3;
        size += attr_size(col0_mode, color_direct_size(va, false));

        // ── Color1 (bits 15–16) ────────────────────────────────────────────
        let col1_mode = (vcd_l >> 15) & 3;
        size += attr_size(col1_mode, color_direct_size(va, true));

        // ── Texture coordinates 0–7 (2 bits each in VCD High) ─────────────
        for tc in 0..8usize {
            let tc_mode = (vcd_h >> (tc * 2)) & 3;
            size += attr_size(tc_mode, texcoord_direct_size(tc, va, vb, vc));
        }

        size
    }
}

// ─── Attribute size helpers ───────────────────────────────────────────────────

/// Byte size contributed by one attribute given its descriptor `mode` and
/// `direct_size` (the size when the attribute is embedded directly in the
/// vertex stream).
///
/// Maps `AttributeMode` variants:
/// - `None` (0b00) → 0 bytes
/// - `Direct` (0b01) → `direct_size`
/// - `Index8` (0b10) → 1 byte
/// - `Index16` (0b11) → 2 bytes
#[inline]
fn attr_size(mode: u32, direct_size: usize) -> usize {
    match mode {
        0 => 0,            // AttributeMode::None
        1 => direct_size,  // AttributeMode::Direct
        2 => 1,            // AttributeMode::Index8
        3 => 2,            // AttributeMode::Index16
        _ => 0,
    }
}

/// Byte size of one element in `CoordsFormat`.
///
/// | Code | Format | Bytes |
/// |------|--------|-------|
/// | 0    | u8     | 1     |
/// | 1    | i8     | 1     |
/// | 2    | u16    | 2     |
/// | 3    | i16    | 2     |
/// | 4    | f32    | 4     |
/// | 5–7  | rsvd   | 1     |
#[inline]
fn coords_fmt_size(fmt: u32) -> usize {
    match fmt {
        0 | 1 => 1,
        2 | 3 => 2,
        4 => 4,
        _ => 1, // reserved
    }
}

/// Position attribute direct size from `VAT_A`.
///
/// VAT_A bits 0–8 = PositionDescriptor:
/// - bit 0  : PositionKind (0=Vec2, 1=Vec3)
/// - bits 1–3: CoordsFormat
/// - bits 4–8: shift (irrelevant for size)
#[inline]
fn pos_direct_size(va: u32) -> usize {
    let comps = if (va >> 0) & 1 == 0 { 2 } else { 3 };
    let fmt   = (va >> 1) & 7;
    comps * coords_fmt_size(fmt)
}

/// Normal attribute direct size from `VAT_A`.
///
/// VAT_A bits 9–12 = NormalDescriptor:
/// - bit 9    : NormalKind (0=N3, 1=N9)
/// - bits 10–12: CoordsFormat
#[inline]
fn norm_direct_size(va: u32) -> usize {
    let comps = if (va >> 9) & 1 == 0 { 3 } else { 9 };
    let fmt   = (va >> 10) & 7;
    comps * coords_fmt_size(fmt)
}

/// Color attribute direct size from `VAT_A`.
///
/// ColorDescriptor for `chan0` lives at VAT_A bits 13–16; `chan1` at 17–20.
///
/// | Format | Bytes |
/// |--------|-------|
/// | 0 RGB565  | 2  |
/// | 1 RGB888  | 3  |
/// | 2 RGB888x | 4  |
/// | 3 RGBA4444| 2  |
/// | 4 RGBA6666| 3  |
/// | 5 RGBA8888| 4  |
#[inline]
fn color_direct_size(va: u32, chan1: bool) -> usize {
    let base = if chan1 { 17u32 } else { 13u32 };
    // bit `base` = ColorKind (unused for size); bits base+1..base+3 = ColorFormat
    let fmt = (va >> (base + 1)) & 7;
    match fmt {
        0 | 3 => 2,
        1 | 4 => 3,
        2 | 5 => 4,
        _ => 4,
    }
}

/// Texture coordinate attribute direct size.
///
/// TexCoordsDescriptor (9 bits):
/// - bit 0  : TexCoordsKind (0=S, 1=ST)
/// - bits 1–3: CoordsFormat
/// - bits 4–8: shift (irrelevant for size)
///
/// Layout in VAT registers:
/// | TC index | VAT reg | Start bit |
/// |----------|---------|-----------|
/// | 0        | A       | 21        |
/// | 1        | B       | 0         |
/// | 2        | B       | 9         |
/// | 3        | B       | 18        |
/// | 4        | B/C     | 27+0      |
/// | 5        | C       | 5         |
/// | 6        | C       | 14        |
/// | 7        | C       | 23        |
#[inline]
fn texcoord_direct_size(tc: usize, va: u32, vb: u32, vc: u32) -> usize {
    let (kind, fmt) = match tc {
        0 => ((va >> 21) & 1, (va >> 22) & 7),
        1 => ((vb >>  0) & 1, (vb >>  1) & 7),
        2 => ((vb >>  9) & 1, (vb >> 10) & 7),
        3 => ((vb >> 18) & 1, (vb >> 19) & 7),
        // tex4: kind at VAT_B bit 27, format at VAT_B bits 28-30
        4 => ((vb >> 27) & 1, (vb >> 28) & 7),
        5 => ((vc >>  5) & 1, (vc >>  6) & 7),
        6 => ((vc >> 14) & 1, (vc >> 15) & 7),
        7 => ((vc >> 23) & 1, (vc >> 24) & 7),
        _ => (0, 0),
    };
    let comps = if kind == 0 { 1 } else { 2 };
    comps * coords_fmt_size(fmt)
}
