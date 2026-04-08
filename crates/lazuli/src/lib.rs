#![feature(deque_extend_front)]

pub mod primitive;
pub mod stream;

pub mod cores;
pub mod modules;

pub mod panic;
pub mod system;

pub use disks;
pub use gekko::{self, Address, Cycles};
pub use primitive::Primitive;

use std::time::Instant;

use crate::cores::Cores;
use crate::system::{Modules, System};

/// How many DSP instructions to execute per cycle.
const DSP_INST_PER_CYCLE: f64 = 1.0;
/// How many DSP cycles to execute per step.
const DSP_STEP: u32 = 64;
/// How many DSP instructions to execute per step.
const DSP_INST_PER_STEP: u32 = (DSP_STEP as f64 * DSP_INST_PER_CYCLE) as u32;

/// Wall-clock timestamps for notable emulator boot-phase milestones.
///
/// Fields are `None` until that milestone has been reached.  Logged on the
/// first phase transition that triggers each one, and can be read by callers
/// (e.g. a UI layer) to report how long each boot stage took.
#[derive(Default)]
pub struct DebugMilestones {
    /// When [`Lazuli::exec`] was first called (start of emulated time).
    pub started_at: Option<Instant>,
    /// First block executed in the ipl-hle address range (0x81300000+).
    pub ipl_hle_started: Option<Instant>,
    /// First block executed in the apploader address range (0x81200000+).
    pub apploader_running: Option<Instant>,
    /// First block executed in OS/game RAM (0x80000000–0x817FFFFF, outside
    /// the boot stubs).  Approximate equivalent of "game entry" reached.
    pub game_entry: Option<Instant>,
}

impl DebugMilestones {
    fn elapsed_ms(&self, milestone_time: Option<Instant>) -> String {
        match (self.started_at, milestone_time) {
            (Some(start), Some(ts)) => format!("{} ms", ts.duration_since(start).as_millis()),
            _ => "?".into(),
        }
    }
}

/// The Lazuli emulator.
pub struct Lazuli {
    /// System state.
    pub sys: System,
    /// Cores of the emulator.
    cores: Cores,
    /// How many DSP cycles are pending.
    dsp_pending: f64,
    /// Boot-phase milestones for diagnostic logging.
    pub milestones: DebugMilestones,
    /// PC phase label from the most recent exec iteration.
    prev_phase: &'static str,
}

impl Lazuli {
    pub fn new(cores: Cores, modules: Modules, config: system::Config) -> Self {
        Self {
            sys: System::new(modules, config),
            cores,
            dsp_pending: 0.0,
            milestones: DebugMilestones::default(),
            prev_phase: "unknown",
        }
    }

    /// Advances emulation by the specified number of CPU cycles.
    pub fn exec(&mut self, cycles: Cycles, breakpoints: &[Address]) -> cores::Executed {
        // Latch the start time on the very first exec() call so all milestone
        // elapsed values are relative to "when emulation began".
        if self.milestones.started_at.is_none() {
            self.milestones.started_at = Some(Instant::now());
        }

        let mut total_executed = cores::Executed::default();
        while total_executed.cycles < cycles {
            // how many CPU cycles can we execute?
            let remaining = cycles - total_executed.cycles;
            let until_next_dsp_step =
                Cycles((6.0 * ((DSP_STEP as f64) - self.dsp_pending)).ceil() as u64);
            let until_next_event = Cycles(self.sys.scheduler.until_next().unwrap_or(u64::MAX));
            let can_execute = until_next_dsp_step.min(until_next_event).min(remaining);

            // execute CPU
            let executed = self.cores.cpu.exec(&mut self.sys, can_execute, breakpoints);
            total_executed.instructions += executed.instructions;
            total_executed.cycles += executed.cycles;

            // execute DSP
            self.dsp_pending += executed.cycles.to_dsp_cycles();
            while self.dsp_pending >= DSP_STEP as f64 {
                self.cores.dsp.exec(&mut self.sys, DSP_INST_PER_STEP);
                self.dsp_pending -= DSP_STEP as f64;
            }

            self.sys.scheduler.advance(executed.cycles.0);
            self.sys.process_events();

            // ── Phase transition detection ──────────────────────────────────
            // Classify the current PC and log exactly once per phase boundary.
            // Milestone timestamps are latched here for the first occurrence of
            // each phase.  Uses println! so output appears directly in the
            // native terminal console.
            let pc = self.sys.cpu.pc.value();
            let phase = system::classify_pc(pc);
            if phase != self.prev_phase {
                println!(
                    "→ Phase transition: {} → {} @ 0x{:08X}",
                    self.prev_phase, phase, pc
                );

                // When leaving ipl-hle, dump key registers so the entry-point
                // and calling-convention state is always diagnosable — mirrors
                // the `[debug] ipl-hle exit` line emitted by bootstrap.js.
                if self.prev_phase == "ipl-hle" {
                    println!(
                        "[debug] ipl-hle exit: PC=0x{:08X} r3=0x{:08X} LR=0x{:08X} CTR=0x{:08X}",
                        pc,
                        self.sys.cpu.user.gpr[3],
                        self.sys.cpu.user.lr,
                        self.sys.cpu.user.ctr,
                    );
                }

                if phase == "ipl-hle" && self.milestones.ipl_hle_started.is_none() {
                    self.milestones.ipl_hle_started = Some(Instant::now());
                    let elapsed = self.milestones.elapsed_ms(self.milestones.ipl_hle_started);
                    println!("✓ Milestone: ipl-hle started ({elapsed} since boot)");
                }
                if phase == "apploader" && self.milestones.apploader_running.is_none() {
                    self.milestones.apploader_running = Some(Instant::now());
                    let elapsed = self.milestones.elapsed_ms(self.milestones.apploader_running);
                    println!("✓ Milestone: apploader running ({elapsed} since boot)");
                }
                if phase == "OS/game RAM" && self.milestones.game_entry.is_none() {
                    self.milestones.game_entry = Some(Instant::now());
                    let elapsed = self.milestones.elapsed_ms(self.milestones.game_entry);
                    println!(
                        "✓ Milestone: game entry @ 0x{:08X} — OS/game RAM first reached \
                         ({elapsed} since boot)",
                        pc
                    );
                }

                self.prev_phase = phase;
            }

            if executed.hit_breakpoint || breakpoints.contains(&self.sys.cpu.pc) {
                std::hint::cold_path();
                total_executed.hit_breakpoint = true;
                break;
            }
        }

        total_executed
    }

    pub fn step(&mut self) -> cores::Executed {
        // execute CPU
        let executed = self.cores.cpu.step(&mut self.sys);
        self.dsp_pending += executed.cycles.to_dsp_cycles();

        // execute DSP
        while self.dsp_pending >= DSP_STEP as f64 {
            self.cores.dsp.exec(&mut self.sys, DSP_INST_PER_STEP);
            self.dsp_pending -= DSP_STEP as f64;
        }

        // process events
        self.sys.scheduler.advance(executed.cycles.0);
        self.sys.process_events();

        executed
    }
}
