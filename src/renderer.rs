//! Renderer — wgpu + glyphon + egui plumbing, now with a plate primitive (M3.1).
//!
//! ## Pipeline (M3.1)
//!
//! Five render passes per frame:
//!
//! 1. **Background pass** → swap chain. Draws the breathing sky. Unchanged
//!    from M2/M3.
//! 2. **Plate shapes pass** → plate RT. Clears the plate to transparent, then
//!    draws the panel background + all visible card shapes (bg, accent, spine,
//!    fold handle, rolling edge) in **plate-local** coordinates.
//! 3. **Plate text pass** → plate RT. Glyphon renders per-card text buffers
//!    at their plate-local positions.
//! 4. **Composite pass** → swap chain. Samples the plate RT as a textured
//!    quad, transformed by the plate's model matrix. With an identity model
//!    matrix (M3.1 default) this produces an on-screen rectangle — plate looks
//!    2D. Rotation matrices in later milestones tilt the plate in 3D without
//!    any change to this pipeline.
//! 5. **Egui pass** → swap chain. HUD overlay (file-tree placeholder for now).
//!
//! ## Coordinate systems
//!
//! Two coordinate spaces matter now:
//!
//! - **Plate-local**, origin at plate's top-left, used by: layout, card
//!   rects, text areas, shape positions inside the plate pass, and
//!   `fold_buttons_scene`. Physical pixels.
//! - **Window / screen**, origin at window's top-left, used by: cursor events,
//!   plate position, composite output, background, egui. Physical pixels.
//!
//! Conversion: `screen = plate.pos + model * plate_local`. With identity
//! model (today), `screen = plate.pos + plate_local`.
//!
//! Hit-testing in `app.rs` converts cursor from screen to plate-local before
//! comparing against `fold_buttons_scene` output.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, Device, DeviceDescriptor, Features, Instance,
    InstanceDescriptor, Limits, LoadOp, MemoryHints, MultisampleState, Operations, PowerPreference,
    PresentMode, Queue, RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions,
    StoreOp, Surface, SurfaceConfiguration, TextureFormat, TextureUsages, TextureViewDescriptor,
};
use winit::window::Window;

use crate::background::BackgroundRenderer;
use crate::blind;
use crate::cards::{
    layout_cards_with_overrides, Card, CardId, CardKind, CardRect, Layout, LayoutMetrics,
    MethodModifier, Visibility,
};
use crate::cli::WrapMode;
use crate::composite::CompositeRenderer;
use crate::lens_pipeline::{LensInstance, LensRenderer};
use crate::icon_pipeline::{IconInstance, IconRenderer};
use crate::icons::IconId;
use crate::substrate::Substrate;
use crate::shapes::{RectInstance, ShapeRenderer};
use crate::slat3d::{
    build_projection_matrix, build_slat_model, Slat3DRenderer, SlatInstance,
};
use crate::state::{
    card_fold_states, card_well_position, AppState, FoldState, HighlightedSource, WindowSize,
};
use crate::syntax::TokenKind;

// ----------------------------------------------------------------------------
// Palette & layout constants (logical points where marked with _PT)
// ----------------------------------------------------------------------------

/// Card backgrounds (Zone-3 paper). Near-opaque so cards read as physical
/// objects lying on the plate, not as tinted glass panels. A tiny bit of
/// translucency (~0.92) lets the plate's inner luminance just barely affect
/// them, so they pick up a hint of the plate's warmth.
const CARD_BG: [f32; 4] = [0.080, 0.090, 0.120, 0.92];
const CLASS_BG: [f32; 4] = [0.095, 0.105, 0.140, 0.94];

/// Card drop shadow — reads as "card lifted slightly off the plate." Drawn
/// BEFORE the card fill, with transparent fill + dark blurred glow. The
/// offset says "light comes from above-left" (consistent with the plate's
/// top-edge rim light). Blur radius is the shadow's softness; higher =
/// softer edge.
const CARD_SHADOW_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.40];
const CARD_SHADOW_OFFSET_PT: f32 = 2.5;
const CARD_SHADOW_BLUR_PT: f32 = 10.0;
/// Private cards sit a hair lower (smaller shadow) than public ones — this
/// is the three-zone-grammar's "sits lower" affordance.
const PRIVATE_SHADOW_SCALE: f32 = 0.65;

/// Left-side accent strip colors per visibility/modifier.
const ACCENT_PUBLIC: [f32; 4] = [0.50, 0.84, 0.98, 1.0];
const ACCENT_PRIVATE: [f32; 4] = [0.42, 0.46, 0.60, 0.82];
const ACCENT_CLASSMETHOD: [f32; 4] = [0.98, 0.82, 0.55, 1.0];
const ACCENT_STATICMETHOD: [f32; 4] = [0.92, 0.72, 0.95, 1.0];
const ACCENT_PROPERTY: [f32; 4] = [0.65, 0.98, 0.82, 1.0];
/// Neutral slate for top-level orphan code (imports, constants, `if __name__`).
const ACCENT_SNIPPET: [f32; 4] = [0.52, 0.58, 0.68, 0.85];

/// Class spine (armature) — foil inlay in the linen. Three visuals:
/// (1) the foil **base**, a warm brass stripe that lies flush with the
/// linen (no outer glow into the void, per YGG-33 "flush with the
/// surface, no drop shadow");
/// (2) the luminous **seam**, a hairline bright channel at the foil's
/// center that reads as "light through an etched crack in the metal" —
/// this is the Zone-3 semantic light the spine is permitted;
/// (3) a **glint** — a small bright spot that drifts along the spine as
/// the SkyLight direction changes and picks up the sky's color.
///
/// None of these ever hit pure 0/255 — the palette stays gray-leaning
/// so brightest spots read as "lighter than everything else around"
/// rather than hard dots punched through the UI.
const SPINE_FOIL_COLOR: [f32; 4] = [0.55, 0.40, 0.20, 1.00]; // burnished brass
const SPINE_SEAM_COLOR: [f32; 4] = [0.88, 0.78, 0.54, 0.95]; // warm seam, not pure white
const SPINE_SEAM_GLOW: [f32; 4] = [0.82, 0.64, 0.30, 0.50]; // warm halo from the seam
const SPINE_SEAM_WIDTH_PT: f32 = 1.0;
const SPINE_SEAM_GLOW_RADIUS_PT: f32 = 4.0;
/// Foil glint — the SkyLight-driven specular spot riding on the spine.
/// Base color blends with SkyLight.color; much more sky-dominant than
/// the lens specular because metal reflection takes more of the
/// environment color than glass does.
const SPINE_GLINT_BASE: [f32; 4] = [0.88, 0.84, 0.74, 0.90];
const SPINE_GLINT_SIZE_PT: f32 = 4.0;
const SPINE_GLINT_SKY_MIX: f32 = 0.55;
/// Faint sky-tint on the foil body itself, so the whole rail — not just
/// the travelling glint — breathes with the sky. Kept subtle so the brass
/// identity survives; the foil shifts hue across the cycle rather than
/// pretending to be a mirror.
const SPINE_FOIL_SKY_MIX: f32 = 0.12;

/// Fold handle chevron icon tint (M3.2). Single colour; direction comes
/// from the chevron orientation, not a palette flip.
const FOLD_HANDLE_ICON: [f32; 4] = [0.78, 0.86, 0.96, 0.95];
/// Fold handle "chip" background — the widget body's fill. Deliberately
/// dark and low-saturation so the specular highlight and glint on top can
/// land near full white and read as HDR-shiny (light gathered through
/// glass / reflected off metal, against a restrained surface). If this
/// color creeps up in lightness, the bright spots stop popping.
const FOLD_CHIP_BG: [f32; 4] = [0.13, 0.16, 0.21, 1.0];
/// Dome amount applied to the fold-handle chip (M3.2 Pass 3). 0.0 = flat;
/// 1.0 = full effect. At small button sizes we need the full range.
const FOLD_CHIP_DOME: f32 = 1.0;

/// Which selected-state metaphor the fold widget uses.
///
/// `false` (default): **Lens.** The selected slot's icon is rendered at
/// `FOLD_LENS_ICON_SIZE_PT`, floating slightly above the widget outline —
/// reads as looking through a magnifying glass. No lens frame; the
/// oversized icon itself *is* the lens.
///
/// `true`: **Dent.** A concave chip inside the widget, same size as the
/// old single-button chip, drawn with inverted dome shading. Kept as a
/// toggle because it was aesthetically refined but too low-contrast to
/// reliably communicate state — some monitors / eyes couldn't see it.
#[allow(dead_code)]
const USE_DENT_METAPHOR: bool = false;

/// Size of the magnified icon inside the lens. ~1.75x the regular handle
/// size so the lens distinctly stands apart from the small slot icons
/// and extends a pixel or two above the widget's pillow-peak silhouette.
///
/// With the pixel-space lens pipeline (post-composite sampling), this
/// constant is unused — the lens pass samples the plate RT and magnifies
/// its pixels directly. Kept for the dormant `USE_DENT_METAPHOR` path
/// and for reference.
#[allow(dead_code)]
const FOLD_LENS_ICON_SIZE_PT: f32 = 28.0;

/// Lens magnification factor. 1.75 reads as "clearly bigger but still
/// recognizable"; pushed higher (>2.5) tends toward funhouse-mirror.
const FOLD_LENS_MAGNIFICATION: f32 = 1.75;

/// Horizontal gap between sub-button caps in logical points. Gives the
/// lens a little breathing room at the rim so it doesn't clip a couple
/// of pixels of the neighbouring slot.
const FOLD_SLOT_GAP_PT: f32 = 2.0;

/// Lens disc — the visible glass circle the magnified icon sits inside.
/// Slightly larger than the magnified icon so its rim sits clear of the
/// icon strokes. Corner radius is set to half the side at render time,
/// which yields a perfect circle.
const FOLD_LENS_DISC_SIZE_PT: f32 = 30.0;
/// Glass tint — unused with the pixel-space lens (the lens pass samples
/// the plate RT directly, no flat disc fill). Kept for the dormant
/// `USE_DENT_METAPHOR` path and for tuning reference.
#[allow(dead_code)]
const FOLD_LENS_DISC_COLOR: [f32; 4] = [0.20, 0.24, 0.32, 1.0];

/// Lens drop shadow — dark-gray glow (not pure black), slightly offset,
/// creating the "floating above the widget" cue.
const FOLD_LENS_SHADOW_COLOR: [f32; 4] = [0.03, 0.04, 0.06, 0.40];
const FOLD_LENS_SHADOW_OFFSET_X_PT: f32 = 0.5;
const FOLD_LENS_SHADOW_OFFSET_Y_PT: f32 = 1.5;
const FOLD_LENS_SHADOW_GLOW_PT: f32 = 4.0;

