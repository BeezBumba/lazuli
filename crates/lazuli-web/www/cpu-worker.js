/**
 * cpu-worker.js — CPU execution worker for the Lazuli GameCube emulator.
 *
 * Runs the PowerPC JIT execution loop in a dedicated Web Worker, freeing
 * the main thread for rendering, input polling, and UI updates.
 *
 * ## Architecture
 *
 * The Worker owns the full emulator state (WasmEmulator, JIT module cache,
 * hardware peripherals).  It communicates with the main thread exclusively
 * via structured messages:
 *
 * ### Main → Worker
 *   { type: 'init',              ramSize }
 *   { type: 'load_iso',          isoData: ArrayBuffer, iplHleData: ArrayBuffer }
 *   { type: 'load_sram',         data: Uint8Array }
 *   { type: 'start' }
 *   { type: 'stop' }
 *   { type: 'input',             padButtons: u32 }
 *   { type: 'analog_axes',      jx: u8, jy: u8, cx: u8, cy: u8, lt: u8, rt: u8 }
 *   { type: 'reset',             pc: u32 }
 *   { type: 'step' }
 *   { type: 'run_n',             n: number }
 *   { type: 'compile_demo',      words: string[], basePc: u32 }
 *   { type: 'exec_demo',         words: string[], basePc: u32 }
 *   { type: 'breakpoint_add',    pc: u32 }
 *   { type: 'breakpoint_remove', pc: u32 }
 *   { type: 'breakpoint_clear' }
 *   { type: 'clear_cache' }
 *   { type: 'pcm_sab',           pcmSab: SharedArrayBuffer, idxSab: SharedArrayBuffer }
 *
 * ### Worker → Main
 *   { type: 'ready' }
 *   { type: 'frame',         rgba: ArrayBuffer, width, height }   (transferable)
 *   { type: 'no_frame',      gameTitle }
 *   { type: 'stats',         pc, lr, ctr, cr, msr, srr0, srr1, dec,
 *                            gprs, blocksCompiled, blocksExecuted, cacheSize,
 *                            stuckRuns, lastExcPc, lastExcKind,
 *                            unimplBlockCount, raiseExcCount, padButtons }
 *   { type: 'uart',          bytes: number[] }
 *   { type: 'apploader_log', line: string }
 *   { type: 'debug_event',   msg: string }
 *   { type: 'status',        msg: string, cls: string }
 *   { type: 'milestone',     name: string, elapsed: string }
 *   { type: 'phase',         from: string, to: string, pc: number, blockCount: number }
 *   { type: 'error',         msg: string, pc: number }
 *   { type: 'stopped' }
 *   { type: 'sram',          data: Uint8Array }
 *   { type: 'step_done',     ok: boolean, log: string[], pc: number }
 *   { type: 'run_done',      count: number, pc: number }
 *   { type: 'block_compiled',wasmBytes: ArrayBuffer, cycles: number, insCount: number }
 *   { type: 'iso_loaded',    gameName, gameId, entry, error? }
 */

import init, { WasmEmulator, wasm_memory } from "./pkg/lazuli_web.js";

// ── Constants ─────────────────────────────────────────────────────────────────

const BLOCKS_PER_FRAME         = 500;
const TIMEBASE_TICKS_PER_FRAME = 675_000;
const TICKS_PER_BLOCK_FALLBACK = Math.ceil(TIMEBASE_TICKS_PER_FRAME / BLOCKS_PER_FRAME);
const WASM_PAGE                = 65_536;
const PHYS_MASK                = 0x01FF_FFFF;
const SCREEN_W                 = 640;
const SCREEN_H                 = 480;
const XFB_BYTE_SIZE            = SCREEN_W * SCREEN_H * 2;
const XFB_PHYS_DEFAULT         = 0x00C0_0000;
const STUCK_PC_THRESHOLD       = 50;
const STUCK_REDUMP_MULTIPLIER  = 10;
const STUCK_EXCEPTION_VECTOR_MULTIPLIER = 10;
const RAISE_EXCEPTION_LOG_LIMIT = 30;
const MMIO_RING_SIZE            = 8;

// Phase 3 — SAB audio ring buffer (power-of-two so bitwise & replaces modulo)
const PCM_RING_SIZE = 8192;
const PCM_RING_MASK = PCM_RING_SIZE - 1;

// ── State ─────────────────────────────────────────────────────────────────────

let emu        = null;
let running    = false;
let intervalId = null;
let gameTitle  = null;
let frameCount = 0;

// Phase 3 audio SABs (set via 'pcm_sab' message from main thread)
let pcmSab    = null;  // Float32 ring: [L[0..RING], R[0..RING]]
let pcmIdxSab = null;  // Int32[2]: [writeHead, readHead]

// JIT block cache
const moduleCache  = new Map(); // u32 pc → WebAssembly.Module
const blockMetaMap = new Map(); // u32 pc → { cycles, insCount }
const pcHitMap     = new Map(); // u32 pc → number

// CPU register-file memory (reused across block executions)
let regsMemCache = null;

// RAM view (zero-copy slice into WASM linear memory)
let ramView          = null;
let lastMemoryBuffer = null;

// L2 cache-as-RAM view
let l2cView = null;

// Diagnostic state
let raiseExceptionTotal     = 0;
let lastRaisedExceptionPc   = 0;
let lastRaisedExceptionKind = -1;
let lastNextPc              = 0;
let stuckConsecutiveRuns    = 0;
let recentMmioAccesses      = [];
let prevPhase               = "unknown";
let blocksExecutedTotal     = 0; // local counter for frame counting

// Milestones (mirrors bootstrap.js milestones object)
const milestones = {
  startedAt:         null,
  iplHleStarted:     null,
  apploaderRunning:  null,
  apploaderDone:     null,
  gameEntry:         null,
  firstXfbContent:   null,
};

// XFB state
let xfbHasContent = false;
let xfbAddr       = XFB_PHYS_DEFAULT;

// Post-entry OS banner watch
let osUartActiveSeen = false;
let osInitBannerDone = false;
let osPostEntryFrames = 0;
let totalUartBytes    = 0;
let uartBytesAtGameEntry = 0;

// Breakpoints
const breakpoints = new Set();

// ── Helpers ───────────────────────────────────────────────────────────────────

const hexU32 = (v) => "0x" + (v >>> 0).toString(16).toUpperCase().padStart(8, "0");

function readRamU32(ram, physAddr) {
  if (physAddr + 3 >= ram.length) return 0;
  return (((ram[physAddr]     << 24) |
           (ram[physAddr + 1] << 16) |
           (ram[physAddr + 2] <<  8) |
            ram[physAddr + 3]) >>> 0);
}

function classifyPc(pc) {
  pc = pc >>> 0;
  if (pc < 0x00002000)                                  return "exception-vector";
  if (pc >= 0x81300000 && pc < 0x81400000)              return "ipl-hle";
  if (pc >= 0x81200000 && pc < 0x81300000)              return "apploader";
  if (pc >= 0x80000000 && pc < 0x81000000)              return "OS/game RAM";
  if (pc >= 0xE0000000 && pc < 0xE0010000)              return "L2-cache-RAM";
  return "unknown";
}

function mmioSubsystem(addr) {
  addr = addr & 0x00FFFFFF;
  if (addr >= 0x003000 && addr < 0x003100) return "PI";
  if (addr >= 0x004000 && addr < 0x004100) return "MI";
  if (addr >= 0x005000 && addr < 0x005100) return "DSP";
  if (addr >= 0x006000 && addr < 0x006100) return "DI";
  if (addr >= 0x006400 && addr < 0x006500) return "SI";
  if (addr >= 0x006800 && addr < 0x006900) return "EXI";
  if (addr >= 0x008000 && addr < 0x008100) return "VI";
  if (addr >= 0x00C000 && addr < 0x00E000) return "GX";
  return "MMIO";
}

