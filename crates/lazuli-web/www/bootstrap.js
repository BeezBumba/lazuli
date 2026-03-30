/**
 * bootstrap.js — Lazuli GameCube WASM emulator frontend
 *
 * Features implemented here:
 *  • GameCube ISO loading: parses the disc header and boot DOL binary,
 *    loads each section into the emulator's 24 MiB guest RAM.
 *  • Dynarec game loop: uses `requestAnimationFrame` to drive the PPC→WASM
 *    JIT pipeline; compiled WASM modules are cached in a JS Map keyed by
 *    guest PC so each block is compiled at most once.
 *  • Zero-copy RAM: accesses the emulator's RAM buffer through
 *    `wasm_memory()` + `ram_ptr()` to avoid per-block copies.
 *  • Hook closures: memory read/write hooks read from and write directly to
 *    the zero-copy RAM view; no `get_ram_copy()` / `sync_ram()` in the loop.
 *  • Keyboard controller: maps keyboard keys to GameCube button bits and
 *    forwards the bitmask to the Rust emulator on every keydown/keyup.
 *  • XFB rendering: converts the GameCube YUV422 external frame-buffer region
 *    to RGBA and paints it onto a 640×480 canvas each frame.
 *  • Web Audio: creates an AudioContext for future DSP output; a startup
 *    chime confirms that audio is active.
 *
 * ## Build & serve
 *
 *   cd crates/lazuli-web && wasm-pack build --target web --out-dir www/pkg
 *   cd www && python3 -m http.server 8080
 *
 * Or from the workspace root:
 *
 *   just web-serve
 */

// ── Imports ───────────────────────────────────────────────────────────────────
import init, { WasmEmulator, wasm_memory } from "./pkg/lazuli_web.js";

// ── DOM helpers ───────────────────────────────────────────────────────────────
const $ = (id) => document.getElementById(id);

function setStatus(msg, cls = "status-info") {
  const el = $("status-bar");
  el.textContent = msg;
  el.className = cls;
}

function updateStats(emu) {
  $("stat-compiled").textContent = emu.blocks_compiled();
  $("stat-executed").textContent = emu.blocks_executed();
  $("stat-cache").textContent    = moduleCache.size;
  $("stat-pad").textContent      = "0x" + emu.get_pad_buttons().toString(16).toUpperCase().padStart(4, "0");
}

function renderRegisters(emu) {
  const grid = $("reg-grid");
  grid.innerHTML = "";

  for (let i = 0; i < 32; i++) {
    const cell = document.createElement("div");
    cell.className = "reg-cell";
    const val = emu.get_gpr(i);
    cell.innerHTML =
      `<span class="reg-name">r${i}&nbsp;</span>` +
      `<span class="reg-val">0x${val.toString(16).padStart(8, "0").toUpperCase()}</span>`;
    grid.appendChild(cell);
  }

  const pcCell = document.createElement("div");
  pcCell.className = "reg-cell";
  const pc = emu.get_pc();
  pcCell.innerHTML =
    `<span class="reg-name">PC&nbsp;</span>` +
    `<span class="reg-val">0x${pc.toString(16).padStart(8, "0").toUpperCase()}</span>`;
  grid.appendChild(pcCell);
}

// ── Hex dump helper ───────────────────────────────────────────────────────────
function annotateWasm(bytes) {
  if (bytes.length === 0) return "(empty)";
  const hex = (b) => b.toString(16).padStart(2, "0");
  const lines = ["; WASM binary module", ";"];
  if (bytes.length >= 8) {
    lines.push(`; magic:   ${Array.from(bytes.slice(0, 4)).map(hex).join(" ")}  (\\0asm)`);
    lines.push(`; version: ${Array.from(bytes.slice(4, 8)).map(hex).join(" ")}  (1)`);
    lines.push(";");
  }
  lines.push(`; Full bytecode (${bytes.length} bytes):`);
  for (let i = 0; i < bytes.length; i += 16) {
    const chunk    = bytes.slice(i, i + 16);
    const hexPart  = Array.from(chunk).map(hex).join(" ").padEnd(48, " ");
    const asciiPart = Array.from(chunk)
      .map((b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : "."))
      .join("");
    lines.push(`  ${i.toString(16).padStart(4, "0")}  ${hexPart}  ${asciiPart}`);
  }
  return lines.join("\n");
}

// ── GameCube ISO / DOL parsing ────────────────────────────────────────────────

/**
 * Parse a GameCube DOL header and return a list of loadable sections plus the
 * entry-point address.
 *
 * DOL header layout (all fields big-endian, header is 0x100 bytes):
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
 * @param {DataView}  view      DataView over the entire ISO/DOL ArrayBuffer
 * @param {Uint8Array} bytes    Uint8Array over the same buffer
 * @param {number}    dolOffset Byte offset of the DOL within the ISO
 */
