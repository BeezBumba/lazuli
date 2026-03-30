//! PowerPC → WebAssembly instruction translator.
//!
//! ## Design overview
//!
//! The translation strategy mirrors how the Play! PS2 emulator's Nuanceur IR
//! is compiled to WebAssembly: each guest instruction is lowered directly to
//! typed WASM stack-machine instructions.  Guest register state lives in WASM
//! linear memory at the byte offsets computed by [`crate::offsets::RegOffsets`],
//! so load/store instructions use immediate-offset memory accesses with no
//! runtime address arithmetic overhead.
//!
//! ### Generated WASM module layout
//!
//! ```text
//! types:
//!   0: (i32) -> i32      ; read hooks + execute
//!   1: (i32, i32) -> ()  ; write hooks
//!   2: (i32) -> ()       ; raise_exception / generic
//!
//! imports (module "hooks"):
//!   0  read_u8            type 0
//!   1  read_u16           type 0
//!   2  read_u32           type 0
//!   3  write_u8           type 1
//!   4  write_u16          type 1
//!   5  write_u32          type 1
//!   6  raise_exception    type 2
//!
//! functions:
//!   7  execute            type 0   ; (regs_ptr: i32) -> (next_pc: i32)
//!
//! exports:
//!   "execute" -> func 7
//! ```
//!
//! ### Register access
//!
//! Every guest GPR, FPR, and special-purpose register is accessed via
//! `i32.load` / `i32.store` with `regs_ptr` (local 0) as the base and a
//! pre-computed immediate `MemArg::offset`.  This is identical in spirit to
//! how native JIT backends (like `ppcjit` with Cranelift) lay out the
//! [`gekko::Cpu`] struct in the host address space and access fields through
//! constant offsets.

use gekko::disasm::{Ins, Opcode};
use gekko::InsExt;
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemArg, Module, TypeSection, ValType,
};

use crate::block::{WasmBlock, imports};
use crate::offsets::RegOffsets;

// ─── Type section indices ─────────────────────────────────────────────────────
/// `(i32) -> i32`  — read hooks + execute
const TY_I32_RET_I32: u32 = 0;
/// `(i32, i32) -> ()`  — write hooks
const TY_I32_I32_RET_VOID: u32 = 1;
/// `(i32) -> ()`  — raise_exception
const TY_I32_RET_VOID: u32 = 2;
/// Alias: the `execute` function uses the same signature as the read hooks.
const TY_EXECUTE: u32 = TY_I32_RET_I32;

// ─── WASM local indices ────────────────────────────────────────────────────────
/// Local 0: `regs_ptr` (i32) — the function parameter.
const LOCAL_REGS_PTR: u32 = 0;
/// Local 1: `scratch` (i32) — a general-purpose scratch register used for
/// intermediate results (e.g., CR update, with-update effective address).
const LOCAL_SCRATCH: u32 = 1;

// ─── Block builder ────────────────────────────────────────────────────────────

/// Translates a sequence of PowerPC instructions into a self-contained WASM
/// module containing a single exported `execute` function.
pub(crate) struct BlockBuilder<'a> {
    offsets: &'a RegOffsets,
    /// Accumulated WASM instructions for the function body.
    body: Vec<Instruction<'static>>,
    /// Number of successfully lowered PPC instructions.
    ins_count: u32,
    /// Estimated cycle count.
    cycles: u32,
    /// Whether the last instruction was a terminal (branch/return).
    has_terminal: bool,
}

impl<'a> BlockBuilder<'a> {
    pub fn new(offsets: &'a RegOffsets) -> Self {
        Self {
            offsets,
            body: Vec::new(),
            ins_count: 0,
            cycles: 0,
            has_terminal: false,
        }
    }

    // ── MemArg helpers ────────────────────────────────────────────────────────

    fn m32(&self, offset: u64) -> MemArg {
        MemArg { offset, align: 2, memory_index: 0 }
    }
    #[allow(dead_code)]
    fn m16(&self, offset: u64) -> MemArg {
        MemArg { offset, align: 1, memory_index: 0 }
    }
    #[allow(dead_code)]
    fn m8(&self, offset: u64) -> MemArg {
        MemArg { offset, align: 0, memory_index: 0 }
    }

    // ── Stack push helpers ────────────────────────────────────────────────────

    /// Push `regs_ptr` (local 0) onto the WASM operand stack.
    fn push_base(&mut self) {
        self.body.push(Instruction::LocalGet(LOCAL_REGS_PTR));
    }

    /// Push GPR[r] (loaded from linear memory).
    fn push_gpr(&mut self, r: u8) {
        self.push_base();
        self.body.push(Instruction::I32Load(self.m32(self.offsets.gpr[r as usize])));
    }

    /// Push LR.
    fn push_lr(&mut self) {
        self.push_base();
        self.body.push(Instruction::I32Load(self.m32(self.offsets.lr)));
    }

    /// Push CTR.
    fn push_ctr(&mut self) {
        self.push_base();
        self.body.push(Instruction::I32Load(self.m32(self.offsets.ctr)));
    }

    /// Push CR.
    fn push_cr(&mut self) {
        self.push_base();
        self.body.push(Instruction::I32Load(self.m32(self.offsets.cr)));
    }

    // ── Store helpers ─────────────────────────────────────────────────────────
    //
    // Pattern: the caller first `push_base()`, then computes the value,
    // then calls one of these `store_*` methods which emit the `i32.store`.
    // The WASM stack at the point of the store instruction must be:
    //   [..., address (regs_ptr), value]

    fn store_gpr(&mut self, r: u8) {
        self.body.push(Instruction::I32Store(self.m32(self.offsets.gpr[r as usize])));
    }