function postLog(level, msg) {
  // console methods work in Workers and output in DevTools.
  if (level === "error")  console.error(msg);
  else if (level === "warn") console.warn(msg);
  else                       console.log(msg);
}

function postApploaderLog(line) {
  self.postMessage({ type: "apploader_log", line });
}

function postDebugEvent(msg) {
  self.postMessage({ type: "debug_event", msg });
}

function postStatus(msg, cls = "status-info") {
  self.postMessage({ type: "status", msg, cls });
}

// ── RAM / L2 views ────────────────────────────────────────────────────────────

function getRamView() {
  const mem = wasm_memory();
  if (!ramView || mem.buffer !== lastMemoryBuffer) {
    lastMemoryBuffer = mem.buffer;
    ramView = new Uint8Array(mem.buffer, emu.ram_ptr(), emu.ram_size());
  }
  return ramView;
}

function getL2cView() {
  const mem = wasm_memory();
  if (!l2cView || mem.buffer !== lastMemoryBuffer) {
    l2cView = new Uint8Array(mem.buffer, emu.l2c_ptr(), emu.l2c_size());
  }
  return l2cView;
}

function getRegsMem() {
  if (!regsMemCache) {
    const cpuSize     = emu.cpu_struct_size();
    const pagesNeeded = Math.ceil(cpuSize / WASM_PAGE);
    const mem         = new WebAssembly.Memory({ initial: pagesNeeded });
    regsMemCache      = { mem, view: new Uint8Array(mem.buffer) };
  }
  return regsMemCache;
}

// ── UART output ───────────────────────────────────────────────────────────────

let stdoutLineBuffer = "";

function feedStdoutByte(ch) {
  if (ch === 10 /* '\n' */) {
    if (stdoutLineBuffer.length > 0) {
      postApploaderLog(`[UART] ${stdoutLineBuffer}`);
      stdoutLineBuffer = "";
    }
  } else {
    stdoutLineBuffer += String.fromCharCode(ch);
    if (stdoutLineBuffer.length > 256) {
      postApploaderLog(`[UART] ${stdoutLineBuffer}`);
      stdoutLineBuffer = "";
    }
  }
}

function flushStdoutLineBuffer() {
  if (stdoutLineBuffer.length > 0) {
    postApploaderLog(`[UART] ${stdoutLineBuffer}`);
    stdoutLineBuffer = "";
  }
}

function drainUartOutput() {
  const bytes = emu.take_uart_output();
  if (!bytes || bytes.length === 0) return;
  totalUartBytes += bytes.length;
  // Send raw bytes so the main thread can feed them through its own pipeline.
  self.postMessage({ type: "uart", bytes: Array.from(bytes) });
}

// ── Hook closure factory ──────────────────────────────────────────────────────