function parseDol(view, bytes, dolOffset) {
  const u32 = (off) => view.getUint32(dolOffset + off, false /* big-endian */);

  const textOffsets = Array.from({ length: 7  }, (_, i) => u32(0x000 + i * 4));
  const dataOffsets = Array.from({ length: 11 }, (_, i) => u32(0x01C + i * 4));
  const textTargets = Array.from({ length: 7  }, (_, i) => u32(0x048 + i * 4));
  const dataTargets = Array.from({ length: 11 }, (_, i) => u32(0x064 + i * 4));
  const textSizes   = Array.from({ length: 7  }, (_, i) => u32(0x090 + i * 4));
  const dataSizes   = Array.from({ length: 11 }, (_, i) => u32(0x0AC + i * 4));
  const bssTarget   = u32(0x0D8);
  const bssSize     = u32(0x0DC);
  const entry       = u32(0x0E0);

  const sections = [];

  for (let i = 0; i < 7; i++) {
    if (textOffsets[i] !== 0 && textSizes[i] !== 0) {
      const start = dolOffset + textOffsets[i];
      sections.push({ target: textTargets[i], data: bytes.slice(start, start + textSizes[i]) });
    }
  }
  for (let i = 0; i < 11; i++) {
    if (dataOffsets[i] !== 0 && dataSizes[i] !== 0) {
      const start = dolOffset + dataOffsets[i];
      sections.push({ target: dataTargets[i], data: bytes.slice(start, start + dataSizes[i]) });
    }
  }

  return { sections, bssTarget, bssSize, entry };
}

/**
 * Parse a GameCube ISO image, extract the boot DOL, load every section into
 * the emulator's RAM, zero the BSS, and set the CPU entry point.
 *
 * GameCube ISO header layout (big-endian):
 *   0x000  console_id (1 B) + game_id (5 B)
 *   0x01C  magic word 0xC2339F3D
 *   0x020  game name (null-terminated, ≤ 0x3E0 bytes)
 *   0x420  bootfile_offset  — byte offset of the boot DOL within the ISO
 *
 * @param {ArrayBuffer} arrayBuffer Raw ISO bytes
 * @param {WasmEmulator} emu        Emulator instance
 * @returns {{ gameId: string, gameName: string, entry: number }}
 */
function parseAndLoadIso(arrayBuffer, emu) {
  const view  = new DataView(arrayBuffer);
  const bytes = new Uint8Array(arrayBuffer);

  // Verify the GameCube magic word at 0x001C
  const magic = view.getUint32(0x1C, false);
  if (magic !== 0xC2339F3D) {
    throw new Error(
      `Not a valid GameCube ISO — magic word mismatch ` +
      `(got 0x${magic.toString(16).toUpperCase()}, expected 0xC2339F3D)`
    );
  }

  // Game ID: bytes 0–5 (e.g. "GMSE01" for Super Mario Sunshine NTSC).
  // Only keep printable ASCII (0x20–0x7E) to avoid garbage in the display.
  const gameId = String.fromCharCode(
    ...bytes.slice(0, 6).filter(b => b >= 0x20 && b <= 0x7E)
  );

  // Game name: null-terminated string starting at 0x020
  let gameName = "";
  for (let i = 0x020; i < 0x400 && bytes[i] !== 0; i++) {
    gameName += String.fromCharCode(bytes[i]);
  }

  // Boot DOL offset lives at 0x420 in the ISO header
  const dolOffset = view.getUint32(0x420, false);
  if (dolOffset === 0 || dolOffset >= arrayBuffer.byteLength) {
    throw new Error(`Invalid DOL offset 0x${dolOffset.toString(16)} in ISO header`);
  }

  // Parse the DOL and load sections into emulator RAM.
  // load_bytes() on the Rust side masks the target address with 0x01FFFFFF
  // to convert GameCube virtual addresses (0x8xxxxxxx) to physical offsets.
  const dol = parseDol(view, bytes, dolOffset);
  for (const { target, data } of dol.sections) {
    emu.load_bytes(target, data);
  }

  // Zero the BSS region
  if (dol.bssSize > 0) {
    emu.load_bytes(dol.bssTarget, new Uint8Array(dol.bssSize));
  }

  // Point the CPU at the entry point
  emu.set_pc(dol.entry);

  return { gameId, gameName, entry: dol.entry };
}

// ── GameCube button bitmask ───────────────────────────────────────────────────

/** GameCube controller button bits used by set_pad_buttons(). */
const GC_BTN = {
  A:     0x0001,
  B:     0x0002,
  X:     0x0004,
  Y:     0x0008,
  Z:     0x0010,
  START: 0x0020,
  UP:    0x0040,
  DOWN:  0x0080,
  LEFT:  0x0100,
  RIGHT: 0x0200,
  L:     0x0400,
  R:     0x0800,
  // Analog stick pseudo-buttons (discrete, not analog — for demo purposes)
  STICK_UP:    0x1000,
  STICK_DOWN:  0x2000,
  STICK_LEFT:  0x4000,
  STICK_RIGHT: 0x8000,
};

/** Keyboard key → GameCube button mapping. */
const KEY_MAP = {
  "x":          GC_BTN.A,
  "z":          GC_BTN.B,
  "s":          GC_BTN.X,
  "a":          GC_BTN.Y,
  "d":          GC_BTN.Z,
  "Enter":      GC_BTN.START,
  "ArrowUp":    GC_BTN.UP,
  "ArrowDown":  GC_BTN.DOWN,
  "ArrowLeft":  GC_BTN.LEFT,
  "ArrowRight": GC_BTN.RIGHT,
  "q":          GC_BTN.L,
  "e":          GC_BTN.R,
  "i":          GC_BTN.STICK_UP,
  "k":          GC_BTN.STICK_DOWN,
  "j":          GC_BTN.STICK_LEFT,
  "l":          GC_BTN.STICK_RIGHT,
};

// ── Zero-copy RAM view ────────────────────────────────────────────────────────

/**
 * Create (or refresh) a zero-copy live view of the Rust emulator's RAM.
 *
 * The view is backed by the WASM module's linear memory buffer.  It must be
 * recreated after any WASM memory growth (detected by comparing buffers).
 */
