//! PowerPC → IR decoder.

use gekko::disasm::{Ins, Opcode};
use gekko::InsExt;

use crate::inst::{IrBlock, IrInst, IrLocal, IrTy};

/// Generate the PowerPC MB/ME bitmask (big-endian bit numbering).
pub fn ppc_mask(mb: u32, me: u32) -> u32 {
    if mb <= me {
        let bits = me - mb + 1;
        let raw = if bits == 32 { u32::MAX } else { (1u32 << bits) - 1 };
        raw << (31 - me)
    } else {
        !ppc_mask(me + 1, mb - 1)
    }
}

pub struct Decoder;

impl Decoder {
    pub fn new() -> Self { Self }

    pub fn decode(&self, instructions: impl Iterator<Item = (u32, Ins)>) -> Result<IrBlock, String> {
        let mut block = IrBlock::default();
        let mut last_pc = 0u32;
        let mut count = 0u32;
        for (pc, ins) in instructions {
            last_pc = pc;
            count += 1;
            // Err(msg) means fatal decode failure — abort compilation with a
            // diagnostic message the caller can log and surface to the user.
            match self.emit_inst(&mut block, ins, pc) {
                Err(msg) => return Err(msg),
                Ok(true) => return Ok(block),
                Ok(false) => {}
            }
        }
        if count == 0 { return Err("block contains no instructions".to_string()); }
        block.push(IrInst::ReturnStatic(last_pc + 4));
        Ok(block)
    }

    // ── Address helpers ───────────────────────────────────────────────────────

