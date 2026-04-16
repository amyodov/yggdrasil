//! winit `ApplicationHandler` implementation — the event→state→render bridge.

use std::sync::Arc;

use anyhow::Result;
use winit::application::ApplicationHandler;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::renderer::Renderer;
use crate::state::{AppState, WindowSize, LINES_PER_WHEEL_NOTCH};

/// Initial window size request. Real systems may snap to DPI/display bounds.
const INITIAL_WIDTH: u32 = 1280;
const INITIAL_HEIGHT: u32 = 800;

/// The winit-facing application.
///
/// Winit 0.30 requires the window to be created in `resumed`, not up-front
/// (Android parity) — so all GPU state is `Option` until that happens.
pub struct App {
    state: AppState,
    /// `None` until the event loop calls `resumed` and we can create a window.
    renderer: Option<Renderer>,
}

impl App {
    pub fn new(state: AppState) -> Self {
        Self { state, renderer: None }
    }

    pub fn run(self) -> Result<()> {
        let event_loop = winit::event_loop::EventLoop::new()?;
        // Poll produces redraw-on-demand behavior combined with request_redraw
        // from `about_to_wait`. Animation milestones may switch to a continuous
        // drive; for M1 on-demand is enough.
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
        let mut app = self;
        event_loop.run_app(&mut app)?;
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            // On desktop `resumed` fires exactly once, but the handler must be
            // idempotent for mobile-parity — on second entry we reuse.
            return;
        }

        let window_attrs = Window::default_attributes()
            .with_title(format!(
                "Ygg — {}",
                self.state.highlighted.source.path.display()
            ))
            .with_inner_size(winit::dpi::LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));

        let window = match event_loop.create_window(window_attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        self.state.window_size = WindowSize { width: size.width.max(1), height: size.height.max(1) };
        self.state.scale_factor = window.scale_factor() as f32;

        let renderer = match pollster::block_on(Renderer::new(
            window.clone(),
            &self.state.highlighted,
            self.state.effective_font_size(),
            self.state.effective_line_height(),
        )) {
            Ok(r) => r,
            Err(e) => {
                log::error!("renderer init failed: {e:#}");
                event_loop.exit();
                return;
            }
        };
        self.renderer = Some(renderer);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(renderer) = self.renderer.as_mut() else { return };

        // egui peeks every window event first; if it consumed it we still
        // let our own handlers run for everything except pointer input in
        // the egui region. For M1 (static HUD) the simpler rule "let both
        // see it" is fine.
        let window = renderer.window().clone();
        let _ = renderer.egui_state_mut().on_window_event(&window, &event);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                let s = WindowSize { width: size.width.max(1), height: size.height.max(1) };
                self.state.window_size = s;
                renderer.resize(s);
                renderer.window().request_redraw();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Text metrics are derived from scale_factor on every frame
                // inside the renderer, so updating state is sufficient — no
                // explicit buffer rebuild needed here.
                self.state.scale_factor = scale_factor as f32;
                renderer.window().request_redraw();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // Right-region only is the intent, but M1 doesn't differentiate —
                // egui already absorbs wheel when the cursor is over it.
                let lh = self.state.effective_line_height();
                let dy = match delta {
                    // Line-delta wheels jump `LINES_PER_WHEEL_NOTCH` per notch;
                    // scale by line height so the feel is constant across DPI.
                    MouseScrollDelta::LineDelta(_x, y) => y * LINES_PER_WHEEL_NOTCH * lh,
                    // Pixel-delta trackpads report in physical pixels already.
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                // Upward wheel scrolls text upward (scroll_y increases).
                self.state.scroll_y -= dy;
                self.state.clamp_scroll(lh);
                renderer.window().request_redraw();
            }

            WindowEvent::RedrawRequested => {
                match renderer.render(&self.state) {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        // Force a reconfigure on the next frame by replaying current size.
                        renderer.resize(self.state.window_size);
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        log::error!("surface OOM — exiting");
                        event_loop.exit();
                    }
                    Err(e) => log::warn!("surface error: {e:?}"),
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Wait mode: no continuous redraws. Explicit request_redraw from input
        // handlers drives repaint. M4+ with ambient background animation will
        // switch to Poll.
    }
}
