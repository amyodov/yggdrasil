//! Renderer — wgpu + egui + glyphon plumbing.
//!
//! Owns the GPU resources and draws a frame from an `AppState`. This is the
//! seed of what CLAUDE.md calls "the Renderer": in later milestones cards,
//! glow, animations etc. extend this. M2 adds:
//! - A rich-text code buffer with per-byte syntax colors.
//! - A second glyphon buffer for the line-number gutter.
//! - Virtualization: glyphon's `shape_until_scroll` lazily shapes visible
//!   content; we use `visible_line_range` to decide when to reshape.

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

use crate::state::{visible_line_range, AppState, HighlightedSource, WindowSize};
use crate::syntax::TokenKind;

/// The near-black void background. Slightly warm to avoid pure-#000 harshness.
/// Ambient animation (M4) will modulate this subtly — for now, static.
const BG: wgpu::Color = wgpu::Color {
    r: 0.012,
    g: 0.012,
    b: 0.018,
    a: 1.0,
};

/// Monospace rendering metrics. 14/20 reads well at 1x and 2x.
pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 20.0;

/// Number of lines to overshape past the viewport in each direction. Keeps
/// near-edge scrolling from stuttering while glyphon catches up.
const SCROLL_OVERSCAN: usize = 8;

/// Horizontal padding inside the code pane so text doesn't touch the edges.
const CODE_PAD_LEFT: f32 = 12.0;
const CODE_PAD_RIGHT: f32 = 12.0;

/// Gutter = right-aligned line numbers in their own column. Width scales with
/// max-digits so it never clips. Includes a trailing gap before the code.
const GUTTER_DIGIT_PAD: usize = 1;   // leading spaces inside the gutter
const GUTTER_TRAILING_GAP: f32 = 10.0; // pixels between gutter and code

/// Gutter color (subtle slate; same family as Comment but a bit dimmer).
const GUTTER_COLOR: (u8, u8, u8) = (80, 95, 120);

/// Empirical monospace glyph advance at FONT_SIZE=14. Used to size the gutter
/// column. Correct for most monospace fonts cosmic-text will pick on macOS;
/// being off by ±1px is harmless because the gutter has padding on both sides.
const MONO_GLYPH_WIDTH: f32 = FONT_SIZE * 0.6;

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

    /// Code text with per-token color attrs. Set once at init; scroll moves
    /// the TextArea rather than rebuilding the buffer.
    code_buffer: Buffer,
    /// Line-number gutter. Set once; scrolls in lockstep with the code buffer.
    gutter_buffer: Buffer,
    /// Width of the gutter in pixels (depends on max digits in the file).
    gutter_width: f32,

    // egui (HUD overlays)
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

