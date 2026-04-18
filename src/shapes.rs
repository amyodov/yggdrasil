//! SDF rounded-rectangle pipeline with outer glow.
//!
//! Each instance is a rectangle with: rounded corners (configurable radius),
//! a fill color, and an emissive glow halo that falls off smoothly outside
//! the rect. Rendered with premultiplied-alpha blending so halos compose
//! cleanly over the background and each other.
//!
//! Coordinates: `pos` / `size` are in **physical pixels**, top-left origin,
//! y growing downward. The shader flips to clip space using the viewport
//! size uploaded via the uniform buffer.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

/// One rectangle instance. Laid out so GPU alignment is satisfied without
/// explicit padding inserted between fields (all 4-byte floats, 64 bytes
/// total = multiple of 16).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct RectInstance {
    /// Top-left in physical pixels.
    pub pos: [f32; 2],
    /// Width, height in physical pixels.
    pub size: [f32; 2],
    /// Fill color (sRGB, **straight** / not pre-multiplied — the shader
    /// premultiplies before composition).
    pub color: [f32; 4],
    /// Glow halo color (sRGB, straight). Alpha controls intensity.
    pub glow_color: [f32; 4],
    /// Corner radius in physical pixels. 0 = sharp corners.
    pub corner_radius: f32,
    /// Halo falloff radius in physical pixels. 0 = no glow.
    pub glow_radius: f32,
    /// Dome amount (M3.2 Pass 3): 0.0 = flat; >0.0 applies a subtle
    /// convex-lens shading to the rect's interior — lit on the top-left,
    /// shadowed on the bottom-right, matching the plate's implicit above-
    /// light. Makes rounded "chips" read as physical buttons without any
    /// geometry change. Typical values around 0.5–1.0; above that is
    /// cartoonish.
    pub dome: f32,
    /// Padding to keep the struct size a multiple of 16 bytes.
    pub _pad: f32,
}

impl RectInstance {
    /// Build a filled rectangle without a glow halo — use for interior shapes
    /// like the fold handle square.
    pub fn solid(x: f32, y: f32, w: f32, h: f32, color: [f32; 4], corner_radius: f32) -> Self {
        Self {
            pos: [x, y],
            size: [w, h],
            color,
            glow_color: [0.0; 4],
            corner_radius,
            glow_radius: 0.0,
            dome: 0.0,
            _pad: 0.0,
        }
    }

    /// Builder-style override: domed shading on an already-constructed rect.
    /// `amount` 0.0 disables the effect; values around 0.6–1.0 read as a
    /// subtle physical button.
    pub fn with_dome(mut self, amount: f32) -> Self {
        self.dome = amount;
        self
    }

    /// Build a rectangle with an outer glow halo.
    #[allow(clippy::too_many_arguments)]
    pub fn glowing(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [f32; 4],
        corner_radius: f32,
        glow_color: [f32; 4],
        glow_radius: f32,
    ) -> Self {
        Self {
            pos: [x, y],
            size: [w, h],
            color,
            glow_color,
            corner_radius,
            glow_radius,
            dome: 0.0,
            _pad: 0.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport_size: [f32; 2],
    _pad: [f32; 2],
}

pub struct ShapeRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_buffer_capacity: usize,
    pending_count: u32,
}

impl ShapeRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ygg-shapes-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-shapes-uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("ygg-shapes-bgl"),
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ygg-shapes-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ygg-shapes-pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ygg-shapes-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<RectInstance>() as u64,
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
                            format: wgpu::VertexFormat::Float32x4,
                        },
                        wgpu::VertexAttribute {
                            offset: 48,
                            shader_location: 4,
                            format: wgpu::VertexFormat::Float32,
                        },
                        wgpu::VertexAttribute {
                            offset: 52,
                            shader_location: 5,
                            format: wgpu::VertexFormat::Float32,
                        },
                        wgpu::VertexAttribute {
                            offset: 56,
                            shader_location: 6,
                            format: wgpu::VertexFormat::Float32,
                        },
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
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
                        // Premultiplied-alpha compositing: fragment outputs
                        // (color * alpha, alpha), we blend with (1, 1-srcA).
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

        let initial_capacity = 64;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-shapes-instances"),
            size: (initial_capacity * size_of::<RectInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group,
            uniform_buffer,
            instance_buffer,
            instance_buffer_capacity: initial_capacity,
            pending_count: 0,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[RectInstance],
        viewport_size: (u32, u32),
    ) {
        if instances.len() > self.instance_buffer_capacity {
            let mut new_cap = self.instance_buffer_capacity.max(1);
            while new_cap < instances.len() {
                new_cap *= 2;
            }
            self.instance_buffer = device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("ygg-shapes-instances"),
                    contents: bytemuck::cast_slice(&pad_to(instances, new_cap)),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                },
            );
            self.instance_buffer_capacity = new_cap;
        } else if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }

        let uniforms = Uniforms {
            viewport_size: [viewport_size.0 as f32, viewport_size.1 as f32],
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        self.pending_count = instances.len() as u32;
    }

    pub fn render<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.pending_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..self.pending_count);
    }
}

