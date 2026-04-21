//! 3D slat pipeline — YGG-62 Phase 2 (real 3D, no fakes).
//!
//! Each slat is a **tessellated curved strip**: N vertical slices
//! across its short axis, with a concave parabolic arc adding depth
//! to the middle of the strip. Sides of the slat stay perfectly
//! vertical; it's the middle-of-face that recedes in Z.
//!
//! The strip is rendered with a real one-point perspective matrix
//! whose vanishing point is anchored at the monitor's
//! physical-screen center-top (see `build_projection_matrix` and
//! `AppState::projection_anchor`). Points at z = 0 project to their
//! natural (x, y) on screen; points with positive z (arc-depth
//! middle of slat) are foreshortened slightly toward the anchor.
//!
//! The visible effect is subtle with small arc depths at monitor-
//! scale focal distances — that's honest physics, not a bug. Tune
//! `arc_depth` per slat if a more pronounced curve is desired; fold
//! animations will drive this value per-slat in later phases.
//!
//! ## Mesh
//!
//! Shared across all slats:
//! * `N_STRIPS = 8` strips across the vertical axis.
//! * `2 * (N_STRIPS + 1) = 18` vertices (left and right at each
//!   horizontal row).
//! * `2 * N_STRIPS = 16` triangles (`48` indices).
//!
//! Per-slat state (instance buffer): model matrix + color + size /
//! corner / arc-depth.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

/// Number of vertical strips per slat mesh. Higher = smoother arc
/// at the cost of more triangles. 8 is plenty for a 23-px-tall slat.
const N_STRIPS: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    /// Slat-local position in [0, 1]^2 — scaled by the model matrix
    /// to world-space pixels.
    pos: [f32; 3],
    /// Slat-local uv in [0, 1]^2. x=0 is the left side, x=1 the
    /// right; y=0 is the top, y=1 the bottom.
    uv: [f32; 2],
}

/// Per-slat data uploaded to the instance buffer.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SlatInstance {
    /// Column-major model matrix: slat-local → world pixels. Just
    /// scale + translate — no rotation, no fake depth. Arc is
    /// applied in the vertex shader after this matrix.
    pub model: [[f32; 4]; 4],
    /// Slat face color (straight, not pre-multiplied).
    pub color: [f32; 4],
    /// Slat natural size in physical pixels (x = width, y = height).
    pub size_px: [f32; 2],
    /// Corner radius (physical pixels) for the rounded-rect SDF.
    pub corner_radius: f32,
    /// Arc depth (physical pixels). Max z offset added at v = 0.5
    /// for a concave parabolic arc. 0 = perfectly flat slat.
    pub arc_depth: f32,
    /// Hole parameters in SLAT-LOCAL pixel space.
    /// `xy` = hole center; `zw` = hole half-width + half-height.
    /// Fragment shader discards fragments inside the elliptical hole,
    /// giving a real cut-through that the rope (drawn behind) shows
    /// through. Set all-zero to disable (slats without holes).
    pub hole: [f32; 4],
    /// Text region in SLAT-LOCAL pixel space — `xy` = top-left,
    /// `zw` = width/height. Fragment shader samples the text atlas
    /// only inside this region, leaving the rest of the slat bare.
    /// Set all-zero to disable text on this slat.
    pub text_rect_px: [f32; 4],
    /// Text region in ATLAS uv space — `xy` = (u_min, v_min),
    /// `zw` = (u_max, v_max). Slat-local text_rect_px maps into
    /// this atlas sub-rect, so the text keeps its rendered pixel
    /// size (no stretching).
    pub atlas_sub: [f32; 4],
    /// Ink color (straight rgba). Fragment mixes this over the slat
    /// color using the atlas alpha as opacity.
    pub ink_color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    projection: [[f32; 4]; 4],
    viewport_size: [f32; 2],
    _pad0: [f32; 2],
    /// Unit vector pointing toward the unseen dominant star, in the
    /// same world-space convention as `model` (y DOWN, z INTO screen).
    /// `w` is unused padding.
    sky_direction: [f32; 4],
    /// `xyz` = linear-space sky colour, `w` = intensity 0..1. Slat
    /// shading multiplies diffuse Lambert by intensity and mixes a
    /// fraction of the colour into the slat tint so slats warm at
    /// dawn / cool at noon / dim at night alongside everything else.
    sky_color_intensity: [f32; 4],
}