    fn store_lr(&mut self) {
        self.body.push(Instruction::I32Store(self.m32(self.offsets.lr)));
    }

    fn store_ctr(&mut self) {
        self.body.push(Instruction::I32Store(self.m32(self.offsets.ctr)));
    }

    fn store_pc(&mut self) {
        self.body.push(Instruction::I32Store(self.m32(self.offsets.pc)));
    }

    fn store_cr(&mut self) {
        self.body.push(Instruction::I32Store(self.m32(self.offsets.cr)));
    }

    // ── Effective-address helpers ─────────────────────────────────────────────

    /// Push `GPR[rA] + sign_extend(d)` onto the stack.
    ///
    /// If `rA == 0`, the PowerPC convention treats the base as zero (not the
    /// register value), so only `d` is pushed.
    fn push_ea_d(&mut self, ra: u8, d: i32) {
        if ra == 0 {
            self.body.push(Instruction::I32Const(d));
        } else {
            self.push_gpr(ra);
            if d != 0 {
                self.body.push(Instruction::I32Const(d));
                self.body.push(Instruction::I32Add);
            }
        }
    }

    /// Push `GPR[rA] + GPR[rB]` (indexed addressing).
    ///
    /// If `rA == 0`, only `GPR[rB]` is used as the address.
    fn push_ea_x(&mut self, ra: u8, rb: u8) {
        if ra == 0 {
            self.push_gpr(rb);
        } else {
            self.push_gpr(ra);
            self.push_gpr(rb);
            self.body.push(Instruction::I32Add);
        }
    }

    // ── CR update helper ──────────────────────────────────────────────────────

    /// Emit code that updates a single CR field based on a signed comparison
    /// of the value in `val_local` with zero.
    ///
    /// PowerPC CR layout (MSB = bit 31):
    /// - Field 0 → bits 31..28 (LT=31, GT=30, EQ=29, SO=28)
    /// - Field n → bits (31-4n)..(28-4n)
    ///
    /// Stack effect: none (reads and writes the CR field in linear memory).
    fn emit_update_cr_field(&mut self, cr_fd: u8, val_local: u32) {
        let lt_bit = 31u32.wrapping_sub((cr_fd as u32) * 4);
        let gt_bit = lt_bit.wrapping_sub(1);
        let eq_bit = lt_bit.wrapping_sub(2);
        let so_bit = lt_bit.wrapping_sub(3);
        let field_mask: u32 = 0xF_u32.wrapping_shl(so_bit);

        // CR = (CR & ~field_mask) | LT | GT | EQ | SO
        self.push_base();

        // (CR & ~field_mask)
        self.push_cr();
        self.body.push(Instruction::I32Const((!field_mask) as i32));
        self.body.push(Instruction::I32And);

        // LT bit: (val < 0) << lt_bit
        self.body.push(Instruction::LocalGet(val_local));
        self.body.push(Instruction::I32Const(0));
        self.body.push(Instruction::I32LtS);
        self.body.push(Instruction::I32Const(lt_bit as i32));
        self.body.push(Instruction::I32Shl);
        self.body.push(Instruction::I32Or);

        // GT bit: (val > 0) << gt_bit
        self.body.push(Instruction::LocalGet(val_local));
        self.body.push(Instruction::I32Const(0));
        self.body.push(Instruction::I32GtS);
        self.body.push(Instruction::I32Const(gt_bit as i32));
        self.body.push(Instruction::I32Shl);
        self.body.push(Instruction::I32Or);

        // EQ bit: (val == 0) << eq_bit
        self.body.push(Instruction::LocalGet(val_local));
        self.body.push(Instruction::I32Eqz);
        self.body.push(Instruction::I32Const(eq_bit as i32));
        self.body.push(Instruction::I32Shl);
        self.body.push(Instruction::I32Or);

        // SO bit: copy from XER bit 31
        self.push_base();
        self.body.push(Instruction::I32Load(self.m32(self.offsets.xer)));
        self.body.push(Instruction::I32Const(31));
        self.body.push(Instruction::I32ShrU);
        self.body.push(Instruction::I32Const(so_bit as i32));
        self.body.push(Instruction::I32Shl);
        self.body.push(Instruction::I32Or);

        self.store_cr();
    }

    /// Emit an unsigned CR field update (for `cmpl`/`cmpli`).
    fn emit_update_cr_field_unsigned(&mut self, cr_fd: u8, lhs_local: u32, rhs_local: u32) {
        let lt_bit = 31u32.wrapping_sub((cr_fd as u32) * 4);
        let gt_bit = lt_bit.wrapping_sub(1);
        let eq_bit = lt_bit.wrapping_sub(2);
        let so_bit = lt_bit.wrapping_sub(3);
        let field_mask: u32 = 0xF_u32.wrapping_shl(so_bit);

        self.push_base();

        // (CR & ~field_mask)
        self.push_cr();
        self.body.push(Instruction::I32Const((!field_mask) as i32));
        self.body.push(Instruction::I32And);

        // LT: lhs <u rhs
        self.body.push(Instruction::LocalGet(lhs_local));
        self.body.push(Instruction::LocalGet(rhs_local));
        self.body.push(Instruction::I32LtU);
        self.body.push(Instruction::I32Const(lt_bit as i32));
        self.body.push(Instruction::I32Shl);
        self.body.push(Instruction::I32Or);

        // GT: lhs >u rhs
        self.body.push(Instruction::LocalGet(lhs_local));
        self.body.push(Instruction::LocalGet(rhs_local));
        self.body.push(Instruction::I32GtU);
        self.body.push(Instruction::I32Const(gt_bit as i32));
        self.body.push(Instruction::I32Shl);
        self.body.push(Instruction::I32Or);

        // EQ: lhs == rhs
        self.body.push(Instruction::LocalGet(lhs_local));
        self.body.push(Instruction::LocalGet(rhs_local));
        self.body.push(Instruction::I32Eq);
        self.body.push(Instruction::I32Const(eq_bit as i32));
        self.body.push(Instruction::I32Shl);
        self.body.push(Instruction::I32Or);

        // SO from XER bit 31
        self.push_base();
        self.body.push(Instruction::I32Load(self.m32(self.offsets.xer)));
        self.body.push(Instruction::I32Const(31));
        self.body.push(Instruction::I32ShrU);
        self.body.push(Instruction::I32Const(so_bit as i32));
        self.body.push(Instruction::I32Shl);
        self.body.push(Instruction::I32Or);

        self.store_cr();
    }

