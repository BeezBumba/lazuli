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
//! 4. Decode vertices from draw-command payloads and forward them to the
//!    `renderer::Renderer` as `Action::Draw` so 3D geometry is actually shown
//!    (wasm32 only, via [`GxFifo::push_u32_gfx`]).
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
//! | CP register | CP addr range | Contents                     |
//! |-------------|---------------|------------------------------|
//! | VCD Low     | 0x50          | Vertex Component Descriptor  |
//! | VCD High    | 0x60          | Vertex Component Descriptor  |
//! | VAT A       | 0x70–0x77     | Vertex Attribute Tables A    |
//! | VAT B       | 0x80–0x87     | Vertex Attribute Tables B    |
//! | VAT C       | 0x90–0x97     | Vertex Attribute Tables C    |
//! | ArrBase     | 0xA0–0xAF     | Array base pointers          |
//! | ArrStride   | 0xB0–0xBF     | Array strides                |

/// BP register address for the draw-done signal.
const BP_PIXEL_DONE: u8 = 0x45;
/// BP register address for the PE token.
const BP_PIXEL_TOKEN: u8 = 0x47;
/// BP register address for the PE token interrupt.
const BP_PIXEL_TOKEN_INT: u8 = 0x48;

// ── BP register addresses for pipeline state ──────────────────────────────────
const BP_Z_MODE: u8         = 0x40;
const BP_BLEND_MODE: u8     = 0x41;
const BP_CONST_ALPHA: u8    = 0x42;
const BP_SCISSOR_TL: u8     = 0x20;
const BP_SCISSOR_BR: u8     = 0x21;
const BP_SCISSOR_OFFSET: u8 = 0x59;
const BP_TEV_ALPHA_FUNC: u8 = 0xF3;
const BP_FOG_A: u8          = 0xEE;
const BP_FOG_B0: u8         = 0xEF;
const BP_FOG_B1: u8         = 0xF0;
const BP_FOG_C: u8          = 0xF1;
const BP_FOG_COLOR: u8      = 0xF2;
const BP_CLEAR_AR: u8       = 0x4F;
const BP_CLEAR_GB: u8       = 0x50;
const BP_CLEAR_Z: u8        = 0x51;

// ── XF named register addresses ───────────────────────────────────────────────
const XF_VIEWPORT_START: u16 = 0x101A;
const XF_VIEWPORT_END: u16   = 0x101F;
const XF_PROJECTION_START: u16 = 0x1020;
const XF_PROJECTION_END: u16   = 0x1026;
const XF_CHAN0_COLOR_CTRL: u16 = 0x100D;
const XF_CHAN1_COLOR_CTRL: u16 = 0x100E;
const XF_CHAN0_ALPHA_CTRL: u16 = 0x1011;
const XF_CHAN1_ALPHA_CTRL: u16 = 0x1012;

/// Number of XF registers tracked (covers matrix banks + named registers).
const XF_REG_COUNT: usize = 0x1058;

/// GX command-processor FIFO parser, CP register file, and GX pipeline state.
///
/// Accumulates raw bytes written to `0xCC008000` (the PI Write-Gather Port),
/// then parses and processes complete GX commands whenever new data arrives.
pub(crate) struct GxFifo {
    /// Byte accumulator.
    buf: Vec<u8>,
    /// Current read cursor into `buf`.
    read_pos: usize,

    // ── CP register file ──────────────────────────────────────────────────
    pub(crate) vcd_low:  u32,
    pub(crate) vcd_high: u32,
    pub(crate) vat_a: [u32; 8],
    pub(crate) vat_b: [u32; 8],
    pub(crate) vat_c: [u32; 8],
    cp_array_base:   [u32; 16],
    cp_array_stride: [u32; 16],

    // ── XF state (wasm32 only: drives renderer pipeline state) ───────────
    #[cfg(target_arch = "wasm32")]
    xf_regs: Box<[u32; XF_REG_COUNT]>,
    #[cfg(target_arch = "wasm32")]
    xf_viewport_dirty: bool,
    #[cfg(target_arch = "wasm32")]
    xf_projection_dirty: bool,
    #[cfg(target_arch = "wasm32")]
    xf_channels_dirty: bool,

