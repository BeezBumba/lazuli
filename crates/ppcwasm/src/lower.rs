//! IR → WebAssembly lowering.
//!
//! [`lower`] accepts an [`IrBlock`] produced by [`ppcir::Decoder`] and
//! assembles a self-contained WASM binary module containing one exported
//! function:
//!
//! ```text
//! (func "execute" (param $regs_ptr i32) (result i32))
//! ```
//!
//! ## Local variable layout
//!
//! ```text
//! local 0      i32   regs_ptr  (function parameter)
//! local 1..N   i32/f64  IR block locals (1-to-1 with IrLocal::index)
//! local N+1    i32   i32-scratch  (used by every i32 guest-register store)
//! local N+2    f64   f64-scratch  (used by every f64 guest-register store)
//! ```

use ppcir::{IrBlock, IrInst, IrTy};
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function,
    FunctionSection, Ieee64, ImportSection, Instruction, MemArg, MemoryType, Module,
    TypeSection, ValType,
};

use crate::block::{WasmBlock, imports};
use crate::offsets::RegOffsets;

// ─── WASM type indices ────────────────────────────────────────────────────────
const TY_I32_I32:   u32 = 0; // (i32) -> i32  — read hooks + execute
const TY_II_VOID:   u32 = 1; // (i32, i32) -> ()
const TY_I_VOID:    u32 = 2; // (i32) -> ()
const TY_I_F64:     u32 = 3; // (i32) -> f64
const TY_IF64_VOID: u32 = 4; // (i32, f64) -> ()
const TY_EXECUTE:   u32 = TY_I32_I32;

// ─── MemArg helpers ───────────────────────────────────────────────────────────
#[inline] fn m32 (o: u64) -> MemArg { MemArg { offset: o, align: 2, memory_index: 0 } }
#[allow(dead_code)]
#[inline] fn m8  (o: u64) -> MemArg { MemArg { offset: o, align: 0, memory_index: 0 } }
#[inline] fn mf64(o: u64) -> MemArg { MemArg { offset: o, align: 3, memory_index: 0 } }

// ─── Public entry point ───────────────────────────────────────────────────────

/// Compile an [`IrBlock`] into a standalone WASM binary module.
pub fn lower(block: &IrBlock, offsets: &RegOffsets) -> WasmBlock {
    let n_ir = block.local_types.len() as u32;
    let si32 = n_ir + 1; // i32 scratch local
    let sf64 = n_ir + 2; // f64 scratch local

    // ── Emit WASM body ────────────────────────────────────────────────────────
    let mut body: Vec<Instruction<'static>> = Vec::new();
    for inst in &block.insts {
        emit_inst(inst, offsets, si32, sf64, &mut body);
    }
    // Final `end` of the function body.
    body.push(Instruction::End);

    // ── Build locals list ─────────────────────────────────────────────────────
    // Group consecutive IR locals of the same type for the WASM locals section.
    let mut locals: Vec<(u32, ValType)> = Vec::new();
    for ty in &block.local_types {
        let vt = to_val(*ty);
        match locals.last_mut() {
            Some(last) if last.1 == vt => last.0 += 1,
            _ => locals.push((1, vt)),
        }
    }
    locals.push((1, ValType::I32)); // i32 scratch
    locals.push((1, ValType::F64)); // f64 scratch

    // ── Assemble WASM module ──────────────────────────────────────────────────
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32], [ValType::I32]);                    // 0
    types.ty().function([ValType::I32, ValType::I32], []);                  // 1
    types.ty().function([ValType::I32], []);                                // 2
    types.ty().function([ValType::I32], [ValType::F64]);                    // 3
    types.ty().function([ValType::I32, ValType::F64], []);                  // 4

    let mut isec = ImportSection::new();
    isec.import("env", "memory", EntityType::Memory(MemoryType {
        minimum: 1, maximum: None, memory64: false, shared: false, page_size_log2: None,
    }));
    let h = "hooks";
    isec.import(h, "read_u8",         EntityType::Function(TY_I32_I32));
    isec.import(h, "read_u16",        EntityType::Function(TY_I32_I32));
    isec.import(h, "read_u32",        EntityType::Function(TY_I32_I32));
    isec.import(h, "read_f64",        EntityType::Function(TY_I_F64));
    isec.import(h, "write_u8",        EntityType::Function(TY_II_VOID));
    isec.import(h, "write_u16",       EntityType::Function(TY_II_VOID));
    isec.import(h, "write_u32",       EntityType::Function(TY_II_VOID));
    isec.import(h, "write_f64",       EntityType::Function(TY_IF64_VOID));
    isec.import(h, "raise_exception", EntityType::Function(TY_I_VOID));

    let mut fsec = FunctionSection::new();
    fsec.function(TY_EXECUTE);

    let mut esec = ExportSection::new();
    esec.export("execute", ExportKind::Func, imports::COUNT);

    let mut func = Function::new(locals);
    for inst in &body { func.instruction(inst); }
    let mut codes = CodeSection::new();
    codes.function(&func);

    let mut module = Module::new();
    module.section(&types);
    module.section(&isec);
    module.section(&fsec);
    module.section(&esec);
    module.section(&codes);

    WasmBlock {
        bytes: module.finish(),
        instruction_count: block.instruction_count,
        cycles: block.cycles,
        unimplemented_ops: block.unimplemented_ops.clone(),
    }
}