    // ── Branch condition helper ───────────────────────────────────────────────

    /// Emit a conditional test for a BC instruction and return a boolean (i32
    /// 0 or 1) on the stack representing whether the branch should be taken.
    ///
    /// `bo` is the 5-bit BO field; `bi` is the 5-bit BI field (CR bit index
    /// measured from bit 31, i.e., `bi == 0` → CR bit 31 = CR0.LT).
    fn emit_branch_condition(&mut self, bo: u8, bi: u8) {
        // BO encoding (PPC Architecture):
        //   bit 4 (MSB): ignore_ctr
        //   bit 3: ctr_cond (0=CTR≠0, 1=CTR==0)
        //   bit 2: ignore_cr
        //   bit 1: desired_cr (0=false, 1=true)
        //   bit 0: branch prediction hint (ignored here)
        let ignore_ctr = (bo >> 4) & 1 != 0;
        let ctr_zero = (bo >> 3) & 1 != 0;
        let ignore_cr = (bo >> 2) & 1 != 0;
        let desired_cr = (bo >> 1) & 1 != 0;

        // ctr_ok: CTR condition satisfied
        // cr_ok:  CR condition satisfied
        // taken:  ctr_ok AND cr_ok

        // We always emit a value on the stack.  Start with `1` (taken), then
        // AND in the CTR and CR conditions as needed.
        self.body.push(Instruction::I32Const(1));

        if !ignore_ctr {
            // CTR -= 1
            self.push_base();
            self.push_ctr();
            self.body.push(Instruction::I32Const(1));
            self.body.push(Instruction::I32Sub);
            self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
            self.store_ctr();

            // ctr_ok = (CTR == 0) == ctr_zero
            self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
            self.body.push(Instruction::I32Eqz); // CTR == 0
            if !ctr_zero {
                // want CTR ≠ 0 → invert
                self.body.push(Instruction::I32Const(1));
                self.body.push(Instruction::I32Xor);
            }

            // AND into accumulated condition
            self.body.push(Instruction::I32And);
        }

        if !ignore_cr {
            // cr_ok = ((CR >> bi) & 1) == desired_cr
            self.push_cr();
            self.body.push(Instruction::I32Const(bi as i32));
            self.body.push(Instruction::I32ShrU);
            self.body.push(Instruction::I32Const(1));
            self.body.push(Instruction::I32And);

            if !desired_cr {
                // want CR bit == 0 → invert
                self.body.push(Instruction::I32Const(1));
                self.body.push(Instruction::I32Xor);
            }

            // AND into accumulated condition
            self.body.push(Instruction::I32And);
        }
        // Stack top: 1 if branch taken, 0 if not taken.
    }

    // ── Instruction emission ─────────────────────────────────────────────────

    /// Translate one PowerPC instruction.  Returns `true` when the instruction
    /// is a terminal (block ends; no fall-through PC is appended).
    pub fn emit(&mut self, ins: Ins, ins_pc: u32) -> bool {
        self.ins_count += 1;
        self.cycles += 1;

        match ins.op {
            // ── Integer arithmetic ────────────────────────────────────────────

            // addi  rD, rA, SIMM   (li rD, SIMM when rA==0)
            Opcode::Addi => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let simm = ins.field_simm() as i32;
                self.push_base();
                self.push_ea_d(ra, simm);
                self.store_gpr(rd);
            }

            // addis  rD, rA, SIMM  (lis rD, SIMM when rA==0)
            Opcode::Addis => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let simm = (ins.field_uimm() as i32) << 16;
                self.push_base();
                if ra == 0 {
                    self.body.push(Instruction::I32Const(simm));
                } else {
                    self.push_gpr(ra);
                    self.body.push(Instruction::I32Const(simm));
                    self.body.push(Instruction::I32Add);
                }
                self.store_gpr(rd);
            }

