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
/// GPU alignment: `mat4x4<f32>` needs 16-byte column alignment, `vec4<f32>`
/// needs 16-byte alignment, `vec2<f32>` needs 8-byte alignment. Field order
/// is chosen so every 16-byte-aligned type lands on a 16-byte boundary.
///
/// Layout (total 128 bytes):
/// - viewport_size + plate_pos  ( 0.. 16)
/// - plate_size + _pad0         (16.. 32)
/// - bloom_color                (32.. 48)
/// - corner_radius + bloom_radius + _pad1 (48.. 64)
/// - model                      (64..128)
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CompositeUniforms {
    /// Window (swap chain) size in physical pixels.
    pub viewport_size: [f32; 2],
    /// Plate top-left in window space, physical pixels.
    pub plate_pos: [f32; 2],
    /// Plate size in physical pixels.
    pub plate_size: [f32; 2],
    pub _pad0: [f32; 2],
    /// Plate outer bloom color — a dim emissive tint that halos the plate in
    /// the void. RGBA where A is peak bloom alpha at the plate edge (the
    /// shader multiplies by SDF falloff).
    pub bloom_color: [f32; 4],
    /// Plate rounded-corner radius in physical pixels. Must match the
    /// panel's rounded silhouette so bloom and plate-mask align.
    pub corner_radius: f32,
    /// How far (physical pixels) the bloom extends outside the plate edge.
    /// The vertex shader expands the composite quad by this much on each
    /// side so the halo fits.
    pub bloom_radius: f32,
    /// Rim-light thickness in physical pixels — how far inward from the
    /// plate's top edge the rim highlight extends before fading out.
    pub rim_thickness: f32,
    /// Rim-light peak intensity (multiplier on the rim color).
    pub rim_intensity: f32,
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
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &self,
        queue: &wgpu::Queue,
        viewport_size: (u32, u32),
        plate_pos: [f32; 2],
        plate_size: [u32; 2],
        corner_radius: f32,
        bloom_radius: f32,
        bloom_color: [f32; 4],
        rim_thickness: f32,
        rim_intensity: f32,
        model: [[f32; 4]; 4],
    ) {
        let u = CompositeUniforms {
            viewport_size: [viewport_size.0 as f32, viewport_size.1 as f32],
            plate_pos,
            plate_size: [plate_size[0] as f32, plate_size[1] as f32],
            _pad0: [0.0; 2],
            bloom_color,
            corner_radius,
            bloom_radius,
            rim_thickness,
            rim_intensity,
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
    _pad0:         vec2<f32>,
    bloom_color:   vec4<f32>,
    corner_radius: f32,
    bloom_radius:  f32,
    rim_thickness: f32,
    rim_intensity: f32,
    model:         mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var plate_tex: texture_2d<f32>;
@group(0) @binding(2) var plate_samp: sampler;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    /// Fragment position in plate-local pixel space. Inside the plate
    /// this is `0..plate_size`; outside (in the bloom halo) it extends up
    /// to `plate_size + bloom_radius` on each side.
    @location(0) plate_local: vec2<f32>,
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

// Signed distance to an axis-aligned rounded rectangle whose top-left is at
// (0, 0) and size is `s`, with corner radius `r`. Negative inside, zero on
// the edge, positive outside.
fn sdf_rounded_box(p: vec2<f32>, s: vec2<f32>, r: f32) -> f32 {
    let centered = p - s * 0.5;
    let q = abs(centered) - s * 0.5 + vec2<f32>(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - r;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let corner = corner_for(vi);
    // Expand the quad by bloom_radius on every side so the halo has room
    // to render outside the plate's bounds. In plate-local pixel space,
    // corner (0,0) becomes (-bloom, -bloom) and (1,1) becomes
    // (plate_size + bloom).
    let b = u.bloom_radius;
    let local = -vec2<f32>(b, b) + corner * (u.plate_size + vec2<f32>(b * 2.0, b * 2.0));
    // Apply model matrix (identity today; rotations rotate the plate later).
    let transformed = u.model * vec4<f32>(local, 0.0, 1.0);
    // Translate into window space.
    let screen = transformed.xy + u.plate_pos;
    // Convert to NDC (y flipped: screen space origin is top-left).
    let ndc_x = (screen.x / u.viewport_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (screen.y / u.viewport_size.y) * 2.0;

    var out: VsOut;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, transformed.z, 1.0);
    out.plate_local = local;
    return out;
}

// Plate interior: a lit-material background. Called per-pixel when we're
// inside the plate's rounded silhouette. Returns premultiplied RGBA.
//
// Components (Zone-2 of the visual grammar):
//   - Vertical gradient (subtly brighter top, dimmer bottom) — implies light
//     falling from above.
//   - Radial gradient (brighter toward plate center) — reads as a luminous
//     material rather than flat paint.
//   - Top-edge rim light hugging the rounded corners — the strongest cue
//     that the plate is lit, not printed.
//   - Tinted-glass translucency so the nebula reads through behind.
// Linen weave — rhythmic cross-hatch of horizontal + vertical threads with
// tiny weave holes at their intersections. Output is a "material presence"
// scalar: 1.0 where a thread crosses this pixel, 0.0 in a pure hole, smooth
// transitions in between.
//
// Thread spacing is in physical pixels so the weave reads at consistent
// scale regardless of viewport size. Slight per-thread noise adds a hint
// of non-ideal handmade cloth: thread edges wobble, not perfectly straight.
//
// Unused by M3.3.1 Pass 1 but left here for future use: the decoupled-
// opacity contract (diffuse light uniformly, point light through holes
// only) will sample this same function to drive two different blend rules.
fn linen_weave(p_px: vec2<f32>) -> f32 {
    // One warp/weft cycle every N pixels. ~4px gives visible weave pattern
    // on a typical 1x-DPI display; on Retina the weave looks finer but
    // still clearly woven.
    let thread_spacing = 4.2;

    // Slight per-row noise to wobble thread edges — "non-ideal" cloth.
    let row = floor(p_px.y / thread_spacing);
    let col = floor(p_px.x / thread_spacing);
    let row_jitter = (fract(sin(row * 12.989) * 43758.55) - 0.5) * 0.6;
    let col_jitter = (fract(sin(col * 78.233) * 43758.55) - 0.5) * 0.6;

    // Position within the thread cycle.
    let h_phase = (p_px.y / thread_spacing + col_jitter * 0.05) * 6.28318;
    let v_phase = (p_px.x / thread_spacing + row_jitter * 0.05) * 6.28318;
    let h_wave = abs(sin(h_phase));
    let v_wave = abs(sin(v_phase));

    // Thread occupies the lower part of each cycle (near zero-crossing).
    // Smoothstep gives the soft anti-aliased thread edge.
    let thread_edge_inner = 0.20;
    let thread_edge_outer = 0.42;
    let h = smoothstep(thread_edge_outer, thread_edge_inner, h_wave);
    let v = smoothstep(thread_edge_outer, thread_edge_inner, v_wave);

    // Thread wherever either direction has a thread — union, not intersect.
    // The holes are the small regions where NEITHER thread is present.
    return max(h, v);
}

fn plate_interior(p: vec2<f32>, s: vec2<f32>, d: f32, rim_thickness: f32, rim_intensity: f32) -> vec4<f32> {
    let uv = p / s;

    // Vertical gradient — linen has natural warm-ivory; the plate picks up
    // cooler blue at the top (lit from above) and warmer cream at the
    // bottom. Difference is small so the overall tint stays consistent.
    let base_top    = vec3<f32>(0.080, 0.085, 0.110);
    let base_bottom = vec3<f32>(0.068, 0.060, 0.050);
    var rgb = mix(base_top, base_bottom, clamp(uv.y, 0.0, 1.0));

    // Radial gradient — brighter toward center, fades to the base by ~70%
    // of the plate's half-diagonal.
    let centered = uv - vec2<f32>(0.5, 0.5);
    let r = length(centered);
    let radial = smoothstep(0.70, 0.05, r);
    rgb = rgb + vec3<f32>(0.026, 0.030, 0.034) * radial;

    // Top-edge rim light. Uses SDF inside-depth (`-d`, positive inside) so
    // the rim follows the rounded corners naturally. Gated by a vertical
    // factor so only the top half of the plate lights up (light from above).
    let inside_depth = max(-d, 0.0);
    let rim_falloff = 1.0 - smoothstep(0.0, rim_thickness, inside_depth);
    let upperness = smoothstep(s.y * 0.55, 0.0, p.y);
    let rim = rim_falloff * upperness * rim_intensity;
    rgb = rgb + vec3<f32>(0.14, 0.18, 0.28) * rim;

    // Linen weave: modulates per-pixel alpha. Threads mostly opaque; weave
    // holes transparent so sky + lightnings read through them. A small
    // baseline keeps the plate visible even in pure-hole pixels so it
    // doesn't look like the material has been punched out.
    let weave = linen_weave(p);
    let thread_alpha = 0.82;
    let hole_alpha   = 0.06;
    let alpha = mix(hole_alpha, thread_alpha, weave);

    return vec4<f32>(rgb * alpha, alpha);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // SDF to the plate's rounded-rect silhouette. Negative inside, zero
    // on the edge, positive outside.
    let d = sdf_rounded_box(in.plate_local, u.plate_size, u.corner_radius);

    // Sample the plate RT (the card content) only where we're inside its
    // rectangular bounds. Outside, treat as transparent.
    var tex = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    let plate_uv = in.plate_local / u.plate_size;
    let inside_bounds = plate_uv.x >= 0.0 && plate_uv.x <= 1.0
                     && plate_uv.y >= 0.0 && plate_uv.y <= 1.0;
    if (inside_bounds) {
        tex = textureSample(plate_tex, plate_samp, plate_uv);
    }

    // Plate interior background (lit material) — computed whenever inside
    // the silhouette. Outside the silhouette, the plate mask zeroes this out.
    let bg = plate_interior(in.plate_local, u.plate_size, d, u.rim_thickness, u.rim_intensity);

    // Plate silhouette mask with 1-pixel AA band across the rounded edge.
    // At d < -0.5 (firmly inside) mask = 1; at d > 0.5 (firmly outside) = 0.
    let plate_mask = smoothstep(0.5, -0.5, d);

    // Inside the plate: cards over the lit background.
    let plate_rgb = (tex.rgb + bg.rgb * (1.0 - tex.a)) * plate_mask;
    let plate_a   = (tex.a   + bg.a   * (1.0 - tex.a)) * plate_mask;

    // Outside the plate: bloom halo. Quadratic falloff based on SDF
    // distance. Masked by (1 - plate_mask) so it doesn't double up with
    // the plate interior inside the edge.
    let bloom_t = clamp(1.0 - max(d, 0.0) / u.bloom_radius, 0.0, 1.0);
    let bloom_alpha = bloom_t * bloom_t * u.bloom_color.a * (1.0 - plate_mask);
    let bloom_rgb = u.bloom_color.rgb * bloom_alpha;  // premultiplied

    return vec4<f32>(plate_rgb + bloom_rgb, plate_a + bloom_alpha);
}
"#;
