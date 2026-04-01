use lazuli::system::gx::color::Rgba;
#[cfg(not(feature = "webgpu"))]
use wesl::include_wesl;
use zerocopy::{Immutable, IntoBytes};

#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct State {
    color: Rgba,
    depth: f32,
}

pub struct Cleaner {
    pipeline_color: wgpu::RenderPipeline,
    pipeline_depth: wgpu::RenderPipeline,
    pipeline_both: wgpu::RenderPipeline,
    #[cfg(feature = "webgpu")]
    push_uniform: crate::push::PushUniform,
}

impl Cleaner {
    pub fn new(device: &wgpu::Device) -> Self {
        #[cfg(not(feature = "webgpu"))]
        let push_constant_ranges = &[wgpu::PushConstantRange {
            stages: wgpu::ShaderStages::FRAGMENT,
            range: 0..20,
        }];
        #[cfg(feature = "webgpu")]
        let push_uniform = crate::push::PushUniform::new(
            device,
            20,
            "clear push uniform",
            wgpu::ShaderStages::FRAGMENT,
        );

        #[cfg(feature = "webgpu")]
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[push_uniform.layout()],
            push_constant_ranges: &[],
        });
        #[cfg(not(feature = "webgpu"))]
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[],
            push_constant_ranges,
        });

        #[cfg(not(feature = "webgpu"))]
        let shader = include_wesl!("clear");
        #[cfg(feature = "webgpu")]
        let shader = include_str!(concat!(env!("OUT_DIR"), "/clear_webgpu.wgsl"));

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(shader.into()),
        });

        macro_rules! descriptor {
            ($color:expr, $depth:expr) => {
                wgpu::RenderPipelineDescriptor {
                    label: Some("cleaner pipeline"),
                    layout: Some(&layout),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleStrip,
                        strip_index_format: None,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: None,
                        unclipped_depth: false,
                        polygon_mode: wgpu::PolygonMode::Fill,
                        conservative: false,
                    },
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("fs_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba8UnormSrgb,
                            blend: None,
                            write_mask: if $color {
                                wgpu::ColorWrites::all()
                            } else {
                                wgpu::ColorWrites::empty()
                            },
                        })],
                    }),
                    multisample: wgpu::MultisampleState {
                        count: 4,
                        mask: !0,
                        alpha_to_coverage_enabled: false,
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: wgpu::TextureFormat::Depth32Float,
                        depth_write_enabled: $depth,
                        depth_compare: wgpu::CompareFunction::Always,
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                    multiview: None,
                    cache: None,
                }
            };
        }

        let pipeline_color = device.create_render_pipeline(&descriptor!(true, false));
        let pipeline_depth = device.create_render_pipeline(&descriptor!(false, true));
        let pipeline_both = device.create_render_pipeline(&descriptor!(true, true));

        Self {
            pipeline_color,
            pipeline_depth,
            pipeline_both,
            #[cfg(feature = "webgpu")]
            push_uniform,
        }
    }

    pub fn clear_target(
        &self,
        color: Option<Rgba>,
        depth: Option<f32>,
        pass: &mut wgpu::RenderPass<'_>,
        queue: &wgpu::Queue,
    ) {
        #[cfg(not(feature = "webgpu"))]
        let _ = queue;
        let pipeline = match (color, depth) {
            (Some(_), Some(_)) => &self.pipeline_both,
            (Some(_), None) => &self.pipeline_color,
            (None, Some(_)) => &self.pipeline_depth,
            (None, None) => return,
        };

        let state = State {
            color: color.unwrap_or_default(),
            depth: depth.unwrap_or_default(),
        };

        pass.set_pipeline(pipeline);

        #[cfg(not(feature = "webgpu"))]
        pass.set_push_constants(wgpu::ShaderStages::FRAGMENT, 0, state.as_bytes());

        #[cfg(feature = "webgpu")]
        {
            self.push_uniform.update(queue, state.as_bytes());
            self.push_uniform.set_bind_group(pass, 0);
        }

        pass.draw(0..4, 0..1);
    }
}