pub struct Slat3DRenderer {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Bind group 1: text atlas texture + sampler. Populated externally
    /// with glyphon-rendered filenames; sampled per-fragment and
    /// composited over the slat face.
    atlas_bg: wgpu::BindGroup,
    instance_count: u32,
}

impl Slat3DRenderer {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
    ) -> Self {
        let (verts, indices) = build_mesh(N_STRIPS);

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ygg-slat3d-verts"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ygg-slat3d-indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let index_count = indices.len() as u32;

        let initial_capacity: usize = 256;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-slat3d-instances"),
            size: (initial_capacity * size_of::<SlatInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ygg-slat3d-uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ygg-slat3d-bgl"),
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
            label: Some("ygg-slat3d-bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // Atlas bind group (group 1): sampled 2D texture + sampler.
        let atlas_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ygg-slat3d-atlas-bgl"),
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
        let atlas_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ygg-slat3d-atlas-bg"),
            layout: &atlas_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ygg-slat3d-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("ygg-slat3d-pipeline-layout"),
                bind_group_layouts: &[&bgl, &atlas_bgl],
                push_constant_ranges: &[],
            });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: 12,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
            ],
        };
        // Instance layout: model (4 vec4) + color (vec4) +
        // size_and_corner_and_arc (vec4) + hole (vec4) +
        // text_rect_px (vec4) + atlas_sub (vec4) + ink_color (vec4).
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: size_of::<SlatInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 48,
                    shader_location: 5,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 64,
                    shader_location: 6,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 80,
                    shader_location: 7,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 96,
                    shader_location: 8,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 112,
                    shader_location: 9,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 128,
                    shader_location: 10,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 144,
                    shader_location: 11,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ygg-slat3d-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[vertex_layout, instance_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            vertex_buffer,
            index_buffer,
            index_count,
            instance_buffer,
            instance_capacity: initial_capacity,
            uniform_buffer,
            bind_group,
            atlas_bg,
            instance_count: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[SlatInstance],
        projection: [[f32; 4]; 4],
        viewport_size: (u32, u32),
        sky_direction: [f32; 3],
        sky_color: [f32; 3],
        sky_intensity: f32,
    ) {
        if instances.len() > self.instance_capacity {
            let new_cap = instances.len().next_power_of_two().max(256);
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ygg-slat3d-instances"),
                size: (new_cap * size_of::<SlatInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = new_cap;
        }
        if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }
        self.instance_count = instances.len() as u32;

        let u = Uniforms {
            projection,
            viewport_size: [viewport_size.0 as f32, viewport_size.1 as f32],
            _pad0: [0.0; 2],
            sky_direction: [sky_direction[0], sky_direction[1], sky_direction[2], 0.0],
            sky_color_intensity: [sky_color[0], sky_color[1], sky_color[2], sky_intensity],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&u));
    }

    pub fn render<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.instance_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_bind_group(1, &self.atlas_bg, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.index_count, 0, 0..self.instance_count);
    }
}

fn build_mesh(n_strips: u32) -> (Vec<Vertex>, Vec<u16>) {
    let mut verts = Vec::with_capacity(2 * (n_strips as usize + 1));
    for i in 0..=n_strips {
        let v = i as f32 / n_strips as f32;
        verts.push(Vertex { pos: [0.0, v, 0.0], uv: [0.0, v] });
        verts.push(Vertex { pos: [1.0, v, 0.0], uv: [1.0, v] });
    }
    let mut indices = Vec::with_capacity(6 * n_strips as usize);
    for i in 0..n_strips {
        let a = (i * 2) as u16; // top-left of this strip (smaller v)
        let b = a + 1; // top-right
        let c = a + 2; // bot-left (next row)
        let d = a + 3; // bot-right
        // Two triangles: (a, c, d) and (a, d, b). CCW when seen
        // from +z (viewer side).
        indices.extend_from_slice(&[a, c, d, a, d, b]);
    }
    (verts, indices)
}