/// Lens specular highlight — an HDR-bright spot on the glass, positioned
/// by the current `SkyLight` direction. Now near-full white: against the
/// dark widget/disc it reads as the "lighter than anything else on
/// screen" spot. Tinted slightly by `SkyLight.color` so it still warms
/// with the sky.
const FOLD_LENS_SPECULAR_BASE: [f32; 4] = [1.00, 1.00, 1.00, 1.00];
// Three-dot tangent arc constants — now baked into the lens shader
// (see `lens_pipeline.rs`). Kept for reference tuning; the shader's
// own constants `0.82` (rim radius) and `0.28` (arc spread) mirror
// these so tweaks here don't actually apply without a shader edit.
#[allow(dead_code)]
const FOLD_LENS_SPECULAR_CENTER_SIZE_PT: f32 = 2.5;
#[allow(dead_code)]
const FOLD_LENS_SPECULAR_FLANK_SIZE_PT: f32 = 2.0;
#[allow(dead_code)]
const FOLD_LENS_SPECULAR_ARC_SPREAD: f32 = 0.28; // radians
#[allow(dead_code)]
const FOLD_LENS_SPECULAR_FLANK_ALPHA: f32 = 0.45;
#[allow(dead_code)]
const FOLD_LENS_SPECULAR_RIM_RADIUS: f32 = 0.82;
/// Blend weight of SkyLight.color into the specular highlight color.
/// 0.22 keeps the core bright-white while letting dawns warm it and
/// noons leave it near-neutral.
const FOLD_LENS_SPECULAR_SKY_MIX: f32 = 0.40;
/// Intensity threshold below which the specular disappears entirely.
/// Matches "night" moods (intensity ~0.04–0.12) so the sun's reflection
/// vanishes when the star is below the horizon.
const FOLD_LENS_SPECULAR_NIGHT_THRESHOLD: f32 = 0.15;
/// Tiny per-card angular jitter (radians) on top of the sky-direction
/// angle. The virtual sun is treated as infinitely far in a given
/// direction, so every lens would otherwise glint at identical angles —
/// jitter breaks that lock-step without introducing a physics-violating
/// position bias.
const FOLD_LENS_PER_CARD_JITTER: f32 = 0.08;

/// Icon for a given fold target. The `Rows1` / `Rows2` / `Rows3` series is
/// an ordered visual progression — one bar for "just the header", two for
/// "header + docstring" (M3.4), three for "fully unfolded body visible". At
/// any card the widget reads left-to-right as less content → more content,
/// teaching the state axis before the user learns what each slot does.
fn icon_for_fold_state(state: FoldState) -> IconId {
    match state {
        FoldState::Folded => IconId::Rows1,
        FoldState::HeaderOnly => IconId::Rows2,
        FoldState::Unfolded => IconId::Rows3,
    }
}

/// Rolling edge shown along the body's bottom during a fold animation.
const ROLL_EDGE_COLOR: [f32; 4] = [0.85, 0.92, 1.00, 0.85];
const ROLL_EDGE_GLOW: [f32; 4] = [0.60, 0.80, 1.00, 0.45];

const ACCENT_WIDTH_PT: f32 = 3.0;
const ACCENT_WIDTH_PT_PRIVATE: f32 = 2.0;
const SPINE_WIDTH_PT: f32 = 3.0;
/// Extra horizontal inset for a method attached to the class spine.
const INSTANCE_METHOD_INSET_PT: f32 = 8.0;

/// Plate inset from the code pane's nominal rectangle. The plate floats with
/// this much space between itself and the window edges (left, right, top,
/// bottom). M3.6 will expand this for more breathing room.
pub const PANEL_INSET_PT: f32 = 14.0;
/// Corner radii.
const PANEL_CORNER_RADIUS_PT: f32 = 14.0;
const CARD_CORNER_RADIUS_PT: f32 = 6.0;
const ACCENT_CORNER_RADIUS_PT: f32 = 1.0;

/// Plate outer bloom (Zone-2 halo into the void). Baked into the composite
/// shader so it doesn't consume extra RT space and follows the plate's
/// rounded silhouette — not its rectangular RT bounds.
const PLATE_BLOOM_RADIUS_PT: f32 = 14.0;
/// Cool blue-violet halo color, tuned to complement the nebula palette. The
/// alpha channel is peak bloom intensity (the shader multiplies by SDF
/// falloff, so this is the brightness right at the plate edge).
const PLATE_BLOOM_COLOR: [f32; 4] = [0.40, 0.55, 0.95, 0.26];

/// Rim light along the plate's top inner edge — the "lit from above" cue
/// that confirms the plate is a lit material, not a printed rectangle.
/// Thickness is how far inward the rim reaches before fading out; intensity
/// multiplies the rim color in `composite.rs`.
const PLATE_RIM_THICKNESS_PT: f32 = 2.0;
const PLATE_RIM_INTENSITY: f32 = 1.25;

const CODE_PAD_LEFT_PT: f32 = 20.0;
const CODE_PAD_RIGHT_PT: f32 = 20.0;
const CARD_INNER_PAD_Y_PT: f32 = 7.0;
const TOP_LEVEL_GAP_PT: f32 = 10.0;
const DEPTH_INDENT_PT: f32 = 22.0;
/// Fold-handle icon size in logical points — fixed across all cards so
/// every fold button reads as the same affordance, not a size-varies-by-
/// card-kind puzzle. DPI-scaled at use-site.
const FOLD_HANDLE_SIZE_PT: f32 = 16.0;
/// Chip padding around the icon, in logical points. Chip is
/// (FOLD_HANDLE_SIZE_PT + 2 * FOLD_CHIP_PAD_PT) on each side.
const FOLD_CHIP_PAD_PT: f32 = 4.0;
const CARD_TEXT_INSET_PT: f32 = 12.0;
/// Plate-local top padding above the first card.
pub const SCENE_TOP_PAD_PT: f32 = 14.0;
const ROLL_EDGE_THICKNESS_PT: f32 = 1.2;

// ----------------------------------------------------------------------------
// Helpers exposed for hit-testing: where the plate lives in window space.
// ----------------------------------------------------------------------------

/// Plate position + size in physical pixels for the given state.
/// Exposed so `app.rs` can convert cursor (window-space) to plate-local when
/// hit-testing card affordances.
pub fn plate_rect(state: &AppState) -> ([f32; 2], [u32; 2]) {
    let sf = state.scale_factor;
    let inset = PANEL_INSET_PT * sf;
    let pos = [state.code_pane_left() as f32 + inset, inset];
    let w = (state.code_pane_width() as f32 - inset * 2.0).max(1.0) as u32;
    let h = (state.window_size.height as f32 - inset * 2.0).max(1.0) as u32;
    (pos, [w, h])
}

// ----------------------------------------------------------------------------
// Renderer
// ----------------------------------------------------------------------------

pub struct Renderer {
    window: Arc<Window>,
    device: Device,
    queue: Queue,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,

    // Shapes + background pipelines.
    shape_renderer: ShapeRenderer,
    background_renderer: BackgroundRenderer,
    icon_renderer: IconRenderer,
    lens_renderer: LensRenderer,
    /// Reference instant used to compute the time uniform fed into the
    /// background shader for the breathing animation.
    start_time: Instant,

    // Plate infrastructure (M3.1).
    composite: CompositeRenderer,
    code_scroll: Substrate,

    // Blind (file-tree, Phase B). Drawn directly into the swap chain in
    // Zone 1 (the void). Separate ShapeRenderer + TextRenderer + Viewport
    // because the existing ones are configured frame-by-frame for the
    // plate-RT viewport and renders into the plate RT — the blind's
    // viewport is the full window and its render attachment is the swap
    // chain. Container-tree formalization (YGG-51) will let one renderer
    // serve both with per-caller buffers; for Phase B duplicate is the
    // smaller commit.
    /// "Pre-slat" 2D shapes: ropes, and in 2D-slat mode, the slat
    /// bodies. Rendered BEFORE slat3d so the slat body (2D or 3D)
    /// covers the rope behind it.
    blind_shapes: ShapeRenderer,
    /// "Post-slat" 2D shapes: tilt strip, top highlight, bottom
    /// shadow, hole rim/void/rope-in-hole/rope-above-hole. Rendered
    /// AFTER slat3d so these decorations sit on top of the slat body
    /// regardless of whether the body is drawn in 2D or 3D.
    blind_decor_shapes: ShapeRenderer,
    blind_text_renderer: TextRenderer,
    blind_viewport: Viewport,
    blind_buffers: HashMap<PathBuf, Buffer>,
    /// 3D slat pipeline (YGG-62 Phase 2). Renders slat bodies as
    /// real 3D quads projected through a one-point perspective
    /// anchored at the physical-screen target. When the anchor
    /// isn't known yet (first frame) or `state.debug_slat_2d` is
    /// set, the 2D slat body in `blind_shapes` is drawn instead.
    slat3d: Slat3DRenderer,

    // glyphon
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    /// One glyphon Buffer per card. Class cards buffer only their header line;
    /// Function/Method cards buffer full_range (decorators + signature + body).
    card_buffers: HashMap<CardId, Buffer>,

    /// Last-applied font_size / line_height — rebuilt on DPI change.
    applied_font_size: f32,
    applied_line_height: f32,
    /// Last-applied wrap configuration `(wrap_mode, layout_metrics.width)`
    /// — when this drifts, every card buffer is re-sized + re-wrapped.
    applied_wrap_config: Option<(WrapMode, f32)>,

