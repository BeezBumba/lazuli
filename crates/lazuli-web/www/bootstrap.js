/**
 * bootstrap.js — Lazuli web frontend glue
 *
 * This script:
 *  1. Loads the Lazuli WASM module produced by `wasm-pack build`.
 *  2. Creates a `WasmEmulator` instance.
 *  3. Wires up the UI so the user can enter PowerPC hex words and see the
 *     compiled WebAssembly bytecode — demonstrating the dynarec-to-WASM
 *     pipeline at work in the browser.
 */

// ── Load the wasm-bindgen generated module ─────────────────────────────────
import init, { WasmEmulator } from "./lazuli_web.js";

// ── DOM helpers ────────────────────────────────────────────────────────────
const $ = (id) => document.getElementById(id);

function setStatus(msg, cls = "status-info") {
  const el = $("status-bar");
  el.textContent = msg;
  el.className = cls;
}

function updateStats(emu) {
  $("stat-compiled").textContent = emu.blocks_compiled();
  $("stat-cache").textContent    = emu.cache_size();
}

function renderRegisters(emu) {
  const grid = $("reg-grid");
  grid.innerHTML = "";

  // GPRs r0..r31
  for (let i = 0; i < 32; i++) {
    const cell = document.createElement("div");
    cell.className = "reg-cell";
    const val = emu.get_gpr(i);
    cell.innerHTML =
      `<span class="reg-name">r${i}&nbsp;</span>` +
      `<span class="reg-val">0x${val.toString(16).padStart(8, "0").toUpperCase()}</span>`;
    grid.appendChild(cell);
  }

  // PC
  const pcCell = document.createElement("div");
  pcCell.className = "reg-cell";
  const pc = emu.get_pc();
  pcCell.innerHTML =
    `<span class="reg-name">PC&nbsp;</span>` +
    `<span class="reg-val">0x${pc.toString(16).padStart(8, "0").toUpperCase()}</span>`;
  grid.appendChild(pcCell);
}

// ── Hex dump + simple WAT-like annotation ─────────────────────────────────
function annotateWasm(bytes) {
  if (bytes.length === 0) return "(empty)";

  const hex = (b) => b.toString(16).padStart(2, "0");
  const lines = [];

  // Magic + version
  lines.push("; WASM binary module");
  lines.push(";");

  // Print first 8 bytes (magic + version)
  if (bytes.length >= 8) {
    const magic   = Array.from(bytes.slice(0, 4)).map(hex).join(" ");
    const version = Array.from(bytes.slice(4, 8)).map(hex).join(" ");
    lines.push(`; magic:   ${magic}  (\\0asm)`);
    lines.push(`; version: ${version}  (1)`);
    lines.push(";");
  }

  // Hex dump, 16 bytes per row
  lines.push(`; Full bytecode (${bytes.length} bytes):`);
  for (let i = 0; i < bytes.length; i += 16) {
    const chunk = bytes.slice(i, i + 16);
    const hexPart = Array.from(chunk).map(hex).join(" ").padEnd(48, " ");
    const asciiPart = Array.from(chunk)
      .map((b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : "."))
      .join("");
    lines.push(`  ${i.toString(16).padStart(4, "0")}  ${hexPart}  ${asciiPart}`);
  }

  return lines.join("\n");
}

// ── Demo programs ──────────────────────────────────────────────────────────
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

// ── Main ───────────────────────────────────────────────────────────────────
async function main() {
  setStatus("Loading Lazuli WASM module…", "status-info");

  let emu;
  try {
    await init();
    // 1 MiB of guest RAM for demos
    emu = new WasmEmulator(1024 * 1024);
    emu.set_pc(0x80000000);
    setStatus("✓ WASM module loaded — ready", "status-ok");
    $("btn-compile").disabled = false;
  } catch (e) {
    setStatus(`✗ Failed to load WASM module: ${e}`, "status-err");
    console.error(e);
    return;
  }

  renderRegisters(emu);
  updateStats(emu);

  // ── Compile button ─────────────────────────────────────────────────────
  $("btn-compile").addEventListener("click", async () => {
    const rawLines = $("asm-input").value.trim().split(/\s+/);
    const pcHex    = $("base-pc").value.trim();
    const basePc   = parseInt(pcHex, 16) || 0x80000000;

    // Convert hex words to a Uint8Array (big-endian)
    const bytes = [];
    for (const line of rawLines) {
      const cleaned = line.replace(/^0x/i, "").replace(/[^0-9a-fA-F]/g, "");
      if (cleaned.length === 0) continue;
      const word = parseInt(cleaned.padStart(8, "0"), 16);
      bytes.push((word >>> 24) & 0xff);
      bytes.push((word >>> 16) & 0xff);
      bytes.push((word >>>  8) & 0xff);
      bytes.push( word         & 0xff);
    }

    if (bytes.length === 0) {
      setStatus("✗ No valid instructions entered", "status-err");
      return;
    }

    // Load the instruction bytes into guest RAM at basePc
    // (RAM starts at 0; map basePc to offset 0 for simplicity in the demo)
    const insBytes = new Uint8Array(bytes);
    emu.load_bytes(0, insBytes);
    emu.set_pc(0);

    setStatus("Compiling…", "status-info");
    try {
      // ── The dynarec-to-WASM step ──
      const wasmBytes = emu.compile_block(0);

      // ── Verify with WebAssembly.compile() ──
      const module = await WebAssembly.compile(wasmBytes);
      const moduleBytes = Array.from(wasmBytes);

      // Display the bytecode
      $("block-output").textContent = annotateWasm(moduleBytes);

      // Update stats
      $("stat-ins").textContent   = rawLines.filter(l => l.trim()).length;
      $("stat-bytes").textContent = moduleBytes.length + " bytes";
      updateStats(emu);

      setStatus(
        `✓ Block compiled to ${moduleBytes.length} WASM bytes ` +
        `and verified with WebAssembly.compile()`,
        "status-ok"
      );
    } catch (e) {
      setStatus(`✗ Compilation failed: ${e}`, "status-err");
      console.error(e);
      $("block-output").textContent = `Error: ${e}`;
    }
  });

  // ── Demo programs ──────────────────────────────────────────────────────
  $("btn-load-demo").addEventListener("click", () => {
    const keys = Object.keys(DEMO_PROGRAMS);
    const key  = keys[Math.floor(Math.random() * keys.length)];
    const prog = DEMO_PROGRAMS[key];
    $("asm-input").value = prog.words.join("\n");
    setStatus(`Loaded demo: "${key}" — ${prog.description}`, "status-info");
  });
}

main();
