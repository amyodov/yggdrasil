//! winit `ApplicationHandler` implementation — event→state→render bridge.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::{CursorIcon, Window, WindowId};

use crate::blind;
use crate::cards::{layout_cards, CardId};
use crate::cli::WrapMode;
use crate::renderer::{fold_buttons_scene, plate_rect, Renderer, SCENE_TOP_PAD_PT};
use crate::sky::DEFAULT_DAY_CYCLE_SECS;
use crate::state::{AppState, FoldState, WindowSize, LINES_PER_WHEEL_NOTCH};

const INITIAL_WIDTH: u32 = 1280;
const INITIAL_HEIGHT: u32 = 800;

pub struct App {
    state: AppState,
    renderer: Option<Renderer>,
    /// Wall-clock instant at the previous frame. `None` before first frame.
    last_frame: Option<Instant>,
    /// Last simulated-time HH:MM printed to the console (under
    /// `--debug-day-loop-length`). Kept so we print once per minute of
    /// simulated time instead of spamming every frame.
    last_debug_time_print: Option<(u32, u32)>,
}

impl App {
    pub fn new(state: AppState) -> Self {
        Self { state, renderer: None, last_frame: None, last_debug_time_print: None }
    }

    pub fn run(self) -> Result<()> {
        let event_loop = winit::event_loop::EventLoop::new()?;
        // Poll always: the void has ambient breathing/cloud-drift animation
        // that must keep ticking even when nothing else is happening.
        // `about_to_wait` requests the next redraw; VSync (AutoVsync) caps
        // the rate at display refresh.
        //
        // A future optimization could drop to WaitUntil with a ~30fps budget
        // when the window is unfocused; for the prototype, 60fps background
        // animation is fine.
        event_loop.set_control_flow(ControlFlow::Poll);
        let mut app = self;
        event_loop.run_app(&mut app)?;
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
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
        self.state.window_size =
            WindowSize { width: size.width.max(1), height: size.height.max(1) };
        self.state.scale_factor = window.scale_factor() as f32;

        let renderer = match pollster::block_on(Renderer::new(
            window.clone(),
            &self.state.highlighted,
            &self.state.cards,
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
        self.last_frame = Some(Instant::now());
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(renderer) = self.renderer.as_mut() else { return };

        // egui sees every event first; we don't block our handlers on its
        // `consumed` flag in M3 (file-tree panel is a placeholder).
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
                self.state.scale_factor = scale_factor as f32;
                renderer.window().request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                // Cursor positions come in physical pixels on winit 0.30.
                let cx = position.x as f32;
                let cy = position.y as f32;
                self.state.cursor_pos = Some((cx, cy));
                // Slat rows read as clickable affordances — switch the
                // cursor to a pointer so the user stops seeing the i-beam
                // the platform defaults to. Actual click handling lands in
                // Phase C; the cursor icon is the early affordance.
                let over_slat = cursor_over_slat(&self.state, cx, cy);
                let icon = if over_slat {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                };
                renderer.window().set_cursor(icon);
            }

            WindowEvent::CursorLeft { .. } => {
                self.state.cursor_pos = None;
                renderer.window().set_cursor(CursorIcon::Default);
            }

            WindowEvent::MouseInput { state: btn_state, button: MouseButton::Left, .. } => {
                // Fold-switch press lifecycle:
                //   - Pressed over a slot → `begin_press`. This captures
                //     the pre-press fold_target AND redirects fold_target
                //     to the clicked slot — the well starts sliding
                //     toward the pressed slot immediately, before the
                //     user even releases. That moving well IS the "I
                //     heard you" feedback.
                //   - Released over the same slot → commit. The well is
                //     already (approaching) the target; just drop the
                //     press record.
                //   - Released elsewhere → cancel. Restore fold_target;
                //     the well animates back.
                let metrics = renderer.layout_metrics(&self.state);
                match btn_state {
                    ElementState::Pressed => {
                        if let Some((card_id, target)) =
                            hit_test_fold_button(&self.state, metrics)
                        {
                            self.state.begin_press(card_id, target);
                            renderer.window().request_redraw();
                        }
                    }
                    ElementState::Released => {
                        let Some(press) = self.state.press else {
                            return;
                        };
                        let released_on =
                            hit_test_fold_button(&self.state, metrics);
                        if released_on == Some((press.card_id, press.clicked_state)) {
                            self.state.commit_press();
                        } else {
                            self.state.cancel_press();
                        }
                        renderer.window().request_redraw();
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let lh = self.state.effective_line_height();
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (
                        x * LINES_PER_WHEEL_NOTCH * lh,
                        y * LINES_PER_WHEEL_NOTCH * lh,
                    ),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
                };
                // Route vertical scroll to whichever pane the cursor is
                // over. Horizontal scroll goes to the code pane's scroll_x
                // when wrap is off (the only context in which horizontal
                // content can exist).
                let over_tree_pane = match self.state.cursor_pos {
                    Some((cx, _)) => {
                        self.state.tree.is_some()
                            && cx < self.state.code_pane_left() as f32
                    }
                    None => false,
                };
                if over_tree_pane {
                    scroll_tree(&mut self.state, dy);
                } else {
                    self.state.scroll_y -= dy;
                    self.state.clamp_scroll(lh);
                    if self.state.wrap_mode == WrapMode::Off && dx.abs() > 0.5 {
                        self.state.scroll_x -= dx;
                        let max_x = 5000.0;
                        self.state.scroll_x = self.state.scroll_x.clamp(0.0, max_x);
                    }
                }
                renderer.window().request_redraw();
            }

            WindowEvent::RedrawRequested => {
                // Advance fold animations using the time since the last frame.
                let now = Instant::now();
                let dt = match self.last_frame {
                    Some(prev) => (now - prev).as_secs_f32().min(0.1), // clamp to 100ms to avoid big jumps
                    None => 0.0,
                };
                self.last_frame = Some(now);

                // Fold animations still use the dt tick, but redraw scheduling
                // is handled unconditionally by `about_to_wait` (the void
                // always breathes).
                let _ = self.state.tick_animations(dt);
                // Advance the SkyLight clock. Every environmental-light-
                // dependent visual (nebula tint, lens glint, foil specular,
                // …) derives from this single scalar.
                self.state.advance_clock(dt);

                // Under --debug-day-loop-length, print the simulated time
                // of day once per simulated minute — lets the tuner match
                // what they see on screen to a clock position in the cycle.
                if (self.state.day_cycle_secs - DEFAULT_DAY_CYCLE_SECS).abs() > 1e-3 {
                    let tod = self.state.time_of_day_hours();
                    let hh = tod.floor() as u32 % 24;
                    let mm = ((tod - tod.floor()) * 60.0).floor() as u32 % 60;
                    if self.last_debug_time_print != Some((hh, mm)) {
                        println!("[sky] time-of-day {hh:02}:{mm:02}");
                        self.last_debug_time_print = Some((hh, mm));
                    }
                }

                match renderer.render(&self.state) {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
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

    /// In Poll mode, called after each batch of events is processed. Request
    /// a redraw here so the void's breathing/cloud animation keeps ticking
    /// independent of user input. VSync caps the actual rate.
    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.window().request_redraw();
        }
    }
}

/// True if `(cursor_x, cursor_y)` (window-space physical pixels) falls
/// over any slat's clickable slot. Recomputes the blind layout on
/// demand — cheap, flatten + a handful of rect computations. Returns
/// false when no tree is loaded.
fn cursor_over_slat(state: &AppState, cursor_x: f32, cursor_y: f32) -> bool {
    let Some(tree) = state.tree.as_ref() else {
        return false;
    };
    let pane_width = state.code_pane_left() as f32;
    let pane_height = state.window_size.height as f32;
    let layout = blind::layout(
        tree,
        0.0,
        pane_width,
        pane_height,
        state.scale_factor,
        state.slat_mode,
    );
    blind::hit_test_slat(&layout, cursor_x, cursor_y)
}

/// Apply `dy` of scroll delta to the tree's scroll_y and clamp it to
/// legal bounds. Max scroll is (content_height - viewport_height) or 0
/// if the blind's content fits the viewport.
fn scroll_tree(state: &mut AppState, dy: f32) {
    let Some(tree) = state.tree.as_mut() else {
        return;
    };
    let sf = state.scale_factor;
    let slot = (blind::SLAT_HEIGHT_PT + blind::SLAT_GAP_PT) * sf;
    let count = crate::filetree::flatten(tree).len();
    let content_h = count as f32 * slot;
    let viewport_h =
        (state.window_size.height as f32 - 2.0 * blind::BLIND_MARGIN_PT * sf).max(0.0);
    let max = (content_h - viewport_h).max(0.0);
    tree.scroll_y -= dy;
    if tree.scroll_y < 0.0 {
        tree.scroll_y = 0.0;
    }
    if tree.scroll_y > max {
        tree.scroll_y = max;
    }
}

/// Is the cursor (from `state.cursor_pos`) over a fold-switch button?
/// Returns the card + target FoldState of the hit button, or `None` if the
/// cursor isn't over any. Free function so it doesn't alias with the `&mut
/// renderer` borrow in the event loop.
///
/// Coordinate systems (M3.1): `fold_buttons_scene` returns **plate-local**
/// coordinates (x, y relative to the plate's top-left). The cursor comes
/// in **window-space**. We convert cursor → plate-local using `plate_rect`,
/// then compare.
fn hit_test_fold_button(
    state: &AppState,
    metrics: crate::cards::LayoutMetrics,
) -> Option<(CardId, FoldState)> {
    let (cx, cy) = state.cursor_pos?;
    let (plate_pos, _) = plate_rect(state);
    let local_cx = cx - plate_pos[0];
    let local_cy = cy - plate_pos[1];
    let scene_top_local = SCENE_TOP_PAD_PT * state.scale_factor;
    let layout = layout_cards(&state.cards, &state.fold_progress, metrics);
    for card in &state.cards {
        let Some(rect) = layout.rects.get(&card.id) else { continue };
        for (target, (hx, hy, hw, hh)) in fold_buttons_scene(card, rect, state) {
            let local_hy = hy - state.scroll_y + scene_top_local;
            if local_cx >= hx
                && local_cx <= hx + hw
                && local_cy >= local_hy
                && local_cy <= local_hy + hh
            {
                return Some((card.id, target));
            }
        }
    }
    None
}