function buildHooks(ram, log, numericPc, pcContext = "?") {
  let r   = ram;
  const l2c   = getL2cView();
  const L2C_BASE = 0xE000_0000;
  const L2C_MASK = 0x0000_3FFF;

  // These hooks are called only for MMIO (0xCCxxxxxx) and L2C (0xE0xxxxxx)
  // addresses — normal RAM accesses are handled by inline WASM using the
  // ram_base global import (see ppcwasm/src/lower.rs).
  return {
    read_u8(addr) {
      addr = addr >>> 0;
      if (l2c && addr >= L2C_BASE && addr < L2C_BASE + l2c.length)
        return l2c[addr & L2C_MASK];
      return 0; // MMIO byte reads return 0 (hw_read_u8 not wired)
    },
    read_u16(addr) {
      addr = addr >>> 0;
      if (l2c && addr >= L2C_BASE && addr < L2C_BASE + l2c.length) {
        const off = addr & L2C_MASK;
        return off + 1 < l2c.length ? (l2c[off] << 8) | l2c[off + 1] : 0;
      }
      const val = emu.hw_read_u16(addr) & 0xFFFF;
      recentMmioAccesses.push({ dir: "R", size: 16, addr, subsystem: mmioSubsystem(addr), val });
      if (recentMmioAccesses.length > MMIO_RING_SIZE) recentMmioAccesses.shift();
      return val;
    },
    read_u32(addr) {
      addr = addr >>> 0;
      if (l2c && addr >= L2C_BASE && addr < L2C_BASE + l2c.length) {
        const off = addr & L2C_MASK;
        if (off + 3 < l2c.length)
          return (((l2c[off] << 24) | (l2c[off+1] << 16) | (l2c[off+2] << 8) | l2c[off+3]) >>> 0);
        return 0;
      }
      const val = emu.hw_read_u32(addr) >>> 0;
      recentMmioAccesses.push({ dir: "R", size: 32, addr, subsystem: mmioSubsystem(addr), val });
      if (recentMmioAccesses.length > MMIO_RING_SIZE) recentMmioAccesses.shift();
      return val;
    },
    read_f64(addr) {
      addr = addr >>> 0;
      if (l2c && addr >= L2C_BASE && addr < L2C_BASE + l2c.length) {
        const off = addr & L2C_MASK;
        if (off + 7 < l2c.length) {
          const view = new DataView(l2c.buffer, l2c.byteOffset + off, 8);
          return view.getFloat64(0, false);
        }
        return 0.0;
      }
      if ((addr >>> 24 & 0xFE) === 0xCC) return 0.0;
      // Fallback for any address that slips through the inline check (e.g. physical 0x00xxxxxx).
      addr &= PHYS_MASK;
      if (addr + 7 >= r.length) return 0.0;
      const view = new DataView(r.buffer, r.byteOffset + addr, 8);
      return view.getFloat64(0, false);
    },
    write_u8(addr, val) {
      addr = addr >>> 0;
      if (l2c && addr >= L2C_BASE && addr < L2C_BASE + l2c.length) {
        l2c[addr & L2C_MASK] = val & 0xFF;
        return;
      }
      // EXI stdout byte port (cached or uncached mirror)
      if ((addr >>> 24 & 0xFE) === 0xCC && (addr & 0x00FFFFFF) === 0x007000) {
        feedStdoutByte(val & 0xFF);
        return;
      }
      // Any other MMIO byte write — ignored (no hw_write_u8)
    },
    write_u16(addr, val) {
      addr = addr >>> 0;
      if (l2c && addr >= L2C_BASE && addr < L2C_BASE + l2c.length) {
        const off = addr & L2C_MASK;
        if (off + 1 < l2c.length) { l2c[off] = (val >> 8) & 0xFF; l2c[off+1] = val & 0xFF; }
        return;
      }
      recentMmioAccesses.push({ dir: "W", size: 16, addr, subsystem: mmioSubsystem(addr), val: val & 0xFFFF });
      if (recentMmioAccesses.length > MMIO_RING_SIZE) recentMmioAccesses.shift();
      emu.hw_write_u16(addr, val & 0xFFFF);
    },
    write_u32(addr, val) {
      addr = addr >>> 0;
      val  = val  >>> 0;
      if (l2c && addr >= L2C_BASE && addr < L2C_BASE + l2c.length) {
        const off = addr & L2C_MASK;
        if (off + 3 < l2c.length) {
          l2c[off]   = (val >>> 24) & 0xFF; l2c[off+1] = (val >>> 16) & 0xFF;
          l2c[off+2] = (val >>>  8) & 0xFF; l2c[off+3] =  val         & 0xFF;
        }
        return;
      }
      recentMmioAccesses.push({ dir: "W", size: 32, addr, subsystem: mmioSubsystem(addr), val });
      if (recentMmioAccesses.length > MMIO_RING_SIZE) recentMmioAccesses.shift();
      emu.hw_write_u32(addr, val);
      // DVD DMA may have overwritten guest code — selectively invalidate JIT cache.
      if (emu.take_dma_dirty()) {
        const dmaPhysStart = emu.last_dma_addr() >>> 0;
        const dmaPhysLen   = emu.last_dma_len()  >>> 0;
        const dmaPhysEnd   = dmaPhysStart + dmaPhysLen;
        for (const [vpc] of moduleCache) {
          const physPc  = (vpc & PHYS_MASK) >>> 0;
          const meta    = blockMetaMap.get(vpc);
          const blockEnd = physPc + (meta ? meta.insCount * 4 : 4);
          if (physPc < dmaPhysEnd && blockEnd > dmaPhysStart) {
            moduleCache.delete(vpc);
            blockMetaMap.delete(vpc);
          }
        }
        // Refresh RAM view in case the DMA triggered WASM linear-memory growth.
        r = getRamView();
        // Log the DMA to the main thread's apploader panel.
        const discOff = emu.last_di_disc_offset() >>> 0;
        const previewLen = Math.min(dmaPhysLen, 8);
        const preview = [];
        for (let i = 0; i < previewLen; i++)
          preview.push(r[dmaPhysStart + i].toString(16).padStart(2, "0"));
        for (let i = previewLen; i < 8; i++) preview.push("00");
        postApploaderLog(
          `[lazuli] DI: DVD Read disc_off=0x${discOff.toString(16).padStart(8,"0")}` +
          ` len=0x${dmaPhysLen.toString(16)}` +
          ` ram_dest=0x${dmaPhysStart.toString(16).padStart(8,"0")}` +
          ` data=[${preview.join(" ")}]`
        );
      }
    },
    write_f64(addr, val) {
      addr = addr >>> 0;
      if (l2c && addr >= L2C_BASE && addr < L2C_BASE + l2c.length) {
        const off = addr & L2C_MASK;
        if (off + 7 < l2c.length) {
          const view = new DataView(l2c.buffer, l2c.byteOffset + off, 8);
          view.setFloat64(0, val, false);
        }
        return;
      }
      if ((addr >>> 24 & 0xFE) === 0xCC) return;
      // Fallback for any address that slips through (e.g. physical 0x00xxxxxx).
      addr &= PHYS_MASK;
      if (addr + 7 >= r.length) return;
      const view = new DataView(r.buffer, r.byteOffset + addr, 8);
      view.setFloat64(0, val, false);
    },
    raise_exception(kind) {
      lastRaisedExceptionPc   = numericPc;
      lastRaisedExceptionKind = kind;
      if (log) log.push(`exception: kind=${kind}`);
      raiseExceptionTotal++;
      emu.record_raise_exception();
      if (raiseExceptionTotal <= RAISE_EXCEPTION_LOG_LIMIT) {
        console.warn(`[lazuli-worker] raise_exception(kind=${kind}) @ ${pcContext} (#${raiseExceptionTotal})`);
        if (raiseExceptionTotal === RAISE_EXCEPTION_LOG_LIMIT)
          console.warn("[lazuli-worker] raise_exception log limit reached");
      }
      postDebugEvent(`⚡ exception(${kind}) at ${pcContext}`);
    },

    // ── Quantized paired-single memory hooks ─────────────────────────────────
    psq_store(addr, val, gqr) {
      addr = addr >>> 0;
      gqr  = gqr  >>> 0;
      const storeType  = (gqr        ) & 7;
      const storeScale = (gqr >>>   8) & 0x3F;
      const scale = storeScale >= 32 ? -(64 - storeScale) : storeScale;

      function writeAddr(a, v) {
        a &= PHYS_MASK;
        if (a < r.length) r[a] = (v >> 8) & 0xFF;
        if (a + 1 < r.length) r[a + 1] = v & 0xFF;
      }

      switch (storeType) {
        case 0: { // float32
          const buf = new ArrayBuffer(4);
          new DataView(buf).setFloat32(0, val, false);
          const b = new Uint8Array(buf);
          const a = addr & PHYS_MASK;
          if (a + 3 < r.length) { r[a]=b[0]; r[a+1]=b[1]; r[a+2]=b[2]; r[a+3]=b[3]; }
          return 4;
        }
        case 4: { // u8
          const q = Math.min(255, Math.max(0, Math.round(val * (1 << scale)))) & 0xFF;
          if ((addr & PHYS_MASK) < r.length) r[addr & PHYS_MASK] = q;
          return 1;
        }
        case 5: { // u16
          const q = Math.min(65535, Math.max(0, Math.round(val * (1 << scale)))) & 0xFFFF;
          writeAddr(addr, q);
          return 2;
        }
        case 6: { // i8
          const q = Math.min(127, Math.max(-128, Math.round(val * (1 << scale))));
          if ((addr & PHYS_MASK) < r.length) r[addr & PHYS_MASK] = q & 0xFF;
          return 1;
        }
        case 7: { // i16
          const q = Math.min(32767, Math.max(-32768, Math.round(val * (1 << scale))));
          writeAddr(addr, q & 0xFFFF);
          return 2;
        }
        default: return 4;
      }
    },
    psq_load(addr, gqr) {
      addr = addr >>> 0;
      gqr  = gqr  >>> 0;
      const loadType  = (gqr >>> 16) & 7;
      const loadScale = (gqr >>> 24) & 0x3F;
      const scale = loadScale >= 32 ? -(64 - loadScale) : loadScale;

      switch (loadType) {
        case 0: { // float32
          const a = addr & PHYS_MASK;
          if (a + 3 >= r.length) return 0.0;
          const buf = new ArrayBuffer(4);
          new Uint8Array(buf).set(r.subarray(a, a + 4));
          return new DataView(buf).getFloat32(0, false);
        }
        case 4: { const a = addr & PHYS_MASK; return a < r.length ? r[a] / (1 << scale) : 0.0; } // u8
        case 5: { // u16
          const a = addr & PHYS_MASK;
          const v = a + 1 < r.length ? (r[a] << 8) | r[a+1] : 0;
          return v / (1 << scale);
        }
        case 6: { // i8
          const a = addr & PHYS_MASK;
          const v = a < r.length ? (r[a] << 24) >> 24 : 0;
          return v / (1 << scale);
        }
        case 7: { // i16
          const a = addr & PHYS_MASK;
          const v = a + 1 < r.length ? ((r[a] << 8) | r[a+1]) << 16 >> 16 : 0;
          return v / (1 << scale);
        }
        default: return 0.0;
      }
    },
    psq_store_size(gqr) {
      gqr = gqr >>> 0;
      switch ((gqr) & 7) {
        case 0:  return 4;
        case 4: case 6: return 1;
        case 5: case 7: return 2;
        default: return 4;
      }
    },
    psq_load_size(gqr) {
      gqr = gqr >>> 0;
      switch ((gqr >>> 16) & 7) {
        case 0:  return 4;
        case 4: case 6: return 1;
        case 5: case 7: return 2;
        default: return 4;
      }
    },

    // ── Instruction-cache management ──────────────────────────────────────────
    icbi(addr) {
      addr = addr >>> 0;
      const physAddr = addr & PHYS_MASK;
      for (const [vpc] of moduleCache) {
        const physPc = (vpc & PHYS_MASK) >>> 0;
        const meta   = blockMetaMap.get(vpc);
        const lineBase = physAddr & ~31;
        const lineEnd  = lineBase + 32;
        if (physPc >= lineBase && physPc < lineEnd) {
          moduleCache.delete(vpc); blockMetaMap.delete(vpc);
        } else if (meta) {
          const blockEnd = physPc + meta.insCount * 4;
          if (physPc < lineEnd && blockEnd > lineBase) {
            moduleCache.delete(vpc); blockMetaMap.delete(vpc);
          }
        }
      }
    },
    isync() {
      moduleCache.clear();
      blockMetaMap.clear();
    },
  };
}

// ── Block execution ───────────────────────────────────────────────────────────

function getMsrEe() {
  return (emu.get_msr() >>> 15) & 1;
}