fn pad_to(instances: &[RectInstance], capacity: usize) -> Vec<RectInstance> {
    let mut v = Vec::with_capacity(capacity);
    v.extend_from_slice(instances);
    v.resize(capacity, RectInstance::zeroed());
    v
}

const SHADER: &str = r#"
struct Uniforms {
    viewport_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct Instance {
    @location(0) pos:           vec2<f32>,
    @location(1) size:          vec2<f32>,
    @location(2) color:         vec4<f32>,
    @location(3) glow_color:    vec4<f32>,
    @location(4) corner_radius: f32,
    @location(5) glow_radius:   f32,
    @location(6) dome:          f32,
};

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) rel_pos:        vec2<f32>,
    @location(1) half_size:      vec2<f32>,
    @location(2) color:          vec4<f32>,
    @location(3) glow_color:     vec4<f32>,
    @location(4) corner_radius:  f32,
    @location(5) glow_radius:    f32,
    @location(6) dome:           f32,
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
    // Expand the quad by glow_radius on each side so the halo fits. Also
    // add a small pad when the dome is active so the pillowed outer edge
    // (which pushes slightly outward at mid-edges) doesn't clip.
    let g = inst.glow_radius;
    let dome_pad = select(0.0, 2.0, inst.dome > 0.001);
    let pad = g + dome_pad;
    let expanded_pos  = inst.pos  - vec2<f32>(pad, pad);
    let expanded_size = inst.size + vec2<f32>(pad * 2.0, pad * 2.0);

    let corner = corner_for(vi);
    let px = expanded_pos + corner * expanded_size;

    // Fragment position relative to the *core* rect's center. SDF operates
    // in this space.
    let center = inst.pos + inst.size * 0.5;
    let rel = px - center;

    let clip_x = (px.x / u.viewport_size.x) * 2.0 - 1.0;
    let clip_y = 1.0 - (px.y / u.viewport_size.y) * 2.0;

    var out: VsOut;
    out.clip_pos = vec4<f32>(clip_x, clip_y, 0.0, 1.0);
    out.rel_pos = rel;
    out.half_size = inst.size * 0.5;
    out.color = inst.color;
    out.glow_color = inst.glow_color;
    out.corner_radius = inst.corner_radius;
    out.glow_radius = inst.glow_radius;
    out.dome = inst.dome;
    return out;
}

