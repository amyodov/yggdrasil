//! Icon atlas — Lucide icons rasterized via tiny-skia at init time.
//!
//! Lucide icons are short SVG polylines (most of them two segments with
//! rounded joins/caps). Rather than pull in a full SVG parser for the tiny
//! set we actually need, we transcribe the path data by hand — each icon's
//! `rasterize` function is ~5 lines and stays close to the Lucide source.
//!
//! All icons are packed horizontally into a single RGBA8 texture. Each icon
//! occupies an `ICON_PX × ICON_PX` tile; the atlas is wide and thin. Icons
//! draw as white-on-transparent; the icon-rendering pipeline tints them at
//! sample time. That means the atlas is reusable: any tint, any size,
//! anywhere, without re-rasterization.

use tiny_skia::{LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindingResource, Device,
    Extent3d, Queue, Sampler, SamplerDescriptor, Texture, TextureAspect, TextureDescriptor,
    TextureDimension, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
};

/// Each icon tile is rasterized at this resolution. Plenty for a fold
/// handle (~14 pt = 14–28 px physical) and leaves room for upscaling if
/// we ever want bigger handles. Oversampling helps anti-aliasing.
pub const ICON_PX: u32 = 48;

/// Lucide icon catalogue. Order defines index in the atlas.
///
/// The `Rows*` family is the fold-switch's visual vocabulary: each icon is
/// a progressively denser stack of horizontal bars, matching how much
/// content the card will show in that state (Rows1 = just the header;
/// Rows2 = header + docstring; Rows3 = full body). They form an ordered
/// visual series, so the switch reads left-to-right as "less content →
/// more content" even before the user learns what each state does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // ChevronDown/Right are kept in the atlas for future UI
// (tree expanders, breadcrumbs); Rows2 is reserved for M3.4's HeaderOnly
// fold state. Cheap to keep — a few extra tiles in the atlas texture.
pub enum IconId {
    ChevronDown = 0,
    ChevronRight = 1,
    /// One horizontal bar — Lucide `minus`. Folded state (header only).
    Rows1 = 2,
    /// Two horizontal bars — Lucide `equal`. Header+docstring (M3.4).
    Rows2 = 3,
    /// Three horizontal bars — Lucide `menu`. Fully unfolded.
    Rows3 = 4,
}

impl IconId {
    pub const COUNT: u32 = 5;

    pub fn atlas_index(self) -> u32 {
        self as u32
    }
}

/// Atlas = the GPU texture + the sampler + bind-group layout description.
/// Owns its bind group since the atlas texture never changes after init.
pub struct IconAtlas {
    /// Held to keep the GPU texture alive even though we sample via the
    /// bind group rather than reading `texture` directly.
    #[allow(dead_code)]
    pub texture: Texture,
    #[allow(dead_code)]
    pub view: TextureView,
    #[allow(dead_code)]
    pub sampler: Sampler,
    pub bind_group: BindGroup,
    pub bind_group_layout: BindGroupLayout,
}

impl IconAtlas {
    pub fn new(device: &Device, queue: &Queue) -> Self {
        let atlas_width = ICON_PX * IconId::COUNT;
        let atlas_height = ICON_PX;

        let pixmap = rasterize_all(atlas_width, atlas_height);

        let texture = device.create_texture(&TextureDescriptor {
            label: Some("ygg-icon-atlas"),
            size: Extent3d { width: atlas_width, height: atlas_height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            // Unorm (not sRGB) — we sample the alpha channel as a coverage
            // mask and tint with an sRGB colour. Gamma doesn't apply to
            // an alpha mask, and linear sampling produces correct AA edges.
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            pixmap.data(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(atlas_width * 4),
                rows_per_image: Some(atlas_height),
            },
            Extent3d { width: atlas_width, height: atlas_height, depth_or_array_layers: 1 },
        );

        let view = texture.create_view(&TextureViewDescriptor::default());

        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("ygg-icon-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ygg-icon-atlas-bgl"),
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

        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("ygg-icon-atlas-bg"),
            layout: &bind_group_layout,
            entries: &[
                BindGroupEntry { binding: 0, resource: BindingResource::TextureView(&view) },
                BindGroupEntry { binding: 1, resource: BindingResource::Sampler(&sampler) },
            ],
        });

        Self { texture, view, sampler, bind_group, bind_group_layout }
    }
}