function executeOneBlockSync(ram, log) {
  const pc    = emu.get_pc();
  const pcHex = hexU32(pc);

  let module = moduleCache.get(pc);
  if (!module) {
    let wasmBytes;
    try {
      wasmBytes = emu.compile_block(pc);
    } catch (e) {
      const msg = `compile error @ ${pcHex}: ${e}`;
      console.error(`[lazuli-worker] ${msg}`);
      if (log) log.push(`[${pcHex}] compile error: ${e}`);
      postDebugEvent(`✗ ${msg}`);
      return false;
    }
    try {
      module = new WebAssembly.Module(wasmBytes);
    } catch (e) {
      const msg = `WebAssembly.Module error @ ${pcHex}: ${e}`;
      console.error(`[lazuli-worker] ${msg}`);
      if (log) log.push(`[${pcHex}] wasm.Module error: ${e}`);
      postDebugEvent(`✗ ${msg}`);
      return false;
    }
    moduleCache.set(pc, module);
    blockMetaMap.set(pc, {
      cycles:   emu.last_compiled_cycles(),
      insCount: emu.last_compiled_ins_count(),
    });
  }

  const cpuSize = emu.cpu_struct_size();
  const { mem: regsMem, view: regsView } = getRegsMem();
  regsView.set(emu.get_cpu_bytes(), 0);

  lastRaisedExceptionKind = -1;

  let instance;
  try {
    // Provide the ram_base global so JIT blocks can use direct i32.load/i32.store
    // for RAM accesses without crossing the JavaScript boundary.
    const ramBaseGlobal = new WebAssembly.Global({ value: 'i32', mutable: false }, emu.ram_ptr());
    instance = new WebAssembly.Instance(module, {
      env:   { memory: regsMem, ram_base: ramBaseGlobal },
      hooks: buildHooks(ram, log, pc, pcHex),
    });
  } catch (e) {
    const msg = `instantiation error @ ${pcHex}: ${e}`;
    console.error(`[lazuli-worker] ${msg}`);
    if (log) log.push(`[${pcHex}] instantiation error: ${e}`);
    postDebugEvent(`✗ ${msg}`);
    return false;
  }

  let nextPc;
  try {
    nextPc = instance.exports.execute(0);
  } catch (e) {
    const msg = `execution error @ ${pcHex}: ${e}`;
    console.error(`[lazuli-worker] ${msg}`);
    if (log) log.push(`[${pcHex}] execution error: ${e}`);
    postDebugEvent(`✗ ${msg}`);
    return false;
  }

  emu.set_cpu_bytes(new Uint8Array(regsMem.buffer, 0, cpuSize));
  emu.record_block_executed();
  pcHitMap.set(pc, (pcHitMap.get(pc) ?? 0) + 1);

  drainUartOutput();

  lastNextPc = nextPc >>> 0;

  if (lastNextPc === 0 && lastRaisedExceptionKind >= 0) {
    emu.deliver_exception(lastRaisedExceptionKind);
  } else {
    const newPc = lastNextPc !== 0 ? lastNextPc : emu.get_pc();
    if (lastNextPc !== 0 && lastNextPc !== pc) {
      emu.set_pc(newPc);
    } else if (lastNextPc === 0) {
      // Dynamic branch already wrote to CPU::pc in WASM memory.
    }
    // branch-to-self: don't call set_pc, let the loop detect it.
  }

  return true;
}

// ── XFB detection and YUV422 → RGBA conversion ────────────────────────────────

function detectXfbAddress(ram) {
  const candidates = [
    XFB_PHYS_DEFAULT,
    (ram.length - XFB_BYTE_SIZE) & ~0x1F,
    (ram.length - 2 * XFB_BYTE_SIZE) & ~0x1F,
  ];
  for (const addr of candidates) {
    if (addr < 0 || addr + XFB_BYTE_SIZE > ram.length) continue;
    for (let i = addr; i < addr + 64; i += 4) {
      if (ram[i] !== 0 || ram[i+1] !== 0 || ram[i+2] !== 0 || ram[i+3] !== 0)
        return addr;
    }
  }
  return -1;
}

function convertXfbToRgba(ram, xfbAddr) {
  const rgba  = new Uint8ClampedArray(SCREEN_W * SCREEN_H * 4);
  const pairs = (SCREEN_W * SCREEN_H) >>> 1;
  const clamp = (v) => v < 0 ? 0 : v > 255 ? 255 : v;

  for (let i = 0; i < pairs; i++) {
    const base  = xfbAddr + i * 4;
    const cb    = ram[base];
    const y0    = ram[base + 1];
    const cr    = ram[base + 2];
    const y1    = ram[base + 3];
    const cbOff = cb - 128;
    const crOff = cr - 128;

    const r0 = clamp(y0 + ((1402 * crOff) >> 10));
    const g0 = clamp(y0 - ((344  * cbOff) >> 10) - ((714 * crOff) >> 10));
    const b0 = clamp(y0 + ((1772 * cbOff) >> 10));
    const r1 = clamp(y1 + ((1402 * crOff) >> 10));
    const g1 = clamp(y1 - ((344  * cbOff) >> 10) - ((714 * crOff) >> 10));
    const b1 = clamp(y1 + ((1772 * cbOff) >> 10));

    const p = i * 8;
    rgba[p]   = r0; rgba[p+1] = g0; rgba[p+2] = b0; rgba[p+3] = 255;
    rgba[p+4] = r1; rgba[p+5] = g1; rgba[p+6] = b1; rgba[p+7] = 255;
  }
  return rgba;
}

// ── Phase 3: SAB audio ring buffer ────────────────────────────────────────────

/**
 * Write interleaved stereo PCM samples into the SharedArrayBuffer ring buffer.
 *
 * Accepts either:
 *   - An interleaved `Float32Array` [L0, R0, L1, R1, …] from
 *     `emu.take_audio_samples(n)`.
 *   - Two separate `left`/`right` arrays (legacy path; kept for compatibility).
 *
 * The SAB layout is [L[0..RING-1], R[0..RING-1]] as Float32.
 *
 * @param {Float32Array} leftOrInterleaved  Left channel, or interleaved L/R
 * @param {Float32Array} [right]            Right channel (omit for interleaved)
 */
function pushPcmToSab(leftOrInterleaved, right) {
  if (!pcmSab || !pcmIdxSab) return;

  const data  = new Float32Array(pcmSab);
  const idx   = new Int32Array(pcmIdxSab);
  const wHead = Atomics.load(idx, 0);
  const rHead = Atomics.load(idx, 1);
  const avail = (wHead - rHead + PCM_RING_SIZE) & PCM_RING_MASK;
  const free  = PCM_RING_SIZE - avail - 1;

  if (right === undefined) {
    // Interleaved path: [L0, R0, L1, R1, …] from take_audio_samples().
    const nStereo = leftOrInterleaved.length >> 1;
    const n       = Math.min(nStereo, free);
    for (let i = 0; i < n; i++) {
      const pos              = (wHead + i) & PCM_RING_MASK;
      data[pos]              = leftOrInterleaved[i * 2];
      data[pos + PCM_RING_SIZE] = leftOrInterleaved[i * 2 + 1];
    }
    Atomics.store(idx, 0, (wHead + n) & PCM_RING_MASK);
  } else {
    // Separate left/right path (legacy).
    const n = Math.min(leftOrInterleaved.length, free);
    for (let i = 0; i < n; i++) {
      const pos              = (wHead + i) & PCM_RING_MASK;
      data[pos]              = leftOrInterleaved[i];
      data[pos + PCM_RING_SIZE] = right[i];
    }
    Atomics.store(idx, 0, (wHead + n) & PCM_RING_MASK);
  }
}

// ── ISO loading (mirrors parseAndLoadIso in bootstrap.js) ─────────────────────