/// One-point perspective projection matrix with vanishing at
/// `(vx, vy)` in window-space physical pixels, focal depth `focal`.
/// World-space y is DOWN (screen convention); clip-space y is UP.
/// Column-major.
///
/// ## Focal depth — tuning guide
///
/// The projection is equivalent to a pinhole camera at
/// `(vx, vy, -focal)` looking at `+z`. `focal` = distance from that
/// camera to the screen plane (`z = 0`). It controls how strong the
/// perspective falloff is: a point at depth `focal` projects halfway
/// to the vanishing point on screen (`t = focal/(focal+focal) = 0.5`).
/// Smaller `focal` ⇒ more aggressive perspective.
///
/// Currently the renderer passes `focal = monitor_width / 2` (≈ 960
/// on a 1920-wide monitor). Some alternatives worth trying if the
/// slat 3D ever feels under- or over-perspective-d:
///
/// - `monitor_width / 2` (current, ~960): a point at `z = 960` is
///   halfway to vanishing. Gentle; slat tilts need ~30–60° to feel
///   obviously 3D.
/// - `monitor_width` (~1920): double the focal, half the perspective
///   strength. Slats stay almost flat-looking even at big tilts.
/// - `monitor_width / 4` (~480): twice the strength. Small tilts
///   (~15–25°) already show strong shelf perspective.
/// - `monitor_height` (~1080): uses the short dimension — matches
///   the "eye at screen-top" mental model a bit closer since the
///   vertical axis is what frames the viewer's field of view.
/// - Fixed anchor-z (whatever the user's desired physical distance
///   is): if you decide "I want the horizon point 500 px behind the
///   screen", use 500. Breaks the automatic-per-monitor scaling but
///   gives consistent feel across displays.
///
/// The `anchor` in `AppState::projection_anchor` carries the focal
/// as its Z component; change it there to test alternatives without
/// touching this function.
pub fn build_projection_matrix(
    viewport_size: (u32, u32),
    vanishing: (f32, f32),
    focal: f32,
) -> [[f32; 4]; 4] {
    let a = (viewport_size.0 as f32 * 0.5).max(1.0);
    let b = (viewport_size.1 as f32 * 0.5).max(1.0);
    let (vx, vy) = vanishing;
    let f = focal.max(1.0);
    [
        [f / a, 0.0, 0.0, 0.0],
        [0.0, -f / b, 0.0, 0.0],
        [(vx - a) / a, (b - vy) / b, 0.0, 1.0],
        [-f, f, 0.0, f],
    ]
}

/// Scale + rotate-around-mid-horizontal-axis + translate. Model
/// matrix maps slat-local `(u, v, 0) ∈ [0,1]^3` to world position
/// with the slat tilted by `angle_rad` around its horizontal mid
/// axis (axis through y = y + h/2, z = 0). `angle_rad = 0` leaves
/// the slat flat (face-on, z = 0 everywhere); `PI/2` rotates it
/// edge-on (top swings forward in -z, bottom back in +z).
///
/// Arc z is applied ON TOP of this in the vertex shader based on
/// `arc_depth`; the arc bows the slat's middle out of its (rotated)
/// face plane, independent of the rotation angle.
///
/// Column-major.
pub fn build_slat_model(x: f32, y: f32, w: f32, h: f32, angle_rad: f32) -> [[f32; 4]; 4] {
    // Sign convention: positive `angle_rad` rotates the slat so its
    // TOP recedes into +z (farther from viewer) and bottom comes
    // forward in -z (closer). Matches "looking down at a shelf from
    // above": far edge = top = narrower on screen; near edge = bottom
    // = wider. At `angle_rad = 0` the slat is face-on and flat.
    //
    // Achieved by rotating around the slat's horizontal mid-axis
    // with NEGATIVE sin — i.e., RotX(-angle_rad).
    let c = angle_rad.cos();
    let s = -angle_rad.sin();
    // Derivation (slat-local (u, v, 0)):
    //   scale → (u*w, v*h, 0)
    //   shift mid to origin → (u*w, v*h - h/2, 0)
    //   rotate around X by -angle → (u*w, (v*h-h/2)*cos, (v*h-h/2)*(-sin))
    //   shift back → (u*w, (v*h-h/2)*cos + h/2, (v*h-h/2)*(-sin))
    //   translate to world → (x + u*w, y + h/2 + (v*h-h/2)*cos, (v*h-h/2)*(-sin))
    //
    // World components as linear function of (u, v, slat_z=0, 1):
    //   world.x = w*u + x
    //   world.y = h*c*v + (y + h/2*(1 - c))
    //   world.z = h*s*v + (-h/2*s)   with s = -sin(angle) (already negated above)
    [
        [w, 0.0, 0.0, 0.0],                              // col 0: u coeff
        [0.0, h * c, h * s, 0.0],                        // col 1: v coeff
        [0.0, 0.0, 0.0, 0.0],                            // col 2: slat-local z (unused)
        [x, y + h * 0.5 * (1.0 - c), -h * 0.5 * s, 1.0], // col 3: constant
    ]
}

