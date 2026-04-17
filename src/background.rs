//! Ambient sky background — a single fullscreen-quad pipeline whose fragment
//! shader computes a slow-breathing, cloud-drifting, lightly vignetted void.
//!
//! **M3.8 tuning goal**: the void should give the canvas *soul* — visibly
//! alive, slightly mysterious, never distracting. "Soul" here comes from
//! four independent layers:
//!
//! 1. A slowly-drifting two-scale cloud field — the void has *depth* and
//!    *variation*, not flat paint. You shouldn't see a "cloud shape," just
//!    sense there's *something* varying.
//! 2. A breathing pulse that slightly modulates brightness and hue — the sky
//!    *inhales and exhales*. Periods are minutes, not seconds, so the eye
//!    never catches the loop.
//! 3. A radial vignette that darkens toward the edges — gives the void a
//!    *center of gravity*. Plates floating near the middle read as "resting
//!    in the focus of the frame."
//! 4. Fine noise at pixel granularity breaks up uniform regions.
//!
//! Amplitudes are still below 6% of the total dynamic range. The user never
//! consciously notices any single element — they notice that the *canvas is
//! alive* in a way it wasn't before.
//!
//! Rendered first each frame (before shapes + text + egui), with LoadOp::Clear
//! overwritten by the shader output — so we don't need a solid clear.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport_size: [f32; 2],
    time_seconds: f32,
    _pad: f32,
}

pub struct BackgroundRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
}

impl BackgroundRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ygg-background-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-background-uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ygg-bg-bgl"),
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
            label: Some("ygg-bg-bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ygg-bg-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ygg-bg-pipeline"),
            layout: Some(&layout),
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
                    // No blend: we overwrite the whole framebuffer with the sky.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        Self { pipeline, bind_group, uniform_buffer }
    }

    pub fn prepare(
        &self,
        queue: &wgpu::Queue,
        viewport_size: (u32, u32),
        time_seconds: f32,
    ) {
        let u = Uniforms {
            viewport_size: [viewport_size.0 as f32, viewport_size.1 as f32],
            time_seconds,
            _pad: 0.0,
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&u));
    }

    pub fn render<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1); // oversized triangle covers the viewport
    }
}

const SHADER: &str = r#"
struct Uniforms {
    viewport_size: vec2<f32>,
    time_seconds: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// A single oversized triangle that covers the clip-space square. Skips
// needing a vertex buffer or index buffer — classic fullscreen-quad trick.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    let clip = vec2<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0);
    out.pos = vec4<f32>(clip, 0.0, 1.0);
    // UV in [0, 1] with origin top-left (y grows downward in pixel space).
    out.uv = vec2<f32>(x, 1.0 - y);
    return out;
}

// 2D hash — cheap noise for a very subtle grain at low amplitude.
fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 = p3 + dot(p3, p3.yzx + vec3<f32>(33.33));
    return fract((p3.x + p3.y) * p3.z);
}

// Two-octave cloud-field: two sine lattices at different frequencies,
// multiplied together. Each lattice drifts at its own slow rate, creating
// apparent depth (parallax-like motion between scales).
fn cloud_field(uv: vec2<f32>, t: f32) -> f32 {
    // Slow-drift large cloud layer.
    let drift_a = vec2<f32>(sin(t * 0.031) * 0.18, cos(t * 0.023) * 0.12);
    let uv_a = uv + drift_a;
    let a = (sin(uv_a.x * 2.1) * 0.5 + 0.5)
          * (sin(uv_a.y * 1.5 + 1.2) * 0.5 + 0.5);

    // Faster, smaller-scale cloud layer drifting the other way — breaks the
    // regularity of a single sine lattice.
    let drift_b = vec2<f32>(cos(t * 0.047 + 0.9) * 0.24, sin(t * 0.037) * 0.18);
    let uv_b = uv + drift_b;
    let b = (sin(uv_b.x * 4.3 - 0.5) * 0.5 + 0.5)
          * (sin(uv_b.y * 3.1 + 2.2) * 0.5 + 0.5);

    // Weighted blend — large layer dominates, small layer adds texture.
    return mix(a, b, 0.35);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let px = in.uv * u.viewport_size;
    let t = u.time_seconds;

    // ---- Base palette: deep blue-violet void with a hint of warmth ----
    // A touch brighter than pre-M3.8 to give subsequent layers room to
    // modulate visibly without crushing to black.
    let base_top    = vec3<f32>(0.028, 0.031, 0.052);  // cool blue-violet at top
    let base_bottom = vec3<f32>(0.038, 0.030, 0.048);  // slightly warmer, a hair redder at bottom
    let base_color  = mix(base_top, base_bottom, in.uv.y * 0.70);

    // ---- Breathing: two slow sine pulses, out of phase. ----
    // Period ~25-40s — longer than a held breath, so conscious perception
    // dissolves. Amplitude ~4% on brightness + a tiny hue shift.
    let breath_bright = 1.0
        + 0.028 * sin(t * 0.22)
        + 0.014 * sin(t * 0.13 + 1.9);
    // Subtle hue breathing: a very small shift toward warm/cool that the
    // eye reads as "the sky is alive," not as a color change.
    let hue_shift = 0.010 * sin(t * 0.08 + 0.4);
    let hue_bias  = vec3<f32>(hue_shift * 0.6, 0.0, -hue_shift * 0.8);

    // ---- Cloud field — adds depth and variation. ----
    // Tinted slightly toward violet so clouds read as "atmosphere," not as
    // visible shapes. Amplitude ~3% of total range.
    let cloud = cloud_field(in.uv, t);
    let cloud_tint = vec3<f32>(0.022, 0.024, 0.044) * cloud * 1.0;

    // ---- Fine noise at pixel granularity. ----
    let n = (hash21(floor(px / 2.0)) - 0.5) * 0.010;

    // ---- Radial vignette: center of gravity for the void. ----
    // Use aspect-corrected coordinates so the vignette is circular, not
    // elliptical on wide windows.
    let aspect = u.viewport_size.x / max(u.viewport_size.y, 1.0);
    let centered = vec2<f32>((in.uv.x - 0.5) * aspect, in.uv.y - 0.5);
    let r = length(centered) / 0.75;          // normalize so ~1.0 at corners
    let vignette = 1.0 - smoothstep(0.35, 1.2, r) * 0.28;

    // ---- Compose ----
    var color = (base_color + hue_bias) * breath_bright + cloud_tint + vec3<f32>(n, n, n);
    color = color * vignette;
    return vec4<f32>(color, 1.0);
}
"#;