let ramView = null;
let lastMemoryBuffer = null;

function getRamView(emu) {
  const mem = wasm_memory();
  if (!ramView || mem.buffer !== lastMemoryBuffer) {
    lastMemoryBuffer = mem.buffer;
    ramView = new Uint8Array(mem.buffer, emu.ram_ptr(), emu.ram_size());
  }
  return ramView;
}

// ── Hook closure factory ──────────────────────────────────────────────────────

/**
 * Build the `hooks` import object for a compiled JIT block.
 *
 * Each compiled block imports these functions from the `"hooks"` module:
 *   read_u8(addr) / read_u16(addr) / read_u32(addr)
 *   write_u8(addr, val) / write_u16(addr, val) / write_u32(addr, val)
 *   raise_exception(kind)
 *
 * The closures operate directly on the zero-copy `ramView`, so reads and
 * writes are immediately visible in the Rust emulator without any syncing.
 *
 * Addresses are masked with PHYS_MASK before indexing into `ram`.  This
 * mirrors the `phys_addr` helper in the Rust emulator: GameCube virtual
 * addresses like 0x80xxxxxx and hardware-register addresses like 0xCCxxxxxx
 * both strip their high bits, leaving a 25-bit physical offset that fits
 * within the 24 MiB guest RAM.  Without this mask every 0x80xxxxxx read
 * returns 0 and every write is silently dropped because the index exceeds
 * the ram buffer length.
 *
 * @param {Uint8Array} ram  Zero-copy view of guest RAM
 * @param {string[]}   log  Array to append exception/error messages to
 * @param {string}     [pcContext]  Human-readable PC string for console messages
 */

/** 25-bit physical address mask — matches Rust's `phys_addr` helper. */
const PHYS_MASK = 0x01FFFFFF;

/**
 * Total number of raise_exception calls since the last ISO load / Reset.
 * Used to throttle console output so we log only the first few occurrences.
 */
let raiseExceptionTotal = 0;
/** Maximum number of raise_exception events logged to the console. */
const RAISE_EXCEPTION_LOG_LIMIT = 30;

function buildHooks(ram, log, pcContext = "?") {
  return {
    read_u8(addr) {
      addr = (addr >>> 0) & PHYS_MASK;
      return addr < ram.length ? ram[addr] : 0;
    },
    read_u16(addr) {
      addr = (addr >>> 0) & PHYS_MASK;
      if (addr + 1 >= ram.length) return 0;
      return (ram[addr] << 8) | ram[addr + 1];
    },
    read_u32(addr) {
      addr = (addr >>> 0) & PHYS_MASK;
      if (addr + 3 >= ram.length) return 0;
      return (((ram[addr] << 24) | (ram[addr + 1] << 16) |
               (ram[addr + 2] << 8) | ram[addr + 3]) >>> 0);
    },
    write_u8(addr, val) {
      addr = (addr >>> 0) & PHYS_MASK;
      if (addr < ram.length) ram[addr] = val & 0xff;
    },
    write_u16(addr, val) {
      addr = (addr >>> 0) & PHYS_MASK;
      if (addr + 1 < ram.length) {
        ram[addr]     = (val >> 8) & 0xff;
        ram[addr + 1] = val & 0xff;
      }
    },
    write_u32(addr, val) {
      addr = (addr >>> 0) & PHYS_MASK;
      val  = val  >>> 0;
      if (addr + 3 < ram.length) {
        ram[addr]     = (val >>> 24) & 0xff;
        ram[addr + 1] = (val >>> 16) & 0xff;
        ram[addr + 2] = (val >>>  8) & 0xff;
        ram[addr + 3] =  val         & 0xff;
      }
    },
    raise_exception(kind) {
      if (log) log.push(`exception: kind=${kind}`);
      raiseExceptionTotal++;
      if (raiseExceptionTotal <= RAISE_EXCEPTION_LOG_LIMIT) {
        console.warn(`[lazuli] raise_exception(kind=${kind}) in block @ ${pcContext} (total #${raiseExceptionTotal})`);
        if (raiseExceptionTotal === RAISE_EXCEPTION_LOG_LIMIT) {
          console.warn("[lazuli] raise_exception log limit reached — suppressing further messages");
        }
      }
    },
  };
}

// ── Compiled block cache ──────────────────────────────────────────────────────

/**
 * JavaScript-side module cache: maps guest PC → WebAssembly.Module.
 *
 * Each unique basic block is compiled by the Rust ppcwasm JIT exactly once
 * and then cached here.  On cache hit, only a synchronous `new
 * WebAssembly.Instance()` is needed, which is fast.
 *
 * The cache is cleared whenever a new ROM/ISO is loaded via `clearModuleCache`.
 */
const moduleCache = new Map(); // u32 pc → WebAssembly.Module

function clearModuleCache() {
  moduleCache.clear();
  raiseExceptionTotal = 0;
}

// ── Synchronous block execution ───────────────────────────────────────────────

const WASM_PAGE = 65536;

/**
 * Compile (or fetch from cache), instantiate, and execute one PPC basic block.
 *
 * Uses the synchronous `new WebAssembly.Module()` / `new WebAssembly.Instance()`
 * APIs so it can be called from a `requestAnimationFrame` callback without
 * `await`.  Browsers allow synchronous WASM compilation for small modules on
 * the main thread; our JIT blocks are always < 4 KB.
 *
 * @param {WasmEmulator} emu   Emulator instance
 * @param {Uint8Array}   ram   Zero-copy RAM view (from getRamView)
 * @param {string[]|null} log  Optional log array; pass null in tight loops
 * @returns {boolean}  true on success, false on compile / execution error
 */
