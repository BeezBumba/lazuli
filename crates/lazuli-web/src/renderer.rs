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
//! const renderer = await init_webgpu_renderer("gc-canvas");
//! if (renderer) {
//!   // WebGPU surface is ready; renderer.present_xfb(xfbData) now works.
//! }
//! ```
//!
//! ## XFB blitting pipeline
//!
//! `present_xfb` converts the raw GameCube YCbYCr 4:2:2 frame-buffer to RGBA
//! on the WASM/CPU side (BT.601 fixed-point coefficients), uploads the result
//! to a 640×480 `Rgba8Unorm` GPU texture via `Queue::write_texture`, then
//! draws a fullscreen quad that samples the texture onto the swap-chain surface.
//!
//! The function is compiled **only on `wasm32`** (gated with
//! `#[cfg(target_arch = "wasm32")]`) because `wgpu::SurfaceTarget::Canvas`
//! is only available in the WebGPU backend.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

// ─── XFB blit constants ───────────────────────────────────────────────────────

/// GameCube XFB dimensions (pixels).
const XFB_W: u32 = 640;
const XFB_H: u32 = 480;

/// Raw GameCube XFB byte length: 640 × 480 × 2 bytes (YCbYCr 4:2:2).
const XFB_RAW_BYTES: usize = (XFB_W * XFB_H * 2) as usize;

/// Decoded RGBA byte length for the XFB texture: 640 × 480 × 4 bytes.
const XFB_RGBA_BYTES: usize = (XFB_W * XFB_H * 4) as usize;

// ─── Fullscreen-quad WGSL shader ─────────────────────────────────────────────
//
// The vertex shader generates a clip-space fullscreen quad from vertex indices
// 0–3 (triangle-strip topology; no vertex buffer needed).  The fragment shader
// samples the bound XFB RGBA texture.

const XFB_BLIT_WGSL: &str = r#"
@group(0) @binding(0) var xfb_tex: texture_2d<f32>;
@group(0) @binding(1) var xfb_smp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0)       uv:  vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Produce four vertices of a screen-filling quad in CCW winding order.
    let x  = select(-1.0,  1.0, (vi & 1u) != 0u);
    let y  = select(-1.0,  1.0, (vi & 2u) == 0u);
    let u  = select( 0.0,  1.0, (vi & 1u) != 0u);
    let v  = select( 0.0,  1.0, (vi & 2u) != 0u);
    return VsOut(vec4<f32>(x, y, 0.0, 1.0), vec2<f32>(u, v));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(xfb_tex, xfb_smp, in.uv);
}
"#;

// ─── YUV422 → RGBA conversion ─────────────────────────────────────────────────

/// Convert a 640×480 GameCube YCbYCr 4:2:2 XFB buffer to RGBA8.
///
/// The GameCube stores each pair of pixels as `[Cb, Y0, Cr, Y1]` (4 bytes for
/// 2 pixels).  BT.601 fixed-point conversion (coefficients scaled by 1024):
///
/// ```text
/// R = Y + (1402 * (Cr − 128)) >> 10
/// G = Y − (344  * (Cb − 128) + 714 * (Cr − 128)) >> 10
/// B = Y + (1772 * (Cb − 128)) >> 10
/// ```
///
/// Results are clamped to `[0, 255]`.  The output buffer is 640 × 480 × 4 bytes
/// in row-major RGBA order.
fn yuv422_to_rgba(xfb: &[u8]) -> Vec<u8> {
    debug_assert_eq!(xfb.len(), XFB_RAW_BYTES);

    let mut rgba = vec![0u8; XFB_RGBA_BYTES];
    let pairs = (XFB_W * XFB_H / 2) as usize;

    for i in 0..pairs {
        let base = i * 4;
        let cb = xfb[base]     as i32;
        let y0 = xfb[base + 1] as i32;
        let cr = xfb[base + 2] as i32;
        let y1 = xfb[base + 3] as i32;

        let cb_off = cb - 128;
        let cr_off = cr - 128;

        macro_rules! to_pixel {
            ($y:expr) => {{
                let r = ($y + ((1402 * cr_off) >> 10)).clamp(0, 255) as u8;
                let g = ($y - ((344 * cb_off + 714 * cr_off) >> 10)).clamp(0, 255) as u8;
                let b = ($y + ((1772 * cb_off) >> 10)).clamp(0, 255) as u8;
                [r, g, b, 255u8]
            }};
        }

        let p = i * 8;
        rgba[p..p + 4].copy_from_slice(&to_pixel!(y0));
        rgba[p + 4..p + 8].copy_from_slice(&to_pixel!(y1));
    }

    rgba
}

