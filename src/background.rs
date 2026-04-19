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

use crate::sky::SkyLight;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport_size: [f32; 2],
    time_seconds: f32,
    sky_intensity: f32,
    /// Unit vector toward the unseen dominant star; xyz. `w` is padding.
    sky_direction: [f32; 4],
    /// Sky color temperature (linear RGB); rgb. `a` is padding.
    sky_color: [f32; 4],
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
        sky: SkyLight,
    ) {
        let u = Uniforms {
            viewport_size: [viewport_size.0 as f32, viewport_size.1 as f32],
            time_seconds,
            sky_intensity: sky.intensity,
            sky_direction: [sky.direction.x, sky.direction.y, sky.direction.z, 0.0],
            sky_color: [sky.color.x, sky.color.y, sky.color.z, 0.0],
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
    sky_intensity: f32,
    sky_direction: vec4<f32>,
    sky_color: vec4<f32>,
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

// ---- Nebula lightnings (M3.8.1) -------------------------------------------
//
// Not stars. Brief electrical-looking arcs threading through the nebula —
// think plasma discharges, curving energy flows along invisible magnetic
// field lines. The ethos:
//
//   - Only visible during their brief active window (~0.5 s per strike).
//     No persistent pinpoints — persistence reads as "dirt on the screen."
//   - Short arc (5–10 px total) — pinpoint-small, not sparkler.
//   - Mostly tinted by the *local nebula colour* so each strike feels like
//     a property of the cloud it lives in. Rare outliers (cyan, aurora
//     green, magenta, electric lilac) for visual variety.
//   - Bright near-white core, tinted halo — matches how any hot light
//     source looks: the middle is always white-hot, the edges colour.
//
// Form: quadratic bezier approximated as a two-segment polyline (p0-p1-p2).
// Subtle curvature — gentle arcs, not jagged terrestrial lightning (that
// would clash with the calm cosmic-fairytale dialect).
//
// Output: premultiplied-ish (color * intensity, intensity). Caller adds
// this additively to the sky.
//
// Future M3.3.1 (linen): the plate shader will gate this as a sharp
// point-light layer — strikes behind linen threads are blocked, strikes
// behind weave holes punch through.

fn dist_to_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let ap = p - a;
    let t = clamp(dot(ap, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return distance(p, a + ab * t);
}

fn nebula_lightning(uv: vec2<f32>, t: f32, local_tint: vec3<f32>) -> vec4<f32> {
    // Coarse grid — strikes are sparse by design.
    let grid = 45.0;
    let cell_uv = uv * grid;
    let cell = floor(cell_uv);
    let local = fract(cell_uv);

    // ~4% of cells are strike-capable. With 45×(45*aspect) cells you get
    // roughly 130–180 lightning-capable sites; combined with 20–40 s cycles
    // and a 0.5 s active window this gives ~1–2 strikes visible somewhere
    // at any given moment — rare-feeling but not sparse.
    let capable = hash12(cell + vec2<f32>(211.3, 77.7));
    if (capable < 0.96) { return vec4<f32>(0.0); }

    // Cycle length 20–40 s per cell, desynchronised.
    let cycle = 20.0 + hash12(cell + vec2<f32>(3.7, 9.1)) * 20.0;
    let offset = hash12(cell + vec2<f32>(47.1, 88.5)) * cycle;
    let phase_t = (t + offset) - cycle * floor((t + offset) / cycle);

    // Active window 0.5 s — short flash, not a lingering glow.
    let strike_len = 0.5;
    if (phase_t >= strike_len) { return vec4<f32>(0.0); }

    // Envelope: quick rise, fast decay. Peaks around t = 0.25.
    let nt = phase_t / strike_len;
    let envelope = pow(nt, 0.35) * pow(1.0 - nt, 1.6) * 3.2;

    // Strike geometry: two endpoints + a mid control point perpendicular to
    // the line, giving a gentle curve. Arc length 0.18–0.32 of the cell →
    // ~4–8 px on typical viewports.
    let p0_x = hash12(cell + vec2<f32>(17.3, 42.1));
    let p0_y = hash12(cell + vec2<f32>(81.9, 9.7));
    let p0 = vec2<f32>(0.20 + p0_x * 0.25, 0.20 + p0_y * 0.25);
    let angle = hash12(cell + vec2<f32>(55.7, 13.2)) * 6.28318;
    let length = 0.18 + hash12(cell + vec2<f32>(11.1, 99.9)) * 0.14;
    let dir = vec2<f32>(cos(angle), sin(angle));
    let p2 = p0 + dir * length;
    let perp = vec2<f32>(-dir.y, dir.x);
    let bend = (hash12(cell + vec2<f32>(7.3, 3.9)) - 0.5) * length * 0.35;
    let p1 = (p0 + p2) * 0.5 + perp * bend;

    // Distance to the two-segment polyline approximation of the bezier.
    let d = min(dist_to_segment(local, p0, p1), dist_to_segment(local, p1, p2));

    // Core sigma ~1–2 px; halo sigma ~3 px.
    let core_sigma = 0.012;
    let halo_sigma = 0.028;
    let core = exp(-(d * d) / (core_sigma * core_sigma));
    let halo = exp(-(d * d) / (halo_sigma * halo_sigma));
    let intensity = (core * 0.7 + halo * 0.30) * envelope;

    // Colour: mostly the nebula's local tint; rare outliers pick from a
    // small "electrical spectrum" for variety. The CORE is always pushed
    // toward white — hot light, desaturated; the HALO carries the tint.
    let outlier_roll = hash12(cell + vec2<f32>(91.1, 44.7));
    var tint = local_tint * 2.2;  // boost the nebula tint so it reads vivid
    if (outlier_roll > 0.80) {
        let which = floor(hash12(cell + vec2<f32>(123.4, 56.7)) * 4.0);
        if      (which < 1.0) { tint = vec3<f32>(0.35, 0.85, 1.00); }  // cyan
        else if (which < 2.0) { tint = vec3<f32>(0.35, 0.95, 0.55); }  // aurora green
        else if (which < 3.0) { tint = vec3<f32>(0.95, 0.40, 0.85); }  // magenta
        else                  { tint = vec3<f32>(0.80, 0.72, 1.00); }  // electric lilac
    }
    // Core weight: where the core gaussian dominates over halo → push white.
    let core_weight = core / max(core + halo * 0.5 + 1e-4, 1e-4);
    let color = mix(tint, vec3<f32>(1.0, 1.0, 1.0), core_weight * 0.75);

    return vec4<f32>(color * intensity, intensity);
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
    // Near clouds lean *neutral*, not cosmic — they're the layer closest
    // to the viewer/star, so they should take most of their color from
    // the current sky (applied in the blend below) rather than from the
    // deep-space palette. A dim neutral base means even at night (when
    // star response → 0) they stay restrained and don't dominate.
    let near_color = mix(accent * 1.1, vec3<f32>(0.35, 0.35, 0.40), 0.55);

    // FINE TURBULENCE: surface shimmer, not a cloud layer — just texture.
    let turb_uv = auv * 12.0 + vec2<f32>(-t * 0.280, t * 0.210);
    let turb = fbm(turb_uv) * 0.12;

    // ---- SkyLight blend per cloud layer.
    //
    // Each layer INTERPOLATES from its intrinsic cosmic color toward the
    // sky's current color, proportional to `star_response × intensity`.
    // Multiplicative tinting preserved the cosmic palette even at high
    // response (magenta × warm-orange = muddy brown); `mix` replaces the
    // cosmic palette with the sky color as response rises, which gives
    // the near layers their "sunlit clouds at noon, deep-red at dusk"
    // reading. Far layers keep a low response so they stay cosmic —
    // distant clouds don't take much light from the nearby star.
    //
    // Response coefficients:
    //   far  ~0.15  — barely sky-colored, always cosmic
    //   mid  ~0.55  — half-and-half at peak intensity
    //   near ~0.90  — dominant sky color when the sun is out
    let sky_bright = u.sky_color.rgb * 1.5;
    let far_final  = mix(far_color,  sky_bright, 0.15 * u.sky_intensity);
    let mid_final  = mix(mid_color,  sky_bright, 0.55 * u.sky_intensity);
    let near_final = mix(near_color, sky_bright, 0.90 * u.sky_intensity);

    // Compose. Three cloud layers sum additively because in real nebulae,
    // atmospheric emission stacks. Peak composite ≈ 0.18 — visible but
    // keeps the sky comfortably dim.
    var nebula = far_final  * far  * 0.55
               + mid_final  * mid  * 0.75
               + near_final * near * 0.60
               + vec3<f32>(turb, turb, turb) * 0.25;

    // Overall brightness modulated by SkyLight.intensity: ~45% at night,
    // 100% at noon. Wide diapason so you can see the day/night cycle in
    // the sky itself, not only in the reflections.
    let brightness = 0.45 + 0.55 * u.sky_intensity;
    nebula = nebula * brightness;

    // ---- Nebula lightnings (M3.8.1). Short curved arcs, bright white core
    // with tinted halo. Mostly tinted by the local nebula colour so each
    // strike feels atmospheric; ~20% are spectrum outliers for variety.
    // Computed in aspect-corrected UV so arcs are circular not stretched.
    // When M3.3.1 lands, this layer will be gated by the plate's linen-
    // weave alpha so only strikes behind holes punch through. ----
    let local_tint = mix(primary, accent, 0.5);
    let lightning = nebula_lightning(auv, t, local_tint);

    // ---- Extremely faint pixel grain breaks up uniform regions. ----
    let grain = (hash12(floor(px / 2.0)) - 0.5) * 0.006;

    // ---- Aspect-corrected radial vignette. ----
    let centered = vec2<f32>((in.uv.x - 0.5) * aspect, in.uv.y - 0.5);
    let r = length(centered) / 0.75;
    let vignette = 1.0 - smoothstep(0.30, 1.15, r) * 0.42;

    // ---- Compose ----
    var color = (base + nebula) * vignette
              + lightning.rgb
              + vec3<f32>(grain, grain, grain);
    return vec4<f32>(color, 1.0);
}
"#;