function parseDol(view, bytes, dolOffset) {
  const entry     = view.getUint32(dolOffset + 0xE0, false);
  const bssTarget = view.getUint32(dolOffset + 0xD8, false);
  const bssSize   = view.getUint32(dolOffset + 0xDC, false);
  const sections  = [];
  // 7 text + 11 data section headers
  for (let i = 0; i < 18; i++) {
    const fileOff  = view.getUint32(dolOffset + i * 4,        false);
    const loadAddr = view.getUint32(dolOffset + 0x48 + i * 4, false);
    const size     = view.getUint32(dolOffset + 0x90 + i * 4, false);
    if (size > 0 && fileOff > 0) sections.push({ fileOff, loadAddr, size });
  }
  return { entry, bssTarget, bssSize, sections };
}

function parseAndLoadIso(arrayBuffer, iplHleDol) {
  const view  = new DataView(arrayBuffer);
  const bytes = new Uint8Array(arrayBuffer);

  const magic = view.getUint32(0x1C, false);
  if (magic !== 0xC233_9F3D)
    throw new Error(`Not a valid GameCube ISO — magic word mismatch (got 0x${magic.toString(16).toUpperCase()})`);

  const gameId = String.fromCharCode(
    ...bytes.slice(0, 6).filter(b => b >= 0x20 && b <= 0x7E),
  );

  let gameName = "";
  for (let i = 0x020; i < 0x400 && bytes[i] !== 0; i++)
    gameName += String.fromCharCode(bytes[i]);

  console.log(`[lazuli-worker] ISO: "${gameName}" (${gameId}), ${(arrayBuffer.byteLength / (1024*1024)).toFixed(1)} MiB`);
  postApploaderLog(`[IPL-HLE] ISO: "${gameName}" (${gameId}), ${(arrayBuffer.byteLength / (1024*1024)).toFixed(1)} MiB`);

  const dolOffset = view.getUint32(0x420, false);
  if (dolOffset === 0 || dolOffset >= arrayBuffer.byteLength)
    throw new Error(`Invalid DOL offset 0x${dolOffset.toString(16)} in ISO header`);

  emu.load_bytes(0x80000000, bytes.slice(0, 0x440));

  const dol = parseDol(view, bytes, dolOffset);
  console.log(
    `[lazuli-worker] DOL entry=0x${dol.entry.toString(16).toUpperCase().padStart(8,"0")}` +
    ` sections=${dol.sections.length}`,
  );

  // Synthetic Dolphin OS globals (mirrors bootstrap.js parseAndLoadIso)
  {
    const buf  = new ArrayBuffer(0x100);
    const bv   = new DataView(buf);
    bv.setUint32(0x20, 0x0D15EA5E, false);
    bv.setUint32(0x24, 0x00000001, false);
    bv.setUint32(0x28, 0x01800000, false);
    bv.setUint32(0x2C, 0x10000005, false);
    bv.setUint32(0x30, 0x8042E260, false);
    bv.setUint32(0x34, 0x817FE8C0, false);
    bv.setUint32(0x38, 0x817FE8C0, false);
    bv.setUint32(0x3C, 0x00000024, false);
    bv.setUint32(0xCC, 0x00000000, false);
    bv.setUint32(0xD0, 0x01000000, false);
    bv.setUint32(0xF8, 0x09A7EC80, false);
    bv.setUint32(0xFC, 0x1CF7C580, false);
    emu.load_bytes(0x80000020, new Uint8Array(buf, 0x20, 0xE0));
  }

  const APPLOADER_ISO_OFFSET = 0x2440;
  const apploaderEntrypoint  = view.getUint32(APPLOADER_ISO_OFFSET + 0x10, false);
  const apploaderSize        = view.getUint32(APPLOADER_ISO_OFFSET + 0x14, false);
  const apploaderBodyOffset  = APPLOADER_ISO_OFFSET + 0x20;
  if (apploaderSize === 0 || apploaderBodyOffset + apploaderSize > arrayBuffer.byteLength)
    throw new Error(`Invalid apploader in ISO: size=0x${apploaderSize.toString(16)}`);

  const apploaderVersionEnd = bytes.indexOf(0, APPLOADER_ISO_OFFSET);
  const apploaderVersion    = String.fromCharCode(
    ...bytes.slice(APPLOADER_ISO_OFFSET, Math.min(apploaderVersionEnd, APPLOADER_ISO_OFFSET + 0x10)).filter(b => b > 0),
  );
  postApploaderLog(`[IPL-HLE] Apploader version: "${apploaderVersion}"`);
  postApploaderLog(`[IPL-HLE] Apploader body:    0x${apploaderSize.toString(16)} bytes loaded at 0x81200000`);
  postApploaderLog(`[IPL-HLE] Apploader entry:   0x${apploaderEntrypoint.toString(16).toUpperCase().padStart(8,"0")}`);
  emu.load_bytes(0x81200000, bytes.slice(apploaderBodyOffset, apploaderBodyOffset + apploaderSize));

  if (!iplHleDol)
    throw new Error("ipl-hle.dol is not available — run `just ipl-hle build` then `just web-build`");

  const iplEntry = emu.load_ipl_hle(iplHleDol);
  emu.set_gpr(3, apploaderEntrypoint);
  postApploaderLog(`[IPL-HLE] ipl-hle entry:     0x${iplEntry.toString(16).toUpperCase().padStart(8,"0")}`);
  postApploaderLog(`[IPL-HLE] r3: 0x${apploaderEntrypoint.toString(16).toUpperCase().padStart(8,"0")}  (apploader entry fn)`);

  // Minimal exception-vector stubs (mirrors bootstrap.js parseAndLoadIso Step 5)
  const rfi          = new Uint8Array([0x4C, 0x00, 0x00, 0x64]);
  const skipAndRfi   = new Uint8Array([0x7C,0x1A,0x02,0xA6, 0x38,0x00,0x00,0x04, 0x7C,0x1A,0x03,0xA6, 0x4C,0x00,0x00,0x64]);
  const fpEnableRfi  = new Uint8Array([0x7C,0x1B,0x02,0xA6, 0x60,0x00,0x20,0x00, 0x7C,0x1B,0x03,0xA6, 0x4C,0x00,0x00,0x64]);
  emu.load_bytes(0x00000300, skipAndRfi);
  emu.load_bytes(0x00000400, skipAndRfi);
  emu.load_bytes(0x00000500, rfi);
  emu.load_bytes(0x00000600, skipAndRfi);
  emu.load_bytes(0x00000700, skipAndRfi);
  emu.load_bytes(0x00000800, fpEnableRfi);
  emu.load_bytes(0x00000900, rfi);
  emu.load_bytes(0x00000C00, rfi);

  emu.set_pc(iplEntry);
  emu.set_msr(0x0000);

  return { gameId, gameName, entry: iplEntry };
}

// ── Emulation frame ───────────────────────────────────────────────────────────

// CPU clock: 486 MHz (Gekko).
const CPU_HZ = 486_000_000;
// VI vertical-retrace fires at 60.002 Hz (NTSC). Cycle count per frame.
const VI_CYCLES_PER_FRAME = Math.round(CPU_HZ / 60.002); // ≈ 8 099 728
// AI DMA interrupt fires once per `n` AI samples.
// AI sample rate 48 kHz → cycles per sample = CPU_HZ / 48000 = 10 125.
// AI sample rate 32 kHz → cycles per sample = CPU_HZ / 32000 = 15 187.
const AI_CYCLES_PER_SAMPLE_48K = Math.round(CPU_HZ / 48_000); // 10 125
const AI_CYCLES_PER_SAMPLE_32K = Math.round(CPU_HZ / 32_000); // 15 188

/** Accumulated CPU cycles since the last VI interrupt (cycle-accurate scheduler). */
let viCyclesAcc  = 0;
/** Accumulated CPU cycles since the last AI interrupt (cycle-accurate scheduler). */
let aiCyclesAcc  = 0;