// Signed distance to an axis-aligned rounded box centered at origin with
// half-size `h` and corner radius `r`. Negative inside, zero on the edge.
fn sdf_rounded_box(p: vec2<f32>, h: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - h + vec2<f32>(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - r;
}

// "Squi-pillow" SDF: rounded rect whose edges bulge slightly outward at
// their midpoints, corners staying put. Produces a pillow-on-a-couch
// silhouette rather than a straight-edged rounded rect. `bulge` is the
// extra outward reach at mid-edge in pixels.
//
// Mechanism: at a mid-edge point, abs(p.x/h.x) and abs(p.y/h.y) differ
// sharply (one is ~1, the other is ~0). At a corner, both are ~1. The
// `mid_weight` expression peaks at mid-edges and is zero at corners, so
// subtracting `bulge * mid_weight` from the rounded-rect SDF pushes the
// silhouette outward only at the sides, not the corners.
fn sdf_pillow_box(p: vec2<f32>, h: vec2<f32>, r: f32, bulge: f32) -> f32 {
    let d_rect = sdf_rounded_box(p, h, r);
    let norm = abs(p) / max(h, vec2<f32>(1.0, 1.0));
    let mid_weight = abs(norm.x - norm.y);  // 0 at corners, ~1 at mid-edge
    return d_rect - bulge * mid_weight;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Use the pillow SDF when this instance is domed; otherwise plain
    // rounded box. Pillow bulge scales with the smaller half-size so it
    // reads proportional regardless of chip dimensions.
    let d = select(
        sdf_rounded_box(in.rel_pos, in.half_size, in.corner_radius),
        sdf_pillow_box(
            in.rel_pos,
            in.half_size,
            in.corner_radius,
            min(in.half_size.x, in.half_size.y) * 0.12,
        ),
        in.dome > 0.001
    );

    // Anti-aliased fill: full inside, fade across ~1px of the edge.
    let fill_alpha = clamp(0.5 - d, 0.0, 1.0) * in.color.a;

    // Outer glow: starts at the edge (d=0), falls off to zero at d=glow_radius.
    // Quadratic falloff reads softer than linear.
    var glow_alpha = 0.0;
    if (in.glow_radius > 0.0001 && d > -0.5) {
        let t = clamp(1.0 - max(d, 0.0) / in.glow_radius, 0.0, 1.0);
        glow_alpha = t * t * in.glow_color.a;
    }

    // ---- Dome shading (M3.2 Pass 3) ----
    // Per-pixel radial shading: brightest near the button's center, fading
    // to neutral at the corners. Combined with a gentle diagonal tilt
    // (top-left slightly brighter, bottom-right slightly darker) this
    // produces a "bubble wrap" / "sat-on pillow" look — a visible bump in
    // the middle of the chip, consistent with the pillowed outer silhouette
    // produced by `sdf_pillow_box`.
    //
    // Disabled entirely when `in.dome` is zero so regular shapes take
    // zero extra cost.
    var fill_rgb = in.color.rgb;
    if (in.dome > 0.001) {
        let np = in.rel_pos / max(in.half_size, vec2<f32>(1.0, 1.0));
        let r = length(np);
        // Radial bump: strongest at center, tapers smoothly.
        let bump = (1.0 - smoothstep(0.0, 0.95, r)) * in.dome;
        // Diagonal tilt: normalised diagonal -1..+1 along top-left →
        // bottom-right axis. Negative = top-left (lit side).
        let diag = dot(np, vec2<f32>(0.7071, 0.7071));
        let tilt = -diag * (1.0 - smoothstep(0.35, 1.0, r)) * in.dome;
        // Overall brightness modulation: bump at center + diagonal tilt.
        let shade = 1.0 + bump * 0.22 + tilt * 0.32;
        fill_rgb = fill_rgb * shade;
    }

    // Composite fill over glow, output premultiplied so the blend state
    // (One, OneMinusSrcAlpha) gives correct results.
    let fill_rgb_pre = fill_rgb * fill_alpha;
    let glow_rgb = in.glow_color.rgb * glow_alpha;
    let rgb = fill_rgb_pre + glow_rgb * (1.0 - fill_alpha);
    let a = fill_alpha + glow_alpha * (1.0 - fill_alpha);
    return vec4<f32>(rgb, a);
}
"#;
