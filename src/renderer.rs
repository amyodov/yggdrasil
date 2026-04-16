//! Renderer — wgpu + egui + glyphon plumbing.
//!
//! Owns the GPU resources and draws a frame from an `AppState`. This is the
//! seed of what CLAUDE.md calls "the Renderer": in later milestones cards,
//! glow, animations etc. extend this; for M1 it paints a solid near-black
//! canvas with monospace text on the right and an egui placeholder panel on
//! the left.

use std::sync::Arc;

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

use crate::state::{AppState, WindowSize};

/// The near-black void background. Slightly warm to avoid pure-#000 harshness.
/// Ambient animation (M4) will modulate this subtly — for M1, it's static.
const BG: wgpu::Color = wgpu::Color {
    r: 0.012,
    g: 0.012,
    b: 0.018,
    a: 1.0,
};

/// Monospace rendering metrics. The size will be tuned for taste in later
/// milestones; 14/20 is a sensible baseline that reads well at 1x and 2x.
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;

pub struct Renderer {
    // winit/wgpu core
    window: Arc<Window>,
    device: Device,
    queue: Queue,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,

    // glyphon (text on GPU)
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    text_buffer: Buffer,

    // egui (HUD overlays)
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

impl Renderer {
    /// Initialize all GPU + text resources for the given window.
    pub async fn new(window: Arc<Window>, source_text: &str) -> Result<Self> {
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
        // Prefer SRGB formats for correct color. glyphon writes linear colors
        // into an SRGB target, which gives us perceptually-correct text.
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

        // --- glyphon ---
        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer = TextRenderer::new(
            &mut atlas,
            &device,
            MultisampleState::default(),
            None,
        );

        // M1: the full file sits in a single non-wrapped buffer. M2 will swap
        // this for a virtualized one-Buffer-per-visible-line scheme.
        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        // Very wide width effectively disables wrapping; M1 spec: "horizontal
        // overflow: ignore for now, let it clip".
        text_buffer.set_size(&mut font_system, Some(100_000.0), None);
        text_buffer.set_text(
            &mut font_system,
            source_text,
            Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
        );
        text_buffer.shape_until_scroll(&mut font_system, false);

        // --- egui ---
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
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            text_buffer,
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

    /// Handle a resize: reconfigure the surface. Glyphon's viewport gets
    /// refreshed each frame, so no action needed there.
    pub fn resize(&mut self, new_size: WindowSize) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.surface_config.width = new_size.width;
        self.surface_config.height = new_size.height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    /// Draw one frame.
    pub fn render(&mut self, state: &AppState) -> Result<(), wgpu::SurfaceError> {
        // If upstream never resized us (first frame after creation), the
        // config may already match; configure is idempotent and cheap.

        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: Some("ygg-encoder") });

        // ---- glyphon prepare ----
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        let code_left = state.code_pane_left() as f32;
        let code_width = state.code_pane_width() as f32;
        let code_height = state.window_size.height as f32;

        // TextArea.top is where the first line of the buffer sits on-screen.
        // Scrolling "down" moves the text upward, so top = -scroll_y.
        let top = -state.scroll_y;

        let bounds = TextBounds {
            left: code_left as i32,
            top: 0,
            right: (code_left + code_width) as i32,
            bottom: code_height as i32,
        };

        let area = TextArea {
            buffer: &self.text_buffer,
            left: code_left + 8.0, // small inner padding so text doesn't touch the edge
            top,
            scale: 1.0,
            bounds,
            default_color: GlyphonColor::rgb(220, 220, 220),
            custom_glyphs: &[],
        };

        // prepare() can fail if the atlas is out of room; in that case we
        // surface the error as a skipped frame — a recoverable condition.
        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            [area],
            &mut self.swash_cache,
        ) {
            log::warn!("glyphon prepare failed: {e:?}");
        }

        // ---- egui prepare ----
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            draw_egui(ctx);
        });
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

        // ---- render passes ----
        //
        // Two passes because glyphon's `render` borrows its inputs with the
        // pass lifetime, while `egui-wgpu::Renderer::render` requires
        // `RenderPass<'static>`. These constraints cannot coexist in a single
        // pass in wgpu 22. Splitting is cheap: pass 1 clears + draws text,
        // pass 2 loads the previous attachment and overlays egui on top.
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("ygg-text-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(BG),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Err(e) = self.text_renderer.render(&self.atlas, &self.viewport, &mut pass) {
                log::warn!("glyphon render failed: {e:?}");
            }
        }
        {
            let mut pass = encoder
                .begin_render_pass(&RenderPassDescriptor {
                    label: Some("ygg-egui-pass"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: Operations {
                            load: LoadOp::Load,
                            store: StoreOp::Store,
                        },
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

        // Atlas trim keeps VRAM usage bounded. No-op if nothing to free.
        self.atlas.trim();

        Ok(())
    }
}

/// Build the egui UI for this frame. Extracted so the render method stays about
/// plumbing, not layout.
///
/// Panes sit over the shared near-black canvas with no borders or separator
/// lines — CLAUDE.md's "Panes as light, not boxes" principle. The luminous
/// activation glow arrives in M4; for M1 the pane is only identifiable by the
/// placeholder label sitting inside it.
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
            // Keep it minimal — this is the M1 placeholder only.
            let label = egui::RichText::new("file tree")
                .color(egui::Color32::from_rgb(140, 140, 160))
                .monospace()
                .size(13.0);
            ui.label(label);
        });
}
