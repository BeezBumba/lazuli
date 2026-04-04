//! CPU register access, serialisation, timebase, decrementer, and stats.

use std::mem::size_of;

use wasm_bindgen::prelude::*;

use crate::WasmEmulator;

macro_rules! console_log {
    ($($t:tt)*) => {
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!($($t)*)))
    };
}

#[wasm_bindgen]
impl WasmEmulator {
    // ── Register access ───────────────────────────────────────────────────────

    /// Set the program counter.
    pub fn set_pc(&mut self, pc: u32) {
        self.cpu.pc = gekko::Address(pc);
    }

    /// Get the current program counter.
    pub fn get_pc(&self) -> u32 {
        self.cpu.pc.0
    }

    /// Get GPR[i].
    pub fn get_gpr(&self, i: u8) -> u32 {
        self.cpu.user.gpr[i as usize]
    }

    /// Set GPR[i].
    pub fn set_gpr(&mut self, i: u8, value: u32) {
        self.cpu.user.gpr[i as usize] = value;
    }

    /// Current Link Register value.
    pub fn get_lr(&self) -> u32 {
        self.cpu.user.lr
    }

    /// Current Counter Register (CTR) value.
    pub fn get_ctr(&self) -> u32 {
        self.cpu.user.ctr
    }

    /// Current Machine State Register (MSR) as a raw 32-bit word.
    ///
    /// Bit 15 (`interrupts` / `EE`) is the external-interrupt enable flag.
    /// Check `(msr >> 15) & 1` to see if external interrupts are enabled.
    pub fn get_msr(&self) -> u32 {
        self.cpu.supervisor.config.msr.to_bits()
    }

    /// Overwrite the Machine State Register with a raw 32-bit value.
    ///
    /// Call this once after loading a DOL/ISO to establish the CPU state that
    /// the IPL ROM would normally leave before handing off to the game.  The
    /// most important bit to clear is **IP** (bit 6, `exception_prefix`):
    ///
    /// * `IP = 1` (default reset value): exception vectors at `0xFFF0xxxx` —
    ///   physical `0x01F00900` for the decrementer, which is **beyond** the
    ///   24 MiB GameCube RAM and therefore unexecutable.
    /// * `IP = 0`: exception vectors at `0x000xxxxx` — physical `0x00000900`
    ///   for the decrementer, which is within RAM and where the Dolphin OS
    ///   (`OSInit`) installs its exception handlers.
    ///
    /// Passing `0` clears all MSR bits (EE=0, FP=0, IP=0 …), which matches
    /// the state the IPL ROM leaves the CPU in before the game's `__start`
    /// runs `OSInit`.
    pub fn set_msr(&mut self, value: u32) {
        self.cpu.supervisor.config.msr = gekko::MachineState::from_bits(value);
    }

    /// Saved Restore Register 0 (SRR0) — the PC saved when the last exception fired.
    pub fn get_srr0(&self) -> u32 {
        self.cpu.supervisor.exception.srr[0]
    }

    /// Saved Restore Register 1 (SRR1) — the MSR saved when the last exception fired.
    pub fn get_srr1(&self) -> u32 {
        self.cpu.supervisor.exception.srr[1]
    }

    /// Current Decrementer (DEC) value (signed; goes negative when it expires).
    pub fn get_dec(&self) -> u32 {
        self.cpu.supervisor.misc.dec
    }

    // ── Controller input ──────────────────────────────────────────────────────

    /// Set the GameCube controller button bitmask.
    ///
    /// Called by the JavaScript keyboard handler on every `keydown` / `keyup`
    /// event.  The bitmask layout matches the `GC_BTN` constants defined in
    /// `bootstrap.js`.
    pub fn set_pad_buttons(&mut self, buttons: u32) {
        self.pad_buttons = buttons;
    }

    /// Get the current GameCube controller button bitmask.
    pub fn get_pad_buttons(&self) -> u32 {
        self.pad_buttons
    }

    // ── Timebase ──────────────────────────────────────────────────────────────

