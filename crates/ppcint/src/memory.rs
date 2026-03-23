use zerocopy::IntoBytes;
use zerocopy::FromBytes;
use gekko::disasm::Ins;
use gekko::{Address, Cpu, Cycles, InsExt};
use lazuli::system::System;

#[inline]
fn ea_d_signed(ins: Ins, sys: &System) -> u32 {
    let ra = ins.field_ra() as usize;
    let d  = ins.field_offset() as i16 as i32 as u32;
    let base = if ra == 0 { 0 } else { sys.cpu.user.gpr[ra] };
    base.wrapping_add(d)
}
#[inline]
fn ea_d_zero(ins: Ins, sys: &System) -> u32 {
    let ra = ins.field_ra() as usize;
    let d  = ins.field_offset() as u32;
    let base = if ra == 0 { 0 } else { sys.cpu.user.gpr[ra] };
    base.wrapping_add(d)
}

#[inline]
fn ea_offset_signed(ins: Ins, sys: &System) -> u32 {
    let ra = ins.field_ra() as usize;
    let off = ins.field_offset() as i16 as i32 as u32;
    let base = if ra == 0 { 0 } else { sys.cpu.user.gpr[ra] };
    base.wrapping_add(off)
}
#[inline]
fn ea_offset_zero(ins: Ins, sys: &System) -> u32 {
    let ra = ins.field_ra() as usize;
    let off = ins.field_offset() as u32;
    let base = if ra == 0 { 0 } else { sys.cpu.user.gpr[ra] };
    base.wrapping_add(off)
}
#[inline]
fn ea_indexed(ins: Ins, sys: &System) -> u32 {
    let ra = ins.field_ra() as usize;
    let rb = ins.field_rb() as usize;
    let base = if ra == 0 { 0 } else { sys.cpu.user.gpr[ra] };
    base.wrapping_add(sys.cpu.user.gpr[rb])
}

pub fn lwz(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let ea = ea_offset_signed(ins, sys);
    sys.cpu.user.gpr[rd] = sys.read::<u32>(Address(ea)).expect("lwz: invalid read");
}

pub fn stw(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ea = ea_offset_signed(ins, sys);
    let val = sys.cpu.user.gpr[rs];
    sys.write::<u32>(Address(ea), val);
}

pub fn stwu(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ra = ins.field_ra() as usize;

    let ea = ea_offset_signed(ins, sys);

    if sys.write::<u32>(Address(ea), sys.cpu.user.gpr[rs]) {
        if ra != 0 {
            sys.cpu.user.gpr[ra] = ea;
        }
    }
}

pub fn lbz(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let ea = ea_offset_signed(ins, sys);
    sys.cpu.user.gpr[rd] =
        sys.read::<u8>(Address(ea)).expect("lbz: invalid read") as u32;
}

pub fn lbzu(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let ra = ins.field_ra() as usize;

    let ea = ea_offset_signed(ins, sys);
    sys.cpu.user.gpr[rd] =
        sys.read::<u8>(Address(ea)).expect("lbzu: invalid read") as u32;

    if ra != 0 {
        sys.cpu.user.gpr[ra] = ea;
    }
}

pub fn lhzu(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let ra = ins.field_ra() as usize;

    if ra == 0 || ra == rd {
        panic!("lhzu: invalid form (rA == 0 or rA == rD)");
    }

    let offset = ins.field_simm() as i32 as u32;

    let base = sys.cpu.user.gpr[ra];
    let ea = base.wrapping_add(offset);

    let value = sys.read::<u16>(Address(ea)).expect("lhzu: invalid read");

    sys.cpu.user.gpr[rd] = value as u32;

    sys.cpu.user.gpr[ra] = ea;
}

pub fn stb(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ea = ea_offset_signed(ins, sys);
    let value = (sys.cpu.user.gpr[rs] & 0xFF) as u8;
    sys.write::<u8>(Address(ea), value);
}