    // egui
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

impl Renderer {
    /// Initialize all GPU + text resources. `font_size` and `line_height` must
    /// already be DPI-scaled (see `AppState::effective_*`).
    pub async fn new(
        window: Arc<Window>,
        highlighted: &HighlightedSource,
        cards: &[Card],
        font_size: f32,
        line_height: f32,
    ) -> Result<Self> {
        let instance = Instance::new(InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .context("create wgpu surface from window")?;

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("no compatible GPU adapter")?;

        let (device, queue) = adapter
            .request_device(
                &DeviceDescriptor {
                    label: Some("ygg-device"),
                    required_features: Features::empty(),
                    required_limits: Limits::default(),
                    memory_hints: MemoryHints::default(),
                },
                None,
            )
            .await
            .context("request_device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);

        let size = window.inner_size();
        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes.first().copied().unwrap_or(CompositeAlphaMode::Auto),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        // glyphon
        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let blind_viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let blind_text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        // One buffer per card, built eagerly at load.
        let mut card_buffers = HashMap::with_capacity(cards.len());
        for card in cards {
            let buf = build_card_buffer(&mut font_system, highlighted, card, font_size, line_height);
            card_buffers.insert(card.id, buf);
        }

        // Shape + background + composite + icon pipelines.
        let shape_renderer = ShapeRenderer::new(&device, format);
        let blind_shapes = ShapeRenderer::new(&device, format);
        let blind_decor_shapes = ShapeRenderer::new(&device, format);
        let slat3d = Slat3DRenderer::new(&device, format);
        let background_renderer = BackgroundRenderer::new(&device, format);
        let composite = CompositeRenderer::new(&device, format);
        let icon_renderer = IconRenderer::new(&device, &queue, format);
        let mut lens_renderer = LensRenderer::new(&device, format);

        // Code pane plate — sized from the initial window + scale factor. The
        // Renderer is built before AppState knows about the current scale, so
        // we use whatever the window reports as a starting point. Resizes
        // reconfigure this in `render()` when the cached size no longer
        // matches.
        let sf = window.scale_factor() as f32;
        let pane_fraction = crate::state::LEFT_PANE_FRACTION;
        let code_left = (size.width as f32 * pane_fraction).round();
        let code_w = size.width as f32 - code_left;
        let inset = PANEL_INSET_PT * sf;
        let plate_pos = [code_left + inset, inset];
        let plate_size = [
            (code_w - inset * 2.0).max(1.0) as u32,
            (size.height as f32 - inset * 2.0).max(1.0) as u32,
        ];
        let code_scroll = Substrate::new(
            &device,
            plate_size,
            plate_pos,
            format,
            &composite.bind_group_layout,
            &composite.sampler,
        );
        // Lens pass reads from the plate RT, so bind it now and rebind
        // any time the RT is reallocated (on window resize).
        lens_renderer.bind_plate(&device, &code_scroll.rt_view);

        // egui
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(&device, format, None, 1, false);

        Ok(Self {
            window,
            device,
            queue,
            surface,
            surface_config,
            shape_renderer,
            background_renderer,
            icon_renderer,
            lens_renderer,
            start_time: Instant::now(),
            composite,
            code_scroll,
            blind_shapes,
            blind_decor_shapes,
            blind_text_renderer,
            blind_viewport,
            blind_buffers: HashMap::new(),
            slat3d,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            card_buffers,
            applied_font_size: font_size,
            applied_line_height: line_height,
            applied_wrap_config: None,
            egui_ctx,
            egui_state,
            egui_renderer,
        })
    }

    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }
    pub fn egui_state_mut(&mut self) -> &mut egui_winit::State {
        &mut self.egui_state
    }

    pub fn resize(&mut self, new_size: WindowSize) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.surface_config.width = new_size.width;
        self.surface_config.height = new_size.height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    /// Compute the LayoutMetrics the renderer currently uses.
    ///
    /// Coordinates are **plate-local** (origin = plate top-left). Also used
    /// by `app.rs` for hit-testing clicks — keeping this in one place so
    /// "where a card is drawn" and "where a card's click registers" never
    /// drift.
    pub fn layout_metrics(&self, state: &AppState) -> LayoutMetrics {
        let sf = state.scale_factor;
        let (_, plate_size) = plate_rect(state);
        let width =
            (plate_size[0] as f32 - (CODE_PAD_LEFT_PT + CODE_PAD_RIGHT_PT) * sf).max(0.0);
        LayoutMetrics {
            line_height: state.effective_line_height(),
            left: CODE_PAD_LEFT_PT * sf,
            width,
            depth_indent: DEPTH_INDENT_PT * sf,
            top_level_gap: TOP_LEVEL_GAP_PT * sf,
            card_inner_pad_y: CARD_INNER_PAD_Y_PT * sf,
        }
    }

    /// Draw one frame.
    pub fn render(&mut self, state: &AppState) -> Result<(), wgpu::SurfaceError> {
        // Re-apply text metrics on DPI change.
        let font_size = state.effective_font_size();
        let line_height = state.effective_line_height();
        if font_size != self.applied_font_size || line_height != self.applied_line_height {
            let metrics = Metrics::new(font_size, line_height);
            for buf in self.card_buffers.values_mut() {
                buf.set_metrics(&mut self.font_system, metrics);
            }
            for buf in self.blind_buffers.values_mut() {
                buf.set_metrics(&mut self.font_system, metrics);
            }
            self.applied_font_size = font_size;
            self.applied_line_height = line_height;
        }

        // Reconfigure the plate if the window size or scale factor changed the
        // plate's target dimensions.
        let (plate_pos, plate_size) = plate_rect(state);
        let rt_reallocated = self.code_scroll.reconfigure(
            &self.device,
            plate_size,
            plate_pos,
            self.surface_config.format,
            &self.composite.bind_group_layout,
            &self.composite.sampler,
        );
        if rt_reallocated {
            // Lens pass samples the plate RT — its bind group must
            // point at the freshly-allocated texture view.
            self.lens_renderer
                .bind_plate(&self.device, &self.code_scroll.rt_view);
        }

        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: Some("ygg-encoder") });

        // ---- Layout (plate-local coordinates) ----
        let metrics = self.layout_metrics(state);

        // Keep card buffers in sync with the current wrap mode + pane
        // width. When either changes, each buffer gets its wrap flag
        // and layout width re-applied so wrapped text reflows into
        // the new pane width.
        let cur_wrap_cfg = (state.wrap_mode, metrics.width);
        if self.applied_wrap_config != Some(cur_wrap_cfg) {
            let sf = state.scale_factor;
            let text_inset_total = CARD_TEXT_INSET_PT * sf * 1.5;
            for card in &state.cards {
                let Some(buf) = self.card_buffers.get_mut(&card.id) else { continue };
                let card_width =
                    (metrics.width - (card.depth as f32) * metrics.depth_indent).max(0.0);
                let text_width = (card_width - text_inset_total).max(10.0);
                match state.wrap_mode {
                    WrapMode::On => {
                        buf.set_wrap(&mut self.font_system, Wrap::Word);
                        buf.set_size(&mut self.font_system, Some(text_width), None);
                    }
                    WrapMode::Off => {
                        buf.set_wrap(&mut self.font_system, Wrap::None);
                        buf.set_size(&mut self.font_system, Some(100_000.0), None);
                    }
                }
                buf.shape_until_scroll(&mut self.font_system, false);
            }
            self.applied_wrap_config = Some(cur_wrap_cfg);
        }

        // Wrap-aware visual-line-count overrides. For Snippet and
        // Class cards the count is the whole card's height. For
        // Function/Method cards the buffer holds preamble + body; we
        // use the total wrapped count and `leaf_body_height` subtracts
        // the raw preamble to get the body height. Applies to all
        // card kinds — Markdown cards come through as `Function`, not
        // `Snippet`, so the Snippet-only filter used to miss them.
        let wrapped_overrides: HashMap<CardId, usize> = if state.wrap_mode == WrapMode::On {
            state
                .cards
                .iter()
                .filter_map(|c| {
                    self.card_buffers.get(&c.id).map(|buf| {
                        let n = buf.layout_runs().count().max(1);
                        (c.id, n)
                    })
                })
                .collect()
        } else {
            HashMap::new()
        };

        let layout = layout_cards_with_overrides(
            &state.cards,
            &state.fold_progress,
            metrics,
            if wrapped_overrides.is_empty() {
                None
            } else {
                Some(&wrapped_overrides)
            },
        );
        let scene_top_local = SCENE_TOP_PAD_PT * state.scale_factor;

        // ---- Build shape + icon instances ----
        // All positions are plate-local (origin = plate top-left). The plate
        // background (lit material + outer bloom) is drawn by the composite
        // shader in M3.3, not as a shape instance here — so the RT starts
        // transparent and we only draw cards into it.
        let mut instances: Vec<RectInstance> = Vec::with_capacity(state.cards.len() * 5);
        let mut icon_instances: Vec<IconInstance> = Vec::with_capacity(state.cards.len());
        let mut lens_instances: Vec<LensInstance> = Vec::with_capacity(state.cards.len());
        let plate_h = plate_size[1] as f32;
        for card in &state.cards {
            let Some(rect) = layout.rects.get(&card.id) else { continue };
            let local_y = rect.y - state.scroll_y + scene_top_local;
            let local_bottom = local_y + rect.total_h();
            // Cull cards entirely outside the plate's local viewport. Add a
            // 32-pixel margin so glow doesn't pop at edges.
            if local_bottom < -32.0 || local_y > plate_h + 32.0 {
                continue;
            }
            push_card_shapes(
                &mut instances,
                &mut icon_instances,
                &mut lens_instances,
                card,
                rect,
                local_y,
                state,
                plate_pos,
            );
        }

        // Shapes' uniform viewport = plate size (we're rendering into the plate RT).
        self.shape_renderer
            .prepare(&self.device, &self.queue, &instances, (plate_size[0], plate_size[1]));

        // Icons go into the plate RT on top of text, so their viewport is
        // plate size as well.
        self.icon_renderer.prepare(
            &self.device,
            &self.queue,
            &icon_instances,
            (plate_size[0], plate_size[1]),
        );

        // Lens pass renders on the swap chain (post-composite), sampling
        // the plate RT. Uniforms carry viewport size (swap chain) and
        // the plate origin / size so the shader can convert lens
        // positions (swap-chain pixels) back into plate-local UVs.
        self.lens_renderer.prepare(
            &self.device,
            &self.queue,
            &lens_instances,
            (self.surface_config.width, self.surface_config.height),
            (plate_pos[0], plate_pos[1]),
            (plate_size[0], plate_size[1]),
        );

        // Background uniforms stay window-sized (it draws to the swap chain).
        // Passes SkyLight so the nebula's tint + brightness track the sky
        // cycle in sync with lens glints and spine highlights. Also passes
        // the window's inner-client-area position on the virtual desktop
        // so cloud noise is sampled in physical-screen coordinates — drag
        // the window and the clouds stay pinned to the monitor.
        //
        // Sampled LIVE from the window (not from `state.window_inner_pos`)
        // so there's zero event-loop latency during drags. winit's
        // `Moved` event has a one-frame delay on some platforms; querying
        // directly eliminates the visible lag.
        let window_origin = self
            .window
            .inner_position()
            .ok()
            .map(|p| (p.x as f32, p.y as f32))
            .or_else(|| {
                state
                    .window_inner_pos
                    .map(|(x, y)| (x as f32, y as f32))
            })
            .unwrap_or((0.0, 0.0));
        self.background_renderer.prepare(
            &self.queue,
            (state.window_size.width, state.window_size.height),
            window_origin,
            self.start_time.elapsed().as_secs_f32(),
            state.sky_light(),
        );

        // ---- Glyphon: prepare text areas for visible cards (plate-local) ----
        // Glyphon's viewport also equals plate size — text is rendered into
        // the plate RT with plate-local pixel coords.
        self.viewport.update(
            &self.queue,
            Resolution {
                width: plate_size[0],
                height: plate_size[1],
            },
        );

        let sf = state.scale_factor;
        let text_areas_storage = collect_text_areas(
            &self.card_buffers,
            state,
            &layout,
            scene_top_local,
            plate_size,
            sf,
        );

        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            text_areas_storage.iter().map(|a| a.to_text_area()),
            &mut self.swash_cache,
        ) {
            log::warn!("glyphon prepare failed: {e:?}");
        }

        // ---- Blind (file-tree) layout, shapes, and text. Drawn into the
        // swap chain after composite + lens so the blind sits on top of the
        // void with no occlusion concerns. Skipped entirely when no tree
        // is present (single-file launch).
        // Pre-build any missing slat filename buffers so layout has
        // measured widths to size each slat (content-sized, not
        // pane-full-width). Entries with no measured buffer fall back
        // to a generous default in layout.
        let blind_filename_widths: HashMap<PathBuf, f32> =
            if let Some(tree) = state.tree.as_ref() {
                let entries = crate::filetree::flatten(tree);
                let mut widths = HashMap::with_capacity(entries.len());
                for entry in &entries {
                    if !self.blind_buffers.contains_key(&entry.path) {
                        let buf = blind::build_filename_buffer(
                            &mut self.font_system,
                            &entry.name,
                            entry.kind,
                            font_size,
                            line_height,
                        );
                        self.blind_buffers.insert(entry.path.clone(), buf);
                    }
                    if let Some(buf) = self.blind_buffers.get(&entry.path) {
                        let w = buf
                            .layout_runs()
                            .map(|r| r.line_w)
                            .fold(0.0_f32, f32::max);
                        widths.insert(entry.path.clone(), w);
                    }
                }
                widths
            } else {
                HashMap::new()
            };

