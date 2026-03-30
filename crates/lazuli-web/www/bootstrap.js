/**
 * bootstrap.js — Lazuli web frontend glue
 *
 * This script:
 *  1. Loads the Lazuli WASM module produced by `wasm-pack build`.
 *  2. Creates a `WasmEmulator` instance.
 *  3. Wires up the UI so the user can:
 *     - Paste PowerPC hex words and see the compiled WASM bytecode.
 *     - Load a raw binary file into guest RAM.
 *     - Step through execution one compiled block at a time.
 *
 * ## How to build and serve
 *
 *   # Install wasm-pack (once):
 *   curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
 *
 *   # Build the WASM package (from workspace root):
 *   cd crates/lazuli-web && wasm-pack build --target web --out-dir www/pkg
 *
 *   # Serve on localhost:8080:
 *   cd crates/lazuli-web/www && python3 -m http.server 8080
 *
 * Then open http://localhost:8080 in your browser.
 * Alternatively run `just web-serve` from the workspace root.
 */

// ── Load the wasm-bindgen generated module ─────────────────────────────────
import init, { WasmEmulator } from "./pkg/lazuli_web.js";

// ── DOM helpers ────────────────────────────────────────────────────────────
const $ = (id) => document.getElementById(id);

function setStatus(msg, cls = "status-info") {
  const el = $("status-bar");
  el.textContent = msg;
  el.className = cls;
}