    /// Advance the CPU time-base register by `delta` ticks.
    ///
    /// The GameCube's Gekko time base increments at approximately 40.5 MHz
    /// (CPU clock / 12).  Call this once per animation frame so that
    /// time-base polling loops (`mftb` / `OSWaitVBlank`) see a monotonically
    /// increasing counter and do not spin forever.
    ///
    /// Suggested value: `675_000` ticks per frame (= 40.5 MHz / 60 fps).
    pub fn advance_timebase(&mut self, delta: u32) {
        self.cpu.supervisor.misc.tb =
            self.cpu.supervisor.misc.tb.wrapping_add(delta as u64);
    }

    /// Tick the decrementer down by `delta` ticks and deliver a decrementer
    /// exception if the result is negative and external interrupts are enabled.
    ///
    /// The interrupt request is **level-sensitive**: it is asserted whenever
    /// DEC < 0 and de-asserted as soon as the guest writes a non-negative value
    /// back to DEC via `mtspr DEC`.
    ///
    /// Call this once per JIT block (not just once per animation frame) so that
    /// the exception fires as soon as the guest enables `EE` inside a spin-wait
    /// loop.
    pub fn advance_decrementer(&mut self, delta: u32) {
        let old_dec = self.cpu.supervisor.misc.dec;
        let new_dec = old_dec.wrapping_sub(delta);
        self.cpu.supervisor.misc.dec = new_dec;

        self.decrementer_pending = (new_dec as i32) < 0;

        let ee = self.cpu.supervisor.config.msr.interrupts();

        if self.decrementer_pending && ee {
            self.decrementer_pending = false;
            self.cpu.raise_exception(gekko::Exception::Decrementer);
            return;
        }

        // Deliver any pending PI external interrupt (e.g. VI retrace) if EE=1
        // and the interrupt source is unmasked in PI_INTMSK.
        if ee {
            let pending = self.pi_intsr & self.pi_intmsk;
            if pending != 0 {
                self.cpu.raise_exception(gekko::Exception::Interrupt);
            }
        }
    }

    // ── Compilation stats ─────────────────────────────────────────────────────

    /// Number of distinct blocks that have been JIT-compiled to WASM.
    pub fn blocks_compiled(&self) -> u32 {
        self.blocks_compiled as u32
    }

    /// Number of blocks that have been executed.
    pub fn blocks_executed(&self) -> u32 {
        self.blocks_executed as u32
    }

    /// Notify the emulator that one compiled block has just been executed.
    pub fn record_block_executed(&mut self) {
        self.blocks_executed += 1;
    }

    /// Number of blocks currently in the module cache.
    pub fn cache_size(&self) -> u32 {
        self.cache.len() as u32
    }

    /// Guest PC of the most recently JIT-compiled block (0 if none compiled yet).
    pub fn last_compiled_pc(&self) -> u32 {
        self.last_compiled_pc
    }

    /// PPC instruction count of the most recently compiled block.
    pub fn last_compiled_ins_count(&self) -> u32 {
        self.last_compiled_ins_count
    }

    /// WASM byte length of the most recently compiled block.
    pub fn last_compiled_wasm_bytes(&self) -> u32 {
        self.last_compiled_wasm_bytes
    }

    /// Number of compiled blocks that contained at least one unimplemented opcode.
    pub fn unimplemented_block_count(&self) -> u32 {
        self.unimplemented_block_count as u32
    }

    /// Increment the `raise_exception` counter.
    pub fn record_raise_exception(&mut self) {
        self.raise_exception_count += 1;
    }

    /// Total number of `raise_exception` calls since emulator creation.
    pub fn raise_exception_count(&self) -> u32 {
        self.raise_exception_count as u32
    }

    /// Return a [`js_sys::Array`] containing the guest PC of every block
    /// currently held in the compiled-block cache (module cache).
    pub fn get_compiled_block_pcs(&self) -> js_sys::Array {
        let arr = js_sys::Array::new();
        for &pc in self.cache.modules.keys() {
            arr.push(&JsValue::from(pc));
        }
        arr
    }

    // ── Memory views ──────────────────────────────────────────────────────────

    /// Returns a raw pointer (WASM linear memory offset) to the start of the
    /// guest RAM buffer.
    ///
    /// Combine with [`wasm_memory`] and [`ram_size`] to create a live,
    /// zero-copy JavaScript view:
    ///
    /// ```js
    /// const ram = new Uint8Array(wasm_memory().buffer, emu.ram_ptr(), emu.ram_size());
    /// ```
    pub fn ram_ptr(&self) -> u32 {
        self.ram.as_ptr() as u32
    }