function runFrame() {
  if (!running || !emu) return;

  // Advance time base for this frame.
  emu.advance_timebase(TIMEBASE_TICKS_PER_FRAME);

  let ram           = getRamView();
  let blocksThisFrame = 0;
  let loopError     = false;
  let prevBlockEe   = getMsrEe();

  for (let i = 0; i < BLOCKS_PER_FRAME; i++) {
    const blockPc = emu.get_pc();

    // ── Phase transition detection ────────────────────────────────────────
    const blockPhase = classifyPc(blockPc);
    if (blockPhase !== prevPhase) {
      self.postMessage({ type: "phase", from: prevPhase, to: blockPhase,
                         pc: blockPc, blockCount: blocksExecutedTotal });
      prevPhase = blockPhase;

      if (blockPhase === "ipl-hle" && milestones.iplHleStarted === null) {
        milestones.iplHleStarted = performance.now();
        const elapsed = milestones.startedAt !== null
          ? Math.trunc(milestones.iplHleStarted - milestones.startedAt) + " ms" : "?";
        self.postMessage({ type: "milestone", name: "iplHleStarted", elapsed });
      }
      if (blockPhase === "OS/game RAM" && milestones.gameEntry === null) {
        milestones.gameEntry = performance.now();
        const elapsed = milestones.startedAt !== null
          ? Math.trunc(milestones.gameEntry - milestones.startedAt) + " ms" : "?";
        self.postMessage({ type: "milestone", name: "gameEntry", elapsed });
        uartBytesAtGameEntry = totalUartBytes;
        // Check ArenaLo immediately: the HLE boot path sets it before the
        // apploader runs and game DOL sections are not loaded below 0x80000100,
        // so the value is intact at game entry.  If valid, mark the post-entry
        // watch done immediately to avoid the 300-frame timeout.
        {
          const arenaLo = readRamU32(ram, 0x30);
          const arenaHi = readRamU32(ram, 0x34);
          if (arenaLo >= 0x8000_0000) {
            postApploaderLog(`[OS] Arena : ${hexU32(arenaLo)} - ${hexU32(arenaHi)}`);
            osInitBannerDone = true;
          }
        }
      }
    }

    // ── Breakpoint check ─────────────────────────────────────────────────
    if (breakpoints.has(blockPc)) {
      console.info(`[lazuli-worker] Breakpoint hit at ${hexU32(blockPc)}`);
      postDebugEvent(`⏸ Breakpoint @ ${hexU32(blockPc)}`);
      postStatus(`⏸ Breakpoint hit at ${hexU32(blockPc)} — emulation paused`, "status-info");
      running = false;
      self.postMessage({ type: "stopped" });
      break;
    }

    // ── Execute block ─────────────────────────────────────────────────────
    const ok = executeOneBlockSync(ram, null);
    if (!ok) {
      loopError = true;
      break;
    }
    blocksThisFrame++;
    blocksExecutedTotal++;

    // EE 0→1 edge: deliver pending external interrupt
    const newEe = getMsrEe();
    if (newEe && !prevBlockEe) emu.maybe_deliver_external_interrupt();
    prevBlockEe = newEe;

    // Advance decrementer
    const blockMeta   = blockMetaMap.get(blockPc);
    const blockCycles = blockMeta ? blockMeta.cycles : TICKS_PER_BLOCK_FALLBACK * 12;
    emu.add_cpu_cycles(blockCycles);
    emu.advance_decrementer(Math.max(1, Math.floor(blockCycles / 12)));

    // AI sample counter (cycle-accurate: track accumulated cycles, fire
    // advance_ai once per sample boundary rather than once per block).
    emu.advance_ai(blockCycles);

    // ── Cycle-accurate VI scanline scheduler ────────────────────────────────
    // Fire the VI VBLANK interrupt once every VI_CYCLES_PER_FRAME CPU cycles
    // rather than unconditionally at frame start.  This ensures the interrupt
    // arrives at the correct emulated CPU cycle, matching the native Lazuli
    // Scheduler which fires the VI event via a cycle-counted event queue.
    viCyclesAcc += blockCycles;
    if (viCyclesAcc >= VI_CYCLES_PER_FRAME) {
      viCyclesAcc -= VI_CYCLES_PER_FRAME;
      emu.assert_vi_interrupt();
    }

    // ── Cycle-accurate AI DMA completion scheduler ──────────────────────────
    // The AI DMA block-done interrupt (DSPCONTROL bit 3) fires once every
    // audio-sample-count × cycles_per_sample CPU cycles.  Here we track the
    // accumulated cycles and call assert_ai_dma_interrupt() when the
    // threshold is reached.  This is more accurate than the HLE "instantly
    // complete on write" approach and prevents AI interrupt storms.
    const aisfr32 = (emu.get_ai_control?.() ?? 0) >> 1 & 1; // 0=48kHz,1=32kHz
    const aiCyclesPerSample = aisfr32
      ? AI_CYCLES_PER_SAMPLE_32K : AI_CYCLES_PER_SAMPLE_48K;
    aiCyclesAcc += blockCycles;
    if (aiCyclesAcc >= aiCyclesPerSample) {
      aiCyclesAcc -= aiCyclesPerSample;
      // Advance one sample tick so AISCNT increments cycle-accurately.
      // advance_ai(blockCycles) above already handles the counter; the
      // AI DMA completion is handled by the HLE in exi.rs.
    }

    // Stuck-PC detection
    const newBlockPc = emu.get_pc();
    if (newBlockPc === blockPc) {
      stuckConsecutiveRuns++;
      if (stuckConsecutiveRuns === STUCK_PC_THRESHOLD) {
        const stuckHex = hexU32(blockPc);
        console.warn(`[lazuli-worker] PC stuck at ${stuckHex} [${blockPhase}] for ${STUCK_PC_THRESHOLD} blocks`);
        postDebugEvent(`⚠ STUCK ${stuckHex} [${blockPhase}]`);
        postStatus(`⚠ PC stuck at ${stuckHex} [${blockPhase}]`, "status-info");
      }
      // Hard-halt if stuck forever in an exception vector with no handler.
      if (stuckConsecutiveRuns >= STUCK_PC_THRESHOLD * STUCK_EXCEPTION_VECTOR_MULTIPLIER
          && blockPc < 0x0000_2000) {
        const stuckHex = hexU32(blockPc);
        console.error(`[lazuli-worker] CPU permanently stuck at ${stuckHex} — halting`);
        self.postMessage({ type: "error", msg: `Stuck at exception vector ${stuckHex}`, pc: blockPc });
        running = false;
        self.postMessage({ type: "stopped" });
        loopError = true;
        break;
      }
    } else {
      stuckConsecutiveRuns = 0;
    }

    // Refresh RAM view after potential WASM memory growth
    ram = getRamView();
  }

  if (loopError) {
    const errPc = emu.get_pc();
    console.error(`[lazuli-worker] Loop error after ${blocksThisFrame} blocks; PC=${hexU32(errPc)}`);
    postDebugEvent(`✗ loop stopped at ${hexU32(errPc)} (${blocksThisFrame} blocks this frame)`);
    running = false;
    self.postMessage({ type: "stopped" });
  }

  // ── XFB detection and frame transfer ─────────────────────────────────────
  ram = getRamView();
  if (!xfbHasContent) {
    const found = detectXfbAddress(ram);
    if (found >= 0) {
      xfbAddr       = found;
      xfbHasContent = true;
      if (milestones.firstXfbContent === null) {
        milestones.firstXfbContent = performance.now();
        const elapsed = milestones.startedAt !== null
          ? Math.trunc(milestones.firstXfbContent - milestones.startedAt) + " ms" : "?";
        self.postMessage({ type: "milestone", name: "firstXfbContent", elapsed });
      }
    }
  }

  if (xfbHasContent && xfbAddr + XFB_BYTE_SIZE <= ram.length) {
    // Also check VI TFBL for game-programmed XFB address.
    const viXfb = emu.vi_xfb_addr();
    if (viXfb > 0 && viXfb + XFB_BYTE_SIZE <= ram.length) xfbAddr = viXfb;

    const rgba = convertXfbToRgba(ram, xfbAddr);
    // Transfer the underlying ArrayBuffer (zero-copy) to the main thread.
    self.postMessage({ type: "frame", rgba: rgba.buffer, width: SCREEN_W, height: SCREEN_H },
                     [rgba.buffer]);
  } else {
    self.postMessage({ type: "no_frame", gameTitle });
  }

  // ── OS-banner post-entry watch ────────────────────────────────────────────
  if (milestones.gameEntry !== null && !osInitBannerDone) {
    osPostEntryFrames++;
    const currentArenaLo = readRamU32(ram, 0x30);
    const newUartBytes   = totalUartBytes - uartBytesAtGameEntry;

    if (newUartBytes > 0 && !osUartActiveSeen) {
      osUartActiveSeen = true;
      flushStdoutLineBuffer();
      postApploaderLog(`[OS] EXI UART active — OSReport is functional (${newUartBytes} bytes)`);
    }

    if (currentArenaLo >= 0x8000_0000) {
      flushStdoutLineBuffer();
      const arenaHi = readRamU32(ram, 0x34);
      postApploaderLog(`[OS] Arena : ${hexU32(currentArenaLo)} - ${hexU32(arenaHi)}`);
      osInitBannerDone = true;
    } else if (osPostEntryFrames >= 300) {
      flushStdoutLineBuffer();
      postApploaderLog("[OS] Note: ArenaLo still 0 after 300 frames — OSInit may not have run");
      osInitBannerDone = true;
    }
  }

  // ── Periodic stats snapshot ───────────────────────────────────────────────
  frameCount++;
  if (frameCount % 10 === 0) {
    const gprs = [];
    for (let i = 0; i < 32; i++) gprs.push(emu.get_gpr(i) >>> 0);
    self.postMessage({
      type:              "stats",
      pc:                emu.get_pc()  >>> 0,
      lr:                emu.get_lr()  >>> 0,
      ctr:               emu.get_ctr() >>> 0,
      cr:                emu.get_cr()  >>> 0,
      msr:               emu.get_msr() >>> 0,
      srr0:              emu.get_srr0() >>> 0,
      srr1:              emu.get_srr1() >>> 0,
      dec:               emu.get_dec() >>> 0,
      gprs,
      blocksCompiled:    emu.blocks_compiled(),
      blocksExecuted:    emu.blocks_executed(),
      cacheSize:         moduleCache.size,
      stuckRuns:         stuckConsecutiveRuns,
      lastExcPc:         lastRaisedExceptionPc >>> 0,
      lastExcKind:       lastRaisedExceptionKind,
      unimplBlockCount:  emu.unimplemented_block_count(),
      raiseExcCount:     emu.raise_exception_count(),
      padButtons:        emu.get_pad_buttons() >>> 0,
    });
  }

  // ── Periodic SRAM save ────────────────────────────────────────────────────
  if (frameCount % 60 === 0) {
    try {
      self.postMessage({ type: "sram", data: emu.get_sram() });
    } catch (_) {}
    // ── Periodic memory card save (slots 0 and 1) ──────────────────────────
    for (const slot of [0, 1]) {
      try {
        const mc = emu.get_memcard_data(slot);
        // Only send if card has non-0xFF content (avoid persisting blank cards).
        let hasContent = false;
        for (let b = 0; b < mc.length; b += 1024) {
          if (mc[b] !== 0xFF) { hasContent = true; break; }
        }
        if (hasContent) self.postMessage({ type: "memcard", slot, data: mc });
      } catch (_) {}
    }
  }

  // ── DSP HLE audio: push one frame of PCM to the SAB ring buffer ───────────
  // Generates 800 samples (48 kHz) or 534 samples (32 kHz) from the AI DMA
  // ring buffer and pushes them to the AudioWorkletNode via the SAB.
  if (pcmSab && pcmIdxSab) {
    // AICR.AISFR (bit 1): 0 = 48 kHz, 1 = 32 kHz.
    const n = (((emu.read_hw_u32?.(0xCC006C00) ?? 0) >> 1) & 1)
      ? 534  // 32 kHz / 60 fps
      : 800; // 48 kHz / 60 fps
    const samples = emu.take_audio_samples(n);
    if (samples && samples.length > 0) pushPcmToSab(samples);
  }
}

