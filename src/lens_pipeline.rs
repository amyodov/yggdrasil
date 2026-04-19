//! Pixel-space lens pipeline — a real magnifying glass that samples the
//! plate RT at magnified coordinates and writes the result on the swap
//! chain.
//!
//! Runs as pass 5b (after the plate composite), reads from the plate RT
//! (which contains widget body + cards + small icons but **NOT** the lens
//! disc itself), writes inside the lens disc region on the swap chain.
//! Because the plate RT isn't modified by this pass, the icons at slots
//! stay where they are — the lens naturally reveals whichever icon (or
//! widget body, or nothing) is underneath its current position.
//!
//! The fragment shader also draws the rim darkening + three-dot specular
//! arc in the same pass, so the lens looks like a single composed object
//! (glass + magnified contents + rim + glint) with a single pipeline.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use wgpu::{
    util::DeviceExt, BindGroup, BindGroupLayout, Device, Queue, Sampler, TextureFormat,
    TextureView,
};

/// Per-lens instance. Drawn as an expanded quad covering the disc area.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LensInstance {
    /// Lens center in swap-chain pixels (where the disc is visually
    /// placed on screen).
    pub center: [f32; 2],
    /// Disc radius in swap-chain pixels.
    pub radius: f32,
    /// Magnification factor. 1.75 reads as "clearly bigger but still
    /// recognizable"; >2.5 tends toward funhouse-mirror.
    pub magnification: f32,
    /// Strength of barrel distortion + chromatic aberration. 0 = off.
    pub distort: f32,
    /// Angle (radians) the three-dot specular arc is centered on.
    pub spec_angle: f32,
    /// 0..1. Scales the specular arc's brightness; 0 hides it entirely
    /// (used at night when the sun is below the horizon).
    pub spec_intensity: f32,
    /// Specular tint (RGB, linear). Alpha used as base opacity.
    pub spec_color: [f32; 4],
    pub _pad: [f32; 3],
}

impl LensInstance {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        center: [f32; 2],
        radius: f32,
        magnification: f32,
        distort: f32,
        spec_angle: f32,
        spec_intensity: f32,
        spec_color: [f32; 4],
    ) -> Self {
        Self {
            center,
            radius,
            magnification,
            distort,
            spec_angle,
            spec_intensity,
            spec_color,
            _pad: [0.0; 3],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport_size: [f32; 2],
    plate_origin: [f32; 2],
    plate_size: [f32; 2],
    _pad: [f32; 2],
}

pub struct LensRenderer {
    pipeline: wgpu::RenderPipeline,
    uniform_bind_group_layout: BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: BindGroup,
    plate_bind_group_layout: BindGroupLayout,
    plate_sampler: Sampler,
    // Plate bind group is rebuilt when the plate RT is (re)created, so
    // we store it lazily.
    plate_bind_group: Option<BindGroup>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    pending_count: u32,
}

impl LensRenderer {
    pub fn new(device: &Device, target_format: TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ygg-lens-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-lens-uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ygg-lens-uniform-bgl"),
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
            label: Some("ygg-lens-uniform-bg"),
            layout: &uniform_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let plate_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ygg-lens-plate-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let plate_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ygg-lens-plate-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ygg-lens-pl"),
            bind_group_layouts: &[&uniform_bgl, &plate_bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ygg-lens-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<LensInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        // center
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        // radius
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32,
                        },
                        // magnification
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32,
                        },
                        // distort
                        wgpu::VertexAttribute {
                            offset: 16,
                            shader_location: 3,
                            format: wgpu::VertexFormat::Float32,
                        },
                        // spec_angle
                        wgpu::VertexAttribute {
                            offset: 20,
                            shader_location: 4,
                            format: wgpu::VertexFormat::Float32,
                        },
                        // spec_intensity
                        wgpu::VertexAttribute {
                            offset: 24,
                            shader_location: 5,
                            format: wgpu::VertexFormat::Float32,
                        },
                        // spec_color
                        wgpu::VertexAttribute {
                            offset: 28,
                            shader_location: 6,
                            format: wgpu::VertexFormat::Float32x4,
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
                        // Premultiplied-alpha "over."
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

        let initial_capacity = 8;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-lens-instances"),
            size: (initial_capacity * size_of::<LensInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            uniform_bind_group_layout: uniform_bgl,
            uniform_buffer,
            uniform_bind_group,
            plate_bind_group_layout: plate_bgl,
            plate_sampler,
            plate_bind_group: None,
            instance_buffer,
            instance_capacity: initial_capacity,
            pending_count: 0,
        }
    }

    /// Rebuild the plate-texture bind group. Call whenever the plate RT
    /// is (re)created — i.e., on resize.
    pub fn bind_plate(&mut self, device: &Device, plate_view: &TextureView) {
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ygg-lens-plate-bg"),
            layout: &self.plate_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(plate_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.plate_sampler),
                },
            ],
        });
        self.plate_bind_group = Some(bg);
    }

    pub fn prepare(
        &mut self,
        device: &Device,
        queue: &Queue,
        instances: &[LensInstance],
        viewport_size: (u32, u32),
        plate_origin: (f32, f32),
        plate_size: (u32, u32),
    ) {
        if instances.len() > self.instance_capacity {
            let mut new_cap = self.instance_capacity.max(1);
            while new_cap < instances.len() {
                new_cap *= 2;
            }
            self.instance_buffer = device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("ygg-lens-instances"),
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
            plate_origin: [plate_origin.0, plate_origin.1],
            plate_size: [plate_size.0 as f32, plate_size.1 as f32],
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&u));
        self.pending_count = instances.len() as u32;
    }

    pub fn render<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.pending_count == 0 {
            return;
        }
        let Some(plate_bg) = self.plate_bind_group.as_ref() else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_bind_group(1, plate_bg, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..self.pending_count);
    }

    // Silence dead_code on the layout field — wgpu keeps it alive via
    // the pipeline layout, but Rust's lint doesn't see that indirect use.
    #[allow(dead_code)]
    fn _touch_layout(&self) -> &BindGroupLayout {
        &self.uniform_bind_group_layout
    }
}