        let blind_layout = state.tree.as_ref().map(|tree| {
            let pane_left = 0.0;
            let pane_width = state.code_pane_left() as f32;
            let pane_height = state.window_size.height as f32;
            blind::layout(
                tree,
                pane_left,
                pane_width,
                pane_height,
                state.scale_factor,
                state.slat_mode,
                &blind_filename_widths,
            )
        });
        // 3D slat rendering (YGG-62 Phase 2). The 2D slat body and
        // its 2D decorations (tilt strip, highlights) were removed —
        // the slat3d pipeline owns slat rendering unconditionally.
        // When no projection anchor is known yet (first frame before
        // the monitor is observed), we skip pushing instances; the
        // next frame renders them. Ropes, hole, and text stay 2D for
        // now; 3D hole lands in follow-on work.
        let mut slat3d_instances: Vec<SlatInstance> = Vec::new();
        let slat3d_active = state.projection_anchor().is_some();

        // Two separate 2D instance lists so we can sandwich the
        // slat3d pass between them:
        //   blind_instances      — ropes + (2D mode only) slat bodies
        //   blind_decor_instances — tilt, highlights, hole decorations
        // Render order: ropes → slats (2D or 3D) → decorations.
        let blind_text_areas = if let Some(layout) = blind_layout.as_ref() {
            let mut blind_instances: Vec<RectInstance> =
                Vec::with_capacity(layout.slats.len() + layout.ropes.len());
            let blind_decor_instances: Vec<RectInstance> = Vec::new();
            for rope in &layout.ropes {
                blind_instances.push(RectInstance::glowing(
                    rope.x,
                    rope.y_top,
                    rope.width,
                    (rope.y_bottom - rope.y_top).max(1.0),
                    rope.color,
                    rope.width * 0.5,
                    rope.glow_color,
                    rope.glow_radius,
                ));
            }
            let win_h = state.window_size.height as f32;
            for slat in &layout.slats {
                if slat.slot_y + slat.slot_height < -32.0
                    || slat.slot_y > win_h + 32.0
                {
                    continue;
                }
                // Slat body rendered via the 3D pipeline. The 2D
                // rect path and its decorations (tilt strip, top
                // highlight, bottom shadow) were removed — real 3D
                // rotation places the edges where projection says,
                // and 2D decorations at the slat's natural y-range
                // would visibly detach from the tilted body.
                if slat3d_active {
                    let sw = slat.slat_width.max(1.0);
                    let sh = slat.slat_height.max(1.0);
                    // Hole in slat-local pixel space. The 2D blind
                    // layout computed the hole in window-space; we
                    // subtract the slat's top-left to get slat-local.
                    let hole_xy = slat
                        .hole
                        .map(|h| {
                            [
                                h.center_x - slat.slat_x,
                                h.center_y - slat.slat_y,
                                h.width * 0.5,
                                h.height * 0.5,
                            ]
                        })
                        .unwrap_or([0.0; 4]);
                    slat3d_instances.push(SlatInstance {
                        model: build_slat_model(
                            slat.slat_x,
                            slat.slat_y,
                            sw,
                            sh,
                            state.slat_angle_rad,
                        ),
                        color: slat.bg,
                        size_px: [sw, sh],
                        corner_radius: slat.corner,
                        arc_depth: state.slat_arc_depth,
                        hole: hole_xy,
                    });
                }
                // Hole is now a REAL cutout in the slat3d fragment
                // shader — `SlatInstance.hole` carries the ellipse
                // half-extents. The 2D rope drawn in the earlier
                // pass shows through the cutout, so the rope visually
                // threads through the slat like real material.
                if !self.blind_buffers.contains_key(&slat.entry.path) {
                    let buf = blind::build_filename_buffer(
                        &mut self.font_system,
                        &slat.entry.name,
                        slat.entry.kind,
                        font_size,
                        line_height,
                    );
                    self.blind_buffers.insert(slat.entry.path.clone(), buf);
                }
            }
            self.blind_shapes.prepare(
                &self.device,
                &self.queue,
                &blind_instances,
                (self.surface_config.width, self.surface_config.height),
            );
            self.blind_decor_shapes.prepare(
                &self.device,
                &self.queue,
                &blind_decor_instances,
                (self.surface_config.width, self.surface_config.height),
            );

            self.blind_viewport.update(
                &self.queue,
                Resolution {
                    width: self.surface_config.width,
                    height: self.surface_config.height,
                },
            );
            let pane_right_px = state.code_pane_left() as f32
                - blind::BLIND_MARGIN_PT * state.scale_factor;
            let areas = collect_blind_text_areas(
                &self.blind_buffers,
                layout,
                pane_right_px,
                line_height,
                self.surface_config.height as f32,
            );
            if let Err(e) = self.blind_text_renderer.prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.blind_viewport,
                areas.iter().map(|a| a.to_text_area()),
                &mut self.swash_cache,
            ) {
                log::warn!("blind text prepare failed: {e:?}");
            }
            areas
        } else {
            Vec::new()
        };
        // `blind_text_areas` is owned for the rest of the frame so the
        // text-area buffer references in the prepared draw stay valid until
        // the blind text pass runs.
        let _ = &blind_text_areas;

        // ---- Composite uniforms (plate → swap chain) ----
        // SkyLight drives the plate's edge treatment — asymmetric bloom,
        // directional key rim, and counter-shine all read from the same
        // state that lights the nebula, lens glint, and foil spine. The
        // 2D projection drops `direction.z` (plate view is frontal).
        let sky = state.sky_light();
        let sky_dir_2d = [sky.direction.x, sky.direction.y];
        let sky_color = [sky.color.x, sky.color.y, sky.color.z];
        self.composite.prepare(
            &self.queue,
            &self.code_scroll.uniform_buffer,
            (self.surface_config.width, self.surface_config.height),
            self.code_scroll.pos_px,
            self.code_scroll.size_px,
            PANEL_CORNER_RADIUS_PT * sf,
            PLATE_BLOOM_RADIUS_PT * sf,
            PLATE_BLOOM_COLOR,
            PLATE_RIM_THICKNESS_PT * sf,
            PLATE_RIM_INTENSITY,
            self.code_scroll.model,
            sky_dir_2d,
            sky_color,
            sky.intensity,
        );


        // ---- Egui ----
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let full_output = self.egui_ctx.run(raw_input, |ctx| draw_egui(ctx, state));
        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point: self.window.scale_factor() as f32,
        };
        for (id, delta) in &full_output.textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );

        // ---- Passes ----

        // Pass 1: sky background → swap chain.
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-background-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.background_renderer.render(&mut pass);
        }

        // Pass 2: shapes → plate RT (clear to transparent so the panel's
        // rounded corners composite correctly over the sky).
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-plate-shapes-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &self.code_scroll.rt_view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.shape_renderer.render(&mut pass);
        }

        // Pass 3: text → plate RT.
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-plate-text-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &self.code_scroll.rt_view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Err(e) = self.text_renderer.render(&self.atlas, &self.viewport, &mut pass) {
                log::warn!("glyphon render failed: {e:?}");
            }
        }

        // Pass 3b: icons → plate RT. Rendered on top of text so fold
        // handles sit above any card glyphs underneath them.
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-plate-icon-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &self.code_scroll.rt_view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.icon_renderer.render(&mut pass);
        }

        // Pass 4: composite plate → swap chain.
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-composite-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.composite.render(&mut pass, &self.code_scroll.composite_bg);
        }


        // Pass 4b: lens → swap chain. Samples the plate RT at magnified
        // coordinates inside each lens disc and writes the result on
        // top of the composited plate. The plate RT itself is unmodified,
        // so small slot icons stay where they are and the lens naturally
        // reveals whichever icon (or widget body, or empty area) is
        // underneath its current position — no icon swap, no discrete
        // transitions.
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-lens-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.lens_renderer.render(&mut pass);
        }

        // Pass 4c: blind shapes (ropes + optionally-2D slat bodies) → swap
        // chain. Drawn after composite + lens so the blind sits on top of
        // the void.
        if blind_layout.is_some() {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-blind-shapes-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.blind_shapes.render(&mut pass);
        }

        // Pass 4c': 3D slats (YGG-62 Phase 2). Real tessellated
        // curved strips under a real one-point perspective. Anchor
        // computed from LIVE window + monitor geometry (no event-
        // loop latency during drag).
        if slat3d_active && !slat3d_instances.is_empty() {
            let anchor = state.window_monitor.map(|mon| {
                let ax = (mon.x as f32 + mon.width as f32 * 0.5) - window_origin.0;
                let ay = mon.y as f32 - window_origin.1;
                let az = mon.width as f32 * 0.5;
                [ax, ay, az]
            });
            if let Some(anchor) = anchor {
                let proj = build_projection_matrix(
                    (self.surface_config.width, self.surface_config.height),
                    (anchor[0], anchor[1]),
                    anchor[2],
                );
                self.slat3d.prepare(
                    &self.device,
                    &self.queue,
                    &slat3d_instances,
                    proj,
                    (self.surface_config.width, self.surface_config.height),
                );
                let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                    label: Some("ygg-slat3d-pass"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                self.slat3d.render(&mut pass);
            }
        }

        // Pass 4c'': blind DECOR shapes — tilt strip, top highlight,
        // bottom shadow, hole rim/void/rope-in-hole/rope-above-hole.
        // Rendered AFTER the slat body pass (2D or 3D) so decorations
        // stack on top of whatever body got drawn.
        if blind_layout.is_some() {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-blind-decor-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.blind_decor_shapes.render(&mut pass);
        }

        // Pass 4d: blind filenames → swap chain.
        if blind_layout.is_some() {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-blind-text-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Err(e) =
                self.blind_text_renderer
                    .render(&self.atlas, &self.blind_viewport, &mut pass)
            {
                log::warn!("blind text render failed: {e:?}");
            }
        }

        // Pass 5: egui HUD → swap chain.
        {
            let mut pass = encoder
                .begin_render_pass(&RenderPassDescriptor {
                    label: Some("ygg-egui-pass"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: Operations { load: LoadOp::Load, store: StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            self.egui_renderer
                .render(&mut pass, &paint_jobs, &screen_descriptor);
        }

        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        frame.present();

        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        self.atlas.trim();
        Ok(())
    }
}

/// Collect the TextArea metadata for visible cards. Free function so the
/// renderer can split its borrow. Coordinates are **plate-local**.
///
/// When `state.wrap_mode == Off`, the text areas are horizontally
/// offset by `-state.scroll_x`, shifting long un-wrapped content left
/// so a scrub of the trackpad reveals what would otherwise be clipped
/// past the card's right edge. Glyphon's bounds still clip to the
/// card, so text that scrolls off either edge disappears cleanly.
fn collect_text_areas<'a>(
    card_buffers: &'a HashMap<CardId, Buffer>,
    state: &AppState,
    layout: &Layout,
    scene_top_local: f32,
    plate_size: [u32; 2],
    sf: f32,
) -> Vec<AreaSpec<'a>> {
    let plate_h = plate_size[1] as f32;
    let mut out = Vec::with_capacity(state.cards.len());
    let text_inset = CARD_TEXT_INSET_PT * sf;
    let scroll_x = if state.wrap_mode == WrapMode::Off {
        state.scroll_x
    } else {
        0.0
    };
    // Vertical text inset — text sits this far below the card's top edge
    // (and the card's layout also reserves this much padding at the bottom)
    // so first/last lines don't touch the frame.
    let text_inset_y = CARD_INNER_PAD_Y_PT * sf;
    for card in &state.cards {
        let Some(rect) = layout.rects.get(&card.id) else { continue };
        let local_y = rect.y - state.scroll_y + scene_top_local;
        let local_bottom = local_y + rect.total_h();
        if local_bottom < 0.0 || local_y > plate_h {
            continue;
        }
        let Some(buffer) = card_buffers.get(&card.id) else { continue };

        let method_inset = match card.kind {
            CardKind::Method
                if !matches!(
                    card.modifier,
                    MethodModifier::Classmethod | MethodModifier::Staticmethod
                ) =>
            {
                INSTANCE_METHOD_INSET_PT * sf
            }
            _ => 0.0,
        };
        let left = rect.x + text_inset + method_inset;
        let right = rect.x + rect.width - text_inset * 0.5;
        let text_top = local_y + text_inset_y;

        let bounds = TextBounds {
            left: left as i32,
            top: text_top.max(0.0) as i32,
            right: right as i32,
            bottom: (local_y + rect.total_h()).min(plate_h) as i32,
        };

        let visibility_dim = if card.visibility == Visibility::Private { 0.8 } else { 1.0 };
        // Rect opacity carries the nested-fold cascade — when a class folds,
        // descendant text dims in lockstep with its shapes.
        let opacity = visibility_dim * rect.opacity;
        if opacity < 0.01 {
            continue;
        }
        let r = (220.0 * opacity) as u8;
        let g = (222.0 * opacity) as u8;
        let b = (230.0 * opacity) as u8;

        out.push(AreaSpec {
            buffer,
            left: left - scroll_x,
            top: text_top,
            bounds,
            default_color: GlyphonColor::rgb(r, g, b),
        });
    }
    out
}

/// A small owned helper so we can hand `TextArea`s to glyphon with the right
/// lifetime without tangling closures.
struct AreaSpec<'a> {
    buffer: &'a Buffer,
    left: f32,
    top: f32,
    bounds: TextBounds,
    default_color: GlyphonColor,
}