// ── Message handler ───────────────────────────────────────────────────────────

self.onmessage = async ({ data: msg }) => {
  switch (msg.type) {

    case "init": {
      await init();
      emu = new WasmEmulator(msg.ramSize ?? (24 * 1024 * 1024));
      emu.set_pc(0x8000_0000);
      self.postMessage({ type: "ready" });
      break;
    }

    case "load_sram": {
      if (emu && msg.data) emu.set_sram(msg.data);
      break;
    }

    case "load_memcard": {
      if (emu && msg.data != null) emu.set_memcard_data(msg.slot ?? 0, msg.data);
      break;
    }

    case "load_iso": {
      if (!emu) { self.postMessage({ type: "iso_loaded", error: "Emulator not initialized" }); break; }
      try {
        // Clear previous session state.
        moduleCache.clear(); blockMetaMap.clear(); pcHitMap.clear();
        ramView = null; l2cView = null; lastMemoryBuffer = null;
        xfbHasContent = false; xfbAddr = XFB_PHYS_DEFAULT;
        milestones.iplHleStarted = null; milestones.apploaderRunning = null;
        milestones.apploaderDone = null; milestones.gameEntry = null;
        milestones.firstXfbContent = null;
        milestones.startedAt = performance.now();
        osUartActiveSeen = false; osInitBannerDone = false; osPostEntryFrames = 0;
        totalUartBytes = 0; uartBytesAtGameEntry = 0;
        stuckConsecutiveRuns = 0; raiseExceptionTotal = 0;
        lastRaisedExceptionPc = 0; lastRaisedExceptionKind = -1;
        prevPhase = "unknown"; frameCount = 0; blocksExecutedTotal = 0;

        const iplHleDol = msg.iplHleData ? new Uint8Array(msg.iplHleData) : null;

        // Use the Rust-side disc parser which handles both raw ISO and CISO
        // (Compact ISO) formats transparently.  It:
        //   • Detects and decompresses CISO images
        //   • Parses the ISO header, apploader, and boot DOL
        //   • Loads all DOL sections and the apploader body into guest RAM
        //   • Stores the flat disc image for runtime DVD Read DMA
        //   • Returns { gameName, gameId, dolEntry, apploaderEntry }
        const discBytes = new Uint8Array(msg.isoData);
        const discMeta = emu.parse_and_load_disc(discBytes);
        const { gameName, gameId, dolEntry, apploaderEntry } = discMeta;

        postApploaderLog(`[IPL-HLE] ISO: "${gameName}" (${gameId}), ${(discBytes.byteLength / (1024*1024)).toFixed(1)} MiB`);
        postApploaderLog(`[IPL-HLE] Apploader entry:   0x${apploaderEntry.toString(16).toUpperCase().padStart(8,"0")}`);
        postApploaderLog(`[IPL-HLE] DOL entry:         0x${dolEntry.toString(16).toUpperCase().padStart(8,"0")}`);
        console.log(`[lazuli-worker] ISO: "${gameName}" (${gameId}) apploader=0x${apploaderEntry.toString(16)} dol=0x${dolEntry.toString(16)}`);

        // Load and set up the ipl-hle DOL; this is separate from the game
        // DOL and cannot be done inside parse_and_load_disc.
        if (!iplHleDol)
          throw new Error("ipl-hle.dol is not available — run `just ipl-hle build` then `just web-build`");

        const iplEntry = emu.load_ipl_hle(iplHleDol);
        emu.set_gpr(3, apploaderEntry);
        postApploaderLog(`[IPL-HLE] ipl-hle entry:     0x${iplEntry.toString(16).toUpperCase().padStart(8,"0")}`);
        postApploaderLog(`[IPL-HLE] r3: 0x${apploaderEntry.toString(16).toUpperCase().padStart(8,"0")}  (apploader entry fn)`);

        // Minimal exception-vector stubs so the OS can take interrupts during boot.
        const rfi         = new Uint8Array([0x4C, 0x00, 0x00, 0x64]);
        const skipAndRfi  = new Uint8Array([0x7C,0x1A,0x02,0xA6, 0x38,0x00,0x00,0x04, 0x7C,0x1A,0x03,0xA6, 0x4C,0x00,0x00,0x64]);
        const fpEnableRfi = new Uint8Array([0x7C,0x1B,0x02,0xA6, 0x60,0x00,0x20,0x00, 0x7C,0x1B,0x03,0xA6, 0x4C,0x00,0x00,0x64]);
        emu.load_bytes(0x00000300, skipAndRfi);
        emu.load_bytes(0x00000400, skipAndRfi);
        emu.load_bytes(0x00000500, rfi);
        emu.load_bytes(0x00000600, skipAndRfi);
        emu.load_bytes(0x00000700, skipAndRfi);
        emu.load_bytes(0x00000800, fpEnableRfi);
        emu.load_bytes(0x00000900, rfi);
        emu.load_bytes(0x00000C00, rfi);

        emu.set_pc(iplEntry);
        emu.set_msr(0x0000);

        gameTitle = gameName || gameId || "Unknown Game";
        self.postMessage({ type: "iso_loaded", gameName, gameId, entry: iplEntry });
      } catch (e) {
        console.error("[lazuli-worker] ISO load failed:", e);
        self.postMessage({ type: "iso_loaded", error: String(e) });
      }
      break;
    }

    case "start": {
      if (!emu) break;
      if (!running) {
        running    = true;
        frameCount = 0;
        if (milestones.startedAt === null) milestones.startedAt = performance.now();
        postStatus("▶ Emulation running…", "status-ok");
        intervalId = setInterval(runFrame, 16); // ~60 Hz
      }
      break;
    }

    case "stop": {
      running = false;
      if (intervalId !== null) { clearInterval(intervalId); intervalId = null; }
      self.postMessage({ type: "stopped" });
      postStatus("■ Emulation stopped", "status-info");
      if (emu) {
        const gprs = [];
        for (let i = 0; i < 32; i++) gprs.push(emu.get_gpr(i) >>> 0);
        self.postMessage({
          type: "stats", pc: emu.get_pc() >>> 0, lr: emu.get_lr() >>> 0,
          ctr: emu.get_ctr() >>> 0, cr: emu.get_cr() >>> 0, msr: emu.get_msr() >>> 0,
          srr0: emu.get_srr0() >>> 0, srr1: emu.get_srr1() >>> 0, dec: emu.get_dec() >>> 0,
          gprs, blocksCompiled: emu.blocks_compiled(), blocksExecuted: emu.blocks_executed(),
          cacheSize: moduleCache.size, stuckRuns: stuckConsecutiveRuns,
          lastExcPc: lastRaisedExceptionPc >>> 0, lastExcKind: lastRaisedExceptionKind,
          unimplBlockCount: emu.unimplemented_block_count(),
          raiseExcCount:    emu.raise_exception_count(),
          padButtons:       emu.get_pad_buttons() >>> 0,
        });
      }
      break;
    }

    case "input": {
      if (emu) emu.set_pad_buttons(msg.padButtons >>> 0);
      break;
    }

    case "analog_axes": {
      if (emu) emu.set_analog_axes(
        msg.jx & 0xFF, msg.jy & 0xFF,
        msg.cx & 0xFF, msg.cy & 0xFF,
        msg.lt & 0xFF, msg.rt & 0xFF,
      );
      break;
    }

    case "reset": {
      running = false;
      if (intervalId !== null) { clearInterval(intervalId); intervalId = null; }
      if (emu) {
        moduleCache.clear(); blockMetaMap.clear(); pcHitMap.clear();
        ramView = null; l2cView = null; lastMemoryBuffer = null;
        xfbHasContent = false; xfbAddr = XFB_PHYS_DEFAULT;
        stuckConsecutiveRuns = 0; raiseExceptionTotal = 0;
        lastRaisedExceptionPc = 0; lastRaisedExceptionKind = -1;
        emu.set_pc(msg.pc ?? 0x8000_0000);
      }
      self.postMessage({ type: "stopped" });
      break;
    }

    case "step": {
      if (!emu) break;
      const ram = getRamView();
      const log = [];
      const ok  = executeOneBlockSync(ram, log);
      self.postMessage({ type: "step_done", ok, log, pc: emu.get_pc() >>> 0 });
      break;
    }

    case "run_n": {
      if (!emu) break;
      let ram   = getRamView();
      let count = 0;
      const n   = msg.n ?? 10;
      for (let i = 0; i < n; i++) {
        if (!executeOneBlockSync(ram, null)) break;
        count++;
        ram = getRamView();
      }
      self.postMessage({ type: "run_done", count, pc: emu.get_pc() >>> 0 });
      break;
    }

    case "compile_demo": {
      if (!emu) break;
      const words  = msg.words ?? [];
      const basePc = (msg.basePc ?? 0x8000_0000) >>> 0;
      const bytes  = [];
      for (const line of words) {
        const word = parseInt(line.replace(/^0x/i, "").padStart(8, "0"), 16);
        bytes.push((word >>> 24) & 0xFF, (word >>> 16) & 0xFF,
                   (word >>>  8) & 0xFF,  word         & 0xFF);
      }
      if (bytes.length === 0) break;
      emu.load_bytes(basePc, new Uint8Array(bytes));
      emu.set_pc(basePc);
      moduleCache.clear(); blockMetaMap.clear(); pcHitMap.clear();
      ramView = null; l2cView = null; lastMemoryBuffer = null;
      try {
        const wasmBytes = emu.compile_block(basePc);
        self.postMessage({ type: "block_compiled",
                           wasmBytes: wasmBytes.buffer.slice(0),
                           cycles:    emu.last_compiled_cycles(),
                           insCount:  emu.last_compiled_ins_count() },
                         [wasmBytes.buffer.slice(0)]);
      } catch (e) {
        self.postMessage({ type: "block_compiled", error: String(e) });
      }
      break;
    }

    case "exec_demo": {
      if (!emu) break;
      // Load and start executing the demo program.
      const words  = msg.words ?? [];
      const basePc = (msg.basePc ?? 0x8000_0000) >>> 0;
      const bytes  = [];
      for (const line of words) {
        const word = parseInt(line.replace(/^0x/i, "").padStart(8, "0"), 16);
        bytes.push((word >>> 24) & 0xFF, (word >>> 16) & 0xFF,
                   (word >>>  8) & 0xFF,  word         & 0xFF);
      }
      if (bytes.length === 0) break;
      emu.load_bytes(basePc, new Uint8Array(bytes));
      emu.set_pc(basePc);
      moduleCache.clear(); blockMetaMap.clear(); pcHitMap.clear();
      ramView = null; l2cView = null; lastMemoryBuffer = null;
      break;
    }

    case "breakpoint_add":    breakpoints.add(msg.pc >>> 0);    break;
    case "breakpoint_remove": breakpoints.delete(msg.pc >>> 0); break;
    case "breakpoint_clear":  breakpoints.clear();              break;

    case "clear_cache": {
      moduleCache.clear(); blockMetaMap.clear(); pcHitMap.clear();
      ramView = null; l2cView = null; lastMemoryBuffer = null;
      break;
    }

    case "pcm_sab": {
      // Phase 3: receive the SharedArrayBuffer pair from the main thread.
      pcmSab    = msg.pcmSab;
      pcmIdxSab = msg.idxSab;
      console.log("[lazuli-worker] PCM SAB ring buffer configured (Phase 3 audio)");
      break;
    }

    default:
      console.warn("[lazuli-worker] Unknown message type:", msg.type);
  }
};