fn pad_to(instances: &[LensInstance], capacity: usize) -> Vec<LensInstance> {
    let mut v = Vec::with_capacity(capacity);
    v.extend_from_slice(instances);
    v.resize(capacity, LensInstance::zeroed());
    v
}

const SHADER: &str = r#"
struct Uniforms {
    viewport_size: vec2<f32>,
    plate_origin:  vec2<f32>,
    plate_size:    vec2<f32>,
    _pad:          vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var plate:   texture_2d<f32>;
@group(1) @binding(1) var plate_s: sampler;

struct Instance {
    @location(0) center:         vec2<f32>,
    @location(1) radius:         f32,
    @location(2) magnification:  f32,
    @location(3) distort:        f32,
    @location(4) spec_angle:     f32,
    @location(5) spec_intensity: f32,
    @location(6) spec_color:     vec4<f32>,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) rel:            vec2<f32>,
    @location(1) center:         vec2<f32>,
    @location(2) radius:         f32,
    @location(3) magnification:  f32,
    @location(4) distort:        f32,
    @location(5) spec_angle:     f32,
    @location(6) spec_intensity: f32,
    @location(7) spec_color:     vec4<f32>,
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
    // Expand the quad a hair past the disc radius so AA at the rim fits.
    let half = inst.radius + 1.0;
    let rel = (corner * 2.0 - vec2<f32>(1.0, 1.0)) * half;
    let px = inst.center + rel;

    let ndc_x = (px.x / u.viewport_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / u.viewport_size.y) * 2.0;

    var out: VsOut;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.rel = rel;
    out.center = inst.center;
    out.radius = inst.radius;
    out.magnification = inst.magnification;
    out.distort = inst.distort;
    out.spec_angle = inst.spec_angle;
    out.spec_intensity = inst.spec_intensity;
    out.spec_color = inst.spec_color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r = length(in.rel);
    // SDF distance to disc boundary.
    let d = r - in.radius;
    let disc_alpha = clamp(0.5 - d, 0.0, 1.0);
    if (disc_alpha <= 0.0) {
        discard;
    }

    // Normalized radius [0..1] within disc.
    let norm_r = clamp(r / in.radius, 0.0, 1.0);
    let dir = select(vec2<f32>(0.0, 0.0), in.rel / max(r, 1e-5), r > 1e-5);