function updateStats(emu) {
  $("stat-compiled").textContent = emu.blocks_compiled();
  $("stat-executed").textContent = emu.blocks_executed();
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

// ── Block execution via dynarec pipeline ──────────────────────────────────
//
// To execute a compiled block we need to:
//  1. Compile the block at the current PC → raw WASM bytes.
//  2. Copy the emulator's serialised CPU struct into a fresh WebAssembly.Memory.
//  3. Instantiate the block with:
//       • env.memory  — the memory holding the CPU registers.
//       • hooks.*     — closures that read/write the emulator's guest RAM.
//  4. Call instance.exports.execute(regs_ptr=0) → returns the next PC.
//  5. Copy the (potentially modified) CPU bytes back into the Rust emulator.
//
// The hook closures hold a Uint8Array over a *snapshot* of the guest RAM
// taken before execution.  Writes from the block are buffered in that
// snapshot and flushed back to the Rust emulator afterwards via sync_ram().
//
const WASM_PAGE_SIZE = 65536;

async function executeOneBlock(emu, execLog) {
  const pc = emu.get_pc();
  const pcHex = "0x" + pc.toString(16).toUpperCase().padStart(8, "0");

  // Step 1 – compile
  let wasmBytes;
  try {
    wasmBytes = emu.compile_block(pc);
  } catch (e) {
    execLog.push(`[PC ${pcHex}] compile error: ${e}`);
    return false;
  }

  // Step 2 – allocate a WASM memory large enough for the CPU struct
  const cpuSize   = emu.cpu_struct_size();
  const pagesNeeded = Math.ceil(cpuSize / WASM_PAGE_SIZE);
  const regsMemory = new WebAssembly.Memory({ initial: pagesNeeded });
  const regsView   = new Uint8Array(regsMemory.buffer);

  // Write the current CPU register state into the memory
  const cpuBytes = emu.get_cpu_bytes();
  regsView.set(cpuBytes, 0);

  // Step 3 – snapshot guest RAM and build hook closures
  const ramSnap = emu.get_ram_copy(); // Uint8Array copy

  const hooks = {
    read_u8:  (addr) => {
      addr = addr >>> 0; // treat as unsigned
      return addr < ramSnap.length ? ramSnap[addr] : 0;
    },
    read_u16: (addr) => {
      addr = addr >>> 0;
      if (addr + 1 >= ramSnap.length) return 0;
      return (ramSnap[addr] << 8) | ramSnap[addr + 1];
    },
    read_u32: (addr) => {
      addr = addr >>> 0;
      if (addr + 3 >= ramSnap.length) return 0;
      return ((ramSnap[addr] << 24) | (ramSnap[addr + 1] << 16) |
              (ramSnap[addr + 2] << 8)  |  ramSnap[addr + 3]) >>> 0;
    },
    write_u8: (addr, val) => {
      addr = addr >>> 0;
      if (addr < ramSnap.length) ramSnap[addr] = val & 0xff;
    },
    write_u16: (addr, val) => {
      addr = addr >>> 0;
      if (addr + 1 < ramSnap.length) {
        ramSnap[addr]     = (val >> 8) & 0xff;
        ramSnap[addr + 1] = val & 0xff;
      }
    },
    write_u32: (addr, val) => {
      addr = addr >>> 0; val = val >>> 0;
      if (addr + 3 < ramSnap.length) {
        ramSnap[addr]     = (val >>> 24) & 0xff;
        ramSnap[addr + 1] = (val >>> 16) & 0xff;
        ramSnap[addr + 2] = (val >>>  8) & 0xff;
        ramSnap[addr + 3] =  val         & 0xff;
      }
    },
    raise_exception: (kind) => {
      execLog.push(`[PC ${pcHex}] exception raised: kind=${kind}`);
    },
  };

  // Step 4 – instantiate and execute
  let nextPc;
  try {
    const { instance } = await WebAssembly.instantiate(wasmBytes, {
      env: { memory: regsMemory },
      hooks,
    });
    nextPc = instance.exports.execute(0); // regs_ptr = 0
  } catch (e) {
    execLog.push(`[PC ${pcHex}] instantiation/execution error: ${e}`);
    return false;
  }

  // Step 5 – sync CPU state and RAM back to the Rust emulator
  const updatedCpuBytes = new Uint8Array(regsMemory.buffer, 0, cpuSize);
  emu.set_cpu_bytes(updatedCpuBytes);
  emu.sync_ram(ramSnap);
  emu.record_block_executed();

  // Determine the next PC: 0 means the block updated Cpu::pc itself
  const newPc = (nextPc >>> 0) !== 0 ? (nextPc >>> 0) : emu.get_pc();
  emu.set_pc(newPc);

  const newPcHex = "0x" + newPc.toString(16).toUpperCase().padStart(8, "0");
  execLog.push(
    `[PC ${pcHex}] executed ${wasmBytes.length} byte block → next PC ${newPcHex}`
  );
  return true;
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

// ── Execution log helpers ──────────────────────────────────────────────────
const MAX_LOG_LINES = 200;
let execLogLines = [];

function appendExecLog(line) {
  execLogLines.push(line);
  if (execLogLines.length > MAX_LOG_LINES) {
    execLogLines = execLogLines.slice(-MAX_LOG_LINES);
  }
  const el = $("exec-log");
  el.textContent = execLogLines.join("\n");
  el.scrollTop = el.scrollHeight;
}

// ── Main ───────────────────────────────────────────────────────────────────
async function main() {
  setStatus("Loading Lazuli WASM module…", "status-info");

  let emu;
  try {
    await init();
    // 4 MiB of guest RAM (enough for most demo programs and small ROM blobs)
    emu = new WasmEmulator(4 * 1024 * 1024);
    emu.set_pc(0x80000000);
    setStatus("✓ WASM module loaded — ready", "status-ok");
    $("btn-compile").disabled  = false;
    $("btn-load-rom").disabled = false;
    $("btn-step").disabled     = false;
    $("btn-run10").disabled    = false;
  } catch (e) {
    setStatus(`✗ Failed to load WASM module: ${e}`, "status-err");
    console.error(e);
    return;
  }

  renderRegisters(emu);
  updateStats(emu);

  // ── ROM file loader ────────────────────────────────────────────────────
  $("btn-load-rom").addEventListener("click", () => {
    const file = $("rom-file").files[0];
    if (!file) {
      setStatus("✗ No file selected", "status-err");
      return;
    }

    const addrHex  = $("rom-addr").value.trim();
    const guestAddr = parseInt(addrHex, 16) || 0x80000000;

    const reader = new FileReader();
    reader.onload = (evt) => {
      const data = new Uint8Array(evt.target.result);

      // The emulator's RAM starts at address 0 and is 4 MiB.
      // We map the guest address modulo RAM size for the demo.
      const ramOffset = guestAddr % (4 * 1024 * 1024);
      emu.load_bytes(ramOffset, data);

      // Set the PC to the load address (or override)
      const pcHex = $("rom-pc").value.trim();
      const newPc = pcHex ? parseInt(pcHex, 16) : guestAddr;
      emu.set_pc(newPc % (4 * 1024 * 1024));

      renderRegisters(emu);
      updateStats(emu);
      setStatus(
        `✓ Loaded ${data.length} bytes from "${file.name}" at RAM offset 0x` +
        ramOffset.toString(16).toUpperCase() +
        ` — PC set to 0x` + (newPc % (4 * 1024 * 1024)).toString(16).toUpperCase(),
        "status-ok"
      );
    };
    reader.onerror = () => setStatus("✗ Failed to read file", "status-err");
    reader.readAsArrayBuffer(file);
  });

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
      await WebAssembly.compile(wasmBytes);
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

  // ── Step button (execute one block) ───────────────────────────────────
  $("btn-step").addEventListener("click", async () => {
    const log = [];
    const ok  = await executeOneBlock(emu, log);
    for (const line of log) appendExecLog(line);
    renderRegisters(emu);
    updateStats(emu);
    if (ok) {
      setStatus(
        `✓ Stepped — PC now 0x${emu.get_pc().toString(16).toUpperCase().padStart(8, "0")}`,
        "status-ok"
      );
    } else {
      setStatus("✗ Step failed — see execution log", "status-err");
    }
  });

  // ── Run 10 blocks button ───────────────────────────────────────────────
  $("btn-run10").addEventListener("click", async () => {
    let count = 0;
    for (let i = 0; i < 10; i++) {
      const log = [];
      const ok  = await executeOneBlock(emu, log);
      for (const line of log) appendExecLog(line);
      if (!ok) break;
      count++;
    }
    renderRegisters(emu);
    updateStats(emu);
    setStatus(
      count > 0
        ? `✓ Ran ${count} block(s) — PC now 0x${emu.get_pc().toString(16).toUpperCase().padStart(8, "0")}`
        : "✗ Run failed at first block — see execution log",
      count > 0 ? "status-ok" : "status-err"
    );
  });
}

main();

