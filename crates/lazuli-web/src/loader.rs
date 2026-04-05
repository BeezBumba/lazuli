//! ROM loading — ISO disc images and raw byte payloads.

use wasm_bindgen::prelude::*;

use crate::WasmEmulator;

macro_rules! console_log {
    ($($t:tt)*) => {
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!($($t)*)))
    };
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

    /// Load the embedded ipl-hle DOL into guest RAM and return its entry point.
    ///
    /// The ipl-hle binary is baked into the WASM module at compile time via
    /// `include_bytes!` (the same mechanism the native `lazuli` crate uses).
    /// It is built from `crates/ipl-hle/` and placed at `local/ipl-hle.dol`
    /// by `just ipl-hle build` before `wasm-pack build` is run.
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
    pub fn load_ipl_hle(&mut self) -> u32 {
        const DOL: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../local/ipl-hle.dol"
        ));

        /// Read a big-endian u32 from the DOL bytes at `offset`.
        fn u32be(dol: &[u8], offset: usize) -> u32 {
            u32::from_be_bytes(dol[offset..offset + 4].try_into().unwrap())
        }

        let text_offsets: [u32; 7]  = core::array::from_fn(|i| u32be(DOL, 0x000 + i * 4));
        let data_offsets: [u32; 11] = core::array::from_fn(|i| u32be(DOL, 0x01C + i * 4));
        let text_targets: [u32; 7]  = core::array::from_fn(|i| u32be(DOL, 0x048 + i * 4));
        let data_targets: [u32; 11] = core::array::from_fn(|i| u32be(DOL, 0x064 + i * 4));
        let text_sizes:   [u32; 7]  = core::array::from_fn(|i| u32be(DOL, 0x090 + i * 4));
        let data_sizes:   [u32; 11] = core::array::from_fn(|i| u32be(DOL, 0x0AC + i * 4));
        let bss_target = u32be(DOL, 0x0D8);
        let bss_size   = u32be(DOL, 0x0DC);
        let entry      = u32be(DOL, 0x0E0);

        for i in 0..7 {
            if text_offsets[i] != 0 && text_sizes[i] != 0 {
                let start = text_offsets[i] as usize;
                let size  = text_sizes[i] as usize;
                self.load_bytes(text_targets[i], &DOL[start..start + size]);
            }
        }
        for i in 0..11 {
            if data_offsets[i] != 0 && data_sizes[i] != 0 {
                let start = data_offsets[i] as usize;
                let size  = data_sizes[i] as usize;
                self.load_bytes(data_targets[i], &DOL[start..start + size]);
            }
        }
        if bss_size > 0 {
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
    /// Call this in addition to (not instead of) the DOL-loading path in
    /// `parseAndLoadIso` / `load_bytes`.
    pub fn load_disc_image(&mut self, data: &[u8]) {
        let disc_size_mib = data.len() / (1024 * 1024);
        self.disc = Some(data.to_vec());
        // Mark the cover as closed (bit 2 = DICVR_STATE = 1 → cover closed,
        // disc present) so games that poll DICOVER before issuing commands see
        // a valid disc in the drive.
        self.di.cover = 0x4;
        console_log!(
            "[lazuli] DiscImageDevice: stored {} MiB disc image for runtime reads",
            disc_size_mib
        );
    }
}
