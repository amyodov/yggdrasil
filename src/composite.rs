//! Composite pipeline — draws a plate's offscreen RT as a textured quad on
//! the swap chain.
//!
//! The vertex shader:
//! 1. Starts with a unit quad corner (0..1).
//! 2. Scales to plate-local pixel space (`corner * plate_size`).
//! 3. Applies the plate's model matrix — identity = frontal (M3.1 default),
//!    a rotation matrix = 3D tilt (future milestones).
//! 4. Adds the plate's top-left screen position.
//! 5. Converts to NDC.
//!
//! The fragment shader samples the plate RT at the unit-quad UV.
//!
//! ## Blend state
//!
//! Premultiplied-alpha "over" (source factor = `One`, dest factor =
//! `OneMinusSrcAlpha`). This matches both:
//!
//! - **Shapes pipeline** (`shapes.rs`): its fragment shader emits
//!   `vec4<f32>(rgb_pre, a)` where `rgb_pre = rgb_straight * a`, and uses the
//!   same blend. Values stored in the plate RT are therefore premultiplied.
//! - **Glyphon 0.6**: text pipeline emits premultiplied alpha too.
//!
//! So sampling the plate RT and writing with premultiplied-alpha over the
//! swap chain (which already has the sky from the background pass) produces
//! the correct composite.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};

/// Uniforms consumed by the composite shader. Written once per plate per
/// frame via `CompositeRenderer::prepare`.
///
/// GPU alignment: the `mat4x4<f32>` in WGSL has 16-byte column alignment, and
/// `vec2<f32>` has 8-byte alignment. Laid out as two `vec2`s + one `vec2` +
/// an explicit 8-byte pad puts `model` on a 16-byte boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CompositeUniforms {
    /// Window (swap chain) size in physical pixels.
    pub viewport_size: [f32; 2],
    /// Plate top-left in window space, physical pixels.
    pub plate_pos: [f32; 2],
    /// Plate size in physical pixels.
    pub plate_size: [f32; 2],
    pub _pad: [f32; 2],
    /// Column-major 4x4 matrix applied to plate-local coordinates before the
    /// screen translation. Identity = frontal (M3.1).
    pub model: [[f32; 4]; 4],
}

pub struct CompositeRenderer {
    pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    pub uniform_buffer: wgpu::Buffer,
}

impl CompositeRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ygg-composite-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("ygg-composite-bgl"),
                entries: &[
                    // Uniforms (viewport, plate pos/size, model matrix).
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Plate RT texture.
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
                    // Sampler.
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ygg-plate-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-composite-uniforms"),
            size: size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ygg-composite-pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ygg-composite-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        // Premultiplied-alpha "over".
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        Self { pipeline, bind_group_layout, sampler, uniform_buffer }
    }

    /// Write the uniforms for the single plate we're about to composite.
    /// For multi-plate rendering in future milestones, call once per plate
    /// (with dynamic offsets) or switch to per-plate uniform buffers.
    pub fn prepare(
        &self,
        queue: &wgpu::Queue,
        viewport_size: (u32, u32),
        plate_pos: [f32; 2],
        plate_size: [u32; 2],
        model: [[f32; 4]; 4],
    ) {
        let u = CompositeUniforms {
            viewport_size: [viewport_size.0 as f32, viewport_size.1 as f32],
            plate_pos,
            plate_size: [plate_size[0] as f32, plate_size[1] as f32],
            _pad: [0.0; 2],
            model,
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&u));
    }

    pub fn render<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        bind_group: &'a wgpu::BindGroup,
    ) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

const SHADER: &str = r#"
struct Uniforms {
    viewport_size: vec2<f32>,
    plate_pos:     vec2<f32>,
    plate_size:    vec2<f32>,
    _pad:          vec2<f32>,
    model:         mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var plate_tex: texture_2d<f32>;
@group(0) @binding(2) var plate_samp: sampler;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

fn corner_for(vi: u32) -> vec2<f32> {
    switch vi {
        case 0u: { return vec2<f32>(0.0, 0.0); }
        case 1u: { return vec2<f32>(1.0, 0.0); }
        case 2u: { return vec2<f32>(0.0, 1.0); }
        case 3u: { return vec2<f32>(1.0, 0.0); }
        case 4u: { return vec2<f32>(1.0, 1.0); }
        default: { return vec2<f32>(0.0, 1.0); }
    }
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let corner = corner_for(vi);
    // Plate-local pixel coordinates: 0..plate_size.
    let local = vec4<f32>(corner * u.plate_size, 0.0, 1.0);
    // Apply model matrix (identity today, rotations later).
    let transformed = u.model * local;
    // Translate into window space.
    let screen = transformed.xy + u.plate_pos;
    // Convert to NDC (y flipped: screen space origin is top-left).
    let ndc_x = (screen.x / u.viewport_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (screen.y / u.viewport_size.y) * 2.0;

    var out: VsOut;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, transformed.z, 1.0);
    out.uv = corner; // unit-quad UV sampled straight into the plate RT
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Plate RT stores premultiplied alpha (shapes + glyphon both emit it).
    // The pipeline's blend state treats this as premultiplied "over".
    return textureSample(plate_tex, plate_samp, in.uv);
}
"#;