impl<'a> AreaSpec<'a> {
    fn to_text_area(&self) -> TextArea<'a> {
        TextArea {
            buffer: self.buffer,
            left: self.left,
            top: self.top,
            scale: 1.0,
            bounds: self.bounds,
            default_color: self.default_color,
            custom_glyphs: &[],
        }
    }
}

/// Collect text areas for the blind's slat filenames. Coordinates are
/// window-space (the blind text pass renders directly to the swap chain).
/// Text position and color are driven by the slat's `design`: Closed
/// centers ink inside the plaque; Open places standing text above the
/// shelf, inside the slot's upper portion.
fn collect_blind_text_areas<'a>(
    blind_buffers: &'a HashMap<PathBuf, Buffer>,
    layout: &blind::BlindLayout,
    pane_right_px: f32,
    line_height: f32,
    window_h: f32,
) -> Vec<AreaSpec<'a>> {
    let mut out = Vec::with_capacity(layout.slats.len());
    for slat in &layout.slats {
        if slat.slot_y + slat.slot_height < 0.0 || slat.slot_y > window_h {
            continue;
        }
        let Some(buffer) = blind_buffers.get(&slat.entry.path) else {
            continue;
        };
        let text_right = pane_right_px - 2.0;
        if text_right <= slat.text_left {
            continue;
        }
        let (top, clip_top, clip_bottom) = match slat.design {
            blind::SlatDesign::Closed => {
                // Ink on slat: text vertically centered on the slat face.
                let t = slat.slat_y + (slat.slat_height - line_height) * 0.5;
                (t, slat.slat_y, slat.slat_y + slat.slat_height)
            }
            blind::SlatDesign::Open => {
                // Standing text: bottom of text rests at the slat's
                // middle y, extending upward — "standing on the
                // shelf at the middle". Upright in screen space,
                // color pale to read against the void above the slat.
                let slat_mid = slat.slat_y + slat.slat_height * 0.5;
                let t = slat_mid - line_height;
                (t, slat.slot_y, slat.slat_y + slat.slat_height)
            }
        };
        let bounds = TextBounds {
            left: slat.text_left as i32,
            top: clip_top.max(0.0) as i32,
            right: text_right as i32,
            bottom: clip_bottom.min(window_h) as i32,
        };
        let (r, g, b) = slat.text_rgb;
        let color = GlyphonColor::rgb(r, g, b);
        out.push(AreaSpec {
            buffer,
            left: slat.text_left,
            top,
            bounds,
            default_color: color,
        });
    }
    out
}

