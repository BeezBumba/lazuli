use gekko::disasm::Ins;
use gekko::{Address, Cpu, Cycles, Exception, InsExt};
use lazuli::system::System;
use gekko::MachineState;



pub fn mfspr(ins: Ins, sys: &mut System) {
    let spr = ins.field_spr();
    let rd  = ins.field_rd() as usize;

    sys.cpu.user.gpr[rd] = match spr {
        1 => sys.cpu.user.xer.to_u32(),
        8 => sys.cpu.user.lr,
        9 => sys.cpu.user.ctr,
        26 => sys.cpu.supervisor.exception.srr[0],
        27 => sys.cpu.supervisor.exception.srr[1],
        528 => sys.cpu.supervisor.memory.ibat[0].upper_u32(),
        529 => sys.cpu.supervisor.memory.ibat[0].lower_u32(),
        530 => sys.cpu.supervisor.memory.ibat[1].upper_u32(),
        531 => sys.cpu.supervisor.memory.ibat[1].lower_u32(),
        532 => sys.cpu.supervisor.memory.ibat[2].upper_u32(),
        533 => sys.cpu.supervisor.memory.ibat[2].lower_u32(),
        534 => sys.cpu.supervisor.memory.ibat[3].upper_u32(),
        535 => sys.cpu.supervisor.memory.ibat[3].lower_u32(),
        536 => sys.cpu.supervisor.memory.dbat[0].upper_u32(),
        537 => sys.cpu.supervisor.memory.dbat[0].lower_u32(),
        538 => sys.cpu.supervisor.memory.dbat[1].upper_u32(),
        539 => sys.cpu.supervisor.memory.dbat[1].lower_u32(),
        540 => sys.cpu.supervisor.memory.dbat[2].upper_u32(),
        541 => sys.cpu.supervisor.memory.dbat[2].lower_u32(),
        542 => sys.cpu.supervisor.memory.dbat[3].upper_u32(),
        543 => sys.cpu.supervisor.memory.dbat[3].lower_u32(),
        912..=919 => sys.cpu.supervisor.gq[(spr - 912) as usize].to_u32(),
        920 => sys.cpu.supervisor.config.hid[2],
        1008 => sys.cpu.supervisor.config.hid[0],
        1009 => sys.cpu.supervisor.config.hid[1],
        1017 => sys.cpu.supervisor.misc.l2cr,
        


        _ => panic!("Unknown SPR {}", spr),
    };
}

pub fn mtspr(ins: Ins, sys: &mut System) {
    let spr = ins.field_spr();
    let rs  = ins.field_rs() as usize;
    let val = sys.cpu.user.gpr[rs];

    match spr {
        1 => sys.cpu.user.xer.set_u32(val),
        8 => sys.cpu.user.lr = val,
        9 => sys.cpu.user.ctr = val,
        26 => sys.cpu.supervisor.exception.srr[0] = val,
        27 => sys.cpu.supervisor.exception.srr[1] = val,
        528 => sys.cpu.supervisor.memory.ibat[0].set_upper_u32(val),
        529 => sys.cpu.supervisor.memory.ibat[0].set_lower_u32(val),
        530 => sys.cpu.supervisor.memory.ibat[1].set_upper_u32(val),
        531 => sys.cpu.supervisor.memory.ibat[1].set_lower_u32(val),
        532 => sys.cpu.supervisor.memory.ibat[2].set_upper_u32(val),
        533 => sys.cpu.supervisor.memory.ibat[2].set_lower_u32(val),
        534 => sys.cpu.supervisor.memory.ibat[3].set_upper_u32(val),
        535 => sys.cpu.supervisor.memory.ibat[3].set_lower_u32(val),
        536 => sys.cpu.supervisor.memory.dbat[0].set_upper_u32(val),
        537 => sys.cpu.supervisor.memory.dbat[0].set_lower_u32(val),
        538 => sys.cpu.supervisor.memory.dbat[1].set_upper_u32(val),
        539 => sys.cpu.supervisor.memory.dbat[1].set_lower_u32(val),
        540 => sys.cpu.supervisor.memory.dbat[2].set_upper_u32(val),
        541 => sys.cpu.supervisor.memory.dbat[2].set_lower_u32(val),
        542 => sys.cpu.supervisor.memory.dbat[3].set_upper_u32(val),
        543 => sys.cpu.supervisor.memory.dbat[3].set_lower_u32(val),
        912..=919 => sys.cpu.supervisor.gq[(spr - 912) as usize].set_u32(val),
        920 => sys.cpu.supervisor.config.hid[2] = val,
        1008 => sys.cpu.supervisor.config.hid[0] = val,
        1009 => sys.cpu.supervisor.config.hid[1] = val,
        1017 => sys.cpu.supervisor.misc.l2cr = val,

        _ => panic!("Unknown SPR {}", spr),
    };
}

pub fn mfmsr(ins: Ins, sys: &mut System) {
    let rd = ins.field_rd() as usize;
    sys.cpu.user.gpr[rd] = sys.cpu.supervisor.config.msr.to_bits();
}

pub fn mtmsr(ins: Ins, sys: &mut System) {
    let rs = ins.field_rs() as usize;
    let value = sys.cpu.user.gpr[rs];
    sys.cpu.supervisor.config.msr = MachineState::from_bits(value);
}

pub fn sync(_ins: Ins, _sys: &mut System) {}
pub fn isync(_ins: Ins, _sys: &mut System) {}
pub fn dcbi(_ins: Ins, _sys: &mut System) {}
pub fn dcbf(_ins: Ins, _sys: &mut System) {}
pub fn icbi(_ins: Ins, _sys: &mut System) {}

