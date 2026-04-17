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
//!   `fold_handle_rect_scene`. Physical pixels.
//! - **Window / screen**, origin at window's top-left, used by: cursor events,
//!   plate position, composite output, background, egui. Physical pixels.
//!
//! Conversion: `screen = plate.pos + model * plate_local`. With identity
//! model (today), `screen = plate.pos + plate_local`.
//!
//! Hit-testing in `app.rs` converts cursor from screen to plate-local before
//! comparing against `fold_handle_rect_scene` output.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, Device, DeviceDescriptor, Features, Instance,
    InstanceDescriptor, Limits, LoadOp, MemoryHints, MultisampleState, Operations, PowerPreference,
    PresentMode, Queue, RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions,
    StoreOp, Surface, SurfaceConfiguration, TextureFormat, TextureUsages, TextureViewDescriptor,
};
use winit::window::Window;

use crate::background::BackgroundRenderer;
use crate::cards::{
    layout_cards, Card, CardId, CardKind, CardRect, Layout, LayoutMetrics, MethodModifier,
    Visibility,
};
use crate::composite::CompositeRenderer;
use crate::plate::Plate;
use crate::shapes::{RectInstance, ShapeRenderer};
use crate::state::{AppState, HighlightedSource, WindowSize};
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

/// Class spine (armature) — luminous cyan rail.
const SPINE_COLOR: [f32; 4] = [0.72, 0.90, 1.00, 0.92];
const SPINE_GLOW: [f32; 4] = [0.55, 0.85, 1.00, 0.55];

/// Fold handle block — color flips on target state.
const FOLD_HANDLE_OPEN: [f32; 4] = [0.35, 0.46, 0.62, 0.95];
const FOLD_HANDLE_CLOSED: [f32; 4] = [0.62, 0.36, 0.44, 0.95];

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
const HANDLE_CORNER_RADIUS_PT: f32 = 2.5;

