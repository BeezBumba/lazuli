use gekko::disasm::Ins;
use lazuli::system::System;
use crate::compare::update_cr0;

/* ADDITION */
#[derive(Clone, Copy)]
enum AddLhs {
    RA,
    ZeroOrRA,
}


#[derive(Clone, Copy)]
enum AddRhs {
    RB,
    Imm,
    ShiftedImm,
    Zero,
    MinusOne,
}

#[derive(Clone, Copy)]
struct AddOp {
    lhs: AddLhs,
    rhs: AddRhs,
    extend: bool,
    record: bool,
    carry: bool,
    overflow: bool,
}

struct AddOperands {
    lhs: u32,
    rhs: u32,
    cin: u32,
}


fn get_add_operands(
    ins: Ins,
    sys: &mut System,
    lhs_mode: AddLhs,
    rhs_mode: AddRhs,
    extend: bool,
) -> AddOperands {
    let lhs = match lhs_mode {
        AddLhs::RA => sys.cpu.user.gpr[ins.field_ra() as usize],
        AddLhs::ZeroOrRA => {
            if ins.field_ra() == 0 {
                0
            } else {
                sys.cpu.user.gpr[ins.field_ra() as usize]
            }
        }
    };

    let rhs = match rhs_mode {
        AddRhs::RB => sys.cpu.user.gpr[ins.field_rb() as usize],
        AddRhs::Imm => ins.field_simm() as i16 as i32 as u32,
        AddRhs::ShiftedImm => ((ins.field_simm() as i16 as i32) << 16) as u32,
        AddRhs::Zero => 0,
        AddRhs::MinusOne => 0xFFFF_FFFF,
    };

    let cin = if extend {
        sys.cpu.user.xer.carry() as u32
    } else {
        0
    };

    AddOperands { lhs, rhs, cin }
}

fn detect_overflow(lhs: u32, rhs: u32, result: u32) -> bool {
    let lhs_sign = (lhs >> 31) & 1;
    let rhs_sign = (rhs >> 31) & 1;
    let res_sign = (result >> 31) & 1;

    (lhs_sign == rhs_sign) && (res_sign != lhs_sign)
}



fn execute_add(
    ins: Ins,
    sys: &mut System,
    lhs_mode: AddLhs,
    rhs_mode: AddRhs,
    extend: bool,
    record: bool,
    carry_out: bool,
    overflow_flag: bool,
) {
    let ops = get_add_operands(ins, sys, lhs_mode, rhs_mode, extend);

    let (tmp, c1) = ops.lhs.overflowing_add(ops.rhs);
    let (result, c2) = tmp.overflowing_add(ops.cin);

    let carry = c1 || c2;

    if overflow_flag {
        let ov = detect_overflow(ops.lhs, ops.rhs, result);

        sys.cpu.user.xer.set_overflow(ov);
        if ov {
            sys.cpu.user.xer.set_overflow_fuse(true);
        }
    }

    if carry_out {
        sys.cpu.user.xer.set_carry(carry);
    }

    if record {
    let val = result as i32;

    update_cr0(result, sys);
}

    sys.cpu.user.gpr[ins.field_rd() as usize] = result;
}

pub fn add(ins: Ins, sys: &mut System) {
    execute_add(
        ins, sys,
        AddLhs::RA,
        AddRhs::RB,
        false,
        ins.field_rc(),
        false,
        ins.field_oe(),
    );
}

pub fn adde(ins: Ins, sys: &mut System) {
    execute_add(
        ins,
        sys,
        AddLhs::RA,       
        AddRhs::RB,       
        true,             
        ins.field_rc(),   
        true,             
        ins.field_oe(),   
    );
}

pub fn addc(ins: Ins, sys: &mut System) {
    execute_add(
        ins,
        sys,
        AddLhs::RA,       
        AddRhs::RB,       
        true,            
        ins.field_rc(),  
        true,            
        ins.field_oe(),  
    );
}

pub fn addi(ins: Ins, sys: &mut System) {
    execute_add(
        ins, sys,
        AddLhs::ZeroOrRA,
        AddRhs::Imm,
        false,
        false,
        false,
        false,
    );
}

pub fn addic(ins: Ins, sys: &mut System) {
    execute_add(
        ins, sys,
        AddLhs::RA,
        AddRhs::Imm,
        false,
        false,
        true,
        false,
    );
}

pub fn addic_(ins: Ins, sys: &mut System) {
    execute_add(
        ins, sys,
        AddLhs::RA,
        AddRhs::Imm,
        false,
        true,
        true,
        false,
    );
}

pub fn addis(ins: Ins, sys: &mut System) {
    execute_add(
        ins, sys,
        AddLhs::ZeroOrRA,
        AddRhs::ShiftedImm,
        false,
        false,
        false,
        false,
    );
}
pub fn addze(ins: Ins, sys: &mut System) {
    execute_add(
        ins, sys,
        AddLhs::RA,
        AddRhs::Zero,
        true,  
        false,  
        true,  
        false, 
    );
}

