//! ROM loading — ISO disc images and raw byte payloads.
//!
//! Supports both raw GameCube ISO images and CISO (Compact ISO) sparse disc
//! images.  CISO files are detected by the `"CISO"` magic header at offset
//! 0 and decompressed to a flat in-memory representation before any further
//! parsing, so all downstream code can treat the disc image as a plain byte
//! slice regardless of the original format.

use disks::binrw::BinRead;
use wasm_bindgen::prelude::*;

use crate::WasmEmulator;

macro_rules! console_log {
    ($($t:tt)*) => {
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!($($t)*)))
    };
}

/// The GameCube ISO disc magic word at byte offset 0x1C.
const GC_ISO_MAGIC: u32 = 0xC233_9F3D;

/// The Wii ISO disc magic word at byte offset 0x1C (the Wii uses the same
/// layout with a different magic, but the emulator only handles GameCube).
const _WII_ISO_MAGIC: u32 = 0x5D1C_9EA3;

/// Canonical guest address at which the GameCube apploader is mapped.
const APPLOADER_LOAD_ADDR: u32 = 0x8120_0000;

/// ISO byte offset of the apploader header (immediately after the boot header).
const APPLOADER_ISO_OFFSET: u64 = 0x2440;

/// Detect the disc image format and return a flat byte vector.
///
/// If `data` starts with the `"CISO"` magic (a Compact ISO / CISO disc
/// image), the sparse blocks are decompressed into a contiguous buffer.
/// Unused blocks (those marked 0 in the CISO block map) are zero-filled.
///
/// If `data` does not look like a CISO, it is assumed to be a raw ISO and is
/// returned as-is (zero-copy via a single allocation).
fn flatten_disc_image(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() >= 4 && &data[0..4] == b"CISO" {
        flatten_ciso(data)
    } else {
        Ok(data.to_vec())
    }
}

/// Decompress a CISO (Compact ISO) sparse disc image into a flat buffer.
///
/// CISO stores a 32-byte header followed by a 32,760-byte block-presence map.
/// Blocks marked present are stored consecutively in the file; missing blocks
/// expand to zero-filled pages.  This function reads every used block via
/// [`disks::cso::Cso::read`] and writes them into a contiguous `Vec<u8>`
/// whose length equals `(last_used_block + 1) * block_size`, matching the
/// equivalent region of a raw ISO.
fn flatten_ciso(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Cursor;

    let cursor = Cursor::new(data);
    let mut cso = disks::cso::Cso::new(cursor).map_err(|e| format!("CISO header parse error: {e}"))?;

    let block_size = cso.header().block_size as usize;
    if block_size == 0 {
        return Err("CISO block size is zero".to_string());
    }

    // Find the highest used block to determine how large the flat image must be.
    let last_used = cso.map().iter().rposition(|b| b.is_some());
    let flat_size = match last_used {
        Some(idx) => (idx + 1) * block_size,
        None => return Err("CISO contains no used blocks".to_string()),
    };

    let mut flat = vec![0u8; flat_size];
    cso.read(0, &mut flat).map_err(|e| format!("CISO read error: {e}"))?;

    Ok(flat)
}

/// Read a big-endian u32 from `buf` at `offset`, returning 0 on out-of-bounds.
fn read_u32be(buf: &[u8], offset: usize) -> u32 {
    if offset + 4 > buf.len() {
        return 0;
    }
    u32::from_be_bytes(buf[offset..offset + 4].try_into().unwrap())
}