// ---------------------------------------------------------------------------
// Rasterization
// ---------------------------------------------------------------------------

fn rasterize_all(atlas_w: u32, atlas_h: u32) -> Pixmap {
    let mut pixmap = Pixmap::new(atlas_w, atlas_h).expect("icon-atlas alloc");

    rasterize_chevron_down(&mut pixmap, 0, 0, ICON_PX);
    rasterize_chevron_right(&mut pixmap, ICON_PX, 0, ICON_PX);
    // `rows-N`: N horizontal bars. Bars are spaced to match Lucide's
    // `minus` / `equal` / `menu` — 1 bar at y=12; 2 bars at y=9,15;
    // 3 bars at y=6,12,18. The three icons look like a clear ordered
    // progression even at small render sizes.
    rasterize_rows(&mut pixmap, 2 * ICON_PX, 0, ICON_PX, &[12.0]);
    rasterize_rows(&mut pixmap, 3 * ICON_PX, 0, ICON_PX, &[9.0, 15.0]);
    rasterize_rows(&mut pixmap, 4 * ICON_PX, 0, ICON_PX, &[6.0, 12.0, 18.0]);

    pixmap
}

/// Lucide `chevron-down`: `<path d="m6 9 6 6 6-6"/>` in a 24×24 viewBox.
fn rasterize_chevron_down(pixmap: &mut Pixmap, ox: u32, oy: u32, size: u32) {
    // Lucide paths are defined in a 24-unit box.
    let pts = [(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)];
    stroke_polyline(pixmap, ox, oy, size, &pts);
}

/// Lucide `chevron-right`: `<path d="m9 18 6-6-6-6"/>` — traverses
/// (9, 18) → (15, 12) → (9, 6), which is the same `>` shape.
fn rasterize_chevron_right(pixmap: &mut Pixmap, ox: u32, oy: u32, size: u32) {
    let pts = [(9.0, 6.0), (15.0, 12.0), (9.0, 18.0)];
    stroke_polyline(pixmap, ox, oy, size, &pts);
}

/// Rasterize N horizontal bars at the given y coordinates (in Lucide's
/// 24-unit viewbox). Each bar spans x=4 to x=20 with rounded caps, matching
/// the stroke width / cap style of Lucide's `minus` / `equal` / `menu`.
fn rasterize_rows(pixmap: &mut Pixmap, ox: u32, oy: u32, size: u32, ys: &[f32]) {
    for &y in ys {
        let bar = [(4.0, y), (20.0, y)];
        stroke_polyline(pixmap, ox, oy, size, &bar);
    }
}

/// Stroke a polyline matching Lucide's default stroke settings:
/// width 2 (in the 24-unit viewBox), round caps, round joins, white colour.
fn stroke_polyline(pixmap: &mut Pixmap, ox: u32, oy: u32, size: u32, pts: &[(f32, f32)]) {
    let scale = size as f32 / 24.0;

    let mut pb = PathBuilder::new();
    let (x0, y0) = pts[0];
    pb.move_to(x0 * scale, y0 * scale);
    for &(x, y) in &pts[1..] {
        pb.line_to(x * scale, y * scale);
    }
    let Some(path) = pb.finish() else { return };

    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;

    let stroke = Stroke {
        width: 2.0 * scale,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };

    let transform = Transform::from_translate(ox as f32, oy as f32);
    pixmap.stroke_path(&path, &paint, &stroke, transform, None);
}
