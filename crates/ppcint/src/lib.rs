use gekko::{Address, Cpu, Cycles, InsExt};
use gekko::disasm::{Ins, Opcode};
use lazuli::cores::{CpuCore, Executed};
use lazuli::system::System;
mod arithmetic;
mod branch;
mod compare;
mod floating;
mod logic;
mod memory;
mod others;
mod utils;


pub struct Core {
    pub instr_count: u64,
    total_cycles: u64,
}


impl Core {
    pub fn new() -> Self {
        Self {
            instr_count: 0,
            total_cycles: 0,
        }
    }
}


impl CpuCore for Core {
    fn exec(&mut self, sys: &mut System, cycles: Cycles, breakpoints: &[Address]) -> Executed {
        let mut executed = Executed::default();
        while executed.cycles < cycles {
            if breakpoints.contains(&sys.cpu.pc) {
                executed.hit_breakpoint = true;
                break;
            }

            let exec = self.step(sys);
            executed.instructions += exec.instructions;
            executed.cycles += exec.cycles;

            if exec.instructions == 0 {
                break;
            }
        }
        executed
    }

    fn step(&mut self, sys: &mut System) -> Executed {
        let old_pc = sys.cpu.pc;
        self.instr_count += 1;
        let Some(phys_pc) = sys.translate_inst_addr(old_pc) else {
            panic!("Failed to translate instruction address at {old_pc}");
        };

        let Some(raw_ins) = sys.read_phys_pure::<u32>(phys_pc) else {
            panic!("Failed to read instruction at {old_pc}");
        };

 


        
        let ins = Ins::new(raw_ins, Default::default());

        match ins.op {
            Opcode::Add      => arithmetic::add(ins, sys),
            Opcode::Addc     => arithmetic::addc(ins, sys),
            Opcode::Adde     => arithmetic::adde(ins, sys),
            Opcode::Addi     => arithmetic::addi(ins, sys),
            Opcode::Addic    => arithmetic::addic(ins, sys),
            Opcode::Addic_   => arithmetic::addic_(ins, sys),
            Opcode::Addis    => arithmetic::addis(ins, sys),
            Opcode::Addze    => arithmetic::addze(ins, sys),
            Opcode::Subf     => arithmetic::subf(ins, sys),
            Opcode::Subfc    => arithmetic::subfc(ins, sys),
            Opcode::Subfic   => arithmetic::subfic(ins, sys),
            Opcode::Subfe    => arithmetic::subfe(ins, sys),
            Opcode::Neg      => arithmetic::neg(ins, sys),
            Opcode::Mulli    => arithmetic::mulli(ins, sys),
            Opcode::Mullw    => arithmetic::mullw(ins, sys),
            Opcode::Mulhw    => arithmetic::mulhw(ins, sys),
            Opcode::Mulhwu   => arithmetic::mulhwu(ins, sys),
            Opcode::Divw     => arithmetic::divw(ins, sys),
            Opcode::Divwu    => arithmetic::divwu(ins, sys),  
            Opcode::B        => branch::b(ins, sys),
            Opcode::Bc       => branch::bc(ins, sys),
            Opcode::Bcctr    => branch::bcctr(ins, sys),
            Opcode::Bclr     => branch::bclr(ins, sys),
            Opcode::Sc       => branch::sc(ins, sys),
            Opcode::Rfi      => branch::rfi(ins, sys),
            Opcode::Cmp      => compare::cmp(ins, sys),
            Opcode::Cmpi     => compare::cmpi(ins, sys),
            Opcode::Cmpl     => compare::cmpl(ins, sys),
            Opcode::Cmpli    => compare::cmpli(ins, sys),
            Opcode::Extsb    => compare::extsb(ins, sys),
            Opcode::Extsh    => compare::extsh(ins, sys),
            Opcode::And      => logic::and(ins, sys),
            Opcode::Andc     => logic::andc(ins, sys),
            Opcode::Andi_    => logic::andi_(ins, sys),
            Opcode::Or       => logic::or(ins, sys),
            Opcode::Ori      => logic::ori(ins, sys),
            Opcode::Oris     => logic::oris(ins, sys),
            Opcode::Xor      => logic::xor(ins, sys),
            Opcode::Xoris    => logic::xoris(ins, sys),
            Opcode::Nor      => logic::nor(ins, sys),
            Opcode::Cntlzw   => logic::cntlzw(ins, sys),
            Opcode::Crxor    => logic::crxor(ins, sys),
            Opcode::Mtfsb1   => logic::mtfsb1(ins, sys),
            Opcode::Rlwinm   => logic::rlwinm(ins, sys),
            Opcode::Rlwimi   => logic::rlwimi(ins, sys),
            Opcode::Slw      => logic::slw(ins, sys),
            Opcode::Sraw     => logic::sraw(ins, sys),
            Opcode::Lbz      => memory::lbz(ins, sys),
            Opcode::Lbzu     => memory::lbzu(ins, sys),
            Opcode::Lhz      => memory::lhz(ins, sys),
            Opcode::Lhzu     => memory::lhzu(ins, sys),
            Opcode::Lhzx     => memory::lhzx(ins, sys),
            Opcode::Lwz      => memory::lwz(ins, sys),
            Opcode::Lwzu     => memory::lwzu(ins, sys),
            Opcode::Lwzx     => memory::lwzx(ins, sys),
            Opcode::Lmw      => memory::lmw(ins, sys),
            Opcode::Stb      => memory::stb(ins, sys),
            Opcode::Stbu     => memory::stbu(ins, sys),
            Opcode::Stbx     => memory::stbx(ins, sys),
            Opcode::Sth      => memory::sth(ins, sys),
            Opcode::Stw      => memory::stw(ins, sys),
            Opcode::Stwu     => memory::stwu(ins, sys),
            Opcode::Stwx     => memory::stwx(ins, sys),
            Opcode::Stmw     => memory::stmw(ins, sys),
            Opcode::Stfdu    => memory::stfdu(ins, sys),
            Opcode::Mftb     => memory::mftb(ins, sys),
            Opcode::Mfmsr    => others::mfmsr(ins, sys),
            Opcode::Mtmsr    => others::mtmsr(ins, sys),
            Opcode::Mfspr    => others::mfspr(ins, sys),
            Opcode::Mtspr    => others::mtspr(ins, sys),
            Opcode::Sync     => others::sync(ins, sys),
            Opcode::Isync    => others::isync(ins, sys),
            Opcode::Icbi     => others::icbi(ins, sys),
            Opcode::Dcbi     => others::dcbi(ins, sys),
            Opcode::Dcbf     => others::dcbf(ins, sys),

            
            _ => panic!("Unimplemented instruction: {:?} at {}", ins.op, old_pc),
        }

        if sys.cpu.pc == old_pc {
            sys.cpu.pc = Address(old_pc.value().wrapping_add(4));
        }
        
        Executed {
            instructions: 1,
            cycles: Cycles(2),
            hit_breakpoint: false,
        }
    }
}