// ─── Per-instruction translation ─────────────────────────────────────────────

fn to_val(ty: IrTy) -> ValType {
    match ty { IrTy::I32 => ValType::I32, IrTy::F64 => ValType::F64 }
}

/// Load an i32 from `[regs_ptr + offset]`.
fn load_i32(offset: u64, b: &mut Vec<Instruction<'static>>) {
    b.push(Instruction::LocalGet(0));
    b.push(Instruction::I32Load(m32(offset)));
}

/// Load an f64 from `[regs_ptr + offset]`.
fn load_f64(offset: u64, b: &mut Vec<Instruction<'static>>) {
    b.push(Instruction::LocalGet(0));
    b.push(Instruction::F64Load(mf64(offset)));
}

/// Store the i32 on stack-top to `[regs_ptr + offset]`.
///
/// WASM's `i32.store` expects `(addr, value)` with address below value on the
/// stack, but our value is already on top.  We use the scratch local to swap.
fn store_i32(offset: u64, si32: u32, b: &mut Vec<Instruction<'static>>) {
    b.push(Instruction::LocalSet(si32));      // pop value → scratch
    b.push(Instruction::LocalGet(0));          // push regs_ptr (addr)
    b.push(Instruction::LocalGet(si32));       // push value
    b.push(Instruction::I32Store(m32(offset)));
}

/// Store the f64 on stack-top to `[regs_ptr + offset]`.
fn store_f64(offset: u64, sf64: u32, b: &mut Vec<Instruction<'static>>) {
    b.push(Instruction::LocalSet(sf64));
    b.push(Instruction::LocalGet(0));
    b.push(Instruction::LocalGet(sf64));
    b.push(Instruction::F64Store(mf64(offset)));
}