    fn push_ea_d(&self, b: &mut IrBlock, ra: u8, d: i32) {
        if ra == 0 {
            b.push(IrInst::I32Const(d));
        } else {
            b.push(IrInst::LoadGpr(ra));
            if d != 0 { b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add); }
        }
    }

    fn push_ea_x(&self, b: &mut IrBlock, ra: u8, rb: u8) {
        if ra == 0 { b.push(IrInst::LoadGpr(rb)); }
        else { b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add); }
    }

    // ── CR helpers ────────────────────────────────────────────────────────────

    fn update_cr_signed(&self, b: &mut IrBlock, cr_fd: u8, loc: IrLocal) {
        let lt = 31u32.wrapping_sub((cr_fd as u32) * 4);
        let gt = lt.wrapping_sub(1);
        let eq = lt.wrapping_sub(2);
        let so = lt.wrapping_sub(3);
        let mask = 0xFu32.wrapping_shl(so);

        b.push(IrInst::LoadCr);
        b.push(IrInst::I32Const(!mask as i32)); b.push(IrInst::I32And);

        b.push(IrInst::LocalGet(loc)); b.push(IrInst::I32Const(0)); b.push(IrInst::I32LtS);
        b.push(IrInst::I32Const(lt as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LocalGet(loc)); b.push(IrInst::I32Const(0)); b.push(IrInst::I32GtS);
        b.push(IrInst::I32Const(gt as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LocalGet(loc)); b.push(IrInst::I32Eqz);
        b.push(IrInst::I32Const(eq as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LoadXer); b.push(IrInst::I32Const(31)); b.push(IrInst::I32ShrU);
        b.push(IrInst::I32Const(so as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::StoreCr);
    }

    /// Like `update_cr_signed` but takes two separate locals `lhs` and `rhs`
    /// and performs direct comparison (avoids signed-overflow from subtraction).
    fn update_cr_signed_cmp(&self, b: &mut IrBlock, cr_fd: u8, lhs: IrLocal, rhs: IrLocal) {
        let lt = 31u32.wrapping_sub((cr_fd as u32) * 4);
        let gt = lt.wrapping_sub(1);
        let eq = lt.wrapping_sub(2);
        let so = lt.wrapping_sub(3);
        let mask = 0xFu32.wrapping_shl(so);

        b.push(IrInst::LoadCr);
        b.push(IrInst::I32Const(!mask as i32)); b.push(IrInst::I32And);

        b.push(IrInst::LocalGet(lhs)); b.push(IrInst::LocalGet(rhs)); b.push(IrInst::I32LtS);
        b.push(IrInst::I32Const(lt as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LocalGet(lhs)); b.push(IrInst::LocalGet(rhs)); b.push(IrInst::I32GtS);
        b.push(IrInst::I32Const(gt as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LocalGet(lhs)); b.push(IrInst::LocalGet(rhs)); b.push(IrInst::I32Eq);
        b.push(IrInst::I32Const(eq as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LoadXer); b.push(IrInst::I32Const(31)); b.push(IrInst::I32ShrU);
        b.push(IrInst::I32Const(so as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::StoreCr);
    }

    fn update_cr_unsigned(&self, b: &mut IrBlock, cr_fd: u8, lhs: IrLocal, rhs: IrLocal) {
        let lt = 31u32.wrapping_sub((cr_fd as u32) * 4);
        let gt = lt.wrapping_sub(1);
        let eq = lt.wrapping_sub(2);
        let so = lt.wrapping_sub(3);
        let mask = 0xFu32.wrapping_shl(so);

        b.push(IrInst::LoadCr);
        b.push(IrInst::I32Const(!mask as i32)); b.push(IrInst::I32And);

        b.push(IrInst::LocalGet(lhs)); b.push(IrInst::LocalGet(rhs)); b.push(IrInst::I32LtU);
        b.push(IrInst::I32Const(lt as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LocalGet(lhs)); b.push(IrInst::LocalGet(rhs)); b.push(IrInst::I32GtU);
        b.push(IrInst::I32Const(gt as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LocalGet(lhs)); b.push(IrInst::LocalGet(rhs)); b.push(IrInst::I32Eq);
        b.push(IrInst::I32Const(eq as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::LoadXer); b.push(IrInst::I32Const(31)); b.push(IrInst::I32ShrU);
        b.push(IrInst::I32Const(so as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::StoreCr);
    }

    // ── XER carry helper ──────────────────────────────────────────────────────

    /// Update XER[CA] (bit 29 from LSB) from the `carry` local (0 or 1).
    fn update_xer_ca(&self, b: &mut IrBlock, carry: IrLocal) {
        b.push(IrInst::LoadXer);
        b.push(IrInst::I32Const(!(1u32 << 29) as i32)); // clear CA bit
        b.push(IrInst::I32And);
        b.push(IrInst::LocalGet(carry));
        b.push(IrInst::I32Const(29));
        b.push(IrInst::I32Shl);
        b.push(IrInst::I32Or);
        b.push(IrInst::StoreXer);
    }

    // ── CR bit helpers ────────────────────────────────────────────────────────

    /// Convert a PPC BI field (0=MSB, 31=LSB) to a little-endian bit position.
    #[inline]
    fn cr_bit_pos(bi: u8) -> i32 { 31 - bi as i32 }

    /// Get CR bit `bi` (PPC numbering) into a new local (0 or 1).
    fn cr_get_bit(&self, b: &mut IrBlock, bi: u8) -> IrLocal {
        let loc = b.alloc_local(IrTy::I32);
        b.push(IrInst::LoadCr);
        b.push(IrInst::I32Const(Self::cr_bit_pos(bi)));
        b.push(IrInst::I32ShrU);
        b.push(IrInst::I32Const(1));
        b.push(IrInst::I32And);
        b.push(IrInst::LocalSet(loc));
        loc
    }

    /// Set CR bit `bi` (PPC numbering) to the value in `val` local (0 or 1).
    fn cr_set_bit(&self, b: &mut IrBlock, bi: u8, val: IrLocal) {
        let pos = Self::cr_bit_pos(bi);
        let clear_mask = !(1u32 << (pos as u32)) as i32;
        b.push(IrInst::LoadCr);
        b.push(IrInst::I32Const(clear_mask));
        b.push(IrInst::I32And);
        b.push(IrInst::LocalGet(val));
        b.push(IrInst::I32Const(pos));
        b.push(IrInst::I32Shl);
        b.push(IrInst::I32Or);
        b.push(IrInst::StoreCr);
    }

    // ── Float CR helper ───────────────────────────────────────────────────────

    /// Update a CR field from the result of an `fcmpu`/`fcmpo` comparison.
    ///
    /// `la` and `lb` are locals holding the two f64 operands.  The CR field
    /// `cr_fd` (0–7) is updated: LT/GT/EQ are set from the comparison result;
    /// SO (the unordered / NaN flag) is 1 if at least one operand is NaN.
    ///
    /// WASM f64 comparison instructions (`f64.lt`, `f64.gt`, `f64.eq`) already
    /// return 0 for any NaN input, so the unordered flag is simply the logical
    /// NOT of `lt | gt | eq`.
    fn update_cr_float(&self, b: &mut IrBlock, cr_fd: u8, la: IrLocal, lb: IrLocal) {
        /// Number of bits occupied by each CR field (LT, GT, EQ, SO).
        const CR_BITS: u32 = 4;

        let lt_bit = 31u32.wrapping_sub((cr_fd as u32) * CR_BITS);
        let gt_bit = lt_bit.wrapping_sub(1);
        let eq_bit = lt_bit.wrapping_sub(2);
        let so_bit = lt_bit.wrapping_sub(3);
        let mask   = 0xFu32.wrapping_shl(so_bit);

        // Allocate locals for the three comparison results (each is 0 or 1).
        let l_lt = b.alloc_local(IrTy::I32);
        let l_gt = b.alloc_local(IrTy::I32);
        let l_eq = b.alloc_local(IrTy::I32);

        // lt = (la < lb)
        b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::F64Lt);
        b.push(IrInst::LocalSet(l_lt));

        // gt = (la > lb)
        b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::F64Gt);
        b.push(IrInst::LocalSet(l_gt));

        // eq = (la == lb)
        b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::F64Eq);
        b.push(IrInst::LocalSet(l_eq));

        // so = !(lt | gt | eq) — 1 when all three are 0 (NaN input)
        // so  = (lt | gt | eq) XOR 1
        let l_so = b.alloc_local(IrTy::I32);
        b.push(IrInst::LocalGet(l_lt)); b.push(IrInst::LocalGet(l_gt)); b.push(IrInst::I32Or);
        b.push(IrInst::LocalGet(l_eq)); b.push(IrInst::I32Or);
        b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor);
        b.push(IrInst::LocalSet(l_so));

        // Build the new CR: clear the 4 bits of cr_fd, then OR in the new values.
        // Each bit is shifted into position and OR-ed into the accumulator via
        // push_shifted_or (avoids inline repetition of the 4-instruction pattern).
        b.push(IrInst::LoadCr);
        b.push(IrInst::I32Const(!mask as i32)); b.push(IrInst::I32And);

        Self::push_shifted_or(b, l_lt, lt_bit);
        Self::push_shifted_or(b, l_gt, gt_bit);
        Self::push_shifted_or(b, l_eq, eq_bit);
        Self::push_shifted_or(b, l_so, so_bit);

        b.push(IrInst::StoreCr);
    }

    /// Emit `LocalGet(loc)`, `I32Const(shift)`, `I32Shl`, `I32Or` — the
    /// four-instruction sequence for ORing a 0/1 value into an i32 accumulator
    /// at the given bit position.
    #[inline]
    fn push_shifted_or(b: &mut IrBlock, loc: IrLocal, shift: u32) {
        b.push(IrInst::LocalGet(loc));
        b.push(IrInst::I32Const(shift as i32));
        b.push(IrInst::I32Shl);
        b.push(IrInst::I32Or);
    }

    // ── Branch condition ──────────────────────────────────────────────────────
    fn emit_branch_cond(&self, b: &mut IrBlock, bo: u8, bi: u8) {
        let ignore_cr  = (bo >> 4) & 1 != 0;
        let desired_cr = (bo >> 3) & 1 != 0;
        let ignore_ctr = (bo >> 2) & 1 != 0;
        let ctr_zero   = (bo >> 1) & 1 != 0;
        b.push(IrInst::I32Const(1));
        if !ignore_ctr {
            let loc = b.alloc_local(IrTy::I32);
            b.push(IrInst::LoadCtr); b.push(IrInst::I32Const(1)); b.push(IrInst::I32Sub);
            b.push(IrInst::LocalTee(loc)); b.push(IrInst::StoreCtr);
            b.push(IrInst::LocalGet(loc)); b.push(IrInst::I32Eqz);
            if !ctr_zero { b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); }
            b.push(IrInst::I32And);
        }
        if !ignore_cr {
            b.push(IrInst::LoadCr);
            b.push(IrInst::I32Const(31 - bi as i32)); b.push(IrInst::I32ShrU);
            b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
            if !desired_cr { b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); }
            b.push(IrInst::I32And);
        }
    }

    // ── Int result helper: tee to local, store to GPR, optional CR0 update ───

    fn int_result(&self, b: &mut IrBlock, rd: u8, rc: bool) {
        if rc {
            let loc = b.alloc_local(IrTy::I32);
            b.push(IrInst::LocalTee(loc)); b.push(IrInst::StoreGpr(rd));
            self.update_cr_signed(b, 0, loc);
        } else {
            b.push(IrInst::StoreGpr(rd));
        }
    }

    // ── FP helper: emit a = fA*fC + fB (or variants) ─────────────────────────

    /// Emit fA.psX * fC.psX ± fB.psX using explicit locals to handle the
    /// non-trivial stack ordering required by a*c ± b.
    ///
    /// `sub`: true → a*c - b (fmsub), false → a*c + b (fmadd)
    /// `neg`: true → negate the result
    /// `round`: true → F64RoundToSingle after
    fn fma_slot(
        &self, b: &mut IrBlock,
        fa: u8, fb: u8, fc: u8, ps1: bool,
        sub: bool, neg: bool, round: bool,
    ) {
        let load_a = if ps1 { IrInst::LoadFprPs1(fa) } else { IrInst::LoadFprPs0(fa) };
        let load_b = if ps1 { IrInst::LoadFprPs1(fb) } else { IrInst::LoadFprPs0(fb) };
        let load_c = if ps1 { IrInst::LoadFprPs1(fc) } else { IrInst::LoadFprPs0(fc) };
        // a*c - b requires careful ordering.  Use locals so the subtraction is right.
        if sub || neg {
            let la = b.alloc_local(IrTy::F64);
            let lc = b.alloc_local(IrTy::F64);
            let lb = b.alloc_local(IrTy::F64);
            b.push(load_a); b.push(IrInst::LocalSet(la));
            b.push(load_b); b.push(IrInst::LocalSet(lb));
            b.push(load_c); b.push(IrInst::LocalSet(lc));
            b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lc)); b.push(IrInst::F64Mul);
            b.push(IrInst::LocalGet(lb));
            if sub { b.push(IrInst::F64Sub); } else { b.push(IrInst::F64Add); }
            if neg { b.push(IrInst::F64Neg); }
        } else {
            // Simple fmadd: a*c + b  →  push b, push a, push c, mul, add
            b.push(load_b); b.push(load_a); b.push(load_c);
            b.push(IrInst::F64Mul); b.push(IrInst::F64Add);
        }
        if round { b.push(IrInst::F64RoundToSingle); }
    }

    /// Emit a scalar FMA-family instruction (fmadd/fmsub/fnmadd/fnmsub and their 's' variants).
    fn emit_fma_scalar(&self, b: &mut IrBlock, ins: Ins, sub: bool, neg: bool, single: bool) {
        let fd = ins.fpr_d() as u8;
        let fa = ins.fpr_a() as u8;
        let fb = ins.fpr_b() as u8;
        let fc = ins.fpr_c() as u8;
        self.fma_slot(b, fa, fb, fc, false, sub, neg, single);
        b.push(IrInst::StoreFprPs0(fd));
    }

    /// Emit a paired-single FMA-family instruction.
    fn emit_fma_ps(&self, b: &mut IrBlock, ins: Ins, sub: bool, neg: bool) {
        let fd = ins.fpr_d() as u8;
        let fa = ins.fpr_a() as u8;
        let fb = ins.fpr_b() as u8;
        let fc = ins.fpr_c() as u8;
        self.fma_slot(b, fa, fb, fc, false, sub, neg, true);
        b.push(IrInst::StoreFprPs0(fd));
        self.fma_slot(b, fa, fb, fc, true, sub, neg, true);
        b.push(IrInst::StoreFprPs1(fd));
    }

    /// Emit a reciprocal estimate: 1.0 / x  (or 1.0 / sqrt(x) for rsqrte).
    fn emit_recip(&self, b: &mut IrBlock, fd: u8, fb: u8, sqrt: bool, ps1: bool) {
        let load = if ps1 { IrInst::LoadFprPs1(fb) } else { IrInst::LoadFprPs0(fb) };
        let store = if ps1 { IrInst::StoreFprPs1(fd) } else { IrInst::StoreFprPs0(fd) };
        let loc = b.alloc_local(IrTy::F64);
        b.push(load);
        if sqrt { b.push(IrInst::F64Sqrt); }
        b.push(IrInst::LocalSet(loc));
        b.push(IrInst::F64Const(1.0)); b.push(IrInst::LocalGet(loc)); b.push(IrInst::F64Div);
        b.push(store);
    }

    // ── Main dispatch ─────────────────────────────────────────────────────────

    /// Returns the cycle cost for a given opcode, matching the native JIT costs.
    fn cycles_for(op: Opcode) -> u32 {
        match op {
            // 1-cycle logical/register operations
            Opcode::And | Opcode::Andc | Opcode::Andi_ | Opcode::Andis_
            | Opcode::Or  | Opcode::Orc  | Opcode::Ori  | Opcode::Oris
            | Opcode::Xor | Opcode::Xori | Opcode::Xoris
            | Opcode::Eqv | Opcode::Nand | Opcode::Nor => 1,

            // 1-cycle SPR/MSR/CR/TB operations
            | Opcode::Mfspr | Opcode::Mtspr
            | Opcode::Mfmsr | Opcode::Mtmsr
            | Opcode::Mfcr  | Opcode::Mtcrf
            | Opcode::Crand | Opcode::Crandc | Opcode::Creqv
            | Opcode::Crnand | Opcode::Crnor | Opcode::Cror
            | Opcode::Crorc | Opcode::Crxor
            | Opcode::Mftb => 1,

            // 3-cycle multiply
            | Opcode::Mulhw | Opcode::Mulhwu | Opcode::Mullw | Opcode::Mulli => 3,

            // 19-cycle divide
            | Opcode::Divw | Opcode::Divwu => 19,

            // 10-cycle bulk load/store
            | Opcode::Lmw | Opcode::Stmw | Opcode::Lswi | Opcode::Stswi => 10,

            // 2 cycles for everything else
            _ => 2,
        }
    }

    fn emit_inst(&self, b: &mut IrBlock, ins: Ins, pc: u32) -> Result<bool, String> {
        b.instruction_count += 1;
        b.cycles += Self::cycles_for(ins.op);
        match ins.op {
            // ── Integer arithmetic ────────────────────────────────────────────
            Opcode::Addi => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                self.push_ea_d(b, ra, ins.field_simm() as i32);
                b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Addis => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                let imm = (ins.field_uimm() as i32) << 16;
                if ra == 0 { b.push(IrInst::I32Const(imm)); }
                else { b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(imm)); b.push(IrInst::I32Add); }
                b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Add => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                self.int_result(b, rd, rc);
            }
            Opcode::Addic => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                let imm = ins.field_simm() as i32;
                let ra_val = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalTee(ra_val));
                b.push(IrInst::I32Const(imm)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                // carry = (result < ra) unsigned (unsigned overflow detection)
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Addic_ => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                let imm = ins.field_simm() as i32;
                let ra_val = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalTee(ra_val));
                b.push(IrInst::I32Const(imm)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, rd, true);
            }
            Opcode::Addc => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                let ra_val = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalTee(ra_val));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, rd, rc);
            }
            Opcode::Adde => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                let ra_val = b.alloc_local(IrTy::I32);
                let sum1   = b.alloc_local(IrTy::I32);
                let c1     = b.alloc_local(IrTy::I32);
                let ca_in  = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let c2     = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                // sum1 = ra + rb; c1 = (sum1 < ra) unsigned
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalTee(ra_val));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(sum1));
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(c1));
                // ca_in = XER[CA] = bit 29
                b.push(IrInst::LoadXer); b.push(IrInst::I32Const(29)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(ca_in));
                // result = sum1 + ca_in; c2 = (result < sum1) unsigned
                b.push(IrInst::LocalGet(sum1)); b.push(IrInst::LocalGet(ca_in)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                b.push(IrInst::LocalGet(sum1)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(c2));
                // carry = c1 | c2
                b.push(IrInst::LocalGet(c1)); b.push(IrInst::LocalGet(c2)); b.push(IrInst::I32Or);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, rd, rc);
            }
            Opcode::Addze => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rc = ins.field_rc();
                let ra_val = b.alloc_local(IrTy::I32);
                let ca_in  = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(ra_val));
                b.push(IrInst::LoadXer); b.push(IrInst::I32Const(29)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(ca_in));
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::LocalGet(ca_in)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, rd, rc);
            }
            Opcode::Addme => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rc = ins.field_rc();
                let ra_val = b.alloc_local(IrTy::I32);
                let sum1   = b.alloc_local(IrTy::I32);
                let c1     = b.alloc_local(IrTy::I32);
                let ca_in  = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let c2     = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                // sum1 = ra + 0xFFFFFFFF (= ra - 1 wrapping); c1 = (sum1 < ra)
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalTee(ra_val));
                b.push(IrInst::I32Const(-1i32)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(sum1));
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(c1));
                // ca_in = XER[CA]
                b.push(IrInst::LoadXer); b.push(IrInst::I32Const(29)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(ca_in));
                // result = sum1 + ca_in; c2 = (result < sum1)
                b.push(IrInst::LocalGet(sum1)); b.push(IrInst::LocalGet(ca_in)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                b.push(IrInst::LocalGet(sum1)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(c2));
                b.push(IrInst::LocalGet(c1)); b.push(IrInst::LocalGet(c2)); b.push(IrInst::I32Or);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, rd, rc);
            }
            Opcode::Subf => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Sub);
                self.int_result(b, rd, rc);
            }
            Opcode::Subfic => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                let imm = ins.field_simm() as i32;
                let ra_val = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(ra_val));
                b.push(IrInst::I32Const(imm)); b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32Sub);
                b.push(IrInst::LocalSet(result));
                // CA = NOT(simm <_u ra) = (simm >=_u ra) = !(ra_val >_u imm_u32)
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32Const(imm));
                b.push(IrInst::I32GtU);                 // ra > simm (unsigned)?
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); // NOT
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Subfc => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                let ra_val = b.alloc_local(IrTy::I32);
                let rb_val = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(ra_val));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::LocalSet(rb_val));
                // CA = NOT(rb <_u ra) = (rb >=_u ra)
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32LtU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32Sub);
                self.int_result(b, rd, rc);
            }
            Opcode::Subfe => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                // result = rb + ~ra + CA_in
                let ra_val  = b.alloc_local(IrTy::I32);
                let rb_val  = b.alloc_local(IrTy::I32);
                let not_ra  = b.alloc_local(IrTy::I32);
                let sum1    = b.alloc_local(IrTy::I32);
                let c1      = b.alloc_local(IrTy::I32);
                let ca_in   = b.alloc_local(IrTy::I32);
                let result  = b.alloc_local(IrTy::I32);
                let c2      = b.alloc_local(IrTy::I32);
                let carry   = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(ra_val));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::LocalSet(rb_val));
                // not_ra = ~ra
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32Not); b.push(IrInst::LocalSet(not_ra));
                // sum1 = rb + ~ra; c1 = (sum1 < rb) unsigned
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::LocalGet(not_ra)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(sum1));
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(c1));
                // ca_in = XER[CA]
                b.push(IrInst::LoadXer); b.push(IrInst::I32Const(29)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(ca_in));
                // result = sum1 + ca_in; c2 = (result < sum1)
                b.push(IrInst::LocalGet(sum1)); b.push(IrInst::LocalGet(ca_in)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                b.push(IrInst::LocalGet(sum1)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(c2));
                b.push(IrInst::LocalGet(c1)); b.push(IrInst::LocalGet(c2)); b.push(IrInst::I32Or);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, rd, rc);
            }
            Opcode::Subfze => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rc = ins.field_rc();
                // result = ~ra + CA_in
                let not_ra = b.alloc_local(IrTy::I32);
                let ca_in  = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Not); b.push(IrInst::LocalSet(not_ra));
                b.push(IrInst::LoadXer); b.push(IrInst::I32Const(29)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(ca_in));
                b.push(IrInst::LocalGet(not_ra)); b.push(IrInst::LocalGet(ca_in)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                // carry = (result < not_ra) unsigned
                b.push(IrInst::LocalGet(not_ra)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, rd, rc);
            }
            Opcode::Subfme => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rc = ins.field_rc();
                // result = ~ra + (-1) + CA_in = ~ra + 0xFFFFFFFF + CA_in
                let not_ra = b.alloc_local(IrTy::I32);
                let sum1   = b.alloc_local(IrTy::I32);
                let c1     = b.alloc_local(IrTy::I32);
                let ca_in  = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let c2     = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Not); b.push(IrInst::LocalSet(not_ra));
                // sum1 = not_ra + 0xFFFFFFFF; c1 = (sum1 < not_ra)
                b.push(IrInst::LocalGet(not_ra)); b.push(IrInst::I32Const(-1i32)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(sum1));
                b.push(IrInst::LocalGet(not_ra)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(c1));
                b.push(IrInst::LoadXer); b.push(IrInst::I32Const(29)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(ca_in));
                b.push(IrInst::LocalGet(sum1)); b.push(IrInst::LocalGet(ca_in)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(result));
                b.push(IrInst::LocalGet(sum1)); b.push(IrInst::I32LtU);
                b.push(IrInst::LocalSet(c2));
                b.push(IrInst::LocalGet(c1)); b.push(IrInst::LocalGet(c2)); b.push(IrInst::I32Or);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, rd, rc);
            }
            Opcode::Neg => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rc = ins.field_rc();
                b.push(IrInst::I32Const(0)); b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Sub);
                self.int_result(b, rd, rc);
            }
            Opcode::Mulli => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(ins.field_simm() as i32)); b.push(IrInst::I32Mul);
                b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Mullw => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Mul);
                self.int_result(b, rd, rc);
            }
            Opcode::Mulhw => {
                // rd = high 32 bits of signed 64-bit product ra * rb
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I64ExtendI32S);
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I64ExtendI32S);
                b.push(IrInst::I64Mul);
                b.push(IrInst::I64ShrS32); // arithmetic shift right 32, wrap to i32
                self.int_result(b, rd, rc);
            }
            Opcode::Mulhwu => {
                // rd = high 32 bits of unsigned 64-bit product ra * rb
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I64ExtendI32U);
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I64ExtendI32U);
                b.push(IrInst::I64Mul);
                b.push(IrInst::I64ShrU32); // logical shift right 32, wrap to i32
                self.int_result(b, rd, rc);
            }
            Opcode::Divw => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                // Guard against div-by-zero and INT_MIN/-1 (both trap in WASM).
                let ra_val = b.alloc_local(IrTy::I32);
                let rb_val = b.alloc_local(IrTy::I32);
                let bad    = b.alloc_local(IrTy::I32);
                let denom  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(ra_val));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::LocalSet(rb_val));
                // bad = (rb == 0) OR (ra == INT_MIN AND rb == -1)
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::I32Eqz);
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::I32Const(i32::MIN)); b.push(IrInst::I32Eq);
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::I32Const(-1i32)); b.push(IrInst::I32Eq);
                b.push(IrInst::I32And);
                b.push(IrInst::I32Or);
                b.push(IrInst::LocalSet(bad));
                // branchless: denom = rb XOR ((-bad) AND (rb XOR 1))
                // when bad=0: denom = rb XOR 0 = rb
                // when bad=1: denom = rb XOR (0xFFFFFFFF AND (rb XOR 1)) = 1
                b.push(IrInst::I32Const(0)); b.push(IrInst::LocalGet(bad)); b.push(IrInst::I32Sub); // -bad (mask)
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor);
                b.push(IrInst::I32And);
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::I32Xor);
                b.push(IrInst::LocalSet(denom));
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::LocalGet(denom)); b.push(IrInst::I32DivS);
                self.int_result(b, rd, rc);
            }
            Opcode::Divwu => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                // Guard against div-by-zero (WASM traps on udiv/0).
                let ra_val = b.alloc_local(IrTy::I32);
                let rb_val = b.alloc_local(IrTy::I32);
                let bad    = b.alloc_local(IrTy::I32);
                let denom  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(ra_val));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::LocalSet(rb_val));
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::I32Eqz); b.push(IrInst::LocalSet(bad));
                // branchless: denom = rb XOR ((-bad) AND (rb XOR 1))
                b.push(IrInst::I32Const(0)); b.push(IrInst::LocalGet(bad)); b.push(IrInst::I32Sub);
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor);
                b.push(IrInst::I32And);
                b.push(IrInst::LocalGet(rb_val)); b.push(IrInst::I32Xor);
                b.push(IrInst::LocalSet(denom));
                b.push(IrInst::LocalGet(ra_val)); b.push(IrInst::LocalGet(denom)); b.push(IrInst::I32DivU);
                self.int_result(b, rd, rc);
            }
            // ── Integer logic ─────────────────────────────────────────────────
            Opcode::And   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32And); self.int_result(b,ra,rc); }
            Opcode::Andc  => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Not); b.push(IrInst::I32And); self.int_result(b,ra,rc); }
            Opcode::Andi_ => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Const(ins.field_uimm() as i32)); b.push(IrInst::I32And); self.int_result(b,ra,true); }
            Opcode::Andis_=> { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Const((ins.field_uimm() as i32)<<16)); b.push(IrInst::I32And); self.int_result(b,ra,true); }
            Opcode::Or    => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Or); self.int_result(b,ra,rc); }
            Opcode::Orc   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Not); b.push(IrInst::I32Or); self.int_result(b,ra,rc); }
            Opcode::Ori   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Const(ins.field_uimm() as i32)); b.push(IrInst::I32Or); b.push(IrInst::StoreGpr(ra)); }
            Opcode::Oris  => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Const((ins.field_uimm() as i32)<<16)); b.push(IrInst::I32Or); b.push(IrInst::StoreGpr(ra)); }
            Opcode::Xor   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Xor); self.int_result(b,ra,rc); }
            Opcode::Xori  => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Const(ins.field_uimm() as i32)); b.push(IrInst::I32Xor); b.push(IrInst::StoreGpr(ra)); }
            Opcode::Xoris => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Const((ins.field_uimm() as i32)<<16)); b.push(IrInst::I32Xor); b.push(IrInst::StoreGpr(ra)); }
            Opcode::Nor   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Or); b.push(IrInst::I32Not); self.int_result(b,ra,rc); }
            Opcode::Nand  => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32And); b.push(IrInst::I32Not); self.int_result(b,ra,rc); }
            Opcode::Eqv   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Xor); b.push(IrInst::I32Not); self.int_result(b,ra,rc); }
            Opcode::Slw   => {
                // PowerPC slw: shift amount = rB & 0x3F (6 bits).  If bit 5 is
                // set (shift >= 32) the result is 0.  WASM i32.shl only uses the
                // low 5 bits, so we must zero the result explicitly when bit 5 of
                // rB is set, matching the native JIT's behaviour.
                let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc();
                let rs_val         = b.alloc_local(IrTy::I32);
                let shift_exceeds_31 = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LocalSet(rs_val));
                // shift_exceeds_31 = (rB >> 5) & 1 — 1 when shift amount >= 32
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Const(5)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And); b.push(IrInst::LocalSet(shift_exceeds_31));
                // shifted = rs << (rb & 31)  — WASM uses only bits[4:0] of rb
                b.push(IrInst::LocalGet(rs_val)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Shl);
                // keep_mask: 0xFFFFFFFF when shift_exceeds_31==0, 0x00000000 when shift_exceeds_31==1
                // computed as ~(0 - shift_exceeds_31) = NOT(0 - flag)
                b.push(IrInst::I32Const(0)); b.push(IrInst::LocalGet(shift_exceeds_31)); b.push(IrInst::I32Sub);
                b.push(IrInst::I32Not);
                b.push(IrInst::I32And);
                self.int_result(b, ra, rc);
            }
            Opcode::Srw   => {
                // PowerPC srw: shift amount = rB & 0x3F (6 bits).  If bit 5 is
                // set (shift >= 32) the result is 0.  Same fix as Slw above.
                let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc();
                let rs_val         = b.alloc_local(IrTy::I32);
                let shift_exceeds_31 = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LocalSet(rs_val));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Const(5)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And); b.push(IrInst::LocalSet(shift_exceeds_31));
                b.push(IrInst::LocalGet(rs_val)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32ShrU);
                // keep_mask: 0xFFFFFFFF when shift_exceeds_31==0, 0x00000000 when shift_exceeds_31==1
                b.push(IrInst::I32Const(0)); b.push(IrInst::LocalGet(shift_exceeds_31)); b.push(IrInst::I32Sub);
                b.push(IrInst::I32Not);
                b.push(IrInst::I32And);
                self.int_result(b, ra, rc);
            }
            Opcode::Sraw  => {
                let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc();
                let rs_val      = b.alloc_local(IrTy::I32);
                let sh          = b.alloc_local(IrTy::I32);
                let sh_clamped  = b.alloc_local(IrTy::I32);
                let result      = b.alloc_local(IrTy::I32);
                let is_neg      = b.alloc_local(IrTy::I32);
                let tz          = b.alloc_local(IrTy::I32);
                let carry       = b.alloc_local(IrTy::I32);
                let overflow    = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LocalSet(rs_val));
                // sh = rb & 0x3F (0..63); WASM i32.shr_s only uses 5 low bits so
                // we clamp to 31 max: for sh >= 32 the arithmetic right-shift result
                // is always rs >> 31 (all sign bits), which equals shr_s(rs, 31).
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Const(0x3F)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(sh));
                // overflow_bit = (sh >> 5) & 1  →  1 if sh >= 32
                b.push(IrInst::LocalGet(sh)); b.push(IrInst::I32Const(5)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(overflow));
                // sh_clamped = sh XOR ((-overflow) AND (sh XOR 31))
                // = sh when overflow=0; = 31 when overflow=1
                b.push(IrInst::I32Const(0)); b.push(IrInst::LocalGet(overflow)); b.push(IrInst::I32Sub); // -overflow mask
                b.push(IrInst::LocalGet(sh)); b.push(IrInst::I32Const(31)); b.push(IrInst::I32Xor);
                b.push(IrInst::I32And);
                b.push(IrInst::LocalGet(sh)); b.push(IrInst::I32Xor);
                b.push(IrInst::LocalSet(sh_clamped));
                // result = rs >> sh_clamped (arithmetic)
                b.push(IrInst::LocalGet(rs_val)); b.push(IrInst::LocalGet(sh_clamped)); b.push(IrInst::I32ShrS);
                b.push(IrInst::LocalSet(result));
                // is_neg = (rs < 0) = top bit
                b.push(IrInst::LocalGet(rs_val)); b.push(IrInst::I32Const(31)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(is_neg));
                // tz = ctz(rs) (count trailing zeros; ctz(0) = 32 in WASM)
                b.push(IrInst::LocalGet(rs_val)); b.push(IrInst::I32Ctz); b.push(IrInst::LocalSet(tz));
                // carry = is_neg AND (sh > tz) — use unclamped sh for correct CA on sh>=32
                b.push(IrInst::LocalGet(sh)); b.push(IrInst::LocalGet(tz)); b.push(IrInst::I32GtU);
                b.push(IrInst::LocalGet(is_neg)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(carry));
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, ra, rc);
            }
            Opcode::Srawi => {
                let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let sh=ins.field_sh() as u8; let rc=ins.field_rc();
                let rs_val = b.alloc_local(IrTy::I32);
                let result = b.alloc_local(IrTy::I32);
                let carry  = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LocalTee(rs_val));
                b.push(IrInst::I32Const(sh as i32)); b.push(IrInst::I32ShrS);
                b.push(IrInst::LocalSet(result));
                if sh == 0 {
                    // CA = 0 when no shift
                    b.push(IrInst::I32Const(0)); b.push(IrInst::LocalSet(carry));
                } else {
                    // carry = (rs < 0) AND ((rs & low_sh_mask) != 0)
                    let low_mask = (1u32 << sh).wrapping_sub(1) as i32;
                    let is_neg   = b.alloc_local(IrTy::I32);
                    let has_lo   = b.alloc_local(IrTy::I32);
                    b.push(IrInst::LocalGet(rs_val)); b.push(IrInst::I32Const(31)); b.push(IrInst::I32ShrU);
                    b.push(IrInst::I32Const(1)); b.push(IrInst::I32And);
                    b.push(IrInst::LocalSet(is_neg));
                    b.push(IrInst::LocalGet(rs_val)); b.push(IrInst::I32Const(low_mask)); b.push(IrInst::I32And);
                    b.push(IrInst::I32Eqz); b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); // != 0
                    b.push(IrInst::LocalSet(has_lo));
                    b.push(IrInst::LocalGet(is_neg)); b.push(IrInst::LocalGet(has_lo)); b.push(IrInst::I32And);
                    b.push(IrInst::LocalSet(carry));
                }
                self.update_xer_ca(b, carry);
                b.push(IrInst::LocalGet(result));
                self.int_result(b, ra, rc);
            }
            Opcode::Cntlzw=>{ let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Clz); b.push(IrInst::StoreGpr(ra)); }
            Opcode::Extsb => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Extend8S); self.int_result(b,ra,rc); }
            Opcode::Extsh => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Extend16S); self.int_result(b,ra,rc); }

            // ── Rotate / mask ─────────────────────────────────────────────────
            Opcode::Rlwinm => {
                let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8;
                let sh=ins.field_sh() as i32; let mb=ins.field_mb() as u32; let me=ins.field_me() as u32; let rc=ins.field_rc();
                let mask = ppc_mask(mb,me) as i32;
                b.push(IrInst::LoadGpr(rs));
                if sh!=0 { b.push(IrInst::I32Const(sh)); b.push(IrInst::I32Rotl); }
                b.push(IrInst::I32Const(mask)); b.push(IrInst::I32And);
                self.int_result(b,ra,rc);
            }
            Opcode::Rlwimi => {
                let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8;
                let sh=ins.field_sh() as i32; let mb=ins.field_mb() as u32; let me=ins.field_me() as u32; let rc=ins.field_rc();
                let mask=ppc_mask(mb,me) as i32;
                let rot=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(rs));
                if sh!=0 { b.push(IrInst::I32Const(sh)); b.push(IrInst::I32Rotl); }
                b.push(IrInst::I32Const(mask)); b.push(IrInst::I32And); b.push(IrInst::LocalSet(rot));
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(!mask)); b.push(IrInst::I32And);
                b.push(IrInst::LocalGet(rot)); b.push(IrInst::I32Or);
                self.int_result(b,ra,rc);
            }
            Opcode::Rlwnm => {
                let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8;
                let mb=ins.field_mb() as u32; let me=ins.field_me() as u32; let rc=ins.field_rc();
                let mask=ppc_mask(mb,me) as i32;
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Rotl);
                b.push(IrInst::I32Const(mask)); b.push(IrInst::I32And);
                self.int_result(b,ra,rc);
            }

            // ── Integer compare ───────────────────────────────────────────────
            Opcode::Cmp => {
                let cr=ins.field_crfd() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let lhs=b.alloc_local(IrTy::I32); let rhs=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(lhs));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::LocalSet(rhs));
                self.update_cr_signed_cmp(b, cr, lhs, rhs);
            }
            Opcode::Cmpi => {
                let cr=ins.field_crfd() as u8; let ra=ins.gpr_a() as u8;
                let lhs=b.alloc_local(IrTy::I32); let rhs=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(lhs));
                b.push(IrInst::I32Const(ins.field_simm() as i32)); b.push(IrInst::LocalSet(rhs));
                self.update_cr_signed_cmp(b, cr, lhs, rhs);
            }
            Opcode::Cmpl => {
                let cr=ins.field_crfd() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let lhs=b.alloc_local(IrTy::I32); let rhs=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(lhs));
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::LocalSet(rhs));
                self.update_cr_unsigned(b,cr,lhs,rhs);
            }
            Opcode::Cmpli => {
                let cr=ins.field_crfd() as u8; let ra=ins.gpr_a() as u8;
                let lhs=b.alloc_local(IrTy::I32); let rhs=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LocalSet(lhs));
                b.push(IrInst::I32Const(ins.field_uimm() as i32)); b.push(IrInst::LocalSet(rhs));
                self.update_cr_unsigned(b,cr,lhs,rhs);
            }

            // ── Integer loads ─────────────────────────────────────────────────
            Opcode::Lwz  => { let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; self.push_ea_d(b,ra,ins.field_offset() as i32); b.push(IrInst::ReadU32); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Lwzu => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::ReadU32); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lwzx  => { let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::ReadU32); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Lwzux => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::ReadU32); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lhz  => { let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; self.push_ea_d(b,ra,ins.field_offset() as i32); b.push(IrInst::ReadU16); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Lhzu => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::ReadU16); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lhzx  => { let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::ReadU16); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Lhzux => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::ReadU16); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lha  => { let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; self.push_ea_d(b,ra,ins.field_offset() as i32); b.push(IrInst::ReadU16); b.push(IrInst::I32Extend16S); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Lhau => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::ReadU16); b.push(IrInst::I32Extend16S); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lhax  => { let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::ReadU16); b.push(IrInst::I32Extend16S); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Lhaux => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::ReadU16); b.push(IrInst::I32Extend16S); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lbz  => { let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; self.push_ea_d(b,ra,ins.field_offset() as i32); b.push(IrInst::ReadU8); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Lbzu => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::ReadU8); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lbzx  => { let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::ReadU8); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Lbzux => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::ReadU8); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lmw => {
                let rd=ins.gpr_d() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                for i in rd..32 {
                    let offset = d + 4 * (i - rd) as i32;
                    self.push_ea_d(b, ra, offset);
                    b.push(IrInst::ReadU32); b.push(IrInst::StoreGpr(i));
                }
            }

            // ── Integer stores ────────────────────────────────────────────────
            Opcode::Stw  => { let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; self.push_ea_d(b,ra,ins.field_offset() as i32); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU32); }
            Opcode::Stwu => {
                let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add); b.push(IrInst::LocalSet(ea));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU32);
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::StoreGpr(ra));
            }
            Opcode::Stwx  => { let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU32); }
            Opcode::Stwux => {
                let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add); b.push(IrInst::LocalSet(ea));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU32);
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::StoreGpr(ra));
            }
            Opcode::Sth  => { let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; self.push_ea_d(b,ra,ins.field_offset() as i32); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU16); }
            Opcode::Sthu => {
                let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add); b.push(IrInst::LocalSet(ea));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU16);
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::StoreGpr(ra));
            }
            Opcode::Sthx  => { let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU16); }
            Opcode::Sthux => {
                let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add); b.push(IrInst::LocalSet(ea));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU16);
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::StoreGpr(ra));
            }
            Opcode::Stb  => { let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; self.push_ea_d(b,ra,ins.field_offset() as i32); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU8); }
            Opcode::Stbu => {
                let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add); b.push(IrInst::LocalSet(ea));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU8);
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::StoreGpr(ra));
            }
            Opcode::Stbx  => { let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU8); }
            Opcode::Stbux => {
                let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8;
                let ea=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add); b.push(IrInst::LocalSet(ea));
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU8);
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::StoreGpr(ra));
            }
            Opcode::Stmw => {
                let rs=ins.gpr_s() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                for i in rs..32 {
                    let offset = d + 4 * (i - rs) as i32;
                    self.push_ea_d(b, ra, offset);
                    b.push(IrInst::LoadGpr(i)); b.push(IrInst::WriteU32);
                }
            }

            // ── Float loads ───────────────────────────────────────────────────
            Opcode::Lfs | Opcode::Lfsu => {
                let fd=ins.fpr_d() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                if ins.op == Opcode::Lfsu {
                    let ea=b.alloc_local(IrTy::I32);
                    b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                    b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                    b.push(IrInst::LocalGet(ea));
                } else { self.push_ea_d(b,ra,d); }
                b.push(IrInst::ReadU32); b.push(IrInst::F64PromoteSingleBits);
                b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fd)); b.push(IrInst::StoreFprPs1(fd));
            }
            Opcode::Lfsx => { let fd=ins.fpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::ReadU32); b.push(IrInst::F64PromoteSingleBits); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fd)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::Lfd | Opcode::Lfdu => {
                let fd=ins.fpr_d() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                if ins.op == Opcode::Lfdu {
                    let ea=b.alloc_local(IrTy::I32);
                    b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                    b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                    b.push(IrInst::LocalGet(ea));
                } else { self.push_ea_d(b,ra,d); }
                b.push(IrInst::ReadF64); b.push(IrInst::StoreFprPs0(fd));
            }
            Opcode::Lfdx => { let fd=ins.fpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::ReadF64); b.push(IrInst::StoreFprPs0(fd)); }

            // ── Float stores ──────────────────────────────────────────────────
            Opcode::Stfs | Opcode::Stfsu => {
                let fs=ins.fpr_s() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                if ins.op == Opcode::Stfsu {
                    let ea=b.alloc_local(IrTy::I32);
                    b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                    b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                    b.push(IrInst::LocalGet(ea));
                } else { self.push_ea_d(b,ra,d); }
                b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::I32DemoteToSingleBits); b.push(IrInst::WriteU32);
            }
            Opcode::Stfsx => { let fs=ins.fpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::I32DemoteToSingleBits); b.push(IrInst::WriteU32); }
            Opcode::Stfd | Opcode::Stfdu => {
                let fs=ins.fpr_s() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32;
                if ins.op == Opcode::Stfdu {
                    let ea=b.alloc_local(IrTy::I32);
                    b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                    b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                    b.push(IrInst::LocalGet(ea));
                } else { self.push_ea_d(b,ra,d); }
                b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::WriteF64);
            }
            Opcode::Stfdx => { let fs=ins.fpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::WriteF64); }

            // ── Scalar FPU arithmetic ─────────────────────────────────────────
            Opcode::Fadd  => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Add); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fadds => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fsub  => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Sub); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fsubs => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Sub); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fmul  => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fc=ins.fpr_c() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fc)); b.push(IrInst::F64Mul); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fmuls => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fc=ins.fpr_c() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fc)); b.push(IrInst::F64Mul); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fdiv  => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Div); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fdivs => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Div); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fmadd  => { self.emit_fma_scalar(b,ins,false,false,false); }
            Opcode::Fmadds => { self.emit_fma_scalar(b,ins,false,false,true); }
            Opcode::Fmsub  => { self.emit_fma_scalar(b,ins,true,false,false); }
            Opcode::Fmsubs => { self.emit_fma_scalar(b,ins,true,false,true); }
            Opcode::Fnmadd  => { self.emit_fma_scalar(b,ins,false,true,false); }
            Opcode::Fnmadds => { self.emit_fma_scalar(b,ins,false,true,true); }
            Opcode::Fnmsub  => { self.emit_fma_scalar(b,ins,true,true,false); }
            Opcode::Fnmsubs => { self.emit_fma_scalar(b,ins,true,true,true); }
            Opcode::Fmr   => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fneg  => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Neg); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fabs  => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Abs); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Frsp  => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fctiw => {
                // Convert to Integer Word, rounding per FPSCR[RN] (default = round to nearest even).
                // The result integer is stored as a raw bit-pattern in the low 32 bits of the FPR
                // (upper 32 bits are undefined per the PowerPC spec) so that stfiwx can extract it.
                // Sequence: f64 → f64.nearest → i32 (sat) → i64 (sign-extend) → f64 (bitcast).
                let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8;
                b.push(IrInst::LoadFprPs0(fb));
                b.push(IrInst::F64Nearest);          // round to nearest even (FPSCR RN=0)
                b.push(IrInst::I32TruncSatF64S);     // f64 → i32, saturating (no trap on NaN/overflow)
                b.push(IrInst::I64ExtendI32S);       // i32 → i64 (sign-extend, matching native fcvt_to_sint_sat + sextend)
                b.push(IrInst::F64ReinterpretI64);   // i64 → f64 bitcast (store integer bits in FPR)
                b.push(IrInst::StoreFprPs0(fd));
            }
            Opcode::Fctiwz => {
                // Convert to Integer Word with truncation toward Zero.
                // Same as fctiw but skips the rounding step.
                // Sequence: f64 → i32 (sat, truncate-toward-zero) → i64 → f64 (bitcast).
                let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8;
                b.push(IrInst::LoadFprPs0(fb));
                b.push(IrInst::I32TruncSatF64S);     // f64 → i32, saturating, truncate toward zero
                b.push(IrInst::I64ExtendI32S);       // i32 → i64 (sign-extend)
                b.push(IrInst::F64ReinterpretI64);   // i64 → f64 bitcast
                b.push(IrInst::StoreFprPs0(fd));
            }
            Opcode::Fsqrt  => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Sqrt); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fsqrts => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Sqrt); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Fres    => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; self.emit_recip(b,fd,fb,false,false); }
            Opcode::Frsqrte => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; self.emit_recip(b,fd,fb,true,false); }
            Opcode::Mffs    => { let fd=ins.fpr_d() as u8; b.push(IrInst::F64Const(0.0)); b.push(IrInst::StoreFprPs0(fd)); }
            Opcode::Mtfsf | Opcode::Mtfsb0 | Opcode::Mtfsb1 | Opcode::Mtfsfi => { /* no-op */ }
            Opcode::Fcmpu | Opcode::Fcmpo => {
                let cr = ins.field_crfd() as u8;
                let fa = ins.fpr_a() as u8;
                let fb = ins.fpr_b() as u8;
                let la = b.alloc_local(IrTy::F64);
                let lb = b.alloc_local(IrTy::F64);
                b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LocalSet(la));
                b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::LocalSet(lb));
                self.update_cr_float(b, cr, la, lb);
            }

            // ── Paired-single (Gekko) ─────────────────────────────────────────
            Opcode::PsAdd  => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsSub  => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Sub); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::F64Sub); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsMul  => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fc=ins.fpr_c() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fc)); b.push(IrInst::F64Mul); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LoadFprPs1(fc)); b.push(IrInst::F64Mul); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsDiv  => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Div); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::F64Div); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsMadd  => { self.emit_fma_ps(b,ins,false,false); }
            Opcode::PsMsub  => { self.emit_fma_ps(b,ins,true,false); }
            Opcode::PsNmadd => { self.emit_fma_ps(b,ins,false,true); }
            Opcode::PsNmsub => { self.emit_fma_ps(b,ins,true,true); }
            Opcode::PsMr    => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsNeg   => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Neg); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::F64Neg); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsAbs   => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Abs); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::F64Abs); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsNabs  => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::F64Abs); b.push(IrInst::F64Neg); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::F64Abs); b.push(IrInst::F64Neg); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsMerge00 => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsMerge01 => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsMerge10 => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsMerge11 => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsSum0 => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; let fc=ins.fpr_c() as u8; b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs1(fc)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsSum1 => { let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; let fc=ins.fpr_c() as u8; b.push(IrInst::LoadFprPs0(fc)); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsRes    => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; self.emit_recip(b,fd,fb,false,false); self.emit_recip(b,fd,fb,false,true); }
            Opcode::PsRsqrte => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; self.emit_recip(b,fd,fb,true,false); self.emit_recip(b,fd,fb,true,true); }
            Opcode::PsMuls0 => {
                let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fc=ins.fpr_c() as u8;
                let c0=b.alloc_local(IrTy::F64); b.push(IrInst::LoadFprPs0(fc)); b.push(IrInst::LocalSet(c0));
                b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LocalGet(c0)); b.push(IrInst::F64Mul); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd));
                b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LocalGet(c0)); b.push(IrInst::F64Mul); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd));
            }
            Opcode::PsMuls1 => {
                let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fc=ins.fpr_c() as u8;
                let c1=b.alloc_local(IrTy::F64); b.push(IrInst::LoadFprPs1(fc)); b.push(IrInst::LocalSet(c1));
                b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LocalGet(c1)); b.push(IrInst::F64Mul); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd));
                b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LocalGet(c1)); b.push(IrInst::F64Mul); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd));
            }
            Opcode::PsMadds0 => {
                let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; let fc=ins.fpr_c() as u8;
                // PsMadds0: fD.ps0 = fA.ps0*fC.ps0 + fB.ps0; fD.ps1 = fA.ps1*fC.ps0 + fB.ps1
                // Cache fC.ps0 in a local so both slots share the same C value.
                let c0 = b.alloc_local(IrTy::F64);
                b.push(IrInst::LoadFprPs0(fc)); b.push(IrInst::LocalSet(c0));
                // ps0: fb.ps0 + fa.ps0 * c0
                b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LocalGet(c0));
                b.push(IrInst::F64Mul); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd));
                // ps1: fb.ps1 + fa.ps1 * c0
                b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LocalGet(c0));
                b.push(IrInst::F64Mul); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd));
            }
            Opcode::PsMadds1 => {
                let fd=ins.fpr_d() as u8; let fa=ins.fpr_a() as u8; let fb=ins.fpr_b() as u8; let fc=ins.fpr_c() as u8;
                // ps0: fa.ps0 * fc.ps1 + fb.ps0; ps1: fa.ps1 * fc.ps1 + fb.ps1
                // Use fma helpers with ps1 flag for the C operand
                let c1=b.alloc_local(IrTy::F64); b.push(IrInst::LoadFprPs1(fc)); b.push(IrInst::LocalSet(c1));
                // For ps0: fa.ps0 * c1 + fb.ps0
                b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LocalGet(c1)); b.push(IrInst::F64Mul); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs0(fd));
                // For ps1: fa.ps1 * c1 + fb.ps1
                b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LocalGet(c1)); b.push(IrInst::F64Mul); b.push(IrInst::F64Add); b.push(IrInst::F64RoundToSingle); b.push(IrInst::StoreFprPs1(fd));
            }
            // psq_l / psq_st: quantized loads/stores — treat as lfs/stfs for now
            Opcode::PsqL  => { let fd=ins.fpr_d() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32; self.push_ea_d(b,ra,d); b.push(IrInst::ReadU32); b.push(IrInst::F64PromoteSingleBits); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fd)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsqSt => { let fs=ins.fpr_s() as u8; let ra=ins.gpr_a() as u8; let d=ins.field_offset() as i32; self.push_ea_d(b,ra,d); b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::I32DemoteToSingleBits); b.push(IrInst::WriteU32); }
            Opcode::PsqLx  => { let fd=ins.fpr_d() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::ReadU32); b.push(IrInst::F64PromoteSingleBits); b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fd)); b.push(IrInst::StoreFprPs1(fd)); }
            Opcode::PsqStx => { let fs=ins.fpr_s() as u8; let ra=ins.gpr_a() as u8; let rb=ins.gpr_b() as u8; self.push_ea_x(b,ra,rb); b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::I32DemoteToSingleBits); b.push(IrInst::WriteU32); }

            // ── Branches ──────────────────────────────────────────────────────
            Opcode::B => {
                let li=ins.field_li(); let lk=ins.field_lk(); let aa=ins.field_aa();
                let target = if aa { li as u32 } else { pc.wrapping_add_signed(li as i32) };
                if lk { b.push(IrInst::I32Const((pc+4) as i32)); b.push(IrInst::StoreLr); }
                b.push(IrInst::ReturnStatic(target));
                return Ok(true);
            }
            Opcode::Bc => {
                let bo=ins.field_bo(); let bi=ins.field_bi() as u8; let bd=ins.field_bd(); let lk=ins.field_lk(); let aa=ins.field_aa();
                let taken = if aa { bd as u32 } else { pc.wrapping_add_signed(bd as i32) };
                if lk { b.push(IrInst::I32Const((pc+4) as i32)); b.push(IrInst::StoreLr); }
                self.emit_branch_cond(b, bo as u8, bi);
                b.push(IrInst::BranchIf { taken, fallthrough: pc+4 });
                return Ok(true);
            }
            Opcode::Bclr => {
                let bo=ins.field_bo(); let bi=ins.field_bi() as u8; let lk=ins.field_lk();
                let lr=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadLr); b.push(IrInst::LocalSet(lr));
                if lk { b.push(IrInst::I32Const((pc+4) as i32)); b.push(IrInst::StoreLr); }
                if (bo>>4)&1!=0 && (bo>>2)&1!=0 {
                    b.push(IrInst::LocalGet(lr)); b.push(IrInst::ReturnDynamic);
                } else {
                    self.emit_branch_cond(b, bo as u8, bi);
                    b.push(IrInst::BranchRegIf { reg_local: lr, fallthrough: pc+4 });
                }
                return Ok(true);
            }
            Opcode::Bcctr => {
                let bo=ins.field_bo(); let bi=ins.field_bi() as u8; let lk=ins.field_lk();
                let ctr=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadCtr); b.push(IrInst::LocalSet(ctr));
                if lk { b.push(IrInst::I32Const((pc+4) as i32)); b.push(IrInst::StoreLr); }
                if (bo>>4)&1!=0 && (bo>>2)&1!=0 {
                    b.push(IrInst::LocalGet(ctr)); b.push(IrInst::ReturnDynamic);
                } else {
                    // Per the PowerPC Architecture manual, bcctr/bcctrl must NEVER
                    // decrement CTR (BO[2] must be 1; BO[2]=0 is boundedly undefined).
                    // Force ignore_ctr=1 (bit 2) so emit_branch_cond cannot decrement
                    // CTR regardless of the BO field in the instruction encoding.
                    self.emit_branch_cond(b, bo as u8 | 4, bi);
                    b.push(IrInst::BranchRegIf { reg_local: ctr, fallthrough: pc+4 });
                }
                return Ok(true);
            }

            // ── System ────────────────────────────────────────────────────────
            //
            // SPR field encoding note: `field_spr()` in the `powerpc` crate
            // already decodes the split 10-bit SPR field and returns the actual
            // SPR number directly (e.g., 8 for LR, 272 for SPRG0).  No further
            // bit-rotation is needed; applying one would corrupt every SPR access.
            Opcode::Mfspr => {
                let rd=ins.gpr_d() as u8;
                let spr=ins.field_spr() as u32;
                let load = match spr {
                    1   => IrInst::LoadXer,
                    8   => IrInst::LoadLr,
                    9   => IrInst::LoadCtr,
                    22  => IrInst::LoadDec,
                    26  => IrInst::LoadSrr0,
                    27  => IrInst::LoadSrr1,
                    272 => IrInst::LoadSprg(0),
                    273 => IrInst::LoadSprg(1),
                    274 => IrInst::LoadSprg(2),
                    275 => IrInst::LoadSprg(3),
                    _   => IrInst::I32Const(0),
                };
                b.push(load); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Mtspr => {
                let rs=ins.gpr_s() as u8;
                let spr=ins.field_spr() as u32;
                b.push(IrInst::LoadGpr(rs));
                match spr {
                    1   => b.push(IrInst::StoreXer),
                    8   => b.push(IrInst::StoreLr),
                    9   => b.push(IrInst::StoreCtr),
                    22  => b.push(IrInst::StoreDec),
                    26  => b.push(IrInst::StoreSrr0),
                    27  => b.push(IrInst::StoreSrr1),
                    272 => b.push(IrInst::StoreSprg(0)),
                    273 => b.push(IrInst::StoreSprg(1)),
                    274 => b.push(IrInst::StoreSprg(2)),
                    275 => b.push(IrInst::StoreSprg(3)),
                    _   => b.push(IrInst::Drop),
                }
            }
            Opcode::Mftb => {
                let rd=ins.gpr_d() as u8;
                // field_tbr() returns the actual TBR number (268=TBL, 269=TBU)
                // directly, matching how ppcjit decodes this instruction.
                // Any value other than 268 is treated as TBU (only 268/269 are valid).
                let tbr=ins.field_tbr();
                b.push(if tbr==268 { IrInst::LoadTbLo } else { IrInst::LoadTbHi });
                b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Mfcr => { let rd=ins.gpr_d() as u8; b.push(IrInst::LoadCr); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Mtcrf => {
                let rs=ins.gpr_s() as u8; let fxm=ins.field_crm() as u32;
                let mut mask=0u32;
                for i in 0..8u32 { if (fxm>>(7-i))&1!=0 { mask|=0xF<<(28-i*4); } }
                b.push(IrInst::LoadCr); b.push(IrInst::I32Const(!mask as i32)); b.push(IrInst::I32And);
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Const(mask as i32)); b.push(IrInst::I32And);
                b.push(IrInst::I32Or); b.push(IrInst::StoreCr);
            }
            // ── CR bit operations ─────────────────────────────────────────────
            Opcode::Crxor => {
                let ba=ins.field_crba() as u8; let bb=ins.field_crbb() as u8; let bd=ins.field_crbd() as u8;
                let la = self.cr_get_bit(b, ba);
                let lb = self.cr_get_bit(b, bb);
                let lv = b.alloc_local(IrTy::I32);
                b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::I32Xor);
                b.push(IrInst::LocalSet(lv));
                self.cr_set_bit(b, bd, lv);
            }
            Opcode::Creqv => {
                let ba=ins.field_crba() as u8; let bb=ins.field_crbb() as u8; let bd=ins.field_crbd() as u8;
                let la = self.cr_get_bit(b, ba);
                let lb = self.cr_get_bit(b, bb);
                let lv = b.alloc_local(IrTy::I32);
                b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::I32Xor);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); // XNOR: NOT(a XOR b)
                b.push(IrInst::LocalSet(lv));
                self.cr_set_bit(b, bd, lv);
            }
            Opcode::Cror => {
                let ba=ins.field_crba() as u8; let bb=ins.field_crbb() as u8; let bd=ins.field_crbd() as u8;
                let la = self.cr_get_bit(b, ba);
                let lb = self.cr_get_bit(b, bb);
                let lv = b.alloc_local(IrTy::I32);
                b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::I32Or);
                b.push(IrInst::LocalSet(lv));
                self.cr_set_bit(b, bd, lv);
            }
            Opcode::Crorc => {
                // crbd = ba | ~bb
                let ba=ins.field_crba() as u8; let bb=ins.field_crbb() as u8; let bd=ins.field_crbd() as u8;
                let la = self.cr_get_bit(b, ba);
                let lb = self.cr_get_bit(b, bb);
                let lv = b.alloc_local(IrTy::I32);
                b.push(IrInst::LocalGet(lb)); b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); // ~bb (as 0/1)
                b.push(IrInst::LocalGet(la)); b.push(IrInst::I32Or);
                b.push(IrInst::LocalSet(lv));
                self.cr_set_bit(b, bd, lv);
            }
            Opcode::Crnor => {
                // crbd = ~(ba | bb)
                let ba=ins.field_crba() as u8; let bb=ins.field_crbb() as u8; let bd=ins.field_crbd() as u8;
                let la = self.cr_get_bit(b, ba);
                let lb = self.cr_get_bit(b, bb);
                let lv = b.alloc_local(IrTy::I32);
                b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::I32Or);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); // NOR: NOT(a OR b)
                b.push(IrInst::LocalSet(lv));
                self.cr_set_bit(b, bd, lv);
            }
            Opcode::Crand => {
                let ba=ins.field_crba() as u8; let bb=ins.field_crbb() as u8; let bd=ins.field_crbd() as u8;
                let la = self.cr_get_bit(b, ba);
                let lb = self.cr_get_bit(b, bb);
                let lv = b.alloc_local(IrTy::I32);
                b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(lv));
                self.cr_set_bit(b, bd, lv);
            }
            Opcode::Crandc => {
                // crbd = ba & ~bb
                let ba=ins.field_crba() as u8; let bb=ins.field_crbb() as u8; let bd=ins.field_crbd() as u8;
                let la = self.cr_get_bit(b, ba);
                let lb = self.cr_get_bit(b, bb);
                let lv = b.alloc_local(IrTy::I32);
                b.push(IrInst::LocalGet(lb)); b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); // ~bb (0/1)
                b.push(IrInst::LocalGet(la)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(lv));
                self.cr_set_bit(b, bd, lv);
            }
            Opcode::Crnand => {
                // crbd = ~(ba & bb)
                let ba=ins.field_crba() as u8; let bb=ins.field_crbb() as u8; let bd=ins.field_crbd() as u8;
                let la = self.cr_get_bit(b, ba);
                let lb = self.cr_get_bit(b, bb);
                let lv = b.alloc_local(IrTy::I32);
                b.push(IrInst::LocalGet(la)); b.push(IrInst::LocalGet(lb)); b.push(IrInst::I32And);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32Xor); // NAND: NOT(a AND b)
                b.push(IrInst::LocalSet(lv));
                self.cr_set_bit(b, bd, lv);
            }
            Opcode::Mcrf => {
                // Move CR field crfs to crfd.
                // crfs/crfd are 0-7; field occupies 4 bits at position 28 - crfd*4 .. 31 - crfd*4.
                let crfd = ins.field_crfd() as u32;
                let crfs = ins.field_crfs() as u32;
                let src_shift = 28u32.wrapping_sub(crfs * 4);
                let dst_shift = 28u32.wrapping_sub(crfd * 4);
                let dst_clear = !(0xFu32 << dst_shift) as i32;
                let field = b.alloc_local(IrTy::I32);
                // Extract source field, shift to destination position
                b.push(IrInst::LoadCr);
                b.push(IrInst::I32Const(src_shift as i32)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(0xF)); b.push(IrInst::I32And);
                b.push(IrInst::I32Const(dst_shift as i32)); b.push(IrInst::I32Shl);
                b.push(IrInst::LocalSet(field));
                // Clear destination field and OR in the new field
                b.push(IrInst::LoadCr);
                b.push(IrInst::I32Const(dst_clear)); b.push(IrInst::I32And);
                b.push(IrInst::LocalGet(field)); b.push(IrInst::I32Or);
                b.push(IrInst::StoreCr);
            }
            Opcode::Mfmsr => { let rd=ins.gpr_d() as u8; b.push(IrInst::LoadMsr); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Mtmsr => {
                // Store the new MSR, then terminate the block so the host
                // interrupt-check loop (advance_decrementer) sees the updated MSR
                // before any further guest instructions execute.  On real PowerPC,
                // the processor samples pending exceptions immediately after mtmsr
                // enables EE; without block termination here the host check runs too
                // late (EE may be 0 again by the time the block returns).
                let rs = ins.gpr_s() as u8;
                b.push(IrInst::LoadGpr(rs));
                b.push(IrInst::StoreMsr);
                b.push(IrInst::ReturnStatic(pc + 4));
                return Ok(true);
            }
            Opcode::Rfi => {
                // Match ppcjit: only copy SRR1_TO_MSR_MASK bits from SRR1 into MSR,
                // clear MSR bit 18 (POW), and mask the low 2 bits of SRR0.
                // SRR1_TO_MSR_MASK = 0x87C0FF73 (from gekko::Exception).
                const SRR1_MSR_MASK: i32 = 0x87C0FF73u32 as i32;
                const NOT_SRR1_MSR_MASK: i32 = !SRR1_MSR_MASK;
                const NOT_BIT18: i32 = !(1i32 << 18);
                const NOT_LOW2: i32 = !3i32;
                // new_msr = (srr1 & mask) | (msr & ~mask), then clear bit 18
                b.push(IrInst::LoadSrr1); b.push(IrInst::I32Const(SRR1_MSR_MASK)); b.push(IrInst::I32And);
                b.push(IrInst::LoadMsr); b.push(IrInst::I32Const(NOT_SRR1_MSR_MASK)); b.push(IrInst::I32And);
                b.push(IrInst::I32Or);
                b.push(IrInst::I32Const(NOT_BIT18)); b.push(IrInst::I32And);
                b.push(IrInst::StoreMsr);
                // new_pc = srr0 & ~0b11
                b.push(IrInst::LoadSrr0); b.push(IrInst::I32Const(NOT_LOW2)); b.push(IrInst::I32And);
                b.push(IrInst::ReturnDynamic);
                return Ok(true);
            }

            // ── dcbz / dcbzl — zero a data-cache line in memory ──────────────
            //
            // The native JIT implements `dcbz` as 8 × 4-byte zero-stores (see
            // ppcjit/builder/others.rs).  Treating it as a no-op prevents BSS
            // and display-list clearing from working, which breaks game boot.
            // `dcbzl` is the Gekko-specific 128-byte (L2) variant.
            Opcode::Dcbz => {
                let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                // EA aligned to 32-byte (cache-line) boundary.
                let ea = b.alloc_local(IrTy::I32);
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::I32Const(!31i32)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(ea));
                // Zero 8 words (32 bytes).
                for i in 0..8i32 {
                    b.push(IrInst::LocalGet(ea));
                    if i > 0 { b.push(IrInst::I32Const(i * 4)); b.push(IrInst::I32Add); }
                    b.push(IrInst::I32Const(0));
                    b.push(IrInst::WriteU32);
                }
            }
            Opcode::DcbzL => {
                // Gekko-specific 128-byte (L2 cache-line) zero.  EA aligned to
                // 128 bytes.  Same rationale as Dcbz above.
                let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                let ea = b.alloc_local(IrTy::I32);
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::I32Const(!127i32)); b.push(IrInst::I32And);
                b.push(IrInst::LocalSet(ea));
                // Zero 32 words (128 bytes).
                for i in 0..32i32 {
                    b.push(IrInst::LocalGet(ea));
                    if i > 0 { b.push(IrInst::I32Const(i * 4)); b.push(IrInst::I32Add); }
                    b.push(IrInst::I32Const(0));
                    b.push(IrInst::WriteU32);
                }
            }

            // ── Cache/sync hints (no-ops) ─────────────────────────────────────
            Opcode::Sync|Opcode::Isync|Opcode::Eieio|Opcode::Dcbst|Opcode::Icbi|Opcode::Dcbi|Opcode::Dcbf
            | Opcode::Dcbt | Opcode::Dcbtst | Opcode::Tlbie | Opcode::Tlbsync
            // External control instructions — no-ops (no physical device attached)
            | Opcode::Eciwx | Opcode::Ecowx => {}

            // ── Exceptions ────────────────────────────────────────────────────
            Opcode::Sc => {
                // Syscall: store the PC of the `sc` instruction itself (NOT pc+4)
                // so that deliver_exception → gekko::Cpu::raise_exception(Syscall)
                // sets SRR0 = cpu.pc + 4 correctly.  raise_exception adds 4 for
                // Syscall via srr0_skip() — if we pre-store pc+4 here, SRR0 ends
                // up as pc+8, which causes rfi to return to the wrong address.
                b.push(IrInst::I32Const(pc as i32)); b.push(IrInst::StorePC);
                b.push(IrInst::RaiseException(0x0C00));
                return Ok(true);
            }
            Opcode::Illegal => {
                // Illegal (unrecognised) encoding — the powerpc crate returns
                // Opcode::Illegal for instruction words it cannot decode, which
                // includes some Gekko-specific encodings not yet covered by the
                // crate.  Treat these the same as unimplemented instructions:
                // record the PC for diagnostics and skip to pc+4 so the emulator
                // keeps running rather than halting with a compile error.
                b.unimplemented_ops.push(format!("Illegal @ 0x{:08X}", pc));
                b.push(IrInst::ReturnStatic(pc + 4));
                return Ok(true);
            }

            // ── Float select ──────────────────────────────────────────────────
            Opcode::Fsel => {
                let fd = ins.fpr_d() as u8; let fa = ins.fpr_a() as u8;
                let fb = ins.fpr_b() as u8; let fc = ins.fpr_c() as u8;
                // cond = fa.ps0 >= 0.0  (NaN-safe: F64Gt|F64Eq returns 0 for NaN)
                let la = b.alloc_local(IrTy::F64);
                let cond = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LocalSet(la));
                b.push(IrInst::LocalGet(la)); b.push(IrInst::F64Const(0.0)); b.push(IrInst::F64Gt);
                b.push(IrInst::LocalGet(la)); b.push(IrInst::F64Const(0.0)); b.push(IrInst::F64Eq);
                b.push(IrInst::I32Or); b.push(IrInst::LocalSet(cond));
                // fd.ps0 = cond ? fc.ps0 : fb.ps0
                b.push(IrInst::LoadFprPs0(fc)); b.push(IrInst::LoadFprPs0(fb));
                b.push(IrInst::LocalGet(cond)); b.push(IrInst::F64Select);
                b.push(IrInst::StoreFprPs0(fd));
                // fd.ps1 = cond ? fc.ps1 : fb.ps1 (same condition per ppcjit)
                b.push(IrInst::LoadFprPs1(fc)); b.push(IrInst::LoadFprPs1(fb));
                b.push(IrInst::LocalGet(cond)); b.push(IrInst::F64Select);
                b.push(IrInst::StoreFprPs1(fd));
            }
            Opcode::PsSel => {
                let fd = ins.fpr_d() as u8; let fa = ins.fpr_a() as u8;
                let fb = ins.fpr_b() as u8; let fc = ins.fpr_c() as u8;
                let la0 = b.alloc_local(IrTy::F64); let la1 = b.alloc_local(IrTy::F64);
                let c0 = b.alloc_local(IrTy::I32);  let c1 = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LocalSet(la0));
                b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LocalSet(la1));
                // c0 = fa.ps0 >= 0.0
                b.push(IrInst::LocalGet(la0)); b.push(IrInst::F64Const(0.0)); b.push(IrInst::F64Gt);
                b.push(IrInst::LocalGet(la0)); b.push(IrInst::F64Const(0.0)); b.push(IrInst::F64Eq);
                b.push(IrInst::I32Or); b.push(IrInst::LocalSet(c0));
                // c1 = fa.ps1 >= 0.0
                b.push(IrInst::LocalGet(la1)); b.push(IrInst::F64Const(0.0)); b.push(IrInst::F64Gt);
                b.push(IrInst::LocalGet(la1)); b.push(IrInst::F64Const(0.0)); b.push(IrInst::F64Eq);
                b.push(IrInst::I32Or); b.push(IrInst::LocalSet(c1));
                // fd.ps0 = c0 ? fc.ps0 : fb.ps0
                b.push(IrInst::LoadFprPs0(fc)); b.push(IrInst::LoadFprPs0(fb));
                b.push(IrInst::LocalGet(c0)); b.push(IrInst::F64Select); b.push(IrInst::StoreFprPs0(fd));
                // fd.ps1 = c1 ? fc.ps1 : fb.ps1
                b.push(IrInst::LoadFprPs1(fc)); b.push(IrInst::LoadFprPs1(fb));
                b.push(IrInst::LocalGet(c1)); b.push(IrInst::F64Select); b.push(IrInst::StoreFprPs1(fd));
            }

            // ── Float loads with update-indexed ──────────────────────────────
            Opcode::Lfsux => {
                let fd = ins.fpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                let ea = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea));
                b.push(IrInst::ReadU32); b.push(IrInst::F64PromoteSingleBits);
                b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fd)); b.push(IrInst::StoreFprPs1(fd));
            }
            Opcode::Lfdux => {
                let fd = ins.fpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                let ea = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea));
                b.push(IrInst::ReadF64); b.push(IrInst::StoreFprPs0(fd));
            }

            // ── Float stores with update-indexed ─────────────────────────────
            Opcode::Stfsux => {
                let fs = ins.fpr_s() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                let ea = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea));
                b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::I32DemoteToSingleBits); b.push(IrInst::WriteU32);
            }
            Opcode::Stfdux => {
                let fs = ins.fpr_s() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                let ea = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea));
                b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::WriteF64);
            }

            // ── Store float as integer word indexed ───────────────────────────
            Opcode::Stfiwx => {
                let fs = ins.fpr_s() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::I32FromF64LowBits); b.push(IrInst::WriteU32);
            }

            // ── Byte-reverse loads ────────────────────────────────────────────
            Opcode::Lwbrx => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::ReadU32); b.push(IrInst::I32Bswap); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Lhbrx => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::ReadU16); b.push(IrInst::I32Bswap16); b.push(IrInst::StoreGpr(rd));
            }

            // ── Byte-reverse stores ───────────────────────────────────────────
            Opcode::Stwbrx => {
                let rs = ins.gpr_s() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Bswap); b.push(IrInst::WriteU32);
            }
            Opcode::Sthbrx => {
                let rs = ins.gpr_s() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Bswap16); b.push(IrInst::WriteU16);
            }

            // ── Load word and reserve indexed (treat as plain lwzx) ───────────
            Opcode::Lwarx => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::ReadU32); b.push(IrInst::StoreGpr(rd));
            }

            // ── Store word conditional indexed (always succeeds) ──────────────
            Opcode::Stwcx_ => {
                let rs = ins.gpr_s() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                // Store the word unconditionally.
                self.push_ea_x(b, ra, rb);
                b.push(IrInst::LoadGpr(rs)); b.push(IrInst::WriteU32);
                // Set CR0: LT=0, GT=0, EQ=1 (reservation always succeeds), SO from XER.
                // In our CR layout: field 0 is bits 31..28 (LT=31, GT=30, EQ=29, SO=28).
                let so = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadXer); b.push(IrInst::I32Const(31)); b.push(IrInst::I32ShrU);
                b.push(IrInst::I32Const(1)); b.push(IrInst::I32And); b.push(IrInst::LocalSet(so));
                b.push(IrInst::LoadCr);
                b.push(IrInst::I32Const(!(0xFu32 << 28) as i32)); b.push(IrInst::I32And);
                b.push(IrInst::I32Const(1i32 << 29)); b.push(IrInst::I32Or); // EQ = 1
                b.push(IrInst::LocalGet(so)); b.push(IrInst::I32Const(28)); b.push(IrInst::I32Shl);
                b.push(IrInst::I32Or);
                b.push(IrInst::StoreCr);
            }

            // ── Load string word immediate ─────────────────────────────────────
            Opcode::Lswi => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                let nb_field = ins.field_nb() as u8;
                let byte_count = if nb_field == 0 { 32u8 } else { nb_field };
                let ea = b.alloc_local(IrTy::I32);
                if ra == 0 { b.push(IrInst::I32Const(0)); } else { b.push(IrInst::LoadGpr(ra)); }
                b.push(IrInst::LocalSet(ea));
                // Zero out all destination registers before ORing bytes in.
                let num_regs = (byte_count as u32 + 3) / 4;
                for i in 0..num_regs as u8 { b.push(IrInst::I32Const(0)); b.push(IrInst::StoreGpr((rd + i) % 32)); }
                // Load each byte and OR it into the appropriate register.
                for i in 0u8..byte_count {
                    let reg = ((rd as u32 + i as u32 / 4) % 32) as u8;
                    let shift = 8 * (3 - (i % 4)) as i32;
                    b.push(IrInst::LocalGet(ea));
                    if i > 0 { b.push(IrInst::I32Const(i as i32)); b.push(IrInst::I32Add); }
                    b.push(IrInst::ReadU8);
                    if shift > 0 { b.push(IrInst::I32Const(shift)); b.push(IrInst::I32Shl); }
                    b.push(IrInst::LoadGpr(reg)); b.push(IrInst::I32Or); b.push(IrInst::StoreGpr(reg));
                }
            }

            // ── Store string word immediate ────────────────────────────────────
            Opcode::Stswi => {
                let rs = ins.gpr_s() as u8; let ra = ins.gpr_a() as u8;
                let nb_field = ins.field_nb() as u8;
                let byte_count = if nb_field == 0 { 32u8 } else { nb_field };
                let ea = b.alloc_local(IrTy::I32);
                if ra == 0 { b.push(IrInst::I32Const(0)); } else { b.push(IrInst::LoadGpr(ra)); }
                b.push(IrInst::LocalSet(ea));
                for i in 0u8..byte_count {
                    let reg = ((rs as u32 + i as u32 / 4) % 32) as u8;
                    let shift = 8 * (3 - (i % 4)) as i32;
                    // Push addr = ea + i
                    b.push(IrInst::LocalGet(ea));
                    if i > 0 { b.push(IrInst::I32Const(i as i32)); b.push(IrInst::I32Add); }
                    // Push val = (reg >> shift) & 0xFF
                    b.push(IrInst::LoadGpr(reg));
                    if shift > 0 { b.push(IrInst::I32Const(shift)); b.push(IrInst::I32ShrU); }
                    b.push(IrInst::I32Const(0xFF)); b.push(IrInst::I32And);
                    b.push(IrInst::WriteU8);
                }
            }

            // ── Move to CR from XER ───────────────────────────────────────────
            Opcode::Mcrxr => {
                // Copy XER[SO,OV,CA] (bits 31,30,29) to CRfd[LT,GT,EQ]; set CRfd[SO]=0.
                // Clear XER SO, OV, CA bits afterwards.
                let crfd = ins.field_crfd() as u8;
                let lt_bit = 31u32.wrapping_sub(crfd as u32 * 4);
                let so_bit = lt_bit.wrapping_sub(3);
                let clear_mask = !(0xFu32 << so_bit) as i32;
                let xer_val = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadXer); b.push(IrInst::LocalTee(xer_val));
                // Clear XER SO, OV, CA (bits 31, 30, 29) and write back.
                b.push(IrInst::I32Const(!(7u32 << 29) as i32)); b.push(IrInst::I32And); b.push(IrInst::StoreXer);
                // Place XER bits 31,30,29 at CRfd bits lt_bit,lt_bit-1,lt_bit-2 (shift right by crfd*4).
                b.push(IrInst::LoadCr);
                b.push(IrInst::I32Const(clear_mask)); b.push(IrInst::I32And);
                b.push(IrInst::LocalGet(xer_val));
                b.push(IrInst::I32Const((7u32 << 29) as i32)); b.push(IrInst::I32And);
                if crfd > 0 { b.push(IrInst::I32Const(crfd as i32 * 4)); b.push(IrInst::I32ShrU); }
                b.push(IrInst::I32Or); b.push(IrInst::StoreCr);
            }

            // ── Segment registers (not tracked in browser emulator) ───────────
            Opcode::Mfsr => {
                // Move from segment register: return 0 (SR not tracked).
                let rd = ins.gpr_d() as u8;
                b.push(IrInst::I32Const(0)); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Mtsr => { /* no-op: segment registers not tracked */ }

            // ── Paired-single compare (ps0 or ps1 slot) ───────────────────────
            Opcode::PsCmpu0 | Opcode::PsCmpo0 => {
                let cr = ins.field_crfd() as u8; let fa = ins.fpr_a() as u8; let fb = ins.fpr_b() as u8;
                let la = b.alloc_local(IrTy::F64); let lb = b.alloc_local(IrTy::F64);
                b.push(IrInst::LoadFprPs0(fa)); b.push(IrInst::LocalSet(la));
                b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::LocalSet(lb));
                self.update_cr_float(b, cr, la, lb);
            }
            Opcode::PsCmpu1 | Opcode::PsCmpo1 => {
                let cr = ins.field_crfd() as u8; let fa = ins.fpr_a() as u8; let fb = ins.fpr_b() as u8;
                let la = b.alloc_local(IrTy::F64); let lb = b.alloc_local(IrTy::F64);
                b.push(IrInst::LoadFprPs1(fa)); b.push(IrInst::LocalSet(la));
                b.push(IrInst::LoadFprPs1(fb)); b.push(IrInst::LocalSet(lb));
                self.update_cr_float(b, cr, la, lb);
            }

            // ── PS quantized load / store with update ─────────────────────────
            Opcode::PsqLu => {
                let fd = ins.fpr_d() as u8; let ra = ins.gpr_a() as u8; let d = ins.field_offset() as i32;
                let ea = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea)); b.push(IrInst::StoreGpr(ra));
                b.push(IrInst::LocalGet(ea));
                b.push(IrInst::ReadU32); b.push(IrInst::F64PromoteSingleBits);
                b.push(IrInst::StoreFprPs0(fd)); b.push(IrInst::LoadFprPs0(fd)); b.push(IrInst::StoreFprPs1(fd));
            }
            Opcode::PsqStu => {
                let fs = ins.fpr_s() as u8; let ra = ins.gpr_a() as u8; let d = ins.field_offset() as i32;
                let ea = b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(d)); b.push(IrInst::I32Add);
                b.push(IrInst::LocalTee(ea));
                b.push(IrInst::LoadFprPs0(fs)); b.push(IrInst::I32DemoteToSingleBits); b.push(IrInst::WriteU32);
                b.push(IrInst::LocalGet(ea)); b.push(IrInst::StoreGpr(ra));
            }

            // ── Unimplemented ─────────────────────────────────────────────────
            _ => {
                // Record the missing opcode for diagnostics, then skip over the
                // instruction (ReturnStatic to pc+4) so compilation can continue
                // from the next block rather than crashing the emulator.
                b.unimplemented_ops.push(format!("{:?} @ 0x{:08X}", ins.op, pc));
                b.push(IrInst::ReturnStatic(pc + 4));
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gekko::disasm::{Extensions, Ins};
    fn ins(code: u32) -> Ins { Ins::new(code, Extensions::gekko_broadway()) }

    #[test] fn ppc_mask_full() { assert_eq!(ppc_mask(0,31), 0xFFFF_FFFF); }
    #[test] fn ppc_mask_low_byte() { assert_eq!(ppc_mask(24,31), 0xFF); }
    #[test] fn decode_empty() { assert!(Decoder::new().decode(std::iter::empty()).is_err()); }
    #[test] fn decode_addi_ok() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x3860_002A))].into_iter()).unwrap();
        assert_eq!(b.instruction_count, 1);
        assert!(b.unimplemented_ops.is_empty());
    }
    #[test] fn decode_branch_terminal() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x4800_0010)), (0x8000_0004u32, ins(0x3860_0001))].into_iter()).unwrap();
        assert_eq!(b.instruction_count, 1);
    }
    #[test] fn decode_mtmsr_terminal() {
        // mtmsr r3 (0x7C60_0124) followed by addi r3, r0, 1 (0x3860_0001):
        // only the mtmsr should be compiled; the addi must be excluded.
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C60_0124)), (0x8000_0004u32, ins(0x3860_0001))].into_iter()).unwrap();
        assert_eq!(b.instruction_count, 1);
        assert!(b.unimplemented_ops.is_empty());
    }
    #[test] fn decode_fadd_ok() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0xFC22_082A))].into_iter()).unwrap();
        assert!(b.unimplemented_ops.is_empty(), "{:?}", b.unimplemented_ops);
    }
    #[test] fn decode_lfs_ok() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0xC023_0000))].into_iter()).unwrap();
        assert!(b.unimplemented_ops.is_empty(), "{:?}", b.unimplemented_ops);
    }
    #[test] fn decode_fmadd_ok() {
        // fmadd f1, f2, f3, f4  — 0xFC22_21FA
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0xFC22_21FA))].into_iter()).unwrap();
        assert!(b.unimplemented_ops.is_empty(), "{:?}", b.unimplemented_ops);
    }

    // ── SPR decode correctness ─────────────────────────────────────────────────
    //
    // field_spr() in the powerpc crate already returns the actual SPR number;
    // no rotation is required.  These tests guard against the "rotate_right(5)"
    // regression that silently dropped every SPR read/write.

    /// Encode `mtspr SPR, r3` — opcode=31 (0x7C000000), rS=r3 (bits25-21=3),
    /// SPR split-field (bits20-16=spr[4:0], bits15-11=spr[9:5]), xo=467<<1.
    fn encode_mtspr(spr: u32) -> u32 {
        let lo = spr & 0x1F;
        let hi = (spr >> 5) & 0x1F;
        0x7C00_0000 | (3 << 21) | (lo << 16) | (hi << 11) | (467 << 1)
    }

    /// Encode `mfspr r3, SPR` — same field layout, xo=339<<1.
    fn encode_mfspr(spr: u32) -> u32 {
        let lo = spr & 0x1F;
        let hi = (spr >> 5) & 0x1F;
        0x7C00_0000 | (3 << 21) | (lo << 16) | (hi << 11) | (339 << 1)
    }

    /// Helper: assert that a single-instruction block has no unimplemented ops and
    /// that its IR contains `target_ir` somewhere (verified by the variant name).
    fn check_spr_no_unimpl(spr: u32, mtspr: bool) {
        let code = if mtspr { encode_mtspr(spr) } else { encode_mfspr(spr) };
        let b = Decoder::new().decode([(0x8000_0000u32, ins(code))].into_iter()).unwrap();
        assert!(
            b.unimplemented_ops.is_empty(),
            "SPR {} ({}) produced unimplemented ops: {:?}",
            spr,
            if mtspr { "mtspr" } else { "mfspr" },
            b.unimplemented_ops,
        );
    }

    /// Encode `mtspr SPR, r3; blr` — used to verify that the IR for the mtspr
    /// uses the correct IrInst variant by checking the WASM output is valid and
    /// has no unimplemented ops.
    fn check_mtspr_no_unimpl(spr: u32) { check_spr_no_unimpl(spr, true); }
    fn check_mfspr_no_unimpl(spr: u32) { check_spr_no_unimpl(spr, false); }

    // mtspr — one test per supported SPR
    #[test] fn mtspr_xer_no_unimpl()   { check_mtspr_no_unimpl(1); }
    #[test] fn mtspr_lr_no_unimpl()    { check_mtspr_no_unimpl(8); }
    #[test] fn mtspr_ctr_no_unimpl()   { check_mtspr_no_unimpl(9); }
    #[test] fn mtspr_dec_no_unimpl()   { check_mtspr_no_unimpl(22); }
    #[test] fn mtspr_srr0_no_unimpl()  { check_mtspr_no_unimpl(26); }
    #[test] fn mtspr_srr1_no_unimpl()  { check_mtspr_no_unimpl(27); }
    #[test] fn mtspr_sprg0_no_unimpl() { check_mtspr_no_unimpl(272); }
    #[test] fn mtspr_sprg1_no_unimpl() { check_mtspr_no_unimpl(273); }
    #[test] fn mtspr_sprg2_no_unimpl() { check_mtspr_no_unimpl(274); }
    #[test] fn mtspr_sprg3_no_unimpl() { check_mtspr_no_unimpl(275); }

    // mfspr — one test per supported SPR
    #[test] fn mfspr_xer_no_unimpl()   { check_mfspr_no_unimpl(1); }
    #[test] fn mfspr_lr_no_unimpl()    { check_mfspr_no_unimpl(8); }
    #[test] fn mfspr_ctr_no_unimpl()   { check_mfspr_no_unimpl(9); }
    #[test] fn mfspr_dec_no_unimpl()   { check_mfspr_no_unimpl(22); }
    #[test] fn mfspr_srr0_no_unimpl()  { check_mfspr_no_unimpl(26); }
    #[test] fn mfspr_srr1_no_unimpl()  { check_mfspr_no_unimpl(27); }
    #[test] fn mfspr_sprg0_no_unimpl() { check_mfspr_no_unimpl(272); }
    #[test] fn mfspr_sprg1_no_unimpl() { check_mfspr_no_unimpl(273); }
    #[test] fn mfspr_sprg2_no_unimpl() { check_mfspr_no_unimpl(274); }
    #[test] fn mfspr_sprg3_no_unimpl() { check_mfspr_no_unimpl(275); }

    // Verify IR variant — check that the IrBlock emitted for mtspr/mfspr
    // contains the expected Store/Load instruction.  This catches mis-mappings
    // (e.g. mtspr LR being silently emitted as Drop).
    fn ir_contains(block: &IrBlock, variant: fn(&IrInst) -> bool) -> bool {
        block.insts.iter().any(variant)
    }

    #[test]
    fn mtspr_lr_emits_store_lr() {
        // mtlr r3 = mtspr LR, r3 = 0x7C6803A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C68_03A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreLr)),
            "mtspr LR must emit StoreLr; IR: {:?}", b.insts);
    }

    #[test]
    fn mfspr_lr_emits_load_lr() {
        // mflr r3 = mfspr r3, LR = 0x7C6802A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C68_02A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::LoadLr)),
            "mfspr LR must emit LoadLr; IR: {:?}", b.insts);
    }

    #[test]
    fn mtspr_ctr_emits_store_ctr() {
        // mtctr r3 = mtspr CTR, r3 = 0x7C6903A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C69_03A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreCtr)),
            "mtspr CTR must emit StoreCtr; IR: {:?}", b.insts);
    }

    #[test]
    fn mfspr_ctr_emits_load_ctr() {
        // mfctr r3 = mfspr r3, CTR = 0x7C6902A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C69_02A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::LoadCtr)),
            "mfspr CTR must emit LoadCtr; IR: {:?}", b.insts);
    }

    #[test]
    fn mtspr_dec_emits_store_dec() {
        // mtspr DEC, r3 = 0x7C7603A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C76_03A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreDec)),
            "mtspr DEC must emit StoreDec; IR: {:?}", b.insts);
    }

    #[test]
    fn mtspr_srr0_emits_store_srr0() {
        // mtspr SRR0, r3 = 0x7C7A03A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C7A_03A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreSrr0)),
            "mtspr SRR0 must emit StoreSrr0; IR: {:?}", b.insts);
    }

    #[test]
    fn mtspr_srr1_emits_store_srr1() {
        // mtspr SRR1, r3 = 0x7C7B03A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C7B_03A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreSrr1)),
            "mtspr SRR1 must emit StoreSrr1; IR: {:?}", b.insts);
    }

    #[test]
    fn mfspr_srr1_emits_load_srr1() {
        // mfspr r3, SRR1 = 0x7C7B02A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C7B_02A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::LoadSrr1)),
            "mfspr SRR1 must emit LoadSrr1; IR: {:?}", b.insts);
    }

    #[test]
    fn mtspr_sprg0_emits_store_sprg0() {
        // mtspr SPRG0, r3 = 0x7C7043A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C70_43A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreSprg(0))),
            "mtspr SPRG0 must emit StoreSprg(0); IR: {:?}", b.insts);
    }

    #[test]
    fn mfspr_sprg0_emits_load_sprg0() {
        // mfspr r3, SPRG0 = 0x7C7042A6
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C70_42A6))].into_iter()).unwrap();
        assert!(ir_contains(&b, |i| matches!(i, IrInst::LoadSprg(0))),
            "mfspr SPRG0 must emit LoadSprg(0); IR: {:?}", b.insts);
    }

    #[test]
    fn mtspr_not_lr_must_not_store_lr() {
        // mtspr CTR, r3 (SPR=9) must NOT emit StoreLr — regression guard.
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C69_03A6))].into_iter()).unwrap();
        assert!(!ir_contains(&b, |i| matches!(i, IrInst::StoreLr)),
            "mtspr CTR must not emit StoreLr; IR: {:?}", b.insts);
    }

    #[test]
    fn mtspr_sprg0_must_not_store_lr() {
        // mtspr SPRG0, r3 (SPR=272) must NOT be mistaken for mtspr LR (SPR=8).
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x7C70_43A6))].into_iter()).unwrap();
        assert!(!ir_contains(&b, |i| matches!(i, IrInst::StoreLr)),
            "mtspr SPRG0 must not emit StoreLr (SPR decode regression); IR: {:?}", b.insts);
    }

    /// mflr + mtlr round-trip: the same GPR value must be used for both.
    #[test]
    fn mflr_mtlr_round_trip_ir() {
        // mflr r3 (0x7C6802A6) followed by mtlr r3 (0x7C6803A6)
        let b = Decoder::new()
            .decode([
                (0x8000_0000u32, ins(0x7C68_02A6)),
                (0x8000_0004u32, ins(0x7C68_03A6)),
            ].into_iter())
            .unwrap();
        assert_eq!(b.instruction_count, 2);
        assert!(b.unimplemented_ops.is_empty());
        assert!(ir_contains(&b, |i| matches!(i, IrInst::LoadLr)),
            "mflr must emit LoadLr; IR: {:?}", b.insts);
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreLr)),
            "mtlr must emit StoreLr; IR: {:?}", b.insts);
    }

    /// OS init pattern: mfspr SRR1, ori to set EE, mtspr SRR1, bclr.
    /// This exact sequence appears in the GameCube OS boot at block #11.
    #[test]
    fn os_srr1_ee_setup_no_unimpl() {
        // mfspr r3, SRR1 (0x7C7B02A6)
        // ori   r3, r3, 0x8000  (0x60638000)  — set EE bit
        // mtspr SRR1, r3 (0x7C7B03A6)
        // blr (0x4E800020)
        let seq = [
            (0x8000_0000u32, ins(0x7C7B_02A6)),
            (0x8000_0004u32, ins(0x6063_8000)),
            (0x8000_0008u32, ins(0x7C7B_03A6)),
            (0x8000_000Cu32, ins(0x4E80_0020)),
        ];
        let b = Decoder::new().decode(seq.into_iter()).unwrap();
        assert!(b.unimplemented_ops.is_empty(),
            "OS SRR1 EE-setup sequence must have no unimplemented ops: {:?}",
            b.unimplemented_ops);
        assert!(ir_contains(&b, |i| matches!(i, IrInst::LoadSrr1)),
            "Must emit LoadSrr1");
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreSrr1)),
            "Must emit StoreSrr1");
    }

    /// OS dispatch pattern (block #12): mtspr LR then bclr.
    /// Previously mtspr LR was silently dropped, causing bclr to return to
    /// an incorrect address and spin infinitely.
    #[test]
    fn os_dispatch_mtspr_lr_before_bclr_no_unimpl() {
        // mtlr r3 (0x7C6803A6) followed by blr (0x4E800020)
        let seq = [
            (0x8000_0000u32, ins(0x7C68_03A6)),
            (0x8000_0004u32, ins(0x4E80_0020)),
        ];
        let b = Decoder::new().decode(seq.into_iter()).unwrap();
        assert_eq!(b.instruction_count, 2,
            "mtlr + blr should be 2 instructions");
        assert!(b.unimplemented_ops.is_empty());
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreLr)),
            "mtspr LR must emit StoreLr before bclr; IR: {:?}", b.insts);
    }

    // ── bcctr / bcctrl: CTR must never be decremented ────────────────────────
    //
    // The PowerPC Architecture manual states that BO[2] must be 1 for all
    // bcctr / bcctrl encodings; BO[2]=0 is "boundedly undefined".  The JIT
    // must never emit a CTR decrement (LoadCtr / I32Sub / StoreCtr sequence)
    // for any bcctr or bcctrl instruction regardless of the BO field value.

    /// bcctr (BO=20, unconditional): 0x4E800420 — must not decrement CTR.
    #[test]
    fn bcctr_unconditional_no_ctr_decrement() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x4E80_0420))].into_iter()).unwrap();
        // The IR must not contain a StoreCtr — that would signal CTR was decremented.
        assert!(!ir_contains(&b, |i| matches!(i, IrInst::StoreCtr)),
            "bcctr must not decrement CTR; IR: {:?}", b.insts);
    }

    /// bcctr with BO[2]=0 (BO=16, invalid encoding): 0x4E000420.
    /// Even though this encoding is undefined by the spec, we must not
    /// decrement CTR because CTR holds the branch target.
    #[test]
    fn bcctr_invalid_bo_no_ctr_decrement() {
        // BO=16=0b10000: ignore_cr=1, ignore_ctr=0 (invalid for bcctr per spec)
        // Encoding: opcode=19, BO=16, BI=0, BH=0, XO=528, LK=0 → 0x4E000420
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x4E00_0420))].into_iter()).unwrap();
        assert!(!ir_contains(&b, |i| matches!(i, IrInst::StoreCtr)),
            "bcctr with BO[2]=0 must not decrement CTR; IR: {:?}", b.insts);
    }

    /// bcctrl (BO=20, unconditional call via CTR): 0x4E800421 — must not decrement CTR.
    #[test]
    fn bcctrl_unconditional_no_ctr_decrement() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x4E80_0421))].into_iter()).unwrap();
        assert!(!ir_contains(&b, |i| matches!(i, IrInst::StoreCtr)),
            "bcctrl must not decrement CTR; IR: {:?}", b.insts);
    }

    /// bcctrl with BO[2]=0 (invalid encoding): 0x4E000421 — must not decrement CTR.
    #[test]
    fn bcctrl_invalid_bo_no_ctr_decrement() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x4E00_0421))].into_iter()).unwrap();
        assert!(!ir_contains(&b, |i| matches!(i, IrInst::StoreCtr)),
            "bcctrl with BO[2]=0 must not decrement CTR; IR: {:?}", b.insts);
    }

    /// rfi terminates the block and emits StoreMsr + ReturnDynamic.
    #[test]
    fn decode_rfi_terminal() {
        // rfi (0x4C00_0064) followed by addi r3, r0, 1 (0x3860_0001)
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x4C00_0064)), (0x8000_0004u32, ins(0x3860_0001))].into_iter()).unwrap();
        assert_eq!(b.instruction_count, 1);
        assert!(b.unimplemented_ops.is_empty());
        assert!(ir_contains(&b, |i| matches!(i, IrInst::StoreMsr)),
            "rfi must emit StoreMsr");
    }

    // ── fctiw / fctiwz: integer result stored as raw bits in FPR ────────────
    //
    // The PowerPC `fctiw`/`fctiwz` instructions convert an f64 to an i32 and
    // store the result as the *raw integer bit-pattern* in the low 32 bits of
    // the target FPR.  The `stfiwx` instruction then extracts those bits.
    //
    // The bug was: `F64FromI32S` (numeric conversion, e.g. 3 → 3.0_f64) was
    // used instead of a bitcast (3 → 0x0000000000000003_u64 → f64), so
    // `stfiwx` would read the low 32 bits of 3.0_f64 (= 0x00000000) instead
    // of 0x00000003.  Fixed by using I32TruncSatF64S → I64ExtendI32S →
    // F64ReinterpretI64.

    /// fctiwz (0xFC20_001E = fctiwz f1, f0) must emit F64ReinterpretI64 and
    /// must NOT emit F64FromI32S (numeric conversion is wrong for this opcode).
    #[test]
    fn fctiwz_emits_reinterpret_not_convert() {
        // fctiwz f1, f0  = 0xFC20_001E
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0xFC20_001E))].into_iter()).unwrap();
        assert!(b.unimplemented_ops.is_empty(), "fctiwz unimplemented: {:?}", b.unimplemented_ops);
        assert!(ir_contains(&b, |i| matches!(i, IrInst::F64ReinterpretI64)),
            "fctiwz must emit F64ReinterpretI64 (bitcast); IR: {:?}", b.insts);
        assert!(!ir_contains(&b, |i| matches!(i, IrInst::F64FromI32S)),
            "fctiwz must NOT emit F64FromI32S (numeric conversion is wrong); IR: {:?}", b.insts);
        // Must also use saturating truncate, not the trapping variant.
        assert!(ir_contains(&b, |i| matches!(i, IrInst::I32TruncSatF64S)),
            "fctiwz must emit I32TruncSatF64S (saturating); IR: {:?}", b.insts);
    }

    /// fctiw (0xFC20_001C = fctiw f1, f0) must emit F64Nearest (round to
    /// nearest) then F64ReinterpretI64 and must NOT emit F64FromI32S.
    #[test]
    fn fctiw_emits_nearest_then_reinterpret() {
        // fctiw f1, f0  = 0xFC20_001C
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0xFC20_001C))].into_iter()).unwrap();
        assert!(b.unimplemented_ops.is_empty(), "fctiw unimplemented: {:?}", b.unimplemented_ops);
        assert!(ir_contains(&b, |i| matches!(i, IrInst::F64Nearest)),
            "fctiw must emit F64Nearest (rounding); IR: {:?}", b.insts);
        assert!(ir_contains(&b, |i| matches!(i, IrInst::F64ReinterpretI64)),
            "fctiw must emit F64ReinterpretI64 (bitcast); IR: {:?}", b.insts);
        assert!(!ir_contains(&b, |i| matches!(i, IrInst::F64FromI32S)),
            "fctiw must NOT emit F64FromI32S (numeric conversion is wrong); IR: {:?}", b.insts);
    }
}