/* SUBTRACTION */

#[derive(Clone, Copy)]
enum SubLhs {
    RA,
    RB,
    ZeroOrRA,
    MinusOne,
}

#[derive(Clone, Copy)]
enum SubRhs {
    RB,
    RA,
    Imm,
    ShiftedImm,
    Zero,
    MinusOne,
}

#[derive(Clone, Copy)]
struct SubOp {
    lhs: SubLhs,
    rhs: SubRhs,
    extend: bool,
    record: bool,
    carry: bool,
    overflow: bool,
}

struct SubOperands {
    lhs: u32,
    rhs: u32,
    cin: u32,
}


fn get_sub_operands(
    ins: Ins,
    sys: &mut System,
    lhs_mode: SubLhs,
    rhs_mode: SubRhs,
    extend: bool,
) -> SubOperands {
    let ra = sys.cpu.user.gpr[ins.field_ra() as usize];
    let rb = sys.cpu.user.gpr[ins.field_rb() as usize];

    let lhs = match lhs_mode {
        SubLhs::RA => ra,
        SubLhs::RB => rb,
        SubLhs::ZeroOrRA => {
            if ins.field_ra() == 0 {
                0
            } else {
                ra
            }
        }
        SubLhs::MinusOne => 0xFFFF_FFFF, 
    };

    let rhs = match rhs_mode {
        SubRhs::RB => rb,
        SubRhs::RA => ra,
        SubRhs::Imm => ins.field_simm() as i16 as i32 as u32,
        SubRhs::ShiftedImm => ((ins.field_simm() as i16 as i32) << 16) as u32,
        SubRhs::Zero => 0,
        SubRhs::MinusOne => 0xFFFF_FFFF,
    };

    let cin = if extend {
        1 - sys.cpu.user.xer.carry() as u32
    } else {
        0
    };

    SubOperands { lhs, rhs, cin }
}

fn execute_sub(
    ins: Ins,
    sys: &mut System,
    lhs_mode: SubLhs,
    rhs_mode: SubRhs,
    extend: bool,
    record: bool,
    carry_out: bool,
    overflow_flag: bool,
) {
    let ops = get_sub_operands(ins, sys, lhs_mode, rhs_mode, extend);

    let (tmp, b1) = ops.lhs.overflowing_sub(ops.rhs);
    let (result, b2) = tmp.overflowing_sub(ops.cin);

    let borrow = b1 || b2;
    let carry = !borrow; 

    if overflow_flag {
        let ov = detect_overflow(ops.lhs, ops.rhs, result);
        sys.cpu.user.xer.set_overflow(ov);
        if ov {
            sys.cpu.user.xer.set_overflow_fuse(true);
        }
    }

    sys.cpu.user.gpr[ins.field_rd() as usize] = result;

    if carry_out {
        sys.cpu.user.xer.set_carry(carry);
    }

    if record {
        let val = result as i32;
        update_cr0(result, sys);
    }
}

pub fn subf(ins: Ins, sys: &mut System) {
    execute_sub(
        ins, sys,
        SubLhs::RB,
        SubRhs::RA,
        false, 
        false, 
        false,  
        false,  
    );
}
pub fn subfc(ins: Ins, sys: &mut System) {
    execute_sub(
        ins, sys,
        SubLhs::RB,
        SubRhs::RA,
        false,  
        false,  
        true,   
        false, 
    );
}
pub fn subfe(ins: Ins, sys: &mut System) {
    execute_sub(
        ins, sys,
        SubLhs::RB,
        SubRhs::RA,
        true,   
        false,  
        true,   
        false,  
    );
}
pub fn subfic(ins: Ins, sys: &mut System) {
    execute_sub(
        ins, sys,
        SubLhs::RA,
        SubRhs::Imm,
        false,  
        false, 
        true,   
        false,  
    );
}
pub fn subfme(ins: Ins, sys: &mut System) {
    execute_sub(
        ins, sys,
        SubLhs::MinusOne,
        SubRhs::RA,
        true,  
        false,  
        true,  
        false,  
    );
}
pub fn subfze(ins: Ins, sys: &mut System) {
    execute_sub(
        ins, sys,
        SubLhs::ZeroOrRA,
        SubRhs::Zero,
        true,  
        false,  
        true,  
        false,  
    );
}

pub fn neg(ins: Ins, sys: &mut System) {
    let rD = ins.field_rd() as usize;
    let rA = ins.field_ra() as usize;

    let a = sys.cpu.user.gpr[rA];
    let oe = ins.field_oe();  
    let rc = ins.field_rc();   
    let result = (0u32).wrapping_sub(a);

 let overflow = oe && a == 0x8000_0000;

if overflow {
    sys.cpu.user.xer.set_overflow(true);       
    sys.cpu.user.xer.set_overflow_fuse(true);   
}

    sys.cpu.user.gpr[rD] = result;

    if rc {
        update_cr0(result,sys);
    }
}

/* MULTIPLIATION */