    // Barrel distortion applied to the radial sample coordinate: the
    // farther from center, the more the sample is pushed further out.
    // Convex-lens outward curvature.
    let barrel_k = in.distort * 3.0;
    let distorted_r = r * (1.0 + barrel_k * norm_r * norm_r);

    // Base sample position in plate-local pixels = lens center on plate
    // + radial offset / magnification. The /magnification is what makes
    // the lens *magnify*: we sample from a smaller area than we paint.
    let lens_on_plate = in.center - u.plate_origin;
    let sample_plate = lens_on_plate + dir * (distorted_r / in.magnification);
    let base_uv = sample_plate / u.plate_size;

    // Chromatic aberration: red sample shifts outward, blue inward
    // (radially). Peak is in the mid-zone, not at the rim — the rim
    // is reserved for light play (darkening + specular), and overlapping
    // CA there just turns it into color mush. `ca_fade` ramps the
    // aberration back down in the outer 30% of the disc.
    let ca_fade = 1.0 - smoothstep(0.70, 1.0, norm_r);
    let ca_amount_px = in.distort * 4.0 * norm_r * ca_fade;
    let ca_offset_uv = dir * ca_amount_px / u.plate_size;
    let r_uv = base_uv + ca_offset_uv;
    let g_uv = base_uv;
    let b_uv = base_uv - ca_offset_uv;

    // Plate RT stores premultiplied pixels (from the composite blend
    // chain). For a "through-glass" read we treat them as if straight —
    // the near-opaque card regions the lens tends to sit over have
    // plate_alpha ≈ 1, so the premul is effectively straight.
    let rs = textureSample(plate, plate_s, r_uv);
    let gs = textureSample(plate, plate_s, g_uv);
    let bs = textureSample(plate, plate_s, b_uv);
    var out_rgb = vec3<f32>(rs.r, gs.g, bs.b);
    let plate_a = max(max(rs.a, gs.a), bs.a);

    // Rim darkening: the glass's edges refract light and read darker.
    let rim_dark = smoothstep(0.72, 1.0, norm_r) * 0.32;
    out_rgb = out_rgb * (1.0 - rim_dark);

    // Three-dot specular arc: one center dot at `spec_angle`, two
    // flanks at ±0.28 rad, all at radius 0.82. Each dot is a small
    // bright circle drawn additively on top of the magnified content.
    // Gated by spec_intensity so night produces no glint.
    let spec_rim = 0.82;
    let spec_dot_size = 0.08; // in normalized disc radii
    let frag_norm = in.rel / max(in.radius, 1.0);
    var spec_strength = 0.0;
    // Center dot:
    {
        let a = in.spec_angle;
        let dot_center = vec2<f32>(cos(a), sin(a)) * spec_rim;
        let d_dot = length(frag_norm - dot_center);
        let alpha = 1.0 - smoothstep(0.0, spec_dot_size, d_dot);
        spec_strength = max(spec_strength, alpha * 1.0);
    }
    // Flank -:
    {
        let a = in.spec_angle - 0.28;
        let dot_center = vec2<f32>(cos(a), sin(a)) * spec_rim;
        let d_dot = length(frag_norm - dot_center);
        let alpha = 1.0 - smoothstep(0.0, spec_dot_size * 0.8, d_dot);
        spec_strength = max(spec_strength, alpha * 0.45);
    }
    // Flank +:
    {
        let a = in.spec_angle + 0.28;
        let dot_center = vec2<f32>(cos(a), sin(a)) * spec_rim;
        let d_dot = length(frag_norm - dot_center);
        let alpha = 1.0 - smoothstep(0.0, spec_dot_size * 0.8, d_dot);
        spec_strength = max(spec_strength, alpha * 0.45);
    }
    spec_strength = spec_strength * in.spec_intensity * in.spec_color.a;
    out_rgb = mix(out_rgb, in.spec_color.rgb, spec_strength);

    // Final alpha: disc AA × plate coverage. If plate_a is small, the
    // lens over an empty area is transparent — but in practice the lens
    // sits on opaque cards so plate_a ≈ 1.
    let final_a = disc_alpha * max(plate_a, 0.25);
    return vec4<f32>(out_rgb * final_a, final_a);
}
"#;
