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

    pub fn decode(&self, instructions: impl Iterator<Item = (u32, Ins)>) -> Option<IrBlock> {
        let mut block = IrBlock::default();
        let mut last_pc = 0u32;
        let mut count = 0u32;
        for (pc, ins) in instructions {
            last_pc = pc;
            count += 1;
            if self.emit_inst(&mut block, ins, pc) {
                return Some(block);
            }
        }
        if count == 0 { return None; }
        block.push(IrInst::ReturnStatic(last_pc + 4));
        Some(block)
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
        let lt_bit = 31u32.wrapping_sub((cr_fd as u32) * 4);
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
        b.push(IrInst::LoadCr);
        b.push(IrInst::I32Const(!mask as i32)); b.push(IrInst::I32And);

        b.push(IrInst::LocalGet(l_lt)); b.push(IrInst::I32Const(lt_bit as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);
        b.push(IrInst::LocalGet(l_gt)); b.push(IrInst::I32Const(gt_bit as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);
        b.push(IrInst::LocalGet(l_eq)); b.push(IrInst::I32Const(eq_bit as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);
        b.push(IrInst::LocalGet(l_so)); b.push(IrInst::I32Const(so_bit as i32)); b.push(IrInst::I32Shl); b.push(IrInst::I32Or);

        b.push(IrInst::StoreCr);
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

    fn emit_inst(&self, b: &mut IrBlock, ins: Ins, pc: u32) -> bool {
        b.instruction_count += 1;
        b.cycles += 1;
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
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(ins.field_simm() as i32)); b.push(IrInst::I32Add);
                b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Addic_ => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(ins.field_simm() as i32)); b.push(IrInst::I32Add);
                self.int_result(b, rd, true);
            }
            Opcode::Subf => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8; let rc = ins.field_rc();
                b.push(IrInst::LoadGpr(rb)); b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Sub);
                self.int_result(b, rd, rc);
            }
            Opcode::Subfic => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8;
                b.push(IrInst::I32Const(ins.field_simm() as i32)); b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Sub);
                b.push(IrInst::StoreGpr(rd));
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
            Opcode::Divw => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32DivS);
                b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Divwu => {
                let rd = ins.gpr_d() as u8; let ra = ins.gpr_a() as u8; let rb = ins.gpr_b() as u8;
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32DivU);
                b.push(IrInst::StoreGpr(rd));
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
            Opcode::Eqv   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Xor); b.push(IrInst::I32Not); self.int_result(b,ra,rc); }
            Opcode::Slw   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Shl); self.int_result(b,ra,rc); }
            Opcode::Srw   => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32ShrU); self.int_result(b,ra,rc); }
            Opcode::Sraw  => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let rb=ins.gpr_b() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32ShrS); b.push(IrInst::StoreGpr(ra)); }
            Opcode::Srawi => { let ra=ins.gpr_a() as u8; let rs=ins.gpr_s() as u8; let sh=ins.field_sh() as i32; let rc=ins.field_rc(); b.push(IrInst::LoadGpr(rs)); b.push(IrInst::I32Const(sh)); b.push(IrInst::I32ShrS); self.int_result(b,ra,rc); }
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
                let loc=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::LoadGpr(rb)); b.push(IrInst::I32Sub);
                b.push(IrInst::LocalSet(loc)); self.update_cr_signed(b,cr,loc);
            }
            Opcode::Cmpi => {
                let cr=ins.field_crfd() as u8; let ra=ins.gpr_a() as u8;
                let loc=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadGpr(ra)); b.push(IrInst::I32Const(ins.field_simm() as i32)); b.push(IrInst::I32Sub);
                b.push(IrInst::LocalSet(loc)); self.update_cr_signed(b,cr,loc);
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
            Opcode::Fctiw | Opcode::Fctiwz => { let fd=ins.fpr_d() as u8; let fb=ins.fpr_b() as u8; b.push(IrInst::LoadFprPs0(fb)); b.push(IrInst::I32TruncF64S); b.push(IrInst::F64FromI32S); b.push(IrInst::StoreFprPs0(fd)); }
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
                return true;
            }
            Opcode::Bc => {
                let bo=ins.field_bo(); let bi=ins.field_bi() as u8; let bd=ins.field_bd(); let lk=ins.field_lk(); let aa=ins.field_aa();
                let taken = if aa { bd as u32 } else { pc.wrapping_add_signed(bd as i32) };
                if lk { b.push(IrInst::I32Const((pc+4) as i32)); b.push(IrInst::StoreLr); }
                self.emit_branch_cond(b, bo as u8, bi);
                b.push(IrInst::BranchIf { taken, fallthrough: pc+4 });
                return true;
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
                return true;
            }
            Opcode::Bcctr => {
                let bo=ins.field_bo(); let bi=ins.field_bi() as u8; let lk=ins.field_lk();
                let ctr=b.alloc_local(IrTy::I32);
                b.push(IrInst::LoadCtr); b.push(IrInst::LocalSet(ctr));
                if lk { b.push(IrInst::I32Const((pc+4) as i32)); b.push(IrInst::StoreLr); }
                if (bo>>4)&1!=0 && (bo>>2)&1!=0 {
                    b.push(IrInst::LocalGet(ctr)); b.push(IrInst::ReturnDynamic);
                } else {
                    self.emit_branch_cond(b, bo as u8, bi);
                    b.push(IrInst::BranchRegIf { reg_local: ctr, fallthrough: pc+4 });
                }
                return true;
            }

            // ── System ────────────────────────────────────────────────────────
            Opcode::Mfspr => {
                let rd=ins.gpr_d() as u8;
                let spr=(ins.field_spr() as u32).rotate_right(5)&0x3FF;
                let load = match spr { 1=>IrInst::LoadXer, 8=>IrInst::LoadLr, 9=>IrInst::LoadCtr, 22=>IrInst::LoadDec, 26=>IrInst::LoadSrr0, 27=>IrInst::LoadSrr1, 16=>IrInst::LoadSprg(0), 17=>IrInst::LoadSprg(1), 18=>IrInst::LoadSprg(2), 19=>IrInst::LoadSprg(3), _=>IrInst::I32Const(0) };
                b.push(load); b.push(IrInst::StoreGpr(rd));
            }
            Opcode::Mtspr => {
                let rs=ins.gpr_s() as u8;
                let spr=(ins.field_spr() as u32).rotate_right(5)&0x3FF;
                b.push(IrInst::LoadGpr(rs));
                match spr { 1=>b.push(IrInst::StoreXer), 8=>b.push(IrInst::StoreLr), 9=>b.push(IrInst::StoreCtr), 22=>b.push(IrInst::StoreDec), 26=>b.push(IrInst::StoreSrr0), 27=>b.push(IrInst::StoreSrr1), 16=>b.push(IrInst::StoreSprg(0)), 17=>b.push(IrInst::StoreSprg(1)), 18=>b.push(IrInst::StoreSprg(2)), 19=>b.push(IrInst::StoreSprg(3)), _=>b.push(IrInst::Drop) }
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
            Opcode::Mfmsr => { let rd=ins.gpr_d() as u8; b.push(IrInst::LoadMsr); b.push(IrInst::StoreGpr(rd)); }
            Opcode::Mtmsr => { let rs=ins.gpr_s() as u8; b.push(IrInst::LoadGpr(rs)); b.push(IrInst::StoreMsr); }
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
                return true;
            }

            // ── Cache/sync hints (no-ops) ─────────────────────────────────────
            Opcode::Sync|Opcode::Isync|Opcode::Eieio|Opcode::Dcbst|Opcode::Dcbz|Opcode::Icbi|Opcode::Dcbi|Opcode::Dcbf => {}

            // ── Unimplemented ─────────────────────────────────────────────────
            _ => {
                b.unimplemented_ops.push(format!("{:?}", ins.op));
                b.push(IrInst::RaiseException(0));
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gekko::disasm::{Extensions, Ins};
    fn ins(code: u32) -> Ins { Ins::new(code, Extensions::gekko_broadway()) }

    #[test] fn ppc_mask_full() { assert_eq!(ppc_mask(0,31), 0xFFFF_FFFF); }
    #[test] fn ppc_mask_low_byte() { assert_eq!(ppc_mask(24,31), 0xFF); }
    #[test] fn decode_empty() { assert!(Decoder::new().decode(std::iter::empty()).is_none()); }
    #[test] fn decode_addi_ok() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x3860_002A))].into_iter()).unwrap();
        assert_eq!(b.instruction_count, 1);
        assert!(b.unimplemented_ops.is_empty());
    }
    #[test] fn decode_branch_terminal() {
        let b = Decoder::new().decode([(0x8000_0000u32, ins(0x4800_0010)), (0x8000_0004u32, ins(0x3860_0001))].into_iter()).unwrap();
        assert_eq!(b.instruction_count, 1);
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
}
