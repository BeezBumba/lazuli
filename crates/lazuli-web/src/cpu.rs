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

    /// Get the primary (ps0) value of FPR[i].
    pub fn get_fpr(&self, i: u8) -> f64 {
        self.cpu.user.fpr[i as usize].0[0]
    }

    /// Get the paired-single (ps1) value of FPR[i].
    pub fn get_fpr_ps1(&self, i: u8) -> f64 {
        self.cpu.user.fpr[i as usize].0[1]
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

    /// Force-deliver a decrementer exception regardless of whether `MSR.EE`
    /// is set.
    ///
    /// On real PowerPC hardware the decrementer interrupt is maskable: it
    /// cannot fire while `EE = 0`.  However, if a JIT or emulation bug leaves
    /// the guest permanently stuck in a branch-to-self loop with `EE = 0` and
    /// `DEC < 0`, the normal `advance_decrementer` path can never break the
    /// deadlock.  The JavaScript host calls this method after detecting that
    /// threshold-many consecutive same-PC blocks have executed with `EE = 0`
    /// and the decrementer already expired, giving the OS decrementer handler
    /// a chance to run and reset `DEC` to a positive value.
    ///
    /// The method clears `decrementer_pending` and calls `raise_exception` with
    /// [`gekko::Exception::Decrementer`] (vector `0x00000900`) so that `SRR0`,
    /// `SRR1`, and `MSR` are updated exactly as they would be for a normal
    /// hardware decrementer interrupt.  This is the same exception type used by
    /// the `decrementer_pending && ee` branch in [`advance_decrementer`] and is
    /// distinct from [`gekko::Exception::Interrupt`] (vector `0x00000500`),
    /// which is reserved for external PI interrupts.
    pub fn force_decrementer_exception(&mut self) {
        self.decrementer_pending = false;
        self.cpu.raise_exception(gekko::Exception::Decrementer);
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

    /// Estimated CPU cycle count of the most recently compiled block.
    ///
    /// Mirrors [`ppcwasm::WasmBlock::cycles`], which is set to one cycle per
    /// PPC instruction (the same heuristic used by ppcjit's `Meta::cycles`).
    /// JavaScript should read this immediately after `compile_block` returns
    /// and store it per-PC so the game loop can advance the decrementer by
    /// the correct number of timebase ticks (`cycles / 12`) rather than a
    /// fixed per-block constant.
    pub fn last_compiled_cycles(&self) -> u32 {
        self.last_compiled_cycles
    }

    /// Advance the internal CPU cycle counter by `delta` emulated cycles.
    ///
    /// This mirrors the role of `Lazuli::exec`'s per-block cycle accumulator.
    /// JavaScript calls this after every block execution with the block's own
    /// cycle count so the counter stays accurate regardless of how many blocks
    /// happen to run per animation frame.
    pub fn add_cpu_cycles(&mut self, delta: u32) {
        self.cpu_cycles = self.cpu_cycles.wrapping_add(delta as u64);
    }

    /// Low 32 bits of the total emulated CPU cycle counter.
    pub fn cpu_cycles_lo(&self) -> u32 {
        self.cpu_cycles as u32
    }

    /// High 32 bits of the total emulated CPU cycle counter.
    pub fn cpu_cycles_hi(&self) -> u32 {
        (self.cpu_cycles >> 32) as u32
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

    /// Deliver a PowerPC exception by raw exception-vector offset.
    ///
    /// Called by JavaScript after a compiled WASM block's `raise_exception(kind)`
    /// hook fires and the block's CPU state has been synced back via
    /// [`set_cpu_bytes`].  Maps the numeric `kind` (which matches the
    /// [`gekko::Exception`] discriminant, e.g. `0x0C00` for Syscall) to the
    /// corresponding exception and calls [`gekko::Cpu::raise_exception`] to
    /// update `SRR0`, `SRR1`, `MSR`, and `PC` exactly as real hardware would.
    ///
    /// Returns `true` if the kind was recognised and the exception was
    /// delivered; `false` if the kind is unknown (no CPU state change).
    pub fn deliver_exception(&mut self, kind: i32) -> bool {
        use gekko::Exception;
        let exc = match kind as u32 {
            0x0100 => Exception::Reset,
            0x0200 => Exception::MachineCheck,
            0x0300 => Exception::DSI,
            0x0400 => Exception::ISI,
            0x0500 => Exception::Interrupt,
            0x0600 => Exception::Alignment,
            0x0700 => Exception::Program,
            0x0800 => Exception::FloatUnavailable,
            0x0900 => Exception::Decrementer,
            0x0C00 => Exception::Syscall,
            0x0D00 => Exception::Trace,
            0x0F00 => Exception::PerformanceMonitor,
            0x1300 => Exception::Breakpoint,
            _ => return false,
        };
        self.cpu.raise_exception(exc);
        true
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

    /// Serialize the complete emulator state to a byte array for savestate support.
    ///
    /// The returned [`js_sys::Uint8Array`] can be saved to `localStorage` or
    /// downloaded as a file, then passed back to [`load_state`] to restore.
    ///
    /// ## Format (little-endian binary)
    ///
    /// ```text
    ///   [4]  magic     = b"LAZU"
    ///   [4]  version   = 2
    ///   [4]  cpu_size  = size_of::<gekko::Cpu>()
    ///   [N]  cpu       = raw bytes of gekko::Cpu
    ///   [4]  ram_size  = self.ram.len()
    ///   [M]  ram       = self.ram
    ///   [4]  pi_intsr
    ///   [4]  pi_intmsk
    ///   [4]  pad_buttons
    ///   [4]  flags     = decrementer_pending as u32
    ///   [128] vi_regs  = 32 × u32 (raw VI register file)
    ///   [12] si_out    = 3 × u32 SI output buffers 0–2
    ///   [4]  si_out3   = SI output buffer 3
    ///   [16] si_in_hi  = 4 × u32 SI input buffer high words
    ///   [16] si_in_lo  = 4 × u32 SI input buffer low words
    ///   [4]  si_poll
    ///   [4]  si_comm_ctrl
    ///   [4]  si_status
    ///   [4]  ai_control
    ///   [4]  ai_volume
    ///   [4]  ai_sample_count
    ///   [4]  ai_interrupt_sample
    /// ```
    pub fn save_state(&self) -> js_sys::Uint8Array {
        let cpu_bytes = unsafe {
            std::slice::from_raw_parts(
                (&self.cpu as *const gekko::Cpu).cast::<u8>(),
                size_of::<gekko::Cpu>(),
            )
        };

        // Pre-calculate buffer size to avoid repeated reallocations.
        let total = 4 + 4              // magic + version
            + 4 + cpu_bytes.len()      // cpu_size + cpu
            + 4 + self.ram.len()       // ram_size + ram
            + 4 * 4                    // pi_intsr, pi_intmsk, pad_buttons, flags
            + 128                      // VI regs (32 × u32)
            + 4 * 4                    // SI out_buf (4 × u32)
            + 4 * 4                    // SI in_buf_hi (4 × u32)
            + 4 * 4                    // SI in_buf_lo (4 × u32)
            + 4 * 4                    // SI poll, comm_ctrl, status, exi_clock_lock
            + 4 * 4;                   // AI control, volume, sample_count, interrupt_sample

        let mut buf = Vec::with_capacity(total);

        // Header
        buf.extend_from_slice(b"LAZU");
        buf.extend_from_slice(&2u32.to_le_bytes());

        // CPU
        buf.extend_from_slice(&(cpu_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(cpu_bytes);

        // RAM
        buf.extend_from_slice(&(self.ram.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.ram);

        // PI registers
        buf.extend_from_slice(&self.pi_intsr.to_le_bytes());
        buf.extend_from_slice(&self.pi_intmsk.to_le_bytes());
        buf.extend_from_slice(&self.pad_buttons.to_le_bytes());
        buf.extend_from_slice(&(self.decrementer_pending as u32).to_le_bytes());

        // VI registers (32 × u32, stored as raw u32 LE)
        for i in 0..32usize {
            buf.extend_from_slice(&self.vi.read_u32((i as u32) * 4).to_le_bytes());
        }

        // SI registers
        for i in 0..4usize {
            buf.extend_from_slice(&self.si.read_u32(match i { 0=>0, 1=>0x0C, 2=>0x18, _=>0x24 }).to_le_bytes());
        }
        for i in 0..4usize {
            let base = match i { 0=>0x04, 1=>0x10, 2=>0x1C, _=>0x28 };
            buf.extend_from_slice(&self.si.read_u32(base).to_le_bytes());
        }
        for i in 0..4usize {
            let base = match i { 0=>0x08, 1=>0x14, 2=>0x20, _=>0x2C };
            buf.extend_from_slice(&self.si.read_u32(base).to_le_bytes());
        }
        buf.extend_from_slice(&self.si.read_u32(0x30).to_le_bytes()); // poll
        buf.extend_from_slice(&self.si.read_u32(0x34).to_le_bytes()); // comm_ctrl
        buf.extend_from_slice(&self.si.read_u32(0x38).to_le_bytes()); // status
        buf.extend_from_slice(&self.si.read_u32(0x3C).to_le_bytes()); // exi_clock_lock

        // AI registers
        buf.extend_from_slice(&self.ai.read_u32(0x00).to_le_bytes());
        buf.extend_from_slice(&self.ai.read_u32(0x04).to_le_bytes());
        buf.extend_from_slice(&self.ai.read_u32(0x08).to_le_bytes());
        buf.extend_from_slice(&self.ai.read_u32(0x0C).to_le_bytes());

        js_sys::Uint8Array::from(buf.as_slice())
    }

    /// Restore the complete emulator state from a savestate byte array.
    ///
    /// `data` must have been produced by a previous call to [`save_state`].
    /// Returns `true` on success, `false` on format mismatch.
    pub fn load_state(&mut self, data: &[u8]) -> bool {
        // Validate magic
        if data.len() < 8 || &data[0..4] != b"LAZU" {
            console_log!("[lazuli] load_state: invalid magic");
            return false;
        }
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap_or([0; 4]));
        if version < 2 {
            console_log!("[lazuli] load_state: unsupported version {}", version);
            return false;
        }

        let mut pos = 8usize;

        macro_rules! read_u32 {
            () => {{
                if pos + 4 > data.len() { return false; }
                let v = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap_or([0;4]));
                pos += 4;
                v
            }};
        }
        macro_rules! read_bytes {
            ($n:expr) => {{
                let n = $n as usize;
                if pos + n > data.len() { return false; }
                let s = &data[pos..pos+n];
                pos += n;
                s
            }};
        }

        // CPU
        let cpu_size = read_u32!() as usize;
        let cpu_bytes = read_bytes!(cpu_size);
        let expected_cpu = size_of::<gekko::Cpu>();
        if cpu_size != expected_cpu {
            console_log!("[lazuli] load_state: CPU size mismatch ({} != {})", cpu_size, expected_cpu);
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                cpu_bytes.as_ptr(),
                (&mut self.cpu as *mut gekko::Cpu).cast::<u8>(),
                expected_cpu,
            );
        }

        // RAM
        let ram_size = read_u32!() as usize;
        let ram_bytes = read_bytes!(ram_size);
        let copy_len = ram_size.min(self.ram.len());
        self.ram[..copy_len].copy_from_slice(&ram_bytes[..copy_len]);

        // PI registers
        self.pi_intsr = read_u32!();
        self.pi_intmsk = read_u32!();
        self.pad_buttons = read_u32!();
        self.decrementer_pending = read_u32!() != 0;

        // VI registers (32 × u32)
        for i in 0..32u32 {
            let val = read_u32!();
            self.vi.write_u32(i * 4, val);
        }

        // SI registers
        let si_out_offsets = [0u32, 0x0C, 0x18, 0x24];
        let si_hi_offsets  = [0x04u32, 0x10, 0x1C, 0x28];
        let si_lo_offsets  = [0x08u32, 0x14, 0x20, 0x2C];
        for &off in &si_out_offsets { let v = read_u32!(); self.si.write_u32(off, v, 0); }
        for &off in &si_hi_offsets  { let v = read_u32!(); self.si.write_u32(off, v, 0); }
        for &off in &si_lo_offsets  { let v = read_u32!(); self.si.write_u32(off, v, 0); }
        { let v = read_u32!(); self.si.write_u32(0x30, v, 0); }
        { let v = read_u32!(); self.si.write_u32(0x34, v, 0); }
        { let v = read_u32!(); self.si.write_u32(0x38, v, 0); }
        { let v = read_u32!(); self.si.write_u32(0x3C, v, 0); }

        // AI registers
        for off in [0x00u32, 0x04, 0x08, 0x0C] {
            let v = read_u32!();
            self.ai.write_u32(off, v);
        }

        // Invalidate JIT cache since RAM was restored.
        self.cache.clear();

        true
    }
}

#[wasm_bindgen]
impl WasmEmulator {
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