/// Default arc angle in degrees — how much of a full 360° the
/// slat's short cross-section subtends on its circle of curvature.
/// DPI- and slat-size-independent: a 90° arc reads as a 90° arc
/// whether the slat is 16 px or 60 px tall.
///
/// Physical model: the slat is a rigid partial cylinder (like a
/// short section of a rain gutter), long edges touching a flat
/// table, convex side facing the viewer. Its short-side cross-
/// section is a chord of a circle; this angle is what that chord
/// subtends.
///
/// Rough tuning reference at slat_height ≈ 23 px (cf. sagitta
/// mapping `arc_d_px = (h / 2) · tan(angle / 4)`):
///   - 0°   — perfectly flat slat. Rope splits hole symmetrically.
///   - 10°  — rigid rib (real aluminum blind). ~0.5 px sagitta.
///   - 30°  — clearly curved, still rigid-looking. ~1.5 px.
///   - 30°  — current default. Rigid rib, still clearly curved.
///            ~1.5 px sagitta at h = 23.
///   - 90°  — ~5 px sagitta; reads as a mild deliberate vault.
///   - 180° — half-pipe / U-channel.
///
/// Override per-session with `--debug-slat-arc-angle`.
///
/// Note: any nonzero arc pushes the slat's mid-row toward the
/// viewer. In combination with rotation, that drifts the projected
/// hole center slightly relative to the rope. Pass `0` to verify
/// symmetry.
pub const DEFAULT_ARC_ANGLE_DEG: f32 = 30.0;

