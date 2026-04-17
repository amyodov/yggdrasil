//! Plate primitive — the architectural foundation of M3.1.
//!
//! A **plate** is a rectangular surface that owns an offscreen render target
//! (RT). Its contents (shapes, text) are drawn into the RT in *plate-local*
//! coordinates (`0..size`). The compositor then samples the RT as a textured
//! quad, transformed by the plate's model matrix, onto the swap chain.
//!
//! ## Why this primitive exists (architectural stake)
//!
//! Every downstream visual concern benefits from rendering through a plate:
//!
//! - **Scroll**: today we re-render visible content each frame; future work
//!   samples a larger-than-plate RT with a UV offset, getting scroll for free.
//! - **Curl / scroll-winding (M3.7)**: the top/bottom pin zones sample the
//!   plate RT onto a cylindrical UV mapping. Impossible without an RT.
//! - **3D rotation / plate-as-page (future)**: the model matrix handles this.
//!   With identity it looks 2D today; with a rotation it *is* 3D. No code
//!   elsewhere has to know.
//! - **Dirty-flag caching**: if the plate's contents didn't change, don't
//!   redraw them — just re-composite the cached RT. Cheap frame-to-frame.
//!
//! ## M3.1 scope and deferred
//!
//! M3.1 ships:
//! - RT = plate size. One RT per plate.
//! - Identity model matrix (ortho + frontal). Looks 2D.
//! - Mipmap level count = 1. No angular rendering yet.
//! - `dirty` field is maintained but the renderer re-renders every frame for
//!   now; wiring up actual dirty-skip is trivial once there's motivation.
//!
//! Deferred to follow-ups (noted here, not reimplemented-and-forgotten):
//! - Plate-height × 3 RT with UV-offset scrolling.
//! - Manual mipmap chain + sampling at higher mip for angled views.
//! - RT pool with recycling across plates.

use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindingResource, Buffer,
    Device, Extent3d, Sampler, Texture, TextureDescriptor, TextureDimension, TextureFormat,
    TextureUsages, TextureView, TextureViewDescriptor,
};

/// A 4x4 column-major identity matrix. The plate's default orientation —
/// frontal, no rotation. Rotations swap this for a real matrix later.
pub const IDENTITY4: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

pub struct Plate {
    /// Plate size in physical pixels (also the RT size in M3.1).
    pub size_px: [u32; 2],
    /// Top-left of the plate in *window / swap-chain* space, physical pixels.
    /// The compositor adds this to model-transformed plate-local coordinates
    /// to compute screen positions.
    pub pos_px: [f32; 2],
    /// 4x4 model matrix applied to plate-local coords `[0, size_px]` before
    /// the screen translation. Identity = frontal.
    pub model: [[f32; 4]; 4],
    /// RT texture the plate's contents draw into.
    pub rt_texture: Texture,
    /// View of the RT — used both as a render attachment (when drawing into
    /// the plate) and as a shader resource (during composite).
    pub rt_view: TextureView,
    /// Bind group for the composite pipeline: shared uniforms + this plate's
    /// texture + sampler. Rebuilt when the RT is reallocated (on resize).
    pub composite_bg: BindGroup,
    /// Does the plate's RT need re-rendering this frame? M3.1 re-renders
    /// unconditionally, but the field exists so follow-ups can opt in to
    /// cached-RT compositing without touching the primitive.
    pub dirty: bool,
}

impl Plate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &Device,
        size_px: [u32; 2],
        pos_px: [f32; 2],
        format: TextureFormat,
        layout: &BindGroupLayout,
        sampler: &Sampler,
        uniform_buffer: &Buffer,
    ) -> Self {
        let (rt_texture, rt_view) = allocate_rt(device, size_px, format);
        let composite_bg = build_bind_group(device, layout, uniform_buffer, &rt_view, sampler);
        Self {
            size_px,
            pos_px,
            model: IDENTITY4,
            rt_texture,
            rt_view,
            composite_bg,
            dirty: true,
        }
    }

    /// Update the plate's position and (optionally) size. A size change
    /// reallocates the RT and rebuilds the composite bind group. Returns
    /// `true` if the RT was reallocated (callers may want to invalidate
    /// caches).
    #[allow(clippy::too_many_arguments)]
    pub fn reconfigure(
        &mut self,
        device: &Device,
        size_px: [u32; 2],
        pos_px: [f32; 2],
        format: TextureFormat,
        layout: &BindGroupLayout,
        sampler: &Sampler,
        uniform_buffer: &Buffer,
    ) -> bool {
        self.pos_px = pos_px;
        let size_changed = self.size_px != size_px;
        if size_changed {
            self.size_px = size_px;
            let (tex, view) = allocate_rt(device, size_px, format);
            self.rt_texture = tex;
            self.rt_view = view;
            self.composite_bg =
                build_bind_group(device, layout, uniform_buffer, &self.rt_view, sampler);
        }
        self.dirty = true;
        size_changed
    }

    #[allow(dead_code)] // Used by follow-ups; exposed now so the primitive has
    // the full API it needs for cached compositing later.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

fn allocate_rt(
    device: &Device,
    size_px: [u32; 2],
    format: TextureFormat,
) -> (Texture, TextureView) {
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("ygg-plate-rt"),
        size: Extent3d {
            width: size_px[0].max(1),
            height: size_px[1].max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor::default());
    (texture, view)
}

fn build_bind_group(
    device: &Device,
    layout: &BindGroupLayout,
    uniform_buffer: &Buffer,
    rt_view: &TextureView,
    sampler: &Sampler,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("ygg-plate-composite-bg"),
        layout,
        entries: &[
            BindGroupEntry { binding: 0, resource: uniform_buffer.as_entire_binding() },
            BindGroupEntry { binding: 1, resource: BindingResource::TextureView(rt_view) },
            BindGroupEntry { binding: 2, resource: BindingResource::Sampler(sampler) },
        ],
    })
}

// ---------------------------------------------------------------------------
// Model-matrix helpers — kept tiny, no external linear-algebra dep. If we
// start composing more than translate/rotate/scale at the call site, pull in
// `glam` (or similar) in the next sub-milestone.
// ---------------------------------------------------------------------------

/// Multiply two 4x4 column-major matrices. `c = a * b` in the usual sense
/// (apply `b` first, then `a`).
#[allow(dead_code)] // Lives here for the rotation sub-milestone; validated
// in tests so we know it's correct when we need it.
#[allow(clippy::needless_range_loop)] // 4x4 fixed-size; range is clearer than iter().enumerate()
pub fn mat4_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k][j] * b[i][k];
            }
            out[i][j] = s;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Identity * identity = identity.
    #[test]
    #[allow(clippy::needless_range_loop)]
    fn identity_times_identity_is_identity() {
        let p = mat4_mul(IDENTITY4, IDENTITY4);
        for i in 0..4 {
            for j in 0..4 {
                let expect = if i == j { 1.0 } else { 0.0 };
                assert!((p[i][j] - expect).abs() < 1e-6);
            }
        }
    }

    /// Identity * M = M.
    #[test]
    #[allow(clippy::needless_range_loop)]
    fn identity_times_m_is_m() {
        // An arbitrary but simple matrix (translation by (10, 20, 0)).
        let m = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [10.0, 20.0, 0.0, 1.0],
        ];
        let p = mat4_mul(IDENTITY4, m);
        for i in 0..4 {
            for j in 0..4 {
                assert!((p[i][j] - m[i][j]).abs() < 1e-6, "i={i} j={j}");
            }
        }
    }
}
