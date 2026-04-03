//! IR instruction set for the Lazuli GameCube WASM JIT.
//!
//! Inspired by Play!'s Jitter framework: a typed, stack-machine IR that sits
//! between PowerPC decoding and the WASM code generator.  Every instruction
//! pops its inputs from the top of the virtual operand stack and pushes its
//! result (if any).  Stack notation: `(inputs) → (outputs)`.

/// Type tag for IR values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrTy {
    /// 32-bit integer (GPRs, CR, LR, CTR, XER, …).
    I32,
    /// 64-bit IEEE-754 double (FPRs, paired-single slots).
    F64,
}

/// A typed local variable slot.  Index 0 is always `regs_ptr` (the function
/// parameter).  Decoder-allocated locals start at index 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrLocal {
    pub index: u32,
    pub ty: IrTy,
}

/// One IR instruction.  All arithmetic / compare ops work on a virtual stack;
/// loads / stores access the guest CPU struct via `regs_ptr` (local 0).
#[derive(Debug, Clone)]
pub enum IrInst {
    // ─── Constants ───────────────────────────────────────────────────────────
    I32Const(i32),
    F64Const(f64),

    // ─── Stack ops ────────────────────────────────────────────────────────────
    /// `(T) → ()` — discard top of stack.
    Drop,

    // ─── Locals ──────────────────────────────────────────────────────────────
    LocalSet(IrLocal),   // `(T) → ()`
    LocalGet(IrLocal),   // `() → (T)`
    LocalTee(IrLocal),   // `(T) → (T)`

    // ─── Guest register loads ─────────────────────────────────────────────────
    LoadGpr(u8),         // () → i32
    LoadFprPs0(u8),      // () → f64
    LoadFprPs1(u8),      // () → f64
    LoadCr,
    LoadLr,
    LoadCtr,
    LoadXer,
    LoadMsr,
    LoadSrr0,
    LoadSrr1,
    LoadSprg(u8),        // n ∈ 0..=3
    LoadTbLo,
    LoadTbHi,
    LoadDec,

    // ─── Guest register stores ────────────────────────────────────────────────
    StoreGpr(u8),        // (i32) → ()
    StoreFprPs0(u8),     // (f64) → ()
    StoreFprPs1(u8),     // (f64) → ()
    StoreCr,
    StoreLr,
    StoreCtr,
    StoreXer,
    StoreMsr,
    StoreSrr0,
    StoreSrr1,
    StoreSprg(u8),
    StoreDec,
    StorePC,             // write CPU's pc field

    // ─── Integer arithmetic ───────────────────────────────────────────────────
    I32Add, I32Sub, I32Mul, I32DivS, I32DivU,

    // ─── Integer bitwise / shift ──────────────────────────────────────────────
    I32And, I32Or, I32Xor,
    I32Not,              // (i32) → (i32) — bitwise NOT
    I32Shl, I32ShrU, I32ShrS, I32Rotl, I32Clz,

    // ─── Integer comparisons → i32 0/1 ───────────────────────────────────────
    I32Eq, I32LtS, I32LtU, I32GtS, I32GtU, I32Eqz,

    // ─── Integer sign-extension ───────────────────────────────────────────────
    I32Extend8S, I32Extend16S,

    // ─── Float arithmetic ─────────────────────────────────────────────────────
    F64Add, F64Sub, F64Mul, F64Div, F64Neg, F64Abs, F64Sqrt,

    // ─── Float comparisons → i32 0/1 ─────────────────────────────────────────
    /// `(f64, f64) → (i32)` — 1 if lhs < rhs, 0 otherwise (0 for NaN inputs).
    F64Lt,
    /// `(f64, f64) → (i32)` — 1 if lhs > rhs, 0 otherwise (0 for NaN inputs).
    F64Gt,
    /// `(f64, f64) → (i32)` — 1 if lhs == rhs, 0 otherwise (0 for NaN inputs).
    F64Eq,

    // ─── Float conversions ────────────────────────────────────────────────────
    /// `(i32) → (f64)` — signed i32 to f64.
    F64FromI32S,
    /// `(f64) → (i32)` — truncate f64 to i32 (toward zero).
    I32TruncF64S,
    /// `(f64) → (f64)` — round f64 to f32 precision and back (`frsp`).
    F64RoundToSingle,
    /// `(i32) → (f64)` — reinterpret u32 bits as f32, promote to f64 (`lfs`).
    F64PromoteSingleBits,
    /// `(f64) → (i32)` — demote f64 to f32, reinterpret as u32 (`stfs`).
    I32DemoteToSingleBits,

    // ─── Memory (via hook calls) ──────────────────────────────────────────────
    ReadU8, ReadU16, ReadU32, ReadF64,
    WriteU8, WriteU16, WriteU32, WriteF64,

    // ─── Control flow (terminal) ──────────────────────────────────────────────
    /// Return a compile-time constant PC.
    ReturnStatic(u32),
    /// Return the i32 on top of the stack as the next PC.
    ReturnDynamic,
    /// `(i32 cond) → ()` — if cond ≠ 0 return `taken`, else return `fallthrough`.
    BranchIf { taken: u32, fallthrough: u32 },
    /// `(i32 cond) → ()` — if cond ≠ 0 store `reg_local` into CPU::pc and
    /// return 0; else return `fallthrough`.
    BranchRegIf { reg_local: IrLocal, fallthrough: u32 },
    /// Call `raise_exception(kind)` and terminate.
    RaiseException(i32),
}

/// A compiled IR basic block.
#[derive(Debug, Default, Clone)]
pub struct IrBlock {
    pub insts: Vec<IrInst>,
    pub instruction_count: u32,
    pub cycles: u32,
    /// Types of extra locals (local_types[n] = type of WASM local n+1).
    pub local_types: Vec<IrTy>,
    /// Names of unimplemented PPC opcodes.
    pub unimplemented_ops: Vec<String>,
}

impl IrBlock {
    /// Allocate a new typed local and return its handle.
    pub fn alloc_local(&mut self, ty: IrTy) -> IrLocal {
        let index = (self.local_types.len() as u32) + 1;
        self.local_types.push(ty);
        IrLocal { index, ty }
    }

    #[inline]
    pub fn push(&mut self, inst: IrInst) {
        self.insts.push(inst);
    }
}
