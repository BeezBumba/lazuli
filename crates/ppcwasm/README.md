# ppcwasm — PowerPC → WebAssembly Dynarec

`ppcwasm` is a **dynamic recompiler** (dynarec) that translates PowerPC machine code into
[WebAssembly](https://webassembly.org/) bytecode at runtime.  The compiled blocks can be
executed by any WASM runtime, including a web browser via `WebAssembly.instantiate()`.

Together with the [`lazuli-web`](../lazuli-web) browser frontend it forms the **dynarec WASM
demo**: a fully in-browser GameCube CPU emulation demo where you can load a ROM, step through
execution block-by-block, and watch the CPU register file update in real time — all without any
native binary.

> **Quick start**
> ```sh
> just web-serve          # build ppcwasm + lazuli-web and open http://localhost:8080
> ```

---

## Table of Contents

- [Architecture overview](#architecture-overview)
- [Translation pipeline](#translation-pipeline)
- [Generated WASM module interface](#generated-wasm-module-interface)
- [Register layout in linear memory](#register-layout-in-linear-memory)
- [Memory access via hooks](#memory-access-via-hooks)
- [Block termination and chaining](#block-termination-and-chaining)
- [Relationship to the Play! PS2 emulator](#relationship-to-the-play-ps2-emulator)
- [Browser frontend (lazuli-web)](#browser-frontend-lazuli-web)
- [Crate structure](#crate-structure)

---

## Architecture overview

```
┌──────────────────────────────────────────────┐
│              lazuli-web (browser)             │
│                                               │
│  ┌──────────────┐    ┌─────────────────────┐ │
│  │  bootstrap.js│    │  lazuli-web Rust lib│ │
│  │  game loop   │◄──►│  (wasm-bindgen)     │ │
│  │  XFB render  │    │  WasmEmulator       │ │
│  └──────┬───────┘    └─────────┬───────────┘ │
│         │                      │              │
│    WebAssembly.instantiate()   │ compile_block│
│         │                      ▼              │
│         │            ┌─────────────────────┐  │
│         │            │   ppcwasm  (Rust)   │  │
│         │            │   WasmJit::build()  │  │
│         │            │   BlockBuilder      │  │
│         │            └─────────────────────┘  │
│         │                                      │
│  ┌──────▼──────────────────────────────────┐  │
│  │         WASM linear memory              │  │
│  │  [0..sizeof(Cpu)]  = gekko::Cpu state   │  │
│  │  [sizeof(Cpu)..]   = guest RAM (24 MiB) │  │
│  └─────────────────────────────────────────┘  │
└──────────────────────────────────────────────┘
```

The key insight — borrowed directly from the **Play! PS2 emulator** — is that *both* the CPU
register file and the guest RAM live inside a single flat WASM linear memory.  This means the
JIT-emitted code can access any register or guest address through a plain `i32.load` /
`i32.store` with a constant immediate offset, avoiding any runtime address computation.

---

## Translation pipeline

```
PowerPC instructions (raw u32 words)
        │
        ▼
gekko::disasm::Ins              ← decoded instruction objects
        │
        ▼  (for each instruction)
builder::BlockBuilder::emit()   ← lowered to WASM stack-machine ops
        │
        ▼
wasm_encoder::Module::finish()  ← assembled into a binary WASM module
        │
        ▼
WasmBlock { bytes: Vec<u8> }    ← raw WASM module (starts with \0asm)
        │
        ▼
WebAssembly.compile(bytes)      ← browser/runtime compiles to native code
        │
        ▼
WebAssembly.instantiate(module, { env: { memory }, hooks: { … } })
        │
        ▼
instance.exports.execute(regs_ptr)   ← JIT-compiled WASM runs
```

**Step 1 — Decode.**  The caller turns raw 32-bit PowerPC words into `gekko::disasm::Ins`
objects.  `lazuli-web` reads these from guest RAM at the current PC.

**Step 2 — Translate.**  `WasmJit::build()` iterates over `(pc, Ins)` pairs and calls
`BlockBuilder::emit()` for each one.  Every PowerPC instruction becomes a short sequence of
WASM stack-machine instructions.  For example:

```
addi r3, r0, 42
  →  local.get  $regs_ptr        // push base pointer
     local.get  $regs_ptr        // push base pointer (for the store)
     i32.const  0                 // r0 is always 0 for addi
     i32.const  42
     i32.add
     i32.store  offset=<gpr[3]>  // write result into CPU struct
```

**Step 3 — Assemble.**  `BlockBuilder::finish()` wraps the accumulated instructions into a
complete WASM binary module: type section → import section → function section →
export section → code section.

**Step 4 — Compile & instantiate.**  `lazuli-web` calls the browser's
`WebAssembly.compile()` + `WebAssembly.instantiate()` APIs.  The browser JIT-compiles the WASM
bytecode to native machine code (x86-64, AArch64, …) internally.

**Step 5 — Execute.**  `instance.exports.execute(regs_ptr)` is called with the byte offset of
the `Cpu` struct inside WASM memory.  It returns the next PC for the emulator to jump to.

---

## Generated WASM module interface

Every `WasmBlock` is a self-contained WASM binary module with this layout:

```wat
(module
  ;; --- type section ---
  (type $t_read      (func (param i32) (result i32)))       ;; read hooks + execute
  (type $t_write     (func (param i32 i32)))                ;; write hooks
  (type $t_except    (func (param i32)))                    ;; raise_exception

  ;; --- import section ---
  (import "env"   "memory"          (memory 1))
  (import "hooks" "read_u8"         (func $read_u8   (type $t_read)))
  (import "hooks" "read_u16"        (func $read_u16  (type $t_read)))
  (import "hooks" "read_u32"        (func $read_u32  (type $t_read)))
  (import "hooks" "write_u8"        (func $write_u8  (type $t_write)))
  (import "hooks" "write_u16"       (func $write_u16 (type $t_write)))
  (import "hooks" "write_u32"       (func $write_u32 (type $t_write)))
  (import "hooks" "raise_exception" (func $except    (type $t_except)))

  ;; --- function + export ---
  (func $execute (export "execute") (param $regs_ptr i32) (result i32)
    ;; … translated PowerPC instructions …
  )
)
```

| Import | Signature | Purpose |
|--------|-----------|---------|
| `read_u8` | `(i32) → i32` | Load a byte from guest address space |
| `read_u16` | `(i32) → i32` | Load a half-word |
| `read_u32` | `(i32) → i32` | Load a word |
| `write_u8` | `(i32, i32) → ()` | Store a byte |
| `write_u16` | `(i32, i32) → ()` | Store a half-word |
| `write_u32` | `(i32, i32) → ()` | Store a word |
| `raise_exception` | `(i32) → ()` | Signal a CPU exception |

The `execute` function takes one parameter — `regs_ptr`, the byte offset of the `Cpu` struct
inside WASM linear memory — and returns the **next PC** the emulator should fetch from.  A
return value of `0` means the block already wrote the new PC directly into `Cpu::pc` (used for
computed branches like `blr` and `bcctr`).

---

## Register layout in linear memory

This is the most important design decision, and the one most directly inherited from Play!.

The `gekko::Cpu` struct is `#[repr(C)]`, meaning its field layout is fully determined by the
Rust ABI and does not change across compilations with the same toolchain.  `RegOffsets::compute()`
uses Rust's `offset_of!()` macro at startup to record the exact byte offset of every register
field:

```
WASM linear memory
┌─────────────────────────────────────────┐ ← byte 0  (regs_ptr argument)
│  Cpu::pc          (u32, 4 bytes)        │
├─────────────────────────────────────────┤
│  Cpu::user.gpr[0..32]  (u32 × 32)      │
├─────────────────────────────────────────┤
│  Cpu::user.fpr[0..32]  ([f64;2] × 32)  │
├─────────────────────────────────────────┤
│  Cpu::user.cr / lr / ctr / xer  …      │
├─────────────────────────────────────────┤
│  Cpu::supervisor fields (tb, msr, …)   │
└─────────────────────────────────────────┘
  … followed by guest RAM (lazuli-web) …
```

Because all offsets are **static compile-time constants**, every register read or write in the
translated code is a single WASM `i32.load` / `i32.store` with an immediate displacement —
there is no runtime address arithmetic at all.

For example, `push_gpr(3)` emits:

```wasm
local.get  $regs_ptr          ;; push the base pointer
i32.load   offset=<gpr[3]>   ;; load r3 from Cpu struct
```

and `store_gpr(3)` emits:

```wasm
local.get  $regs_ptr          ;; push the base pointer
<value already on stack>
i32.store  offset=<gpr[3]>   ;; write back to Cpu struct
```

---

## Memory access via hooks

Guest memory accesses (`lwz`, `stw`, `lbz`, etc.) are **not** handled by direct WASM memory
loads.  Instead they call into the imported hook functions:

```
lwz r3, 8(r4)
  →  local.get  $regs_ptr
     i32.load   offset=<gpr[4]>   ;; load base address from r4
     i32.const  8                  ;; add displacement
     i32.add
     call $read_u32                ;; call into the JavaScript hook
     local.get  $regs_ptr
     i32.store  offset=<gpr[3]>   ;; write result into r3
```

The hook implementations in `lazuli-web/bootstrap.js` strip the high bits of the guest address
(applying `PHYS_MASK = 0x01FFFFFF`) before indexing into the JavaScript `Uint8Array` view of
the emulated RAM.  This zero-copy approach means the hook can read or write the 24 MiB guest
RAM with a single typed-array access.

---

## Block termination and chaining

`BlockBuilder::emit()` returns `true` when it encounters a **terminal instruction** — an
instruction that unconditionally transfers control:

- `b` / `bl` / `ba` / `bla` (unconditional branch)
- `bc` / `bcl` with BO bits that make it unconditional
- `blr` (branch to LR)
- `bcctr` (branch to CTR)
- `rfi` (return from interrupt)

Once a terminal instruction is emitted, `WasmJit::build()` stops consuming instructions and
calls `finish()`.  This matches how a basic-block compiler works: a block runs until the first
control-flow change.

Terminal branches that produce a **static target** (e.g. `b +0x10`) encode the target address
as the return value:

```wasm
i32.const  0x8000_0010        ;; branch target
return                         ;; exit execute(), return next PC
```

Computed branches (`blr`, `bcctr`) write the new PC directly into `Cpu::pc` and return `0`:

```wasm
local.get  $regs_ptr
local.get  $regs_ptr
i32.load   offset=<lr>        ;; load LR value
i32.store  offset=<pc>        ;; write new PC into Cpu struct
i32.const  0
return
```

The emulator loop in `bootstrap.js` uses the return value to update its `currentPc` variable
and immediately looks up (or compiles) the next block.

---

## Relationship to the Play! PS2 emulator

[**Play!**](https://github.com/jpd002/Play-) is an open-source PlayStation 2 emulator by
Jean-Philip Desjardins.  It introduced the pattern of targeting **WebAssembly as a JIT backend**
so that the emulator can run inside a web browser without any native plugins.

`ppcwasm` directly mirrors the key design choices from Play!'s
`Nuanceur IR → WebAssembly` compiler:

| Concept | Play! (MIPS / EE core) | ppcwasm (PowerPC / Gekko) |
|---------|------------------------|---------------------------|
| **Guest ISA** | MIPS III + PS2 extensions | PowerPC 750CL (Gekko) |
| **JIT target** | WebAssembly | WebAssembly |
| **IR layer** | Nuanceur (SSA-based IR) | None — direct lowering |
| **Register state location** | `#[repr(C)]`-equivalent struct in WASM linear memory | `#[repr(C)] gekko::Cpu` in WASM linear memory |
| **Register access** | Constant-offset `i32.load` / `i32.store` | Constant-offset `i32.load` / `i32.store` |
| **Guest memory** | Imported hook functions | Imported hook functions (`read_u8` … `write_u32`) |
| **Block interface** | `execute(regs_ptr) → next_pc` | `execute(regs_ptr: i32) → i32` |
| **Runtime** | Browser (`WebAssembly.instantiate`) | Browser (`WebAssembly.instantiate`) |

### What ppcwasm does differently

Play! uses an intermediate representation called **Nuanceur**, an SSA-based IR that sits
between the MIPS decoder and the WASM code generator.  This extra layer enables optimisations
such as constant folding and dead-code elimination before code is emitted.

`ppcwasm` is a simpler, **direct-lowering** design: each PowerPC instruction is translated into
a fixed sequence of WASM instructions with no intermediate IR.  This makes the codebase easier
to follow and extend, at the cost of some optimisation opportunities (which are mostly left to
the browser's own WASM JIT).

### Why store registers in linear memory?

The alternative would be to use WASM `local` variables for each guest register.  That sounds
faster, but it makes the `execute` function signature unwieldy (32 GPRs + SPRs + …) and forces
the caller to marshal every register in/out of the WASM call frame on every block boundary.

By storing the entire `Cpu` struct at a fixed location in linear memory, **the state is already
shared** between the Rust/JS host and the WASM block without any copying.  The host can read
and update individual registers (for exceptions, I/O, etc.) by simply writing to the
appropriate offset in the WASM memory buffer.

Play! pioneered this trade-off for emulator JITs and it works extremely well in practice.

---

## Browser frontend (lazuli-web)

`crates/lazuli-web` is the browser-side glue that ties `ppcwasm` to a real game loop.

### Build

```sh
# from workspace root
just web-serve
# or manually:
cd crates/lazuli-web
wasm-pack build --target web --out-dir www/pkg
cd www
python3 -m http.server 8080
```

Then open **http://localhost:8080**.

### What happens at runtime

1. **ISO loading** — `bootstrap.js` parses the GameCube disc header
   (`magic = 0xC2339F3D`), extracts the boot DOL's text/data sections, and loads
   them into the 24 MiB emulated RAM at their target addresses.

2. **Zero-copy RAM view** — `emu.wasm_memory()` returns the raw `WebAssembly.Memory`
   object.  `bootstrap.js` wraps it with a `Uint8Array` so that hook closures can read/write
   guest RAM without any copying.

3. **Block compilation** — `executeOneBlockSync()` calls `emu.compile_block(pc)`,
   which invokes `WasmJit::build()` in Rust and returns the raw WASM bytes to JavaScript.

4. **Instantiation** — `new WebAssembly.Module(bytes)` + `WebAssembly.instantiate(module, imports)`
   compile and link the block.  The imports object wires up the hook closures from step 2.

5. **Execution** — `instance.exports.execute(0)` runs the block (CPU state lives at offset 0
   in WASM memory).  The return value becomes the next PC.

6. **Time base** — `emu.advance_timebase(675_000)` is called once per animation frame
   (≈ 60 Hz × 675 000 = 40.5 MHz, roughly matching the GameCube's 486 MHz / 12 ratio).
   Without this, games that spin on the time base register will hang.

7. **XFB rendering** — Once a frame, `bootstrap.js` reads the YUV422 frame buffer at
   `0x00C00000`, converts it to RGBA using BT.601 coefficients, and paints it to a
   640×480 `<canvas>`.

### Block cache

Compiled `WebAssembly.Module` objects are cached in a `Map<pc, Module>`.  On a cache hit the
block is re-instantiated (cheap) rather than re-compiled (expensive).  The cache is flushed
when the user clicks **Reset**.

---

## Crate structure

```
crates/ppcwasm/
├── src/
│   ├── lib.rs        — public API: WasmJit, build()
│   ├── block.rs      — WasmBlock type + import index constants
│   ├── offsets.rs    — RegOffsets (compile-time CPU struct layout)
│   └── builder.rs    — BlockBuilder: per-instruction translation logic
└── Cargo.toml
```

```
crates/lazuli-web/
├── src/
│   └── lib.rs        — wasm-bindgen glue: WasmEmulator, compile_block(), advance_timebase()
└── www/
    ├── index.html    — UI (register display, canvas, controls)
    └── bootstrap.js  — game loop, ISO parser, hooks, XFB renderer
```