function executeOneBlockSync(emu, ram, log) {
  const pc    = emu.get_pc();
  const pcHex = "0x" + pc.toString(16).toUpperCase().padStart(8, "0");

  // ── Step 1: compile (or load from JS cache) ──────────────────────────────
  let module = moduleCache.get(pc);
  if (!module) {
    let wasmBytes;
    try {
      wasmBytes = emu.compile_block(pc);
    } catch (e) {
      console.error(`[lazuli] compile error @ ${pcHex}: ${e}`);
      if (log) log.push(`[${pcHex}] compile error: ${e}`);
      return false;
    }
    try {
      module = new WebAssembly.Module(wasmBytes);
    } catch (e) {
      console.error(`[lazuli] WebAssembly.Module error @ ${pcHex}: ${e}`);
      if (log) log.push(`[${pcHex}] WebAssembly.Module error: ${e}`);
      return false;
    }
    moduleCache.set(pc, module);
  }

  // ── Step 2: allocate a WASM memory page to hold the CPU register file ────
  const cpuSize    = emu.cpu_struct_size();
  const pagesNeeded = Math.ceil(cpuSize / WASM_PAGE);
  const regsMem    = new WebAssembly.Memory({ initial: pagesNeeded });
  const regsView   = new Uint8Array(regsMem.buffer);
  regsView.set(emu.get_cpu_bytes(), 0);

  // ── Step 3: instantiate with hook closures that use the zero-copy RAM ────
  let instance;
  try {
    instance = new WebAssembly.Instance(module, {
      env:   { memory: regsMem },
      hooks: buildHooks(ram, log, pcHex),
    });
  } catch (e) {
    console.error(`[lazuli] instantiation error @ ${pcHex}: ${e}`);
    if (log) log.push(`[${pcHex}] instantiation error: ${e}`);
    return false;
  }

  // ── Step 4: execute ───────────────────────────────────────────────────────
  let nextPc;
  try {
    nextPc = instance.exports.execute(0 /* regs_ptr = 0 */);
  } catch (e) {
    console.error(`[lazuli] execution error @ ${pcHex}: ${e}`);
    if (log) log.push(`[${pcHex}] execution error: ${e}`);
    return false;
  }

  // ── Step 5: sync CPU state back; RAM is already in sync (zero-copy) ──────
  emu.set_cpu_bytes(new Uint8Array(regsMem.buffer, 0, cpuSize));
  emu.record_block_executed();

  // nextPc == 0 means the block updated Cpu::pc itself (branch taken)
  const newPc = (nextPc >>> 0) !== 0 ? (nextPc >>> 0) : emu.get_pc();
  emu.set_pc(newPc);

  if (log) {
    const newHex = "0x" + newPc.toString(16).toUpperCase().padStart(8, "0");
    log.push(`[${pcHex}] executed → next PC ${newHex}`);
  }
  return true;
}

// ── Canvas rendering ──────────────────────────────────────────────────────────

const SCREEN_W = 640;
const SCREEN_H = 480;
/** YUV422 frame-buffer size in bytes. */
const XFB_BYTE_SIZE = SCREEN_W * SCREEN_H * 2;

/**
 * Candidate physical base addresses to probe when searching for the XFB.
 *
 * Games can allocate the external frame-buffer anywhere in the 24 MiB main
 * RAM.  We check these addresses in order and use the first one that contains
 * non-zero pixel data.  Candidates are:
 *  • 0x00C00000 — a common static XFB address used by many games / SDKs.
 *  • End-of-RAM minus one XFB  — for games that place a single XFB at the
 *    top of heap (e.g. Super Mario Sunshine @ 0x016D2C00).
 *  • End-of-RAM minus two XFBs — for double-buffered games.
 *
 * The candidates are computed lazily using the actual `ram.length` passed to
 * `detectXfbAddress` so they adjust automatically to different RAM sizes.
 */
const XFB_PHYS_DEFAULT = 0x00C00000;

/**
 * Probe `ram` for a non-zero XFB at each candidate address and return the
 * first address that contains frame data, or -1 if none is found yet.
 *
 * Only the first 64 bytes at each candidate are checked to keep the scan
 * fast; this is enough to distinguish a rendered frame from an all-zero
 * buffer.
 *
 * @param {Uint8Array} ram
 * @returns {number}  Physical base address, or -1 if no content found
 */
function detectXfbAddress(ram) {
  const candidates = [
    XFB_PHYS_DEFAULT,
    (ram.length - XFB_BYTE_SIZE) & ~0x1F,        // 1× buffer from end
    (ram.length - 2 * XFB_BYTE_SIZE) & ~0x1F,    // 2× buffers from end
  ];

  for (const addr of candidates) {
    if (addr < 0 || addr + XFB_BYTE_SIZE > ram.length) continue;
    for (let i = addr; i < addr + 64; i += 4) {
      if (ram[i] !== 0 || ram[i + 1] !== 0 || ram[i + 2] !== 0 || ram[i + 3] !== 0) {
        return addr;
      }
    }
  }
  return -1;
}