#[wasm_bindgen]
impl WasmEmulator {
    /// Copy `data` into guest RAM starting at `guest_addr`.
    ///
    /// `guest_addr` may be a GameCube virtual address (`0x8xxxxxxx`) or a raw
    /// physical offset; both are handled transparently via the same
    /// `0x01FF_FFFF` mask used by [`crate::phys_addr`].
    ///
    /// Clears the block cache for any PC that overlaps the written region, so
    /// that stale compiled blocks are not executed after a ROM reload.
    pub fn load_bytes(&mut self, guest_addr: u32, data: &[u8]) {
        let start = crate::phys_addr(guest_addr);
        let end = start + data.len();
        if end > self.ram.len() {
            console_log!(
                "[lazuli-web] load_bytes: data range 0x{:08X}..0x{:08X} exceeds RAM size 0x{:08X}",
                start,
                end,
                self.ram.len()
            );
            return;
        }
        self.ram[start..end].copy_from_slice(data);
        // Invalidate compiled blocks in the written range.
        let start_page = (start & !3) as u32;
        let end_page = (end as u32 + 3) & !3;
        for pc in (start_page..end_page).step_by(4) {
            self.cache.invalidate(pc);
        }
        console_log!("[lazuli-web] loaded {} bytes at physical 0x{:08X}", data.len(), start);
    }

    /// Load an ipl-hle DOL into guest RAM and return its entry point.
    ///
    /// `data` must contain the raw bytes of a GameCube DOL file built from
    /// `crates/ipl-hle/` (via `just ipl-hle build`).  In the browser the
    /// caller fetches `ipl-hle.dol` from the same origin and passes the
    /// resulting `Uint8Array` here; nothing is embedded in the WASM binary.
    ///
    /// The DOL header layout (all fields big-endian u32):
    ///   0x000  text_offsets[7]   — file offset of each .text section
    ///   0x01C  data_offsets[11]  — file offset of each .data section
    ///   0x048  text_targets[7]   — guest load address of each .text section
    ///   0x064  data_targets[11]  — guest load address of each .data section
    ///   0x090  text_sizes[7]     — size in bytes of each .text section
    ///   0x0AC  data_sizes[11]    — size in bytes of each .data section
    ///   0x0D8  bss_target        — guest address of BSS region
    ///   0x0DC  bss_size          — size of BSS region
    ///   0x0E0  entry             — entry-point guest address
    ///
    /// After loading, callers must set `gpr[3]` to the real apploader's entry
    /// function address (read from the ISO apploader header at offset `+0x10`)
    /// so that ipl-hle's `main(entry)` receives it as its first argument,
    /// matching what the native `load_ipl_hle()` does.
    ///
    /// Returns the ipl-hle entry point (e.g. `0x81300000`).
    pub fn load_ipl_hle(&mut self, data: &[u8]) -> u32 {
        /// Read a big-endian u32 from the DOL bytes at `offset`.
        fn u32be(dol: &[u8], offset: usize) -> u32 {
            u32::from_be_bytes(dol[offset..offset + 4].try_into().unwrap())
        }

        let text_offsets: [u32; 7]  = core::array::from_fn(|i| u32be(data, 0x000 + i * 4));
        let data_offsets: [u32; 11] = core::array::from_fn(|i| u32be(data, 0x01C + i * 4));
        let text_targets: [u32; 7]  = core::array::from_fn(|i| u32be(data, 0x048 + i * 4));
        let data_targets: [u32; 11] = core::array::from_fn(|i| u32be(data, 0x064 + i * 4));
        let text_sizes:   [u32; 7]  = core::array::from_fn(|i| u32be(data, 0x090 + i * 4));
        let data_sizes:   [u32; 11] = core::array::from_fn(|i| u32be(data, 0x0AC + i * 4));
        let bss_target = u32be(data, 0x0D8);
        let bss_size   = u32be(data, 0x0DC);
        let entry      = u32be(data, 0x0E0);

        console_log!("[lazuli-web] ipl-hle DOL: {} bytes, entry=0x{:08X}, bss=0x{:08X}..+0x{:X}",
            data.len(), entry, bss_target, bss_size);

        for i in 0..7 {
            if text_offsets[i] != 0 && text_sizes[i] != 0 {
                let start = text_offsets[i] as usize;
                let size  = text_sizes[i] as usize;
                console_log!("[lazuli-web] ipl-hle text[{}]: file+0x{:X} → 0x{:08X}, {} bytes",
                    i, start, text_targets[i], size);
                self.load_bytes(text_targets[i], &data[start..start + size]);
            }
        }
        for i in 0..11 {
            if data_offsets[i] != 0 && data_sizes[i] != 0 {
                let start = data_offsets[i] as usize;
                let size  = data_sizes[i] as usize;
                console_log!("[lazuli-web] ipl-hle data[{}]: file+0x{:X} → 0x{:08X}, {} bytes",
                    i, start, data_targets[i], size);
                self.load_bytes(data_targets[i], &data[start..start + size]);
            }
        }
        if bss_size > 0 {
            console_log!("[lazuli-web] ipl-hle BSS: 0x{:08X}..+0x{:X} zeroed", bss_target, bss_size);
            let zeros = vec![0u8; bss_size as usize];
            self.load_bytes(bss_target, &zeros);
        }

        console_log!("[lazuli-web] ipl-hle loaded, entry = 0x{:08X}", entry);
        entry
    }

