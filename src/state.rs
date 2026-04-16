//! Application state — the single source of truth the renderer derives from.
//!
//! Per CLAUDE.md's "one state, derived rendering" principle: every visual
//! element should be computable as a pure function of this struct (plus, at
//! future milestones, `timeline_position: f32`, diff-operations, camera, etc.).
//!
//! In M1 this is intentionally small. M2 adds pre-computed syntax kinds and
//! line-offset caches. The shape — not the content — is what matters: future
//! milestones extend `AppState` rather than introducing event-driven animation
//! state elsewhere.

use std::ops::Range;

use crate::analyzer::SourceFile;
use crate::syntax::TokenKind;

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

/// A file + its pre-computed syntax kinds + line offsets, the triple the
/// renderer consumes to build virtualized glyphon buffers.
#[derive(Debug)]
pub struct HighlightedSource {
    pub source: SourceFile,
    /// `kinds[i]` is the token kind of byte `i`. `kinds.len() == source.contents.len()`.
    pub kinds: Vec<TokenKind>,
    /// Byte offset of the start of line `n` (0-indexed). Has `line_count + 1`
    /// entries so `line_byte_range(n)` is a single subtraction.
    pub line_offsets: Vec<usize>,
}

impl HighlightedSource {
    pub fn new(source: SourceFile, kinds: Vec<TokenKind>) -> Self {
        let line_offsets = compute_line_offsets(&source.contents);
        Self { source, kinds, line_offsets }
    }

    /// Number of logical lines in the file.
    pub fn line_count(&self) -> usize {
        // `line_offsets` has a sentinel at `contents.len()`, so the count is
        // `len() - 1`.
        self.line_offsets.len().saturating_sub(1)
    }
}

/// Compute byte offsets of the start of each line, plus a sentinel offset at
/// `contents.len()` so `offsets[i+1] - offsets[i]` gives line i's byte length.
fn compute_line_offsets(contents: &str) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(contents.len() / 40 + 2);
    offsets.push(0);
    for (i, b) in contents.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    // Sentinel. If the file doesn't end with '\n' this adds a final line's
    // end offset; if it does, it marks "one past the last newline" (an empty
    // trailing line — matching our SourceFile::lines behavior).
    offsets.push(contents.len());
    offsets
}

#[derive(Debug)]
pub struct AppState {
    /// The file currently shown on the right pane, with its highlight data.
    pub highlighted: HighlightedSource,
    /// Vertical scroll offset in logical pixels. 0 = top of file, grows downward.
    pub scroll_y: f32,
    /// Latest known window size. Kept here so layout math is a pure function of state.
    pub window_size: WindowSize,
    /// Device scale factor (points → physical pixels). Updated on ScaleFactorChanged.
    pub scale_factor: f32,
}

impl AppState {
    pub fn new(highlighted: HighlightedSource) -> Self {
        Self {
            highlighted,
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

    /// Clamp scroll so neither end of the file can leave the viewport in
    /// unreasonable ways. Upper bound: never scroll above the first line.
    /// Lower bound: stop when the last line is still partially on-screen
    /// (keeps one line of context visible rather than a blank canvas).
    pub fn clamp_scroll(&mut self, line_height: f32) {
        let total_h = self.highlighted.line_count() as f32 * line_height;
        // Leave at least one line visible at the bottom — scroll_y can go up
        // to (content_height - one_line).
        let max = (total_h - line_height).max(0.0);
        if self.scroll_y > max {
            self.scroll_y = max;
        }
        if self.scroll_y < 0.0 {
            self.scroll_y = 0.0;
        }
    }
}

/// Given a scroll offset (in pixels from the top of the file), a viewport
/// height (pixels), the line height, and the file's total line count, compute
/// the half-open range `[first, last)` of line indices that intersect the
/// viewport — inflated by `overscan` lines on each side so glyphon can
/// satisfy near-edge requests without stutter.
///
/// Clamps into `[0, total_lines]` and returns an empty range when the file is
/// empty. Negative scroll values (which shouldn't happen but mustn't crash)
/// are treated as zero.
pub fn visible_line_range(
    scroll_y: f32,
    viewport_height: u32,
    line_height: f32,
    total_lines: usize,
    overscan: usize,
) -> Range<usize> {
    if total_lines == 0 || line_height <= 0.0 {
        return 0..0;
    }
    let scroll = scroll_y.max(0.0);
    let first_visible = (scroll / line_height).floor() as isize;
    let last_visible = ((scroll + viewport_height as f32) / line_height).ceil() as isize;

    let first = (first_visible - overscan as isize).max(0) as usize;
    let last = (last_visible as usize).saturating_add(overscan).min(total_lines);

    first.min(total_lines)..last.max(first.min(total_lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    // `visible_line_range` — parameterized over every case that matters:
    // top of file, mid file, past-end-of-file scroll, empty file, overscan
    // clamping. Line height varies so we catch bad rounding assumptions.
    #[rstest]
    // At the top of a big file with no overscan: exactly the viewport.
    #[case::top_no_overscan(0.0,    400, 20.0, 1000, 0,  0..20)]
    // Overscan grows both sides, clamped at 0 on the top.
    #[case::top_with_overscan(0.0,  400, 20.0, 1000, 3,  0..23)]
    // Midway: 100 pixels of scroll @ 20/line = line 5; viewport covers 20 lines.
    #[case::middle(100.0,            400, 20.0, 1000, 0,  5..25)]
    // Fractional scroll: at 10px down, the viewport top shows the last 10px
    // of line 0 and the bottom shows the first 10px of line 20 — 21 lines
    // partially visible.
    #[case::fractional_scroll(10.0,  400, 20.0, 1000, 0,  0..21)]
    // Scroll past the end clamps `last` to total_lines.
    #[case::past_end(30_000.0,       400, 20.0, 1000, 0,  1000..1000)]
    // Empty file returns 0..0 whatever the scroll.
    #[case::empty(500.0,             400, 20.0, 0,    5,  0..0)]
    // Tall viewport, short file: range clamped to file length.
    #[case::short_file(0.0,          1000, 20.0, 5,   2,  0..5)]
    // Non-integer line height; fractional arithmetic is still fine.
    #[case::fractional_line_height(0.0, 100, 14.5, 1000, 0, 0..7)]
    fn visible_line_range_cases(
        #[case] scroll_y: f32,
        #[case] viewport: u32,
        #[case] line_h: f32,
        #[case] total: usize,
        #[case] overscan: usize,
        #[case] expected: Range<usize>,
    ) {
        assert_eq!(
            visible_line_range(scroll_y, viewport, line_h, total, overscan),
            expected,
        );
    }

    #[test]
    fn line_offsets_handles_trailing_newline() {
        let c = "a\nbb\nccc\n";
        let offsets = compute_line_offsets(c);
        // "a" [0..2) (includes newline), "bb" [2..5), "ccc" [5..9), empty final
        assert_eq!(offsets, vec![0, 2, 5, 9, 9]);
    }

    #[test]
    fn line_offsets_handles_no_trailing_newline() {
        let c = "a\nbb";
        let offsets = compute_line_offsets(c);
        assert_eq!(offsets, vec![0, 2, 4]);
    }
}