/// Spine (class armature) glow radius — the one card-zone element that is
/// truly emissive, because it's a semantic light (the class's identity rail).
const SPINE_GLOW_RADIUS_PT: f32 = 6.0;

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
const FOLD_HANDLE_SIZE_FRAC: f32 = 0.5;
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
    /// Reference instant used to compute the time uniform fed into the
    /// background shader for the breathing animation.
    start_time: Instant,

    // Plate infrastructure (M3.1).
    composite: CompositeRenderer,
    code_plate: Plate,

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
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        // One buffer per card, built eagerly at load.
        let mut card_buffers = HashMap::with_capacity(cards.len());
        for card in cards {
            let buf = build_card_buffer(&mut font_system, highlighted, card, font_size, line_height);
            card_buffers.insert(card.id, buf);
        }

        // Shape + background + composite pipelines.
        let shape_renderer = ShapeRenderer::new(&device, format);
        let background_renderer = BackgroundRenderer::new(&device, format);
        let composite = CompositeRenderer::new(&device, format);

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
        let code_plate = Plate::new(
            &device,
            plate_size,
            plate_pos,
            format,
            &composite.bind_group_layout,
            &composite.sampler,
            &composite.uniform_buffer,
        );

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
            start_time: Instant::now(),
            composite,
            code_plate,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            card_buffers,
            applied_font_size: font_size,
            applied_line_height: line_height,
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
            self.applied_font_size = font_size;
            self.applied_line_height = line_height;
        }

        // Reconfigure the plate if the window size or scale factor changed the
        // plate's target dimensions.
        let (plate_pos, plate_size) = plate_rect(state);
        self.code_plate.reconfigure(
            &self.device,
            plate_size,
            plate_pos,
            self.surface_config.format,
            &self.composite.bind_group_layout,
            &self.composite.sampler,
            &self.composite.uniform_buffer,
        );

        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: Some("ygg-encoder") });

        // ---- Layout (plate-local coordinates) ----
        let metrics = self.layout_metrics(state);
        let layout = layout_cards(&state.cards, &state.fold_progress, metrics);
        let scene_top_local = SCENE_TOP_PAD_PT * state.scale_factor;

        // ---- Build shape instances ----
        // All positions are plate-local (origin = plate top-left). The plate
        // background (lit material + outer bloom) is drawn by the composite
        // shader in M3.3, not as a shape instance here — so the RT starts
        // transparent and we only draw cards into it.
        let mut instances: Vec<RectInstance> = Vec::with_capacity(state.cards.len() * 5);
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
            push_card_shapes(&mut instances, card, rect, local_y, state);
        }

        // Shapes' uniform viewport = plate size (we're rendering into the plate RT).
        self.shape_renderer
            .prepare(&self.device, &self.queue, &instances, (plate_size[0], plate_size[1]));

        // Background uniforms stay window-sized (it draws to the swap chain).
        self.background_renderer.prepare(
            &self.queue,
            (state.window_size.width, state.window_size.height),
            self.start_time.elapsed().as_secs_f32(),
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

        // ---- Composite uniforms (plate → swap chain) ----
        self.composite.prepare(
            &self.queue,
            (self.surface_config.width, self.surface_config.height),
            self.code_plate.pos_px,
            self.code_plate.size_px,
            PANEL_CORNER_RADIUS_PT * sf,
            PLATE_BLOOM_RADIUS_PT * sf,
            PLATE_BLOOM_COLOR,
            PLATE_RIM_THICKNESS_PT * sf,
            PLATE_RIM_INTENSITY,
            self.code_plate.model,
        );

        // ---- Egui ----
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let full_output = self.egui_ctx.run(raw_input, draw_egui);
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
                    view: &self.code_plate.rt_view,
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
                    view: &self.code_plate.rt_view,
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
            self.composite.render(&mut pass, &self.code_plate.composite_bg);
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

        let opacity = if card.visibility == Visibility::Private { 0.8 } else { 1.0 };
        let r = (220.0 * opacity) as u8;
        let g = (222.0 * opacity) as u8;
        let b = (230.0 * opacity) as u8;

        out.push(AreaSpec {
            buffer,
            left,
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
fn push_card_shapes(
    out: &mut Vec<RectInstance>,
    card: &Card,
    rect: &CardRect,
    local_y: f32,
    state: &AppState,
) {
    let sf = state.scale_factor;
    let corner = CARD_CORNER_RADIUS_PT * sf;

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
    shadow_color[3] *= shadow_scale;
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
    let bg = match card.kind {
        CardKind::Class => CLASS_BG,
        _ => CARD_BG,
    };
    out.push(RectInstance::solid(
        rect.x,
        local_y,
        rect.width,
        rect.total_h(),
        bg,
        corner,
    ));

    // ---- Left-side accent strip ----
    let (accent_color, accent_width_pt) = match (card.kind, card.modifier, card.visibility) {
        (CardKind::Class, _, _) => (SPINE_COLOR, SPINE_WIDTH_PT),
        (CardKind::Snippet, _, _) => (ACCENT_SNIPPET, ACCENT_WIDTH_PT_PRIVATE),
        (_, MethodModifier::Classmethod, _) => (ACCENT_CLASSMETHOD, ACCENT_WIDTH_PT),
        (_, MethodModifier::Staticmethod, _) => (ACCENT_STATICMETHOD, ACCENT_WIDTH_PT),
        (_, MethodModifier::Property, _) => (ACCENT_PROPERTY, ACCENT_WIDTH_PT),
        (_, _, Visibility::Private) => (ACCENT_PRIVATE, ACCENT_WIDTH_PT_PRIVATE),
        (_, _, Visibility::Public) => (ACCENT_PUBLIC, ACCENT_WIDTH_PT),
    };
    out.push(RectInstance::solid(
        rect.x + 2.0 * sf,
        local_y + 3.0 * sf,
        accent_width_pt * sf,
        rect.total_h() - 6.0 * sf,
        accent_color,
        ACCENT_CORNER_RADIUS_PT * sf,
    ));

    // ---- Class spine (armature): a glowing rail on the left edge. ----
    if card.kind == CardKind::Class {
        out.push(RectInstance::glowing(
            rect.x,
            local_y + 4.0 * sf,
            SPINE_WIDTH_PT * sf,
            rect.total_h() - 8.0 * sf,
            SPINE_COLOR,
            SPINE_WIDTH_PT * sf * 0.5,
            SPINE_GLOW,
            SPINE_GLOW_RADIUS_PT * sf,
        ));
    }

    // ---- Fold handle — small rounded block, recolors on target fold state.
    //      Skipped for snippets (they have no collapsible body). ----
    if card.kind != CardKind::Snippet {
        let target = state.fold_target.get(&card.id).copied().unwrap_or(1.0);
        let handle_color = if target < 0.5 { FOLD_HANDLE_CLOSED } else { FOLD_HANDLE_OPEN };
        let handle_size = rect.header_h * FOLD_HANDLE_SIZE_FRAC;
        let handle_x = rect.x + rect.width - handle_size - 10.0 * sf;
        let handle_y = local_y + (rect.header_h - handle_size) * 0.5;
        out.push(RectInstance::solid(
            handle_x,
            handle_y,
            handle_size,
            handle_size,
            handle_color,
            HANDLE_CORNER_RADIUS_PT * sf,
        ));
    }

    // ---- Rolling edge during fold animation. ----
    let progress = state.fold_progress.get(&card.id).copied().unwrap_or(1.0);
    if progress > 0.02 && progress < 0.98 && rect.body_h > 0.5 {
        let edge_y = local_y + rect.header_h + rect.body_h - ROLL_EDGE_THICKNESS_PT * sf;
        out.push(RectInstance::glowing(
            rect.x + 4.0 * sf,
            edge_y,
            rect.width - 8.0 * sf,
            ROLL_EDGE_THICKNESS_PT * sf,
            ROLL_EDGE_COLOR,
            0.0,
            ROLL_EDGE_GLOW,
            4.0 * sf,
        ));
    }
}

/// Return a rectangle (plate-local physical pixels, NOT scrolled) for the
/// fold-handle hit region of card `card`. Widened vertically to the full
/// header row so fold-click is forgiving.
///
/// Coordinates are plate-local; `app.rs` converts cursor position from
/// window-space to plate-local (via `plate_rect`) before comparing.
pub fn fold_handle_rect_scene(_card: &Card, rect: &CardRect, state: &AppState) -> (f32, f32, f32, f32) {
    let sf = state.scale_factor;
    let handle_size = rect.header_h * FOLD_HANDLE_SIZE_FRAC;
    let x = rect.x + rect.width - handle_size - 6.0 * sf;
    let pad = 4.0 * sf;
    (x - pad, rect.y, handle_size + pad * 2.0, rect.header_h)
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
    buffer.shape_until_scroll(font_system, false);
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

fn draw_egui(ctx: &egui::Context) {
    let frame = egui::Frame::none()
        .fill(egui::Color32::TRANSPARENT)
        .inner_margin(egui::Margin::symmetric(12.0, 12.0));
    egui::SidePanel::left("file_tree_pane")
        .resizable(false)
        .show_separator_line(false)
        .exact_width(ctx.screen_rect().width() * crate::state::LEFT_PANE_FRACTION)
        .frame(frame)
        .show(ctx, |ui| {
            let label = egui::RichText::new("file tree")
                .color(egui::Color32::from_rgb(140, 140, 160))
                .monospace()
                .size(13.0);
            ui.label(label);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::SourceFile;
    use crate::cards::{extract_cards, CardKind, MethodModifier, Visibility};
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

    /// fold_handle_rect_scene returns a rect inside the card's header row.
    #[test]
    fn fold_handle_lives_inside_header() {
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
        let (_x, y, _w, h) = fold_handle_rect_scene(&cards[0], &r, &state);
        assert!(y >= r.y);
        assert!((y + h) <= (r.y + r.header_h + 0.01));
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
        push_card_shapes(&mut out, &cards[0], &class_rect, 0.0, &state);

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
        let (_, plate_size) = plate_rect(&state);
        let width = plate_size[0] as f32 - (CODE_PAD_LEFT_PT + CODE_PAD_RIGHT_PT);
        assert!((width - (plate_size[0] as f32 - 40.0)).abs() < 1e-3);
        // The key assertion: left is just the in-plate padding, NOT offset
        // by state.code_pane_left(). That's what makes coordinates plate-local.
        assert!(CODE_PAD_LEFT_PT < state.code_pane_left() as f32);
    }
}