    /// Store a raw GameCube ISO disc image for runtime sector reads.
    ///
    /// This is the Rust counterpart of Play!'s `DiscImageDevice.ts` +
    /// `Js_DiscImageDeviceStream.cpp`: once the ISO bytes are stored here the
    /// emulated DVD Interface can service `A8h` (DVD Read) DMA commands during
    /// gameplay, so that games that stream textures, audio, or level data from
    /// disc continue to work after the initial boot DOL has been loaded.
    ///
    /// Both raw ISO and CISO (Compact ISO) formats are accepted.  CISO images
    /// are detected by the `"CISO"` magic at byte 0 and decompressed to a flat
    /// buffer before storage so the runtime DVD-read path can use plain byte
    /// slicing without format-awareness.
    ///
    /// Call this in addition to (not instead of) the DOL-loading path in
    /// `parseAndLoadIso` / `load_bytes`.
    pub fn load_disc_image(&mut self, data: &[u8]) {
        let flat = match flatten_disc_image(data) {
            Ok(f) => f,
            Err(e) => {
                console_log!("[lazuli] DiscImageDevice: format error — {e}; storing raw bytes");
                data.to_vec()
            }
        };
        let disc_size_mib = flat.len() / (1024 * 1024);
        let is_ciso = data.len() >= 4 && &data[0..4] == b"CISO";
        self.disc = Some(flat);
        // Mark the cover as closed (bit 2 = DICVR_STATE = 1 → cover closed,
        // disc present) so games that poll DICOVER before issuing commands see
        // a valid disc in the drive.
        self.di.cover = 0x4;
        if is_ciso {
            console_log!(
                "[lazuli] DiscImageDevice: CISO decompressed to {} MiB for runtime reads",
                disc_size_mib
            );
        } else {
            console_log!(
                "[lazuli] DiscImageDevice: stored {} MiB disc image for runtime reads",
                disc_size_mib
            );
        }
    }