/**
 * Try to render the GameCube YUV422 external frame buffer (XFB) to the canvas.
 *
 * The GC XFB format stores pairs of pixels as [Cb, Y0, Cr, Y1] (4 bytes = 2
 * pixels).  This function converts each pair to two RGBA pixels using
 * standard BT.601 coefficients.
 *
 * If no XFB content is found yet, `drawPlaceholder` is called with `title`
 * so the canvas shows the game name and a "waiting for first XFB write" hint
 * rather than the generic "load a game" splash.
 *
 * @param {Uint8Array} ram  Zero-copy RAM view
 * @param {CanvasRenderingContext2D} ctx
 * @param {WasmEmulator} emu
 * @param {string|null}  title  Current game title (or null if no game loaded)
 */
function renderXfb(ram, ctx, emu, title) {
  // Once content has been located we skip the scan; reset xfbHasContent to
  // re-enable it (e.g. after loading a new ISO or pressing Reset).
  if (!xfbHasContent) {
    const found = detectXfbAddress(ram);
    if (found < 0) {
      drawPlaceholder(ctx, title, emu);
      return;
    }
    xfbAddr       = found;
    xfbHasContent = true;
  }

  const xfb = xfbAddr;
  if (xfb + XFB_BYTE_SIZE > ram.length) {
    drawPlaceholder(ctx, title, emu);
    return;
  }

  // YUV422 → RGBA conversion into an ImageData
  const imageData = ctx.createImageData(SCREEN_W, SCREEN_H);
  const px        = imageData.data;
  const pairs     = (SCREEN_W * SCREEN_H) >>> 1;

  for (let i = 0; i < pairs; i++) {
    const base = xfb + i * 4;
    const cb = ram[base];
    const y0 = ram[base + 1];
    const cr = ram[base + 2];
    const y1 = ram[base + 3];

    const cbOff = cb - 128;
    const crOff = cr - 128;

    // BT.601 YUV→RGB using fixed-point coefficients scaled by 1024 (>> 10).
    // R = Y + 1.402 * Cr        → (Y + (1402 * crOff) >> 10)
    // G = Y − 0.344 * Cb − 0.714 * Cr
    // B = Y + 1.772 * Cb        → (Y + (1772 * cbOff) >> 10)
    const clamp = (v) => v < 0 ? 0 : v > 255 ? 255 : v;

    const r0 = clamp(y0 + ((1402 * crOff) >> 10)) | 0;
    const g0 = clamp(y0 - ((344  * cbOff) >> 10) - ((714 * crOff) >> 10)) | 0;
    const b0 = clamp(y0 + ((1772 * cbOff) >> 10)) | 0;

    const r1 = clamp(y1 + ((1402 * crOff) >> 10)) | 0;
    const g1 = clamp(y1 - ((344  * cbOff) >> 10) - ((714 * crOff) >> 10)) | 0;
    const b1 = clamp(y1 + ((1772 * cbOff) >> 10)) | 0;

    const p = i * 8;
    px[p]     = r0; px[p + 1] = g0; px[p + 2] = b0; px[p + 3] = 255;
    px[p + 4] = r1; px[p + 5] = g1; px[p + 6] = b1; px[p + 7] = 255;
  }

  ctx.putImageData(imageData, 0, 0);
}

/**
 * Draw a placeholder screen when no XFB content is available.
 *
 * @param {CanvasRenderingContext2D} ctx
 * @param {string|null} gameTitle  Game title or null if no game loaded
 * @param {WasmEmulator|null} emu  Emulator for PC readout
 */
function drawPlaceholder(ctx, gameTitle, emu) {
  const W = SCREEN_W, H = SCREEN_H;

  ctx.fillStyle = "#0d1117";
  ctx.fillRect(0, 0, W, H);

  // Subtle grid
  ctx.strokeStyle = "#1c2128";
  ctx.lineWidth = 1;
  for (let x = 0; x <= W; x += 40) {
    ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, H); ctx.stroke();
  }
  for (let y = 0; y <= H; y += 40) {
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(W, y); ctx.stroke();
  }

  ctx.textAlign = "center";
  if (gameTitle) {
    ctx.fillStyle = "#58a6ff";
    ctx.font = "bold 28px 'Consolas', monospace";
    ctx.fillText(gameTitle, W / 2, H / 2 - 40);

    ctx.fillStyle = "#3fb950";
    ctx.font = "16px 'Consolas', monospace";
    ctx.fillText("CPU running — waiting for first XFB write…", W / 2, H / 2);

    if (emu) {
      const pc = emu.get_pc();
      ctx.fillStyle = "#8b949e";
      ctx.font = "14px 'Consolas', monospace";
      ctx.fillText(
        `PC: 0x${pc.toString(16).toUpperCase().padStart(8, "0")}`,
        W / 2, H / 2 + 30
      );
    }
  } else {
    ctx.fillStyle = "#58a6ff";
    ctx.font = "bold 36px 'Consolas', monospace";
    ctx.fillText("LAZULI", W / 2, H / 2 - 20);

    ctx.fillStyle = "#8b949e";
    ctx.font = "16px 'Consolas', monospace";
    ctx.fillText("Load a GameCube ISO to start", W / 2, H / 2 + 20);
  }
}

// ── Web Audio ─────────────────────────────────────────────────────────────────

let audioCtx   = null;
let audioActive = false;

/**
 * Initialise (or resume) the Web Audio context and play a short startup chime
 * to confirm that audio routing is working.
 *
 * The AudioContext is created with a 32 kHz sample rate matching the
 * GameCube's native audio output.  Actual DSP audio output will be routed
 * here once the DSP emulation is integrated into the WASM frontend.
 */