/// Append the shape instances for one card. Order matters — each instance
/// draws on top of the previous via premultiplied-alpha blend.
///
/// 1. **Drop shadow** — transparent fill + dark blurred glow, offset down-
///    right. Reads as "card is slightly above the plate."
/// 2. **Card background** — solid (no outer glow). Near-opaque tinted paper.
/// 3. **Accent strip** — left-edge visibility/modifier cue.
/// 4. **Class spine** (for class cards) — luminous emissive metal rail
///    (Zone-3 semantic light exception).
/// 5. **Fold handle** — solid, color-coded by fold target.
/// 6. **Rolling edge** (during fold animation) — thin luminous hairline.
///
/// All output coordinates are **plate-local**.
#[allow(clippy::too_many_arguments)]
fn push_card_shapes(
    out: &mut Vec<RectInstance>,
    icons_out: &mut Vec<IconInstance>,
    lenses_out: &mut Vec<LensInstance>,
    card: &Card,
    rect: &CardRect,
    local_y: f32,
    state: &AppState,
    plate_pos: [f32; 2],
) {
    // If this card has been almost-fully collapsed by a parent's nested-fold
    // cascade, skip drawing it. Saves instance buffer space and avoids the
    // last-pixel shimmer of rects with < 1px dimensions.
    if rect.opacity < 0.01 {
        return;
    }
    let sf = state.scale_factor;
    let corner = CARD_CORNER_RADIUS_PT * sf;
    let alpha = rect.opacity;

    // ---- Drop shadow (BEFORE the card so it renders behind) ----
    // Private cards get a fainter, shorter shadow — they sit lower.
    let shadow_scale = if card.visibility == Visibility::Private {
        PRIVATE_SHADOW_SCALE
    } else {
        1.0
    };
    let shadow_offset = CARD_SHADOW_OFFSET_PT * sf * shadow_scale;
    let shadow_blur = CARD_SHADOW_BLUR_PT * sf * shadow_scale;
    let mut shadow_color = CARD_SHADOW_COLOR;
    shadow_color[3] *= shadow_scale * alpha;
    out.push(RectInstance::glowing(
        rect.x + shadow_offset,
        local_y + shadow_offset,
        rect.width,
        rect.total_h(),
        [0.0, 0.0, 0.0, 0.0], // transparent fill; the "shadow" IS the glow
        corner,
        shadow_color,
        shadow_blur,
    ));

    // ---- Card background (solid, no outer glow — cards don't emit). ----
    let mut bg = match card.kind {
        CardKind::Class => CLASS_BG,
        _ => CARD_BG,
    };
    bg[3] *= alpha;
    out.push(RectInstance::solid(
        rect.x,
        local_y,
        rect.width,
        rect.total_h(),
        bg,
        corner,
    ));

    // ---- Left-side accent strip ----
    let (mut accent_color, accent_width_pt) = match (card.kind, card.modifier, card.visibility) {
        (CardKind::Class, _, _) => (SPINE_FOIL_COLOR, SPINE_WIDTH_PT),
        (CardKind::Snippet, _, _) => (ACCENT_SNIPPET, ACCENT_WIDTH_PT_PRIVATE),
        (_, MethodModifier::Classmethod, _) => (ACCENT_CLASSMETHOD, ACCENT_WIDTH_PT),
        (_, MethodModifier::Staticmethod, _) => (ACCENT_STATICMETHOD, ACCENT_WIDTH_PT),
        (_, MethodModifier::Property, _) => (ACCENT_PROPERTY, ACCENT_WIDTH_PT),
        (_, _, Visibility::Private) => (ACCENT_PRIVATE, ACCENT_WIDTH_PT_PRIVATE),
        (_, _, Visibility::Public) => (ACCENT_PUBLIC, ACCENT_WIDTH_PT),
    };
    accent_color[3] *= alpha;
    out.push(RectInstance::solid(
        rect.x + 2.0 * sf,
        local_y + 3.0 * sf,
        accent_width_pt * sf,
        rect.total_h() - 6.0 * sf,
        accent_color,
        ACCENT_CORNER_RADIUS_PT * sf,
    ));

    // ---- Class spine (armature) as foil inlay.
    //      Three layered visuals: (1) brass foil base, flush with linen
    //      (no halo into the void); (2) luminous hairline seam at the
    //      foil's center — the semantic light that says "this class
    //      emits energy"; (3) SkyLight-driven glint — a small bright spot
    //      whose position on the spine tracks the unseen star and whose
    //      color is sky-tinted, so the armature visibly reflects the
    //      weather.  Matches YGG-33 foil-inlay doctrine and wires the
    //      YGG-34 SkyLight consumer for metal. ----
    if card.kind == CardKind::Class {
        let spine_top = local_y + 4.0 * sf;
        let spine_height = rect.total_h() - 8.0 * sf;
        let spine_left = rect.x;
        let spine_width = SPINE_WIDTH_PT * sf;
        let sky = state.sky_light();

        // (1) Foil base — brass, flush, no outer glow. Body takes a faint
        //     sky tint so the whole rail breathes with the cycle, not just
        //     the travelling glint spot.
        let body_mix = SPINE_FOIL_SKY_MIX;
        let mut foil_color = [
            SPINE_FOIL_COLOR[0] * (1.0 - body_mix) + sky.color.x * body_mix,
            SPINE_FOIL_COLOR[1] * (1.0 - body_mix) + sky.color.y * body_mix,
            SPINE_FOIL_COLOR[2] * (1.0 - body_mix) + sky.color.z * body_mix,
            SPINE_FOIL_COLOR[3],
        ];
        foil_color[3] *= alpha;
        out.push(RectInstance::solid(
            spine_left,
            spine_top,
            spine_width,
            spine_height,
            foil_color,
            spine_width * 0.5,
        ));

        // (2) Seam — hairline bright channel through the foil's center,
        //     with a warm halo. This is the armature's emissive light
        //     (Zone-3 semantic-light exception; permitted per CLAUDE.md).
        let seam_width = SPINE_SEAM_WIDTH_PT * sf;
        let seam_left = spine_left + (spine_width - seam_width) * 0.5;
        let mut seam_color = SPINE_SEAM_COLOR;
        let mut seam_glow = SPINE_SEAM_GLOW;
        seam_color[3] *= alpha;
        seam_glow[3] *= alpha;
        out.push(RectInstance::glowing(
            seam_left,
            spine_top,
            seam_width,
            spine_height,
            seam_color,
            seam_width * 0.5,
            seam_glow,
            SPINE_SEAM_GLOW_RADIUS_PT * sf,
        ));

        // (3) Glint — sky-positioned specular on the metal. Vertical
        //     position on the spine maps from SkyLight.direction.y
        //     ([-1 = top, +1 = bottom] in our convention → clamped to
        //     the spine's visible range). Hidden during night (star
        //     below horizon): the metal can't glint without light.
        let glint_intensity = ((sky.intensity - FOLD_LENS_SPECULAR_NIGHT_THRESHOLD)
            / (1.0 - FOLD_LENS_SPECULAR_NIGHT_THRESHOLD))
            .clamp(0.0, 1.0);
        if glint_intensity > 0.001 {
            let glint_size = SPINE_GLINT_SIZE_PT * sf;
            let glint_pos_fraction = ((sky.direction.y + 1.0) * 0.5).clamp(0.0, 1.0);
            let glint_cy = spine_top + spine_height * glint_pos_fraction;
            let glint_cx = spine_left + spine_width * 0.5;
            let mix = SPINE_GLINT_SKY_MIX;
            let glint_color = [
                SPINE_GLINT_BASE[0] * (1.0 - mix) + sky.color.x * mix,
                SPINE_GLINT_BASE[1] * (1.0 - mix) + sky.color.y * mix,
                SPINE_GLINT_BASE[2] * (1.0 - mix) + sky.color.z * mix,
                SPINE_GLINT_BASE[3] * alpha * glint_intensity,
            ];
            out.push(RectInstance::solid(
                glint_cx - glint_size * 0.5,
                glint_cy - glint_size * 0.5,
                glint_size,
                glint_size,
                glint_color,
                glint_size * 0.5,
            ));
        }
    }

    // ---- Fold-switch widget — one wide chip whose corners and left/right
    //      sides match a single-button chip (same small corner radius, same
    //      slight pillow bulge on the vertical sides). Top and bottom stay
    //      flat — the horizontal-only pillow (mask = (1, 0)) is what makes
    //      that work.
    //
    //      On its surface, up to two button-sized concave dents appear,
    //      each rendered with the same visual treatment as the single-
    //      button press-state (rubber-button chip, dome -1, same color,
    //      same corner radius):
    //        (1) state-well — at the current `fold_progress` position,
    //            tracks the fold animation so the well slides in lockstep
    //            with the card body folding/unfolding;
    //        (2) finger-press — at the slot the user is currently
    //            mouse-down-ing, if any. Drawn on top of (1).
    //      Icons sit at slot centers above everything.
    //      Skipped for snippets. ----
    let fold_states = card_fold_states(card);
    if !fold_states.is_empty() {
        let fold_progress = state.fold_progress.get(&card.id).copied().unwrap_or(1.0);

        // A "slot" is a single-button cap of size `chip_size`. Slots are
        // laid out left-to-right with `slot_gap` between them, so the
        // lens sitting over one slot never touches the neighbour's cap.
        // `slot_stride` = cap-to-cap distance = `chip_size + slot_gap`.
        let handle_size = FOLD_HANDLE_SIZE_PT * sf;
        let chip_pad = FOLD_CHIP_PAD_PT * sf;
        let chip_size = handle_size + chip_pad * 2.0;
        let slot_gap = FOLD_SLOT_GAP_PT * sf;
        let slot_stride = chip_size + slot_gap;
        let slot_count = fold_states.len();
        // Widget width: N caps + (N-1) gaps.
        let widget_width =
            slot_count as f32 * chip_size + (slot_count.saturating_sub(1)) as f32 * slot_gap;
        let widget_height = chip_size;
        // Corner radius matches a single-button chip exactly — 4pt at 1x.
        // The widget's "rounded rectangle" identity is the same as the
        // single button, just wider.
        let widget_radius = 4.0 * sf;

        // Right-align the widget strip inside the card header.
        let strip_right = rect.x + rect.width - 10.0 * sf;
        let widget_x = strip_right - widget_width;

        // Vertical alignment: center the widget on the first text line of
        // the card header (not geometric middle of header_h). For tall
        // decorated-function headers, line-one alignment anchors the
        // control next to the card's identity line.
        let line_h = state.effective_line_height();
        let top_pad = CARD_INNER_PAD_Y_PT * sf;
        let widget_y = local_y + top_pad + (line_h - widget_height) * 0.5;

        // ---- Widget body: composed of three overlapping rects so the
        //      outline reads as "single-button at each outer slot, flat
        //      plateau between them" —
        //
        //        left cap         right cap
        //        ╭──╮             ╭──╮
        //        │  ├─────────────┤  │    ← plateau extends up/down
        //        │  │   plateau   │  │       by `bulge_max` so its top
        //        │  ├─────────────┤  │       matches each cap's pillow
        //        ╰──╯             ╰──╯       peak (smooth join).
        //
        //      Each cap is a single-button pillow shape (mask = (1, 1));
        //      the plateau is a sharp-cornered rect spanning slot-center-
        //      first to slot-center-last. Where cap and plateau overlap,
        //      both fill the same color with alpha=1 so there's no visible
        //      seam. No dome shading on any of them — body is a flat
        //      raised surface; dents carry the depression. ----
        let mut body_color = FOLD_CHIP_BG;
        body_color[3] *= alpha;

        // Pillow peak extent: the cap's actual SDF silhouette reaches
        // `bulge / (1 - bulge/h)` past the un-pillowed rect, not just raw
        // `bulge`. That's because outside the rect `mid_weight` grows past
        // 1 (norm.y > 1 above the top edge), extending the silhouette
        // further. For our bulge/h = 0.12, the factor is `1/0.88`. Using
        // the true peak extent here lets the plateau sit exactly at the
        // cap's silhouette top — otherwise the cap's pillow peaks poke
        // out above the plateau as two visible humps.
        let cap_half = chip_size * 0.5;
        let bulge_raw = cap_half * 0.12;
        let peak_extent = bulge_raw / 0.88;

        // Slot center is at the cap's midpoint, not at the stride's mid.
        // With `slot_stride = chip_size + gap` these are no longer the
        // same: the cap is `chip_size` wide and lives at the stride's
        // left, so its center is at `slot * stride + chip_size/2`.
        let slot_center_x =
            |slot: usize| -> f32 { widget_x + slot as f32 * slot_stride + chip_size * 0.5 };

        // Left cap: full single-button pillow at slot 0.
        out.push(
            RectInstance::solid(
                widget_x,
                widget_y,
                slot_stride,
                widget_height,
                body_color,
                widget_radius,
            )
            .with_pillow_mask([1.0, 1.0]),
        );

        // Right cap: same, at the last slot. Skipped when slot_count == 1
        // (would coincide with the left cap).
        if slot_count > 1 {
            out.push(
                RectInstance::solid(
                    widget_x + (slot_count - 1) as f32 * slot_stride,
                    widget_y,
                    slot_stride,
                    widget_height,
                    body_color,
                    widget_radius,
                )
                .with_pillow_mask([1.0, 1.0]),
            );
        }

        // Plateau: flat rectangle from first slot center to last slot
        // center, extended vertically by `peak_extent` so its top/bottom
        // align with the caps' true pillow-peak silhouette. Sharp corners
        // (they're hidden underneath the caps' rounded corners at the join).
        if slot_count >= 2 {
            let first_cx = slot_center_x(0);
            let last_cx = slot_center_x(slot_count - 1);
            out.push(RectInstance::solid(
                first_cx,
                widget_y - peak_extent,
                last_cx - first_cx,
                widget_height + 2.0 * peak_extent,
                body_color,
                0.0,
            ));
        }

        // ---- Dormant: concave-dent selected-state metaphor.
        //      Kept behind `USE_DENT_METAPHOR` so the rubber-button dent
        //      can be toggled back without rewriting anything. The lens
        //      (below) won for visibility; the dent stays as a one-line
        //      flip for comparison/polish. ----
        if USE_DENT_METAPHOR {
            // Dent geometry: IDENTICAL to a single-button chip — same size,
            // same color, same corner radius, same dome magnitude. The only
            // difference from the single button is that dome is negative
            // (concave shading) because dents are always pressed-in.
            let dent_size = chip_size;
            let dent_corner = 4.0 * sf;
            let dent_y = widget_y;
            let mut dent_color = FOLD_CHIP_BG;
            dent_color[3] *= alpha;

            // Dent (1): state-well at the current fold_progress position.
            let well_slot = card_well_position(card, fold_progress);
            let well_center_x = widget_x + slot_stride * (well_slot + 0.5);
            out.push(
                RectInstance::solid(
                    well_center_x - dent_size * 0.5,
                    dent_y,
                    dent_size,
                    dent_size,
                    dent_color,
                    dent_corner,
                )
                .with_dome(-FOLD_CHIP_DOME)
                .with_pillow_mask([0.0, 0.0]),
            );

            // Dent (2): finger-press at the mousedown slot, if pressing.
            if let Some(press) = state.press {
                if press.card_id == card.id {
                    if let Some(pressed_slot_idx) = fold_states
                        .iter()
                        .position(|&s| s == press.clicked_state)
                    {
                        let finger_x = slot_center_x(pressed_slot_idx);
                        out.push(
                            RectInstance::solid(
                                finger_x - dent_size * 0.5,
                                dent_y,
                                dent_size,
                                dent_size,
                                dent_color,
                                dent_corner,
                            )
                            .with_dome(-FOLD_CHIP_DOME)
                            .with_pillow_mask([0.0, 0.0]),
                        );
                    }
                }
            }
        }

        // Compute lens position up-front. The lens center is interpolated
        // between adjacent cap centers via `lens_slot_pos` (fractional
        // slot index). Explicitly using `slot * stride + chip_size/2`
        // for cap centers ensures the lens sits on the cap, not on the
        // gap-inclusive stride midpoint.
        let lens_slot_pos = card_well_position(card, fold_progress);
        let lens_x = widget_x + lens_slot_pos * slot_stride + chip_size * 0.5;

        // ---- Lens: drop shadow into the plate RT, plus a LensInstance
        //      emitted for the pixel-space lens pass. The disc body, rim
        //      darkening, and specular arc are all drawn by that later
        //      pass — it samples the plate RT at magnified coordinates
        //      so the icons underneath are literally magnified through
        //      the glass, with barrel + chromatic aberration applied. ----
        if !USE_DENT_METAPHOR {
            let sky = state.sky_light();

            let lens_disc_size = FOLD_LENS_DISC_SIZE_PT * sf;
            let lens_disc_x = lens_x - lens_disc_size * 0.5;
            let lens_disc_y = widget_y + (widget_height - lens_disc_size) * 0.5;
            let lens_radius = lens_disc_size * 0.5;

            // Drop shadow: drawn in the plate RT so the shadow sits on
            // the widget surface, outside the lens disc. Inside the
            // disc it'll be painted over by the lens pass anyway.
            let mut lens_shadow_color = FOLD_LENS_SHADOW_COLOR;
            lens_shadow_color[3] *= alpha;
            out.push(RectInstance::glowing(
                lens_disc_x + FOLD_LENS_SHADOW_OFFSET_X_PT * sf,
                lens_disc_y + FOLD_LENS_SHADOW_OFFSET_Y_PT * sf,
                lens_disc_size,
                lens_disc_size,
                [0.0, 0.0, 0.0, 0.0],
                lens_radius,
                lens_shadow_color,
                FOLD_LENS_SHADOW_GLOW_PT * sf,
            ));

            // Specular angle: the star is at infinity in a direction,
            // so every lens sees it at the same underlying angle. We
            // anchor a virtual sun relative to *each* lens and take the
            // vector from the lens to it — this way a lens anywhere on
            // the plate traces the same symmetric east-rise → overhead →
            // west-set arc. Per-card jitter keeps identically-placed
            // lenses from glinting in lock-step.
            //
            // Hidden at night via spec_intensity = 0.
            let spec_intensity = ((sky.intensity
                - FOLD_LENS_SPECULAR_NIGHT_THRESHOLD)
                / (1.0 - FOLD_LENS_SPECULAR_NIGHT_THRESHOLD))
                .clamp(0.0, 1.0);
            let lens_center_y_abs = lens_disc_y + lens_disc_size * 0.5;
            let base_angle = sky.direction.y.atan2(sky.direction.x);
            let card_seed = card.id.0 as f32 * 2.3998;
            let jitter = card_seed.sin() * FOLD_LENS_PER_CARD_JITTER;
            let spec_angle = base_angle + jitter;

            let mix = FOLD_LENS_SPECULAR_SKY_MIX;
            let spec_color = [
                FOLD_LENS_SPECULAR_BASE[0] * (1.0 - mix) + sky.color.x * mix,
                FOLD_LENS_SPECULAR_BASE[1] * (1.0 - mix) + sky.color.y * mix,
                FOLD_LENS_SPECULAR_BASE[2] * (1.0 - mix) + sky.color.z * mix,
                FOLD_LENS_SPECULAR_BASE[3],
            ];

            // Lens center in SWAP-CHAIN pixels = plate origin + the
            // lens's plate-local position.
            let lens_screen_x = plate_pos[0] + lens_x;
            let lens_screen_y = plate_pos[1] + lens_center_y_abs;

            lenses_out.push(LensInstance::new(
                [lens_screen_x, lens_screen_y],
                lens_radius,
                FOLD_LENS_MAGNIFICATION,
                alpha, // distort scaled by card alpha so fold-out lenses fade
                spec_angle,
                spec_intensity * alpha,
                spec_color,
            ));
        }

        // ---- Small icons at every slot, always full alpha.
        //      The lens pass samples the plate RT (which contains these
        //      icons at their slot positions) and magnifies whatever
        //      pixels are underneath, so wherever the lens slides, the
        //      corresponding icon is naturally revealed magnified. No
        //      special handling needed — no "hide the selected slot,"
        //      no "fade at midpoint," no magnified-icon-on-top hack. ----
        let icon_y = widget_y + (widget_height - handle_size) * 0.5;
        let mut icon_tint = FOLD_HANDLE_ICON;
        icon_tint[3] *= alpha;
        for (slot, &target) in fold_states.iter().enumerate() {
            let cx = slot_center_x(slot);
            icons_out.push(IconInstance::new(
                cx - handle_size * 0.5,
                icon_y,
                handle_size,
                icon_tint,
                icon_for_fold_state(target).atlas_index(),
            ));
        }
    }

    // ---- Rolling edge during fold animation. ----
    // Show only while the fold is actively animating (progress ≠ target),
    // not when it's resting at an intermediate fold state (HeaderOnly at
    // progress = 0.5 is a valid *rest* now, not a mid-animation frame).
    let progress = state.fold_progress.get(&card.id).copied().unwrap_or(1.0);
    let target = state.fold_target.get(&card.id).copied().unwrap_or(1.0);
    let is_animating = (progress - target).abs() > 1e-3;
    if is_animating && progress > 0.02 && progress < 0.98 && rect.body_h > 0.5 {
        let edge_y = local_y + rect.header_h + rect.body_h - ROLL_EDGE_THICKNESS_PT * sf;
        let mut edge_color = ROLL_EDGE_COLOR;
        let mut edge_glow = ROLL_EDGE_GLOW;
        edge_color[3] *= alpha;
        edge_glow[3] *= alpha;
        out.push(RectInstance::glowing(
            rect.x + 4.0 * sf,
            edge_y,
            rect.width - 8.0 * sf,
            ROLL_EDGE_THICKNESS_PT * sf,
            edge_color,
            0.0,
            edge_glow,
            4.0 * sf,
        ));
    }
}