    /// Parse a GameCube disc image (raw ISO or CISO), load the apploader into
    /// guest RAM, and store the disc for runtime DVD reads.
    ///
    /// This method consolidates the disc-loading logic that was previously
    /// split between the JavaScript `parseAndLoadIso` function and the
    /// [`load_disc_image`] method, moving all format-aware parsing into Rust:
    ///
    /// 1. **Format detection** — CISO images are decompressed to a flat buffer.
    /// 2. **Header validation** — the GameCube magic word (`0xC2339F3D` at
    ///    offset `0x1C`) is verified.
    /// 3. **Disc header** — the first `0x440` bytes (ISO header + boot info)
    ///    are copied to guest RAM at `0x8000_0000`.
    /// 4. **Dolphin OS globals** — standard boot-time values are written to
    ///    `0x8000_0020`–`0x8000_00FF`, matching what the native IPL ROM writes
    ///    before transferring control to the apploader.
    /// 5. **Apploader** — the apploader body is loaded at `0x8120_0000`.
    /// 6. **Boot DOL** — the header is parsed to read the entry point; sections
    ///    are **not** pre-loaded and BSS is **not** zeroed here.  The apploader
    ///    (run by ipl-hle) loads every section via DI DMA, making pre-loading
    ///    redundant.  Pre-zeroing BSS would also wipe the OS globals written in
    ///    step 4 (e.g. arena_lo at `0x8000_0030`) if the game's BSS covers that
    ///    region, causing a false "ArenaLo still 0" diagnostic.
    /// 7. **Runtime disc** — the flat disc image is stored for later `0xA8`
    ///    DVD Read DMA commands.
    ///
    /// Returns a JavaScript object with the following fields on success:
    ///
    /// | Field              | Type   | Description                              |
    /// |--------------------|--------|------------------------------------------|
    /// | `gameName`         | string | Null-terminated game title from the header |
    /// | `gameId`           | string | 6-character game identifier               |
    /// | `dolEntry`         | number | Boot DOL entry-point address              |
    /// | `apploaderEntry`   | number | Apploader entry function address          |
    ///
    /// Returns a JavaScript `Error` on failure (bad magic, corrupt DOL, …).
    pub fn parse_and_load_disc(&mut self, data: &[u8]) -> Result<JsValue, JsValue> {
        use std::io::{Cursor, Seek, SeekFrom};

        // ── 1. Format detection ───────────────────────────────────────────────
        let flat = flatten_disc_image(data).map_err(|e| JsValue::from_str(&e))?;
        let flat_bytes = flat.as_slice();

        // ── 2. Magic validation ────────────────────────────────────────────────
        let magic = read_u32be(flat_bytes, 0x1C);
        if magic != GC_ISO_MAGIC {
            return Err(JsValue::from_str(&format!(
                "Not a valid GameCube ISO — magic mismatch (got 0x{magic:08X}, expected 0x{GC_ISO_MAGIC:08X})"
            )));
        }

        // ── 3. Parse ISO header ───────────────────────────────────────────────
        let mut cursor = Cursor::new(flat_bytes);
        let iso_header = disks::iso::Header::read(&mut cursor)
            .map_err(|e| JsValue::from_str(&format!("ISO header parse error: {e}")))?;

        // Game name: null-terminated ASCII string in the Meta field.
        let game_name = iso_header
            .meta
            .game_name
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as char)
            .collect::<String>();

        // Game ID: 6-byte printable ASCII run at offset 0x000.
        let game_id = flat_bytes[0..6.min(flat_bytes.len())]
            .iter()
            .filter(|&&b| b >= 0x20 && b <= 0x7E)
            .map(|&b| b as char)
            .collect::<String>();

        console_log!(
            "[lazuli] parse_and_load_disc: \"{}\" ({}) disc_size={} MiB",
            game_name,
            game_id,
            flat.len() / (1024 * 1024),
        );

        // ── 4. Load disc header block into guest RAM at 0x8000_0000 ───────────
        let header_bytes = &flat_bytes[..0x440.min(flat_bytes.len())];
        self.load_bytes(0x8000_0000, header_bytes);

        // ── 5. Dolphin OS globals ─────────────────────────────────────────────
        // Boot-time values that the GameCube IPL ROM writes before handing
        // control to the apploader.  Mirrors the JS version in cpu-worker.js.
        let mut globals = [0u8; 0xE0];
        let set = |buf: &mut [u8; 0xE0], off: usize, val: u32| {
            buf[off..off + 4].copy_from_slice(&val.to_be_bytes());
        };
        set(&mut globals, 0x00, 0x0D15_EA5E); // BI2 magic
        set(&mut globals, 0x04, 0x0000_0001); // BI2 version
        set(&mut globals, 0x08, 0x0180_0000); // physical memory size (24 MiB)
        set(&mut globals, 0x0C, 0x0000_0003); // console type (retail)
        set(&mut globals, 0x10, 0x8042_E260); // arena_lo
        set(&mut globals, 0x14, 0x817F_E8C0); // arena_hi
        set(&mut globals, 0x18, 0x817F_E8C0); // FSSInit
        set(&mut globals, 0x1C, 0x0000_0024); // unknown
        set(&mut globals, 0xAC, 0x0000_0000); // unknown (at 0x800000CC)
        set(&mut globals, 0xB0, 0x0100_0000); // unknown (at 0x800000D0)
        set(&mut globals, 0xD8, 0x09A7_EC80); // bus clock rate
        set(&mut globals, 0xDC, 0x1CF7_C580); // CPU clock rate
        self.load_bytes(0x8000_0020, &globals);

