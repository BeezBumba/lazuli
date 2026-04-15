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
    /// `() → (i32)` — load GQR[n] (Graphics Quantization Register, n ∈ 0..=7).
    LoadGqr(u8),
    /// `() → (i32)` — load FPSCR as raw i32 bits.
    LoadFpscr,

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
    /// `(i32) → ()` — store i32 to GQR[n] (n ∈ 0..=7).
    StoreGqr(u8),
    /// `(i32) → ()` — store raw i32 bits to FPSCR.
    StoreFpscr,

    // ─── Integer arithmetic ───────────────────────────────────────────────────
    I32Add, I32Sub, I32Mul, I32DivS, I32DivU,

    // ─── Integer bitwise / shift ──────────────────────────────────────────────
    I32And, I32Or, I32Xor,
    I32Not,              // (i32) → (i32) — bitwise NOT
    I32Shl, I32ShrU, I32ShrS, I32Rotl, I32Clz, I32Ctz,

    // ─── Integer comparisons → i32 0/1 ───────────────────────────────────────
    I32Eq, I32Ne, I32LtS, I32LtU, I32GtS, I32GtU, I32Eqz,

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
    /// `(f64) → (i32)` — saturating truncate f64 to i32 (toward zero, clamps on overflow/NaN).
    /// Matches `fcvt_to_sint_sat` used by the native JIT for `fctiw`/`fctiwz`.
    I32TruncSatF64S,
    /// `(f64) → (f64)` — round f64 to f32 precision and back (`frsp`).
    F64RoundToSingle,
    /// `(i32) → (f64)` — reinterpret u32 bits as f32, promote to f64 (`lfs`).
    F64PromoteSingleBits,
    /// `(f64) → (i32)` — demote f64 to f32, reinterpret as u32 (`stfs`).
    I32DemoteToSingleBits,
    /// `(f64) → (i32)` — reinterpret f64 bit-pattern as i64, take low 32 bits (`stfiwx`).
    I32FromF64LowBits,
    /// `(i64) → (f64)` — reinterpret i64 bit-pattern as f64 (bitcast, no numeric conversion).
    /// Used by `fctiw`/`fctiwz` to store the integer result as raw bits in the FPR.
    F64ReinterpretI64,
    /// `(f64) → (f64)` — round f64 to nearest integer (ties to even), used by `fctiw`.
    F64Nearest,

    // ─── Float select ─────────────────────────────────────────────────────────
    /// `(f64, f64, i32) → (f64)` — if cond≠0 return first f64, else second (`fsel`).
    F64Select,

    // ─── Integer byte-swap ────────────────────────────────────────────────────
    /// `(i32) → (i32)` — byte-swap all 4 bytes (`lwbrx`, `stwbrx`).
    I32Bswap,
    /// `(i32) → (i32)` — byte-swap low 2 bytes, zero-extend (`lhbrx`, `sthbrx`).
    I32Bswap16,

    // ─── 64-bit helpers (for high-word multiply, no i64 locals needed) ───────
    /// `(i32) → (i64)` — sign-extend i32 to i64.
    I64ExtendI32S,
    /// `(i32) → (i64)` — zero-extend i32 to i64.
    I64ExtendI32U,
    /// `(i64, i64) → (i64)` — 64-bit multiply.
    I64Mul,
    /// `(i64) → (i32)` — arithmetic shift right by 32, then wrap to i32.
    I64ShrS32,
    /// `(i64) → (i32)` — logical shift right by 32, then wrap to i32.
    I64ShrU32,
    /// `(f64) → (i64)` — reinterpret f64 bit-pattern as i64 (bitcast, no numeric conversion).
    /// Used by `mtfsf` to read raw FPR bits as an integer.
    I64ReinterpretF64,
    /// `(i64) → (i32)` — wrap i64 to i32 (take low 32 bits).
    /// Used by `mtfsf` to extract the low 32 bits of an FPR's i64 bit-pattern.
    I32WrapI64,

    // ─── Memory (via hook calls) ──────────────────────────────────────────────
    ReadU8, ReadU16, ReadU32, ReadF64,
    WriteU8, WriteU16, WriteU32, WriteF64,

    // ─── Quantized paired-single memory (via hook calls) ─────────────────────
    /// `(addr: i32, gqr: i32) → (f64)` — load one dequantized element.
    ///
    /// Reads 1, 2, or 4 bytes from guest memory at `addr` depending on
    /// `gqr.load_type`, applies `gqr.load_scale`, and returns the f64 result.
    PsqLoad,
    /// `(addr: i32, gqr: i32, val: f64) → (i32)` — quantize and store one element.
    ///
    /// Converts `val` per `gqr.store_type` / `gqr.store_scale`, writes 1, 2,
    /// or 4 bytes to guest memory at `addr`, and returns the byte count written.
    PsqStore,
    /// `(gqr: i32) → (i32)` — return the byte size of one load element.
    ///
    /// Extracts `gqr.load_type` from bits \[18:16\] and returns 4 for float,
    /// 2 for u16/i16, and 1 for u8/i8.  Used to advance the address between
    /// the first and second element of a paired load (W=0).
    PsqLoadSize,

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