// ─── WgpuRenderer ─────────────────────────────────────────────────────────────

/// Opaque handle to the initialised WebGPU rendering context.
///
/// Exported to JavaScript as `WgpuRenderer`.  When the renderer is
/// unavailable (WebGPU not supported or surface creation failed) the
/// [`init_webgpu_renderer`] factory returns `undefined` instead of this type.
#[wasm_bindgen]
pub struct WgpuRenderer {
    device:               wgpu::Device,
    queue:                wgpu::Queue,
    surface:              wgpu::Surface<'static>,
    config:               wgpu::SurfaceConfiguration,
    // XFB direct-blit resources.
    xfb_texture:          wgpu::Texture,
    xfb_pipeline:         wgpu::RenderPipeline,
    xfb_bind_group_layout: wgpu::BindGroupLayout,
    xfb_sampler:          wgpu::Sampler,
    // GX pipeline renderer (for future hardware-accelerated GX rendering).
    #[allow(dead_code)]
    renderer:             renderer::Renderer,
}

#[wasm_bindgen]
impl WgpuRenderer {
    /// Present a 640×480 YUV422 external frame-buffer via the wgpu blitter.
    ///
    /// `xfb_data` must be a `Uint8Array` (or `ArrayBuffer`-backed view) of
    /// exactly `640 × 480 × 2 = 614 400` bytes in GameCube YCbYCr 4:2:2
    /// byte order (`[Cb, Y0, Cr, Y1]` per pair of pixels).
    ///
    /// The function converts the YUV422 data to RGBA8 on the CPU, uploads it
    /// to a 640×480 GPU texture, then blits the texture to the swap-chain
    /// surface using a fullscreen-quad render pass.
    ///
    /// Returns `true` on success, `false` if the surface texture is lost or
    /// `xfb_data` has the wrong length.
    pub fn present_xfb(&self, xfb_data: &[u8]) -> bool {
        if xfb_data.len() != XFB_RAW_BYTES {
            return false;
        }

        // Convert YUV422 → RGBA8 on the CPU (Rust/WASM, significantly faster than JS).
        let rgba = yuv422_to_rgba(xfb_data);

        // Upload RGBA data to the XFB texture.
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture:   &self.xfb_texture,
                mip_level: 0,
                origin:    wgpu::Origin3d::ZERO,
                aspect:    wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset:         0,
                bytes_per_row:  Some(XFB_W * 4),
                rows_per_image: Some(XFB_H),
            },
            wgpu::Extent3d { width: XFB_W, height: XFB_H, depth_or_array_layers: 1 },
        );

        // Acquire the next swap-chain frame.
        let output = match self.surface.get_current_texture() {
            Ok(t)  => t,
            Err(_) => return false,
        };
        let surface_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Build a bind group for the XFB texture + sampler.
        let xfb_view = self.xfb_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("lazuli xfb bind group"),
            layout:  &self.xfb_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding:  0,
                    resource: wgpu::BindingResource::TextureView(&xfb_view),
                },
                wgpu::BindGroupEntry {
                    binding:  1,
                    resource: wgpu::BindingResource::Sampler(&self.xfb_sampler),
                },
            ],
        });

        // Record and submit the fullscreen-quad blit render pass.
        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("lazuli xfb blit") },
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lazuli xfb pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           &surface_view,
                    resolve_target: None,
                    depth_slice:    None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes:         None,
                occlusion_query_set:      None,
            });
            pass.set_pipeline(&self.xfb_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..4, 0..1); // fullscreen quad (triangle-strip)
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