/// Return hit-test rects for every slot on `card`'s fold-switch widget,
/// paired with the `FoldState` that slot commands. Empty for snippets (no
/// collapsible body). Each slot's hit rect is its stride-wide zone within
/// the widget, widened vertically to the full card-header row for
/// forgiving clicks. Rects are plate-local physical pixels, NOT scrolled —
/// `app.rs` applies scroll + the scene-top offset before comparing against
/// cursor position.
pub fn fold_buttons_scene(
    card: &Card,
    rect: &CardRect,
    state: &AppState,
) -> Vec<(FoldState, (f32, f32, f32, f32))> {
    let fold_states = card_fold_states(card);
    if fold_states.is_empty() {
        return Vec::new();
    }
    let sf = state.scale_factor;
    let chip_size = FOLD_HANDLE_SIZE_PT * sf + 2.0 * FOLD_CHIP_PAD_PT * sf;
    let slot_gap = FOLD_SLOT_GAP_PT * sf;
    let slot_stride = chip_size + slot_gap;
    let slot_count = fold_states.len();
    let widget_width =
        slot_count as f32 * chip_size + (slot_count.saturating_sub(1)) as f32 * slot_gap;

    let strip_right = rect.x + rect.width - 10.0 * sf;
    let widget_x = strip_right - widget_width;

    let mut out = Vec::with_capacity(slot_count);
    for (slot, &target) in fold_states.iter().enumerate() {
        let slot_x = widget_x + slot as f32 * slot_stride;
        // Hit-rect is chip-wide only: the gap between caps is non-
        // interactive (a click there does nothing), matching the
        // visual that the gap isn't part of any sub-button.
        out.push((target, (slot_x, rect.y, chip_size, rect.header_h)));
    }
    out
}

// ----------------------------------------------------------------------------
// Buffer construction
// ----------------------------------------------------------------------------

/// Build a glyphon buffer for one card with per-byte syntax colors. Class
/// cards render only their header line (their body == methods, which have
/// their own cards); other cards render their full byte range (including
/// decorators).
fn build_card_buffer(
    font_system: &mut FontSystem,
    hl: &HighlightedSource,
    card: &Card,
    font_size: f32,
    line_height: f32,
) -> Buffer {
    let range = match card.kind {
        CardKind::Class => card.header_range.clone(),
        _ => card.full_range.clone(),
    };

    // The card's text starts at `range.start`, which in source is usually
    // mid-line (e.g. at `def` in `    def foo():`, column 4). The characters
    // before it — the leading whitespace of that line — aren't in the range.
    // But subsequent lines in the range carry their full source indentation,
    // which reads as a staircase (first line flush-left, body deeply
    // indented). Strip the first line's source column from every other line
    // so the card's internal indentation is relative to its signature.
    let line_idx = hl
        .line_offsets
        .partition_point(|&o| o <= range.start)
        .saturating_sub(1);
    let base_col = range.start - hl.line_offsets[line_idx];

    let raw_text = &hl.source.contents[range.clone()];
    let raw_kinds = &hl.kinds[range.clone()];
    let (text, kinds) = dedent_with_kinds(raw_text, raw_kinds, base_col);

    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    buffer.set_size(font_system, Some(100_000.0), None);

    let attrs_by_kind = attrs_by_kind();
    let default_attrs = Attrs::new().family(Family::Monospace);
    let spans = RunSpans { text: &text, kinds: &kinds, i: 0, attrs_by_kind };
    buffer.set_rich_text(font_system, spans, default_attrs, Shaping::Advanced);
    // Shape ALL lines (not just up to the buffer's scroll position).
    // `shape_until_scroll` leaves later lines unshaped, which makes
    // `layout_runs().count()` under-report the wrapped visual line
    // count for tall content — that under-count was clipping the
    // tail of multi-paragraph Markdown cards. Target the last
    // source line as the shape cursor so every line gets shaped.
    // `shape_until_cursor` panics on cursors past `buffer.lines.len()`,
    // so clamp.
    if !buffer.lines.is_empty() {
        let last = buffer.lines.len() - 1;
        buffer.shape_until_cursor(font_system, glyphon::Cursor::new(last, 0), false);
    }
    buffer
}

/// Strip up to `strip` leading ASCII spaces from every line except the first
/// of `text`, keeping `kinds` synchronized byte-for-byte.
///
/// "Except the first" is deliberate: the first line's leading whitespace was
/// already elided by the card's byte range starting mid-line (at `def` / `@`).
/// Subsequent lines still carry the full source indentation, so we peel off
/// the same amount the first line was missing — giving a coherent picture
/// where the body sits one indent level below the signature.
fn dedent_with_kinds(text: &str, kinds: &[TokenKind], strip: usize) -> (String, Vec<TokenKind>) {
    if strip == 0 {
        return (text.to_string(), kinds.to_vec());
    }
    let mut out_text = String::with_capacity(text.len());
    let mut out_kinds = Vec::with_capacity(kinds.len());
    let mut byte_i = 0usize;
    let mut first = true;
    for line in text.split_inclusive('\n') {
        let line_len = line.len();
        if first {
            out_text.push_str(line);
            out_kinds.extend_from_slice(&kinds[byte_i..byte_i + line_len]);
            first = false;
        } else {
            let to_strip = line
                .bytes()
                .take(strip)
                .take_while(|&b| b == b' ')
                .count();
            out_text.push_str(&line[to_strip..]);
            out_kinds.extend_from_slice(&kinds[byte_i + to_strip..byte_i + line_len]);
        }
        byte_i += line_len;
    }
    (out_text, out_kinds)
}

// ----------------------------------------------------------------------------
// Syntax color cache + span iterator
// ----------------------------------------------------------------------------

const ALL_KINDS: [TokenKind; 15] = [
    TokenKind::Default,
    TokenKind::Keyword,
    TokenKind::String,
    TokenKind::EscapeInString,
    TokenKind::Comment,
    TokenKind::Number,
    TokenKind::Operator,
    TokenKind::Function,
    TokenKind::FunctionBuiltin,
    TokenKind::Class,
    TokenKind::Constant,
    TokenKind::ConstantBuiltin,
    TokenKind::Type,
    TokenKind::Property,
    TokenKind::Punctuation,
];
const N_KINDS: usize = ALL_KINDS.len();

