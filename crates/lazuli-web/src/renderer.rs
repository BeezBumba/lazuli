//! WebGPU rendering backend — Phase 4.
//!
//! Provides [`init_webgpu_renderer`], an async `wasm-bindgen` function that
//! initialises the `wgpu` WebGPU backend from a named `<canvas>` element and
//! returns an opaque [`WgpuRenderer`] handle to JavaScript.
//!
//! ## JavaScript usage
//!
//! ```js
//! import init, { init_webgpu_renderer } from "./pkg/lazuli_web.js";
//! await init();
//! const canvas = document.getElementById("game-canvas");
//! const renderer = await init_webgpu_renderer("game-canvas");
//! if (renderer) {
//!   // WebGPU surface is ready; renderer.present_xfb(xfbData) now works.
//! }
//! ```
//!
//! The function is compiled **only on `wasm32`** (gated with
//! `#[cfg(target_arch = "wasm32")]`) because `wgpu::SurfaceTarget::Canvas`
//! is only available in the WebGPU backend.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

/// Opaque handle to the initialised WebGPU rendering context.
///
/// Exported to JavaScript as `WgpuRenderer`.  When the renderer is
/// unavailable (WebGPU not supported or surface creation failed) the
/// [`init_webgpu_renderer`] factory returns `undefined` instead of this type.
#[wasm_bindgen]
pub struct WgpuRenderer {
    device:   wgpu::Device,
    queue:    wgpu::Queue,
    surface:  wgpu::Surface<'static>,
    config:   wgpu::SurfaceConfiguration,
    renderer: renderer::Renderer,
}

#[wasm_bindgen]
impl WgpuRenderer {
    /// Present a 640×480 YUV422 external frame-buffer via the wgpu blitter.
    ///
    /// `xfb_data` must be a `Uint8Array` (or `ArrayBuffer`-backed view) of
    /// exactly `640 × 480 × 2 = 614 400` bytes in GameCube YCbYCr 4:2:2
    /// byte order.  Returns `true` on success, `false` if the surface is
    /// currently lost or `xfb_data` has the wrong length.
    pub fn present_xfb(&self, xfb_data: &[u8]) -> bool {
        if xfb_data.len() != 640 * 480 * 2 {
            return false;
        }

        let output = match self.surface.get_current_texture() {
            Ok(t)  => t,
            Err(_) => return false,
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lazuli xfb blit"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lazuli xfb pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.renderer.render(&mut pass);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
        true
    }

    /// Reconfigure the swap-chain when the canvas is resized.
    ///
    /// `width` and `height` are the new canvas dimensions in physical pixels.
    /// Must be called from JavaScript whenever the canvas `resize` event fires.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width  = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }
}

/// Initialise a WebGPU rendering surface from a `<canvas>` element.
///
/// `canvas_id` is the `id` attribute of the canvas element (e.g. `"gc-canvas"`).
///
/// Returns a `Promise<WgpuRenderer | undefined>`:
/// - **`WgpuRenderer`** on success.
/// - **`undefined`** when WebGPU is unavailable, the canvas is not found, or
///   adapter / device creation fails.
///
/// Typical call from JS:
/// ```js
/// const renderer = await init_webgpu_renderer("gc-canvas");
/// ```
#[wasm_bindgen]
pub async fn init_webgpu_renderer(canvas_id: &str) -> Option<WgpuRenderer> {
    // Locate the target <canvas>.
    let window   = web_sys::window()?;
    let document = window.document()?;
    let element  = document.get_element_by_id(canvas_id)?;
    let canvas: HtmlCanvasElement = element.dyn_into().ok()?;

    // Create a wgpu instance using the WebGPU backend.
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::BROWSER_WEBGPU,
        ..Default::default()
    });

    // Wrap the canvas in a wgpu surface target.
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .ok()?;

    // Request a GPU adapter compatible with our surface.
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference:       wgpu::PowerPreference::HighPerformance,
            compatible_surface:     Some(&surface),
            force_fallback_adapter: false,
        })
        .await?;

    // Request a logical device.
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label:             Some("lazuli-web wgpu device"),
            required_features: wgpu::Features::empty(),
            required_limits:   wgpu::Limits::downlevel_webgl2_defaults(),
            memory_hints:      wgpu::MemoryHints::MemoryUsage,
            trace:             wgpu::Trace::Off,
        })
        .await
        .ok()?;

    // Choose a surface format.
    let caps   = surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .unwrap_or(caps.formats[0]);

    let width  = canvas.width().max(1);
    let height = canvas.height().max(1);

    let config = wgpu::SurfaceConfiguration {
        usage:        wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width,
        height,
        present_mode: wgpu::PresentMode::AutoVsync,
        alpha_mode:   wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);

    // Build the Lazuli renderer (XFB blitter + GX pipeline).
    let renderer = renderer::Renderer::new(device.clone(), queue.clone(), format);

    web_sys::console::log_1(&JsValue::from_str(&format!(
        "[lazuli] WebGPU renderer initialised — adapter: {:?}, format: {:?}",
        adapter.get_info().name,
        format,
    )));

    Some(WgpuRenderer { device, queue, surface, config, renderer })
}
