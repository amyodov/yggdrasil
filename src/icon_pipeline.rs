//! Icon rendering pipeline — textured quads sampling the Lucide icon atlas.
//!
//! Each instance is a box in physical pixels + a tint + an atlas index.
//! The vertex shader emits a unit-quad UV and the fragment shader samples
//! the appropriate tile in the horizontally-packed atlas (tile N at
//! U ∈ [N/count, (N+1)/count]).
//!
//! The atlas's red/green/blue channels are always white where the icon is
//! drawn; the alpha channel is the coverage mask. Fragment output is
//! premultiplied-alpha `(tint.rgb * a, a)` where `a = sample.a * tint.a`.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use wgpu::{util::DeviceExt, Device, Queue, TextureFormat};

use crate::icons::IconAtlas;

/// Per-icon instance. 32 bytes → instance buffer is cheap.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct IconInstance {
    /// Top-left in physical pixels (plate-local or swap-chain-relative —
    /// same coordinate semantics as `RectInstance`; pipeline doesn't care).
    pub pos: [f32; 2],
    /// Width, height in physical pixels. Not necessarily square; the atlas
    /// tile is square but the quad can stretch.
    pub size: [f32; 2],
    /// Tint color (straight-alpha, sRGB).
    pub color: [f32; 4],
    /// Which atlas tile to sample: `0.0` = chevron-down, `1.0` =
    /// chevron-right, ... See `IconId::atlas_index`.
    pub icon_index: f32,
    pub _pad: [f32; 3],
}

impl IconInstance {
    pub fn new(x: f32, y: f32, size_px: f32, color: [f32; 4], icon_index: u32) -> Self {
        Self {
            pos: [x, y],
            size: [size_px, size_px],
            color,
            icon_index: icon_index as f32,
            _pad: [0.0; 3],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport_size: [f32; 2],
    atlas_tiles: f32,
    _pad: f32,
}

pub struct IconRenderer {
    pipeline: wgpu::RenderPipeline,
    uniform_bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    pending_count: u32,
    pub atlas: IconAtlas,
}

impl IconRenderer {
    pub fn new(device: &Device, queue: &Queue, target_format: TextureFormat) -> Self {
        let atlas = IconAtlas::new(device, queue);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ygg-icon-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-icon-uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ygg-icon-uniform-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ygg-icon-uniform-bg"),
            layout: &uniform_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ygg-icon-pl"),
            bind_group_layouts: &[&uniform_bgl, &atlas.bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ygg-icon-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<IconInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 16,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            offset: 32,
                            shader_location: 3,
                            format: wgpu::VertexFormat::Float32,
                        },
                    ],
                }],
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

        let initial_capacity = 32;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-icon-instances"),
            size: (initial_capacity * size_of::<IconInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            uniform_bind_group,
            uniform_buffer,
            instance_buffer,
            instance_capacity: initial_capacity,
            pending_count: 0,
            atlas,
        }
    }

    pub fn prepare(
        &mut self,
        device: &Device,
        queue: &Queue,
        instances: &[IconInstance],
        viewport_size: (u32, u32),
    ) {
        if instances.len() > self.instance_capacity {
            let mut new_cap = self.instance_capacity.max(1);
            while new_cap < instances.len() {
                new_cap *= 2;
            }
            self.instance_buffer = device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("ygg-icon-instances"),
                    contents: bytemuck::cast_slice(&pad_to(instances, new_cap)),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                },
            );
            self.instance_capacity = new_cap;
        } else if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }

        let u = Uniforms {
            viewport_size: [viewport_size.0 as f32, viewport_size.1 as f32],
            atlas_tiles: crate::icons::IconId::COUNT as f32,
            _pad: 0.0,
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&u));
        self.pending_count = instances.len() as u32;
    }

    pub fn render<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.pending_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_bind_group(1, &self.atlas.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..self.pending_count);
    }
}

fn pad_to(instances: &[IconInstance], capacity: usize) -> Vec<IconInstance> {
    let mut v = Vec::with_capacity(capacity);
    v.extend_from_slice(instances);
    v.resize(capacity, IconInstance::zeroed());
    v
}

const SHADER: &str = r#"
struct Uniforms {
    viewport_size: vec2<f32>,
    atlas_tiles:   f32,
    _pad:          f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var atlas:    texture_2d<f32>;
@group(1) @binding(1) var atlas_s:  sampler;

struct Instance {
    @location(0) pos:        vec2<f32>,
    @location(1) size:       vec2<f32>,
    @location(2) color:      vec4<f32>,
    @location(3) icon_index: f32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv:             vec2<f32>,
    @location(1) color:          vec4<f32>,
    @location(2) icon_index:     f32,
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
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    let corner = corner_for(vi);
    let px = inst.pos + corner * inst.size;
    let ndc_x = (px.x / u.viewport_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / u.viewport_size.y) * 2.0;

    var out: VsOut;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = corner;
    out.color = inst.color;
    out.icon_index = inst.icon_index;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Atlas is horizontally packed: tile N spans U ∈ [N/count, (N+1)/count].
    let tile_u = (in.uv.x + in.icon_index) / u.atlas_tiles;
    let sample = textureSample(atlas, atlas_s, vec2<f32>(tile_u, in.uv.y));
    // Atlas stores RGBA8Unorm; RGB channels are white (1.0) where drawn,
    // alpha channel is the coverage mask. Tint by instance colour.
    let a = sample.a * in.color.a;
    let rgb = in.color.rgb * a;
    return vec4<f32>(rgb, a);
}
"#;
