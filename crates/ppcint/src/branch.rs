use gekko::disasm::Ins;
use gekko::Address;
use lazuli::system::System;
use gekko::Exception;
use gekko::MachineState;

fn get_cr_bit(sys: &System, bi: u32) -> bool {
    let cond_bit = 31 - bi;
    ((sys.cpu.user.cr.to_bits() >> cond_bit) & 1) != 0
}

fn evaluate_ctr(sys: &mut System, bo: u32) -> bool {
    let ignore_ctr = (bo & 0b100) != 0;

    if ignore_ctr {
        return true;
    }

    let ctr = sys.cpu.user.ctr.wrapping_sub(1);
    sys.cpu.user.ctr = ctr;

    let ctr_nonzero = ctr != 0;
    let bo3 = (bo >> 1) & 1 != 0;

    ctr_nonzero ^ bo3
}

fn evaluate_condition(sys: &System, bo: u32, bi: u32) -> bool {
    let ignore_cond = (bo & 0b10000) != 0;

    if ignore_cond {
        return true;
    }

    let cr_bit = get_cr_bit(sys, bi);
    let branch_if_true = (bo & 0b01000) != 0;

    if branch_if_true {
        cr_bit
    } else {
        !cr_bit
    }
}

fn branch_target(pc: u32, offset: i32, absolute: bool) -> u32 {
    if absolute {
        offset as u32
    } else {
        pc.wrapping_add(offset as u32)
    }
}

fn update_lr(sys: &mut System, pc: u32, lk: bool) {
    if lk {
        sys.cpu.user.lr = pc.wrapping_add(4);
    }
}


/* Branch Instructions */


pub fn b(ins: Ins, sys: &mut System) {
    let pc = sys.cpu.pc.value();

    let li = ins.field_li() as i32;
    let target = branch_target(pc, li, ins.field_aa());

    update_lr(sys, pc, ins.field_lk());
    sys.cpu.pc = Address(target);
}

pub fn bc(ins: Ins, sys: &mut System) {
    let pc = sys.cpu.pc.value();
    let bo = ins.field_bo() as u32;
    let bi = ins.field_bi() as u32;

    let ctr_ok = evaluate_ctr(sys, bo);
    let cond_ok = evaluate_condition(sys, bo, bi);

    let offset = ins.field_bd() as i32;
    let target = branch_target(pc, offset, ins.field_aa());
    let taken = ctr_ok && cond_ok;

    update_lr(sys, pc, ins.field_lk());

    sys.cpu.pc = Address(if taken { target } else { pc.wrapping_add(4) });
}

pub fn bclr(ins: Ins, sys: &mut System) {
    let pc = sys.cpu.pc.value();
    let bo = ins.field_bo() as u32;
    let bi = ins.field_bi() as u32;

    let ctr_ok = evaluate_ctr(sys, bo);
    let cond_ok = evaluate_condition(sys, bo, bi);

    let target = sys.cpu.user.lr & 0xFFFF_FFFC;
    let taken = ctr_ok && cond_ok;

    update_lr(sys, pc, ins.field_lk());

    sys.cpu.pc = Address(if taken { target } else { pc.wrapping_add(4) });
}

pub fn bcctr(ins: Ins, sys: &mut System) {
    let pc = sys.cpu.pc.value();
    let bo = ins.field_bo() as u32;
    let bi = ins.field_bi() as u32;

    let ctr_ok = evaluate_ctr(sys, bo);
    let cond_ok = evaluate_condition(sys, bo, bi);

    let target = sys.cpu.user.ctr & 0xFFFF_FFFC;
    let taken = ctr_ok && cond_ok;

    update_lr(sys, pc, ins.field_lk());

    sys.cpu.pc = Address(if taken { target } else { pc.wrapping_add(4) });
}

pub fn write_msr(msr: &mut MachineState, val: u32) {
    let current = msr.to_bits();
    let mut new_msr = (current & !Exception::SRR1_TO_MSR_MASK) | (val & Exception::SRR1_TO_MSR_MASK);
    new_msr &= !(1 << 18);
    *msr = MachineState::from_bits(new_msr);
}

pub fn rfi(_ins: Ins, sys: &mut System) {
    let srr0 = sys.cpu.supervisor.exception.srr[0];
    let srr1 = sys.cpu.supervisor.exception.srr[1];
    write_msr(&mut sys.cpu.supervisor.config.msr, srr1);
    sys.cpu.pc = Address(srr0 & 0xFFFF_FFFC);
}

pub fn sc(_ins: Ins, _sys: &mut System) {}