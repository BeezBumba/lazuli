use gekko::disasm::Ins;
use lazuli::system::System;

pub fn update_cr0(val: u32, sys: &mut System) {
    let s = val as i32;

    let mut field = sys.cpu.user.cr.fields_at(7).unwrap_or_default();
    field.set_lt(s < 0);
    field.set_gt(s > 0);
    field.set_eq(s == 0);
    field.set_ov(sys.cpu.user.xer.overflow_fuse());

    sys.cpu.user.cr.set_fields_at(7, field);
}

fn write_cr_field(sys: &mut System, crfd: usize, lt: bool, gt: bool, eq: bool, ov: bool) {
    let idx = 7 - crfd;

    let mut field = sys.cpu.user.cr.fields_at(idx).unwrap_or_default();
    field.set_lt(lt);
    field.set_gt(gt);
    field.set_eq(eq);
    field.set_ov(ov);

    sys.cpu.user.cr.set_fields_at(idx, field);
}

fn write_compare_u(sys: &mut System, crfd: usize, a: u32, b: u32) {
    write_cr_field(sys, crfd, a < b, a > b, a == b, sys.cpu.user.xer.overflow_fuse());
}

fn write_compare_s(sys: &mut System, crfd: usize, a: i32, b: i32) {
    write_cr_field(sys, crfd, a < b, a > b, a == b, sys.cpu.user.xer.overflow_fuse());
}

pub fn cmpli(ins: Ins, sys: &mut System) {
    let crfd = ins.field_crfd() as usize;
    let a = sys.cpu.user.gpr[ins.field_ra() as usize];
    let b = ins.field_uimm() as u32;
    write_compare_u(sys, crfd, a, b);
}

pub fn cmpl(ins: Ins, sys: &mut System) {
    let crfd = ins.field_crfd() as usize;
    let a = sys.cpu.user.gpr[ins.field_ra() as usize];
    let b = sys.cpu.user.gpr[ins.field_rb() as usize];
    write_compare_u(sys, crfd, a, b);
}

pub fn cmpi(ins: Ins, sys: &mut System) {
    let crfd = ins.field_crfd() as usize;
    let a = sys.cpu.user.gpr[ins.field_ra() as usize] as i32;
    let b = ins.field_simm() as i16 as i32;
    write_compare_s(sys, crfd, a, b);
}

pub fn cmp(ins: Ins, sys: &mut System) {
    let crfd = ins.field_crfd() as usize;
    let l = ins.field_l() != 0;

    let ra = sys.cpu.user.gpr[ins.field_ra() as usize];
    let rb = sys.cpu.user.gpr[ins.field_rb() as usize];

    if l {
        write_compare_s(sys, crfd, ra as i64 as i32, rb as i64 as i32);
    } else {
        write_compare_s(sys, crfd, ra as i32, rb as i32);
    }
}

pub fn extsh(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let ra = ins.field_ra() as usize;

    let val = sys.cpu.user.gpr[rs] as i16;
    sys.cpu.user.gpr[ra] = val as i32 as u32;

    if ins.field_rc() {
        update_cr0(val as u32, sys);
    }
}

pub fn extsb(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    let rs = ins.field_rs() as usize;

    let result = (sys.cpu.user.gpr[rs] as i8) as i32 as u32;
    sys.cpu.user.gpr[rd] = result;

    if ins.field_rc() {
        update_cr0(result, sys);
    }
}