function initAudio() {
  if (!audioCtx) {
    audioCtx = new AudioContext({ sampleRate: 32000 });
  }
  if (audioCtx.state === "suspended") {
    audioCtx.resume();
  }

  // Short startup chime: C5 → G4 two-note sequence
  const chime = [
    { freq: 523.25, start: 0.00, dur: 0.15 },
    { freq: 392.00, start: 0.10, dur: 0.15 },
  ];
  for (const { freq, start, dur } of chime) {
    const osc  = audioCtx.createOscillator();
    const gain = audioCtx.createGain();
    osc.type = "triangle";
    osc.frequency.value = freq;
    gain.gain.setValueAtTime(0.08, audioCtx.currentTime + start);
    gain.gain.exponentialRampToValueAtTime(0.001, audioCtx.currentTime + start + dur);
    osc.connect(gain);
    gain.connect(audioCtx.destination);
    osc.start(audioCtx.currentTime + start);
    osc.stop(audioCtx.currentTime + start + dur + 0.05);
  }

  audioActive = true;
}

function suspendAudio() {
  if (audioCtx) audioCtx.suspend();
  audioActive = false;
}

// ── Game loop ─────────────────────────────────────────────────────────────────

// Number of JIT blocks to execute per animation frame (~60 Hz).
// The GameCube CPU runs at ~486 MHz with ~1-2 IPC; typical games have
// basic blocks of 5–20 instructions.  500 blocks/frame ≈ 7,500–15,000
// instructions per frame — a reasonable starting budget that keeps the
// main thread responsive while making visible forward progress.
const BLOCKS_PER_FRAME = 500;

/**
 * Time-base ticks to advance per animation frame.
 *
 * The GameCube's Gekko time base increments at CPU / 12 ≈ 40.5 MHz.
 * At 60 fps that is 40,500,000 / 60 = 675,000 ticks per frame.  Games use
 * `mftb` to drive all their timing loops (e.g. `OSWaitVBlank`); without a
 * monotonically increasing time base they spin forever and the screen stays
 * blank.
 */
const TIMEBASE_TICKS_PER_FRAME = 675_000;

let running        = false;
let animFrameId    = null;
let gameTitle      = null;
let frameCount     = 0;
let lastFpsTime    = 0;
/** Set to true once non-zero XFB data is found; cleared on ISO load / Reset. */
let xfbHasContent  = false;
/** Physical base address of the discovered XFB (updated by detectXfbAddress). */
let xfbAddr        = XFB_PHYS_DEFAULT;

/**
 * Main emulation loop — called by requestAnimationFrame at ~60 Hz.
 *
 * Each frame executes up to BLOCKS_PER_FRAME JIT blocks, renders the XFB to
 * the canvas, and updates the stats display.
 */
function gameLoop(emu, canvas, ctx, timestamp) {
  if (!running) return;

  // Advance the time base before executing blocks so that mftb-based timing
  // loops inside the game see a non-zero delta on the very first frame.
  emu.advance_timebase(TIMEBASE_TICKS_PER_FRAME);

  // Execute blocks for this frame
  const ram = getRamView(emu);
  let blocksThisFrame = 0;
  let loopError = false;
  for (let i = 0; i < BLOCKS_PER_FRAME; i++) {
    if (!executeOneBlockSync(emu, ram, null)) {
      loopError = true;
      break;
    }
    blocksThisFrame++;
  }
  if (loopError) {
    const stuckPc = emu.get_pc();
    const stuckHex = "0x" + stuckPc.toString(16).toUpperCase().padStart(8, "0");
    console.error(
      `[lazuli] gameLoop: execution stopped after ${blocksThisFrame} blocks ` +
      `(total compiled: ${emu.blocks_compiled()}, executed: ${emu.blocks_executed()}) ` +
      `— PC is now ${stuckHex}`
    );
    stopLoop();
    return;
  }

  // Render XFB to canvas
  renderXfb(ram, ctx, emu, gameTitle);

  // FPS counter (update every second)
  frameCount++;
  if (timestamp - lastFpsTime >= 1000) {
    const fps = (frameCount * 1000 / (timestamp - lastFpsTime)).toFixed(1);
    $("fps-display").textContent = fps;
    frameCount  = 0;
    lastFpsTime = timestamp;
  }

  // Update stats every ~10 frames to avoid layout thrashing
  if (frameCount % 10 === 0) {
    updateStats(emu);
    renderRegisters(emu);
  }

  animFrameId = requestAnimationFrame((ts) => gameLoop(emu, canvas, ctx, ts));
}

function startLoop(emu, canvas, ctx) {
  if (running) return;
  running     = true;
  frameCount  = 0;
  lastFpsTime = performance.now();
  animFrameId = requestAnimationFrame((ts) => gameLoop(emu, canvas, ctx, ts));
  $("btn-start").disabled = true;
  $("btn-stop").disabled  = false;
  setStatus("▶ Emulation running…", "status-ok");
}

function stopLoop() {
  running = false;
  if (animFrameId !== null) {
    cancelAnimationFrame(animFrameId);
    animFrameId = null;
  }
  $("btn-start").disabled = false;
  $("btn-stop").disabled  = true;
  $("fps-display").textContent = "—";
  setStatus("■ Emulation stopped", "status-info");
}

