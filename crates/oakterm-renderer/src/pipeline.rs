//! wgpu render pipeline setup and draw commands.

use wgpu::util::DeviceExt;

/// GPU state for the terminal renderer.
pub struct RenderPipeline {
    bg_pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,
    bg_bind_group_layout: wgpu::BindGroupLayout,
    text_bind_group_layout: wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
}

/// Uniform data for the background pass.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BgUniforms {
    pub cols: u32,
    pub rows: u32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub pad: [f32; 2],
}

/// Uniform data for the text pass.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TextUniforms {
    pub cell_width: f32,
    pub cell_height: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub atlas_width: f32,
    pub atlas_height: f32,
    pub text_gamma: f32,
    pub color_atlas_width: f32,
    pub color_atlas_height: f32,
    pub pad: f32,
}

/// Per-glyph instance data for the text pass.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlyphVertex {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv_origin: [f32; 2],
    pub fg_color: [f32; 4],
    pub bg_luminance: f32,
    /// 1.0 for color emoji (sampled from color atlas), 0.0 for mono text.
    pub is_color: f32,
    pub pad: [f32; 2],
}

impl RenderPipeline {
    /// Create the render pipelines.
    ///
    /// # Panics
    /// Panics if shader compilation fails.
    #[must_use]
    #[allow(clippy::too_many_lines)] // Pipeline setup is inherently verbose
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, blending_mode: u32) -> Self {
        let bg_src = crate::shaders::background_shader(blending_mode);
        let bg_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bg_shader"),
            source: wgpu::ShaderSource::Wgsl(bg_src.into()),
        });

        let text_src = crate::shaders::text_shader(blending_mode);
        let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text_shader"),
            source: wgpu::ShaderSource::Wgsl(text_src.into()),
        });

        let bg_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bg_bind_group_layout"),
                entries: &[
                    // Uniforms.
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Background colors storage buffer.
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let text_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("text_bind_group_layout"),
                entries: &[
                    // Uniforms.
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Atlas texture.
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // Atlas sampler.
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // Color atlas texture (Rgba8UnormSrgb for emoji).
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });

        let bg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bg_pipeline_layout"),
            bind_group_layouts: &[Some(&bg_bind_group_layout)],
            immediate_size: 0,
        });

        let text_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text_pipeline_layout"),
            bind_group_layouts: &[Some(&text_bind_group_layout)],
            immediate_size: 0,
        });

        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bg_pipeline"),
            layout: Some(&bg_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &bg_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &bg_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text_pipeline"),
            layout: Some(&text_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &text_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GlyphVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        // pos
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        // size
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        // uv_origin
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 16,
                            shader_location: 2,
                        },
                        // fg_color
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 24,
                            shader_location: 3,
                        },
                        // bg_luminance
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 40,
                            shader_location: 4,
                        },
                        // is_color
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 44,
                            shader_location: 5,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &text_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            bg_pipeline,
            text_pipeline,
            bg_bind_group_layout,
            text_bind_group_layout,
            format,
        }
    }

    /// Render a frame to the given texture view.
    ///
    /// # Panics
    /// Panics if `glyph_instances` exceeds `u32::MAX` elements.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        bg_uniforms: &BgUniforms,
        bg_colors: &[u32],
        text_uniforms: &TextUniforms,
        glyph_instances: &[GlyphVertex],
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
        color_atlas_view: &wgpu::TextureView,
        clear_color: wgpu::Color,
    ) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_encoder"),
        });

        // Pass 1: backgrounds.
        // Skip buffer creation when grid is empty — wgpu rejects zero-size
        // storage buffers. The render pass still runs to clear the target.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bg_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });

            let cell_count = bg_uniforms.cols * bg_uniforms.rows;
            debug_assert_eq!(
                bg_colors.len(),
                cell_count as usize,
                "bg_colors length ({}) must match cols * rows ({})",
                bg_colors.len(),
                cell_count,
            );
            if cell_count > 0 {
                let bg_uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("bg_uniforms"),
                    contents: bytemuck::bytes_of(bg_uniforms),
                    usage: wgpu::BufferUsages::UNIFORM,
                });

                let bg_colors_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("bg_colors"),
                    contents: bytemuck::cast_slice(bg_colors),
                    usage: wgpu::BufferUsages::STORAGE,
                });

                let bg_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("bg_bind_group"),
                    layout: &self.bg_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: bg_uniform_buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: bg_colors_buf.as_entire_binding(),
                        },
                    ],
                });

                pass.set_pipeline(&self.bg_pipeline);
                pass.set_bind_group(0, &bg_bind_group, &[]);
                pass.draw(0..4, 0..cell_count);
            }
        }

        // Pass 2: text glyphs (skip if nothing to draw).
        if !glyph_instances.is_empty() {
            let text_uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("text_uniforms"),
                contents: bytemuck::bytes_of(text_uniforms),
                usage: wgpu::BufferUsages::UNIFORM,
            });

            let text_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("text_bind_group"),
                layout: &self.text_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: text_uniform_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(atlas_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(color_atlas_view),
                    },
                ],
            });

            let instance_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glyph_instances"),
                contents: bytemuck::cast_slice(glyph_instances),
                usage: wgpu::BufferUsages::VERTEX,
            });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("text_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.text_pipeline);
            pass.set_bind_group(0, &text_bind_group, &[]);
            pass.set_vertex_buffer(0, instance_buf.slice(..));
            let count: u32 = glyph_instances
                .len()
                .try_into()
                .expect("glyph instance count exceeds u32::MAX");
            pass.draw(0..4, 0..count);
        }

        queue.submit(std::iter::once(encoder.finish()));
    }

    /// The texture format this pipeline was created for.
    #[must_use]
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
}