        // ── 6. Apploader ──────────────────────────────────────────────────────
        cursor
            .seek(SeekFrom::Start(APPLOADER_ISO_OFFSET))
            .map_err(|e| JsValue::from_str(&format!("seek to apploader: {e}")))?;
        let apploader = disks::apploader::Header::read(&mut cursor)
            .map_err(|e| JsValue::from_str(&format!("apploader header parse error: {e}")))?;

        let apploader_entry = apploader.entrypoint;
        let apploader_size = apploader.size as usize;
        let apploader_body_offset = (APPLOADER_ISO_OFFSET as usize) + 0x20;

        if apploader_size > 0 {
            let body_end = apploader_body_offset.saturating_add(apploader_size);
            if body_end <= flat_bytes.len() {
                let body = &flat_bytes[apploader_body_offset..body_end];
                self.load_bytes(APPLOADER_LOAD_ADDR, body);
                console_log!(
                    "[lazuli] parse_and_load_disc: apploader 0x{:X} bytes @ 0x{:08X}, entry=0x{:08X}",
                    apploader_size,
                    APPLOADER_LOAD_ADDR,
                    apploader_entry,
                );
            } else {
                console_log!("[lazuli] parse_and_load_disc: apploader body out of bounds, skipping");
            }
        }

        // ── 7. Boot DOL (header only) ─────────────────────────────────────────
        // Parse the DOL header to obtain the entry point for the return value,
        // but do NOT pre-load text/data sections or zero BSS into guest RAM.
        //
        // The apploader (executed by ipl-hle) loads every DOL section into RAM
        // via DI DMA during the boot sequence, so pre-loading them here would
        // be redundant and inconsistent with native IPL behaviour.  More
        // importantly, zeroing BSS at this point would wipe the Dolphin OS
        // globals written in step 5 above (e.g. arena_lo at physical 0x30 /
        // virtual 0x8000_0030) if the game's BSS region covers that area,
        // causing the post-entry ArenaLo watch to report a false timeout.
        // The game's CRT startup code zeroes BSS before OSInit runs anyway.
        cursor
            .seek(SeekFrom::Start(iso_header.bootfile_offset as u64))
            .map_err(|e| JsValue::from_str(&format!("seek to boot DOL: {e}")))?;
        let dol = disks::dol::Dol::read(&mut cursor)
            .map_err(|e| JsValue::from_str(&format!("boot DOL parse error: {e}")))?;

        let dol_entry = dol.entrypoint();

        console_log!(
            "[lazuli] parse_and_load_disc: boot DOL — entry=0x{:08X}, bss=0x{:08X}+0x{:X} (sections not pre-loaded)",
            dol_entry,
            dol.header.bss_target,
            dol.header.bss_size,
        );

        // ── 8. Store disc for runtime DVD reads ───────────────────────────────
        let flat_size_mib = flat.len() / (1024 * 1024);
        self.disc = Some(flat);
        self.di.cover = 0x4; // cover closed = disc present
        console_log!("[lazuli] parse_and_load_disc: {} MiB disc image stored", flat_size_mib);

        // ── Return metadata ───────────────────────────────────────────────────
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"gameName".into(), &JsValue::from_str(&game_name))
            .map_err(|e| JsValue::from_str(&format!("Reflect.set gameName: {e:?}")))?;
        js_sys::Reflect::set(&obj, &"gameId".into(), &JsValue::from_str(&game_id))
            .map_err(|e| JsValue::from_str(&format!("Reflect.set gameId: {e:?}")))?;
        js_sys::Reflect::set(&obj, &"dolEntry".into(), &JsValue::from(dol_entry))
            .map_err(|e| JsValue::from_str(&format!("Reflect.set dolEntry: {e:?}")))?;
        js_sys::Reflect::set(&obj, &"apploaderEntry".into(), &JsValue::from(apploader_entry))
            .map_err(|e| JsValue::from_str(&format!("Reflect.set apploaderEntry: {e:?}")))?;

        Ok(obj.into())
    }
}
