/* @ts-self-types="./lazuli_web.d.ts" */

/**
 * An opaque, clone-able handle to the `renderer::Renderer` instance owned by
 * a [`WgpuRenderer`].
 *
 * Exported to JavaScript so that the game loop can hand it to
 * [`WasmEmulator::attach_gx_renderer`], which allows the GX FIFO parser to
 * dispatch draw actions directly to the GPU renderer as the emulator runs.
 *
 * ```js
 * const wgpuRenderer = await init_webgpu_renderer("gc-canvas");
 * emu.attach_gx_renderer(wgpuRenderer.gx_renderer_handle());
 * ```
 */
export class GxRendererHandle {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(GxRendererHandle.prototype);
        obj.__wbg_ptr = ptr;
        GxRendererHandleFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        GxRendererHandleFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_gxrendererhandle_free(ptr, 0);
    }
}
if (Symbol.dispose) GxRendererHandle.prototype[Symbol.dispose] = GxRendererHandle.prototype.free;

/**
 * GameCube emulator running entirely in the browser via WebAssembly.
 *
 * Exported to JavaScript via `wasm-bindgen`.  The emulator maintains a
 * [`gekko::Cpu`] register file and a flat RAM array.  Compiled PPC blocks are
 * cached as [`WebAssembly::Module`]s and instantiated on demand with
 * JavaScript hook closures for guest memory access.
 */