    // ── BP state (wasm32 only) ────────────────────────────────────────────
    #[cfg(target_arch = "wasm32")]
    bp_regs: Box<[u32; 256]>,
    #[cfg(target_arch = "wasm32")]
    bp_pixel_dirty: bool,
    #[cfg(target_arch = "wasm32")]
    bp_scissor_dirty: bool,
    #[cfg(target_arch = "wasm32")]
    bp_env_dirty: bool,
    #[cfg(target_arch = "wasm32")]
    bp_clear_dirty: bool,

    // ── Interrupt outputs ─────────────────────────────────────────────────
    pub(crate) pe_finish_pending: bool,
    pub(crate) pe_token_pending:  bool,
    pub(crate) pe_token:          u16,
}

impl Default for GxFifo {
    fn default() -> Self {
        Self {
            buf:             Vec::new(),
            read_pos:        0,
            vcd_low:         0,
            vcd_high:        0,
            vat_a:           [0; 8],
            vat_b:           [0; 8],
            vat_c:           [0; 8],
            cp_array_base:   [0; 16],
            cp_array_stride: [0; 16],
            #[cfg(target_arch = "wasm32")]
            xf_regs:             Box::new([0u32; XF_REG_COUNT]),
            #[cfg(target_arch = "wasm32")]
            xf_viewport_dirty:   false,
            #[cfg(target_arch = "wasm32")]
            xf_projection_dirty: false,
            #[cfg(target_arch = "wasm32")]
            xf_channels_dirty:   false,
            #[cfg(target_arch = "wasm32")]
            bp_regs:         Box::new([0u32; 256]),
            #[cfg(target_arch = "wasm32")]
            bp_pixel_dirty:  false,
            #[cfg(target_arch = "wasm32")]
            bp_scissor_dirty: false,
            #[cfg(target_arch = "wasm32")]
            bp_env_dirty:    false,
            #[cfg(target_arch = "wasm32")]
            bp_clear_dirty:  false,
            pe_finish_pending: false,
            pe_token_pending:  false,
            pe_token:          0,
        }
    }
}

impl GxFifo {
    // ─── Data ingestion ───────────────────────────────────────────────────────

    /// Push four bytes from a big-endian 32-bit word (non-rendering path).
    ///
    /// Still tracks CP/BP state for interrupt purposes; use
    /// [`GxFifo::push_u32_gfx`] on wasm32 to also dispatch GX draw actions.
    #[inline]
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn push_u32(&mut self, word: u32) {
        self.buf.extend_from_slice(&word.to_be_bytes());
        self.process_commands_simple();
    }

    /// Push four bytes and dispatch GX pipeline actions to `renderer`.
    ///
    /// On wasm32 this is the hot path: it decodes LoadCP/LoadXF/LoadBP state
    /// _and_ forwards draw commands to the GPU renderer.
    #[cfg(target_arch = "wasm32")]
    #[inline]
    pub(crate) fn push_u32_gfx(
        &mut self,
        word: u32,
        ram: &[u8],
        renderer: Option<&mut crate::renderer::Renderer>,
    ) {
        self.buf.extend_from_slice(&word.to_be_bytes());
        self.process_pending_gfx(ram, renderer);
    }

    // ─── Command parsers ──────────────────────────────────────────────────────

