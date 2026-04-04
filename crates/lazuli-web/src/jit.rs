//! JIT compilation — block cache and PPC→WASM compile methods.
//!
//! [`WasmBlockCache`] stores compiled [`WebAssembly::Module`] objects keyed by
//! guest PC.  The [`WasmEmulator`] compile methods call [`ppcwasm::WasmJit`]
//! to produce WASM bytes and the browser's synchronous `new WebAssembly.Module`
//! constructor to compile them.

use std::collections::HashMap;

use gekko::disasm::{Extensions, Ins};
use js_sys::WebAssembly;
use wasm_bindgen::prelude::*;

use crate::WasmEmulator;

macro_rules! console_log {
    ($($t:tt)*) => {
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!($($t)*)))
    };
}

// ─── WasmBlockCache ──────────────────────────────────────────────────────────

/// Compiled and cached WASM modules, keyed by guest PC.
pub(crate) struct WasmBlockCache {
    pub(crate) modules: HashMap<u32, WebAssembly::Module>,
}

impl WasmBlockCache {
    pub(crate) fn new() -> Self {
        Self { modules: HashMap::new() }
    }

    /// Compile a [`WasmBlock`] into a [`WebAssembly::Module`] and cache it.
    ///
    /// `new WebAssembly.Module(bytes)` is synchronous and permitted inside
    /// WebWorker contexts (where the Lazuli emulator loop is intended to run).
    pub(crate) fn compile_and_cache(
        &mut self,
        pc: u32,
        block: &ppcwasm::WasmBlock,
    ) -> Result<&WebAssembly::Module, JsValue> {
        if !self.modules.contains_key(&pc) {
            let bytes = js_sys::Uint8Array::from(block.bytes.as_slice());
            let module = WebAssembly::Module::new(&bytes)?;
            self.modules.insert(pc, module);
        }
        Ok(self.modules.get(&pc).unwrap())
    }

    #[allow(dead_code)]
    pub(crate) fn get(&self, pc: u32) -> Option<&WebAssembly::Module> {
        self.modules.get(&pc)
    }

    pub(crate) fn invalidate(&mut self, pc: u32) {
        self.modules.remove(&pc);
    }

    pub(crate) fn len(&self) -> usize {
        self.modules.len()
    }
}

// ─── WasmEmulator — compilation methods ──────────────────────────────────────

#[wasm_bindgen]
impl WasmEmulator {
    /// Compile the PowerPC basic block starting at `guest_pc` and return its
    /// WASM bytecode as a [`Uint8Array`].
    ///
    /// This is the key step that mirrors the Play! emulator's dynarec-to-WASM
    /// pipeline: raw guest machine code is translated into a self-contained
    /// WASM binary module.
    ///
    /// The returned bytes can be passed directly to `WebAssembly.instantiate()`
    /// in JavaScript to obtain a callable block.
    pub fn compile_block(&mut self, guest_pc: u32) -> Result<js_sys::Uint8Array, JsValue> {
        let instructions = self.fetch_instructions(guest_pc);
        if instructions.is_empty() {
            console_log!("[lazuli] compile_block 0x{:08X}: no instructions in RAM", guest_pc);
            return Err(JsValue::from_str("no instructions at guest PC"));
        }

        let disasm: Vec<String> = instructions
            .iter()
            .map(|(pc, ins)| format!("0x{:08X}: {:?}", pc, ins.op))
            .collect();

        let block = self
            .jit
            .build(instructions.into_iter())
            .ok_or_else(|| JsValue::from_str("JIT produced no block"))?;

        if block.unimplemented_ops.is_empty() {
            console_log!(
                "[lazuli] block #{} @ 0x{:08X} ({} insns, {} WASM bytes): {}",
                self.blocks_compiled + 1,
                guest_pc,
                block.instruction_count,
                block.bytes.len(),
                disasm.join(" | "),
            );
        } else {
            console_log!(
                "[lazuli] block #{} @ 0x{:08X} ({} insns, {} WASM bytes) \
                 ⚠ UNIMPLEMENTED [{}]: {}",
                self.blocks_compiled + 1,
                guest_pc,
                block.instruction_count,
                block.bytes.len(),
                block.unimplemented_ops.join(", "),
                disasm.join(" | "),
            );
        }

        self.blocks_compiled += 1;
        self.last_compiled_pc = guest_pc;
        self.last_compiled_ins_count = block.instruction_count;
        self.last_compiled_wasm_bytes = block.bytes.len() as u32;
        if !block.unimplemented_ops.is_empty() {
            self.unimplemented_block_count += 1;
        }
        let bytes = js_sys::Uint8Array::from(block.bytes.as_slice());
        Ok(bytes)
    }

