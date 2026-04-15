/**
 * bootstrap.js — Lazuli GameCube browser emulator
 *
 * Implements the JavaScript side of a full in-browser GameCube emulator,
 * mirroring the approach used by the Play! PS2 emulator
 * (https://github.com/jpd002/Play-):
 *
 *  • CPU: PowerPC → WebAssembly dynarec via the Rust `ppcwasm` JIT.
 *    Compiled WASM modules are cached in a JS Map keyed by guest PC so each
 *    block is compiled at most once.
 *
 *  • GPU: WebGPU surface initialised from the game canvas via wgpu's
 *    `webgpu` backend (wasm32 feature).  Falls back to the canvas-based
 *    YUV422 XFB blitter when WebGPU is unavailable.
 *
 *  • Audio: `AudioWorkletNode`-based DSP output pipeline.  The worklet
 *    maintains a per-channel ring buffer and drains it into the WebAudio
 *    output on every 128-sample process() tick.  Interleaved stereo f32
 *    PCM at 32 kHz (GameCube native rate) is pushed each frame via
 *    `pushDspSamples()`.
 *
 *  • IO: Gamepad API controller input merged with keyboard fallback.
 *    Gamepad state is polled every animation frame; the bitmask is ORed
 *    with the keyboard bitmask before being forwarded to the Rust emulator.
 *
 *  • ROM loading: parses GameCube disc (ISO) headers and boot DOL binaries,
 *    loads each section into the emulator's 24 MiB zero-copy guest RAM.
 *
 *  • XFB rendering: converts the GameCube YUV422 external frame-buffer to
 *    RGBA and paints it onto a 640×480 canvas each frame.
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

function setText(id, val) {
  const el = $(id);
  if (el) el.textContent = val;
}

function setStatus(msg, cls = "status-info") {
  const el = $("status-bar");
  if (!el) return;
  el.textContent = msg;
  el.className = cls;
}

function updateStats(emu) {
  setText("stat-compiled", emu.blocks_compiled());
  setText("stat-executed", emu.blocks_executed());
  setText("stat-cache",    moduleCache.size);
  setText("stat-pad",      "0x" + emu.get_pad_buttons().toString(16).toUpperCase().padStart(4, "0"));

  // Live current PC
  const curPc = emu.get_pc();
  setText("stat-current-pc",
    "0x" + curPc.toString(16).toUpperCase().padStart(8, "0"));

  // Stuck-PC streak indicator
  const stuckEl = $("stat-stuck-runs");
  if (stuckEl) {
    stuckEl.textContent = stuckConsecutiveRuns;
    stuckEl.style.color =
      stuckConsecutiveRuns > STUCK_PC_THRESHOLD * 4 ? "var(--red)" :
      stuckConsecutiveRuns > STUCK_PC_THRESHOLD      ? "var(--yellow)" : "";
  }

  // Last exception info
  if (lastRaisedExceptionPc !== 0) {
    setText("stat-exc-pc",
      "0x" + lastRaisedExceptionPc.toString(16).toUpperCase().padStart(8, "0") +
      ` (kind=${lastRaisedExceptionKind})`);
  }

  // LR register
  const lr = emu.get_lr();
  setText("stat-lr", "0x" + lr.toString(16).toUpperCase().padStart(8, "0"));

  // Condition Register (CR) and CTR
  const cr  = emu.get_cr() >>> 0;
  const ctr = emu.get_ctr() >>> 0;
  setText("stat-cr",  "0x" + cr.toString(16).toUpperCase().padStart(8, "0"));
  setText("stat-ctr", (ctr >>> 0).toString(10));

  // CR field breakdown: CR0 (bits 31-28) … CR7 (bits 3-0)
  const crGrid = $("cr-field-grid");
  if (crGrid) {
    const FLAG_NAMES = ["LT", "GT", "EQ", "SO"];
    crGrid.innerHTML = "";
    for (let field = 0; field < 8; field++) {
      const nibble = (cr >>> (28 - field * 4)) & 0xF;
      const flags  = FLAG_NAMES.filter((_, i) => nibble & (8 >> i));
      const cell   = document.createElement("div");
      cell.className = "reg-cell";
      cell.innerHTML =
        `<span class="reg-name">CR${field}</span> ` +
        `<span class="reg-val">${flags.length ? flags.join("|") : "—"}</span>`;
      crGrid.appendChild(cell);
    }
  }

  // Last compiled block details — shown once at least one block has been compiled
  if (emu.blocks_compiled() > 0) {
    const lastPc = emu.last_compiled_pc() >>> 0;
    setText("stat-last-pc",   "0x" + lastPc.toString(16).toUpperCase().padStart(8, "0"));
    setText("stat-last-ins",  emu.last_compiled_ins_count());
    setText("stat-last-wasm", emu.last_compiled_wasm_bytes() + " B");
  }

  // Exception / unimplemented counters
  setText("stat-exceptions",    emu.raise_exception_count());
  setText("stat-unimpl-blocks", emu.unimplemented_block_count());

  // Hot block (most-executed PC)
  let hotPc = 0, hotHits = 0;
  for (const [pc, hits] of pcHitMap) {
    if (hits > hotHits) { hotPc = pc; hotHits = hits; }
  }
  if (hotHits > 0) {
    setText("stat-hot-pc",   "0x" + hotPc.toString(16).toUpperCase().padStart(8, "0"));
    setText("stat-hot-hits", hotHits);
  } else {
    setText("stat-hot-pc",   "—");
    setText("stat-hot-hits", "—");
  }

}

function renderRegisters(emu) {
  const grid = $("reg-grid");
  if (!grid) return;
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

function renderFprRegisters(emu) {
  const grid = $("fpr-grid");
  if (!grid) return;
  grid.innerHTML = "";

  for (let i = 0; i < 32; i++) {
    const cell = document.createElement("div");
    cell.className = "reg-cell";
    const val = emu.get_fpr(i);
    cell.innerHTML =
      `<span class="reg-name">f${i}&nbsp;</span>` +
      `<span class="reg-val">${val.toExponential(4)}</span>`;
    grid.appendChild(cell);
  }
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
 * the emulator's RAM, zero the BSS, load the apploader, load the embedded
 * ipl-hle binary, and point the CPU at the ipl-hle entry (0x81300000).
 *
 * The real apploader entry function address (from the ISO apploader header)
 * is passed to ipl-hle via r3, matching what the native load_ipl_hle() does.
 *
 * GameCube ISO header layout (big-endian):
 *   0x000  console_id (1 B) + game_id (5 B)
 *   0x01C  magic word 0xC2339F3D
 *   0x020  game name (null-terminated, ≤ 0x3E0 bytes)
 *   0x420  bootfile_offset  — byte offset of the boot DOL within the ISO
 *
 * Apploader layout (at ISO offset 0x2440, big-endian):
 *   0x000  version string (null-terminated, padded to 0x10 bytes)
 *   0x010  entrypoint — guest address of the apploader's entry function
 *   0x014  size       — byte length of the apploader body
 *   0x018  trailer_size
 *   0x01C  padding (4 bytes)
 *   0x020  body[size]  — loaded at guest 0x81200000
 *
 * @param {ArrayBuffer} arrayBuffer Raw ISO bytes
 * @param {WasmEmulator} emu        Emulator instance
 * @param {Uint8Array|null} iplHleDol ipl-hle DOL bytes (fetched at startup)
 * @returns {{ gameId: string, gameName: string, entry: number }}
 */