#[derive(Clone, Copy)]
enum MulRhs {
    RB,
    Imm,
}


struct MulOperands {
    lhs: u32,
    rhs: u32,
}

fn get_mul_operands(ins: Ins, sys: &mut System, rhs_mode: MulRhs) -> MulOperands {
    let lhs = sys.cpu.user.gpr[ins.field_ra() as usize];

    let rhs = match rhs_mode {
        MulRhs::RB => sys.cpu.user.gpr[ins.field_rb() as usize],
        MulRhs::Imm => ins.field_simm() as i16 as i32 as u32,
    };

    MulOperands { lhs, rhs }
}

fn detect_mul_overflow(lhs: u32, rhs: u32, result: u32) -> bool {
    let lhs_s = lhs as i32 as i64;
    let rhs_s = rhs as i32 as i64;
    let prod = lhs_s * rhs_s;
    prod != (result as i32 as i64)
}


fn execute_mul(
    ins: Ins,
    sys: &mut System,
    rhs_mode: MulRhs,
    record: bool,
    overflow_flag: bool,
) {
    let ops = get_mul_operands(ins, sys, rhs_mode);

    let lhs_s = ops.lhs as i32 as i64;
    let rhs_s = ops.rhs as i32 as i64;
    let prod = lhs_s * rhs_s;

    let result = prod as u32;

    if overflow_flag {
        let ov = detect_mul_overflow(ops.lhs, ops.rhs, result);
        sys.cpu.user.xer.set_overflow(ov);
        if ov {
            sys.cpu.user.xer.set_overflow_fuse(true);
        }
    }

    if record {
        update_cr0(result, sys);
    }

    sys.cpu.user.gpr[ins.field_rd() as usize] = result;
}

pub fn mullw(ins: Ins, sys: &mut System) {
    execute_mul(
        ins,
        sys,
        MulRhs::RB,
        ins.field_rc(),
        ins.field_oe(),
    );
}

pub fn mulli(ins: Ins, sys: &mut System) {
    execute_mul(
        ins,
        sys,
        MulRhs::Imm,
        false,        
        false,        
    );
}

pub fn mulhwu(ins: Ins, sys: &mut System) {
    let ops = get_mul_operands(ins, sys, MulRhs::RB);

    let a = ops.lhs as u64;
    let b = ops.rhs as u64;
    let prod = a * b;

    let result = (prod >> 32) as u32;

    sys.cpu.user.gpr[ins.field_rd() as usize] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}

pub fn mulhw(ins: Ins, sys: &mut System) {
    let ops = get_mul_operands(ins, sys, MulRhs::RB);

    let a = ops.lhs as i32 as i64;
    let b = ops.rhs as i32 as i64;
    let result = ((a * b) >> 32) as u32;

    sys.cpu.user.gpr[ins.field_rd() as usize] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}

/* DIVISION */

#[derive(Clone, Copy)]
enum DivType {
    Signed,
    Unsigned,
}

struct DivOperands {
    lhs: u32,
    rhs: u32,
}

fn get_div_operands(ins: Ins, sys: &mut System) -> DivOperands {
    DivOperands {
        lhs: sys.cpu.user.gpr[ins.field_ra() as usize],
        rhs: sys.cpu.user.gpr[ins.field_rb() as usize],
    }
}

fn detect_div_overflow(lhs: u32, rhs: u32, signed: bool) -> bool {
    if rhs == 0 {
        return true;
    }
    if signed {
        let lhs_s = lhs as i32;
        let rhs_s = rhs as i32;
        lhs_s == i32::MIN && rhs_s == -1
    } else {
        false
    }
}

fn execute_div(
    ins: Ins,
    sys: &mut System,
    div_type: DivType,
    record: bool,
    overflow_flag: bool,
) {
    let ops = get_div_operands(ins, sys);

    let overflow = match div_type {
        DivType::Signed => detect_div_overflow(ops.lhs, ops.rhs, true),
        DivType::Unsigned => detect_div_overflow(ops.lhs, ops.rhs, false),
    };

    let result = if overflow {
        0
    } else {
        match div_type {
            DivType::Signed => {
                let lhs_s = ops.lhs as i32;
                let rhs_s = ops.rhs as i32;
                (lhs_s / rhs_s) as u32
            }
            DivType::Unsigned => ops.lhs / ops.rhs,
        }
    };

    if overflow_flag {
        sys.cpu.user.xer.set_overflow(overflow);
        if overflow {
            sys.cpu.user.xer.set_overflow_fuse(true);
        }
    }

    if record {
        update_cr0(result, sys);
    }

    sys.cpu.user.gpr[ins.field_rd() as usize] = result;
}

pub fn divw(ins: Ins, sys: &mut System) {
    execute_div(
        ins,
        sys,
        DivType::Signed,
        ins.field_rc(),
        ins.field_oe(),
    );
}

pub fn divwu(ins: Ins, sys: &mut System) {
    execute_div(
        ins,
        sys,
        DivType::Unsigned,
        ins.field_rc(),
        ins.field_oe(),
    );
}