// ─── Initialisation ───────────────────────────────────────────────────────────

/// Initialise a WebGPU rendering surface from a `<canvas>` element.
///
/// `canvas_id` is the `id` attribute of the canvas element (e.g. `"screen"`).
///
/// Returns a `Promise<WgpuRenderer | undefined>`:
/// - **`WgpuRenderer`** on success.
/// - **`undefined`** when WebGPU is unavailable, the canvas is not found, or
///   adapter / device creation fails.
///
/// Typical call from JS:
/// ```js
/// const renderer = await init_webgpu_renderer("screen");
/// if (renderer) { /* use renderer.present_xfb(rawYuv) each frame */ }
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
        .await
        .ok()?;

    // Request a logical device.
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label:                  Some("lazuli-web wgpu device"),
            required_features:      wgpu::Features::empty(),
            required_limits:        wgpu::Limits::downlevel_webgl2_defaults(),
            memory_hints:           wgpu::MemoryHints::MemoryUsage,
            trace:                  wgpu::Trace::Off,
            experimental_features:  wgpu::ExperimentalFeatures::disabled(),
        })
        .await
        .ok()?;

    // Choose a surface format (prefer sRGB for correct colour output).
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

    // ── XFB blit resources ────────────────────────────────────────────────────

    // 640×480 Rgba8Unorm texture that receives the decoded XFB each frame.
    let xfb_texture = device.create_texture(&wgpu::TextureDescriptor {
        label:           Some("lazuli xfb texture"),
        size:            wgpu::Extent3d { width: XFB_W, height: XFB_H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count:    1,
        dimension:       wgpu::TextureDimension::D2,
        format:          wgpu::TextureFormat::Rgba8Unorm,
        usage:           wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats:    &[],
    });

    let xfb_bind_group_layout = device.create_bind_group_layout(
        &wgpu::BindGroupLayoutDescriptor {
            label:   Some("lazuli xfb bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding:    0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled:   false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        },
    );

    let xfb_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label:  Some("lazuli xfb blit shader"),
        source: wgpu::ShaderSource::Wgsl(XFB_BLIT_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label:                Some("lazuli xfb pipeline layout"),
        bind_group_layouts:   &[&xfb_bind_group_layout],
        push_constant_ranges: &[],
    });

    let xfb_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label:  Some("lazuli xfb pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module:               &xfb_shader,
            entry_point:          Some("vs_main"),
            compilation_options:  Default::default(),
            buffers:              &[],
        },
        fragment: Some(wgpu::FragmentState {
            module:              &xfb_shader,
            entry_point:         Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend:       None,
                write_mask:  wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology:           wgpu::PrimitiveTopology::TriangleStrip,
            strip_index_format: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample:   Default::default(),
        multiview:     None,
        cache:         None,
    });

    let xfb_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label:        Some("lazuli xfb sampler"),
        mag_filter:   wgpu::FilterMode::Linear,
        min_filter:   wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    // ── GX pipeline renderer (future hardware-accelerated GX) ─────────────────
    let renderer = renderer::Renderer::new(device.clone(), queue.clone(), format);

    web_sys::console::log_1(&JsValue::from_str(&format!(
        "[lazuli] WebGPU renderer initialised — adapter: {:?}, format: {:?}",
        adapter.get_info().name,
        format,
    )));

    Some(WgpuRenderer {
        device,
        queue,
        surface,
        config,
        xfb_texture,
        xfb_pipeline,
        xfb_bind_group_layout,
        xfb_sampler,
        renderer,
    })
}