// ── Demo programs ─────────────────────────────────────────────────────────────
const DEMO_PROGRAMS = {
  "addi+blr": {
    description: "li r3, 2  /  addis r3, r0, 1  /  ori r3, r3, 0  /  blr",
    words: ["38600002", "3C600001", "60630000", "4E800020"],
  },
  "loop (cmpi+bc)": {
    description: "li r3, 0  /  addi r3, r3, 1  /  cmpi cr0, r3, 10  /  bne -8",
    words: ["38600000", "38630001", "2C030000", "40820000"],
  },
  "store+load": {
    description: "li r4, 42  /  stw r4, 0(r1)  /  lwz r5, 0(r1)  /  blr",
    words: ["3880002A", "90810000", "80A10000", "4E800020"],
  },
};

// ── Execution log helpers ─────────────────────────────────────────────────────
const MAX_LOG_LINES = 300;
let execLogLines = [];

function appendExecLog(line) {
  execLogLines.push(line);
  if (execLogLines.length > MAX_LOG_LINES) {
    execLogLines = execLogLines.slice(-MAX_LOG_LINES);
  }
  const el = $("exec-log");
  el.textContent = execLogLines.join("\n");
  el.scrollTop   = el.scrollHeight;
}

// ── Main ──────────────────────────────────────────────────────────────────────
async function main() {
  setStatus("Loading Lazuli WASM module…", "status-info");

  let emu;
  const canvas = $("screen");
  const ctx    = canvas.getContext("2d");

  // Last ISO entry point — used by the Reset button to restart at the correct PC.
  let lastEntryPoint = 0x80000000;

  // Draw splash screen while WASM loads
  drawPlaceholder(ctx, null, null);

  try {
    await init();
    // 24 MiB of guest RAM — matches the GameCube's main memory
    emu = new WasmEmulator(24 * 1024 * 1024);
    emu.set_pc(0x80000000);
    setStatus("✓ WASM module loaded — load an ISO or a demo program to begin", "status-ok");
    $("btn-compile").disabled  = false;
    $("btn-load-iso").disabled = false;
    $("btn-start").disabled    = false;
    $("btn-audio").disabled    = false;
  } catch (e) {
    setStatus(`✗ Failed to load WASM module: ${e}`, "status-err");
    console.error(e);
    return;
  }

  renderRegisters(emu);
  updateStats(emu);

  // ── Keyboard controller ────────────────────────────────────────────────────
  document.addEventListener("keydown", (e) => {
    const bit = KEY_MAP[e.key];
    if (bit) {
      e.preventDefault();
      emu.set_pad_buttons(emu.get_pad_buttons() | bit);
      $("stat-pad").textContent =
        "0x" + emu.get_pad_buttons().toString(16).toUpperCase().padStart(4, "0");
    }
  });
  document.addEventListener("keyup", (e) => {
    const bit = KEY_MAP[e.key];
    if (bit) {
      emu.set_pad_buttons(emu.get_pad_buttons() & ~bit);
      $("stat-pad").textContent =
        "0x" + emu.get_pad_buttons().toString(16).toUpperCase().padStart(4, "0");
    }
  });

  // ── Start button ───────────────────────────────────────────────────────────
  $("btn-start").addEventListener("click", () => {
    startLoop(emu, canvas, ctx);
  });

  // ── Stop button ────────────────────────────────────────────────────────────
  $("btn-stop").addEventListener("click", () => {
    stopLoop();
    renderRegisters(emu);
    updateStats(emu);
  });

  // ── Reset button ───────────────────────────────────────────────────────────
  $("btn-reset").addEventListener("click", () => {
    stopLoop();
    clearModuleCache();
    ramView       = null;        // force refresh of zero-copy view
    xfbHasContent = false;       // re-arm the XFB content check
    xfbAddr       = XFB_PHYS_DEFAULT;
    // Return to the entry point of the last loaded game (or 0x80000000 if none)
    emu.set_pc(lastEntryPoint);
    drawPlaceholder(ctx, gameTitle, null);
    renderRegisters(emu);
    updateStats(emu);
    setStatus(
      `↺ Reset to entry 0x${lastEntryPoint.toString(16).toUpperCase()} — press ▶ Start`,
      "status-info"
    );
  });

  // ── Audio toggle ───────────────────────────────────────────────────────────
  $("btn-audio").addEventListener("click", () => {
    if (!audioActive) {
      initAudio();
      $("btn-audio").textContent = "🔊 Audio: On";
      setStatus("🔊 Audio enabled (32 kHz, GameCube native rate)", "status-ok");
    } else {
      suspendAudio();
      $("btn-audio").textContent = "🔇 Audio: Off";
      setStatus("🔇 Audio disabled", "status-info");
    }
  });

  // ── ISO file loader ────────────────────────────────────────────────────────
  $("iso-file").addEventListener("change", () => {
    const file = $("iso-file").files[0];
    if (file) {
      $("iso-meta").textContent = `Selected: ${file.name} (${(file.size / 1048576).toFixed(1)} MiB)`;
    }
  });

  $("btn-load-iso").addEventListener("click", () => {
    const file = $("iso-file").files[0];
    if (!file) {
      setStatus("✗ No file selected", "status-err");
      return;
    }

    setStatus(`Reading ${file.name}…`, "status-info");

    const reader = new FileReader();
    reader.onload = (evt) => {
      try {
        stopLoop();
        clearModuleCache();
        ramView       = null;
        xfbHasContent = false;
        xfbAddr       = XFB_PHYS_DEFAULT;

        const meta = parseAndLoadIso(evt.target.result, emu);
        gameTitle     = meta.gameName || meta.gameId || "Unknown Game";
        lastEntryPoint = meta.entry;

        $("header-game").textContent = `— ${gameTitle}`;
        $("iso-meta").textContent =
          `Game ID: ${meta.gameId} | Title: ${meta.gameName} | ` +
          `Entry: 0x${meta.entry.toString(16).toUpperCase()}`;

        $("btn-reset").disabled = false;

        drawPlaceholder(ctx, gameTitle, emu);
        renderRegisters(emu);
        updateStats(emu);

        setStatus(
          `✓ Loaded "${meta.gameName}" (${meta.gameId}) — ` +
          `entry 0x${meta.entry.toString(16).toUpperCase()} — press ▶ Start`,
          "status-ok"
        );
      } catch (e) {
        setStatus(`✗ ISO load failed: ${e}`, "status-err");
        console.error(e);
        $("iso-meta").textContent = `Error: ${e.message}`;
      }
    };
    reader.onerror = () => setStatus("✗ Failed to read file", "status-err");
    reader.readAsArrayBuffer(file);
  });

  // Drag-and-drop ISO loading onto the drop-zone card
  const dropZone = $("iso-drop-zone");
  dropZone.addEventListener("dragover", (e) => {
    e.preventDefault();
    dropZone.classList.add("drop-active");
  });
  dropZone.addEventListener("dragleave", () => {
    dropZone.classList.remove("drop-active");
  });
  dropZone.addEventListener("drop", (e) => {
    e.preventDefault();
    dropZone.classList.remove("drop-active");
    const file = e.dataTransfer?.files[0];
    if (!file) return;
    // Inject into the file input so the "Load ISO" button works as usual
    const dt = new DataTransfer();
    dt.items.add(file);
    $("iso-file").files = dt.files;
    $("iso-meta").textContent = `Selected: ${file.name} (${(file.size / 1048576).toFixed(1)} MiB)`;
    $("btn-load-iso").click();
  });

  // ── Compile → WASM button ─────────────────────────────────────────────────
  $("btn-compile").addEventListener("click", async () => {
    const rawLines = $("asm-input").value.trim().split(/\s+/);
    const basePc   = parseInt($("base-pc").value.trim(), 16) || 0x80000000;

    const bytes = [];
    for (const line of rawLines) {
      const cleaned = line.replace(/^0x/i, "").replace(/[^0-9a-fA-F]/g, "");
      if (!cleaned.length) continue;
      const word = parseInt(cleaned.padStart(8, "0"), 16);
      bytes.push((word >>> 24) & 0xff, (word >>> 16) & 0xff,
                 (word >>>  8) & 0xff,  word         & 0xff);
    }
    if (!bytes.length) {
      setStatus("✗ No valid instructions entered", "status-err");
      return;
    }

    // Load instructions into guest RAM at basePc
    emu.load_bytes(basePc, new Uint8Array(bytes));
    emu.set_pc(basePc);
    clearModuleCache();
    ramView = null;

    setStatus("Compiling…", "status-info");
    try {
      const wasmBytes = emu.compile_block(basePc);
      await WebAssembly.compile(wasmBytes); // validate

      $("block-output").textContent = annotateWasm(Array.from(wasmBytes));
      $("stat-ins").textContent     = rawLines.filter(l => l.trim()).length;
      $("stat-bytes").textContent   = wasmBytes.length + " bytes";
      updateStats(emu);
      setStatus(
        `✓ Block compiled to ${wasmBytes.length} WASM bytes — verified OK`,
        "status-ok"
      );
    } catch (e) {
      setStatus(`✗ Compilation failed: ${e}`, "status-err");
      $("block-output").textContent = `Error: ${e}`;
    }
  });

  // ── Demo loader ────────────────────────────────────────────────────────────
  $("btn-load-demo").addEventListener("click", () => {
    const keys = Object.keys(DEMO_PROGRAMS);
    const key  = keys[Math.floor(Math.random() * keys.length)];
    const prog = DEMO_PROGRAMS[key];
    $("asm-input").value = prog.words.join("\n");
    setStatus(`Loaded demo: "${key}" — ${prog.description}`, "status-info");
  });

  // ── Step button ────────────────────────────────────────────────────────────
  $("btn-step").addEventListener("click", () => {
    const ram = getRamView(emu);
    const log = [];
    const ok  = executeOneBlockSync(emu, ram, log);
    for (const line of log) appendExecLog(line);
    renderRegisters(emu);
    updateStats(emu);
    setStatus(
      ok
        ? `✓ Stepped — PC now 0x${emu.get_pc().toString(16).toUpperCase().padStart(8, "0")}`
        : "✗ Step failed — see execution log",
      ok ? "status-ok" : "status-err"
    );
  });

  // ── Run 10 blocks button ───────────────────────────────────────────────────
  $("btn-run10").addEventListener("click", () => {
    const ram = getRamView(emu);
    let count = 0;
    for (let i = 0; i < 10; i++) {
      const log = [];
      if (!executeOneBlockSync(emu, ram, log)) {
        for (const l of log) appendExecLog(l);
        break;
      }
      count++;
    }
    renderRegisters(emu);
    updateStats(emu);
    setStatus(
      count > 0
        ? `✓ Ran ${count} block(s) — PC now 0x${emu.get_pc().toString(16).toUpperCase().padStart(8, "0")}`
        : "✗ Run failed at first block",
      count > 0 ? "status-ok" : "status-err"
    );
  });

  // Enable step/run buttons now that the emulator is ready
  $("btn-step").disabled  = false;
  $("btn-run10").disabled = false;
}

main();