    /// Returns the size of the guest RAM buffer in bytes.
    pub fn ram_size(&self) -> u32 {
        self.ram.len() as u32
    }

    // ── CPU struct serialisation ──────────────────────────────────────────────

    /// Size in bytes of the [`gekko::Cpu`] struct.
    pub fn cpu_struct_size(&self) -> u32 {
        size_of::<gekko::Cpu>() as u32
    }

    /// Serialise the current CPU register state into a [`js_sys::Uint8Array`].
    ///
    /// The returned bytes match the `#[repr(C)]` in-memory layout of
    /// [`gekko::Cpu`].  Write them to offset 0 of the `env.memory` WASM
    /// memory before calling `execute(0)` on a compiled block.
    pub fn get_cpu_bytes(&self) -> js_sys::Uint8Array {
        // SAFETY: `gekko::Cpu` is `#[repr(C)]` and contains only plain integer /
        // float fields.  We borrow it as a byte slice for the duration of this
        // call, which is safe.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&self.cpu as *const gekko::Cpu).cast::<u8>(),
                size_of::<gekko::Cpu>(),
            )
        };
        js_sys::Uint8Array::from(bytes)
    }

    /// Restore the CPU register state from raw bytes.
    ///
    /// `data` must have been produced by a previous call to [`get_cpu_bytes`]
    /// and must therefore have length exactly [`cpu_struct_size`] bytes.  Call
    /// this after `execute()` returns to sync the register changes made by the
    /// compiled block back into the Rust emulator.
    pub fn set_cpu_bytes(&mut self, data: &[u8]) {
        let expected = size_of::<gekko::Cpu>();
        if data.len() != expected {
            console_log!(
                "[lazuli-web] set_cpu_bytes: expected {} bytes, got {}",
                expected,
                data.len()
            );
            return;
        }
        // SAFETY: `data` has the correct size and alignment is guaranteed by
        // the `#[repr(C)]` layout.  The source slice is valid for the duration
        // of the copy.
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (&mut self.cpu as *mut gekko::Cpu).cast::<u8>(),
                expected,
            );
        }
    }

    /// Return a snapshot of the emulator's guest RAM as a [`js_sys::Uint8Array`].
    pub fn get_ram_copy(&self) -> js_sys::Uint8Array {
        js_sys::Uint8Array::from(self.ram.as_slice())
    }

    /// Copy `data` back over the emulator's guest RAM.
    pub fn sync_ram(&mut self, data: &[u8]) {
        let len = data.len().min(self.ram.len());
        self.ram[..len].copy_from_slice(&data[..len]);
    }

    /// Return the byte offsets of key CPU registers within the [`gekko::Cpu`]
    /// struct as a JavaScript object.
    ///
    /// JavaScript can use these offsets to directly read / write individual
    /// registers in the WASM memory buffer that holds the serialised CPU state.
    pub fn get_reg_offsets(&self) -> JsValue {
        let offsets = self.jit.offsets();
        let obj = js_sys::Object::new();
        let set = |key: &str, val: u64| {
            let _ = js_sys::Reflect::set(&obj, &key.into(), &JsValue::from(val as u32));
        };
        set("pc", offsets.pc);
        set("lr", offsets.lr);
        set("ctr", offsets.ctr);
        set("cr", offsets.cr);
        set("xer", offsets.xer);
        set("srr0", offsets.srr0);
        set("srr1", offsets.srr1);
        set("dec", offsets.dec);
        let gpr_arr = js_sys::Array::new();
        for &off in &offsets.gpr {
            gpr_arr.push(&JsValue::from(off as u32));
        }
        let _ = js_sys::Reflect::set(&obj, &"gpr".into(), &gpr_arr.into());
        let sprg_arr = js_sys::Array::new();
        for &off in &offsets.sprg {
            sprg_arr.push(&JsValue::from(off as u32));
        }
        let _ = js_sys::Reflect::set(&obj, &"sprg".into(), &sprg_arr.into());
        obj.into()
    }
}