pub fn stbu(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ra = ins.field_ra() as usize;

    let ea = ea_offset_signed(ins, sys);
    let value = (sys.cpu.user.gpr[rs] & 0xFF) as u8;

    sys.write::<u8>(Address(ea), value);
    if ra != 0 {
        sys.cpu.user.gpr[ra] = ea;
    }
}

pub fn stbx(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ea = ea_indexed(ins, sys);
    let value = (sys.cpu.user.gpr[rs] & 0xFF) as u8;
    sys.write::<u8>(Address(ea), value);
}


pub fn lwzu(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let ra = ins.field_ra() as usize;

    let ea = ea_offset_signed(ins, sys);
    if ra != 0 {
        sys.cpu.user.gpr[ra] = ea;
    }

    sys.cpu.user.gpr[rd] = sys.read::<u32>(Address(ea)).expect("lwzu: invalid read");
}

pub fn stmw(ins: Ins, sys: &mut System) {
    let rS = ins.field_rs() as usize;
    let mut addr = ea_d_signed(ins, sys);

    for reg in rS..32 {
        let value = sys.cpu.user.gpr[reg];
        sys.write::<u32>(Address(addr), value);
        addr = addr.wrapping_add(4);
    }
}

pub fn lwzx(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let ea = ea_indexed(ins, sys);
    sys.cpu.user.gpr[rd] = sys.read::<u32>(Address(ea)).expect("lwzx: invalid read");
}


pub fn lmw(ins: Ins, sys: &mut System) {
    let rD = ins.field_rd() as usize;
    let mut addr = ea_d_signed(ins, sys);

    for reg in rD..32 {
        let value = sys.read::<u32>(Address(addr)).expect("lmw: invalid read");
        sys.cpu.user.gpr[reg] = value;
        addr = addr.wrapping_add(4);
    }
}

pub fn stfdu(ins: Ins, sys: &mut System) {
    let frs = ins.field_frs() as usize;
    let ra  = ins.field_ra() as usize;

    let ea = ea_d_signed(ins, sys);

    if ra != 0 {
        sys.cpu.user.gpr[ra] = ea;
    }

    let fp = sys.cpu.user.fpr[frs].0[0];
    let bytes = fp.to_be_bytes();

    for (i, b) in bytes.iter().enumerate() {
        sys.write::<u8>(Address(ea + i as u32), *b);
    }
}

pub fn lhz(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let ea = ea_d_signed(ins, sys);
    let value = sys.read::<u16>(Address(ea)).expect("lhz: invalid read") as u32;
    sys.cpu.user.gpr[rd] = value;
}

pub fn lhzx(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let ea = ea_indexed(ins, sys);
    let value = sys.read::<u16>(Address(ea)).expect("lhzx: invalid read");
    sys.cpu.user.gpr[rd] = value as u32;
}


pub fn sth(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ea = ea_d_signed(ins, sys);
    let value = (sys.cpu.user.gpr[rs] & 0xFFFF) as u16;

    sys.write::<u8>(Address(ea),     (value >> 8) as u8);
    sys.write::<u8>(Address(ea + 1), (value & 0xFF) as u8);
}


pub fn stwx(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ea = ea_indexed(ins, sys);
    let value = sys.cpu.user.gpr[rs];

    if !sys.write::<u32>(Address(ea), value) {
        panic!("stwx: invalid write");
    }
}

pub fn mftb(ins: Ins, sys: &mut System) {
    let rt  = ins.field_rd() as usize;
    let tbr = ins.field_tbr();
    let pre_tb = sys.cpu.supervisor.misc.tb;
    sys.update_time_base();
    let value = match tbr {
        268 => pre_tb as u32, 
        269 => (pre_tb >> 32) as u32, 
        _ => panic!("invalid TBR {}", tbr),
    };
    sys.cpu.user.gpr[rt] = value;
}