fn emit_inst(
    inst: &IrInst,
    off: &RegOffsets,
    si32: u32,
    sf64: u32,
    b: &mut Vec<Instruction<'static>>,
) {
    match inst {
        // ── Constants ─────────────────────────────────────────────────────────
        IrInst::I32Const(v) => b.push(Instruction::I32Const(*v)),
        IrInst::F64Const(v) => b.push(Instruction::F64Const(Ieee64::from(*v))),

        // ── Stack ─────────────────────────────────────────────────────────────
        IrInst::Drop => b.push(Instruction::Drop),

        // ── Locals ────────────────────────────────────────────────────────────
        IrInst::LocalSet(l) => b.push(Instruction::LocalSet(l.index)),
        IrInst::LocalGet(l) => b.push(Instruction::LocalGet(l.index)),
        IrInst::LocalTee(l) => b.push(Instruction::LocalTee(l.index)),

        // ── Guest register loads ───────────────────────────────────────────────
        IrInst::LoadGpr(r)     => load_i32(off.gpr[*r as usize], b),
        IrInst::LoadFprPs0(r)  => load_f64(off.fpr_ps0[*r as usize], b),
        IrInst::LoadFprPs1(r)  => load_f64(off.fpr_ps1[*r as usize], b),
        IrInst::LoadCr         => load_i32(off.cr, b),
        IrInst::LoadLr         => load_i32(off.lr, b),
        IrInst::LoadCtr        => load_i32(off.ctr, b),
        IrInst::LoadXer        => load_i32(off.xer, b),
        IrInst::LoadMsr        => load_i32(off.msr, b),
        IrInst::LoadSrr0       => load_i32(off.srr0, b),
        IrInst::LoadSrr1       => load_i32(off.srr1, b),
        IrInst::LoadSprg(n)    => load_i32(off.sprg[*n as usize], b),
        IrInst::LoadTbLo       => load_i32(off.tb_lo, b),
        IrInst::LoadTbHi       => load_i32(off.tb_hi, b),
        IrInst::LoadDec        => load_i32(off.dec, b),

        // ── Guest register stores ──────────────────────────────────────────────
        IrInst::StoreGpr(r)    => store_i32(off.gpr[*r as usize], si32, b),
        IrInst::StoreFprPs0(r) => store_f64(off.fpr_ps0[*r as usize], sf64, b),
        IrInst::StoreFprPs1(r) => store_f64(off.fpr_ps1[*r as usize], sf64, b),
        IrInst::StoreCr        => store_i32(off.cr, si32, b),
        IrInst::StoreLr        => store_i32(off.lr, si32, b),
        IrInst::StoreCtr       => store_i32(off.ctr, si32, b),
        IrInst::StoreXer       => store_i32(off.xer, si32, b),
        IrInst::StoreMsr       => store_i32(off.msr, si32, b),
        IrInst::StoreSrr0      => store_i32(off.srr0, si32, b),
        IrInst::StoreSrr1      => store_i32(off.srr1, si32, b),
        IrInst::StoreSprg(n)   => store_i32(off.sprg[*n as usize], si32, b),
        IrInst::StoreDec       => store_i32(off.dec, si32, b),
        IrInst::StorePC        => store_i32(off.pc, si32, b),

        // ── Integer arithmetic ─────────────────────────────────────────────────
        IrInst::I32Add   => b.push(Instruction::I32Add),
        IrInst::I32Sub   => b.push(Instruction::I32Sub),
        IrInst::I32Mul   => b.push(Instruction::I32Mul),
        IrInst::I32DivS  => b.push(Instruction::I32DivS),
        IrInst::I32DivU  => b.push(Instruction::I32DivU),

        // ── Bitwise / shift ────────────────────────────────────────────────────
        IrInst::I32And  => b.push(Instruction::I32And),
        IrInst::I32Or   => b.push(Instruction::I32Or),
        IrInst::I32Xor  => b.push(Instruction::I32Xor),
        IrInst::I32Not  => {
            b.push(Instruction::I32Const(-1));
            b.push(Instruction::I32Xor);
        }
        IrInst::I32Shl  => b.push(Instruction::I32Shl),
        IrInst::I32ShrU => b.push(Instruction::I32ShrU),
        IrInst::I32ShrS => b.push(Instruction::I32ShrS),
        IrInst::I32Rotl => b.push(Instruction::I32Rotl),
        IrInst::I32Clz  => b.push(Instruction::I32Clz),
        IrInst::I32Ctz  => b.push(Instruction::I32Ctz),

        // ── Integer comparisons → i32 0 or 1 ──────────────────────────────────
        IrInst::I32Eq  => b.push(Instruction::I32Eq),
        IrInst::I32Ne  => b.push(Instruction::I32Ne),
        IrInst::I32LtS => b.push(Instruction::I32LtS),
        IrInst::I32LtU => b.push(Instruction::I32LtU),
        IrInst::I32GtS => b.push(Instruction::I32GtS),
        IrInst::I32GtU => b.push(Instruction::I32GtU),
        IrInst::I32Eqz => b.push(Instruction::I32Eqz),

        // ── Integer sign-extension ─────────────────────────────────────────────
        IrInst::I32Extend8S  => b.push(Instruction::I32Extend8S),
        IrInst::I32Extend16S => b.push(Instruction::I32Extend16S),

        // ── Float arithmetic ───────────────────────────────────────────────────
        IrInst::F64Add  => b.push(Instruction::F64Add),
        IrInst::F64Sub  => b.push(Instruction::F64Sub),
        IrInst::F64Mul  => b.push(Instruction::F64Mul),
        IrInst::F64Div  => b.push(Instruction::F64Div),
        IrInst::F64Neg  => b.push(Instruction::F64Neg),
        IrInst::F64Abs  => b.push(Instruction::F64Abs),
        IrInst::F64Sqrt => b.push(Instruction::F64Sqrt),

        // ── Float comparisons → i32 ────────────────────────────────────────────
        // WASM comparison instructions return i32 0/1; NaN inputs always yield 0.
        IrInst::F64Lt => b.push(Instruction::F64Lt),
        IrInst::F64Gt => b.push(Instruction::F64Gt),
        IrInst::F64Eq => b.push(Instruction::F64Eq),

        // ── Float conversions ──────────────────────────────────────────────────
        IrInst::F64FromI32S => b.push(Instruction::F64ConvertI32S),
        IrInst::I32TruncF64S => b.push(Instruction::I32TruncF64S),
        IrInst::F64RoundToSingle => {
            b.push(Instruction::F32DemoteF64);
            b.push(Instruction::F64PromoteF32);
        }
        IrInst::F64PromoteSingleBits => {
            // i32 bits → f32 → f64  (implements `lfs`)
            b.push(Instruction::F32ReinterpretI32);
            b.push(Instruction::F64PromoteF32);
        }
        IrInst::I32DemoteToSingleBits => {
            // f64 → f32 → i32 bits  (implements `stfs`)
            b.push(Instruction::F32DemoteF64);
            b.push(Instruction::I32ReinterpretF32);
        }
        IrInst::I32FromF64LowBits => {
            // f64 bit-pattern as i64, take low 32 bits  (implements `stfiwx`)
            b.push(Instruction::I64ReinterpretF64);
            b.push(Instruction::I32WrapI64);
        }

        // ── Float select ──────────────────────────────────────────────────────
        // Stack: (f64 val1, f64 val2, i32 cond) → (f64): cond ? val1 : val2
        IrInst::F64Select => b.push(Instruction::Select),

        // ── Integer byte-swap ─────────────────────────────────────────────────
        IrInst::I32Bswap => {
            // Byte-swap 32-bit value using shifts and masks (no native WASM bswap).
            // Result: ((x&0xFF)<<24) | ((x&0xFF00)<<8) | ((x>>8)&0xFF00) | (x>>24)
            b.push(Instruction::LocalSet(si32));
            // byte at position 3 → position 0 (shift left 24)
            b.push(Instruction::LocalGet(si32));
            b.push(Instruction::I32Const(0xFF));
            b.push(Instruction::I32And);
            b.push(Instruction::I32Const(24));
            b.push(Instruction::I32Shl);
            // byte at position 2 → position 1 (shift left 8)
            b.push(Instruction::LocalGet(si32));
            b.push(Instruction::I32Const(0xFF00));
            b.push(Instruction::I32And);
            b.push(Instruction::I32Const(8));
            b.push(Instruction::I32Shl);
            b.push(Instruction::I32Or);
            // byte at position 1 → position 2 (shift right 8)
            b.push(Instruction::LocalGet(si32));
            b.push(Instruction::I32Const(8));
            b.push(Instruction::I32ShrU);
            b.push(Instruction::I32Const(0xFF00));
            b.push(Instruction::I32And);
            b.push(Instruction::I32Or);
            // byte at position 0 → position 3 (shift right 24)
            b.push(Instruction::LocalGet(si32));
            b.push(Instruction::I32Const(24));
            b.push(Instruction::I32ShrU);
            b.push(Instruction::I32Or);
        }
        IrInst::I32Bswap16 => {
            // Byte-swap low 2 bytes, zero-extend: ((x&0xFF)<<8) | ((x>>8)&0xFF)
            b.push(Instruction::LocalSet(si32));
            b.push(Instruction::LocalGet(si32));
            b.push(Instruction::I32Const(0xFF));
            b.push(Instruction::I32And);
            b.push(Instruction::I32Const(8));
            b.push(Instruction::I32Shl);
            b.push(Instruction::LocalGet(si32));
            b.push(Instruction::I32Const(8));
            b.push(Instruction::I32ShrU);
            b.push(Instruction::I32Const(0xFF));
            b.push(Instruction::I32And);
            b.push(Instruction::I32Or);
        }

        // ── 64-bit helpers for high-word multiply ─────────────────────────────
        IrInst::I64ExtendI32S => b.push(Instruction::I64ExtendI32S),
        IrInst::I64ExtendI32U => b.push(Instruction::I64ExtendI32U),
        IrInst::I64Mul        => b.push(Instruction::I64Mul),
        IrInst::I64ShrS32 => {
            // arithmetic shift right by 32, then wrap to i32
            b.push(Instruction::I64Const(32));
            b.push(Instruction::I64ShrS);
            b.push(Instruction::I32WrapI64);
        }
        IrInst::I64ShrU32 => {
            // logical shift right by 32, then wrap to i32
            b.push(Instruction::I64Const(32));
            b.push(Instruction::I64ShrU);
            b.push(Instruction::I32WrapI64);
        }

        // ── Memory access via host hooks ───────────────────────────────────────
        // addr is on top of stack; call pops it, pushes result.
        IrInst::ReadU8  => b.push(Instruction::Call(imports::READ_U8)),
        IrInst::ReadU16 => b.push(Instruction::Call(imports::READ_U16)),
        IrInst::ReadU32 => b.push(Instruction::Call(imports::READ_U32)),
        IrInst::ReadF64 => b.push(Instruction::Call(imports::READ_F64)),
        // (addr, val) on stack in that order; WASM call pops val then addr.
        IrInst::WriteU8  => b.push(Instruction::Call(imports::WRITE_U8)),
        IrInst::WriteU16 => b.push(Instruction::Call(imports::WRITE_U16)),
        IrInst::WriteU32 => b.push(Instruction::Call(imports::WRITE_U32)),
        IrInst::WriteF64 => b.push(Instruction::Call(imports::WRITE_F64)),

        // ── Control flow ──────────────────────────────────────────────────────

        IrInst::ReturnStatic(pc) => {
            b.push(Instruction::I32Const(*pc as i32));
            b.push(Instruction::Return);
        }
        IrInst::ReturnDynamic => {
            // The PC value is already on the stack.  Write it to CPU::pc in the
            // register file so that the host's `set_cpu_bytes` / `get_pc()` path
            // sees the correct next-PC even when the value is 0 (e.g. `bctrl`
            // with CTR=0 or `rfi` with SRR0=0).  Without this write the JS
            // fallback `emu.get_pc()` returns the *old* PC (the block start),
            // leaving the emulator permanently stuck at the same address.
            store_i32(off.pc, si32, b);            // pop, store to CPU::pc, keep in si32
            b.push(Instruction::LocalGet(si32));   // re-push the value for the return
            b.push(Instruction::Return);
        }

        // if (condition) { return taken } else { (fall off → return fallthrough) }
        IrInst::BranchIf { taken, fallthrough } => {
            b.push(Instruction::If(BlockType::Empty));
            b.push(Instruction::I32Const(*taken as i32));
            b.push(Instruction::Return);
            b.push(Instruction::End);
            b.push(Instruction::I32Const(*fallthrough as i32));
            // Function falls off the end, returning fallthrough.
        }

        // if (condition) { CPU::pc = reg_local; return 0 } else { fallthrough }
        IrInst::BranchRegIf { reg_local, fallthrough } => {
            b.push(Instruction::If(BlockType::Empty));
            // Store reg_local into CPU::pc:
            b.push(Instruction::LocalGet(0));           // regs_ptr
            b.push(Instruction::LocalGet(reg_local.index)); // target pc
            b.push(Instruction::I32Store(m32(off.pc))); // CPU::pc = target
            b.push(Instruction::I32Const(0));
            b.push(Instruction::Return);
            b.push(Instruction::End);
            b.push(Instruction::I32Const(*fallthrough as i32));
        }

        IrInst::RaiseException(kind) => {
            b.push(Instruction::I32Const(*kind));
            b.push(Instruction::Call(imports::RAISE_EXCEPTION));
            b.push(Instruction::I32Const(0));
            b.push(Instruction::Return);
        }
    }
}
