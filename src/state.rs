//! Application state — the single source of truth the renderer derives from.
//!
//! Per CLAUDE.md's "one state, derived rendering" principle: every visual
//! element should be computable as a pure function of this struct (plus, at
//! future milestones, `timeline_position: f32`, diff-operations, camera, etc.).
//!
//! In M1 this is intentionally small. The shape — not the content — is what
//! matters: future milestones extend `AppState` rather than introducing event-
//! driven animation state elsewhere.

use crate::analyzer::SourceFile;

/// Window size in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

/// The fraction of window width reserved for the file-tree pane on the left.
/// M1 shows a placeholder; M4 fills it in with a real tree.
pub const LEFT_PANE_FRACTION: f32 = 0.25;

/// Scroll speed multiplier applied to line-based mouse wheel deltas.
/// Pixel-delta trackpads bypass this.
pub const LINE_SCROLL_PIXELS: f32 = 36.0;

#[derive(Debug)]
pub struct AppState {
    /// The file currently shown on the right pane.
    pub source: SourceFile,
    /// Vertical scroll offset in logical pixels. 0 = top of file, grows downward.
    pub scroll_y: f32,
    /// Latest known window size. Kept here so layout math is a pure function of state.
    pub window_size: WindowSize,
    /// Device scale factor (points → physical pixels). Updated on ScaleFactorChanged.
    pub scale_factor: f32,
}

impl AppState {
    pub fn new(source: SourceFile) -> Self {
        Self {
            source,
            scroll_y: 0.0,
            window_size: WindowSize { width: 1280, height: 800 },
            scale_factor: 1.0,
        }
    }

    /// Width of the code pane in physical pixels.
    pub fn code_pane_width(&self) -> u32 {
        let left = (self.window_size.width as f32 * LEFT_PANE_FRACTION).round() as u32;
        self.window_size.width.saturating_sub(left)
    }

    /// Left edge of the code pane in physical pixels.
    pub fn code_pane_left(&self) -> u32 {
        (self.window_size.width as f32 * LEFT_PANE_FRACTION).round() as u32
    }

    /// Clamp scroll so the top of the file never scrolls below the viewport top.
    /// A lower bound (preventing scrolling past the file end) needs the rendered
    /// text height, which glyphon computes after layout — leave that to M2 when
    /// virtualized scroll lands.
    pub fn clamp_scroll(&mut self) {
        if self.scroll_y < 0.0 {
            self.scroll_y = 0.0;
        }
    }
}