export class WasmEmulator {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmEmulatorFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmemulator_free(ptr, 0);
    }
    /**
     * Advance the internal CPU cycle counter by `delta` emulated cycles.
     *
     * This mirrors the role of `Lazuli::exec`'s per-block cycle accumulator.
     * JavaScript calls this after every block execution with the block's own
     * cycle count so the counter stays accurate regardless of how many blocks
     * happen to run per animation frame.
     * @param {number} delta
     */
    add_cpu_cycles(delta) {
        wasm.wasmemulator_add_cpu_cycles(this.__wbg_ptr, delta);
    }
    /**
     * Advance the Audio Interface (AI) sample counter by `cpu_cycles`.
     *
     * Call this from the JavaScript game loop after every executed block,
     * passing the block's CPU cycle count (`blockMeta.cycles`).  Internally
     * the method accumulates cycles across blocks and increments the AI sample
     * counter (`AISCNT`) once per audio sample period (10 125 CPU cycles at
     * 48 kHz; 15 187 at 32 kHz, selected by `AICR.AISFR`).
     *
     * When `AISCNT` crosses `AIIT` (the interrupt threshold), the AI
     * interrupt fires: `PI_INT_AI` is set in `PI_INTSR` and
     * [`maybe_deliver_external_interrupt`] is called immediately.
     *
     * Returns `true` when the AI interrupt fires (informational only —
     * interrupt delivery is handled automatically).
     *
     * Mirrors the native `ai::push_streaming_frame` scheduler event which
     * increments `sample_counter` and calls `pi::check_interrupts` at the
     * audio sample rate.
     * @param {number} cpu_cycles
     * @returns {boolean}
     */
    advance_ai(cpu_cycles) {
        const ret = wasm.wasmemulator_advance_ai(this.__wbg_ptr, cpu_cycles);
        return ret !== 0;
    }
    /**
     * Tick the decrementer down by `delta` ticks and deliver a decrementer
     * exception if a new underflow occurred and external interrupts are enabled.
     *
     * The Gekko hardware fires the decrementer interrupt on the **edge** when
     * DEC transitions from non-negative to negative (bit 31 goes from 0 to 1).
     * Subsequent calls while DEC is still negative do **not** re-assert the
     * interrupt — the guest OS handler is responsible for writing a new
     * positive value to DEC via `mtspr DEC` to re-arm the timer.
     *
     * PI external interrupts are intentionally **not** delivered here.
     * JavaScript must call [`maybe_deliver_external_interrupt`] whenever MSR.EE
     * transitions from 0 to 1 (e.g. after `rfi` or `mtmsr`), mirroring the
     * native JIT's `msr_changed` → `schedule_now(pi::check_interrupts)` hook.
     *
     * Call this once per JIT block (not just once per animation frame) so that
     * the decrementer exception fires promptly inside spin-wait loops.
     * @param {number} delta
     */
    advance_decrementer(delta) {
        wasm.wasmemulator_advance_decrementer(this.__wbg_ptr, delta);
    }
    /**
     * Advance the CPU time-base register by `delta` ticks.
     *
     * The GameCube's Gekko time base increments at approximately 40.5 MHz
     * (CPU clock / 12).  Call this once per animation frame so that
     * time-base polling loops (`mftb` / `OSWaitVBlank`) see a monotonically
     * increasing counter and do not spin forever.
     *
     * Suggested value: `675_000` ticks per frame (= 40.5 MHz / 60 fps).
     * @param {number} delta
     */
    advance_timebase(delta) {
        wasm.wasmemulator_advance_timebase(this.__wbg_ptr, delta);
    }
    /**
     * Assert a Video Interface (VI) vertical-retrace interrupt.
     *
     * Call this **once per animation frame** (~60 Hz) from the JavaScript
     * game loop.  The function advances the vertical counter, fires any
     * enabled VI DisplayInterrupt sources (setting their status bits so the
     * OS handler can identify which source fired), sets the VI bit in
     * `PI_INTSR`, and — if the guest CPU has external interrupts enabled
     * (`MSR.EE = 1`) and the OS has unmasked the VI interrupt in `PI_INTMSK`
     * — delivers an `Exception::Interrupt` (vector `0x00000500`) to the CPU.
     *
     * Additionally fires the PE_FINISH interrupt once per frame.  On real
     * hardware PE_FINISH is generated by the GPU when it finishes processing
     * a draw-done command from the GX FIFO.  Without a full GX pipeline the
     * browser build fires it alongside the VI retrace to unblock games that
     * call `GXWaitForDrawDone()` as a frame-sync primitive.
     */
    assert_vi_interrupt() {
        wasm.wasmemulator_assert_vi_interrupt(this.__wbg_ptr);
    }
    /**
     * Attach a WebGPU renderer so the GX FIFO parser can dispatch draw
     * actions directly to the GPU.
     *
     * Call this once after [`init_webgpu_renderer`] succeeds:
     *
     * ```js
     * const wgpuRenderer = await init_webgpu_renderer("gc-canvas");
     * emu.attach_gx_renderer(wgpuRenderer.gx_renderer_handle());
     * ```
     *
     * The handle shares the same internal `Arc` as the `WgpuRenderer`, so
     * actions enqueued by the emulator are visible to `present_xfb`.
     * @param {GxRendererHandle} handle
     */
    attach_gx_renderer(handle) {
        _assertClass(handle, GxRendererHandle);
        var ptr0 = handle.__destroy_into_raw();
        wasm.wasmemulator_attach_gx_renderer(this.__wbg_ptr, ptr0);
    }
    /**
     * Number of distinct blocks that have been JIT-compiled to WASM.
     * @returns {number}
     */
    blocks_compiled() {
        const ret = wasm.wasmemulator_blocks_compiled(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Number of blocks that have been executed.
     * @returns {number}
     */
    blocks_executed() {
        const ret = wasm.wasmemulator_blocks_executed(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Number of blocks currently in the module cache.
     * @returns {number}
     */
    cache_size() {
        const ret = wasm.wasmemulator_cache_size(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Compile the block at `guest_pc` into the internal [`WasmBlockCache`]
     * and return a JS object with metadata.
     * @param {number} guest_pc
     * @returns {any}
     */
    compile_and_cache_block(guest_pc) {
        const ret = wasm.wasmemulator_compile_and_cache_block(this.__wbg_ptr, guest_pc);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Compile the PowerPC basic block starting at `guest_pc` and return its
     * WASM bytecode as a [`Uint8Array`].
     *
     * This is the key step that mirrors the Play! emulator's dynarec-to-WASM
     * pipeline: raw guest machine code is translated into a self-contained
     * WASM binary module.
     *
     * The returned bytes can be passed directly to `WebAssembly.instantiate()`
     * in JavaScript to obtain a callable block.
     * @param {number} guest_pc
     * @returns {Uint8Array}
     */
    compile_block(guest_pc) {
        const ret = wasm.wasmemulator_compile_block(this.__wbg_ptr, guest_pc);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * High 32 bits of the total emulated CPU cycle counter.
     * @returns {number}
     */
    cpu_cycles_hi() {
        const ret = wasm.wasmemulator_cpu_cycles_hi(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Low 32 bits of the total emulated CPU cycle counter.
     * @returns {number}
     */
    cpu_cycles_lo() {
        const ret = wasm.wasmemulator_cpu_cycles_lo(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Size in bytes of the [`gekko::Cpu`] struct.
     * @returns {number}
     */
    cpu_struct_size() {
        const ret = wasm.wasmemulator_cpu_struct_size(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Deliver a PowerPC exception by raw exception-vector offset.
     *
     * Called by JavaScript after a compiled WASM block's `raise_exception(kind)`
     * hook fires and the block's CPU state has been synced back via
     * [`set_cpu_bytes`].  Maps the numeric `kind` (which matches the
     * [`gekko::Exception`] discriminant, e.g. `0x0C00` for Syscall) to the
     * corresponding exception and calls [`gekko::Cpu::raise_exception`] to
     * update `SRR0`, `SRR1`, `MSR`, and `PC` exactly as real hardware would.
     *
     * Returns `true` if the kind was recognised and the exception was
     * delivered; `false` if the kind is unknown (no CPU state change).
     * @param {number} kind
     * @returns {boolean}
     */
    deliver_exception(kind) {
        const ret = wasm.wasmemulator_deliver_exception(this.__wbg_ptr, kind);
        return ret !== 0;
    }
    /**
     * Read the Audio Interface Control Register (AICR).
     *
     * JavaScript uses bit 1 (AISFR) to select the audio sample rate
     * (0 = 48 kHz, 1 = 32 kHz) for cycle-accurate AI scheduling and
     * per-frame sample generation.
     * @returns {number}
     */
    get_ai_control() {
        const ret = wasm.wasmemulator_get_ai_control(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Return a [`js_sys::Array`] containing the guest PC of every block
     * currently held in the compiled-block cache (module cache).
     * @returns {Array<any>}
     */
    get_compiled_block_pcs() {
        const ret = wasm.wasmemulator_get_compiled_block_pcs(this.__wbg_ptr);
        return ret;
    }
    /**
     * Serialise the current CPU register state into a [`js_sys::Uint8Array`].
     *
     * The returned bytes match the `#[repr(C)]` in-memory layout of
     * [`gekko::Cpu`].  Write them to offset 0 of the `env.memory` WASM
     * memory before calling `execute(0)` on a compiled block.
     * @returns {Uint8Array}
     */
    get_cpu_bytes() {
        const ret = wasm.wasmemulator_get_cpu_bytes(this.__wbg_ptr);
        return ret;
    }
    /**
     * Current Condition Register (CR) as a raw 32-bit word.
     *
     * The CR is split into eight 4-bit fields CR0–CR7 (CR0 occupies the
     * most-significant nibble, CR7 the least-significant). Each field holds
     * the LT, GT, EQ, and SO comparison flags produced by integer compare
     * instructions or the `Rc` update path.
     * @returns {number}
     */
    get_cr() {
        const ret = wasm.wasmemulator_get_cr(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Current Counter Register (CTR) value.
     * @returns {number}
     */
    get_ctr() {
        const ret = wasm.wasmemulator_get_ctr(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Return the lower 32-bit word of a Data BAT register.
     *
     * `n` is 0–3 for DBAT0L–DBAT3L.
     * @param {number} n
     * @returns {number}
     */
    get_dbat_l(n) {
        const ret = wasm.wasmemulator_get_dbat_l(this.__wbg_ptr, n);
        return ret >>> 0;
    }
    /**
     * Return the upper 32-bit word of a Data BAT register.
     *
     * `n` is 0–3 for DBAT0U–DBAT3U.  These are stored in
     * `cpu.supervisor.memory.dbat[n]` (bits 32–63 of the packed 64-bit
     * `Bat` value, as recorded by `mtspr DBAT0U` etc.).
     *
     * JavaScript reads this alongside [`get_dbat_l`] to implement BAT address
     * translation in the MMIO hook fallback path.
     * @param {number} n
     * @returns {number}
     */
    get_dbat_u(n) {
        const ret = wasm.wasmemulator_get_dbat_u(this.__wbg_ptr, n);
        return ret >>> 0;
    }
    /**
     * Current Decrementer (DEC) value (signed; goes negative when it expires).
     * @returns {number}
     */
    get_dec() {
        const ret = wasm.wasmemulator_get_dec(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Get the primary (ps0) value of FPR[i].
     * @param {number} i
     * @returns {number}
     */
    get_fpr(i) {
        const ret = wasm.wasmemulator_get_fpr(this.__wbg_ptr, i);
        return ret;
    }
    /**
     * Get GPR[i].
     * @param {number} i
     * @returns {number}
     */
    get_gpr(i) {
        const ret = wasm.wasmemulator_get_gpr(this.__wbg_ptr, i);
        return ret >>> 0;
    }
    /**
     * Return the lower 32-bit word of an Instruction BAT register.
     *
     * `n` is 0–3 for IBAT0L–IBAT3L.
     * @param {number} n
     * @returns {number}
     */
    get_ibat_l(n) {
        const ret = wasm.wasmemulator_get_ibat_l(this.__wbg_ptr, n);
        return ret >>> 0;
    }
    /**
     * Return the upper 32-bit word of an Instruction BAT register.
     *
     * `n` is 0–3 for IBAT0U–IBAT3U.
     * @param {number} n
     * @returns {number}
     */
    get_ibat_u(n) {
        const ret = wasm.wasmemulator_get_ibat_u(this.__wbg_ptr, n);
        return ret >>> 0;
    }
    /**
     * Current Link Register value.
     * @returns {number}
     */
    get_lr() {
        const ret = wasm.wasmemulator_get_lr(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Return the raw 512 KiB memory card data for the given slot.
     *
     * - `slot = 0`: EXI channel 0, slot A (standard card port).
     * - `slot = 1`: EXI channel 1, slot B.
     *
     * JavaScript should persist this data in `localStorage` (or OPFS for
     * larger cards) and call [`set_memcard_data`] on startup to restore it.
     * @param {number} slot
     * @returns {Uint8Array}
     */
    get_memcard_data(slot) {
        const ret = wasm.wasmemulator_get_memcard_data(this.__wbg_ptr, slot);
        return ret;
    }
    /**
     * Current Machine State Register (MSR) as a raw 32-bit word.
     *
     * Bit 15 (`interrupts` / `EE`) is the external-interrupt enable flag.
     * Check `(msr >> 15) & 1` to see if external interrupts are enabled.
     * @returns {number}
     */
    get_msr() {
        const ret = wasm.wasmemulator_get_msr(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Get the current GameCube controller button bitmask.
     * @returns {number}
     */
    get_pad_buttons() {
        const ret = wasm.wasmemulator_get_pad_buttons(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Get the current program counter.
     * @returns {number}
     */
    get_pc() {
        const ret = wasm.wasmemulator_get_pc(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Return a snapshot of the emulator's guest RAM as a [`js_sys::Uint8Array`].
     * @returns {Uint8Array}
     */
    get_ram_copy() {
        const ret = wasm.wasmemulator_get_ram_copy(this.__wbg_ptr);
        return ret;
    }
    /**
     * Return the byte offsets of key CPU registers within the [`gekko::Cpu`]
     * struct as a JavaScript object.
     *
     * JavaScript can use these offsets to directly read / write individual
     * registers in the WASM memory buffer that holds the serialised CPU state.
     * @returns {any}
     */
    get_reg_offsets() {
        const ret = wasm.wasmemulator_get_reg_offsets(this.__wbg_ptr);
        return ret;
    }
    /**
     * Returns the 64-byte SRAM contents as a `Uint8Array`.
     *
     * JavaScript should persist these bytes in `localStorage` and call
     * [`set_sram`] on startup to restore saved settings (language, sound mode,
     * etc.), mirroring the native emulator's on-disk SRAM persistence.
     * @returns {Uint8Array}
     */
    get_sram() {
        const ret = wasm.wasmemulator_get_sram(this.__wbg_ptr);
        return ret;
    }
    /**
     * Saved Restore Register 0 (SRR0) — the PC saved when the last exception fired.
     * @returns {number}
     */
    get_srr0() {
        const ret = wasm.wasmemulator_get_srr0(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Saved Restore Register 1 (SRR1) — the MSR saved when the last exception fired.
     * @returns {number}
     */
    get_srr1() {
        const ret = wasm.wasmemulator_get_srr1(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Read a 16-bit value from a GameCube hardware register.
     *
     * Called by the JavaScript `read_u16` hook when the guest address has
     * the prefix `0xCC` or `0xCD`.
     * @param {number} addr
     * @returns {number}
     */
    hw_read_u16(addr) {
        const ret = wasm.wasmemulator_hw_read_u16(this.__wbg_ptr, addr);
        return ret;
    }
    /**
     * Read a 32-bit value from a GameCube hardware register.
     *
     * Called by the JavaScript `read_u32` hook when the guest address has
     * the prefix `0xCC` or `0xCD` (GameCube memory-mapped I/O space),
     * **before** the `PHYS_MASK` is applied.  This is necessary because
     * applying `addr & 0x01FFFFFF` to `0xCC006008` yields `0x00006008`,
     * which would silently alias into guest RAM instead of the DVD Interface
     * registers.
     *
     * Both `0xCCxxxxxx` (cached) and `0xCDxxxxxx` (uncached) aliases are
     * normalised to the `0xCC` base before dispatching.
     * @param {number} addr
     * @returns {number}
     */
    hw_read_u32(addr) {
        const ret = wasm.wasmemulator_hw_read_u32(this.__wbg_ptr, addr);
        return ret >>> 0;
    }
    /**
     * Read an 8-bit value from a GameCube hardware register.
     *
     * Most MMIO registers are 16- or 32-bit wide; 8-bit reads are unusual
     * but the OS sometimes uses `lbz` to check single-byte status fields.
     * Returns the appropriate byte from the containing 32-bit register.
     * @param {number} addr
     * @returns {number}
     */
    hw_read_u8(addr) {
        const ret = wasm.wasmemulator_hw_read_u8(this.__wbg_ptr, addr);
        return ret;
    }
    /**
     * Write a 16-bit value to a GameCube hardware register.
     *
     * Called by the JavaScript `write_u16` hook when the guest address has
     * the prefix `0xCC` or `0xCD`.
     * @param {number} addr
     * @param {number} val
     */
    hw_write_u16(addr, val) {
        wasm.wasmemulator_hw_write_u16(this.__wbg_ptr, addr, val);
    }
    /**
     * Write a 32-bit value to a GameCube hardware register.
     *
     * Called by the JavaScript `write_u32` hook when the guest address has
     * the prefix `0xCC` or `0xCD`, before `PHYS_MASK` is applied.
     * @param {number} addr
     * @param {number} val
     */
    hw_write_u32(addr, val) {
        wasm.wasmemulator_hw_write_u32(this.__wbg_ptr, addr, val);
    }
    /**
     * Write an 8-bit value to a GameCube hardware register.
     *
     * Reads the containing 32-bit register, merges the byte, and writes back.
     * @param {number} addr
     * @param {number} val
     */
    hw_write_u8(addr, val) {
        wasm.wasmemulator_hw_write_u8(this.__wbg_ptr, addr, val);
    }
    /**
     * Returns the WASM linear-memory pointer to the L2 cache-as-RAM buffer.
     *
     * The L2 cache region is 16 KiB and corresponds to guest addresses
     * `0xE000_0000`–`0xE003_FFFF`.  JavaScript uses this pointer to create a
     * `Uint8Array` view and services reads/writes to those addresses directly,
     * matching the native emulator's `0xE000_0000` L2 cache-RAM region.
     *
     * ```js
     * const l2c = new Uint8Array(wasm_memory().buffer, emu.l2c_ptr(), emu.l2c_size());
     * ```
     * @returns {number}
     */
    l2c_ptr() {
        const ret = wasm.wasmemulator_l2c_ptr(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Returns the size of the L2 cache-as-RAM buffer in bytes (always 16 KiB).
     * @returns {number}
     */
    l2c_size() {
        const ret = wasm.wasmemulator_l2c_size(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Estimated CPU cycle count of the most recently compiled block.
     *
     * Mirrors [`ppcwasm::WasmBlock::cycles`], which is set to one cycle per
     * PPC instruction (the same heuristic used by ppcjit's `Meta::cycles`).
     * JavaScript should read this immediately after `compile_block` returns
     * and store it per-PC so the game loop can advance the decrementer by
     * the correct number of timebase ticks (`cycles / 12`) rather than a
     * fixed per-block constant.
     * @returns {number}
     */
    last_compiled_cycles() {
        const ret = wasm.wasmemulator_last_compiled_cycles(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * PPC instruction count of the most recently compiled block.
     * @returns {number}
     */
    last_compiled_ins_count() {
        const ret = wasm.wasmemulator_last_compiled_ins_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Guest PC of the most recently JIT-compiled block (0 if none compiled yet).
     * @returns {number}
     */
    last_compiled_pc() {
        const ret = wasm.wasmemulator_last_compiled_pc(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * WASM byte length of the most recently compiled block.
     * @returns {number}
     */
    last_compiled_wasm_bytes() {
        const ret = wasm.wasmemulator_last_compiled_wasm_bytes(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Disc byte offset of the most recent successful DVD Read (0xA8) DMA.
     *
     * JavaScript reads this after [`take_dma_dirty`] returns `true` to
     * format the `[lazuli] DI: DVD Read` diagnostic line in the apploader-log
     * panel, mirroring the same message logged to the browser console by
     * `process_di_command`.
     * @returns {number}
     */
    last_di_disc_offset() {
        const ret = wasm.wasmemulator_last_di_disc_offset(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Physical start address (in emulator RAM) of the most recent successful
     * DVD DMA transfer.
     * @returns {number}
     */
    last_dma_addr() {
        const ret = wasm.wasmemulator_last_dma_addr(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Byte length of the most recent successful DVD DMA transfer.
     * @returns {number}
     */
    last_dma_len() {
        const ret = wasm.wasmemulator_last_dma_len(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Copy `data` into guest RAM starting at `guest_addr`.
     *
     * `guest_addr` may be a GameCube virtual address (`0x8xxxxxxx`) or a raw
     * physical offset; both are handled transparently via the same
     * `0x01FF_FFFF` mask used by [`crate::phys_addr`].
     *
     * Clears the block cache for any PC that overlaps the written region, so
     * that stale compiled blocks are not executed after a ROM reload.
     * @param {number} guest_addr
     * @param {Uint8Array} data
     */
    load_bytes(guest_addr, data) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmemulator_load_bytes(this.__wbg_ptr, guest_addr, ptr0, len0);
    }
    /**
     * Store a raw GameCube ISO disc image for runtime sector reads.
     *
     * This is the Rust counterpart of Play!'s `DiscImageDevice.ts` +
     * `Js_DiscImageDeviceStream.cpp`: once the ISO bytes are stored here the
     * emulated DVD Interface can service `A8h` (DVD Read) DMA commands during
     * gameplay, so that games that stream textures, audio, or level data from
     * disc continue to work after the initial boot DOL has been loaded.
     *
     * Both raw ISO and CISO (Compact ISO) formats are accepted.  CISO images
     * are detected by the `"CISO"` magic at byte 0 and decompressed to a flat
     * buffer before storage so the runtime DVD-read path can use plain byte
     * slicing without format-awareness.
     *
     * Call this in addition to (not instead of) the DOL-loading path in
     * `parseAndLoadIso` / `load_bytes`.
     * @param {Uint8Array} data
     */
    load_disc_image(data) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmemulator_load_disc_image(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Load an ipl-hle DOL into guest RAM and return its entry point.
     *
     * `data` must contain the raw bytes of a GameCube DOL file built from
     * `crates/ipl-hle/` (via `just ipl-hle build`).  In the browser the
     * caller fetches `ipl-hle.dol` from the same origin and passes the
     * resulting `Uint8Array` here; nothing is embedded in the WASM binary.
     *
     * The DOL header layout (all fields big-endian u32):
     *   0x000  text_offsets[7]   — file offset of each .text section
     *   0x01C  data_offsets[11]  — file offset of each .data section
     *   0x048  text_targets[7]   — guest load address of each .text section
     *   0x064  data_targets[11]  — guest load address of each .data section
     *   0x090  text_sizes[7]     — size in bytes of each .text section
     *   0x0AC  data_sizes[11]    — size in bytes of each .data section
     *   0x0D8  bss_target        — guest address of BSS region
     *   0x0DC  bss_size          — size of BSS region
     *   0x0E0  entry             — entry-point guest address
     *
     * After loading, callers must set `gpr[3]` to the real apploader's entry
     * function address (read from the ISO apploader header at offset `+0x10`)
     * so that ipl-hle's `main(entry)` receives it as its first argument,
     * matching what the native `load_ipl_hle()` does.
     *
     * Returns the ipl-hle entry point (e.g. `0x81300000`).
     * @param {Uint8Array} data
     * @returns {number}
     */
    load_ipl_hle(data) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmemulator_load_ipl_hle(this.__wbg_ptr, ptr0, len0);
        return ret >>> 0;
    }
    /**
     * Restore the complete emulator state from a savestate byte array.
     *
     * `data` must have been produced by a previous call to [`save_state`].
     * Returns `true` on success, `false` on format mismatch.
     * @param {Uint8Array} data
     * @returns {boolean}
     */
    load_state(data) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmemulator_load_state(this.__wbg_ptr, ptr0, len0);
        return ret !== 0;
    }
    /**
     * Build the import object descriptor required to instantiate a compiled block.
     *
     * Returns a JavaScript object with metadata that JS can use to wire up its
     * own closures against the emulator's WASM linear memory.
     * @returns {any}
     */
    make_import_descriptor() {
        const ret = wasm.wasmemulator_make_import_descriptor(this.__wbg_ptr);
        return ret;
    }
    /**
     * Deliver an external interrupt to the CPU if EE=1 and any enabled
     * interrupt is pending in `PI_INTSR & PI_INTMSK`.
     *
     * Call this from JavaScript whenever MSR.EE transitions from 0 to 1
     * (e.g. after `rfi` or `mtmsr` restores EE).  This mirrors the native
     * JIT's `msr_changed` → `schedule_now(pi::check_interrupts)` hook, which
     * only re-checks pending interrupts on an actual MSR-change event — not
     * on every single JIT block.
     *
     * Also called internally whenever a PI_INTSR bit is asserted (VI, DI, SI,
     * …) so the interrupt fires immediately if EE is already enabled.
     */
    maybe_deliver_external_interrupt() {
        wasm.wasmemulator_maybe_deliver_external_interrupt(this.__wbg_ptr);
    }
    /**
     * Create a new emulator with `ram_size` bytes of guest RAM.
     *
     * `ram_size` must be a multiple of 65536 (one WASM memory page).
     * For a full GameCube emulation pass `24 * 1024 * 1024` (24 MiB).
     * @param {number} ram_size
     */
    constructor(ram_size) {
        const ret = wasm.wasmemulator_new(ram_size);
        this.__wbg_ptr = ret >>> 0;
        WasmEmulatorFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Parse a GameCube disc image (raw ISO or CISO), load the apploader into
     * guest RAM, and store the disc for runtime DVD reads.
     *
     * This method consolidates the disc-loading logic that was previously
     * split between the JavaScript `parseAndLoadIso` function and the
     * [`load_disc_image`] method, moving all format-aware parsing into Rust:
     *
     * 1. **Format detection** — CISO images are decompressed to a flat buffer.
     * 2. **Header validation** — the GameCube magic word (`0xC2339F3D` at
     *    offset `0x1C`) is verified.
     * 3. **Disc header** — the first `0x440` bytes (ISO header + boot info)
     *    are copied to guest RAM at `0x8000_0000`.
     * 4. **Dolphin OS globals** — standard boot-time values are written to
     *    `0x8000_0020`–`0x8000_00FF`, matching what the native IPL ROM writes
     *    before transferring control to the apploader.
     * 5. **Apploader** — the apploader body is loaded at `0x8120_0000`.
     * 6. **Boot DOL** — the header is parsed to read the entry point; sections
     *    are **not** pre-loaded and BSS is **not** zeroed here.  The apploader
     *    (run by ipl-hle) loads every section via DI DMA, making pre-loading
     *    redundant.  Pre-zeroing BSS would also wipe the OS globals written in
     *    step 4 (e.g. arena_lo at `0x8000_0030`) if the game's BSS covers that
     *    region, causing a false "ArenaLo still 0" diagnostic.
     * 7. **Runtime disc** — the flat disc image is stored for later `0xA8`
     *    DVD Read DMA commands.
     *
     * Returns a JavaScript object with the following fields on success:
     *
     * | Field              | Type   | Description                              |
     * |--------------------|--------|------------------------------------------|
     * | `gameName`         | string | Null-terminated game title from the header |
     * | `gameId`           | string | 6-character game identifier               |
     * | `dolEntry`         | number | Boot DOL entry-point address              |
     * | `apploaderEntry`   | number | Apploader entry function address          |
     *
     * Returns a JavaScript `Error` on failure (bad magic, corrupt DOL, …).
     * @param {Uint8Array} data
     * @returns {any}
     */
    parse_and_load_disc(data) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmemulator_parse_and_load_disc(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Total number of `raise_exception` calls since emulator creation.
     * @returns {number}
     */
    raise_exception_count() {
        const ret = wasm.wasmemulator_raise_exception_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Returns a raw pointer (WASM linear memory offset) to the start of the
     * guest RAM buffer.
     *
     * Combine with [`wasm_memory`] and [`ram_size`] to create a live,
     * zero-copy JavaScript view:
     *
     * ```js
     * const ram = new Uint8Array(wasm_memory().buffer, emu.ram_ptr(), emu.ram_size());
     * ```
     * @returns {number}
     */
    ram_ptr() {
        const ret = wasm.wasmemulator_ram_ptr(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Returns the size of the guest RAM buffer in bytes.
     * @returns {number}
     */
    ram_size() {
        const ret = wasm.wasmemulator_ram_size(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Notify the emulator that one compiled block has just been executed.
     */
    record_block_executed() {
        wasm.wasmemulator_record_block_executed(this.__wbg_ptr);
    }
    /**
     * Increment the `raise_exception` counter.
     */
    record_raise_exception() {
        wasm.wasmemulator_record_raise_exception(this.__wbg_ptr);
    }
    /**
     * Serialize the complete emulator state to a byte array for savestate support.
     *
     * The returned [`js_sys::Uint8Array`] can be saved to `localStorage` or
     * downloaded as a file, then passed back to [`load_state`] to restore.
     *
     * ## Format (little-endian binary)
     *
     * ```text
     *   [4]  magic     = b"LAZU"
     *   [4]  version   = 2
     *   [4]  cpu_size  = size_of::<gekko::Cpu>()
     *   [N]  cpu       = raw bytes of gekko::Cpu
     *   [4]  ram_size  = self.ram.len()
     *   [M]  ram       = self.ram
     *   [4]  pi_intsr
     *   [4]  pi_intmsk
     *   [4]  pad_buttons
     *   [4]  flags     = decrementer_pending as u32
     *   [128] vi_regs  = 32 × u32 (raw VI register file)
     *   [12] si_out    = 3 × u32 SI output buffers 0–2
     *   [4]  si_out3   = SI output buffer 3
     *   [16] si_in_hi  = 4 × u32 SI input buffer high words
     *   [16] si_in_lo  = 4 × u32 SI input buffer low words
     *   [4]  si_poll
     *   [4]  si_comm_ctrl
     *   [4]  si_status
     *   [4]  ai_control
     *   [4]  ai_volume
     *   [4]  ai_sample_count
     *   [4]  ai_interrupt_sample
     * ```
     * @returns {Uint8Array}
     */
    save_state() {
        const ret = wasm.wasmemulator_save_state(this.__wbg_ptr);
        return ret;
    }
    /**
     * Store precise analog axis values for the port-0 controller.
     *
     * Called by the JavaScript input layer once per animation frame after
     * either reading the Gamepad API (full 8-bit resolution) or computing
     * axis values from keyboard `STICK_*` pseudo-buttons (±96 deflection).
     *
     * These values are forwarded verbatim into the SI poll response (bytes 2–7
     * of the 8-byte controller report), replacing the old fixed-deflection
     * calculation that was derived from the digital button bitmask.
     *
     * - `joy_x` / `joy_y`: main stick (0–255, centre = 128; Y: up = larger).
     * - `c_stick_x` / `c_stick_y`: C-Stick (0–255, centre = 128).
     * - `l_trig` / `r_trig`: analog trigger depth (0 = released, 255 = full).
     * @param {number} joy_x
     * @param {number} joy_y
     * @param {number} c_stick_x
     * @param {number} c_stick_y
     * @param {number} l_trig
     * @param {number} r_trig
     */
    set_analog_axes(joy_x, joy_y, c_stick_x, c_stick_y, l_trig, r_trig) {
        wasm.wasmemulator_set_analog_axes(this.__wbg_ptr, joy_x, joy_y, c_stick_x, c_stick_y, l_trig, r_trig);
    }
    /**
     * Restore the CPU register state from raw bytes.
     *
     * `data` must have been produced by a previous call to [`get_cpu_bytes`]
     * and must therefore have length exactly [`cpu_struct_size`] bytes.  Call
     * this after `execute()` returns to sync the register changes made by the
     * compiled block back into the Rust emulator.
     * @param {Uint8Array} data
     */
    set_cpu_bytes(data) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmemulator_set_cpu_bytes(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Set GPR[i].
     * @param {number} i
     * @param {number} value
     */
    set_gpr(i, value) {
        wasm.wasmemulator_set_gpr(this.__wbg_ptr, i, value);
    }
    /**
     * Overwrite the memory card data for the given slot with `data`.
     *
     * Any number of bytes up to 512 KiB may be written; bytes beyond the
     * internal buffer length are silently ignored.  If `data` is shorter than
     * 512 KiB the remainder of the card stays at its current value (default
     * `0xFF` = erased).
     * @param {number} slot
     * @param {Uint8Array} data
     */
    set_memcard_data(slot, data) {
        wasm.wasmemulator_set_memcard_data(this.__wbg_ptr, slot, data);
    }
    /**
     * Overwrite the Machine State Register with a raw 32-bit value.
     *
     * Call this once after loading a DOL/ISO to establish the CPU state that
     * the IPL ROM would normally leave before handing off to the apploader.
     * The two critical bits are:
     *
     * * **IP** (bit 6, `exception_prefix`) — `0` here so exception vectors
     *   land at `0x000xxxxx` (physical `0x00000900` for the decrementer),
     *   which is within the 24 MiB GameCube RAM.  The default reset value
     *   (`IP = 1`) would put vectors at `0xFFF0xxxx` — beyond RAM.
     * * **EE** (bit 15, `interrupts`) — `1` here so decrementer and external
     *   interrupts can fire.  The real IPL ROM jumps to the apploader with
     *   `EE = 1`; without it, any spin-loop that waits for a decrementer
     *   interrupt would stall forever.
     *
     * Typically called with `0x8000` (`EE = 1`, all other bits cleared).  The
     * game's own `__start` / `OSInit` will reconfigure MSR as needed.
     * @param {number} value
     */
    set_msr(value) {
        wasm.wasmemulator_set_msr(this.__wbg_ptr, value);
    }
    /**
     * Set the GameCube controller button bitmask.
     *
     * Called by the JavaScript keyboard handler on every `keydown` / `keyup`
     * event.  The bitmask layout matches the `GC_BTN` constants defined in
     * `bootstrap.js`.
     * @param {number} buttons
     */
    set_pad_buttons(buttons) {
        wasm.wasmemulator_set_pad_buttons(this.__wbg_ptr, buttons);
    }
    /**
     * Set the program counter.
     * @param {number} pc
     */
    set_pc(pc) {
        wasm.wasmemulator_set_pc(this.__wbg_ptr, pc);
    }
    /**
     * Overwrite the 64-byte SRAM with `data`.
     *
     * Call this on emulator startup with bytes previously saved to
     * `localStorage` by [`get_sram`].
     * @param {Uint8Array} data
     */
    set_sram(data) {
        wasm.wasmemulator_set_sram(this.__wbg_ptr, data);
    }
    /**
     * Copy `data` back over the emulator's guest RAM.
     * @param {Uint8Array} data
     */
    sync_ram(data) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmemulator_sync_ram(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Generate up to `n_samples` stereo 16-bit PCM samples from the AI DMA
     * ring buffer and return them as a `Float32Array` of length `n_samples * 2`
     * (interleaved left/right, each sample normalised to `−1.0 … +1.0`).
     *
     * Called once per animation frame by the JavaScript audio pipeline to
     * fill the `SharedArrayBuffer` PCM ring buffer consumed by the
     * `AudioWorkletNode` DSP output worklet.
     *
     * Returns an empty array when the AI DMA is not running (`AudioDmaControl`
     * bit 15 = 0), the DMA buffer length is zero, or `n_samples == 0`.
     * @param {number} n_samples
     * @returns {Float32Array}
     */
    take_audio_samples(n_samples) {
        const ret = wasm.wasmemulator_take_audio_samples(this.__wbg_ptr, n_samples);
        return ret;
    }
    /**
     * Return `true` if a DVD DMA has written new data into guest RAM since the
     * last call, and reset the flag to `false`.
     * @returns {boolean}
     */
    take_dma_dirty() {
        const ret = wasm.wasmemulator_take_dma_dirty(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * Return and clear any EXI UART bytes emitted by the game since the last
     * call.
     *
     * The GameCube OS (`OSReport` → `__OSConsoleWrite`) writes console output
     * via the EXI UART protocol on channel 0 (command `0xA001_0000` then
     * immediate-mode data writes), not via the virtual `0xCC007000` byte port
     * used by ipl-hle.  JavaScript should call this after each emulated block
     * and pipe the returned bytes through the same `stdoutLineBuffer` →
     * `appendApploaderLog` path used for ipl-hle output.
     * @returns {Uint8Array}
     */
    take_uart_output() {
        const ret = wasm.wasmemulator_take_uart_output(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * Number of compiled blocks that contained at least one unimplemented opcode.
     * @returns {number}
     */
    unimplemented_block_count() {
        const ret = wasm.wasmemulator_unimplemented_block_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Physical RAM address of the External Frame Buffer as programmed by the
     * game into the VI TFBL register.
     *
     * Returns `0` if the game has not yet configured the VI (e.g. during
     * early boot).  JavaScript falls back to heuristic XFB detection when
     * this returns `0`.
     * @returns {number}
     */
    vi_xfb_addr() {
        const ret = wasm.wasmemulator_vi_xfb_addr(this.__wbg_ptr);
        return ret >>> 0;
    }
}
if (Symbol.dispose) WasmEmulator.prototype[Symbol.dispose] = WasmEmulator.prototype.free;

/**
 * Opaque handle to the initialised WebGPU rendering context.
 *
 * Exported to JavaScript as `WgpuRenderer`.  When the renderer is
 * unavailable (WebGPU not supported or surface creation failed) the
 * [`init_webgpu_renderer`] factory returns `undefined` instead of this type.
 */
export class WgpuRenderer {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(WgpuRenderer.prototype);
        obj.__wbg_ptr = ptr;
        WgpuRendererFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WgpuRendererFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wgpurenderer_free(ptr, 0);
    }
    /**
     * Clone and return a [`GxRendererHandle`] backed by this renderer.
     *
     * The handle shares the same internal Arc as this `WgpuRenderer`, so GX
     * actions enqueued through the handle are visible to `present_xfb`.
     * @returns {GxRendererHandle}
     */
    gx_renderer_handle() {
        const ret = wasm.wgpurenderer_gx_renderer_handle(this.__wbg_ptr);
        return GxRendererHandle.__wrap(ret);
    }
    /**
     * Present a 640×480 YUV422 external frame-buffer via the wgpu blitter.
     *
     * `xfb_data` must be a `Uint8Array` (or `ArrayBuffer`-backed view) of
     * exactly `640 × 480 × 2 = 614 400` bytes in GameCube YCbYCr 4:2:2
     * byte order (`[Cb, Y0, Cr, Y1]` per pair of pixels).
     *
     * The function converts the YUV422 data to RGBA8 on the CPU, uploads it
     * to a 640×480 GPU texture, then blits the texture to the swap-chain
     * surface using a fullscreen-quad render pass.
     *
     * Returns `true` on success, `false` if the surface texture is lost or
     * `xfb_data` has the wrong length.
     * @param {Uint8Array} xfb_data
     * @returns {boolean}
     */
    present_xfb(xfb_data) {
        const ptr0 = passArray8ToWasm0(xfb_data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wgpurenderer_present_xfb(this.__wbg_ptr, ptr0, len0);
        return ret !== 0;
    }
    /**
     * Reconfigure the swap-chain when the canvas is resized.
     *
     * `width` and `height` are the new canvas dimensions in physical pixels.
     * Must be called from JavaScript whenever the canvas `resize` event fires.
     * @param {number} width
     * @param {number} height
     */
    resize(width, height) {
        wasm.wgpurenderer_resize(this.__wbg_ptr, width, height);
    }
}
if (Symbol.dispose) WgpuRenderer.prototype[Symbol.dispose] = WgpuRenderer.prototype.free;

/**
 * Returns `true` if the browser exposes `navigator.gpu` (WebGPU is available).
 *
 * WebGPU is the GPU API used by `wgpu` on `wasm32` targets (enabled via the
 * `webgpu` feature).  Call this from JavaScript before attempting to
 * initialise the GPU renderer; fall back to the canvas-based XFB blitter if
 * it returns `false`.
 * @returns {boolean}
 */
export function check_webgpu_support() {
    const ret = wasm.check_webgpu_support();
    return ret !== 0;
}

/**
 * Initialise a WebGPU rendering surface from a `<canvas>` element.
 *
 * `canvas_id` is the `id` attribute of the canvas element (e.g. `"screen"`).
 *
 * Returns a `Promise<WgpuRenderer | undefined>`:
 * - **`WgpuRenderer`** on success.
 * - **`undefined`** when WebGPU is unavailable, the canvas is not found, or
 *   adapter / device creation fails.
 *
 * Typical call from JS:
 * ```js
 * const renderer = await init_webgpu_renderer("screen");
 * if (renderer) { /* use renderer.present_xfb(rawYuv) each frame */ }
 * ```
 * @param {string} canvas_id
 * @returns {Promise<WgpuRenderer | undefined>}
 */
export function init_webgpu_renderer(canvas_id) {
    const ptr0 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.init_webgpu_renderer(ptr0, len0);
    return ret;
}

/**
 * Returns the WebAssembly linear memory of this module.
 *
 * JavaScript can use the returned [`WebAssembly::Memory`] together with
 * [`WasmEmulator::ram_ptr`] and [`WasmEmulator::ram_size`] to create a
 * **zero-copy live view** over the emulator's guest RAM:
 *
 * ```js
 * const mem   = wasm_memory();
 * const ptr   = emu.ram_ptr();
 * const size  = emu.ram_size();
 * const ram   = new Uint8Array(mem.buffer, ptr, size);
 * ```
 * @returns {any}
 */
export function wasm_memory() {
    const ret = wasm.wasm_memory();
    return ret;
}

function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Window_1756cca7c20c8b14: function(arg0) {
            const ret = arg0.Window;
            return ret;
        },
        __wbg_WorkerGlobalScope_20611759b16d5562: function(arg0) {
            const ret = arg0.WorkerGlobalScope;
            return ret;
        },
        __wbg___wbindgen_debug_string_ddde1867f49c2442: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_d633e708baf0d146: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_null_a2a19127c13e7126: function(arg0) {
            const ret = arg0 === null;
            return ret;
        },
        __wbg___wbindgen_is_undefined_c18285b9fc34cb7d: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_memory_f1258f0b3cab52b2: function() {
            const ret = wasm.memory;
            return ret;
        },
        __wbg___wbindgen_string_get_3e5751597f39a112: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_39bc967c0e5a9b58: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_b6d832240a919168: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_beginComputePass_7ab36609687f4afd: function(arg0, arg1) {
            const ret = arg0.beginComputePass(arg1);
            return ret;
        },
        __wbg_beginRenderPass_97412588087ef6bd: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.beginRenderPass(arg1);
            return ret;
        }, arguments); },
        __wbg_buffer_b47db24bb1e1d5fd: function(arg0) {
            const ret = arg0.buffer;
            return ret;
        },
        __wbg_call_08ad0d89caa7cb79: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_configure_cd2e352cb466c0cf: function() { return handleError(function (arg0, arg1) {
            arg0.configure(arg1);
        }, arguments); },
        __wbg_copyBufferToBuffer_42b26b2d65341a15: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4, arg5) {
            arg0.copyBufferToBuffer(arg1, arg2, arg3, arg4, arg5);
        }, arguments); },
        __wbg_copyBufferToBuffer_59fbf1f9e9575053: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.copyBufferToBuffer(arg1, arg2, arg3, arg4);
        }, arguments); },
        __wbg_copyExternalImageToTexture_b4fdb27198716270: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.copyExternalImageToTexture(arg1, arg2, arg3);
        }, arguments); },
        __wbg_copyTextureToBuffer_ddc87dfd3c1d76d6: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.copyTextureToBuffer(arg1, arg2, arg3);
        }, arguments); },
        __wbg_copyTextureToTexture_9a335dc0ac390d48: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.copyTextureToTexture(arg1, arg2, arg3);
        }, arguments); },
        __wbg_createBindGroupLayout_c4a5cff0c721d083: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.createBindGroupLayout(arg1);
            return ret;
        }, arguments); },
        __wbg_createBindGroup_cad2330b5802ce26: function(arg0, arg1) {
            const ret = arg0.createBindGroup(arg1);
            return ret;
        },
        __wbg_createBuffer_bdbc83dadf03d0d1: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.createBuffer(arg1);
            return ret;
        }, arguments); },
        __wbg_createCommandEncoder_cda1fa4d6fbf0c7c: function(arg0, arg1) {
            const ret = arg0.createCommandEncoder(arg1);
            return ret;
        },
        __wbg_createComputePipeline_285943f1bfb6ff49: function(arg0, arg1) {
            const ret = arg0.createComputePipeline(arg1);
            return ret;
        },
        __wbg_createPipelineLayout_b96eeb675ae67b1c: function(arg0, arg1) {
            const ret = arg0.createPipelineLayout(arg1);
            return ret;
        },
        __wbg_createRenderPipeline_39e33bd0058fc32b: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.createRenderPipeline(arg1);
            return ret;
        }, arguments); },
        __wbg_createSampler_69fc051d17ae6eca: function(arg0, arg1) {
            const ret = arg0.createSampler(arg1);
            return ret;
        },
        __wbg_createShaderModule_aa70c7e578260f08: function(arg0, arg1) {
            const ret = arg0.createShaderModule(arg1);
            return ret;
        },
        __wbg_createTask_44488751912c7d5f: function() { return handleError(function (arg0, arg1) {
            const ret = console.createTask(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_createTexture_3facac0a8c675065: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.createTexture(arg1);
            return ret;
        }, arguments); },
        __wbg_createView_0d7b4d36af6ca304: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.createView(arg1);
            return ret;
        }, arguments); },
        __wbg_dispatchWorkgroups_167f339118281e7b: function(arg0, arg1, arg2, arg3) {
            arg0.dispatchWorkgroups(arg1 >>> 0, arg2 >>> 0, arg3 >>> 0);
        },
        __wbg_document_0b7613236d782ccc: function(arg0) {
            const ret = arg0.document;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_drawIndexedIndirect_96728c6974b54340: function(arg0, arg1, arg2) {
            arg0.drawIndexedIndirect(arg1, arg2);
        },
        __wbg_drawIndexed_330eac0effb50ccb: function(arg0, arg1, arg2, arg3, arg4, arg5) {
            arg0.drawIndexed(arg1 >>> 0, arg2 >>> 0, arg3 >>> 0, arg4, arg5 >>> 0);
        },
        __wbg_drawIndirect_72ece4af2d7295f1: function(arg0, arg1, arg2) {
            arg0.drawIndirect(arg1, arg2);
        },
        __wbg_draw_55406aeffe495d15: function(arg0, arg1, arg2, arg3, arg4) {
            arg0.draw(arg1 >>> 0, arg2 >>> 0, arg3 >>> 0, arg4 >>> 0);
        },
        __wbg_end_3b65b410f3693f0e: function(arg0) {
            arg0.end();
        },
        __wbg_end_b230f52c6c19d300: function(arg0) {
            arg0.end();
        },
        __wbg_error_ad28debb48b5c6bb: function(arg0) {
            console.error(arg0);
        },
        __wbg_executeBundles_9143ccd85b855309: function(arg0, arg1) {
            arg0.executeBundles(arg1);
        },
        __wbg_features_156686942e1a5a7a: function(arg0) {
            const ret = arg0.features;
            return ret;
        },
        __wbg_finish_2a38e8d1284f8446: function(arg0) {
            const ret = arg0.finish();
            return ret;
        },
        __wbg_finish_fda3655b70b2d97b: function(arg0, arg1) {
            const ret = arg0.finish(arg1);
            return ret;
        },
        __wbg_getContext_04fd91bf79400077: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.getContext(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_getContext_f63e0cc3b9d1dc24: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.getContext(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_getCurrentTexture_dbd47d3d6cbb5207: function() { return handleError(function (arg0) {
            const ret = arg0.getCurrentTexture();
            return ret;
        }, arguments); },
        __wbg_getElementById_dff2c0f6070bc31a: function(arg0, arg1, arg2) {
            const ret = arg0.getElementById(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_getMappedRange_ee4eff6598fc8ce3: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.getMappedRange(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_getPreferredCanvasFormat_0f8c3c4e325a46c4: function(arg0) {
            const ret = arg0.getPreferredCanvasFormat();
            return (__wbindgen_enum_GpuTextureFormat.indexOf(ret) + 1 || 96) - 1;
        },
        __wbg_get_01b80713f61639c9: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_get_18349afdb36339a9: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_gpu_3dcbfc11cfa51a64: function(arg0) {
            const ret = arg0.gpu;
            return ret;
        },
        __wbg_has_69ddb83a8593a730: function(arg0, arg1, arg2) {
            const ret = arg0.has(getStringFromWasm0(arg1, arg2));
            return ret;
        },
        __wbg_height_a2a793f8a2363a46: function(arg0) {
            const ret = arg0.height;
            return ret;
        },
        __wbg_instanceof_GpuAdapter_0cfeec72ed63bd10: function(arg0) {
            let result;
            try {
                result = arg0 instanceof GPUAdapter;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_GpuCanvasContext_646ce2a8705ee4ff: function(arg0) {
            let result;
            try {
                result = arg0 instanceof GPUCanvasContext;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlCanvasElement_d8fa699a8663ca1b: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLCanvasElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_4aba49e4d1a12365: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_label_f4306e9b9c27739d: function(arg0, arg1) {
            const ret = arg1.label;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_length_5855c1f289dfffc1: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_log_3c5e4b64af29e724: function(arg0) {
            console.log(arg0);
        },
        __wbg_mapAsync_7bce156abc438d28: function(arg0, arg1, arg2, arg3) {
            const ret = arg0.mapAsync(arg1 >>> 0, arg2, arg3);
            return ret;
        },
        __wbg_navigator_bb9bf52d5003ebaa: function(arg0) {
            const ret = arg0.navigator;
            return ret;
        },
        __wbg_navigator_c088813b66e0b67c: function(arg0) {
            const ret = arg0.navigator;
            return ret;
        },
        __wbg_new_a3296a626c54757c: function() { return handleError(function (arg0) {
            const ret = new WebAssembly.Module(arg0);
            return ret;
        }, arguments); },
        __wbg_new_cbee8c0d5c479eac: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_ed69e637b553a997: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_from_slice_d7e202fdbee3c396: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_typed_8258a0d8488ef2a2: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___js_sys_a2cf80ff77c7437f___Function_fn_wasm_bindgen_7a0033f4c224aadb___JsValue_____wasm_bindgen_7a0033f4c224aadb___sys__Undefined___js_sys_a2cf80ff77c7437f___Function_fn_wasm_bindgen_7a0033f4c224aadb___JsValue_____wasm_bindgen_7a0033f4c224aadb___sys__Undefined______(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return ret;
            } finally {
                state0.a = state0.b = 0;
            }
        },
        __wbg_new_typed_e8cd930b75161ad3: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_with_byte_offset_and_length_3e6cc05444a2f3c5: function(arg0, arg1, arg2) {
            const ret = new Uint8Array(arg0, arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_new_with_length_186ccc039de832a1: function(arg0) {
            const ret = new Float32Array(arg0 >>> 0);
            return ret;
        },
        __wbg_new_with_length_c8449d782396d344: function(arg0) {
            const ret = new Uint8Array(arg0 >>> 0);
            return ret;
        },
        __wbg_now_edd718b3004d8631: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_onSubmittedWorkDone_2908dbcb39b7b60b: function(arg0) {
            const ret = arg0.onSubmittedWorkDone();
            return ret;
        },
        __wbg_prototypesetcall_f034d444741426c3: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_push_a6f9488ffd3fae3b: function(arg0, arg1) {
            const ret = arg0.push(arg1);
            return ret;
        },
        __wbg_querySelectorAll_0553d7ba7491befc: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.querySelectorAll(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_queueMicrotask_2c8dfd1056f24fdc: function(arg0) {
            const ret = arg0.queueMicrotask;
            return ret;
        },
        __wbg_queueMicrotask_8985ad63815852e7: function(arg0) {
            queueMicrotask(arg0);
        },
        __wbg_queue_5c4b7575b8e477bf: function(arg0) {
            const ret = arg0.queue;
            return ret;
        },
        __wbg_requestAdapter_8696df68634abfdb: function(arg0, arg1) {
            const ret = arg0.requestAdapter(arg1);
            return ret;
        },
        __wbg_requestDevice_66045430e4b22643: function(arg0, arg1) {
            const ret = arg0.requestDevice(arg1);
            return ret;
        },
        __wbg_resolve_5d61e0d10c14730a: function(arg0) {
            const ret = Promise.resolve(arg0);
            return ret;
        },
        __wbg_run_18140d3b2b16bf86: function(arg0, arg1, arg2) {
            try {
                var state0 = {a: arg1, b: arg2};
                var cb0 = () => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___bool_(a, state0.b, );
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = arg0.run(cb0);
                return ret;
            } finally {
                state0.a = state0.b = 0;
            }
        },
        __wbg_setBindGroup_0153ebd756e67818: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
            arg0.setBindGroup(arg1 >>> 0, arg2, getArrayU32FromWasm0(arg3, arg4), arg5, arg6 >>> 0);
        }, arguments); },
        __wbg_setBindGroup_1634280a9ddbebc5: function(arg0, arg1, arg2) {
            arg0.setBindGroup(arg1 >>> 0, arg2);
        },
        __wbg_setBindGroup_4072a5a36c6107ae: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
            arg0.setBindGroup(arg1 >>> 0, arg2, getArrayU32FromWasm0(arg3, arg4), arg5, arg6 >>> 0);
        }, arguments); },
        __wbg_setBindGroup_72aded5c368ce541: function(arg0, arg1, arg2) {
            arg0.setBindGroup(arg1 >>> 0, arg2);
        },
        __wbg_setBlendConstant_dcdec5e100e91661: function() { return handleError(function (arg0, arg1) {
            arg0.setBlendConstant(arg1);
        }, arguments); },
        __wbg_setIndexBuffer_3d432bbc200e8393: function(arg0, arg1, arg2, arg3, arg4) {
            arg0.setIndexBuffer(arg1, __wbindgen_enum_GpuIndexFormat[arg2], arg3, arg4);
        },
        __wbg_setIndexBuffer_6c61e8335b96fb9c: function(arg0, arg1, arg2, arg3) {
            arg0.setIndexBuffer(arg1, __wbindgen_enum_GpuIndexFormat[arg2], arg3);
        },
        __wbg_setPipeline_5cdfdc1a89567486: function(arg0, arg1) {
            arg0.setPipeline(arg1);
        },
        __wbg_setPipeline_73eff3fa465fa696: function(arg0, arg1) {
            arg0.setPipeline(arg1);
        },
        __wbg_setScissorRect_8f54371427a00ab0: function(arg0, arg1, arg2, arg3, arg4) {
            arg0.setScissorRect(arg1 >>> 0, arg2 >>> 0, arg3 >>> 0, arg4 >>> 0);
        },
        __wbg_setStencilReference_18b1084e71a1d091: function(arg0, arg1) {
            arg0.setStencilReference(arg1 >>> 0);
        },
        __wbg_setVertexBuffer_36dda53978ae18ff: function(arg0, arg1, arg2, arg3, arg4) {
            arg0.setVertexBuffer(arg1 >>> 0, arg2, arg3, arg4);
        },
        __wbg_setVertexBuffer_a628509c6712b9eb: function(arg0, arg1, arg2, arg3) {
            arg0.setVertexBuffer(arg1 >>> 0, arg2, arg3);
        },
        __wbg_setViewport_5ff16a2dbdd3b805: function(arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
            arg0.setViewport(arg1, arg2, arg3, arg4, arg5, arg6);
        },
        __wbg_set_410caa03fd65d0ba: function(arg0, arg1, arg2) {
            arg0.set(arg1, arg2 >>> 0);
        },
        __wbg_set_a_5ff87e09923a325c: function(arg0, arg1) {
            arg0.a = arg1;
        },
        __wbg_set_access_82864a8003314384: function(arg0, arg1) {
            arg0.access = __wbindgen_enum_GpuStorageTextureAccess[arg1];
        },
        __wbg_set_address_mode_u_13eb4c280c937ade: function(arg0, arg1) {
            arg0.addressModeU = __wbindgen_enum_GpuAddressMode[arg1];
        },
        __wbg_set_address_mode_v_4d77ca3b21d08c50: function(arg0, arg1) {
            arg0.addressModeV = __wbindgen_enum_GpuAddressMode[arg1];
        },
        __wbg_set_address_mode_w_28eee205120610bc: function(arg0, arg1) {
            arg0.addressModeW = __wbindgen_enum_GpuAddressMode[arg1];
        },
        __wbg_set_alpha_05d52ebdf7006f59: function(arg0, arg1) {
            arg0.alpha = arg1;
        },
        __wbg_set_alpha_mode_31aa52f7e9c9a202: function(arg0, arg1) {
            arg0.alphaMode = __wbindgen_enum_GpuCanvasAlphaMode[arg1];
        },
        __wbg_set_alpha_to_coverage_enabled_d66c844e37871fe7: function(arg0, arg1) {
            arg0.alphaToCoverageEnabled = arg1 !== 0;
        },
        __wbg_set_array_layer_count_c75fe755c29fb1bd: function(arg0, arg1) {
            arg0.arrayLayerCount = arg1 >>> 0;
        },
        __wbg_set_array_stride_96e1285616c3007c: function(arg0, arg1) {
            arg0.arrayStride = arg1;
        },
        __wbg_set_aspect_70a084c67a2f4aa9: function(arg0, arg1) {
            arg0.aspect = __wbindgen_enum_GpuTextureAspect[arg1];
        },
        __wbg_set_aspect_9c16fa42ac535f21: function(arg0, arg1) {
            arg0.aspect = __wbindgen_enum_GpuTextureAspect[arg1];
        },
        __wbg_set_attributes_778d192b7c54a5e6: function(arg0, arg1) {
            arg0.attributes = arg1;
        },
        __wbg_set_b_86e36272ebf1467e: function(arg0, arg1) {
            arg0.b = arg1;
        },
        __wbg_set_bad5c505cc70b5f8: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_set_base_array_layer_af79b53338f23254: function(arg0, arg1) {
            arg0.baseArrayLayer = arg1 >>> 0;
        },
        __wbg_set_base_mip_level_c159752b9b0f1468: function(arg0, arg1) {
            arg0.baseMipLevel = arg1 >>> 0;
        },
        __wbg_set_beginning_of_pass_write_index_6364b384e4528d11: function(arg0, arg1) {
            arg0.beginningOfPassWriteIndex = arg1 >>> 0;
        },
        __wbg_set_beginning_of_pass_write_index_e298058f701fbd63: function(arg0, arg1) {
            arg0.beginningOfPassWriteIndex = arg1 >>> 0;
        },
        __wbg_set_bind_group_layouts_954826eccd7315b8: function(arg0, arg1) {
            arg0.bindGroupLayouts = arg1;
        },
        __wbg_set_binding_2dd40b8c3d0dc785: function(arg0, arg1) {
            arg0.binding = arg1 >>> 0;
        },
        __wbg_set_binding_79355ded0a1993e7: function(arg0, arg1) {
            arg0.binding = arg1 >>> 0;
        },
        __wbg_set_blend_b1d29d50cee760c2: function(arg0, arg1) {
            arg0.blend = arg1;
        },
        __wbg_set_buffer_bf4a41cd961168b4: function(arg0, arg1) {
            arg0.buffer = arg1;
        },
        __wbg_set_buffer_c181b12328e9912e: function(arg0, arg1) {
            arg0.buffer = arg1;
        },
        __wbg_set_buffer_d5f041e0835b145f: function(arg0, arg1) {
            arg0.buffer = arg1;
        },
        __wbg_set_buffers_d2cc0ac0dee27be3: function(arg0, arg1) {
            arg0.buffers = arg1;
        },
        __wbg_set_bytes_per_row_3d75264116872d7c: function(arg0, arg1) {
            arg0.bytesPerRow = arg1 >>> 0;
        },
        __wbg_set_bytes_per_row_eaa42234c4685821: function(arg0, arg1) {
            arg0.bytesPerRow = arg1 >>> 0;
        },
        __wbg_set_clear_value_9d4cda515cc862af: function(arg0, arg1) {
            arg0.clearValue = arg1;
        },
        __wbg_set_code_e3eb238db52fa8d6: function(arg0, arg1, arg2) {
            arg0.code = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_color_63049591202dc6d5: function(arg0, arg1) {
            arg0.color = arg1;
        },
        __wbg_set_color_attachments_af99112ea78ccdd8: function(arg0, arg1) {
            arg0.colorAttachments = arg1;
        },
        __wbg_set_compare_098e80455dc51912: function(arg0, arg1) {
            arg0.compare = __wbindgen_enum_GpuCompareFunction[arg1];
        },
        __wbg_set_compare_eacdf8caefd01d05: function(arg0, arg1) {
            arg0.compare = __wbindgen_enum_GpuCompareFunction[arg1];
        },
        __wbg_set_compute_ee5929a2e09c2cd3: function(arg0, arg1) {
            arg0.compute = arg1;
        },
        __wbg_set_count_a9b1acd6244bda9e: function(arg0, arg1) {
            arg0.count = arg1 >>> 0;
        },
        __wbg_set_cull_mode_c0dc2002092ebc13: function(arg0, arg1) {
            arg0.cullMode = __wbindgen_enum_GpuCullMode[arg1];
        },
        __wbg_set_depth_bias_1ce76e1f72665f43: function(arg0, arg1) {
            arg0.depthBias = arg1;
        },
        __wbg_set_depth_bias_clamp_d1b8f43ed4720419: function(arg0, arg1) {
            arg0.depthBiasClamp = arg1;
        },
        __wbg_set_depth_bias_slope_scale_0b7de979023dff9d: function(arg0, arg1) {
            arg0.depthBiasSlopeScale = arg1;
        },
        __wbg_set_depth_clear_value_665b346cb9fa3c74: function(arg0, arg1) {
            arg0.depthClearValue = arg1;
        },
        __wbg_set_depth_compare_606bc11fcdce512d: function(arg0, arg1) {
            arg0.depthCompare = __wbindgen_enum_GpuCompareFunction[arg1];
        },
        __wbg_set_depth_fail_op_57d07ddb2ae801d4: function(arg0, arg1) {
            arg0.depthFailOp = __wbindgen_enum_GpuStencilOperation[arg1];
        },
        __wbg_set_depth_load_op_6f6cd4dcae14a71e: function(arg0, arg1) {
            arg0.depthLoadOp = __wbindgen_enum_GpuLoadOp[arg1];
        },
        __wbg_set_depth_or_array_layers_77423d7fdd11ff6e: function(arg0, arg1) {
            arg0.depthOrArrayLayers = arg1 >>> 0;
        },
        __wbg_set_depth_read_only_124e54b7aa6fd096: function(arg0, arg1) {
            arg0.depthReadOnly = arg1 !== 0;
        },
        __wbg_set_depth_stencil_1503693246d42f25: function(arg0, arg1) {
            arg0.depthStencil = arg1;
        },
        __wbg_set_depth_stencil_attachment_5e43ae951585406e: function(arg0, arg1) {
            arg0.depthStencilAttachment = arg1;
        },
        __wbg_set_depth_store_op_969f87f4e9599f65: function(arg0, arg1) {
            arg0.depthStoreOp = __wbindgen_enum_GpuStoreOp[arg1];
        },
        __wbg_set_depth_write_enabled_42cbd9b1613bacf6: function(arg0, arg1) {
            arg0.depthWriteEnabled = arg1 !== 0;
        },
        __wbg_set_device_97b55deb618fff98: function(arg0, arg1) {
            arg0.device = arg1;
        },
        __wbg_set_dimension_ca726b48f75349f0: function(arg0, arg1) {
            arg0.dimension = __wbindgen_enum_GpuTextureViewDimension[arg1];
        },
        __wbg_set_dimension_d49127d98aff9c51: function(arg0, arg1) {
            arg0.dimension = __wbindgen_enum_GpuTextureDimension[arg1];
        },
        __wbg_set_dst_factor_055b13fa24fbb378: function(arg0, arg1) {
            arg0.dstFactor = __wbindgen_enum_GpuBlendFactor[arg1];
        },
        __wbg_set_end_of_pass_write_index_d1351e385857b590: function(arg0, arg1) {
            arg0.endOfPassWriteIndex = arg1 >>> 0;
        },
        __wbg_set_end_of_pass_write_index_dde7645952606d6a: function(arg0, arg1) {
            arg0.endOfPassWriteIndex = arg1 >>> 0;
        },
        __wbg_set_entries_7dac852e18b7d619: function(arg0, arg1) {
            arg0.entries = arg1;
        },
        __wbg_set_entries_bb1b29ce357a455c: function(arg0, arg1) {
            arg0.entries = arg1;
        },
        __wbg_set_entry_point_197ab87bb2c3d2fe: function(arg0, arg1, arg2) {
            arg0.entryPoint = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_entry_point_d107632202ab5844: function(arg0, arg1, arg2) {
            arg0.entryPoint = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_entry_point_d2df1b98f5727dfc: function(arg0, arg1, arg2) {
            arg0.entryPoint = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_external_texture_69180fceb9b5b8b0: function(arg0, arg1) {
            arg0.externalTexture = arg1;
        },
        __wbg_set_fail_op_0b23057046f9dff0: function(arg0, arg1) {
            arg0.failOp = __wbindgen_enum_GpuStencilOperation[arg1];
        },
        __wbg_set_flip_y_8bee3b2c77da30e8: function(arg0, arg1) {
            arg0.flipY = arg1 !== 0;
        },
        __wbg_set_format_0b3c0e3ed7e59efa: function(arg0, arg1) {
            arg0.format = __wbindgen_enum_GpuTextureFormat[arg1];
        },
        __wbg_set_format_3152c0e030bbab90: function(arg0, arg1) {
            arg0.format = __wbindgen_enum_GpuTextureFormat[arg1];
        },
        __wbg_set_format_5a174722f62ac7e6: function(arg0, arg1) {
            arg0.format = __wbindgen_enum_GpuVertexFormat[arg1];
        },
        __wbg_set_format_b1666aa3303ecb15: function(arg0, arg1) {
            arg0.format = __wbindgen_enum_GpuTextureFormat[arg1];
        },
        __wbg_set_format_b7e693ee2bce403c: function(arg0, arg1) {
            arg0.format = __wbindgen_enum_GpuTextureFormat[arg1];
        },
        __wbg_set_format_ca9f81b91392c653: function(arg0, arg1) {
            arg0.format = __wbindgen_enum_GpuTextureFormat[arg1];
        },
        __wbg_set_format_f5cbba4e2797edd0: function(arg0, arg1) {
            arg0.format = __wbindgen_enum_GpuTextureFormat[arg1];
        },
        __wbg_set_fragment_ec3cd7aed1e86517: function(arg0, arg1) {
            arg0.fragment = arg1;
        },
        __wbg_set_front_face_35e4499c84cb2225: function(arg0, arg1) {
            arg0.frontFace = __wbindgen_enum_GpuFrontFace[arg1];
        },
        __wbg_set_g_6986fa24f5362d79: function(arg0, arg1) {
            arg0.g = arg1;
        },
        __wbg_set_has_dynamic_offset_0e3bc5145905fa69: function(arg0, arg1) {
            arg0.hasDynamicOffset = arg1 !== 0;
        },
        __wbg_set_height_b3ad521fb0d982ea: function(arg0, arg1) {
            arg0.height = arg1 >>> 0;
        },
        __wbg_set_height_d6ea216e60c349ad: function(arg0, arg1) {
            arg0.height = arg1 >>> 0;
        },
        __wbg_set_height_ed13c7b896d93a3b: function(arg0, arg1) {
            arg0.height = arg1 >>> 0;
        },
        __wbg_set_index_c75dd864a3e5d51a: function(arg0, arg1, arg2) {
            arg0[arg1 >>> 0] = arg2;
        },
        __wbg_set_label_064d341a1bf13986: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_227283ec820d423e: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_3259e85cadf38240: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_32e98c79f088cb98: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_3d8f8a8b9183415d: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_4500748cf1ae9e84: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_599891b66e341446: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_67500070f67090c0: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_6d3063c63b3ef117: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_717aa7ae41e2b932: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_b43c3f9a700232f0: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_dbd46cc0da867a2e: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_e150d9e072c0f458: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_e532929f2ec15ffe: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_label_ec471dff43abbe85: function(arg0, arg1, arg2) {
            arg0.label = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_layout_23e382d308ba946f: function(arg0, arg1) {
            arg0.layout = arg1;
        },
        __wbg_set_layout_77330e7b5b339bac: function(arg0, arg1) {
            arg0.layout = arg1;
        },
        __wbg_set_layout_ee915e46650c3eac: function(arg0, arg1) {
            arg0.layout = arg1;
        },
        __wbg_set_load_op_228844424beb7934: function(arg0, arg1) {
            arg0.loadOp = __wbindgen_enum_GpuLoadOp[arg1];
        },
        __wbg_set_lod_max_clamp_f71815b2d99e26f6: function(arg0, arg1) {
            arg0.lodMaxClamp = arg1;
        },
        __wbg_set_lod_min_clamp_7281d0efcb7f41ce: function(arg0, arg1) {
            arg0.lodMinClamp = arg1;
        },
        __wbg_set_mag_filter_53c457d53e2136f7: function(arg0, arg1) {
            arg0.magFilter = __wbindgen_enum_GpuFilterMode[arg1];
        },
        __wbg_set_mapped_at_creation_ae960b99faf4e4e8: function(arg0, arg1) {
            arg0.mappedAtCreation = arg1 !== 0;
        },
        __wbg_set_mask_2997bc736a5405a6: function(arg0, arg1) {
            arg0.mask = arg1 >>> 0;
        },
        __wbg_set_max_anisotropy_7b38999c805c4d86: function(arg0, arg1) {
            arg0.maxAnisotropy = arg1;
        },
        __wbg_set_min_binding_size_0b87e2d3aec14b18: function(arg0, arg1) {
            arg0.minBindingSize = arg1;
        },
        __wbg_set_min_filter_1ed8563d6060f86b: function(arg0, arg1) {
            arg0.minFilter = __wbindgen_enum_GpuFilterMode[arg1];
        },
        __wbg_set_mip_level_2bf34455eadf82c7: function(arg0, arg1) {
            arg0.mipLevel = arg1 >>> 0;
        },
        __wbg_set_mip_level_count_b26ec54a4ed8c01d: function(arg0, arg1) {
            arg0.mipLevelCount = arg1 >>> 0;
        },
        __wbg_set_mip_level_count_e17e5f4ce1a93e26: function(arg0, arg1) {
            arg0.mipLevelCount = arg1 >>> 0;
        },
        __wbg_set_mip_level_fe200cd19442d3d7: function(arg0, arg1) {
            arg0.mipLevel = arg1 >>> 0;
        },
        __wbg_set_mipmap_filter_f4b7ba86fc2538eb: function(arg0, arg1) {
            arg0.mipmapFilter = __wbindgen_enum_GpuMipmapFilterMode[arg1];
        },
        __wbg_set_module_4e9aad6efeeca4bd: function(arg0, arg1) {
            arg0.module = arg1;
        },
        __wbg_set_module_7d4f315495215525: function(arg0, arg1) {
            arg0.module = arg1;
        },
        __wbg_set_module_b3b6d4799f822254: function(arg0, arg1) {
            arg0.module = arg1;
        },
        __wbg_set_multisample_565e28455efa173b: function(arg0, arg1) {
            arg0.multisample = arg1;
        },
        __wbg_set_multisampled_8fdec6d4c49064e8: function(arg0, arg1) {
            arg0.multisampled = arg1 !== 0;
        },
        __wbg_set_offset_47d7f62f73946d8e: function(arg0, arg1) {
            arg0.offset = arg1;
        },
        __wbg_set_offset_c1fe83b01b1d6f8f: function(arg0, arg1) {
            arg0.offset = arg1;
        },
        __wbg_set_offset_e62da56bdb8ac5c0: function(arg0, arg1) {
            arg0.offset = arg1;
        },
        __wbg_set_offset_f502859a0ff45f9c: function(arg0, arg1) {
            arg0.offset = arg1;
        },
        __wbg_set_operation_257961aa26bfce3d: function(arg0, arg1) {
            arg0.operation = __wbindgen_enum_GpuBlendOperation[arg1];
        },
        __wbg_set_origin_50f74da3f2b380e4: function(arg0, arg1) {
            arg0.origin = arg1;
        },
        __wbg_set_origin_8972e41e53f05b3f: function(arg0, arg1) {
            arg0.origin = arg1;
        },
        __wbg_set_origin_c8ebf5a7c2e9f4ae: function(arg0, arg1) {
            arg0.origin = arg1;
        },
        __wbg_set_pass_op_4c44c1be6717480e: function(arg0, arg1) {
            arg0.passOp = __wbindgen_enum_GpuStencilOperation[arg1];
        },
        __wbg_set_power_preference_8b42b6dc921cb4a7: function(arg0, arg1) {
            arg0.powerPreference = __wbindgen_enum_GpuPowerPreference[arg1];
        },
        __wbg_set_premultiplied_alpha_618b9297f5a55779: function(arg0, arg1) {
            arg0.premultipliedAlpha = arg1 !== 0;
        },
        __wbg_set_primitive_9a376d517fb69f8e: function(arg0, arg1) {
            arg0.primitive = arg1;
        },
        __wbg_set_query_set_6c0325c1de92ed4b: function(arg0, arg1) {
            arg0.querySet = arg1;
        },
        __wbg_set_query_set_cd3a39edbc480d3e: function(arg0, arg1) {
            arg0.querySet = arg1;
        },
        __wbg_set_r_15de75ca19c25d73: function(arg0, arg1) {
            arg0.r = arg1;
        },
        __wbg_set_required_features_f660879f8c5c6c7a: function(arg0, arg1) {
            arg0.requiredFeatures = arg1;
        },
        __wbg_set_resolve_target_11fc249c55922e6c: function(arg0, arg1) {
            arg0.resolveTarget = arg1;
        },
        __wbg_set_resource_98d41d7288d15628: function(arg0, arg1) {
            arg0.resource = arg1;
        },
        __wbg_set_rows_per_image_1d6c751d4311dda5: function(arg0, arg1) {
            arg0.rowsPerImage = arg1 >>> 0;
        },
        __wbg_set_rows_per_image_80b047defd649730: function(arg0, arg1) {
            arg0.rowsPerImage = arg1 >>> 0;
        },
        __wbg_set_sample_count_8bf0aced318f75cf: function(arg0, arg1) {
            arg0.sampleCount = arg1 >>> 0;
        },
        __wbg_set_sample_type_5e8c83b3982d5b13: function(arg0, arg1) {
            arg0.sampleType = __wbindgen_enum_GpuTextureSampleType[arg1];
        },
        __wbg_set_sampler_f3e00fbae3341660: function(arg0, arg1) {
            arg0.sampler = arg1;
        },
        __wbg_set_shader_location_c8764af8701be4be: function(arg0, arg1) {
            arg0.shaderLocation = arg1 >>> 0;
        },
        __wbg_set_size_5935a380ade86764: function(arg0, arg1) {
            arg0.size = arg1;
        },
        __wbg_set_size_8eefee8dd3e9c98a: function(arg0, arg1) {
            arg0.size = arg1;
        },
        __wbg_set_size_c7cb8a5f091b7c17: function(arg0, arg1) {
            arg0.size = arg1;
        },
        __wbg_set_source_e0dae26c20cdf581: function(arg0, arg1) {
            arg0.source = arg1;
        },
        __wbg_set_src_factor_7bbead208b1dcbad: function(arg0, arg1) {
            arg0.srcFactor = __wbindgen_enum_GpuBlendFactor[arg1];
        },
        __wbg_set_stencil_back_36b4b856413661ee: function(arg0, arg1) {
            arg0.stencilBack = arg1;
        },
        __wbg_set_stencil_clear_value_5a46e5231c950c57: function(arg0, arg1) {
            arg0.stencilClearValue = arg1 >>> 0;
        },
        __wbg_set_stencil_front_adbd157696e00df7: function(arg0, arg1) {
            arg0.stencilFront = arg1;
        },
        __wbg_set_stencil_load_op_27d55fa35092de44: function(arg0, arg1) {
            arg0.stencilLoadOp = __wbindgen_enum_GpuLoadOp[arg1];
        },
        __wbg_set_stencil_read_mask_4352e8e42541e091: function(arg0, arg1) {
            arg0.stencilReadMask = arg1 >>> 0;
        },
        __wbg_set_stencil_read_only_843090d8d3d5c2dc: function(arg0, arg1) {
            arg0.stencilReadOnly = arg1 !== 0;
        },
        __wbg_set_stencil_store_op_6609e9b6d3e58a37: function(arg0, arg1) {
            arg0.stencilStoreOp = __wbindgen_enum_GpuStoreOp[arg1];
        },
        __wbg_set_stencil_write_mask_81d6d1725ed7bc0e: function(arg0, arg1) {
            arg0.stencilWriteMask = arg1 >>> 0;
        },
        __wbg_set_step_mode_4eb8a4fbd63ebb42: function(arg0, arg1) {
            arg0.stepMode = __wbindgen_enum_GpuVertexStepMode[arg1];
        },
        __wbg_set_storage_texture_ba253fab68335a24: function(arg0, arg1) {
            arg0.storageTexture = arg1;
        },
        __wbg_set_store_op_45ae2a101a70bfda: function(arg0, arg1) {
            arg0.storeOp = __wbindgen_enum_GpuStoreOp[arg1];
        },
        __wbg_set_strip_index_format_620d3dc01497c3c7: function(arg0, arg1) {
            arg0.stripIndexFormat = __wbindgen_enum_GpuIndexFormat[arg1];
        },
        __wbg_set_targets_0cb6e6d8bca07705: function(arg0, arg1) {
            arg0.targets = arg1;
        },
        __wbg_set_texture_89fcc2823ee5374f: function(arg0, arg1) {
            arg0.texture = arg1;
        },
        __wbg_set_texture_a66b08066bb848b8: function(arg0, arg1) {
            arg0.texture = arg1;
        },
        __wbg_set_texture_ba6926351f924b1b: function(arg0, arg1) {
            arg0.texture = arg1;
        },
        __wbg_set_timestamp_writes_5b97b5634c345fb0: function(arg0, arg1) {
            arg0.timestampWrites = arg1;
        },
        __wbg_set_timestamp_writes_c1b814e44722f243: function(arg0, arg1) {
            arg0.timestampWrites = arg1;
        },
        __wbg_set_topology_c84e27b9a4c043e0: function(arg0, arg1) {
            arg0.topology = __wbindgen_enum_GpuPrimitiveTopology[arg1];
        },
        __wbg_set_type_372821bc8cc754e3: function(arg0, arg1) {
            arg0.type = __wbindgen_enum_GpuSamplerBindingType[arg1];
        },
        __wbg_set_type_edbf418116f79f05: function(arg0, arg1) {
            arg0.type = __wbindgen_enum_GpuBufferBindingType[arg1];
        },
        __wbg_set_unclipped_depth_ab7fa33136ac1c4d: function(arg0, arg1) {
            arg0.unclippedDepth = arg1 !== 0;
        },
        __wbg_set_usage_116f3b3eeb837639: function(arg0, arg1) {
            arg0.usage = arg1 >>> 0;
        },
        __wbg_set_usage_26e068eee3eb7aa5: function(arg0, arg1) {
            arg0.usage = arg1 >>> 0;
        },
        __wbg_set_usage_8373b50687c41cc8: function(arg0, arg1) {
            arg0.usage = arg1 >>> 0;
        },
        __wbg_set_usage_ac4564d5994c0113: function(arg0, arg1) {
            arg0.usage = arg1 >>> 0;
        },
        __wbg_set_vertex_1c82e6b1b7add93d: function(arg0, arg1) {
            arg0.vertex = arg1;
        },
        __wbg_set_view_6e1173ef4eb34b6a: function(arg0, arg1) {
            arg0.view = arg1;
        },
        __wbg_set_view_cb05ca91c6638ae5: function(arg0, arg1) {
            arg0.view = arg1;
        },
        __wbg_set_view_dimension_82a35b4842fa7aef: function(arg0, arg1) {
            arg0.viewDimension = __wbindgen_enum_GpuTextureViewDimension[arg1];
        },
        __wbg_set_view_dimension_f7ceddf7086605ad: function(arg0, arg1) {
            arg0.viewDimension = __wbindgen_enum_GpuTextureViewDimension[arg1];
        },
        __wbg_set_view_formats_b5665e8eee6b4744: function(arg0, arg1) {
            arg0.viewFormats = arg1;
        },
        __wbg_set_view_formats_df3beba40b501e93: function(arg0, arg1) {
            arg0.viewFormats = arg1;
        },
        __wbg_set_visibility_8f4690474f84b8b6: function(arg0, arg1) {
            arg0.visibility = arg1 >>> 0;
        },
        __wbg_set_width_35673e3d57c8d7ee: function(arg0, arg1) {
            arg0.width = arg1 >>> 0;
        },
        __wbg_set_width_7f65ced2ffeee343: function(arg0, arg1) {
            arg0.width = arg1 >>> 0;
        },
        __wbg_set_width_ae28c0c10381c919: function(arg0, arg1) {
            arg0.width = arg1 >>> 0;
        },
        __wbg_set_write_mask_84529bba42e8e0a7: function(arg0, arg1) {
            arg0.writeMask = arg1 >>> 0;
        },
        __wbg_set_x_10e4ac81b3e57e69: function(arg0, arg1) {
            arg0.x = arg1 >>> 0;
        },
        __wbg_set_x_14ff131fc40f874f: function(arg0, arg1) {
            arg0.x = arg1 >>> 0;
        },
        __wbg_set_y_4cfa2f4c6e2f9bca: function(arg0, arg1) {
            arg0.y = arg1 >>> 0;
        },
        __wbg_set_y_9a2ba407bf75a350: function(arg0, arg1) {
            arg0.y = arg1 >>> 0;
        },
        __wbg_set_z_2728ad6304da794b: function(arg0, arg1) {
            arg0.z = arg1 >>> 0;
        },
        __wbg_size_8d41f87c9c90bd48: function(arg0) {
            const ret = arg0.size;
            return ret;
        },
        __wbg_slice_59e856b276aecc91: function(arg0, arg1, arg2) {
            const ret = arg0.slice(arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_static_accessor_GLOBAL_THIS_14325d8cca34bb77: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_f3a1e69f9c5a7e8e: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_50cdb5b517789aca: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_d6c4126e4c244380: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_submit_d2282a878da4cdc0: function(arg0, arg1) {
            arg0.submit(arg1);
        },
        __wbg_then_a5a891fa8b478d8d: function(arg0, arg1, arg2) {
            const ret = arg0.then(arg1, arg2);
            return ret;
        },
        __wbg_then_d4163530723f56f4: function(arg0, arg1, arg2) {
            const ret = arg0.then(arg1, arg2);
            return ret;
        },
        __wbg_then_f1c954fe00733701: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_unmap_13580f2be8912dfd: function(arg0) {
            arg0.unmap();
        },
        __wbg_usage_ef73966d48ab4b47: function(arg0) {
            const ret = arg0.usage;
            return ret;
        },
        __wbg_wgpurenderer_new: function(arg0) {
            const ret = WgpuRenderer.__wrap(arg0);
            return ret;
        },
        __wbg_width_60f44a816d7f9267: function(arg0) {
            const ret = arg0.width;
            return ret;
        },
        __wbg_writeBuffer_899ac52d87547dd0: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4, arg5) {
            arg0.writeBuffer(arg1, arg2, arg3, arg4, arg5);
        }, arguments); },
        __wbg_writeTexture_84f4cd381d5e0ba8: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.writeTexture(arg1, arg2, arg3, arg4);
        }, arguments); },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 749, function: Function { arguments: [Externref], shim_idx: 750, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_7a0033f4c224aadb___closure__destroy___dyn_core_1ee01b24a2067afc___ops__function__FnMut__wasm_bindgen_7a0033f4c224aadb___JsValue____Output_______, wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___wasm_bindgen_7a0033f4c224aadb___JsValue_____);
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 764, function: Function { arguments: [Externref], shim_idx: 1841, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen_7a0033f4c224aadb___closure__destroy___dyn_core_1ee01b24a2067afc___ops__function__FnMut__wasm_bindgen_7a0033f4c224aadb___JsValue____Output___core_1ee01b24a2067afc___result__Result_____wasm_bindgen_7a0033f4c224aadb___JsError___, wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___wasm_bindgen_7a0033f4c224aadb___JsValue__core_1ee01b24a2067afc___result__Result_____wasm_bindgen_7a0033f4c224aadb___JsError__);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Ref(Slice(U8)) -> NamedExternref("Uint8Array")`.
            const ret = getArrayU8FromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./lazuli_web_bg.js": import0,
    };
}

function wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___bool_(arg0, arg1) {
    const ret = wasm.wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___bool_(arg0, arg1);
    return ret !== 0;
}

function wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___wasm_bindgen_7a0033f4c224aadb___JsValue_____(arg0, arg1, arg2) {
    wasm.wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___wasm_bindgen_7a0033f4c224aadb___JsValue_____(arg0, arg1, arg2);
}

function wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___wasm_bindgen_7a0033f4c224aadb___JsValue__core_1ee01b24a2067afc___result__Result_____wasm_bindgen_7a0033f4c224aadb___JsError__(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___wasm_bindgen_7a0033f4c224aadb___JsValue__core_1ee01b24a2067afc___result__Result_____wasm_bindgen_7a0033f4c224aadb___JsError__(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___js_sys_a2cf80ff77c7437f___Function_fn_wasm_bindgen_7a0033f4c224aadb___JsValue_____wasm_bindgen_7a0033f4c224aadb___sys__Undefined___js_sys_a2cf80ff77c7437f___Function_fn_wasm_bindgen_7a0033f4c224aadb___JsValue_____wasm_bindgen_7a0033f4c224aadb___sys__Undefined______(arg0, arg1, arg2, arg3) {
    wasm.wasm_bindgen_7a0033f4c224aadb___convert__closures_____invoke___js_sys_a2cf80ff77c7437f___Function_fn_wasm_bindgen_7a0033f4c224aadb___JsValue_____wasm_bindgen_7a0033f4c224aadb___sys__Undefined___js_sys_a2cf80ff77c7437f___Function_fn_wasm_bindgen_7a0033f4c224aadb___JsValue_____wasm_bindgen_7a0033f4c224aadb___sys__Undefined______(arg0, arg1, arg2, arg3);
}


const __wbindgen_enum_GpuAddressMode = ["clamp-to-edge", "repeat", "mirror-repeat"];


const __wbindgen_enum_GpuBlendFactor = ["zero", "one", "src", "one-minus-src", "src-alpha", "one-minus-src-alpha", "dst", "one-minus-dst", "dst-alpha", "one-minus-dst-alpha", "src-alpha-saturated", "constant", "one-minus-constant", "src1", "one-minus-src1", "src1-alpha", "one-minus-src1-alpha"];


const __wbindgen_enum_GpuBlendOperation = ["add", "subtract", "reverse-subtract", "min", "max"];


const __wbindgen_enum_GpuBufferBindingType = ["uniform", "storage", "read-only-storage"];


const __wbindgen_enum_GpuCanvasAlphaMode = ["opaque", "premultiplied"];


const __wbindgen_enum_GpuCompareFunction = ["never", "less", "equal", "less-equal", "greater", "not-equal", "greater-equal", "always"];


const __wbindgen_enum_GpuCullMode = ["none", "front", "back"];


const __wbindgen_enum_GpuFilterMode = ["nearest", "linear"];


const __wbindgen_enum_GpuFrontFace = ["ccw", "cw"];


const __wbindgen_enum_GpuIndexFormat = ["uint16", "uint32"];


const __wbindgen_enum_GpuLoadOp = ["load", "clear"];


const __wbindgen_enum_GpuMipmapFilterMode = ["nearest", "linear"];


const __wbindgen_enum_GpuPowerPreference = ["low-power", "high-performance"];


const __wbindgen_enum_GpuPrimitiveTopology = ["point-list", "line-list", "line-strip", "triangle-list", "triangle-strip"];


const __wbindgen_enum_GpuSamplerBindingType = ["filtering", "non-filtering", "comparison"];


const __wbindgen_enum_GpuStencilOperation = ["keep", "zero", "replace", "invert", "increment-clamp", "decrement-clamp", "increment-wrap", "decrement-wrap"];


const __wbindgen_enum_GpuStorageTextureAccess = ["write-only", "read-only", "read-write"];


const __wbindgen_enum_GpuStoreOp = ["store", "discard"];


const __wbindgen_enum_GpuTextureAspect = ["all", "stencil-only", "depth-only"];


const __wbindgen_enum_GpuTextureDimension = ["1d", "2d", "3d"];


const __wbindgen_enum_GpuTextureFormat = ["r8unorm", "r8snorm", "r8uint", "r8sint", "r16uint", "r16sint", "r16float", "rg8unorm", "rg8snorm", "rg8uint", "rg8sint", "r32uint", "r32sint", "r32float", "rg16uint", "rg16sint", "rg16float", "rgba8unorm", "rgba8unorm-srgb", "rgba8snorm", "rgba8uint", "rgba8sint", "bgra8unorm", "bgra8unorm-srgb", "rgb9e5ufloat", "rgb10a2uint", "rgb10a2unorm", "rg11b10ufloat", "rg32uint", "rg32sint", "rg32float", "rgba16uint", "rgba16sint", "rgba16float", "rgba32uint", "rgba32sint", "rgba32float", "stencil8", "depth16unorm", "depth24plus", "depth24plus-stencil8", "depth32float", "depth32float-stencil8", "bc1-rgba-unorm", "bc1-rgba-unorm-srgb", "bc2-rgba-unorm", "bc2-rgba-unorm-srgb", "bc3-rgba-unorm", "bc3-rgba-unorm-srgb", "bc4-r-unorm", "bc4-r-snorm", "bc5-rg-unorm", "bc5-rg-snorm", "bc6h-rgb-ufloat", "bc6h-rgb-float", "bc7-rgba-unorm", "bc7-rgba-unorm-srgb", "etc2-rgb8unorm", "etc2-rgb8unorm-srgb", "etc2-rgb8a1unorm", "etc2-rgb8a1unorm-srgb", "etc2-rgba8unorm", "etc2-rgba8unorm-srgb", "eac-r11unorm", "eac-r11snorm", "eac-rg11unorm", "eac-rg11snorm", "astc-4x4-unorm", "astc-4x4-unorm-srgb", "astc-5x4-unorm", "astc-5x4-unorm-srgb", "astc-5x5-unorm", "astc-5x5-unorm-srgb", "astc-6x5-unorm", "astc-6x5-unorm-srgb", "astc-6x6-unorm", "astc-6x6-unorm-srgb", "astc-8x5-unorm", "astc-8x5-unorm-srgb", "astc-8x6-unorm", "astc-8x6-unorm-srgb", "astc-8x8-unorm", "astc-8x8-unorm-srgb", "astc-10x5-unorm", "astc-10x5-unorm-srgb", "astc-10x6-unorm", "astc-10x6-unorm-srgb", "astc-10x8-unorm", "astc-10x8-unorm-srgb", "astc-10x10-unorm", "astc-10x10-unorm-srgb", "astc-12x10-unorm", "astc-12x10-unorm-srgb", "astc-12x12-unorm", "astc-12x12-unorm-srgb"];


const __wbindgen_enum_GpuTextureSampleType = ["float", "unfilterable-float", "depth", "sint", "uint"];


const __wbindgen_enum_GpuTextureViewDimension = ["1d", "2d", "2d-array", "cube", "cube-array", "3d"];


const __wbindgen_enum_GpuVertexFormat = ["uint8", "uint8x2", "uint8x4", "sint8", "sint8x2", "sint8x4", "unorm8", "unorm8x2", "unorm8x4", "snorm8", "snorm8x2", "snorm8x4", "uint16", "uint16x2", "uint16x4", "sint16", "sint16x2", "sint16x4", "unorm16", "unorm16x2", "unorm16x4", "snorm16", "snorm16x2", "snorm16x4", "float16", "float16x2", "float16x4", "float32", "float32x2", "float32x3", "float32x4", "uint32", "uint32x2", "uint32x3", "uint32x4", "sint32", "sint32x2", "sint32x3", "sint32x4", "unorm10-10-10-2", "unorm8x4-bgra"];


const __wbindgen_enum_GpuVertexStepMode = ["vertex", "instance"];
const GxRendererHandleFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_gxrendererhandle_free(ptr >>> 0, 1));
const WasmEmulatorFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmemulator_free(ptr >>> 0, 1));
const WgpuRendererFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wgpurenderer_free(ptr >>> 0, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

function _assertClass(instance, klass) {
    if (!(instance instanceof klass)) {
        throw new Error(`expected instance of ${klass.name}`);
    }
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => state.dtor(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function getArrayU32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint32ArrayMemory0 = null;
function getUint32ArrayMemory0() {
    if (cachedUint32ArrayMemory0 === null || cachedUint32ArrayMemory0.byteLength === 0) {
        cachedUint32ArrayMemory0 = new Uint32Array(wasm.memory.buffer);
    }
    return cachedUint32ArrayMemory0;
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, dtor, f) {
    const state = { a: arg0, b: arg1, cnt: 1, dtor };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            state.dtor(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint32ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('lazuli_web_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