    /// Simple command parser (all platforms): handles CP/BP registers for
    /// interrupt generation but does not decode vertex data.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn process_commands_simple(&mut self) {
        loop {
            let avail = self.buf.len() - self.read_pos;
            if avail == 0 { break; }

            let opcode    = self.buf[self.read_pos];
            let operation = opcode >> 3;
            let vat_index = (opcode & 0x7) as usize;

            let payload = match self.payload_needed(operation, vat_index, avail) {
                Some(p) => p,
                None    => break,
            };
            if avail < 1 + payload { break; }

            let base = self.read_pos + 1;
            self.execute_base(opcode, operation, vat_index, base);
            self.read_pos += 1 + payload;
            self.maybe_compact();
        }
    }

    /// Full command parser (wasm32 only): handles CP/BP/XF registers _and_
    /// decodes vertex data, dispatching draw actions to the renderer.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn process_pending_gfx(
        &mut self,
        ram: &[u8],
        mut renderer: Option<&mut crate::renderer::Renderer>,
    ) {
        loop {
            let avail = self.buf.len() - self.read_pos;
            if avail == 0 { break; }

            let opcode    = self.buf[self.read_pos];
            let operation = opcode >> 3;
            let vat_index = (opcode & 0x7) as usize;

            let payload = match self.payload_needed(operation, vat_index, avail) {
                Some(p) => p,
                None    => break,
            };
            if avail < 1 + payload { break; }

            let base = self.read_pos + 1;
            self.execute_gfx(opcode, operation, vat_index, base, ram, renderer.as_deref_mut());
            self.read_pos += 1 + payload;
            self.maybe_compact();
        }
    }

    // ─── Payload-size computation ─────────────────────────────────────────────

    fn payload_needed(&self, operation: u8, vat_index: usize, avail: usize) -> Option<usize> {
        Some(match operation {
            0b0_0000 => 0,
            0b0_0001 => 5,
            0b0_0010 => {
                if avail < 3 { return None; }
                let lf = u16::from_be_bytes([
                    self.buf[self.read_pos + 1],
                    self.buf[self.read_pos + 2],
                ]);
                4 + ((lf & 0xF) as usize + 1) * 4
            }
            0b0_0100..=0b0_0111 => 4,
            0b0_1000 => 8,
            0b0_1001 => 0,
            0b0_1100 => 4,
            op if op & 0b1_0000 != 0 => {
                if avail < 3 { return None; }
                let count = u16::from_be_bytes([
                    self.buf[self.read_pos + 1],
                    self.buf[self.read_pos + 2],
                ]) as usize;
                2 + count * self.vertex_size(vat_index)
            }
            _ => 0,
        })
    }

    fn maybe_compact(&mut self) {
        if self.read_pos >= 65536 {
            self.buf.drain(..self.read_pos);
            self.read_pos = 0;
        }
    }

    // ─── Base command dispatch (all platforms) ────────────────────────────────

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn execute_base(&mut self, opcode: u8, operation: u8, vat_index: usize, base: usize) {
        let _ = (opcode, vat_index);
        match operation {
            0b0_0001 => {
                let reg = self.buf[base];
                let val = u32::from_be_bytes(
                    self.buf[base + 1..base + 5].try_into().unwrap_or([0; 4]),
                );
                self.apply_cp_register(reg, val);
            }
            0b0_1100 => {
                let reg = self.buf[base];
                let val = u32::from_be_bytes([
                    0,
                    self.buf[base + 1],
                    self.buf[base + 2],
                    self.buf[base + 3],
                ]);
                self.apply_bp_simple(reg, val);
            }
            _ => {}
        }
    }

    // ─── GFX command dispatch (wasm32 only) ───────────────────────────────────

    #[cfg(target_arch = "wasm32")]
    fn execute_gfx(
        &mut self,
        opcode: u8,
        operation: u8,
        vat_index: usize,
        base: usize,
        ram: &[u8],
        renderer: Option<&mut crate::renderer::Renderer>,
    ) {
        match operation {
            0b0_0001 => {
                let reg = self.buf[base];
                let val = u32::from_be_bytes(
                    self.buf[base + 1..base + 5].try_into().unwrap_or([0; 4]),
                );
                self.apply_cp_register(reg, val);
            }
            0b0_0010 => {
                let lf      = u16::from_be_bytes([self.buf[base], self.buf[base + 1]]);
                let xf_base = u16::from_be_bytes([self.buf[base + 2], self.buf[base + 3]]);
                let count   = ((lf & 0xF) as usize) + 1;
                for i in 0..count {
                    let off = base + 4 + i * 4;
                    let val = u32::from_be_bytes(
                        self.buf[off..off + 4].try_into().unwrap_or([0; 4]),
                    );
                    self.apply_xf_register(xf_base + i as u16, val);
                }
            }
            0b0_1100 => {
                let reg = self.buf[base];
                let val = u32::from_be_bytes([
                    0,
                    self.buf[base + 1],
                    self.buf[base + 2],
                    self.buf[base + 3],
                ]);
                self.apply_bp_gfx(reg, val, renderer);
            }
            op if op & 0b1_0000 != 0 => {
                let count     = u16::from_be_bytes([self.buf[base], self.buf[base + 1]]) as usize;
                let vtx_start = base + 2;
                let stride    = self.vertex_size(vat_index);
                let vtx_end   = vtx_start + count * stride;
                // `payload_needed` already verified sufficient bytes are in the buffer
                // before this function is called; this guard is a defensive check.
                if count == 0 || vtx_end > self.buf.len() { return; }
                let topology = draw_topology(opcode);
                if let Some(r) = renderer {
                    let vtx_data: Vec<u8> = self.buf[vtx_start..vtx_end].to_vec();
                    self.decode_and_draw(topology, vat_index, &vtx_data, count as u16, ram, r);
                }
            }
            _ => {}
        }
    }

    // ─── CP register handling ─────────────────────────────────────────────────

    fn apply_cp_register(&mut self, reg: u8, val: u32) {
        match reg {
            0x50 => self.vcd_low  = val,
            0x60 => self.vcd_high = val,
            0x70..=0x77 => self.vat_a[(reg - 0x70) as usize] = val,
            0x80..=0x87 => self.vat_b[(reg - 0x80) as usize] = val,
            0x90..=0x97 => self.vat_c[(reg - 0x90) as usize] = val,
            0xA0..=0xAF => self.cp_array_base[(reg - 0xA0) as usize]   = val,
            0xB0..=0xBF => self.cp_array_stride[(reg - 0xB0) as usize] = val,
            _ => {}
        }
    }

    // ─── BP register handling — simple (all platforms) ────────────────────────

    fn apply_bp_simple(&mut self, reg: u8, val: u32) {
        match reg {
            BP_PIXEL_DONE    => { self.pe_finish_pending = true; }
            BP_PIXEL_TOKEN   => { self.pe_token = (val & 0xFFFF) as u16; }
            BP_PIXEL_TOKEN_INT => {
                self.pe_token = (val & 0xFFFF) as u16;
                self.pe_token_pending = true;
            }
            _ => {}
        }
    }

    // ─── XF register handling (wasm32) ───────────────────────────────────────

    #[cfg(target_arch = "wasm32")]
    fn apply_xf_register(&mut self, addr: u16, val: u32) {
        let idx = addr as usize;
        if idx < XF_REG_COUNT {
            self.xf_regs[idx] = val;
        }
        if (XF_VIEWPORT_START..=XF_VIEWPORT_END).contains(&addr) {
            self.xf_viewport_dirty = true;
        } else if (XF_PROJECTION_START..=XF_PROJECTION_END).contains(&addr) {
            self.xf_projection_dirty = true;
        } else if matches!(
            addr,
            XF_CHAN0_COLOR_CTRL | XF_CHAN1_COLOR_CTRL
            | XF_CHAN0_ALPHA_CTRL | XF_CHAN1_ALPHA_CTRL
        ) {
            self.xf_channels_dirty = true;
        }
    }

    // ─── BP register handling — full (wasm32) ────────────────────────────────

    #[cfg(target_arch = "wasm32")]
    fn apply_bp_gfx(
        &mut self,
        reg: u8,
        val: u32,
        renderer: Option<&mut crate::renderer::Renderer>,
    ) {
        // Interrupt flags (always).
        self.apply_bp_simple(reg, val);

        // Apply write mask (BP register 0xFE) if set.
        let mask = self.bp_regs[0xFE];
        if mask != 0 {
            self.bp_regs[reg as usize] = (self.bp_regs[reg as usize] & !mask) | (val & mask);
            self.bp_regs[0xFE] = 0;
        } else {
            self.bp_regs[reg as usize] = val;
        }

        match reg {
            BP_Z_MODE | BP_BLEND_MODE | BP_CONST_ALPHA => { self.bp_pixel_dirty  = true; }
            BP_SCISSOR_TL | BP_SCISSOR_BR | BP_SCISSOR_OFFSET => { self.bp_scissor_dirty = true; }
            BP_TEV_ALPHA_FUNC
            | BP_FOG_A | BP_FOG_B0 | BP_FOG_B1 | BP_FOG_C | BP_FOG_COLOR => {
                self.bp_env_dirty = true;
            }
            BP_CLEAR_AR | BP_CLEAR_GB | BP_CLEAR_Z => { self.bp_clear_dirty = true; }
            _ => {}
        }

        if let Some(r) = renderer {
            self.flush_bp_actions(r);
        }
    }

    // ─── State flush helpers (wasm32) ─────────────────────────────────────────

    #[cfg(target_arch = "wasm32")]
    fn flush_bp_actions(&mut self, renderer: &mut crate::renderer::Renderer) {
        use lazuli::modules::render::Action;
        use lazuli::system::gx::pix::{
            BlendMode, ConstantAlpha, DepthMode, Scissor, ScissorCorner, ScissorOffset,
        };
        use lazuli::system::gx::tev::alpha;
        use lazuli::system::gx::color::Rgba8;

        /// Extract an `Rgba8` from the two BP clear-color registers:
        /// `ar` carries alpha (bits 31–24) and red (bits 23–16),
        /// `gb` carries green (bits 31–24) and blue (bits 23–16).
        fn rgba8_from_ar_gb(ar: u32, gb: u32) -> Rgba8 {
            Rgba8 { r: (ar >> 8) as u8, g: (gb >> 8) as u8, b: gb as u8, a: ar as u8 }
        }

        if std::mem::take(&mut self.bp_pixel_dirty) {
            renderer.exec(Action::SetDepthMode(
                DepthMode::from_bits(self.bp_regs[BP_Z_MODE as usize]),
            ));
            renderer.exec(Action::SetBlendMode(
                BlendMode::from_bits(self.bp_regs[BP_BLEND_MODE as usize]),
            ));
            renderer.exec(Action::SetConstantAlpha(
                ConstantAlpha::from_bits(self.bp_regs[BP_CONST_ALPHA as usize]),
            ));
        }

        if std::mem::take(&mut self.bp_scissor_dirty) {
            let scissor = Scissor {
                top_left:     ScissorCorner::from_bits(self.bp_regs[BP_SCISSOR_TL as usize]),
                bottom_right: ScissorCorner::from_bits(self.bp_regs[BP_SCISSOR_BR as usize]),
                offset:       ScissorOffset::from_bits(self.bp_regs[BP_SCISSOR_OFFSET as usize]),
            };
            renderer.exec(Action::SetScissor(scissor));
        }

        if std::mem::take(&mut self.bp_env_dirty) {
            renderer.exec(Action::SetAlphaTest(
                alpha::Test::from_bits(self.bp_regs[BP_TEV_ALPHA_FUNC as usize]),
            ));
            use lazuli::system::gx::tev::{Fog, FogParamA, FogParamB0, FogParamB1, FogParamC};
            let fog = Fog {
                a:     FogParamA::from_bits(self.bp_regs[BP_FOG_A as usize]),
                b0:    FogParamB0::from_bits(self.bp_regs[BP_FOG_B0 as usize]),
                b1:    FogParamB1::from_bits(self.bp_regs[BP_FOG_B1 as usize]),
                c:     FogParamC::from_bits(self.bp_regs[BP_FOG_C as usize]),
                color: {
                    let raw = self.bp_regs[BP_FOG_COLOR as usize];
                    // BP fog color: bits 23–16 = R, 15–8 = G, 7–0 = B, no alpha.
                    Rgba8 { r: (raw >> 16) as u8, g: (raw >> 8) as u8, b: raw as u8, a: 255 }
                },
            };
            renderer.exec(Action::SetFog(fog));
        }

        if std::mem::take(&mut self.bp_clear_dirty) {
            let ar = self.bp_regs[BP_CLEAR_AR as usize];
            let gb = self.bp_regs[BP_CLEAR_GB as usize];
            renderer.exec(Action::SetClearColor(rgba8_from_ar_gb(ar, gb).into()));
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn flush_xf_actions(&mut self, renderer: &mut crate::renderer::Renderer) {
        use lazuli::modules::render::{Action, Viewport};

        if std::mem::take(&mut self.xf_viewport_dirty) {
            let base      = XF_VIEWPORT_START as usize;
            let scale_x   = f32::from_bits(self.xf_regs[base]);
            let scale_y   = f32::from_bits(self.xf_regs[base + 1]);
            let scale_z   = f32::from_bits(self.xf_regs[base + 2]);
            let offset_x  = f32::from_bits(self.xf_regs[base + 3]);
            let offset_y  = f32::from_bits(self.xf_regs[base + 4]);
            let _offset_z = f32::from_bits(self.xf_regs[base + 5]);
            // Z offset is not used in the near/far calculation below; the
            // GameCube uses a reversed-Z convention (far=0, near=1) that makes
            // offset_z redundant once scale_z is known.

            // GC XF viewport: half-extents + center, with 342-pixel hardware bias.
            let width      = scale_x.abs() * 2.0;
            let height     = scale_y.abs() * 2.0;
            let top_left_x = offset_x + 342.0 - scale_x.abs();
            let top_left_y = offset_y + 342.0 - scale_y.abs();
            // Near/far from Z scale+offset (GC uses reversed depth).
            let far_depth  = scale_z;
            let near_depth = scale_z - scale_z.abs();

            renderer.exec(Action::SetViewport(Viewport {
                width, height, top_left_x, top_left_y, near_depth, far_depth,
            }));
        }

        if std::mem::take(&mut self.xf_projection_dirty) {
            let base      = XF_PROJECTION_START as usize;
            let proj_type = self.xf_regs[base];
            let a = f32::from_bits(self.xf_regs[base + 1]);
            let b = f32::from_bits(self.xf_regs[base + 2]);
            let c = f32::from_bits(self.xf_regs[base + 3]);
            let d = f32::from_bits(self.xf_regs[base + 4]);
            let e = f32::from_bits(self.xf_regs[base + 5]);
            let f = f32::from_bits(self.xf_regs[base + 6]);

            use lazuli::system::gx::xform::ProjectionMtx;
            let proj = ProjectionMtx {
                // GC XF format: [A, B, C, D, E, F] where the projection is
                //   perspective:   A*x + B*z, C*y + D*z, E*z + F
                //   orthographic:  A*x + B,   C*y + D,   E*z + F
                params: [a, b, c, d, e, f],
                orthographic: proj_type != 0,
            };
            renderer.exec(Action::SetProjectionMatrix(proj));
        }
    }

    // ─── Vertex decoding + draw dispatch (wasm32) ────────────────────────────

    #[cfg(target_arch = "wasm32")]
    fn decode_and_draw(
        &mut self,
        topology: lazuli::system::gx::Topology,
        vat_index: usize,
        vertex_data: &[u8],
        count: u16,
        ram: &[u8],
        renderer: &mut crate::renderer::Renderer,
    ) {
        use lazuli::modules::render::Action;
        use lazuli::modules::vertex::{Ctx, VertexModule};
        use lazuli::system::gx::cmd::attributes::{
            VertexAttributeTable, VertexAttributeTableA,
            VertexAttributeTableB, VertexAttributeTableC,
        };
        use lazuli::system::gx::cmd::{VertexAttributeStream, VertexDescriptor};
        use lazuli::system::gx::xform::DefaultMatrices;
        use lazuli::system::gx::{
            alloc_matrices_handle, alloc_vertices_handle, MatrixSet, VertexStream,
        };
        use modules::vertex::InterpreterModule;

        // Reconstruct VCD / VAT from raw register values.
        let vcd = {
            let lo = self.vcd_low  as u64;
            let hi = self.vcd_high as u64;
            VertexDescriptor::from_bits((hi << 32) | lo)
        };
        if !vcd.position().is_present() {
            // Without position data the renderer cannot place vertices in 3D
            // space; skip this draw call entirely.
            return;
        }

        let vat = VertexAttributeTable {
            a: VertexAttributeTableA::from_bits(self.vat_a[vat_index]),
            b: VertexAttributeTableB::from_bits(self.vat_b[vat_index]),
            c: VertexAttributeTableC::from_bits(self.vat_c[vat_index]),
        };

        let arrays           = self.make_arrays();
        let default_matrices = DefaultMatrices::default();
        let ctx              = Ctx { ram, arrays: &arrays, default_matrices: &default_matrices };

        let stream = VertexAttributeStream::new(vat_index as u8, count, vertex_data.to_vec());

        let mut vertex_handles = alloc_vertices_handle(count as usize);
        let vertices_slice     = unsafe { vertex_handles.as_mut_slice() };

        let mut matrix_set = MatrixSet::default();
        let mut module     = InterpreterModule;
        module.parse(ctx, &vcd, &vat, &stream, vertices_slice, &mut matrix_set);

        let mut matrix_handles = alloc_matrices_handle(matrix_set.len());
        let matrices_slice     = unsafe { matrix_handles.as_mut_slice() };

        for (i, mat_id) in matrix_set.iter().enumerate() {
            let mat = self.read_xf_matrix(mat_id);
            matrices_slice[i].write((mat_id, mat));
        }

        // Flush pending XF viewport/projection state before drawing.
        self.flush_xf_actions(renderer);

        renderer.exec(Action::Draw(topology, VertexStream::new(vertex_handles, matrix_handles)));
    }

    /// Read a position or normal matrix from the XF register bank.
    #[cfg(target_arch = "wasm32")]
    fn read_xf_matrix(&self, mat_id: lazuli::system::gx::MatrixId) -> lazuli::system::gx::glam::Mat4 {
        if mat_id.is_normal() {
            let base = 0x0400 + mat_id.index() as usize * 3;
            if base + 9 <= XF_REG_COUNT {
                let m = lazuli::system::gx::glam::Mat3::from_cols_array(&[
                    f32::from_bits(self.xf_regs[base]),
                    f32::from_bits(self.xf_regs[base + 3]),
                    f32::from_bits(self.xf_regs[base + 6]),
                    f32::from_bits(self.xf_regs[base + 1]),
                    f32::from_bits(self.xf_regs[base + 4]),
                    f32::from_bits(self.xf_regs[base + 7]),
                    f32::from_bits(self.xf_regs[base + 2]),
                    f32::from_bits(self.xf_regs[base + 5]),
                    f32::from_bits(self.xf_regs[base + 8]),
                ]);
                return lazuli::system::gx::glam::Mat4::from_mat3(m);
            }
        } else {
            let base = mat_id.index() as usize * 4;
            if base + 12 <= XF_REG_COUNT {
                let r0 = (
                    f32::from_bits(self.xf_regs[base]),
                    f32::from_bits(self.xf_regs[base + 1]),
                    f32::from_bits(self.xf_regs[base + 2]),
                    f32::from_bits(self.xf_regs[base + 3]),
                );
                let r1 = (
                    f32::from_bits(self.xf_regs[base + 4]),
                    f32::from_bits(self.xf_regs[base + 5]),
                    f32::from_bits(self.xf_regs[base + 6]),
                    f32::from_bits(self.xf_regs[base + 7]),
                );
                let r2 = (
                    f32::from_bits(self.xf_regs[base + 8]),
                    f32::from_bits(self.xf_regs[base + 9]),
                    f32::from_bits(self.xf_regs[base + 10]),
                    f32::from_bits(self.xf_regs[base + 11]),
                );
                // Convert GC 3×4 row-major → glam 4×4 column-major.
                return lazuli::system::gx::glam::Mat4::from_cols_array(&[
                    r0.0, r1.0, r2.0, 0.0,
                    r0.1, r1.1, r2.1, 0.0,
                    r0.2, r1.2, r2.2, 0.0,
                    r0.3, r1.3, r2.3, 1.0,
                ]);
            }
        }
        lazuli::system::gx::glam::Mat4::IDENTITY
    }

    /// Build a `cmd::Arrays` struct from the CP array base/stride registers.
    #[cfg(target_arch = "wasm32")]
    fn make_arrays(&self) -> lazuli::system::gx::cmd::Arrays {
        use gekko::Address;
        use lazuli::system::gx::cmd::{ArrayDescriptor, Arrays};

        let ad = |i: usize| ArrayDescriptor {
            address: Address(self.cp_array_base[i]),
            stride:  self.cp_array_stride[i],
        };

        Arrays {
            position:        ad(0),
            normal:          ad(1),
            chan0:           ad(2),
            chan1:           ad(3),
            tex_coords:      [ad(4), ad(5), ad(6), ad(7), ad(8), ad(9), ad(10), ad(11)],
            general_purpose: [ad(12), ad(13), ad(14), ad(15)],
        }
    }

    // ─── Vertex size computation ──────────────────────────────────────────────

    /// Compute the byte size of a single vertex for VAT slot `vat`.
    pub(crate) fn vertex_size(&self, vat: usize) -> usize {
        let vat   = vat.min(7);
        let vcd_l = self.vcd_low;
        let vcd_h = self.vcd_high;
        let va    = self.vat_a[vat];
        let vb    = self.vat_b[vat];
        let vc    = self.vat_c[vat];

        let mut size = 0usize;

        if (vcd_l >> 0) & 1 != 0 { size += 1; }
        for i in 0..8usize {
            if (vcd_l >> (1 + i)) & 1 != 0 { size += 1; }
        }

        size += attr_size((vcd_l >> 9)  & 3, pos_direct_size(va));
        size += attr_size((vcd_l >> 11) & 3, norm_direct_size(va));
        size += attr_size((vcd_l >> 13) & 3, color_direct_size(va, false));
        size += attr_size((vcd_l >> 15) & 3, color_direct_size(va, true));

        for tc in 0..8usize {
            size += attr_size((vcd_h >> (tc * 2)) & 3, texcoord_direct_size(tc, va, vb, vc));
        }

        size
    }
}

// ─── Topology mapping ─────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
fn draw_topology(opcode: u8) -> lazuli::system::gx::Topology {
    use lazuli::system::gx::Topology;
    match opcode >> 3 {
        0b1_0000 => Topology::QuadList,
        0b1_0010 => Topology::TriangleList,
        0b1_0011 => Topology::TriangleStrip,
        0b1_0100 => Topology::TriangleFan,
        0b1_0101 => Topology::LineList,
        0b1_0110 => Topology::LineStrip,
        0b1_0111 => Topology::PointList,
        _        => Topology::TriangleList,
    }
}

// ─── Attribute size helpers ───────────────────────────────────────────────────

#[inline]
fn attr_size(mode: u32, direct_size: usize) -> usize {
    match mode {
        0 => 0,
        1 => direct_size,
        2 => 1,
        3 => 2,
        _ => 0,
    }
}

#[inline]
fn coords_fmt_size(fmt: u32) -> usize {
    match fmt { 0 | 1 => 1, 2 | 3 => 2, 4 => 4, _ => 1 }
}

#[inline]
fn pos_direct_size(va: u32) -> usize {
    let comps = if (va >> 0) & 1 == 0 { 2 } else { 3 };
    comps * coords_fmt_size((va >> 1) & 7)
}

#[inline]
fn norm_direct_size(va: u32) -> usize {
    let comps = if (va >> 9) & 1 == 0 { 3 } else { 9 };
    comps * coords_fmt_size((va >> 10) & 7)
}

#[inline]
fn color_direct_size(va: u32, chan1: bool) -> usize {
    let base = if chan1 { 17u32 } else { 13u32 };
    match (va >> (base + 1)) & 7 {
        0 | 3 => 2,
        1 | 4 => 3,
        _     => 4,
    }
}

#[inline]
fn texcoord_direct_size(tc: usize, va: u32, vb: u32, vc: u32) -> usize {
    let (kind, fmt) = match tc {
        0 => ((va >> 21) & 1, (va >> 22) & 7),
        1 => ((vb >>  0) & 1, (vb >>  1) & 7),
        2 => ((vb >>  9) & 1, (vb >> 10) & 7),
        3 => ((vb >> 18) & 1, (vb >> 19) & 7),
        4 => ((vb >> 27) & 1, (vb >> 28) & 7),
        5 => ((vc >>  5) & 1, (vc >>  6) & 7),
        6 => ((vc >> 14) & 1, (vc >> 15) & 7),
        7 => ((vc >> 23) & 1, (vc >> 24) & 7),
        _ => (0, 0),
    };
    (if kind == 0 { 1 } else { 2 }) * coords_fmt_size(fmt)
}
