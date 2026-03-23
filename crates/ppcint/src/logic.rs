use gekko::disasm::Ins;
use lazuli::system::System;
use gekko::CondReg;
use zerocopy::FromBytes;
use zerocopy::IntoBytes;
use gekko::FloatRounding;
use crate::compare::update_cr0;

pub fn ori(ins: Ins, sys: &mut System) {
    let rs_idx = ins.field_rs() as usize;
    let ra_idx = ins.field_ra() as usize;
    let imm = ins.field_uimm() as u32;

    let rs_val =  sys.cpu.user.gpr[rs_idx];
    sys.cpu.user.gpr[ra_idx] = rs_val | imm;
}

pub fn or(ins: Ins, sys: &mut System) {
    let rb_idx = ins.field_rb() as usize;
    let ra_idx = ins.field_ra() as usize;
    let rs_idx = ins.field_rs() as usize;

    let rb_val =  sys.cpu.user.gpr[rb_idx];
    let rs_val =  sys.cpu.user.gpr[rs_idx];
    sys.cpu.user.gpr[ra_idx] = rs_val | rb_val;
}

pub fn ppc_mask(mb: u32, me: u32) -> u32 {
    let mb = mb & 31;
    let me = me & 31;

    if mb <= me {

        let count = me - mb + 1;
        (0xFFFF_FFFFu32 >> (32 - count)) << (31 - me)
    } else {

        let left  = 0xFFFF_FFFFu32 >> mb;
        let right = 0xFFFF_FFFFu32 << (31 - me);
        left | right
    }
}



pub fn rlwinm(ins: Ins, sys: &mut System) {
    let rs_idx = ins.field_rs() as usize;
    let ra_idx = ins.field_ra() as usize;
    let sh    = ins.field_sh() as u32;
    let mb    = ins.field_mb() as u32; 
    let me    = ins.field_me() as u32; 

    let rs_val = sys.cpu.user.gpr[rs_idx];

    let rotated = rs_val.rotate_left(sh);

    let mask = ppc_mask(mb, me);

    let result = rotated & mask;
    sys.cpu.user.gpr[ra_idx] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}

pub fn rlwimi(ins: Ins, sys: &mut System) {
    let ra = ins.field_ra() as usize;
    let rs = ins.field_rs() as usize;

    let sh = ins.field_sh() as u32;
    let mb = ins.field_mb() as u32;
    let me = ins.field_me() as u32;

    let a = sys.cpu.user.gpr[ra];
    let s = sys.cpu.user.gpr[rs];

    let rot = s.rotate_left(sh);

    let mask = ppc_mask(mb, me);

    let result = (a & !mask) | (rot & mask);

    sys.cpu.user.gpr[ra] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}

pub fn andi_(ins: Ins, sys: &mut System) {
    let rA = ins.field_ra() as usize;
    let rS = ins.field_rs() as usize;
    let uimm = ins.field_uimm() as u32;

    let result = sys.cpu.user.gpr[rS] & uimm;
    sys.cpu.user.gpr[rA] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}
pub fn xoris(ins: Ins, sys: &mut System) {
    let rs = sys.cpu.user.gpr[ins.field_rs() as usize];
    let uimm = ins.field_uimm() as u32;

    let shifted = uimm << 16;
    let result = rs ^ shifted;

    sys.cpu.user.gpr[ins.field_ra() as usize] = result;
}

pub fn nor(ins: Ins, sys: &mut System) {
    let rA = ins.field_ra() as usize;
    let rS = ins.field_rs() as usize;
    let rB = ins.field_rb() as usize;

    let s = sys.cpu.user.gpr[rS];
    let b = sys.cpu.user.gpr[rB];

    let result = !(s | b);
    sys.cpu.user.gpr[rA] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}

pub fn crxor(ins: Ins, sys: &mut System) {
    let d = ins.field_crbd() as u32; 
    let a = ins.field_crba() as u32;
    let b = ins.field_crbb() as u32;

    let mut cr_u32 = u32::from_be_bytes(sys.cpu.user.cr.as_bytes().try_into().unwrap());

    let pos_d = 31 - d;
    let pos_a = 31 - a;
    let pos_b = 31 - b;

    let bit_a = (cr_u32 >> pos_a) & 1;
    let bit_b = (cr_u32 >> pos_b) & 1;
    let res   = bit_a ^ bit_b;

    cr_u32 &= !(1 << pos_d);

    cr_u32 |= res << pos_d;

    let mut bytes = cr_u32.to_be_bytes();
    let new_cr = CondReg::mut_from_bytes(&mut bytes).unwrap();
    sys.cpu.user.cr = new_cr.clone();
}