function parseAndLoadIso(arrayBuffer, emu, iplHleDol) {
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

  console.log(`[lazuli] parseAndLoadIso: game="${gameName}" id=${gameId} iso=${arrayBuffer.byteLength} bytes`);
  appendApploaderLog(`[IPL-HLE] ISO: "${gameName}" (${gameId}), ${(arrayBuffer.byteLength / (1024*1024)).toFixed(1)} MiB`);

  // Boot DOL offset lives at 0x420 in the ISO header
  const dolOffset = view.getUint32(0x420, false);
  if (dolOffset === 0 || dolOffset >= arrayBuffer.byteLength) {
    throw new Error(`Invalid DOL offset 0x${dolOffset.toString(16)} in ISO header`);
  }

  // ── Step 1: copy the ISO disk header (first 0x440 bytes) into guest RAM
  // at 0x80000000.  This replicates what load_ipl_hle does for the fields
  // that come straight from the disc: game code, maker code, disk ID,
  // version, audio_streaming, stream_buffer_size, and the DVD magic word
  // (0xC2339F3D at 0x1C).  load_bytes() masks the address with 0x01FFFFFF
  // so 0x80000000 maps to physical offset 0.
  emu.load_bytes(0x80000000, bytes.slice(0, 0x440));

  // Parse the DOL header for informational logging only.
  // NOTE: We intentionally do NOT pre-load the game DOL sections here.
  // The native load_ipl_hle() never pre-loads them either — the real apploader
  // loads every section into RAM via DI DMA during the ipl-hle boot sequence.
  // Pre-loading them would be redundant and inconsistent with native behaviour,
  // though the underlying boot sequence is unaffected since the apploader DMA
  // writes the same bytes to the same physical addresses regardless.
  const dol = parseDol(view, bytes, dolOffset);
  console.log(
    `[lazuli] DOL header: entry=0x${dol.entry.toString(16).toUpperCase().padStart(8, '0')}` +
    ` bss=0x${dol.bssTarget.toString(16).toUpperCase().padStart(8, '0')}..+0x${dol.bssSize.toString(16).toUpperCase()}` +
    ` sections=${dol.sections.length}`
  );
  // ── Step 2: write synthetic Dolphin OS globals, mirroring load_ipl_hle().
  // The disc-header copy above already covers 0x00–0x1F (game code, magic,
  // etc.).  Here we fill in the fields the real IPL ROM synthesises itself.
  {
    const osGlobBuf = new ArrayBuffer(0x100);
    const osGlobView = new DataView(osGlobBuf);
    // Offsets are relative to 0x80000000 (physical 0x00000000).
    osGlobView.setUint32(0x20, 0x0D15EA5E, false); // Boot kind (normal)
    osGlobView.setUint32(0x24, 0x00000001, false); // IPL version
    osGlobView.setUint32(0x28, 0x01800000, false); // Physical RAM size (24 MiB)
    osGlobView.setUint32(0x2C, 0x10000005, false); // Console type
    osGlobView.setUint32(0x30, 0x8042E260, false); // Arena Low
    osGlobView.setUint32(0x34, 0x817FE8C0, false); // Arena High
    osGlobView.setUint32(0x38, 0x817FE8C0, false); // FST location
    osGlobView.setUint32(0x3C, 0x00000024, false); // FST max size
    osGlobView.setUint32(0xCC, 0x00000000, false); // TV mode (NTSC)
    osGlobView.setUint32(0xD0, 0x01000000, false); // ARAM size
    osGlobView.setUint32(0xF8, 0x09A7EC80, false); // Bus clock speed
    osGlobView.setUint32(0xFC, 0x1CF7C580, false); // CPU clock speed
    // Load as a single contiguous write; individual fields that were already
    // populated by the disc-header copy (0x00–0x1F) are overwritten only for
    // the synthetic offsets listed above, so the game code / magic / etc. set
    // in Step 1 are preserved (they live before 0x20).
    emu.load_bytes(0x80000020, new Uint8Array(osGlobBuf, 0x20, 0xE0));
  }

  // ── Step 3: load the apploader, mirroring load_apploader() in system.rs.
  // The apploader sits at ISO offset 0x2440 and has a 0x20-byte header:
  //   0x00  version string (null-terminated, padded to 0x10 bytes)
  //   0x10  entrypoint — guest address of the apploader entry function
  //   0x14  size       — byte length of the apploader body
  //   0x18  trailer_size
  //   0x1C  padding (4 bytes)
  //   0x20  body[size]
  // The body is placed at guest 0x81200000 (physical 0x01200000).
  const APPLOADER_ISO_OFFSET  = 0x2440;
  const apploaderVersionEnd   = bytes.indexOf(0, APPLOADER_ISO_OFFSET);
  const apploaderVersionSliceEnd = (apploaderVersionEnd >= 0 && apploaderVersionEnd < APPLOADER_ISO_OFFSET + 0x10)
    ? apploaderVersionEnd : APPLOADER_ISO_OFFSET + 0x10;
  const apploaderVersion      = String.fromCharCode(
    ...bytes.slice(APPLOADER_ISO_OFFSET, apploaderVersionSliceEnd).filter(b => b > 0)
  );
  const apploaderEntrypoint   = view.getUint32(APPLOADER_ISO_OFFSET + 0x10, false);
  const apploaderSize         = view.getUint32(APPLOADER_ISO_OFFSET + 0x14, false);
  const apploaderTrailerSize  = view.getUint32(APPLOADER_ISO_OFFSET + 0x18, false);
  const apploaderBodyOffset   = APPLOADER_ISO_OFFSET + 0x20; // header is 0x20 bytes
  if (apploaderSize === 0 || apploaderBodyOffset + apploaderSize > arrayBuffer.byteLength) {
    throw new Error(
      `Invalid apploader in ISO: size=0x${apploaderSize.toString(16)}, ` +
      `bodyOffset=0x${apploaderBodyOffset.toString(16)}, isoSize=0x${arrayBuffer.byteLength.toString(16)}`
    );
  }
  console.log(
    `[lazuli] apploader: version="${apploaderVersion}" entrypoint=0x${apploaderEntrypoint.toString(16).toUpperCase().padStart(8,'0')}` +
    ` size=0x${apploaderSize.toString(16).toUpperCase().padStart(8,'0')} trailer=0x${apploaderTrailerSize.toString(16).toUpperCase().padStart(8,'0')}`
  );
  appendApploaderLog(`[IPL-HLE] Apploader version: "${apploaderVersion}"`);
  appendApploaderLog(`[IPL-HLE] Apploader body:    0x${apploaderSize.toString(16)} bytes loaded at 0x81200000`);
  appendApploaderLog(`[IPL-HLE] Apploader entry:   0x${apploaderEntrypoint.toString(16).toUpperCase().padStart(8,'0')}`);
  const apploaderBody = bytes.slice(apploaderBodyOffset, apploaderBodyOffset + apploaderSize);
  emu.load_bytes(0x81200000, apploaderBody);

  // ── Step 4: load the ipl-hle DOL into guest RAM.
  // ipl-hle is fetched from the server at startup (ipl-hle.dol, built via
  // `just ipl-hle build` and copied to the www/ directory by `just web-build`).
  // Its main() expects the real apploader's entry function address in r3.
  if (!iplHleDol) {
    throw new Error(
      "ipl-hle.dol is not available — run `just ipl-hle build` then `just web-build` " +
      "to generate and deploy the ipl-hle binary before loading an ISO."
    );
  }
  const iplEntry = emu.load_ipl_hle(iplHleDol);
  emu.set_gpr(3, apploaderEntrypoint);
  console.log(
    `[lazuli] ipl-hle entry=0x${iplEntry.toString(16).toUpperCase().padStart(8,'0')}` +
    ` r3=apploader_entry=0x${apploaderEntrypoint.toString(16).toUpperCase().padStart(8,'0')}`
  );
  appendApploaderLog(`[IPL-HLE] ipl-hle entry:     0x${iplEntry.toString(16).toUpperCase().padStart(8,'0')}`);
  appendApploaderLog(`[IPL-HLE] r3: 0x${apploaderEntrypoint.toString(16).toUpperCase().padStart(8,'0')}  (apploader entry fn — compare with native)`);

  // ── Step 5: install minimal exception-vector stubs ───────────────────────
  //
  // The real GameCube IPL ROM populates the low-memory exception vectors
  // (0x00000100–0x00001300) before handing control to the apploader.
  // Because we use ipl-hle, those handlers are never installed.  When the
  // stuck-PC heuristic force-delivers a decrementer exception the CPU jumps
  // to 0x00000900, which contains all-zero words (= Illegal instructions).
  // Each Illegal instruction raises another Program Exception → 0x00000700 →
  // more Illegal instructions → infinite loop at 0x00000700.
  //
  // We install just enough stub code to break that chain:
  //
  //   0x00000900 (Decrementer) — rfi
  //     Returns to the interrupted instruction (SRR0) with the original MSR
  //     (SRR1).  This lets a force-delivered decrementer interrupt return
  //     cleanly so the tight CTR-decrement loop at 0x81200DA0 can keep
  //     running until CTR reaches 0 and the loop exits naturally.
  //
  //   0x00000700 (Program Exception) — mfspr r0,SRR0 / addi r0,r0,4 /
  //                                    mtspr SRR0,r0 / rfi
  //     Advances SRR0 past the faulting Illegal instruction before returning.
  //     Prevents the 0x00000700 self-referencing infinite loop that results
  //     when an exception handler slot itself contains Illegal instructions.
  //
  //   0x00000500 (External Interrupt) — rfi
  //     Returns from the VI-retrace (external) interrupt without servicing it.
  //     Prevents a crash once the game's OSInit enables EE (bit 15 of MSR)
  //     before installing its own handler at 0x00000500.
  //
  //   0x00000C00 (System Call) — rfi
  //     Returns from a `sc` instruction.  The Syscall exception sets SRR0 to
  //     pc+4 (the instruction after `sc`), so rfi correctly resumes there with
  //     the saved MSR restored.  Without this stub the CPU wanders through zero
  //     memory when `sc` fires before OSInit installs the real handler at
  //     0x80000C00, eventually getting stuck in game code.
  //
  // These stubs are intentionally minimal: OSInit() overwrites them with full
  // Dolphin OS handlers (written via the normal emu.load_bytes path) during
  // the game's first-frame initialisation.
  //
  // Instruction encodings (all big-endian):
  //   rfi                = 0x4C000064  (primary=19, xo=50)
  //   mfspr r0, SRR0     = 0x7C1A02A6  (primary=31, rD=0, SPR=26, xo=339)
  //   addi  r0, r0, 4    = 0x38000004  (primary=14, rD=rA=0, SIMM=4)
  //   mtspr SRR0, r0     = 0x7C1A03A6  (primary=31, rS=0, SPR=26, xo=467)
  {
    // rfi — return from interrupt, restoring SRR0 → PC and SRR1 → MSR.
    const rfi = new Uint8Array([0x4C, 0x00, 0x00, 0x64]);

    // mfspr r0,SRR0 / addi r0,r0,4 / mtspr SRR0,r0 / rfi
    // Advances SRR0 by 4 (skips the faulting instruction) then returns.
    const skipAndRfi = new Uint8Array([
      0x7C, 0x1A, 0x02, 0xA6,  // mfspr r0, SRR0
      0x38, 0x00, 0x00, 0x04,  // addi  r0, r0, 4
      0x7C, 0x1A, 0x03, 0xA6,  // mtspr SRR0, r0
      0x4C, 0x00, 0x00, 0x64,  // rfi
    ]);

    // mfspr r0,SRR1 / ori r0,r0,0x2000 / mtspr SRR1,r0 / rfi
    // Enables FP (bit 13) in the saved MSR then returns to re-execute the
    // faulting FP instruction.  Prevents an FP-Unavailable → FP-Unavailable
    // spin when the OS has not yet enabled FP via mtmsr.
    const fpEnableAndRetry = new Uint8Array([
      0x7C, 0x1B, 0x02, 0xA6,  // mfspr r0, SRR1
      0x60, 0x00, 0x20, 0x00,  // ori   r0, r0, 0x2000
      0x7C, 0x1B, 0x03, 0xA6,  // mtspr SRR1, r0
      0x4C, 0x00, 0x00, 0x64,  // rfi
    ]);

    // These stubs cover the window before OSInit installs the Dolphin OS
    // exception handlers to low memory, and act as a safety net if a
    // bcctr/blr with CTR/LR=0 lands at 0x0 and eventually hits one of
    // these vectors while executing ISO header bytes as instructions.
    // OSInit will overwrite all of them with proper handlers.
    emu.load_bytes(0x00000300, skipAndRfi);    // DSI (Data Storage)    → skip + rfi
    emu.load_bytes(0x00000400, skipAndRfi);    // ISI (Instr Storage)   → skip + rfi
    emu.load_bytes(0x00000500, rfi);           // External Interrupt     → rfi
    emu.load_bytes(0x00000600, skipAndRfi);    // Alignment              → skip + rfi
    emu.load_bytes(0x00000700, skipAndRfi);    // Program Exception      → skip + rfi
    emu.load_bytes(0x00000800, fpEnableAndRetry); // FP Unavailable      → enable FP + retry
    emu.load_bytes(0x00000900, rfi);           // Decrementer            → rfi
    emu.load_bytes(0x00000C00, rfi);           // System Call (sc)       → rfi
  }

  // Point the CPU at the ipl-hle entry (0x81300000), not the raw apploader
  // entrypoint.  This matches what the real IPL ROM does: it loads the
  // apploader at 0x81200000, then hands control to its own stub which calls
  // the apploader's init/main/close functions before jumping to the game DOL.
  emu.set_pc(iplEntry);
  console.log(`[lazuli] PC set to 0x${iplEntry.toString(16).toUpperCase().padStart(8,'0')} (ipl-hle entry), MSR=0x0000`);
  appendApploaderLog(`[IPL-HLE] PC → 0x${iplEntry.toString(16).toUpperCase().padStart(8,'0')} (ipl-hle), MSR=0x8000 (EE=1 IP=0)`);

  // Initialise the MSR to match what native load_ipl_hle() leaves the CPU in.
  // Native only calls set_exception_prefix(false) (IP=0); EE stays at the
  // Cpu::default value of 0 (interrupts disabled).  The printed string
  // "MSR=0x8000 (EE=1 IP=0)" is a hardcoded label in the native source that
  // does not reflect the actual register value — the real boot MSR is 0x0000.
  emu.set_msr(0x0000);  // EE=0, IP=0 — mirrors native load_ipl_hle()

  return { gameId, gameName, entry: iplEntry };
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

/**
 * Standard Gamepad API button index → GameCube button bitmask.
 *
 * Maps the W3C Standard Gamepad layout (Xbox / DualSense / etc.) to the
 * GameCube controller button bits used by `set_pad_buttons()`.  Entries with
 * value `0` have no GameCube equivalent and are ignored.
 */
const GAMEPAD_BTN_MAP = [
  GC_BTN.A,      // 0  South  (A / ×)
  GC_BTN.B,      // 1  East   (B / ○)
  GC_BTN.X,      // 2  West   (X / □)
  GC_BTN.Y,      // 3  North  (Y / △)
  GC_BTN.L,      // 4  LB / L1 — GameCube L trigger (digital)
  GC_BTN.R,      // 5  RB / R1 — GameCube R trigger (digital)
  GC_BTN.L,      // 6  LT / L2 — GameCube L trigger (analog treated as digital)
  GC_BTN.R,      // 7  RT / R2 — GameCube R trigger (analog treated as digital)
  0,             // 8  Back / Select — no GameCube equivalent
  GC_BTN.START,  // 9  Start / Menu
  0,             // 10 Left Stick Press — no GameCube equivalent
  0,             // 11 Right Stick Press — no GameCube equivalent
  GC_BTN.UP,     // 12 D-Pad Up
  GC_BTN.DOWN,   // 13 D-Pad Down
  GC_BTN.LEFT,   // 14 D-Pad Left
  GC_BTN.RIGHT,  // 15 D-Pad Right
  GC_BTN.Z,      // 16 Guide / Home — mapped to GameCube Z for convenience
];

/** Analog stick deflection threshold for digital button emulation (0–1). */
const GAMEPAD_AXIS_THRESHOLD = 0.25;

// ── Input state ───────────────────────────────────────────────────────────────

/**
 * Button bitmask accumulated from keyboard `keydown` / `keyup` events.
 * Separated from `gamepadBits` so that the two input sources can be merged
 * without one clearing the other.
 */
let keyboardBits = 0;

/**
 * Button bitmask polled from the Gamepad API each animation frame.
 * Updated by `pollGamepad()` and ORed with `keyboardBits` before forwarding
 * to the Rust emulator.
 */
let gamepadBits = 0;

/**
 * Poll the Gamepad API and update `gamepadBits`.
 *
 * Uses the first connected gamepad.  Digital buttons are mapped via
 * `GAMEPAD_BTN_MAP`; the left analog stick is converted to the four
 * `STICK_*` pseudo-buttons using `GAMEPAD_AXIS_THRESHOLD`.
 *
 * Must be called once per animation frame (inside `gameLoop`) so that the
 * emulator sees fresh controller state before executing the next batch of
 * blocks.
 *
 * @param {import("./pkg/lazuli_web.js").WasmEmulator} emu
 */
function pollGamepad(emu) {
  if (!navigator.getGamepads) return;

  const gamepads = navigator.getGamepads();
  gamepadBits = 0;

  for (const gp of gamepads) {
    if (!gp || !gp.connected) continue;

    // Map digital buttons using the standard gamepad layout table.
    for (let i = 0; i < Math.min(gp.buttons.length, GAMEPAD_BTN_MAP.length); i++) {
      if (gp.buttons[i].pressed && GAMEPAD_BTN_MAP[i]) {
        gamepadBits |= GAMEPAD_BTN_MAP[i];
      }
    }

    // Map left analog stick (axes 0 = X, 1 = Y) to STICK_* pseudo-buttons.
    const ax = gp.axes[0] ?? 0;
    const ay = gp.axes[1] ?? 0;
    if (ax < -GAMEPAD_AXIS_THRESHOLD) gamepadBits |= GC_BTN.STICK_LEFT;
    if (ax >  GAMEPAD_AXIS_THRESHOLD) gamepadBits |= GC_BTN.STICK_RIGHT;
    if (ay < -GAMEPAD_AXIS_THRESHOLD) gamepadBits |= GC_BTN.STICK_UP;
    if (ay >  GAMEPAD_AXIS_THRESHOLD) gamepadBits |= GC_BTN.STICK_DOWN;

    break; // Use the first connected gamepad only.
  }

  emu.set_pad_buttons(keyboardBits | gamepadBits);
}

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
 *   read_u8(addr) / read_u16(addr) / read_u32(addr) / read_f64(addr)
 *   write_u8(addr, val) / write_u16(addr, val) / write_u32(addr, val) / write_f64(addr, val)
 *   raise_exception(kind)
 *
 * The closures operate directly on the zero-copy `ramView`, so reads and
 * writes are immediately visible in the Rust emulator without any syncing.
 *
 * ## Address routing — hardware registers vs. guest RAM
 *
 * GameCube hardware-register addresses have the prefix `0xCC` (cached I/O)
 * or `0xCD` (uncached I/O).  For example the DVD Interface lives at
 * `0xCC006000` / `0xCD006000`.  These addresses must be intercepted
 * **before** `PHYS_MASK` is applied, because `0xCC006000 & 0x01FFFFFF` equals
 * `0x00006000` — an address inside guest RAM — causing silent corruption.
 *
 * Both cached (`0xCC`) and uncached (`0xCD`) mirrors are detected by checking
 * `(addr >>> 24 & 0xFE) === 0xCC` — this clears bit 0 of the top byte,
 * normalising `0xCD` (uncached) to `0xCC` (cached) before the comparison.
 * The Rust hw dispatch functions perform the same normalisation internally.
 *
 * For 32-bit reads/writes the Rust `hw_read_u32` / `hw_write_u32` exports are
 * called instead; they dispatch to the appropriate hardware module (PI, DI,
 * DSP).  8-bit accesses to MMIO space return 0 / are silently ignored since
 * GameCube MMIO registers are 16-bit (DSP) or 32-bit (PI, DI) wide.
 * 16-bit accesses are forwarded to `hw_read_u16` / `hw_write_u16` for the
 * DSP Interface registers (accessed with `lhz`/`sth` by the OS).
 *
 * For all non-hardware addresses `PHYS_MASK` is applied as before, converting
 * `0x80xxxxxx` guest virtual addresses to 25-bit physical RAM offsets.
 *
 * @param {Uint8Array} ram  Zero-copy view of guest RAM
 * @param {string[]}   log  Array to append exception/error messages to
 * @param {object|null} emu  WasmEmulator instance (used for HW register I/O
 *                           and to record exceptions)
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

// ── Debug state ───────────────────────────────────────────────────────────────

/** Numeric guest PC of the most recent raise_exception call (0 = none yet). */
let lastRaisedExceptionPc   = 0;
/** Exception kind of the most recent raise_exception call (-1 = none yet). */
let lastRaisedExceptionKind = -1;
/**
 * Raw (unsigned) nextPc value returned by the most recently executed WASM
 * block.  0 means the block wrote CPU::pc itself (BranchRegIf / RaiseException
 * path); any other value is the static or dynamic branch target.
 * Updated by executeOneBlockSync; reset to 0 on ISO load / Reset.
 */
let lastNextPc = 0;
/**
 * Number of consecutive block executions where the PC did not change.
 * Resets to 0 whenever the PC advances to a new address.
 */
let stuckConsecutiveRuns = 0;
/**
 * Number of consecutive same-PC blocks that triggers a stuck-PC warning.
 * Also used to colour the stat row (yellow above this value, red above 4×).
 */
const STUCK_PC_THRESHOLD = 50;
/**
 * Reduces the periodic "STILL STUCK" re-dump rate.
 *
 * After the first stuck dump at STUCK_PC_THRESHOLD blocks, subsequent
 * re-dumps fire every STUCK_PC_THRESHOLD * STUCK_REDUMP_MULTIPLIER blocks
 * (i.e. 500 instead of 50), cutting console spam by 10× while still
 * confirming whether any state changes while the CPU is spinning.
 */
const STUCK_REDUMP_MULTIPLIER = 10;
/**
 * Multiplier on top of STUCK_PC_THRESHOLD used to detect a CPU that is
 * permanently stuck inside a PowerPC exception-vector address range
 * (0x00000000–0x00001FFF).  After this many consecutive same-PC blocks the
 * game loop halts with a diagnostic message instead of spinning forever.
 * Total threshold = STUCK_PC_THRESHOLD × STUCK_EXCEPTION_VECTOR_MULTIPLIER.
 */
const STUCK_EXCEPTION_VECTOR_MULTIPLIER = 10;
/** Ring buffer of the last DEBUG_EVENT_MAX notable emulation events. */
let debugEvents = [];
const DEBUG_EVENT_MAX = 30;

// ── MMIO access ring buffer ───────────────────────────────────────────────────

/**
 * Ring buffer of the last MMIO_RING_SIZE hardware-register accesses.
 * Each entry: { dir: "R"|"W", size: 8|16|32, addr: number, subsystem: string, val: number }
 * Populated by buildHooks() for 16- and 32-bit MMIO reads/writes.
 * Reset on ISO load / Reset via clearModuleCache().
 */
const MMIO_RING_SIZE = 8;
let recentMmioAccesses = [];

// ── Execution phase tracking ──────────────────────────────────────────────────

/**
 * PC phase label from the most recently executed block.
 * Used to detect phase transitions without generating per-block log noise.
 * Reset to "unknown" on ISO load / Reset.
 */
let prevPhase = "unknown";

/**
 * Wall-clock timestamps (performance.now()) for notable boot milestones.
 * null = not yet reached.  Reset on ISO load / Reset via clearModuleCache().
 *
 *   startedAt        — when startLoop() was first called after ISO load
 *   iplHleStarted    — first block executed in the ipl-hle address range
 *   apploaderRunning — ipl-hle printed "[IPL-HLE] Running apploader init"
 *   apploaderDone    — ipl-hle printed "[IPL-HLE] Apploader closed!"
 *   gameEntry        — first block executed in OS/game RAM (outside boot stubs)
 *   firstXfbContent  — first frame with non-zero XFB pixel data
 */
let milestones = {
  startedAt:        null,
  iplHleStarted:    null,
  apploaderRunning: null,
  apploaderDone:    null,
  gameEntry:        null,
  firstXfbContent:  null,
};

// ── OS banner / OSInit diagnostic state ──────────────────────────────────────

/**
 * True once the "EXI UART active" line has been emitted by the post-entry
 * watch, so we don't repeat it every frame while UART bytes keep arriving.
 */
let osUartActiveSeen = false;

/**
 * True once the post-game-entry OS-banner watch has produced its final log
 * entry (either from OSInit activity or from the 300-frame timeout).
 */
let osInitBannerDone = false;

/**
 * Animation frames elapsed since milestones.gameEntry fired.
 * Incremented once per frame in the per-frame OS-banner watch.
 */
let osPostEntryFrames = 0;

/**
 * Running total of EXI UART bytes produced by emu.take_uart_output() since
 * emulator creation.  Updated by drainUartOutput on every call.
 */
let totalEximUartBytes = 0;

/**
 * Snapshot of totalEximUartBytes at the moment milestones.gameEntry fired.
 * Any increase after that point means OSReport (EXI UART) is active.
 */
let uartBytesAtGameEntry = 0;

// ── Apploader / OS console stdout state ──────────────────────────────────────

/**
 * Maximum number of lines retained in the apploader/OS console log.
 * Older lines are dropped from the top when this limit is reached.
 */
const APPLOADER_LOG_MAX = 500;

/**
 * Ring buffer of lines already appended to the apploader log panel.
 * Used to enforce APPLOADER_LOG_MAX without re-scanning the DOM.
 */
let apploaderLogLines = [];

/**
 * Accumulates individual characters written to the GC EXI stdout port
 * (0xCC007000) until a newline is received, at which point the complete
 * line is flushed to the apploader log panel.
 */
let stdoutLineBuffer = "";

/**
 * Reference to the active WasmEmulator instance, updated by drainUartOutput
 * so that appendApploaderLog can read CPU registers (e.g. r3) at the exact
 * moment a key ipl-hle stdout line is emitted.
 * @type {WasmEmulator|null}
 */
let currentEmu = null;

/**
 * Feed a single byte from any stdout source (the ipl-hle direct
 * 0xCC007000 write path OR the EXI UART protocol used by OSReport)
 * through the line buffer and flush completed lines to the log panel.
 *
 * @param {number} ch  Byte value (0–255); 0x0D and 0x00 are silently dropped.
 */
function feedStdoutByte(ch) {
  if (ch === 0x0A /* \n */) {
    appendApploaderLog(stdoutLineBuffer);
    stdoutLineBuffer = "";
  } else if (ch !== 0x0D /* strip \r */ && ch !== 0x00 /* strip null padding */) {
    stdoutLineBuffer += String.fromCharCode(ch);
  }
}

/**
 * Flush any partial line accumulated in stdoutLineBuffer to the apploader
 * log, even if no trailing newline has been received.
 *
 * Call this before emitting diagnostic banners (UART-active, ArenaLo, timeout)
 * so that OSReport content that was never newline-terminated is still visible
 * in the apploader window.
 */
function flushStdoutLineBuffer() {
  if (stdoutLineBuffer.length > 0) {
    appendApploaderLog(stdoutLineBuffer);
    stdoutLineBuffer = "";
  }
}

/**
 * Drain any bytes queued in the EXI UART output buffer (OSReport output)
 * and feed them through the same stdoutLineBuffer pipeline used by the
 * ipl-hle 0xCC007000 direct-write path.
 *
 * Call this after every emulated block so that OSReport lines appear in
 * the apploader log in real time.
 *
 * @param {WasmEmulator} emu
 */
function drainUartOutput(emu) {
  currentEmu = emu;
  const bytes = emu.take_uart_output();
  totalEximUartBytes += bytes.length;
  for (const byte of bytes) {
    feedStdoutByte(byte);
  }
}

/**
 * Append a single line of apploader / OS output to the log panel.
 *
 * Lines are colour-coded by source prefix:
 *   [IPL-HLE]   — blue (emulator boot glue)
 *   [APPLOADER] — green (real apploader messages)
 *   others      — yellow (OS / unknown)
 *
 * @param {string} line  Raw line text (no trailing newline).
 */
function appendApploaderLog(line) {
  // Milestone detection: intercept key ipl-hle stdout lines before rendering.
  // The strings match what crates/ipl-hle/src/main.rs prints via stdout_write.
  if (milestones.apploaderRunning === null &&
      line.includes("Running apploader init")) {
    milestones.apploaderRunning = performance.now();
    const elapsed = milestones.startedAt !== null
      ? Math.trunc(milestones.apploaderRunning - milestones.startedAt) + " ms"
      : "?";
    console.info(`[lazuli] ✓ Milestone: apploader init started (${elapsed} since boot)`);
    pushDebugEvent(`✓ Milestone: apploader init (${elapsed})`);
  }
  if (milestones.apploaderDone === null &&
      line.includes("Apploader closed!")) {
    milestones.apploaderDone = performance.now();
    const elapsed = milestones.startedAt !== null
      ? Math.trunc(milestones.apploaderDone - milestones.startedAt) + " ms"
      : "?";
    console.info(
      `[lazuli] ✓ Apploader phase complete — exceptions: ${raiseExceptionTotal}, elapsed: ${elapsed}\n` +
      `         → jumping to OS/game RAM (game entry will be logged on first block there)`
    );
    pushDebugEvent(`✓ Apploader done (${elapsed})`);
  }

  // Log the init/main/close function pointers reported by ipl-hle so we can
  // immediately spot if the apploader's entry() wrote a wrong close address
  // (e.g. 0x8000522C instead of a valid 0x812xxxxx apploader function).
  // These lines match what crates/ipl-hle/src/main.rs prints in main():
  //   "  Init: 0x{hex}"  "  Main: 0x{hex}"  "  Close: 0x{hex}"
  if (line.startsWith("  Init: 0x") || line.startsWith("  Main: 0x") ||
      line.startsWith("  Close: 0x")) {
    // Expect values in the apploader range (0x81200000–0x812FFFFF); warn
    // if the address looks wrong so the divergence is immediately visible.
    const match = line.match(/0x([0-9a-fA-F]+)\s*$/);
    if (match) {
      const ptr = parseInt(match[1], 16) >>> 0;
      const inApploader = ptr >= 0x81200000 && ptr <= 0x812FFFFF;
      const label = line.trim().split(":")[0]; // "Init", "Main", or "Close"
      if (!inApploader) {
        const msg = `[lazuli] ⚠ ipl-hle ${label} fn ptr 0x${ptr.toString(16).padStart(8, "0")} ` +
          `is OUTSIDE the apploader range (0x81200000–0x812FFFFF) — ` +
          `this indicates the apploader's entry() wrote an incorrect function pointer`;
        console.warn(msg);
        appendApploaderLog(msg);
      } else {
        const msg = `[lazuli] ipl-hle ${label} fn ptr 0x${ptr.toString(16).padStart(8, "0")} ✓ (in apploader range)`;
        console.info(msg);
        appendApploaderLog(msg);
      }
    }
  }

  // Log the game entry point printed by ipl-hle just before calling entry().
  // This is the value returned by the apploader's close() function and is the
  // address the CPU will jump to next.  In native it is the real DOL entry
  // (e.g. 0x803439F4); a mismatch here is the root symptom of the divergence.
  if (line.includes("Jumping to bootfile entry: 0x")) {
    const match = line.match(/0x([0-9a-fA-F]+)\s*$/);
    if (match) {
      const entry = parseInt(match[1], 16) >>> 0;
      const inGameRam = entry >= 0x80000000 && entry <= 0x817FFFFF;
      const inApploader = entry >= 0x81200000 && entry <= 0x812FFFFF;
      const inIplHle   = entry >= 0x81300000 && entry <= 0x813FFFFF;
      let region = inIplHle ? "ipl-hle ⚠" : inApploader ? "apploader ⚠" : inGameRam ? "OS/game RAM" : "unknown ⚠";
      const expected = (entry >= 0x80000000 && entry <= 0x811FFFFF);
      if (!expected) {
        console.warn(
          `[lazuli] ⚠ ipl-hle bootfile entry 0x${entry.toString(16).padStart(8, "0")} ` +
          `is in ${region} — expected a game DOL entry in 0x80000000–0x811FFFFF`
        );
      } else {
        console.info(
          `[lazuli] ipl-hle bootfile entry 0x${entry.toString(16).padStart(8, "0")} ` +
          `(${region}) — looks correct`
        );
      }
      // Log r3 at the exact moment ipl-hle is about to jump to the game.
      // currentEmu is set by drainUartOutput so this is the live CPU state.
      if (currentEmu) {
        const r3Val = currentEmu.get_gpr(3) >>> 0;
        const r3Hex = "0x" + r3Val.toString(16).toUpperCase().padStart(8, "0");
        appendApploaderLog(`[lazuli] r3: ${r3Hex}  (at apploader close — compare with native)`);
        console.info(`[lazuli] r3: ${r3Hex}  (at apploader close)`);
      }
    }
  }

  const logEl = document.getElementById("apploader-log");
  if (!logEl) return;

  // If this is the first real line, clear the placeholder text.
  if (apploaderLogLines.length === 0 && logEl.firstChild &&
      logEl.firstChild.nodeType === Node.TEXT_NODE) {
    logEl.textContent = "";
  }

  const entry = document.createElement("div");
  if (line.startsWith("[IPL-HLE]")) {
    entry.className = "apploader-ipl";
  } else if (line.startsWith("[APPLOADER]")) {
    entry.className = "apploader-app";
  } else if (line.length > 0) {
    entry.className = "apploader-os";
  } else {
    entry.className = "apploader-text";
  }
  entry.textContent = line;
  logEl.appendChild(entry);

  apploaderLogLines.push(line);

  // Enforce the line-count limit by removing the oldest DOM child.
  if (apploaderLogLines.length > APPLOADER_LOG_MAX) {
    apploaderLogLines.shift();
    if (logEl.firstChild) logEl.removeChild(logEl.firstChild);
  }

  // Auto-scroll to the newest line.
  logEl.scrollTop = logEl.scrollHeight;
}

/**
 * Clear the apploader log panel and reset the line buffer.
 * Called on ISO load and emulator reset.
 */
function clearApploaderLog() {
  apploaderLogLines = [];
  stdoutLineBuffer  = "";
  const logEl = document.getElementById("apploader-log");
  if (logEl) logEl.textContent = "(no output yet — load an ISO to begin)";
}


/**
 * Set of guest PC addresses (as unsigned 32-bit numbers) at which the emulator
 * should pause.  When `gameLoop` reaches a block whose PC is in this set it
 * calls `stopLoop()` and reports the hit so the user can inspect CPU state.
 */
const breakpoints = new Set();

/**
 * Add a breakpoint at `pc` and refresh the breakpoint list UI.
 * @param {number} pc  Guest PC (unsigned 32-bit).
 */
function addBreakpoint(pc) {
  pc = pc >>> 0;
  breakpoints.add(pc);
  renderBreakpointList();
}

/**
 * Remove the breakpoint at `pc` and refresh the breakpoint list UI.
 * @param {number} pc  Guest PC (unsigned 32-bit).
 */
function removeBreakpoint(pc) {
  pc = pc >>> 0;
  breakpoints.delete(pc);
  renderBreakpointList();
}

/** Remove all breakpoints and refresh the UI. */
function clearBreakpoints() {
  breakpoints.clear();
  renderBreakpointList();
}

/**
 * Re-render the breakpoint list inside #bp-list.
 * Each entry shows the address and a small × remove button.
 */
function renderBreakpointList() {
  const el = $("bp-list");
  if (!el) return;
  if (breakpoints.size === 0) {
    el.textContent = "(no breakpoints set)";
    return;
  }
  el.textContent = "";
  const sorted = [...breakpoints].sort((a, b) => a - b);
  for (const pc of sorted) {
    const row = document.createElement("div");
    row.className = "bp-entry";
    const addr = document.createElement("span");
    addr.className = "bp-addr";
    addr.textContent = hexU32(pc);
    const rmBtn = document.createElement("button");
    rmBtn.className = "btn-secondary bp-remove";
    rmBtn.textContent = "×";
    rmBtn.title = `Remove breakpoint at ${hexU32(pc)}`;
    rmBtn.addEventListener("click", () => removeBreakpoint(pc));
    row.appendChild(addr);
    row.appendChild(rmBtn);
    el.appendChild(row);
  }
}

/**
 * Format a 32-bit unsigned integer as an 8-digit upper-case hex string with
 * the "0x" prefix.  Used throughout the debug/stuck-PC logging paths.
 *
 * @param {number} v
 * @returns {string}
 */
const hexU32 = (v) => "0x" + (v >>> 0).toString(16).toUpperCase().padStart(8, "0");

/**
 * Read a big-endian 32-bit word from a guest RAM view at a physical address.
 *
 * @param {Uint8Array} ram       Zero-copy RAM view (from getRamView).
 * @param {number}     physAddr  Physical address (no segment bits).
 * @returns {number} Unsigned 32-bit integer.
 */
function readRamU32(ram, physAddr) {
  const a = physAddr >>> 0;
  if (a + 3 >= ram.length) return 0;
  return ((ram[a] << 24) | (ram[a + 1] << 16) | (ram[a + 2] << 8) | ram[a + 3]) >>> 0;
}

/**
 * Convert a GC OS console-type code to a display string.
 *
 * The high nibble encodes the hardware class:
 *   0x0xxxxxxx — retail hardware
 *   0x1xxxxxxx — development hardware
 *
 * The hardware-revision label (e.g. "HW1", "HW2") that the real OS prints via
 * OSReport is game/OS-version-specific and cannot be reliably derived from the
 * stored value alone — it may come from a separate hardware register.  We
 * therefore report only the class and the raw hex value so the display is
 * always accurate.  The authoritative string (including "HW1", "HW2", kernel
 * date, etc.) arrives via EXI UART / OSReport once the game's OS runs.
 *
 * @param {number} type  Value of the OS global at 0x8000002C.
 * @returns {string}
 */
function osConsoleTypeString(type) {
  type = type >>> 0;
  const high = (type >>> 28) & 0xF;
  const hex  = type.toString(16).toUpperCase().padStart(8, '0');
  if (high === 0x1) return `Development (${hex})`;
  if (high === 0x0) return `Retail (${hex})`;
  return `Unknown (${hex})`;
}

/**
 * Log a synthetic OS-banner snapshot by reading OS globals directly from guest
 * RAM.  Logs Console Type and Memory size only — ArenaLo/Hi are NOT logged
 * here because the game DOL sections loaded by the apploader can overwrite
 * physical 0x30/0x34 before game entry fires.  The post-entry watch logs Arena
 * once OSInit has written a valid value.
 *
 * Physical addresses (= virtual - 0x80000000):
 *   0x28  RAM size (bytes)     → 0x8000_0028
 *   0x2C  Console type         → 0x8000_002C
 *
 * @param {Uint8Array} ram  Zero-copy guest RAM view.
 */
function logOsBannerFromRam(ram) {
  const consoleType = readRamU32(ram, 0x2C);
  const memSize     = readRamU32(ram, 0x28);
  const memMB       = memSize ? (memSize >>> 20) : 24;
  const typeStr     = osConsoleTypeString(consoleType);
  appendApploaderLog(`[OS] Console Type : ${typeStr}`);
  appendApploaderLog(`[OS] Memory ${memMB} MB`);
}

/**
 * Classify a guest program-counter value into a named emulator boot phase.
 *
 * The ranges mirror the load addresses used by parseAndLoadIso / load_ipl_hle:
 *   exception vectors — 0x00000000–0x00001FFF  (low-RAM exception handlers)
 *   ipl-hle           — 0x81300000–0x813FFFFF  (boot-glue DOL)
 *   apploader         — 0x81200000–0x812FFFFF  (real disc apploader)
 *   OS/game RAM       — 0x80000000–0x817FFFFF  (main GameCube RAM window)
 *   unknown           — anything else
 *
 * Used to annotate stuck/unstuck log lines and to detect phase transitions
 * in the game loop without generating per-block noise.
 *
 * @param {number} pc  Guest PC (unsigned 32-bit)
 * @returns {string}   Human-readable phase label
 */
function classifyPc(pc) {
  pc = pc >>> 0;
  if (pc < 0x00002000) return "exception vectors";
  if (pc >= 0x81300000 && pc <= 0x813FFFFF) return "ipl-hle";
  if (pc >= 0x81200000 && pc <= 0x812FFFFF) return "apploader";
  if (pc >= 0x80000000 && pc <= 0x817FFFFF) return "OS/game RAM";
  return "unknown";
}

/**
 * Map a hardware-register address (0xCCxxxxxx / 0xCDxxxxxx) to a short
 * subsystem label.  Used by the MMIO ring-buffer logging.
 *
 * Ranges match the dispatch table in crates/lazuli-web/src/hw/mod.rs:
 *   VI  0xCC002000  Video Interface
 *   PI  0xCC003000  Processor Interface
 *   DSP 0xCC005000  DSP Interface / ARAM
 *   DI  0xCC006000  DVD Interface
 *   SI  0xCC006400  Serial Interface
 *   EXI 0xCC006800  External Interface
 *   AI  0xCC006C00  Audio Interface
 *
 * @param {number} addr  Raw guest address (before PHYS_MASK)
 * @returns {string}     Subsystem label
 */
function mmioSubsystem(addr) {
  const off = addr & 0x00FFFFFF;
  if (off >= 0x002000 && off < 0x003000) return "VI";
  if (off >= 0x003000 && off < 0x004000) return "PI";
  if (off >= 0x005000 && off < 0x006000) return "DSP";
  if (off >= 0x006000 && off < 0x006400) return "DI";
  if (off >= 0x006400 && off < 0x006800) return "SI";
  if (off >= 0x006800 && off < 0x006C00) return "EXI";
  if (off >= 0x006C00 && off < 0x007000) return "AI";
  return "HW";
}

/**
 * Append a human-readable event to the debug event ring buffer and refresh
 * the on-screen debug log panel.
 *
 * @param {string} msg  Short description of the event.
 */
function pushDebugEvent(msg) {
  const ts = performance.now().toFixed(0);
  debugEvents.push(`[${ts}ms] ${msg}`);
  if (debugEvents.length > DEBUG_EVENT_MAX) debugEvents.shift();
  const el = $("debug-log");
  if (el) {
    el.textContent = debugEvents.slice().reverse().join("\n");
    el.scrollTop = 0;
  }
}

/**
 * @param {Uint8Array} ram  Zero-copy view of guest RAM
 * @param {string[]}   log  Array to append exception/error messages to
 * @param {object|null} emu  WasmEmulator instance (used for HW register I/O
 *                           and to record exceptions)
 * @param {number}     numericPc  Numeric guest PC of the block being executed.
 * @param {string}     [pcContext]  Human-readable PC string for console messages
 */
function buildHooks(ram, log, emu, numericPc, pcContext = "?") {
  // Mutable local view of guest RAM.  A DVD DMA (triggered via write_u32 to
  // DICR) calls into Rust's process_di_command, which may allocate heap memory
  // for console_log! formatting.  If that allocation triggers WASM linear-
  // memory growth, the original Uint8Array buffer is detached and all reads
  // from the old view return 0.  Using a `let` variable lets us refresh the
  // view inside write_u32 (after the DMA completes) so that any subsequent
  // hook calls in the same block execution see the correctly written data.
  let r = ram;
  return {
    read_u8(addr) {
      addr = addr >>> 0;
      // Hardware-register space (0xCCxxxxxx): all GC MMIO is 32-bit wide;
      // sub-word reads to HW space are not meaningful, return 0.
      if ((addr >>> 24 & 0xFE) === 0xCC) return 0;
      addr &= PHYS_MASK;
      return addr < r.length ? r[addr] : 0;
    },
    read_u16(addr) {
      addr = addr >>> 0;
      if ((addr >>> 24 & 0xFE) === 0xCC) {
        // Route to Rust hw_read_u16 for hardware registers (e.g. DSP mailbox).
        const val = emu ? emu.hw_read_u16(addr) & 0xFFFF : 0;
        recentMmioAccesses.push({ dir: "R", size: 16, addr, subsystem: mmioSubsystem(addr), val });
        if (recentMmioAccesses.length > MMIO_RING_SIZE) recentMmioAccesses.shift();
        return val;
      }
      addr &= PHYS_MASK;
      if (addr + 1 >= r.length) return 0;
      return (r[addr] << 8) | r[addr + 1];
    },
    read_u32(addr) {
      addr = addr >>> 0;
      // Route hardware-register reads to hw_read_u32 before masking so that
      // 0xCC006000 (DVD Interface) reaches the correct handler instead of
      // aliasing to RAM offset 0x00006000.
      if ((addr >>> 24 & 0xFE) === 0xCC) {
        const val = emu ? emu.hw_read_u32(addr) >>> 0 : 0;
        recentMmioAccesses.push({ dir: "R", size: 32, addr, subsystem: mmioSubsystem(addr), val });
        if (recentMmioAccesses.length > MMIO_RING_SIZE) recentMmioAccesses.shift();
        return val;
      }
      addr &= PHYS_MASK;
      if (addr + 3 >= r.length) return 0;
      return (((r[addr] << 24) | (r[addr + 1] << 16) |
               (r[addr + 2] << 8) | r[addr + 3]) >>> 0);
    },
    read_f64(addr) {
      // Read a big-endian IEEE-754 double from guest address.
      // GC hardware registers do not hold IEEE doubles — return 0.0.
      addr = addr >>> 0;
      if ((addr >>> 24 & 0xFE) === 0xCC) return 0.0;
      addr &= PHYS_MASK;
      if (addr + 7 >= r.length) return 0.0;
      const view = new DataView(r.buffer, r.byteOffset + addr, 8);
      return view.getFloat64(0, false /* big-endian */);
    },
    write_u8(addr, val) {
      addr = addr >>> 0;
      // Intercept character writes to the GC EXI stdout port.
      // The ipl-hle and apploader write to 0xCC007000 (cached) or
      // 0xCD007000 (uncached) one byte at a time using stb instructions.
      // Check the high byte with the cached/uncached mirror bit masked out
      // (same mask used elsewhere: (addr >>> 24 & 0xFE) === 0xCC).
      if ((addr >>> 24 & 0xFE) === 0xCC && (addr & 0x00FFFFFF) === 0x007000) {
        const ch = val & 0xFF;
        feedStdoutByte(ch);
        return;
      }
      if ((addr >>> 24 & 0xFE) === 0xCC) return; // HW registers are 32-bit only
      addr &= PHYS_MASK;
      if (addr < r.length) r[addr] = val & 0xff;
    },
    write_u16(addr, val) {
      addr = addr >>> 0;
      if ((addr >>> 24 & 0xFE) === 0xCC) {
        // Route to Rust hw_write_u16 for hardware registers (e.g. DSP mailbox).
        recentMmioAccesses.push({ dir: "W", size: 16, addr, subsystem: mmioSubsystem(addr), val: val & 0xFFFF });
        if (recentMmioAccesses.length > MMIO_RING_SIZE) recentMmioAccesses.shift();
        if (emu) emu.hw_write_u16(addr, val & 0xFFFF);
        return;
      }
      addr &= PHYS_MASK;
      if (addr + 1 < r.length) {
        r[addr]     = (val >> 8) & 0xff;
        r[addr + 1] = val & 0xff;
      }
    },
    write_u32(addr, val) {
      addr = addr >>> 0;
      val  = val  >>> 0;
      // Route hardware-register writes to hw_write_u32 before masking.
      // Writing 0xCC006000-0xCC006027 drives the DVD Interface; bit 0 of
      // DICR (0x1C) triggers a DMA from the stored disc image into guest RAM.
      if ((addr >>> 24 & 0xFE) === 0xCC) {
        recentMmioAccesses.push({ dir: "W", size: 32, addr, subsystem: mmioSubsystem(addr), val });
        if (recentMmioAccesses.length > MMIO_RING_SIZE) recentMmioAccesses.shift();
        if (emu) {
          emu.hw_write_u32(addr, val);
          // A DVD Read DMA may have just overwritten guest code in RAM.
          // Selectively invalidate only cached blocks whose physical address
          // range overlaps the DMA destination — mirroring ppcjit's per-address
          // Blocks::invalidate() rather than a blanket cache flush.
          if (emu.take_dma_dirty()) {
            const dmaPhysStart = emu.last_dma_addr() >>> 0;
            const dmaPhysLen   = emu.last_dma_len()  >>> 0;
            const dmaPhysEnd   = dmaPhysStart + dmaPhysLen;
            for (const [vpc] of moduleCache) {
              const physPc   = (vpc & PHYS_MASK) >>> 0;
              const meta     = blockMetaMap.get(vpc);
              // Block covers [physPc, physPc + insCount*4); if insCount is
              // unknown fall back to 4 bytes (one instruction — conservative).
              const blockEnd = physPc + (meta ? meta.insCount * 4 : 4);
              if (physPc < dmaPhysEnd && blockEnd > dmaPhysStart) {
                moduleCache.delete(vpc);
                blockMetaMap.delete(vpc);
              }
            }
            // Refresh the local RAM view: the Rust console_log! inside
            // process_di_command may have caused WASM linear-memory growth,
            // detaching the old Uint8Array.  getRamView detects the buffer
            // change and returns a fresh view over the new memory.
            r = getRamView(emu);
            // Mirror the Rust-side "[lazuli] DI: DVD Read" console_log! to the
            // apploader-log panel so DI activity is visible in the UI without
            // needing to open the browser developer console.
            const discOff = emu.last_di_disc_offset() >>> 0;
            const preview = [];
            const previewLen = Math.min(dmaPhysLen, 8);
            for (let i = 0; i < previewLen; i++) {
              preview.push(r[dmaPhysStart + i].toString(16).padStart(2, "0"));
            }
            for (let i = previewLen; i < 8; i++) {
              preview.push("00");
            }
            appendApploaderLog(
              `[lazuli] DI: DVD Read disc_off=0x${discOff.toString(16).padStart(8, "0")}` +
              ` len=0x${dmaPhysLen.toString(16)}` +
              ` ram_dest=0x${dmaPhysStart.toString(16).padStart(8, "0")}` +
              ` data=[${preview.join(" ")}]`
            );
          }
        }
        return;
      }
      addr &= PHYS_MASK;
      if (addr + 3 < r.length) {
        r[addr]     = (val >>> 24) & 0xff;
        r[addr + 1] = (val >>> 16) & 0xff;
        r[addr + 2] = (val >>>  8) & 0xff;
        r[addr + 3] =  val         & 0xff;
      }
    },
    write_f64(addr, val) {
      // Write a big-endian IEEE-754 double to guest address.
      addr = addr >>> 0;
      if ((addr >>> 24 & 0xFE) === 0xCC) return; // HW registers are not doubles
      addr &= PHYS_MASK;
      if (addr + 7 >= r.length) return;
      const view = new DataView(r.buffer, r.byteOffset + addr, 8);
      view.setFloat64(0, val, false /* big-endian */);
    },
    raise_exception(kind) {
      lastRaisedExceptionPc   = numericPc;
      lastRaisedExceptionKind = kind;
      if (log) log.push(`exception: kind=${kind}`);
      raiseExceptionTotal++;
      if (emu) emu.record_raise_exception();
      if (raiseExceptionTotal <= RAISE_EXCEPTION_LOG_LIMIT) {
        console.warn(`[lazuli] raise_exception(kind=${kind}) in block @ ${pcContext} (total #${raiseExceptionTotal})`);
        if (raiseExceptionTotal === RAISE_EXCEPTION_LOG_LIMIT) {
          console.warn("[lazuli] raise_exception log limit reached — suppressing further messages");
        }
      }
      pushDebugEvent(`⚡ exception(${kind}) at ${pcContext}`);
    },

    // ── Quantized paired-single memory hooks ─────────────────────────────────
    //
    // These implement the GameCube Graphics Quantization Register (GQR) logic.
    // Each GQR is a 32-bit value with:
    //   bits [2:0]   store_type  (0=float, 4=u8, 5=u16, 6=i8, 7=i16)
    //   bits [13:8]  store_scale (signed 6-bit scale factor)
    //   bits [18:16] load_type
    //   bits [29:24] load_scale

    /**
     * Load one dequantized element from guest memory.
     * @param {number} addr  Guest physical address (i32)
     * @param {number} gqr   GQR register value (i32, interpreted as u32)
     * @returns {number}     Dequantized f64 value
     */
    psq_load(addr, gqr) {
      addr = addr >>> 0;
      gqr  = gqr  >>> 0;
      addr &= PHYS_MASK;
      const loadType  = (gqr >>> 16) & 7;
      const loadScale = (gqr >>> 24) & 63;
      // The scale is a 6-bit signed value: if bit 5 is set it is negative.
      const scale = loadScale >= 32 ? loadScale - 64 : loadScale;
      // Dequantize factor: 2^(-scale) for loads.
      const factor = scale >= 0 ? 1.0 / (1 << scale) : (1 << (-scale));

      switch (loadType) {
        case 0: { // float — 4 bytes, no scaling
          if (addr + 3 >= r.length) return 0.0;
          const bits = (((r[addr] << 24) | (r[addr+1] << 16) | (r[addr+2] << 8) | r[addr+3]) >>> 0);
          // Reinterpret u32 as IEEE-754 f32 then promote to f64.
          const buf = new ArrayBuffer(4);
          new Uint32Array(buf)[0] = bits;
          return new Float32Array(buf)[0];
        }
        case 4: { // u8 — 1 byte, scale
          const byte = addr < r.length ? r[addr] : 0;
          return byte * factor;
        }
        case 5: { // u16 — 2 bytes big-endian, scale
          if (addr + 1 >= r.length) return 0.0;
          const val = ((r[addr] << 8) | r[addr+1]) & 0xFFFF;
          return val * factor;
        }
        case 6: { // i8 — 1 byte signed, scale
          const byte = addr < r.length ? r[addr] : 0;
          return ((byte << 24) >> 24) * factor; // sign-extend 8→32
        }
        case 7: { // i16 — 2 bytes big-endian signed, scale
          if (addr + 1 >= r.length) return 0.0;
          const val = ((r[addr] << 8) | r[addr+1]) & 0xFFFF;
          return (((val << 16) >> 16)) * factor; // sign-extend 16→32
        }
        default: return 0.0; // reserved types
      }
    },

    /**
     * Quantize and store one element to guest memory.
     * @param {number} addr  Guest physical address (i32)
     * @param {number} gqr   GQR register value (i32)
     * @param {number} val   f64 value to store
     * @returns {number}     Byte count written (1, 2, or 4) as i32
     */
    psq_store(addr, gqr, val) {
      addr = addr >>> 0;
      gqr  = gqr  >>> 0;
      addr &= PHYS_MASK;
      const storeType  = gqr & 7;
      const storeScale = (gqr >>> 8) & 63;
      const scale = storeScale >= 32 ? storeScale - 64 : storeScale;
      // Quantize factor: 2^scale for stores (inverse of load).
      const factor = scale >= 0 ? (1 << scale) : 1.0 / (1 << (-scale));

      switch (storeType) {
        case 0: { // float — 4 bytes, no scaling
          const buf = new ArrayBuffer(4);
          new Float32Array(buf)[0] = val;
          const bits = new Uint32Array(buf)[0] >>> 0;
          if (addr + 3 < r.length) {
            r[addr]   = (bits >>> 24) & 0xFF;
            r[addr+1] = (bits >>> 16) & 0xFF;
            r[addr+2] = (bits >>>  8) & 0xFF;
            r[addr+3] =  bits         & 0xFF;
          }
          return 4;
        }
        case 4: { // u8
          const q = Math.max(0, Math.min(255, (val * factor) | 0));
          if (addr < r.length) r[addr] = q & 0xFF;
          return 1;
        }
        case 5: { // u16
          const q = Math.max(0, Math.min(65535, (val * factor) | 0));
          if (addr + 1 < r.length) {
            r[addr]   = (q >> 8) & 0xFF;
            r[addr+1] =  q       & 0xFF;
          }
          return 2;
        }
        case 6: { // i8
          const q = Math.max(-128, Math.min(127, (val * factor) | 0));
          if (addr < r.length) r[addr] = q & 0xFF;
          return 1;
        }
        case 7: { // i16
          const q = Math.max(-32768, Math.min(32767, (val * factor) | 0));
          if (addr + 1 < r.length) {
            r[addr]   = (q >> 8) & 0xFF;
            r[addr+1] =  q       & 0xFF;
          }
          return 2;
        }
        default: return 4; // reserved types — treat as float
      }
    },

    /**
     * Return the byte size of one load element based on GQR.load_type.
     * @param {number} gqr  GQR register value (i32)
     * @returns {number}    1, 2, or 4 as i32
     */
    psq_load_size(gqr) {
      gqr = gqr >>> 0;
      const loadType = (gqr >>> 16) & 7;
      switch (loadType) {
        case 0:  return 4;  // float
        case 4:             // u8
        case 6:  return 1;  // i8
        case 5:             // u16
        case 7:  return 2;  // i16
        default: return 4;  // reserved — treat as float
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

/**
 * Per-PC block metadata: cycles count and instruction count.
 *
 * Populated whenever a block is compiled for the first time.  JavaScript uses
 * this to advance the decrementer by the correct number of timebase ticks
 * (`cycles / 12`) rather than a fixed per-block constant — mirroring the way
 * the native emulator's `Scheduler` is fed by `Block::meta().cycles` from
 * ppcjit.  The `insCount` field (instruction count × 4 = byte span) is used
 * during DMA invalidation to determine whether a cached block overlaps the
 * written address range.
 */
const blockMetaMap = new Map(); // u32 pc → { cycles: u32, insCount: u32 }

/**
 * Per-PC execution hit counts.  Incremented each time a block at that PC is
 * executed; cleared alongside `moduleCache` on ISO load / Reset.
 */
const pcHitMap = new Map(); // u32 pc → number

function clearModuleCache() {
  moduleCache.clear();
  blockMetaMap.clear();
  pcHitMap.clear();
  raiseExceptionTotal = 0;
  regsMemCache = null;
  // Reset debug state so stale info from a previous ISO does not carry over.
  lastRaisedExceptionPc   = 0;
  lastRaisedExceptionKind = -1;
  lastNextPc              = 0;
  stuckConsecutiveRuns    = 0;
  debugEvents             = [];
  const el = $("debug-log");
  if (el) el.textContent = "(no events yet)";
  clearApploaderLog();
  // Reset phase-tracking, MMIO ring, and milestones so stale data from a
  // previous ISO session does not pollute the next session's diagnostics.
  recentMmioAccesses = [];
  prevPhase          = "unknown";
  milestones = {
    startedAt:        null,
    iplHleStarted:    null,
    apploaderRunning: null,
    apploaderDone:    null,
    gameEntry:        null,
    firstXfbContent:  null,
  };
  // Reset OS-banner / OSInit diagnostic state.
  osUartActiveSeen   = false;
  osInitBannerDone   = false;
  osPostEntryFrames  = 0;
  totalEximUartBytes = 0;
  uartBytesAtGameEntry = 0;
}

// ── Synchronous block execution ───────────────────────────────────────────────

const WASM_PAGE = 65536;

/**
 * Single `WebAssembly.Memory` used as the register-file backing store for
 * every JIT block execution.  Allocated lazily on first use and reused
 * across all subsequent calls to `executeOneBlockSync` to avoid creating a
 * new 64 KB allocation (+ `get_cpu_bytes` externref copy) on every block.
 *
 * @type {{ mem: WebAssembly.Memory, view: Uint8Array } | null}
 */
let regsMemCache = null;

/**
 * Return (or lazily create) the shared register-file memory.
 *
 * @param {WasmEmulator} emu
 * @returns {{ mem: WebAssembly.Memory, view: Uint8Array }}
 */
function getRegsMem(emu) {
  if (!regsMemCache) {
    const cpuSize    = emu.cpu_struct_size();
    const pagesNeeded = Math.ceil(cpuSize / WASM_PAGE);
    const mem  = new WebAssembly.Memory({ initial: pagesNeeded });
    regsMemCache = { mem, view: new Uint8Array(mem.buffer) };
  }
  return regsMemCache;
}

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
      // Dump full CPU state so the register values that led to the bad PC are
      // visible in the log even though no block executed at this address yet.
      const gprDump = Array.from({ length: 32 }, (_, i) =>
        `r${i}=${hexU32(emu.get_gpr(i))}`
      ).join(" ");
      console.error(
        `[lazuli] compile_block pre-failure CPU state @ ${pcHex}:\n` +
        `  GPR: ${gprDump}\n` +
        `  CR=${hexU32(emu.get_cr())}  CTR=${hexU32(emu.get_ctr())}  LR=${hexU32(emu.get_lr())}`,
      );
      const msg = `compile error @ ${pcHex}: ${e}`;
      console.error(`[lazuli] ${msg}`);
      if (log) log.push(`[${pcHex}] compile error: ${e}`);
      pushDebugEvent(`✗ ${msg}`);
      return false;
    }
    try {
      module = new WebAssembly.Module(wasmBytes);
    } catch (e) {
      const msg = `WebAssembly.Module error @ ${pcHex}: ${e}`;
      console.error(`[lazuli] ${msg}`);
      if (log) log.push(`[${pcHex}] WebAssembly.Module error: ${e}`);
      pushDebugEvent(`✗ ${msg}`);
      return false;
    }
    moduleCache.set(pc, module);
    // Record cycle count and size so the game loop can use the real block
    // cost for decrementer advancement (mirrors ppcjit's Meta::cycles feed
    // into the native Scheduler).
    blockMetaMap.set(pc, {
      cycles:   emu.last_compiled_cycles(),
      insCount: emu.last_compiled_ins_count(),
    });
  }

  // ── Step 2: write CPU register file into the shared (cached) WASM memory ──
  const cpuSize        = emu.cpu_struct_size();
  const { mem: regsMem, view: regsView } = getRegsMem(emu);
  regsView.set(emu.get_cpu_bytes(), 0);



  // Reset exception tracking so we can detect whether THIS block raises an
  // exception (vs. a stale value left by a previous block).
  lastRaisedExceptionKind = -1;

  // ── Step 3: instantiate with hook closures that use the zero-copy RAM ────
  let instance;
  try {
    instance = new WebAssembly.Instance(module, {
      env:   { memory: regsMem },
      hooks: buildHooks(ram, log, emu, pc, pcHex),
    });
  } catch (e) {
    const msg = `instantiation error @ ${pcHex}: ${e}`;
    console.error(`[lazuli] ${msg}`);
    if (log) log.push(`[${pcHex}] instantiation error: ${e}`);
    pushDebugEvent(`✗ ${msg}`);
    return false;
  }

  // ── Step 4: execute ───────────────────────────────────────────────────────
  let nextPc;
  try {
    nextPc = instance.exports.execute(0 /* regs_ptr = 0 */);
  } catch (e) {
    const msg = `execution error @ ${pcHex}: ${e}`;
    console.error(`[lazuli] ${msg}`);
    if (log) log.push(`[${pcHex}] execution error: ${e}`);
    pushDebugEvent(`✗ ${msg}`);
    return false;
  }

  // ── Step 5: sync CPU state back; RAM is already in sync (zero-copy) ──────
  emu.set_cpu_bytes(new Uint8Array(regsMem.buffer, 0, cpuSize));
  emu.record_block_executed();
  pcHitMap.set(pc, (pcHitMap.get(pc) ?? 0) + 1);

  // Drain any EXI UART output produced by this block (e.g. OSReport calls).
  // Must be called after set_cpu_bytes so that any EXI writes made during
  // execution are already committed to the Rust emulator state.
  drainUartOutput(emu);

  // Record the raw nextPc for stuck-PC diagnostics in the game loop.
  lastNextPc = nextPc >>> 0;

  // ── Step 6: advance the program counter ──────────────────────────────────
  if (lastNextPc === 0 && lastRaisedExceptionKind >= 0) {
    // An exception was raised by the WASM block (Sc, Illegal, etc.).
    // The CPU state has been synced from WASM memory, including any StorePC
    // writes that save the return address before the exception (e.g. `sc`
    // saves pc+4 into CPU::pc before calling raise_exception).
    // Deliver the exception: update SRR0, SRR1, MSR, and CPU::pc to the
    // correct exception vector, exactly as real hardware does.
    emu.deliver_exception(lastRaisedExceptionKind);
    // CPU::pc is now the exception vector — no manual set_pc call needed.
  } else {
    // Normal branch path: either a static/dynamic target was returned, or
    // BranchRegIf wrote CPU::pc directly into WASM memory (synced above).
    const newPc = lastNextPc !== 0 ? lastNextPc : emu.get_pc();

    // Detect branch-to-self via ReturnDynamic: a non-zero nextPc equal to the
    // block's own start address means LR or CTR was set to this PC.
    if (lastNextPc !== 0 && lastNextPc === pc) {
      console.warn(
        `[lazuli] ${pcHex}: branch-to-self via ReturnDynamic — ` +
        `LR=${hexU32(emu.get_lr())} CTR=${hexU32(emu.get_ctr())} (block returned own PC as nextPc)`,
      );
    }

    // Warn when a dynamic branch (blr / bcctr / rfi via ReturnDynamic, or a
    // conditional bcctr/blr via BranchRegIf) resolves to an unexpectedly low
    // address.  Two sub-cases:
    //
    //   • lastNextPc === 0, no exception raised:
    //       ReturnDynamic stored 0 to CPU::pc and returned 0, OR BranchRegIf
    //       wrote CTR/LR=0 to CPU::pc and returned 0.  Either way the dynamic
    //       branch target was 0x00000000, which is almost always a bug
    //       (CTR=0 before bcctr, or LR=0 before blr).
    //
    //   • lastNextPc !== 0 but newPc < 0x80000000:
    //       ReturnDynamic returned a non-zero target that is below the normal
    //       GameCube RAM window — likely a corrupted CTR/LR value.
    //
    // In both cases we log CTR and LR at the exact moment the branch fires
    // (CPU state has already been synced back from WASM memory above).
    const ctrVal = emu.get_ctr();
    const lrVal  = emu.get_lr();
    if (lastNextPc === 0 && lastRaisedExceptionKind < 0) {
      // Dynamic branch resolved to 0x00000000 with no exception — most likely
      // bcctr/blr with CTR=0 / LR=0.
      console.warn(
        `[lazuli] ${pcHex}: dynamic branch → 0x00000000 (no exception) — ` +
        `CTR=${hexU32(ctrVal)} LR=${hexU32(lrVal)} ` +
        `(BranchRegIf or ReturnDynamic with target=0; check CTR/LR before bcctr/blr)`,
      );
      pushDebugEvent(`⚠ dyn-branch→0 @ ${pcHex} CTR=${hexU32(ctrVal)} LR=${hexU32(lrVal)}`);
    } else if (lastNextPc !== 0 && newPc < 0x80000000) {
      // ReturnDynamic returned a non-zero target below the GameCube RAM window.
      console.warn(
        `[lazuli] ${pcHex}: dynamic branch → low address ${hexU32(newPc)} — ` +
        `CTR=${hexU32(ctrVal)} LR=${hexU32(lrVal)} ` +
        `(possible corrupted CTR/LR; rawNextPc=${hexU32(lastNextPc)})`,
      );
      pushDebugEvent(`⚠ dyn-branch→low ${hexU32(newPc)} @ ${pcHex} CTR=${hexU32(ctrVal)}`);
    }

    emu.set_pc(newPc);
  }

  if (log) {
    const curPc = emu.get_pc();
    const ctrVal = emu.get_ctr();
    const newHex = "0x" + (curPc >>> 0).toString(16).toUpperCase().padStart(8, "0");
    log.push(
      `[${pcHex}] executed → next PC ${newHex} ` +
      `(rawNextPc=${hexU32(lastNextPc)} CTR=${hexU32(ctrVal)})`,
    );
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

// ── Web Audio / DSP audio pipeline ───────────────────────────────────────────

/**
 * AudioWorklet processor source for the GameCube DSP audio output.
 *
 * The processor maintains independent left/right ring buffers (8192 frames
 * each).  The main thread feeds interleaved stereo f32 PCM samples via
 * `postMessage`; the processor drains them into the Web Audio output on
 * every 128-sample process() tick (the standard AudioWorklet quantum size).
 *
 * When the ring buffer is empty the processor outputs silence rather than
 * repeating stale audio.
 */
const DSP_WORKLET_SOURCE = `
class DspAudioProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super(options);
    const SIZE    = 8192; // must be a power of two so that (pos + n) & MASK is equivalent to modulo
    const MASK    = SIZE - 1;
    this._ringL   = new Float32Array(SIZE);
    this._ringR   = new Float32Array(SIZE);
    this._writePos = 0;
    this._readPos  = 0;
    this._avail    = 0;
    this._size     = SIZE;
    this._mask     = MASK;

    this.port.onmessage = ({ data }) => {
      // data.left / data.right are Float32Array (transferred, not copied).
      const { left, right } = data;
      const n = Math.min(left.length, this._size - this._avail);
      for (let i = 0; i < n; i++) {
        const p = (this._writePos + i) & this._mask;
        this._ringL[p] = left[i];
        this._ringR[p] = right[i];
      }
      this._writePos = (this._writePos + n) & this._mask;
      this._avail   += n;
    };
  }

  process(_inputs, outputs) {
    const out  = outputs[0];
    const outL = out[0];
    // Support both mono (1 channel) and stereo (2 channel) output nodes.
    const outR = out.length > 1 ? out[1] : out[0];
    const n    = outL.length; // typically 128

    const canRead = Math.min(n, this._avail);
    for (let i = 0; i < canRead; i++) {
      const p = (this._readPos + i) & this._mask;
      outL[i] = this._ringL[p];
      outR[i] = this._ringR[p];
    }
    this._readPos = (this._readPos + canRead) & this._mask;
    this._avail  -= canRead;
    // Frames beyond canRead remain at the default 0.0 (silence).

    return true; // Keep processor alive.
  }
}
registerProcessor('dsp-audio-processor', DspAudioProcessor);
`;

let audioCtx    = null;
let audioActive = false;
/** The AudioWorkletNode that feeds GameCube DSP PCM to the speakers. */
let dspWorkletNode = null;

/**
 * Initialise the DSP `AudioWorkletNode` pipeline.
 *
 * Registers the worklet processor via an inline Blob URL (no separate file
 * needed) and connects the node to the AudioContext destination.  Called
 * once from `initAudio()` after the AudioContext is created.
 *
 * @returns {Promise<void>}
 */
async function initDspAudioWorklet() {
  if (!audioCtx) return;
  try {
    const blob = new Blob([DSP_WORKLET_SOURCE], { type: "application/javascript" });
    const url  = URL.createObjectURL(blob);
    await audioCtx.audioWorklet.addModule(url);
    URL.revokeObjectURL(url);

    dspWorkletNode = new AudioWorkletNode(audioCtx, "dsp-audio-processor", {
      numberOfInputs:  0,
      numberOfOutputs: 1,
      outputChannelCount: [2], // Stereo output
    });
    dspWorkletNode.connect(audioCtx.destination);
    console.log("[lazuli] DSP AudioWorklet pipeline ready (32 kHz stereo)");
  } catch (e) {
    console.warn("[lazuli] AudioWorklet init failed — audio will be silent:", e);
  }
}

/**
 * Push interleaved stereo f32 PCM samples to the DSP audio worklet.
 *
 * Call this once per animation frame with the samples produced by the
 * GameCube DSP emulator.  Samples are transferred (not copied) to the
 * worklet thread for zero-allocation audio delivery.
 *
 * @param {Float32Array} interleavedSamples  Interleaved L/R pairs:
 *        [L0, R0, L1, R1, …] at 32 000 Hz.
 *        Typically 32000/60 ≈ 533 frames = 1066 values per animation frame.
 */
function pushDspSamples(interleavedSamples) {
  if (!dspWorkletNode || !interleavedSamples || interleavedSamples.length < 2) return;

  const n    = interleavedSamples.length >> 1; // number of stereo frames
  const left  = new Float32Array(n);
  const right = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    left[i]  = interleavedSamples[i * 2];
    right[i] = interleavedSamples[i * 2 + 1];
  }
  // Transfer the buffers to the worklet thread (zero-copy).
  dspWorkletNode.port.postMessage({ left, right }, [left.buffer, right.buffer]);
}

/**
 * Initialise (or resume) the Web Audio context and DSP audio worklet.
 *
 * Must be triggered by a user gesture (click) due to browser autoplay
 * policies.  A short startup chime confirms that audio routing is working.
 */
function initAudio() {
  if (!audioCtx) {
    audioCtx = new AudioContext({ sampleRate: 32000 });
  }
  if (audioCtx.state === "suspended") {
    audioCtx.resume();
  }

  // Initialise the DSP AudioWorklet pipeline so it is ready to receive PCM
  // samples from the GameCube DSP emulator.
  initDspAudioWorklet().catch((e) =>
    console.warn("[lazuli] DSP worklet setup failed:", e)
  );

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

/**
 * Fallback decrementer ticks to advance when a block's real cycle count is
 * unavailable (e.g. before any block at that PC has been compiled this
 * session).  Equals `ceil(675000 / 500) = 1350` — the same fixed budget
 * used before per-block cycle tracking was introduced.
 *
 * In the steady state this constant is never used because `blockMetaMap`
 * always has a `cycles` entry for every executed block: a block must be
 * compiled (and its metadata recorded) before it can be executed.
 */
const TICKS_PER_BLOCK_FALLBACK = Math.ceil(TIMEBASE_TICKS_PER_FRAME / BLOCKS_PER_FRAME);

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
 * Returns the current MSR.EE (External Interrupt Enable) bit as 0 or 1.
 *
 * MSR bit 15 is the EE flag.  Used by the game loop's EE edge-detection logic
 * to determine when the CPU re-enables external interrupts (e.g. after `rfi`
 * or `mtmsr`), which is the point at which pending PI interrupts should fire.
 *
 * @param {WasmEmulator} emu
 * @returns {0|1}
 */
function getMsrEe(emu) {
  return (emu.get_msr() >>> 15) & 1;
}

/**
 * Main emulation loop — called by requestAnimationFrame at ~60 Hz.
 *
 * Each frame executes up to BLOCKS_PER_FRAME JIT blocks, renders the XFB to
 * the canvas, and updates the stats display.
 */
function gameLoop(emu, canvas, ctx, timestamp) {
  if (!running) return;

  // Poll the Gamepad API and merge with keyboard state before executing
  // blocks, so that the emulator reads the latest controller input.
  pollGamepad(emu);

  // Advance the time base before executing blocks so that mftb-based timing
  // loops inside the game see a non-zero delta on the very first frame.
  emu.advance_timebase(TIMEBASE_TICKS_PER_FRAME);

  // Assert the VI (Video Interface) vertical-retrace interrupt once per frame
  // (~60 Hz).  Games and the OS use VIWaitForRetrace() — a spin loop that
  // briefly enables EE so the VI external interrupt can fire.  Without this
  // call the OS retrace counter never increments and VIWaitForRetrace() spins
  // forever.  The EE edge-detection in the block loop below will deliver the
  // pending External exception as soon as the OS re-enables EE.
  emu.assert_vi_interrupt();

  // Execute blocks for this frame
  let ram = getRamView(emu);
  let blocksThisFrame = 0;
  let loopError = false;
  // Track EE bit across blocks to detect 0→1 transitions (see EE edge-detection comment below).
  let prevBlockEe = getMsrEe(emu);
  for (let i = 0; i < BLOCKS_PER_FRAME; i++) {
    const blockPc = emu.get_pc();

    // ── Phase transition detection ────────────────────────────────────────────
    // Classify the current PC into a named boot phase and emit exactly one log
    // line per phase boundary — no per-block noise.  Also sets milestone
    // timestamps for ipl-hle entry and game entry (OS/game RAM first reached).
    {
      const blockPhase = classifyPc(blockPc);
      if (blockPhase !== prevPhase) {
        const transMsg =
          `→ Phase transition: ${prevPhase} → ${blockPhase} @ ${hexU32(blockPc)} ` +
          `(block #${emu.blocks_executed()})`;
        console.info(`[lazuli] ${transMsg}`);
        appendApploaderLog(transMsg);

        if (blockPhase === "ipl-hle" && milestones.iplHleStarted === null) {
          milestones.iplHleStarted = performance.now();
          const elapsed = milestones.startedAt !== null
            ? Math.trunc(milestones.iplHleStarted - milestones.startedAt) + " ms" : "?";
          console.info(`[lazuli] ✓ Milestone: ipl-hle started (${elapsed} since boot)`);
          appendApploaderLog(`✓ Milestone: ipl-hle started (${elapsed})`);
        }
        if (blockPhase === "OS/game RAM" && milestones.gameEntry === null) {
          milestones.gameEntry = performance.now();
          const elapsed = milestones.startedAt !== null
            ? Math.trunc(milestones.gameEntry - milestones.startedAt) + " ms" : "?";
          console.info(
            `[lazuli] ✓ Milestone: game entry @ ${hexU32(blockPc)} — OS/game RAM first reached ` +
            `(${elapsed} since boot)`
          );
          appendApploaderLog(`✓ Milestone: game entry @ ${hexU32(blockPc)} (${elapsed})`);
          // Log a synthetic OS-banner snapshot from the current RAM globals.
          // ArenaLo is deliberately omitted — the apploader may have overwritten
          // physical 0x30 while loading game DOL sections.  The post-entry watch
          // below logs Arena once OSInit has written a valid value.
          logOsBannerFromRam(ram);
          uartBytesAtGameEntry = totalEximUartBytes;
        }

        // When ipl-hle exits (to OS/game RAM or any other region), dump all
        // GPRs so the wrong-entry-point bug is always diagnosable regardless
        // of which ipl-hle binary layout is in use.  At this point the CPU
        // state reflects the last ipl-hle block that branched to blockPc, so
        // r3 holds whatever close() returned and CTR/LR hold the branch target.
        if (prevPhase === "ipl-hle") {
          const gprVals = Array.from({ length: 32 }, (_, i) => `r${i}=${hexU32(emu.get_gpr(i))}`);
          const lrVal  = hexU32(emu.get_lr());
          const ctrVal = hexU32(emu.get_ctr());
          const crVal  = hexU32(emu.get_cr());
          const dumpMsg =
            `[lazuli] ipl-hle exit → ${blockPhase} @ ${hexU32(blockPc)} GPR dump:\n` +
            gprVals.slice(0, 16).join("  ") + "\n" +
            gprVals.slice(16).join("  ") + "\n" +
            `  LR=${lrVal}  CTR=${ctrVal}  CR=${crVal}`;
          console.warn(dumpMsg);
          // Diagnose the entry-point divergence: blockPc is where ipl-hle
          // jumped to via entry().  If it matches CTR the call was via bctrl/bctr;
          // the expected range for the real game DOL is 0x80000000–0x811FFFFF.
          const entryPc = blockPc >>> 0;
          const inApploader = entryPc >= 0x81200000 && entryPc <= 0x812FFFFF;
          const inIplHle    = entryPc >= 0x81300000 && entryPc <= 0x813FFFFF;
          if (inApploader || inIplHle) {
            const region = inIplHle ? "ipl-hle" : "apploader";
            console.warn(
              `[lazuli] ⚠ Entry point ${hexU32(entryPc)} is in ${region} range, not game RAM.\n` +
              `         This means the apploader's close() returned a wrong address.\n` +
              `         Check the "  Close: 0x..." line in the apploader log above;\n` +
              `         compare all DI DVD Read log lines with the native disc-read sequence.`
            );
          }
          const r3Exit = hexU32(emu.get_gpr(3));
          appendApploaderLog(
            `[debug] ipl-hle exit: PC=${hexU32(blockPc)} LR=${lrVal} CTR=${ctrVal}`
          );
          appendApploaderLog(`[debug] r3: ${r3Exit}  (at ipl-hle exit — compare with native)`);
        }

        prevPhase = blockPhase;
      }
    }

    // ── Breakpoint check ────────────────────────────────────────────────────
    // Pause the emulator before executing this block if a breakpoint is set at
    // the current PC.  stopLoop() cancels the animation-frame loop; the user
    // can inspect CPU state and resume with the Start button or step manually.
    if (breakpoints.has(blockPc >>> 0)) {
      const bpHex = hexU32(blockPc);
      console.info(`[lazuli] breakpoint hit at ${bpHex}`);
      pushDebugEvent(`⬤ BREAKPOINT hit at ${bpHex}`);
      setStatus(`⬤ Breakpoint hit at ${bpHex} — emulation paused`, "status-info");
      updateStats(emu);
      renderRegisters(emu);
      renderFprRegisters(emu);
      stopLoop();
      return;
    }

    // Snapshot EE *before* the block so we can detect a 0→1 transition
    // caused by rfi or mtmsr inside the block.
    prevBlockEe = getMsrEe(emu);

    if (!executeOneBlockSync(emu, ram, null)) {
      loopError = true;
      break;
    }
    blocksThisFrame++;
    // Refresh the RAM view after each block in case a DVD DMA triggered
    // WASM linear-memory growth, detaching the previous Uint8Array buffer.
    ram = getRamView(emu);

    // ── EE edge-detection: deliver pending PI external interrupts ─────────────
    // The native JIT fires pi::check_interrupts only on an MSR-change event
    // (via the `msr_changed` hook triggered by `rfi` / `mtmsr`), NOT on every
    // block.  Mirror that here: if the block raised EE from 0 to 1 (e.g. via
    // `rfi` restoring the pre-exception MSR), call maybe_deliver_external_interrupt
    // exactly once.  This prevents the infinite interrupt re-delivery loop
    // that occurred when PI_INTSR VI bit remained set across the `rfi`
    // that ends the OS interrupt handler.
    const newEe = getMsrEe(emu);
    if (newEe && !prevBlockEe) {
      emu.maybe_deliver_external_interrupt();
    }

    // Advance the decrementer after each block using the block's actual CPU
    // cycle count rather than a fixed per-block constant.  This mirrors the
    // native emulator's Scheduler, which is fed by Block::meta().cycles from
    // ppcjit after every block execution.
    //
    // The Gekko timebase (and decrementer) decrements at CPU/12 ≈ 40.5 MHz.
    // blockMeta.cycles is in CPU cycles, so the number of timebase/DEC ticks
    // consumed by this block is floor(cycles / 12).  We clamp to at least 1
    // so a zero-cycle block (should not happen) still makes forward progress.
    //
    // advance_decrementer only handles the DEC edge-trigger; PI external
    // interrupts are delivered separately via the EE edge-detection above.
    const blockMeta   = blockMetaMap.get(blockPc);
    const blockCycles = blockMeta ? blockMeta.cycles : TICKS_PER_BLOCK_FALLBACK * 12;
    emu.add_cpu_cycles(blockCycles);
    emu.advance_decrementer(Math.max(1, Math.floor(blockCycles / 12)));

    // AI sample counter: advance proportional to emulated CPU cycles.
    // Gekko runs at 486 MHz; the default sample rate is 48 kHz
    // (10 125 CPU cycles per sample: 486_000_000 / 48_000 = 10_125).
    // advance_ai() accumulates fractional samples across blocks so no
    // samples are lost, mirrors the native ai::push_streaming_frame
    // scheduler event, and asserts PI_INT_AI automatically when AISCNT
    // crosses AIIT.
    emu.advance_ai(blockCycles);

    // Stuck-PC detection: track how many consecutive blocks leave the PC
    // unchanged.  This catches both "branch to self" tight loops and the
    // raise_exception path where WASM returns 0 without advancing the PC.
    const newBlockPc = emu.get_pc();
    if (newBlockPc === blockPc) {
      stuckConsecutiveRuns++;
      if (stuckConsecutiveRuns === STUCK_PC_THRESHOLD) {
        const stuckHex = "0x" + blockPc.toString(16).toUpperCase().padStart(8, "0");
        const stuckPhase = classifyPc(blockPc);
        const excInfo  = lastRaisedExceptionPc === blockPc
          ? `exception(${lastRaisedExceptionKind}) loop`
          : "branch-to-self or nextPc=0";
        const msg =
          `PC stuck at ${stuckHex} [${stuckPhase}] for ${STUCK_PC_THRESHOLD} consecutive blocks — ${excInfo}` +
          ` (exceptions raised: ${emu.raise_exception_count()}, compiled: ${emu.blocks_compiled()})`;
        console.warn(`[lazuli] ${msg}`);
        pushDebugEvent(`⚠ STUCK ${stuckHex} [${stuckPhase}] — ${excInfo}`);
        setStatus(`⚠ PC stuck at ${stuckHex} [${stuckPhase}] — ${excInfo}`, "status-info");

        // ── Full CPU state dump on first stuck detection ──────────────────
        const lrVal   = emu.get_lr();
        const ctrVal  = emu.get_ctr();
        const msrVal  = emu.get_msr();
        const srr0Val = emu.get_srr0();
        const srr1Val = emu.get_srr1();
        const decVal  = emu.get_dec();
        const r1Val   = emu.get_gpr(1);
        // nextPc=0 → dynamic branch (blr/bctr/rfi) resolved to address 0, or an
        //            exception was raised; either way CPU::pc is written by the block.
        // nextPc=stuckHex → block returns own start address (branch-to-self).
        const nextPcNote = lastNextPc === 0
          ? `nextPc=0 (branch target=0 or exception — CPU::pc written; emu.get_pc()=${stuckHex})`
          : `nextPc=${hexU32(lastNextPc)} (block returned this target — branch-to-self or loop)`;
        const eeEnabled = (msrVal >> 15) & 1;
        console.warn(
          `[lazuli] STUCK CPU dump @ ${stuckHex}:\n` +
          `  ${nextPcNote}\n` +
          `  LR   = ${hexU32(lrVal)}   ← if equal to stuck PC, blr loops to itself\n` +
          `  CTR  = ${hexU32(ctrVal)}\n` +
          `  R1   = ${hexU32(r1Val)}  (stack pointer)\n` +
          `  MSR  = ${hexU32(msrVal)}  (EE/interrupts bit15=${eeEnabled})\n` +
          `  SRR0 = ${hexU32(srr0Val)}  (PC saved at last exception)\n` +
          `  SRR1 = ${hexU32(srr1Val)}  (MSR saved at last exception)\n` +
          `  DEC  = ${hexU32(decVal)} / ${(decVal | 0)}  (decrementer — negative means expired)\n` +
          `  exceptions raised so far: ${emu.raise_exception_count()}\n` +
          `  blocks compiled: ${emu.blocks_compiled()}, executed: ${emu.blocks_executed()}`,
        );
        pushDebugEvent(
          `⚠ CPU dump: LR=${hexU32(lrVal)} CTR=${hexU32(ctrVal)} MSR=${hexU32(msrVal)} ` +
          `SRR0=${hexU32(srr0Val)} DEC=${decVal | 0} EE=${eeEnabled}`,
        );

        // ── Enriched context: phase, recent MMIO, and milestone status ────
        const now = performance.now();
        const fmtMs = (ts) => ts !== null
          ? `✓ (${Math.trunc(now - ts)} ms ago)`
          : "✗ not reached";
        const mmioLines = recentMmioAccesses.length === 0
          ? "    (no recent MMIO accesses)"
          : recentMmioAccesses.map(e =>
              `    ${e.dir}${e.size} ${e.subsystem} ${hexU32(e.addr)} = ${hexU32(e.val)}`
            ).join("\n");
        console.warn(
          `[lazuli] STUCK context @ ${stuckHex}:\n` +
          `  Phase: ${stuckPhase}\n` +
          `  Last MMIO accesses (oldest→newest):\n${mmioLines}\n` +
          `  Boot milestones:\n` +
          `    iplHleStarted:    ${fmtMs(milestones.iplHleStarted)}\n` +
          `    apploaderRunning: ${fmtMs(milestones.apploaderRunning)}\n` +
          `    apploaderDone:    ${fmtMs(milestones.apploaderDone)}\n` +
          `    gameEntry:        ${fmtMs(milestones.gameEntry)}\n` +
          `    firstXfbContent:  ${fmtMs(milestones.firstXfbContent)}\n` +
          `  → divergence is likely AFTER the last ✓ milestone above`
        );
      }

      // Periodic re-dump every STUCK_PC_THRESHOLD * STUCK_REDUMP_MULTIPLIER runs
      // while stuck (10× less frequent than the initial dump), so we can tell
      // whether any state is changing as execution continues.
      if (stuckConsecutiveRuns > STUCK_PC_THRESHOLD &&
          (stuckConsecutiveRuns % (STUCK_PC_THRESHOLD * STUCK_REDUMP_MULTIPLIER)) === 0) {
        const stuckHex = "0x" + blockPc.toString(16).toUpperCase().padStart(8, "0");
        const lrVal  = emu.get_lr();
        const msrVal = emu.get_msr();
        const decVal = emu.get_dec();
        const eeEnabled = (msrVal >> 15) & 1;
        console.info(
          `[lazuli] STILL STUCK @ ${stuckHex} [${classifyPc(blockPc)}] (run #${stuckConsecutiveRuns}): ` +
          `nextPc=${hexU32(lastNextPc)} LR=${hexU32(lrVal)} MSR=${hexU32(msrVal)} (EE=${eeEnabled}) ` +
          `DEC=${decVal | 0} exceptions=${emu.raise_exception_count()}`,
        );

        // Safety net: if the CPU is still stuck after many blocks inside the
        // PowerPC exception-vector area (0x00000000–0x00001FFF), a stub is
        // missing or broken.  Halt rather than spinning forever.
        if (stuckConsecutiveRuns >= STUCK_PC_THRESHOLD * STUCK_EXCEPTION_VECTOR_MULTIPLIER
            && blockPc < 0x00002000) {
          console.error(
            `[lazuli] CPU permanently stuck at exception vector ${stuckHex} ` +
            `(${stuckConsecutiveRuns} consecutive blocks) — no handler installed; halting.`,
          );
          pushDebugEvent(`✗ stuck at exception vector ${stuckHex} — halted`);
          setStatus(
            `✗ Stuck at exception vector ${stuckHex} — no handler installed (see console)`,
            "status-err",
          );
          loopError = true;
          break;
        }
      }
    } else {
      if (stuckConsecutiveRuns >= STUCK_PC_THRESHOLD) {
        const newHex = "0x" + newBlockPc.toString(16).toUpperCase().padStart(8, "0");
        const unstuckPhase = classifyPc(newBlockPc);
        console.info(`[lazuli] PC unstuck → ${newHex} [${unstuckPhase}] after ${stuckConsecutiveRuns} same-PC blocks`);
        pushDebugEvent(`✓ unstuck → ${newHex} [${unstuckPhase}] (${stuckConsecutiveRuns} blocks)`);
        setStatus("▶ Emulation running…", "status-ok");
      }
      stuckConsecutiveRuns = 0;
    }
  }
  if (loopError) {
    const errPc  = emu.get_pc();
    const errHex = "0x" + errPc.toString(16).toUpperCase().padStart(8, "0");
    const errMsg =
      `[lazuli] gameLoop: execution stopped after ${blocksThisFrame} blocks ` +
      `(total compiled: ${emu.blocks_compiled()}, executed: ${emu.blocks_executed()}) ` +
      `— PC is now ${errHex}`;
    console.error(errMsg);
    pushDebugEvent(`✗ loop stopped at ${errHex} (${blocksThisFrame} blocks this frame)`);
    stopLoop();
    setStatus(`✗ Stopped at ${errHex} — see console / debug log`, "status-err");
    updateStats(emu);
    renderRegisters(emu);
    renderFprRegisters(emu);
    return;
  }

  // Render XFB to canvas
  renderXfb(ram, ctx, emu, gameTitle);

  // Milestone: first frame with non-zero XFB pixel data.
  // renderXfb() sets xfbHasContent=true the first time it finds a non-zero
  // pixel in any candidate XFB region.  We latch that into milestones here
  // so the elapsed time is correct (checked once per animation frame).
  if (xfbHasContent && milestones.firstXfbContent === null) {
    milestones.firstXfbContent = performance.now();
    const elapsed = milestones.startedAt !== null
      ? Math.trunc(milestones.firstXfbContent - milestones.startedAt) + " ms"
      : "?";
    console.info(`[lazuli] ✓ Milestone: first XFB content (first rendered frame) — ${elapsed} since boot`);
    pushDebugEvent(`✓ Milestone: first XFB (${elapsed})`);
  }

  // ── OS-banner post-entry watch ─────────────────────────────────────────────
  // After the game-entry milestone fires, watch for two independent signals:
  //   1. EXI UART output (OSReport active) — logged once via osUartActiveSeen.
  //   2. ArenaLo (physical 0x30) set to a valid RAM address by OSInit.
  // ArenaLo is tracked separately from UART: the first UART bytes arrive a few
  // CPU cycles before OSInit has written the heap bounds, so we keep watching
  // even after UART fires.  After 300 frames (~5 s), emit a timeout note.
  if (milestones.gameEntry !== null && !osInitBannerDone) {
    osPostEntryFrames++;
    const currentArenaLo = readRamU32(ram, 0x30);
    const currentArenaHi = readRamU32(ram, 0x34);
    const newUartBytes   = totalEximUartBytes - uartBytesAtGameEntry;

    // Report UART activity exactly once (bytes may trickle in over many frames).
    if (newUartBytes > 0 && !osUartActiveSeen) {
      osUartActiveSeen = true;
      // Flush any partial OSReport line so the content is visible before the
      // activity banner.  OSReport may not always terminate strings with '\n'.
      flushStdoutLineBuffer();
      appendApploaderLog(
        `[OS] EXI UART active — OSReport is functional (${newUartBytes} bytes)`
      );
    }

    // Detect when OSInit writes a valid ArenaLo (>= 0x80000000 = in main RAM).
    if (currentArenaLo >= 0x80000000) {
      flushStdoutLineBuffer();
      appendApploaderLog(`[OS] Arena : ${hexU32(currentArenaLo)} - ${hexU32(currentArenaHi)}`);
      osInitBannerDone = true;
    } else if (osPostEntryFrames >= 300) {
      // 300 frames (~5 s) with no valid ArenaLo from OSInit.
      flushStdoutLineBuffer();
      appendApploaderLog(
        `[OS] Note: ArenaLo still 0 after 300 frames — ` +
        `OSInit may not have run or OSReport may be a NOP in this binary`
      );
      osInitBannerDone = true;
    }
  }

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
    renderFprRegisters(emu);
  }

  animFrameId = requestAnimationFrame((ts) => gameLoop(emu, canvas, ctx, ts));
}

function startLoop(emu, canvas, ctx) {
  if (running) return;
  running     = true;
  frameCount  = 0;
  lastFpsTime = performance.now();
  // Record the wall-clock start time for milestone elapsed calculations.
  // Only set once (not on Resume after pause) so all milestone timestamps
  // are relative to the very beginning of this ISO session.
  if (milestones.startedAt === null) {
    milestones.startedAt = performance.now();
  }
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
    words: ["38600000", "38630001", "2C03000A", "4082FFF8"],
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

  // ipl-hle DOL bytes fetched from the server at startup.  null when the file
  // is not available (built from crates/ipl-hle/ via `just ipl-hle build` and
  // copied to this directory by the `web-build` justfile recipe).
  let iplHleDol = null;

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

  // Fetch the ipl-hle DOL (built from crates/ipl-hle/ and served alongside
  // the WASM).  A missing file is non-fatal — ISO boot will fail gracefully.
  try {
    const resp = await fetch("./ipl-hle.dol");
    if (resp.ok) {
      iplHleDol = new Uint8Array(await resp.arrayBuffer());
      console.log(`[lazuli] ipl-hle.dol fetched (${iplHleDol.byteLength} bytes)`);
    } else {
      console.warn(`[lazuli] ipl-hle.dol not found (${resp.status}); ISO boot unavailable`);
      setStatus(
        "⚠ ipl-hle.dol not found — run `just ipl-hle build` then `just web-build` before loading an ISO",
        "status-err"
      );
    }
  } catch (e) {
    console.warn("[lazuli] Could not fetch ipl-hle.dol:", e);
    setStatus(
      "⚠ Could not fetch ipl-hle.dol — ISO boot unavailable",
      "status-err"
    );
  }

  renderRegisters(emu);
  renderFprRegisters(emu);
  updateStats(emu);

  // ── Keyboard controller ────────────────────────────────────────────────────
  document.addEventListener("keydown", (e) => {
    const bit = KEY_MAP[e.key];
    if (bit) {
      e.preventDefault();
      keyboardBits |= bit;
      emu.set_pad_buttons(keyboardBits | gamepadBits);
      setText("stat-pad",
        "0x" + emu.get_pad_buttons().toString(16).toUpperCase().padStart(4, "0"));
    }
  });
  document.addEventListener("keyup", (e) => {
    const bit = KEY_MAP[e.key];
    if (bit) {
      keyboardBits &= ~bit;
      emu.set_pad_buttons(keyboardBits | gamepadBits);
      setText("stat-pad",
        "0x" + emu.get_pad_buttons().toString(16).toUpperCase().padStart(4, "0"));
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
    renderFprRegisters(emu);
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
    renderFprRegisters(emu);
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

        const meta = parseAndLoadIso(evt.target.result, emu, iplHleDol);
        // Store the raw ISO bytes in the Rust DiscImageDevice so the emulated
        // DVD controller can service in-game sector reads (streams, audio,
        // textures) without re-reading from the JS File object.
        emu.load_disc_image(new Uint8Array(evt.target.result));
        gameTitle     = meta.gameName || meta.gameId || "Unknown Game";
        lastEntryPoint = meta.entry;

        $("header-game").textContent = `— ${gameTitle}`;
        $("iso-meta").textContent =
          `Game ID: ${meta.gameId} | Title: ${meta.gameName} | ` +
          `Entry: 0x${meta.entry.toString(16).toUpperCase()}`;

        $("btn-reset").disabled = false;

        drawPlaceholder(ctx, gameTitle, emu);
        renderRegisters(emu);
        renderFprRegisters(emu);
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
    renderFprRegisters(emu);
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
    let ram = getRamView(emu);
    let count = 0;
    for (let i = 0; i < 10; i++) {
      const log = [];
      if (!executeOneBlockSync(emu, ram, log)) {
        for (const l of log) appendExecLog(l);
        break;
      }
      count++;
      // Refresh in case a DMA caused WASM linear-memory growth this block.
      ram = getRamView(emu);
    }
    renderRegisters(emu);
    renderFprRegisters(emu);
    updateStats(emu);
    setStatus(
      count > 0
        ? `✓ Ran ${count} block(s) — PC now 0x${emu.get_pc().toString(16).toUpperCase().padStart(8, "0")}`
        : "✗ Run failed at first block",
      count > 0 ? "status-ok" : "status-err"
    );
  });

  // ── Breakpoint controls ───────────────────────────────────────────────────
  function doAddBreakpoint() {
    const raw      = $("bp-addr-input").value.trim().replace(/^0x/i, "");
    const parsedPc = parseInt(raw, 16);
    if (isNaN(parsedPc)) {
      setStatus("✗ Invalid breakpoint address — enter a hex address (e.g. 80003f00)", "status-err");
      return;
    }
    addBreakpoint(parsedPc);
    setStatus(`✓ Breakpoint added at ${hexU32(parsedPc)}`, "status-ok");
  }

  $("btn-bp-add").addEventListener("click", doAddBreakpoint);

  $("bp-addr-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") doAddBreakpoint();
  });

  $("btn-bp-clear").addEventListener("click", () => {
    clearBreakpoints();
    setStatus("✓ All breakpoints cleared", "status-ok");
  });

  // Initialise the (empty) breakpoint list on page load
  renderBreakpointList();

  // ── Clear debug log button ────────────────────────────────────────────────
  $("btn-clear-debug").addEventListener("click", () => {
    debugEvents = [];
    const el = $("debug-log");
    if (el) el.textContent = "(no events yet)";
  });

  // ── Clear apploader log button ────────────────────────────────────────────
  $("btn-clear-apploader").addEventListener("click", () => {
    clearApploaderLog();
  });

  // Enable step/run buttons now that the emulator is ready
  $("btn-step").disabled  = false;
  $("btn-run10").disabled = false;
}

main();
