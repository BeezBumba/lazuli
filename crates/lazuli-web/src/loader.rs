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