pub fn and(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ra = ins.field_ra() as usize;
    let rb = ins.field_rb() as usize;

    let s = sys.cpu.user.gpr[rs];
    let b = sys.cpu.user.gpr[rb];

    let result = s & b;

    sys.cpu.user.gpr[ra] = result;

    if ins.field_rc() {
        let val = result as i32;

        let mut field = sys.cpu.user.cr.fields_at(7).unwrap_or_default();
        field.set_lt(val < 0);
        field.set_gt(val > 0);
        field.set_eq(val == 0);
        field.set_ov(sys.cpu.user.xer.overflow_fuse());
        sys.cpu.user.cr.set_fields_at(7, field);
    }
}

pub fn oris(ins: Ins, sys: &mut System) {
    let d = ins.field_rd() as usize;
    let a = ins.field_ra() as usize;
    let imm = (ins.field_uimm() as u32) << 16;

    let ra_val = if a == 0 {
        0
    } else {
        sys.cpu.user.gpr[a]
    };

    sys.cpu.user.gpr[d] = ra_val | imm;
}

pub fn mtfsb1(ins: Ins, sys: &mut System) {
    let crbd = ins.field_crbd() as u32;
    let fpscr = &mut sys.cpu.user.fpscr;

    match crbd {

        2 => fpscr.set_ieee_disabled(true),

        3 => fpscr.set_inexact_exception_enabled(true),
        4 => fpscr.set_zero_divide_exception_enabled(true),
        5 => fpscr.set_underflow_exception_enabled(true),
        6 => fpscr.set_overflow_exception_enabled(true),
        7 => fpscr.set_invalid_exception_enabled(true),


        8  => fpscr.set_invalid_conversion_exception(true),
        9  => fpscr.set_invalid_sqrt_exception(true),
        10 => fpscr.set_invalid_soft_exception(true),


        17 => fpscr.set_fraction_inexact(true),
        18 => fpscr.set_fraction_rounded(true),

        19 => fpscr.set_invalid_compare_exception(true),
        20 => fpscr.set_invalid_inf_mul_zero_exception(true),
        21 => fpscr.set_invalid_zero_div_zero_exception(true),
        22 => fpscr.set_invalid_inf_div_inf_exception(true),
        23 => fpscr.set_invalid_inf_sub_inf_exception(true),
        24 => fpscr.set_invalid_snan_exception(true),

        25 => fpscr.set_inexact_exception(true),
        26 => fpscr.set_zero_divide_exception(true),
        27 => fpscr.set_underflow_exception(true),
        28 => fpscr.set_overflow_exception(true),

        29 | 30 | 31 => {}

        _ => {}
    }

    if ins.field_rc() {
        let summary = fpscr.exception_summary();
        let mut field = sys.cpu.user.cr.fields_at(1).unwrap_or_default();

        field.set_eq(!summary);
        field.set_gt(false);
        field.set_lt(summary);
        field.set_ov(false);

        sys.cpu.user.cr.set_fields_at(1, field);
    }
}

pub fn andc(ins: Ins, sys: &mut System) {
    let s = sys.cpu.user.gpr[ins.field_rs() as usize];
    let b = sys.cpu.user.gpr[ins.field_rb() as usize];

    let result = s & !b;

    sys.cpu.user.gpr[ins.field_ra() as usize] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}

pub fn cntlzw(ins: Ins, sys: &mut System) {
    let s = sys.cpu.user.gpr[ins.field_rs() as usize];

    let result = s.leading_zeros();

    sys.cpu.user.gpr[ins.field_ra() as usize] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}
pub fn xor(ins: Ins, sys: &mut System) {
    let ra = ins.field_ra() as usize;
    let rs = ins.field_rs() as usize;
    let rb = ins.field_rb() as usize;

    let lhs = sys.cpu.user.gpr[rs];
    let rhs = sys.cpu.user.gpr[rb];

    let result = lhs ^ rhs;

    sys.cpu.user.gpr[ra] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}

pub fn slw(ins: Ins, sys: &mut System) {
    let rs = sys.cpu.user.gpr[ins.field_rs() as usize];
    let rb = sys.cpu.user.gpr[ins.field_rb() as usize];

    let shift = (rb & 0x1F) as u32;

    let result = rs << shift;

    sys.cpu.user.gpr[ins.field_ra() as usize] = result;
}

pub fn sraw(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ra = ins.field_ra() as usize;
    let rb = ins.field_rb() as usize;

    let s  = sys.cpu.user.gpr[rs] as i32;
    let sh = (sys.cpu.user.gpr[rb] & 0x1F) as u32;


    let result = if sh < 32 {
        (s >> sh) as u32
    } else {

        if s < 0 { 0xFFFF_FFFF } else { 0 }
    };

    let shifted_out = if sh == 0 {
        0
    } else {
        let mask = (1u32 << sh) - 1;
        sys.cpu.user.gpr[rs] & mask
    };

    sys.cpu.user.xer.set_carry(shifted_out != 0);

    sys.cpu.user.gpr[ra] = result;

    if ins.field_rc() {
    update_cr0(result, sys);
}
}