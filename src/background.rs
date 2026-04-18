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

// Fullscreen-triangle VS (no vertex buffer).
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    let clip = vec2<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0);
    out.pos = vec4<f32>(clip, 0.0, 1.0);
    out.uv = vec2<f32>(x, 1.0 - y);
    return out;
}

// ---- Value noise + fBm ----------------------------------------------------
//
// We want ORGANIC spatial variation, not sine-lattice patterns. Value noise
// at a few octaves (fBm) gives cloud/nebula-shaped patches without visibly
// regular structure. fBm is fractal: each octave adds finer detail on top of
// the large shapes from the previous octave.

fn hash12(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 = p3 + dot(p3, p3.yzx + vec3<f32>(33.33));
    return fract((p3.x + p3.y) * p3.z);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let a = hash12(i);
    let b = hash12(i + vec2<f32>(1.0, 0.0));
    let c = hash12(i + vec2<f32>(0.0, 1.0));
    let d = hash12(i + vec2<f32>(1.0, 1.0));
    // Hermite smoothing — no visible grid artefacts.
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// 3-octave fBm. `p` is any 2D position; bigger values give finer detail.
fn fbm(p: vec2<f32>) -> f32 {
    var total = 0.0;
    var amp = 0.55;
    var freq = 1.0;
    for (var i = 0; i < 3; i = i + 1) {
        total = total + amp * value_noise(p * freq);
        freq = freq * 2.1;
        amp  = amp * 0.55;
    }
    return total;  // roughly 0..1 range
}

// ---- Star field (M3.8.1) --------------------------------------------------
//
// Sparse pinpoints distributed across the void on a hash-based grid. Each
// grid cell gets zero or one star with a randomized center offset, baseline
// brightness, and twinkle phase. Most cells are empty or very dim; a few
// rare cells fire a nova-pulse (5–10× peak, ~1.5 s envelope) every ~30 s
// within their own lifecycle.
//
// Output: scalar brightness for this pixel. The fs_main composes it onto
// the nebula with a starlight-blue tint. Future M3.3.1 (linen): when the
// plate renders, its weave-hole alpha gates these pinpoints so only stars
// behind holes punch through — that's the decoupled-opacity contract. For
// now (no plate yet at swap-chain level), stars appear everywhere.

fn star_field(uv: vec2<f32>, t: f32) -> f32 {
    // Grid density: ~90 cells wide across the viewport. Non-square cells
    // (scaled by aspect) would distort twinkle timing, so uv is already
    // aspect-corrected by the caller.
    let grid = 90.0;
    let cell_uv = uv * grid;
    let cell = floor(cell_uv);
    let local = fract(cell_uv);

    // Most cells are empty. Keep ~30% populated — higher density means more
    // visual noise; lower, and there are long blank stretches.
    let presence = hash12(cell + vec2<f32>(101.7, 37.1));
    if (presence < 0.70) {
        return 0.0;
    }

    // Per-star center offset within the cell (keeps stars from lining up on
    // the grid). Range [0.2, 0.8] so pinpoints sit away from cell edges.
    let cx = hash12(cell + vec2<f32>(17.3, 42.1));
    let cy = hash12(cell + vec2<f32>(81.9, 9.7));
    let center = vec2<f32>(0.2 + cx * 0.6, 0.2 + cy * 0.6);
    let d = distance(local, center);

    // Gaussian falloff. Star size is a small fraction of the cell so each
    // star is a pinpoint, not a disc.
    let star_sigma = 0.035;
    let glow = exp(-(d * d) / (star_sigma * star_sigma));

    // Baseline brightness: most stars dim, a few moderately bright.
    let bright_roll = hash12(cell + vec2<f32>(5.1, 23.9));
    let baseline = 0.08 + pow(bright_roll, 3.0) * 0.22;

    // Slow twinkle — per-star phase, modest amplitude. Shouldn't catch the
    // eye on its own; it's "the stars feel alive."
    let phase = hash12(cell + vec2<f32>(55.7, 13.2)) * 6.28318;
    let twinkle = 0.75 + 0.25 * sin(t * 0.9 + phase);

    // Nova-pulse. Rare cells (~0.5%) are potential novae. Each such cell
    // cycles through a 32-second lifecycle with a ~1.5s active window; so
    // across the viewport (thousands of cells, of which ~0.5% nova-capable)
    // you see roughly one nova flash every 20–40 seconds somewhere.
    let nova_roll = hash12(cell + vec2<f32>(211.3, 77.7));
    var nova = 0.0;
    if (nova_roll > 0.995) {
        let offset = hash12(cell + vec2<f32>(3.3, 88.8)) * 32.0;
        let cycle = 32.0;
        let phase_t = (t + offset) - cycle * floor((t + offset) / cycle);
        if (phase_t < 1.5) {
            // Smooth bump centred on t = 0.75: quick rise, gentler decay.
            let nt = phase_t / 1.5;
            let envelope = 4.0 * nt * (1.0 - nt);
            nova = envelope * 5.0;  // 5× baseline boost at peak
        }
    }

    return glow * baseline * (twinkle + nova);
}

// --------------------------------------------------------------------------

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let px = in.uv * u.viewport_size;
    let t = u.time_seconds;

    // Aspect-correct coords so features are circular on wide/tall windows.
    let aspect = u.viewport_size.x / max(u.viewport_size.y, 1.0);
    let auv    = vec2<f32>(in.uv.x * aspect, in.uv.y);

    // ---- Near-black base so nebula layers rise above it visibly. ----
    let base = vec3<f32>(0.010, 0.010, 0.018);

    // ---- Deep-space palette. Wider than before so the slow hue layer has
    // real diapason: when it shifts, it's noticeable ("was blue, now green"). ----
    let violet   = vec3<f32>(0.090, 0.035, 0.130);
    let indigo   = vec3<f32>(0.020, 0.035, 0.120);
    let magenta  = vec3<f32>(0.090, 0.030, 0.075);
    let teal     = vec3<f32>(0.020, 0.065, 0.080);
    let aurora_g = vec3<f32>(0.020, 0.095, 0.060);   // new: aurora green
    let ember    = vec3<f32>(0.095, 0.045, 0.020);   // new: rare warm ember

    // ---- Principle: slow layers have WIDE range; fast layers have NARROW
    // range. The slow "epoch" picks from a broad palette (big changes over
    // minutes); the fast cloud layers drift visibly but stay within a
    // narrow amplitude. This is how the eye reads "volumetric" rather than
    // "one thing moving in front of a static backdrop." ----

    // ===== SLOW EPOCH (wide palette) =====
    // Two fBm samples pick primary + accent hues from the full palette.
    // Periods ~1–2 min. When you come back later, the sky has noticeably
    // shifted mood.
    let epoch_a_uv = auv * 0.75 + vec2<f32>(t * 0.030, t * 0.022);
    let epoch_b_uv = auv * 0.85 + vec2<f32>(-t * 0.025, t * 0.033);
    let epoch_a = fbm(epoch_a_uv);
    let epoch_b = fbm(epoch_b_uv);

    // Primary: four-stop blend across violet→indigo→teal→aurora-green.
    // Using nested mix+smoothstep lets the primary go through the full
    // arc — not just A↔B — so "it was blue, now green" is possible.
    let p_01 = mix(violet,  indigo,    smoothstep(0.20, 0.45, epoch_a));
    let p_12 = mix(p_01,    teal,      smoothstep(0.40, 0.62, epoch_a));
    let primary = mix(p_12, aurora_g,  smoothstep(0.60, 0.85, epoch_a));

    // Accent: three-stop, rarer ember on the hot end.
    let a_01 = mix(magenta, teal,      smoothstep(0.25, 0.55, epoch_b));
    let accent = mix(a_01,  ember,     smoothstep(0.75, 0.90, epoch_b));

    // ===== THREE CLOUD LAYERS at different depths =====
    // Each has its own scale, speed, direction, AND slight color bias — so
    // the eye reads them as distinct atmospheric depths, not one blob.

    // FAR CLOUDS: large, soft, slow. Broad diffuse patches in the distance.
    let far_uv = auv * 1.7 + vec2<f32>(t * 0.040, -t * 0.030);
    let far    = smoothstep(0.40, 0.80, fbm(far_uv));
    // Tinted toward primary (the deep-space mood color), slightly dimmer.
    let far_color = primary * 0.9;

    // MID CLOUDS: the main kinetic layer. Medium scale, medium speed.
    // Visibly moves across the window — reads as "clouds drifting through."
    let mid_uv = auv * 3.2 + vec2<f32>(-t * 0.090, t * 0.070);
    let mid    = smoothstep(0.45, 0.85, fbm(mid_uv));
    // Blend primary + accent — mid clouds are where hue variation is
    // most visible.
    let mid_color = mix(primary, accent, 0.55);

    // NEAR CLOUDS: small, fast, crisp-edged. Reads as "wisps close to the
    // camera." Lower amplitude so they don't dominate.
    let near_uv = auv * 7.0 + vec2<f32>(t * 0.180, t * 0.135);
    let near    = smoothstep(0.55, 0.92, fbm(near_uv)) * 0.55;
    // Near clouds bias toward accent — visually distinct from far clouds
    // even in the same frame.
    let near_color = accent * 1.1;

    // FINE TURBULENCE: surface shimmer, not a cloud layer — just texture.
    let turb_uv = auv * 12.0 + vec2<f32>(-t * 0.280, t * 0.210);
    let turb = fbm(turb_uv) * 0.12;

    // Compose. Three cloud layers sum additively because in real nebulae,
    // atmospheric emission stacks. Peak composite ≈ 0.18 — visible but
    // keeps the sky comfortably dim.
    let nebula = far_color  * far  * 0.55
               + mid_color  * mid  * 0.75
               + near_color * near * 0.60
               + vec3<f32>(turb, turb, turb) * 0.25;

    // ---- Star field (M3.8.1). Pinpoint starlight tinted blue-white; nova
    // pulses inherit the same tint but spike through the cloud layers.
    // Computed in aspect-corrected UV so stars are circular not stretched.
    // When M3.3.1 lands, this same layer will be gated by the plate's
    // linen-weave alpha so only stars behind holes punch through. ----
    let star_tint = vec3<f32>(0.85, 0.92, 1.00);
    let stars = star_field(auv, t) * star_tint;

    // ---- Extremely faint pixel grain breaks up uniform regions. ----
    let grain = (hash12(floor(px / 2.0)) - 0.5) * 0.006;

    // ---- Aspect-corrected radial vignette. ----
    let centered = vec2<f32>((in.uv.x - 0.5) * aspect, in.uv.y - 0.5);
    let r = length(centered) / 0.75;
    let vignette = 1.0 - smoothstep(0.30, 1.15, r) * 0.42;

    // ---- Compose ----
    var color = (base + nebula) * vignette
              + stars
              + vec3<f32>(grain, grain, grain);
    return vec4<f32>(color, 1.0);
}
"#;