    /// Compile the block at `guest_pc` into the internal [`WasmBlockCache`]
    /// and return a JS object with metadata.
    pub fn compile_and_cache_block(&mut self, guest_pc: u32) -> Result<JsValue, JsValue> {
        let instructions = self.fetch_instructions(guest_pc);
        if instructions.is_empty() {
            return Err(JsValue::from_str("no instructions at guest PC"));
        }

        let block = self
            .jit
            .build(instructions.into_iter())
            .ok_or_else(|| JsValue::from_str("JIT produced no block"))?;

        let ins_count = block.instruction_count;
        let byte_len = block.bytes.len() as u32;

        self.cache.compile_and_cache(guest_pc, &block)?;
        self.blocks_compiled += 1;
        self.last_compiled_pc = guest_pc;
        self.last_compiled_ins_count = ins_count;
        self.last_compiled_wasm_bytes = byte_len;
        if !block.unimplemented_ops.is_empty() {
            self.unimplemented_block_count += 1;
        }

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"pc".into(), &JsValue::from(guest_pc))?;
        js_sys::Reflect::set(&obj, &"instructionCount".into(), &JsValue::from(ins_count))?;
        js_sys::Reflect::set(&obj, &"wasmByteLength".into(), &JsValue::from(byte_len))?;
        Ok(obj.into())
    }

    /// Build the import object descriptor required to instantiate a compiled block.
    ///
    /// Returns a JavaScript object with metadata that JS can use to wire up its
    /// own closures against the emulator's WASM linear memory.
    pub fn make_import_descriptor(&self) -> JsValue {
        let obj = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &obj,
            &"ramSize".into(),
            &JsValue::from(self.ram.len() as u32),
        );
        let _ = js_sys::Reflect::set(
            &obj,
            &"hookNames".into(),
            &{
                let arr = js_sys::Array::new();
                for name in &[
                    "read_u8",
                    "read_u16",
                    "read_u32",
                    "read_f64",
                    "write_u8",
                    "write_u16",
                    "write_u32",
                    "write_f64",
                    "raise_exception",
                ] {
                    arr.push(&JsValue::from_str(name));
                }
                arr.into()
            },
        );
        obj.into()
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

impl WasmEmulator {
    /// Fetch PowerPC instructions from guest RAM at `guest_pc`, stopping at
    /// the first terminal instruction or after at most 32 instructions.
    ///
    /// `guest_pc` may be a GameCube virtual address (`0x8xxxxxxx`) or a raw
    /// physical RAM offset; both are handled via [`crate::phys_addr`].
    pub(crate) fn fetch_instructions(&self, guest_pc: u32) -> Vec<(u32, Ins)> {
        let exts = Extensions::gekko_broadway();
        let mut out = Vec::new();

        for i in 0..32usize {
            let pc = guest_pc.wrapping_add((i * 4) as u32);
            let addr = crate::phys_addr(pc);
            if addr + 4 > self.ram.len() {
                break;
            }
            let word = u32::from_be_bytes([
                self.ram[addr],
                self.ram[addr + 1],
                self.ram[addr + 2],
                self.ram[addr + 3],
            ]);
            let ins = Ins::new(word, exts);
            let is_terminal = matches!(
                ins.op,
                gekko::disasm::Opcode::B
                    | gekko::disasm::Opcode::Bc
                    | gekko::disasm::Opcode::Bclr
                    | gekko::disasm::Opcode::Bcctr
                    | gekko::disasm::Opcode::Rfi
            );
            out.push((pc, ins));
            if is_terminal {
                break;
            }
        }

        out
    }
}