impl Renderer {
    /// Initialize all GPU + text resources for the given window and file.
    pub async fn new(window: Arc<Window>, highlighted: &HighlightedSource) -> Result<Self> {
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

        // Code buffer with rich text.
        let code_buffer = build_code_buffer(&mut font_system, highlighted);

        // Gutter buffer + width.
        let (gutter_buffer, gutter_width) = build_gutter_buffer(&mut font_system, highlighted);

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
            code_buffer,
            gutter_buffer,
            gutter_width,
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

        // Drive glyphon's lazy shaper to keep up with the current scroll +
        // overscan. The visible range is a tested pure function of state.
        let visible = visible_line_range(
            state.scroll_y,
            state.window_size.height,
            LINE_HEIGHT,
            state.highlighted.line_count(),
            SCROLL_OVERSCAN,
        );
        // `set_scroll` expresses "glyphon: shape & paint from this line
        // downward". We don't use it for positioning — TextArea.top does
        // that — we use it only to steer the shaper.
        let scroll_target = visible.start as i32;
        prime_scroll(&mut self.code_buffer, &mut self.font_system, scroll_target);
        prime_scroll(&mut self.gutter_buffer, &mut self.font_system, scroll_target);

        let gutter_left = state.code_pane_left() as f32 + CODE_PAD_LEFT;
        let code_text_left = gutter_left + self.gutter_width + GUTTER_TRAILING_GAP;
        let code_text_right = (state.code_pane_left() + state.code_pane_width()) as f32
            - CODE_PAD_RIGHT;
        let viewport_h = state.window_size.height as f32;

        // Both buffers share the same vertical origin: buffer line 0 sits at
        // virtual-y 0, scroll_y shifts everything up. For TextArea clipping
        // we bound each buffer to its column.
        let top = -state.scroll_y;

        let code_bounds = TextBounds {
            left: code_text_left as i32,
            top: 0,
            right: code_text_right as i32,
            bottom: viewport_h as i32,
        };
        let gutter_bounds = TextBounds {
            left: gutter_left as i32,
            top: 0,
            right: (gutter_left + self.gutter_width) as i32,
            bottom: viewport_h as i32,
        };

        let code_area = TextArea {
            buffer: &self.code_buffer,
            left: code_text_left,
            top,
            scale: 1.0,
            bounds: code_bounds,
            default_color: GlyphonColor::rgb(220, 222, 230),
            custom_glyphs: &[],
        };
        let gutter_area = TextArea {
            buffer: &self.gutter_buffer,
            left: gutter_left,
            top,
            scale: 1.0,
            bounds: gutter_bounds,
            default_color: GlyphonColor::rgb(GUTTER_COLOR.0, GUTTER_COLOR.1, GUTTER_COLOR.2),
            custom_glyphs: &[],
        };

        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            [gutter_area, code_area],
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

/// Enumerated in the same integer order as `TokenKind` so indexing by
/// `kind as usize` lands on the right entry.
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

/// Pre-compute `Attrs` values per token kind — avoids per-span struct
/// construction during the hot iteration that feeds `set_rich_text`.
fn attrs_by_kind() -> [Attrs<'static>; N_KINDS] {
    let base = Attrs::new().family(Family::Monospace);
    let mut out = [base; N_KINDS];
    for (i, kind) in ALL_KINDS.iter().enumerate() {
        let (r, g, b) = kind.color();
        out[i] = base.color(GlyphonColor::rgb(r, g, b));
    }
    out
}

/// Build the code `Buffer` with per-byte syntax colors. Streams `(slice, Attrs)`
/// spans into glyphon's `set_rich_text` — at most one span per contiguous run
/// of identical token kinds, so 80-char-line Python files produce ~20 spans
/// per line, not one per byte.
fn build_code_buffer(font_system: &mut FontSystem, hl: &HighlightedSource) -> Buffer {
    let mut buffer = Buffer::new(font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
    // Very wide width effectively disables wrapping; M2 spec inherits M1's
    // "horizontal overflow: ignore, let it clip" posture.
    buffer.set_size(font_system, Some(100_000.0), None);

    let attrs_by_kind = attrs_by_kind();
    let default_attrs = Attrs::new().family(Family::Monospace);

    let contents = &hl.source.contents;
    let kinds = &hl.kinds;
    let spans = RunSpans { contents, kinds, i: 0, attrs_by_kind };

    buffer.set_rich_text(font_system, spans, default_attrs, Shaping::Advanced);
    // We don't prune — prune-true is for finished viewports; here we keep
    // shapes around for smooth scroll.
    buffer.shape_until_scroll(font_system, false);
    buffer
}

/// Build the gutter (line numbers) buffer. All line numbers are materialized
/// once; glyphon lazily shapes only visible ones via `shape_until_scroll`.
/// Returns the buffer and its required pixel width.
fn build_gutter_buffer(font_system: &mut FontSystem, hl: &HighlightedSource) -> (Buffer, f32) {
    let line_count = hl.line_count().max(1);
    let digits = digit_count(line_count);
    let col = digits + GUTTER_DIGIT_PAD;
    let width_px = (col as f32) * MONO_GLYPH_WIDTH;

    // Pre-size: ~7 chars * line_count + newlines.
    let mut text = String::with_capacity((col + 1) * line_count);
    for n in 1..=line_count {
        // Right-align within the column.
        // `{:>width$}` doesn't let us set width from a variable in a const
        // context, but format! does at runtime.
        let s = format!("{:>width$}", n, width = col);
        text.push_str(&s);
        if n != line_count {
            text.push('\n');
        }
    }

    let mut buffer = Buffer::new(font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
    buffer.set_size(font_system, Some(width_px + 4.0), None);
    let attrs = Attrs::new()
        .family(Family::Monospace)
        .color(GlyphonColor::rgb(GUTTER_COLOR.0, GUTTER_COLOR.1, GUTTER_COLOR.2));
    buffer.set_text(font_system, &text, attrs, Shaping::Basic);
    buffer.shape_until_scroll(font_system, false);
    (buffer, width_px)
}

fn digit_count(n: usize) -> usize {
    if n == 0 { 1 } else { (n as f64).log10().floor() as usize + 1 }
}

/// Streaming iterator producing `(&str, Attrs)` spans over a contents/kinds
/// pair, coalescing runs of identical kinds.
struct RunSpans<'a> {
    contents: &'a str,
    kinds: &'a [TokenKind],
    i: usize,
    attrs_by_kind: [Attrs<'static>; N_KINDS],
}

impl<'a> Iterator for RunSpans<'a> {
    type Item = (&'a str, Attrs<'static>);
    fn next(&mut self) -> Option<Self::Item> {
        if self.i >= self.contents.len() {
            return None;
        }
        let start = self.i;
        let k = self.kinds[start];
        let mut end = start + 1;
        while end < self.contents.len() && self.kinds[end] == k {
            end += 1;
        }
        // Advance past any trailing partial UTF-8 character so the slice is
        // valid — kinds are per-byte, so a run can split mid-codepoint for
        // the last char only if the kind boundary fell inside a multibyte
        // character. In practice syntax boundaries align with token edges
        // (tree-sitter reports byte-accurate spans), so this is defensive.
        while end < self.contents.len() && !self.contents.is_char_boundary(end) {
            end += 1;
        }
        self.i = end;
        let attrs = self.attrs_by_kind[k as usize];
        Some((&self.contents[start..end], attrs))
    }
}

/// Nudge glyphon's internal scroll so `shape_until_scroll` focuses on the
/// lines around our target. This is purely a perf hint — positioning is
/// still done via `TextArea.top`.
fn prime_scroll(buffer: &mut Buffer, font_system: &mut FontSystem, target_line: i32) {
    let mut scroll = buffer.scroll();
    scroll.line = target_line.max(0) as usize;
    scroll.vertical = 0.0;
    buffer.set_scroll(scroll);
    buffer.shape_until_scroll(font_system, false);
}

/// Build the egui UI for this frame. Extracted so the render method stays about
/// plumbing, not layout.
///
/// Panes sit over the shared near-black canvas with no borders or separator
/// lines — CLAUDE.md's "Panes as light, not boxes" principle. The luminous
/// activation glow arrives in M4; for now the pane is only identifiable by
/// the placeholder label sitting inside it.
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

    #[test]
    fn digit_count_matches_log10() {
        assert_eq!(digit_count(1), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(99), 2);
        assert_eq!(digit_count(100), 3);
        assert_eq!(digit_count(999), 3);
        assert_eq!(digit_count(1000), 4);
        assert_eq!(digit_count(99_999), 5);
        assert_eq!(digit_count(100_000), 6);
    }
}