            // add  rD, rA, rB
            Opcode::Add => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(ra);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Add);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(rd);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // addic  rD, rA, SIMM
            Opcode::Addic => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let simm = ins.field_simm() as i32;
                self.push_base();
                self.push_gpr(ra);
                self.body.push(Instruction::I32Const(simm));
                self.body.push(Instruction::I32Add);
                self.store_gpr(rd);
                // TODO: update XER.CA (carry)
            }

            // addic.  rD, rA, SIMM  (updates CR0)
            Opcode::Addic_ => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let simm = ins.field_simm() as i32;
                self.push_base();
                self.push_gpr(ra);
                self.body.push(Instruction::I32Const(simm));
                self.body.push(Instruction::I32Add);
                self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                self.store_gpr(rd);
                self.emit_update_cr_field(0, LOCAL_SCRATCH);
            }

            // subf  rD, rA, rB   (rD = rB - rA)
            Opcode::Subf => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rb);
                self.push_gpr(ra);
                self.body.push(Instruction::I32Sub);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(rd);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // subfic  rD, rA, SIMM   (rD = SIMM - rA)
            Opcode::Subfic => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let simm = ins.field_simm() as i32;
                self.push_base();
                self.body.push(Instruction::I32Const(simm));
                self.push_gpr(ra);
                self.body.push(Instruction::I32Sub);
                self.store_gpr(rd);
            }

            // neg  rD, rA
            Opcode::Neg => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.body.push(Instruction::I32Const(0));
                self.push_gpr(ra);
                self.body.push(Instruction::I32Sub);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(rd);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // mulli  rD, rA, SIMM
            Opcode::Mulli => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let simm = ins.field_simm() as i32;
                self.push_base();
                self.push_gpr(ra);
                self.body.push(Instruction::I32Const(simm));
                self.body.push(Instruction::I32Mul);
                self.store_gpr(rd);
            }

            // mullw  rD, rA, rB
            Opcode::Mullw => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(ra);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Mul);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(rd);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // divw  rD, rA, rB  (signed)
            Opcode::Divw => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_base();
                self.push_gpr(ra);
                self.push_gpr(rb);
                self.body.push(Instruction::I32DivS);
                self.store_gpr(rd);
            }

            // divwu  rD, rA, rB  (unsigned)
            Opcode::Divwu => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_base();
                self.push_gpr(ra);
                self.push_gpr(rb);
                self.body.push(Instruction::I32DivU);
                self.store_gpr(rd);
            }

            // ── Integer logic ─────────────────────────────────────────────────

            // and  rA, rS, rB
            Opcode::And => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32And);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // andi.  rA, rS, UIMM  (always updates CR0)
            Opcode::Andi_ => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let uimm = ins.field_uimm() as i32;
                self.push_base();
                self.push_gpr(rs);
                self.body.push(Instruction::I32Const(uimm));
                self.body.push(Instruction::I32And);
                self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                self.store_gpr(ra);
                self.emit_update_cr_field(0, LOCAL_SCRATCH);
            }

            // andis.  rA, rS, UIMM  (always updates CR0)
            Opcode::Andis_ => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let uimm = (ins.field_uimm() as i32) << 16;
                self.push_base();
                self.push_gpr(rs);
                self.body.push(Instruction::I32Const(uimm));
                self.body.push(Instruction::I32And);
                self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                self.store_gpr(ra);
                self.emit_update_cr_field(0, LOCAL_SCRATCH);
            }

            // or  rA, rS, rB   (mr  rA, rS  when rS == rB)
            Opcode::Or => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                if rs != rb {
                    self.push_gpr(rb);
                    self.body.push(Instruction::I32Or);
                }
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // ori  rA, rS, UIMM  (nop when all zero)
            Opcode::Ori => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let uimm = ins.field_uimm() as i32;
                self.push_base();
                self.push_gpr(rs);
                if uimm != 0 {
                    self.body.push(Instruction::I32Const(uimm));
                    self.body.push(Instruction::I32Or);
                }
                self.store_gpr(ra);
            }

            // oris  rA, rS, UIMM
            Opcode::Oris => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let uimm = (ins.field_uimm() as i32) << 16;
                self.push_base();
                self.push_gpr(rs);
                if uimm != 0 {
                    self.body.push(Instruction::I32Const(uimm));
                    self.body.push(Instruction::I32Or);
                }
                self.store_gpr(ra);
            }

            // xor  rA, rS, rB
            Opcode::Xor => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Xor);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // xori  rA, rS, UIMM
            Opcode::Xori => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let uimm = ins.field_uimm() as i32;
                self.push_base();
                self.push_gpr(rs);
                if uimm != 0 {
                    self.body.push(Instruction::I32Const(uimm));
                    self.body.push(Instruction::I32Xor);
                }
                self.store_gpr(ra);
            }

            // xoris  rA, rS, UIMM
            Opcode::Xoris => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let uimm = (ins.field_uimm() as i32) << 16;
                self.push_base();
                self.push_gpr(rs);
                if uimm != 0 {
                    self.body.push(Instruction::I32Const(uimm));
                    self.body.push(Instruction::I32Xor);
                }
                self.store_gpr(ra);
            }

            // nor  rA, rS, rB
            Opcode::Nor => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Or);
                self.body.push(Instruction::I32Const(-1i32));
                self.body.push(Instruction::I32Xor); // bitwise NOT via XOR -1
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // andc  rA, rS, rB   (rA = rS & ~rB)
            Opcode::Andc => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Const(-1i32));
                self.body.push(Instruction::I32Xor); // ~rB
                self.body.push(Instruction::I32And);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // orc  rA, rS, rB   (rA = rS | ~rB)
            Opcode::Orc => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Const(-1i32));
                self.body.push(Instruction::I32Xor); // ~rB
                self.body.push(Instruction::I32Or);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // eqv  rA, rS, rB   (rA = ~(rS ^ rB))
            Opcode::Eqv => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Xor);
                self.body.push(Instruction::I32Const(-1i32));
                self.body.push(Instruction::I32Xor); // ~(rS ^ rB)
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // ── Rotate / shift ────────────────────────────────────────────────

            // rlwinm  rA, rS, SH, MB, ME
            Opcode::Rlwinm => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let sh = ins.field_sh() as i32;
                let mb = ins.field_mb() as u32;
                let me = ins.field_me() as u32;
                let rc = ins.field_rc();
                let mask = ppc_mask(mb, me) as i32;
                self.push_base();
                self.push_gpr(rs);
                if sh != 0 {
                    self.body.push(Instruction::I32Const(sh));
                    self.body.push(Instruction::I32Rotl);
                }
                if mask != -1i32 {
                    self.body.push(Instruction::I32Const(mask));
                    self.body.push(Instruction::I32And);
                }
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // rlwimi  rA, rS, SH, MB, ME  (rotate + mask insert)
            Opcode::Rlwimi => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let sh = ins.field_sh() as i32;
                let mb = ins.field_mb() as u32;
                let me = ins.field_me() as u32;
                let rc = ins.field_rc();
                let mask = ppc_mask(mb, me) as i32;
                let not_mask = !mask;
                // rA = (rA & ~mask) | (ROTL(rS, SH) & mask)
                self.push_base();
                self.push_gpr(ra);
                self.body.push(Instruction::I32Const(not_mask));
                self.body.push(Instruction::I32And);
                self.push_gpr(rs);
                if sh != 0 {
                    self.body.push(Instruction::I32Const(sh));
                    self.body.push(Instruction::I32Rotl);
                }
                self.body.push(Instruction::I32Const(mask));
                self.body.push(Instruction::I32And);
                self.body.push(Instruction::I32Or);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // rlwnm  rA, rS, rB, MB, ME  (rotate by register)
            Opcode::Rlwnm => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let mb = ins.field_mb() as u32;
                let me = ins.field_me() as u32;
                let rc = ins.field_rc();
                let mask = ppc_mask(mb, me) as i32;
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Rotl);
                if mask != -1i32 {
                    self.body.push(Instruction::I32Const(mask));
                    self.body.push(Instruction::I32And);
                }
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // slw  rA, rS, rB
            Opcode::Slw => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Shl);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // srw  rA, rS, rB  (logical right shift)
            Opcode::Srw => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32ShrU);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // sraw  rA, rS, rB  (arithmetic right shift)
            Opcode::Sraw => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rb = ins.gpr_b() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.push_gpr(rb);
                self.body.push(Instruction::I32ShrS);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // srawi  rA, rS, SH
            Opcode::Srawi => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let sh = ins.field_sh() as i32;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.body.push(Instruction::I32Const(sh));
                self.body.push(Instruction::I32ShrS);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // extsh  rA, rS
            Opcode::Extsh => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.body.push(Instruction::I32Const(16));
                self.body.push(Instruction::I32Shl);
                self.body.push(Instruction::I32Const(16));
                self.body.push(Instruction::I32ShrS);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // extsb  rA, rS
            Opcode::Extsb => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.body.push(Instruction::I32Const(24));
                self.body.push(Instruction::I32Shl);
                self.body.push(Instruction::I32Const(24));
                self.body.push(Instruction::I32ShrS);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // cntlzw  rA, rS
            Opcode::Cntlzw => {
                let ra = ins.gpr_a() as u8;
                let rs = ins.gpr_s() as u8;
                let rc = ins.field_rc();
                self.push_base();
                self.push_gpr(rs);
                self.body.push(Instruction::I32Clz);
                if rc {
                    self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                }
                self.store_gpr(ra);
                if rc {
                    self.emit_update_cr_field(0, LOCAL_SCRATCH);
                }
            }

            // ── Memory loads ──────────────────────────────────────────────────

            // lwz  rD, d(rA)
            Opcode::Lwz => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                self.push_base();
                self.push_ea_d(ra, d);
                self.body.push(Instruction::Call(imports::READ_U32));
                self.store_gpr(rd);
            }

            // lwzu  rD, d(rA)  (with base update)
            Opcode::Lwzu => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                // ea = rA + d
                self.push_gpr(ra);
                self.body.push(Instruction::I32Const(d));
                self.body.push(Instruction::I32Add);
                self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                // rD = mem[ea]
                self.push_base();
                self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
                self.body.push(Instruction::Call(imports::READ_U32));
                self.store_gpr(rd);
                // rA = ea
                self.push_base();
                self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
                self.store_gpr(ra);
            }

            // lhz  rD, d(rA)  (zero-extend)
            Opcode::Lhz => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                self.push_base();
                self.push_ea_d(ra, d);
                self.body.push(Instruction::Call(imports::READ_U16));
                self.store_gpr(rd);
            }

            // lha  rD, d(rA)  (sign-extend)
            Opcode::Lha => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                self.push_base();
                self.push_ea_d(ra, d);
                self.body.push(Instruction::Call(imports::READ_U16));
                // sign-extend 16 → 32
                self.body.push(Instruction::I32Const(16));
                self.body.push(Instruction::I32Shl);
                self.body.push(Instruction::I32Const(16));
                self.body.push(Instruction::I32ShrS);
                self.store_gpr(rd);
            }

            // lbz  rD, d(rA)
            Opcode::Lbz => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                self.push_base();
                self.push_ea_d(ra, d);
                self.body.push(Instruction::Call(imports::READ_U8));
                self.store_gpr(rd);
            }

            // lwzx  rD, rA, rB
            Opcode::Lwzx => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_base();
                self.push_ea_x(ra, rb);
                self.body.push(Instruction::Call(imports::READ_U32));
                self.store_gpr(rd);
            }

            // lhzx  rD, rA, rB
            Opcode::Lhzx => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_base();
                self.push_ea_x(ra, rb);
                self.body.push(Instruction::Call(imports::READ_U16));
                self.store_gpr(rd);
            }

            // lbzx  rD, rA, rB
            Opcode::Lbzx => {
                let rd = ins.gpr_d() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_base();
                self.push_ea_x(ra, rb);
                self.body.push(Instruction::Call(imports::READ_U8));
                self.store_gpr(rd);
            }

            // ── Memory stores ─────────────────────────────────────────────────

            // stw  rS, d(rA)
            Opcode::Stw => {
                let rs = ins.gpr_s() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                self.push_ea_d(ra, d);
                self.push_gpr(rs);
                self.body.push(Instruction::Call(imports::WRITE_U32));
            }

            // stwu  rS, d(rA)  (with base update)
            Opcode::Stwu => {
                let rs = ins.gpr_s() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                // ea = rA + d
                self.push_gpr(ra);
                self.body.push(Instruction::I32Const(d));
                self.body.push(Instruction::I32Add);
                self.body.push(Instruction::LocalTee(LOCAL_SCRATCH));
                // mem[ea] = rS
                self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
                self.push_gpr(rs);
                self.body.push(Instruction::Call(imports::WRITE_U32));
                // rA = ea
                self.push_base();
                self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
                self.store_gpr(ra);
            }

            // sth  rS, d(rA)
            Opcode::Sth => {
                let rs = ins.gpr_s() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                self.push_ea_d(ra, d);
                self.push_gpr(rs);
                self.body.push(Instruction::Call(imports::WRITE_U16));
            }

            // stb  rS, d(rA)
            Opcode::Stb => {
                let rs = ins.gpr_s() as u8;
                let ra = ins.gpr_a() as u8;
                let d = ins.field_offset() as i32;
                self.push_ea_d(ra, d);
                self.push_gpr(rs);
                self.body.push(Instruction::Call(imports::WRITE_U8));
            }

            // stwx  rS, rA, rB
            Opcode::Stwx => {
                let rs = ins.gpr_s() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_ea_x(ra, rb);
                self.push_gpr(rs);
                self.body.push(Instruction::Call(imports::WRITE_U32));
            }

            // sthx  rS, rA, rB
            Opcode::Sthx => {
                let rs = ins.gpr_s() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_ea_x(ra, rb);
                self.push_gpr(rs);
                self.body.push(Instruction::Call(imports::WRITE_U16));
            }

            // stbx  rS, rA, rB
            Opcode::Stbx => {
                let rs = ins.gpr_s() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_ea_x(ra, rb);
                self.push_gpr(rs);
                self.body.push(Instruction::Call(imports::WRITE_U8));
            }

            // ── Compare ───────────────────────────────────────────────────────

            // cmp  crD, rA, rB  (signed)
            Opcode::Cmp => {
                let cr_fd = ins.field_crfd() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                // val = rA - rB  (for signed comparison: sign determines LT/GT)
                self.push_gpr(ra);
                self.push_gpr(rb);
                self.body.push(Instruction::I32Sub);
                self.body.push(Instruction::LocalSet(LOCAL_SCRATCH));
                self.emit_update_cr_field(cr_fd, LOCAL_SCRATCH);
            }

            // cmpi  crD, rA, SIMM  (signed)
            Opcode::Cmpi => {
                let cr_fd = ins.field_crfd() as u8;
                let ra = ins.gpr_a() as u8;
                let simm = ins.field_simm() as i32;
                self.push_gpr(ra);
                self.body.push(Instruction::I32Const(simm));
                self.body.push(Instruction::I32Sub);
                self.body.push(Instruction::LocalSet(LOCAL_SCRATCH));
                self.emit_update_cr_field(cr_fd, LOCAL_SCRATCH);
            }

            // cmpl  crD, rA, rB  (unsigned)
            Opcode::Cmpl => {
                let cr_fd = ins.field_crfd() as u8;
                let ra = ins.gpr_a() as u8;
                let rb = ins.gpr_b() as u8;
                self.push_gpr(ra);
                self.body.push(Instruction::LocalSet(LOCAL_SCRATCH));
                self.push_gpr(rb);
                // Use a second local (LOCAL_SCRATCH + 1 = 2) for rB
                self.body.push(Instruction::LocalSet(LOCAL_SCRATCH + 1));
                self.emit_update_cr_field_unsigned(cr_fd, LOCAL_SCRATCH, LOCAL_SCRATCH + 1);
            }

            // cmpli  crD, rA, UIMM  (unsigned)
            Opcode::Cmpli => {
                let cr_fd = ins.field_crfd() as u8;
                let ra = ins.gpr_a() as u8;
                let uimm = ins.field_uimm() as i32;
                self.push_gpr(ra);
                self.body.push(Instruction::LocalSet(LOCAL_SCRATCH));
                self.body.push(Instruction::I32Const(uimm));
                self.body.push(Instruction::LocalSet(LOCAL_SCRATCH + 1));
                self.emit_update_cr_field_unsigned(cr_fd, LOCAL_SCRATCH, LOCAL_SCRATCH + 1);
            }

            // ── Branches ──────────────────────────────────────────────────────

            // b / bl / ba / bla
            Opcode::B => {
                let lk = ins.field_lk();
                let aa = ins.field_aa();
                let li = ins.field_li();
                let target: u32 = if aa {
                    li as u32
                } else {
                    ins_pc.wrapping_add_signed(li)
                };
                if lk {
                    // LR = next instruction address
                    self.push_base();
                    self.body.push(Instruction::I32Const((ins_pc + 4) as i32));
                    self.store_lr();
                }
                self.body.push(Instruction::I32Const(target as i32));
                self.body.push(Instruction::Return);
                self.has_terminal = true;
                return true;
            }

            // bc / bcl / bca / bcla  (conditional branch to immediate)
            Opcode::Bc => {
                let bo = ins.field_bo();
                let bi = ins.field_bi() as u8;
                let bd = ins.field_bd();
                let lk = ins.field_lk();
                let aa = ins.field_aa();
                let target: u32 = if aa {
                    bd as u32
                } else {
                    ins_pc.wrapping_add_signed(bd as i32)
                };
                let fallthrough = ins_pc + 4;

                if lk {
                    self.push_base();
                    self.body.push(Instruction::I32Const((ins_pc + 4) as i32));
                    self.store_lr();
                }
                // Emit: if (condition) { return target } else { return fallthrough }
                self.emit_branch_condition(bo as u8, bi);
                self.body.push(Instruction::If(BlockType::Result(ValType::I32)));
                self.body.push(Instruction::I32Const(target as i32));
                self.body.push(Instruction::Else);
                self.body.push(Instruction::I32Const(fallthrough as i32));
                self.body.push(Instruction::End);
                self.body.push(Instruction::Return);
                self.has_terminal = true;
                return true;
            }

            // bclr  (branch conditional to LR)
            Opcode::Bclr => {
                let bo = ins.field_bo();
                let bi = ins.field_bi() as u8;
                let lk = ins.field_lk();

                // Save LR to scratch before possibly overwriting it
                self.push_lr();
                self.body.push(Instruction::LocalSet(LOCAL_SCRATCH));

                if lk {
                    self.push_base();
                    self.body.push(Instruction::I32Const((ins_pc + 4) as i32));
                    self.store_lr();
                }

                let unconditional = (bo >> 4) & 1 != 0 && (bo >> 2) & 1 != 0;
                if unconditional {
                    // Update PC and return 0 (block wrote PC itself)
                    self.push_base();
                    self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
                    self.store_pc();
                    self.body.push(Instruction::I32Const(0));
                } else {
                    self.emit_branch_condition(bo as u8, bi);
                    self.body.push(Instruction::If(BlockType::Result(ValType::I32)));
                    // taken: update PC, return 0
                    self.push_base();
                    self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
                    self.store_pc();
                    self.body.push(Instruction::I32Const(0));
                    self.body.push(Instruction::Else);
                    // not taken: return fallthrough
                    self.body.push(Instruction::I32Const((ins_pc + 4) as i32));
                    self.body.push(Instruction::End);
                }
                self.body.push(Instruction::Return);
                self.has_terminal = true;
                return true;
            }

            // bcctr  (branch conditional to CTR)
            Opcode::Bcctr => {
                let bo = ins.field_bo();
                let bi = ins.field_bi() as u8;
                let lk = ins.field_lk();

                self.push_ctr();
                self.body.push(Instruction::LocalSet(LOCAL_SCRATCH));

                if lk {
                    self.push_base();
                    self.body.push(Instruction::I32Const((ins_pc + 4) as i32));
                    self.store_lr();
                }

                let unconditional = (bo >> 4) & 1 != 0 && (bo >> 2) & 1 != 0;
                if unconditional {
                    self.push_base();
                    self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
                    self.store_pc();
                    self.body.push(Instruction::I32Const(0));
                } else {
                    self.emit_branch_condition(bo as u8, bi);
                    self.body.push(Instruction::If(BlockType::Result(ValType::I32)));
                    self.push_base();
                    self.body.push(Instruction::LocalGet(LOCAL_SCRATCH));
                    self.store_pc();
                    self.body.push(Instruction::I32Const(0));
                    self.body.push(Instruction::Else);
                    self.body.push(Instruction::I32Const((ins_pc + 4) as i32));
                    self.body.push(Instruction::End);
                }
                self.body.push(Instruction::Return);
                self.has_terminal = true;
                return true;
            }

            // ── SPR moves ─────────────────────────────────────────────────────

            // mfspr  rD, SPR
            Opcode::Mfspr => {
                let rd = ins.gpr_d() as u8;
                // SPR number is encoded as (sprhi << 5) | sprlo
                let spr = (ins.field_spr() as u32).rotate_right(5) & 0x3FF;
                self.push_base();
                match spr {
                    1 => {
                        // XER
                        self.push_base();
                        self.body.push(Instruction::I32Load(self.m32(self.offsets.xer)));
                    }
                    8 => {
                        // LR
                        self.push_lr();
                    }
                    9 => {
                        // CTR
                        self.push_ctr();
                    }
                    _ => {
                        // Unimplemented SPR: load 0
                        self.body.push(Instruction::I32Const(0));
                    }
                }
                self.store_gpr(rd);
            }

            // mtspr  SPR, rS
            Opcode::Mtspr => {
                let rs = ins.gpr_s() as u8;
                let spr = (ins.field_spr() as u32).rotate_right(5) & 0x3FF;
                match spr {
                    1 => {
                        // XER
                        self.push_base();
                        self.push_gpr(rs);
                        self.body.push(Instruction::I32Store(self.m32(self.offsets.xer)));
                    }
                    8 => {
                        // LR
                        self.push_base();
                        self.push_gpr(rs);
                        self.store_lr();
                    }
                    9 => {
                        // CTR
                        self.push_base();
                        self.push_gpr(rs);
                        self.store_ctr();
                    }
                    _ => {} // unimplemented SPR: silently ignore
                }
            }

            // mfcr  rD
            Opcode::Mfcr => {
                let rd = ins.gpr_d() as u8;
                self.push_base();
                self.push_cr();
                self.store_gpr(rd);
            }

            // mtcrf  CRM, rS
            Opcode::Mtcrf => {
                let rs = ins.gpr_s() as u8;
                let crm = ins.field_crm() as u32;
                // Expand CRM (8-bit mask) to a 32-bit field mask over CR nibbles
                let mut mask: u32 = 0;
                for bit in 0..8u32 {
                    if crm & (1 << (7 - bit)) != 0 {
                        mask |= 0xF << (28 - bit * 4);
                    }
                }
                self.push_base();
                self.push_cr();
                self.body.push(Instruction::I32Const((!mask) as i32));
                self.body.push(Instruction::I32And);
                self.push_gpr(rs);
                self.body.push(Instruction::I32Const(mask as i32));
                self.body.push(Instruction::I32And);
                self.body.push(Instruction::I32Or);
                self.store_cr();
            }

            // nop  (ori r0, r0, 0)
            // Already handled by the Ori case above (uimm == 0 → no-op emission).

            // sync / isync / eieio  — memory barriers, no-op in WASM
            Opcode::Sync | Opcode::Isync | Opcode::Eieio => {}

            // dcbz / icbi / dcbst  — cache operations, no-op in WASM
            Opcode::Dcbz | Opcode::Icbi | Opcode::Dcbst => {}

            // Unrecognised/unimplemented instruction: call raise_exception(0)
            _ => {
                self.body.push(Instruction::I32Const(0));
                self.body.push(Instruction::Call(imports::RAISE_EXCEPTION));
            }
        }

        false
    }

    /// Assemble the WASM module from the accumulated instructions.
    ///
    /// `fallthrough_pc` is the address of the instruction immediately after
    /// the last instruction in this block; it becomes the default return value
    /// when no terminal instruction set one explicitly.
    pub fn finish(mut self, fallthrough_pc: u32) -> WasmBlock {
        // Append fallthrough return if the block has no terminal.
        if !self.has_terminal {
            self.body.push(Instruction::I32Const(fallthrough_pc as i32));
        }
        self.body.push(Instruction::End); // end of function body

        // ── Type section ──────────────────────────────────────────────────────
        let mut types = TypeSection::new();
        // type 0: (i32) -> i32  — read hooks + execute
        types.ty().function([ValType::I32], [ValType::I32]);
        // type 1: (i32, i32) -> ()  — write hooks
        types.ty().function([ValType::I32, ValType::I32], []);
        // type 2: (i32) -> ()  — raise_exception
        types.ty().function([ValType::I32], []);

        // ── Import section ────────────────────────────────────────────────────
        let mut imports_section = ImportSection::new();
        let hooks = "hooks";
        imports_section.import(hooks, "read_u8", EntityType::Function(TY_I32_RET_I32));
        imports_section.import(hooks, "read_u16", EntityType::Function(TY_I32_RET_I32));
        imports_section.import(hooks, "read_u32", EntityType::Function(TY_I32_RET_I32));
        imports_section.import(hooks, "write_u8", EntityType::Function(TY_I32_I32_RET_VOID));
        imports_section.import(hooks, "write_u16", EntityType::Function(TY_I32_I32_RET_VOID));
        imports_section.import(hooks, "write_u32", EntityType::Function(TY_I32_I32_RET_VOID));
        imports_section.import(
            hooks,
            "raise_exception",
            EntityType::Function(TY_I32_RET_VOID),
        );

        // ── Function section ──────────────────────────────────────────────────
        let mut funcs = FunctionSection::new();
        funcs.function(TY_EXECUTE); // function index = imports::COUNT (7)

        // ── Export section ────────────────────────────────────────────────────
        let mut exports = ExportSection::new();
        exports.export("execute", ExportKind::Func, imports::COUNT);

        // ── Code section ──────────────────────────────────────────────────────
        // Locals: param 0 = regs_ptr (i32) already counted; we declare extras:
        //   local 1: i32  scratch
        //   local 2: i32  scratch2  (needed by cmplu/cmpli unsigned path)
        let mut func = Function::new(vec![(2u32, ValType::I32)]);
        for inst in &self.body {
            func.instruction(inst);
        }

        let mut codes = CodeSection::new();
        codes.function(&func);

        // ── Assemble module ───────────────────────────────────────────────────
        let mut module = Module::new();
        module.section(&types);
        module.section(&imports_section);
        module.section(&funcs);
        module.section(&exports);
        module.section(&codes);

        WasmBlock {
            bytes: module.finish(),
            instruction_count: self.ins_count,
            cycles: self.cycles,
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Generate a contiguous bitmask for PowerPC `rlwinm`/`rlwimi` instructions.
///
/// PowerPC uses big-endian bit numbering (bit 0 = MSB = bit 31 of the 32-bit
/// value).  This function converts MB and ME into a standard 32-bit mask.
pub(crate) fn ppc_mask(mb: u32, me: u32) -> u32 {
    if mb <= me {
        // Contiguous range
        let bits = me - mb + 1;
        let raw = if bits == 32 { u32::MAX } else { (1u32 << bits) - 1 };
        raw << (31 - me)
    } else {
        !ppc_mask(me + 1, mb - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::ppc_mask;

    #[test]
    fn ppc_mask_full_range() {
        assert_eq!(ppc_mask(0, 31), 0xFFFF_FFFF);
    }

    #[test]
    fn ppc_mask_lower_byte() {
        // MB=24, ME=31 → bits 31..24 in PPC notation → low byte 0xFF
        assert_eq!(ppc_mask(24, 31), 0x0000_00FF);
    }

    #[test]
    fn ppc_mask_upper_halfword() {
        // MB=0, ME=15 → bits 31..16 in PPC notation → upper 16 bits 0xFFFF_0000
        assert_eq!(ppc_mask(0, 15), 0xFFFF_0000);
    }

    #[test]
    fn ppc_mask_single_bit() {
        // MB=0, ME=0 → bit 31 = 0x8000_0000
        assert_eq!(ppc_mask(0, 0), 0x8000_0000);
        // MB=31, ME=31 → bit 0 = 0x0000_0001
        assert_eq!(ppc_mask(31, 31), 0x0000_0001);
    }
}