const SHADER: &str = r#"
struct Uniforms {
    projection: mat4x4<f32>,
    viewport_size: vec2<f32>,
    _pad0: vec2<f32>,
    sky_direction: vec4<f32>,
    sky_color_intensity: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) model_0: vec4<f32>,
    @location(3) model_1: vec4<f32>,
    @location(4) model_2: vec4<f32>,
    @location(5) model_3: vec4<f32>,
    @location(6) color: vec4<f32>,
    @location(7) size_and_corner: vec4<f32>,
    @location(8) hole: vec4<f32>,
    @location(9) text_rect_px: vec4<f32>,
    @location(10) atlas_sub: vec4<f32>,
    @location(11) ink_color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) size_and_corner: vec4<f32>,
    @location(3) hole: vec4<f32>,
    @location(4) text_rect_px: vec4<f32>,
    @location(5) atlas_sub: vec4<f32>,
    @location(6) ink_color: vec4<f32>,
    @location(7) normal: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    let flat_world = model * vec4<f32>(in.pos, 1.0);
    let v_param = in.uv.y;
    let arc_d = in.size_and_corner.w;
    // Arc bulges the middle of the slat TOWARD the viewer (-z) —
    // a convex face, like a vaulted card lying on its side. Positive
    // `arc_d` therefore produces a negative z offset at v=0.5. (An
    // earlier convention had the mid recede in +z, which read as a
    // sail filling with wind pulled toward the vanishing point, not
    // as a piece of paper bowing toward the reader.)
    let arc_z = -arc_d * 4.0 * v_param * (1.0 - v_param);
    let world = vec4<f32>(flat_world.x, flat_world.y, flat_world.z + arc_z, 1.0);
    let clip = u.projection * world;

    // Surface normal at this vertex. u-tangent is model col 0
    // (pure +x for slats); v-tangent is model col 1 (rotation's y/z
    // components) PLUS the arc's world-z derivative. `arc_z` is
    // `-arc_d * 4 * v * (1-v)` (bulge toward viewer), so its
    // derivative in v is `-arc_d * 4 * (1 - 2*v)`. Normal =
    // v_tangent x u_tangent, which points toward the camera (-z)
    // when the slat is flat and rolls with rotation + arc otherwise.
    let dv_arc = -arc_d * 4.0 * (1.0 - 2.0 * v_param);
    let tan_v = normalize(vec3<f32>(in.model_1.x, in.model_1.y, in.model_1.z + dv_arc));
    let tan_u = normalize(in.model_0.xyz);
    let normal = normalize(cross(tan_v, tan_u));

    var out: VsOut;
    out.pos = clip;
    out.uv = in.uv;
    out.color = in.color;
    out.size_and_corner = in.size_and_corner;
    out.hole = in.hole;
    out.text_rect_px = in.text_rect_px;
    out.atlas_sub = in.atlas_sub;
    out.ink_color = in.ink_color;
    out.normal = normal;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let size = in.size_and_corner.xy;
    let r = max(in.size_and_corner.z, 0.0);
    let px = in.uv * size;

    // Rounded-rect SDF.
    let d = min(px, size - px);
    if (d.x < r && d.y < r) {
        let from_corner = vec2<f32>(r - d.x, r - d.y);
        if (length(from_corner) > r) {
            discard;
        }
    }

    // Elliptical hole cut.
    let hole_half = in.hole.zw;
    if (hole_half.x > 0.0 && hole_half.y > 0.0) {
        let delta = (px - in.hole.xy) / hole_half;
        if (dot(delta, delta) <= 1.0) {
            discard;
        }
    }

    var final_color = in.color;

    // Paint text from the atlas, if a text rect is set. The slat's
    // local pixel position within the text rect maps to the atlas
    // sub-rect — text keeps its rendered pixel size regardless of
    // the slat's angle or arc (perspective already handled by the
    // vertex projection).
    let text_rect = in.text_rect_px;
    if (text_rect.z > 0.0 && text_rect.w > 0.0) {
        let rel = (px - text_rect.xy) / text_rect.zw;
        if (rel.x >= 0.0 && rel.x <= 1.0 && rel.y >= 0.0 && rel.y <= 1.0) {
            let atlas_uv = mix(in.atlas_sub.xy, in.atlas_sub.zw, rel);
            let sampled = textureSample(atlas_tex, atlas_sampler, atlas_uv);
            let ink = in.ink_color;
            // Use atlas alpha as ink opacity; ink color replaces slat color.
            final_color = vec4<f32>(
                mix(final_color.rgb, ink.rgb, sampled.a * ink.a),
                final_color.a
            );
        }
    }

    // Stellar shading. Two parts:
    //   (a) Lambert diffuse across the slat face — tilt moves face
    //       brightness. Ambient floor is kept modest (0.55) so tilt
    //       actually shows; diffuse gain 0.45 lets a 60° tilt brighten
    //       the face meaningfully.
    //   (b) Per-fragment top-edge rim + contact shadow — the
    //       canonical Zone-3 on-scroll moves. Rim is a thin bright
    //       line inside v=0 ("lit from above"); shadow is a short
    //       dark dart below the rim (cast shadow of the slat above).
    //       These don't depend on tilt, so they always separate
    //       stacked slats even when every slat shares one global tilt.
    // A fraction of the sky's colour mixes into the slat tint so
    // slats warm at dawn/dusk, cool at noon, desaturate at night.
    let n = normalize(in.normal);
    let lambert = max(dot(n, u.sky_direction.xyz), 0.0);
    let sky_intensity = u.sky_color_intensity.w;
    let sky_color = u.sky_color_intensity.xyz;
    let ambient = 0.55;
    let diffuse_gain = 0.45;
    let base_shade = ambient + diffuse_gain * lambert * sky_intensity;

    // Edge lights, in slat-local pixel space.
    let rim_thickness_px = 1.5;
    let rim_t = clamp(1.0 - px.y / rim_thickness_px, 0.0, 1.0);
    let rim = 0.28 * rim_t * sky_intensity;

    let shadow_depth_px = max(size.y * 0.28, 4.0);
    let shadow_t_raw = clamp((px.y - rim_thickness_px) / (shadow_depth_px - rim_thickness_px), 0.0, 1.0);
    let shadow_t = 1.0 - shadow_t_raw;
    let shadow = 0.22 * shadow_t * (0.4 + 0.6 * sky_intensity);

    let shade = clamp(base_shade + rim - shadow, 0.0, 1.2);
    let tint_mix = 0.12 * sky_intensity;
    let tinted = mix(final_color.rgb, sky_color, tint_mix);
    let lit = tinted * shade;

    return vec4<f32>(lit * final_color.a, final_color.a);
}
"#;