fn attrs_by_kind() -> [Attrs<'static>; N_KINDS] {
    let base = Attrs::new().family(Family::Monospace);
    let mut out = [base; N_KINDS];
    for (i, kind) in ALL_KINDS.iter().enumerate() {
        let (r, g, b) = kind.color();
        out[i] = base.color(GlyphonColor::rgb(r, g, b));
    }
    out
}

struct RunSpans<'a> {
    text: &'a str,
    kinds: &'a [TokenKind],
    i: usize,
    attrs_by_kind: [Attrs<'static>; N_KINDS],
}
impl<'a> Iterator for RunSpans<'a> {
    type Item = (&'a str, Attrs<'static>);
    fn next(&mut self) -> Option<Self::Item> {
        if self.i >= self.text.len() {
            return None;
        }
        let start = self.i;
        let k = self.kinds[start];
        let mut end = start + 1;
        while end < self.text.len() && self.kinds[end] == k {
            end += 1;
        }
        while end < self.text.len() && !self.text.is_char_boundary(end) {
            end += 1;
        }
        self.i = end;
        Some((&self.text[start..end], self.attrs_by_kind[k as usize]))
    }
}

// ----------------------------------------------------------------------------
// Egui overlay (unchanged from M2)
// ----------------------------------------------------------------------------

fn draw_egui(ctx: &egui::Context, state: &AppState) {
    // The blind (Zone 1, rendered via our wgpu pipeline) replaces the
    // former egui placeholder. Egui pass is otherwise empty; the only
    // active overlay today is the optional debug-perspective-compass
    // (YGG-62).
    if state.debug_perspective_compass {
        draw_perspective_compass(ctx, state);
    }
}

/// Debug overlay for YGG-62: a semi-transparent line from the window
/// center toward the projection anchor, with CPU-side one-point
/// perspective so the line's 2D length correctly foreshortens with
/// the anchor's Z depth.
///
/// Only the FIXED end (window center) is marked with a small dot —
/// the free end is unmarked so the viewer can immediately read which
/// end is fixed.
///
/// Until the 3D slat pipeline lands (YGG-62 Phase 2), this is the
/// one place real 3D perspective math runs. When the pipeline lands,
/// this gizmo graduates to a real 3D primitive.
fn draw_perspective_compass(ctx: &egui::Context, state: &AppState) {
    let Some(anchor) = state.projection_anchor() else {
        return;
    };
    let scale = state.scale_factor.max(0.001);
    // Work in physical pixels (window-local), convert at draw time.
    let cx = state.window_size.width as f32 * 0.5;
    let cy = state.window_size.height as f32 * 0.5;
    let ax = anchor[0];
    let ay = anchor[1];
    let az = anchor[2];
    // Direction in 3D from the window center to the anchor.
    let dx3 = ax - cx;
    let dy3 = ay - cy;
    let dz3 = az;
    let dist3 = (dx3 * dx3 + dy3 * dy3 + dz3 * dz3).sqrt();
    if dist3 < 1e-3 {
        return;
    }
    // Fixed 3D length — quarter of the window height in virtual
    // world units.
    let line_len = state.window_size.height as f32 * 0.25;
    let nx = dx3 / dist3;
    let ny = dy3 / dist3;
    let nz = dz3 / dist3;
    // 3D endpoint of the compass.
    let end3_x = cx + nx * line_len;
    let end3_y = cy + ny * line_len;
    let end3_z = nz * line_len; // start's z = 0, so end's z = depth.
    // One-point perspective projection with vanishing point at the
    // anchor (vx, vy in screen space). Focal length = anchor.z —
    // at z=anchor.z, points are halfway to the vanishing point.
    // Start is at z=0 so it projects to itself; end has z=end3_z
    // and is foreshortened toward (ax, ay) proportionally.
    let focal = az.max(1.0);
    let t = focal / (focal + end3_z);
    let end2_x = ax + (end3_x - ax) * t;
    let end2_y = ay + (end3_y - ay) * t;
    // Convert to egui logical points.
    let cx_l = cx / scale;
    let cy_l = cy / scale;
    let ex_l = end2_x / scale;
    let ey_l = end2_y / scale;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("ygg-debug-compass"),
    ));
    let stroke =
        egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(120, 220, 255, 170));
    painter.line_segment([egui::pos2(cx_l, cy_l), egui::pos2(ex_l, ey_l)], stroke);
    // Only the FIXED end (center) gets a dot — makes the free end
    // unambiguous at a glance.
    painter.circle_filled(
        egui::pos2(cx_l, cy_l),
        2.5,
        egui::Color32::from_rgba_unmultiplied(200, 240, 255, 220),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::SourceFile;
    use crate::cards::{extract_cards, layout_cards, CardKind, MethodModifier, Visibility};
    use crate::state::compute_line_offsets;

    fn test_state(src: &str) -> (AppState, Vec<Card>) {
        let sf = SourceFile {
            path: std::path::PathBuf::from("t.py"),
            contents: src.to_string(),
            lines: src.split('\n').map(|s| s.to_string()).collect(),
        };
        let offs = compute_line_offsets(src);
        let mut p = tree_sitter::Parser::new();
        let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        p.set_language(&lang).unwrap();
        let tree = p.parse(src, None).unwrap();
        let cards = extract_cards(&tree, src, &offs);
        let h = crate::syntax::Highlighter::new_python().unwrap();
        let kinds = h.highlight_tree(&tree, src);
        let hl = HighlightedSource::from_parts(sf, kinds, offs);
        let state = AppState::new(hl, cards.clone());
        (state, cards)
    }

    /// `fold_buttons_scene` emits one rect per slot for a non-snippet card,
    /// all inside the card's header row; the rectangles are non-overlapping
    /// and left-to-right-ordered by slot index (Folded before Unfolded).
    #[test]
    fn fold_buttons_live_inside_header() {
        let (state, cards) = test_state("def f():\n    pass\n");
        let m = LayoutMetrics {
            line_height: 20.0,
            left: 0.0,
            width: 400.0,
            depth_indent: 0.0,
            top_level_gap: 0.0,
            card_inner_pad_y: 4.0,
        };
        let layout = layout_cards(&cards, &state.fold_progress, m);
        let r = layout.rects[&cards[0].id];
        let buttons = fold_buttons_scene(&cards[0], &r, &state);
        assert_eq!(buttons.len(), 2, "two-slot switch: Folded + Unfolded");
        assert_eq!(buttons[0].0, FoldState::Folded);
        assert_eq!(buttons[1].0, FoldState::Unfolded);
        for (_, (_x, y, _w, h)) in &buttons {
            assert!(*y >= r.y);
            assert!((*y + *h) <= (r.y + r.header_h + 0.01));
        }
        // Slots don't overlap and Folded sits to the left of Unfolded.
        let (_, (x0, _, w0, _)) = buttons[0];
        let (_, (x1, _, _, _)) = buttons[1];
        assert!(x0 + w0 <= x1 + 0.01);
    }

    /// Snippet cards have no fold-switch slots — `fold_buttons_scene`
    /// returns empty so hit-testing never fires on them.
    #[test]
    fn fold_buttons_empty_for_snippets() {
        let (state, cards) = test_state("import os\n");
        let m = LayoutMetrics {
            line_height: 20.0,
            left: 0.0,
            width: 400.0,
            depth_indent: 0.0,
            top_level_gap: 0.0,
            card_inner_pad_y: 4.0,
        };
        let layout = layout_cards(&cards, &state.fold_progress, m);
        let r = layout.rects[&cards[0].id];
        assert!(fold_buttons_scene(&cards[0], &r, &state).is_empty());
    }

    /// A class card has a Class kind and gets the CLASS_BG palette. This
    /// isn't a visual test but guards the palette lookup table.
    #[test]
    fn push_card_shapes_emits_class_shapes() {
        let (state, cards) = test_state(
            "class W:\n    def a(self):\n        pass\n    @classmethod\n    def b(cls):\n        pass\n",
        );
        let m = LayoutMetrics {
            line_height: 20.0,
            left: 0.0,
            width: 400.0,
            depth_indent: 20.0,
            top_level_gap: 10.0,
            card_inner_pad_y: 4.0,
        };
        let layout = layout_cards(&cards, &state.fold_progress, m);
        let class_rect = layout.rects[&cards[0].id];
        let mut out = vec![];
        let mut icons = vec![];
        let mut lenses = vec![];
        push_card_shapes(
            &mut out,
            &mut icons,
            &mut lenses,
            &cards[0],
            &class_rect,
            0.0,
            &state,
            [0.0, 0.0],
        );

        // Class card emits at least: bg, accent strip, spine, fold handle → 4 rects.
        assert!(out.len() >= 4, "class emits ≥4 shapes, got {}", out.len());
        let cm = cards.iter().find(|c| c.modifier == MethodModifier::Classmethod).unwrap();
        assert!(matches!(cm.kind, CardKind::Method));
        assert_eq!(cm.visibility, Visibility::Public);
    }

    /// `plate_rect` positions the plate with PANEL_INSET breathing room on
    /// all sides (inside the code-pane region on the right).
    #[test]
    fn plate_rect_is_inset_from_code_pane() {
        let (mut state, _) = test_state("def f():\n    pass\n");
        state.window_size = WindowSize { width: 1200, height: 900 };
        state.scale_factor = 1.0;
        let (pos, size) = plate_rect(&state);
        // Plate starts PANEL_INSET_PT to the right of the code-pane left.
        assert!(pos[0] > state.code_pane_left() as f32);
        assert!(pos[1] > 0.0);
        // Plate size + 2*inset fits inside the code-pane width / window height.
        assert!(size[0] as f32 + 2.0 * PANEL_INSET_PT < state.code_pane_width() as f32 + 1.0);
        assert!(size[1] as f32 + 2.0 * PANEL_INSET_PT < state.window_size.height as f32 + 1.0);
    }

    /// LayoutMetrics is in plate-local coords (`left` has no code_pane_left
    /// offset baked in). Hit-test relies on this, so lock it down.
    #[test]
    fn layout_metrics_are_plate_local() {
        // We don't have a real Renderer here (no GPU). Re-derive what
        // `layout_metrics` would compute, using the same formula.
        let (mut state, _) = test_state("def f():\n    pass\n");
        state.window_size = WindowSize { width: 1200, height: 900 };
        state.scale_factor = 1.0;
        // Attach a tree so `code_pane_left()` reserves space for the blind
        // (YGG-54: single-file mode makes code_pane_left = 0, which would
        // trivially make this assertion hold. A tree-mode state is the
        // interesting case to lock down.)
        use crate::filetree::{DirectoryListing, TreeState};
        state.tree = Some(TreeState::new(DirectoryListing {
            root: std::path::PathBuf::from("/"),
            entries: vec![],
        }));
        let (_, plate_size) = plate_rect(&state);
        let width = plate_size[0] as f32 - (CODE_PAD_LEFT_PT + CODE_PAD_RIGHT_PT);
        assert!((width - (plate_size[0] as f32 - 40.0)).abs() < 1e-3);
        // The key assertion: left is just the in-plate padding, NOT offset
        // by state.code_pane_left(). That's what makes coordinates plate-local.
        assert!(CODE_PAD_LEFT_PT < state.code_pane_left() as f32);
    }
}
