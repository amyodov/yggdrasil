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

use std::collections::HashMap;

use crate::analyzer::SourceFile;
use crate::cards::{Card, CardId};
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

/// How many lines a single notch of a line-delta mouse wheel scrolls.
/// Trackpads provide pixel-delta and skip this.
pub const LINES_PER_WHEEL_NOTCH: f32 = 3.0;

/// Base text metrics in *logical points*. The renderer multiplies by
/// `AppState::scale_factor` to get physical pixels at display time.
/// 14/20 reads comfortably at 1x and 2x.
pub const BASE_FONT_SIZE: f32 = 14.0;
pub const BASE_LINE_HEIGHT: f32 = 20.0;

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
    /// Build from already-computed pieces. Caller parses once for both kinds
    /// and card extraction, so we don't re-walk the contents here.
    pub fn from_parts(source: SourceFile, kinds: Vec<TokenKind>, line_offsets: Vec<usize>) -> Self {
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
pub fn compute_line_offsets(contents: &str) -> Vec<usize> {
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

/// Duration of the fold/unfold animation. Constants like this live in state
/// because the per-frame `tick` method uses them; renderer-only constants
/// live in renderer.rs.
pub const FOLD_DURATION_SECS: f32 = 0.2;

/// Discrete fold-control target states. The card fold UI is a multi-position
/// switch: each state lives in a fixed slot, and a "traveling well" slides
/// between slots to indicate the current state. Today each card supports
/// two states (`Folded` / `Unfolded`); M3.4 will add `HeaderOnly` to the
/// middle slot for cards whose body starts with a docstring.
///
/// The set of states is **per-card** (see `card_fold_states`) — not every
/// card will gain `HeaderOnly`. A function without a docstring stays as a
/// 2-slot switch even in the M3.4 world. The `FoldState` enum itself is
/// context-free; slot layout lives on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FoldState {
    Folded,
    Unfolded,
}

impl FoldState {
    /// Position on the continuous `fold_progress` axis corresponding to this
    /// discrete state. 0.0 = fully folded; 1.0 = fully unfolded. Used when
    /// a button click commits a new target.
    pub fn target_progress(self) -> f32 {
        match self {
            FoldState::Folded => 0.0,
            FoldState::Unfolded => 1.0,
        }
    }
}

/// Ordered fold states this card supports — equivalent to the physical slot
/// order on the switch, lowest index = most folded. Empty for snippets (no
/// body to fold). Today every non-snippet card returns `[Folded, Unfolded]`;
/// M3.4 will insert `HeaderOnly` in the middle for cards whose body starts
/// with a docstring and leave docstring-less cards at 2 slots.
pub fn card_fold_states(card: &Card) -> &'static [FoldState] {
    use crate::cards::CardKind;
    static TWO_SLOT: &[FoldState] = &[FoldState::Folded, FoldState::Unfolded];
    static NONE: &[FoldState] = &[];
    if matches!(card.kind, CardKind::Snippet) {
        NONE
    } else {
        TWO_SLOT
    }
}

/// Slot index (0-based) of `state` within the given card's fold-switch
/// layout, or `None` if `state` isn't a slot on this card (e.g. asking for
/// `HeaderOnly` on a docstring-less card).
///
/// Unused today — the renderer iterates `card_fold_states` with `.enumerate()`
/// so it gets the slot index for free. M3.4's `HeaderOnly` handling will use
/// this to ask "does this card have slot N?" when routing transitions that
/// skip a slot on docstring-less cards.
#[allow(dead_code)]
pub fn card_slot_index(card: &Card, state: FoldState) -> Option<usize> {
    card_fold_states(card).iter().position(|&s| s == state)
}

/// Well position for this card given a continuous `fold_progress`, as a
/// floating-point slot index (0..=slot_count-1). Integer values sit exactly
/// over a slot; fractional values are mid-slide. Clamped so upstream
/// interpolation overshoots can't send the well off the strip.
///
/// Today's mapping is linear (progress 0 → slot 0, progress 1 → slot N-1).
/// When M3.4 adds `HeaderOnly`, the mapping becomes piecewise for cards
/// that have it so the well passes through the middle slot at the
/// appropriate fold_progress.
pub fn card_well_position(card: &Card, fold_progress: f32) -> f32 {
    let count = card_fold_states(card).len();
    if count <= 1 {
        return 0.0;
    }
    fold_progress.clamp(0.0, 1.0) * (count - 1) as f32
}

#[derive(Debug)]
pub struct AppState {
    /// The file currently shown on the right pane, with its highlight data.
    pub highlighted: HighlightedSource,
    /// Cards (classes / functions / methods) in source order.
    pub cards: Vec<Card>,
    /// Per-card fold animation progress. 1.0 = fully unfolded (default);
    /// 0.0 = fully folded. Missing entry = 1.0.
    pub fold_progress: HashMap<CardId, f32>,
    /// Per-card fold target. The per-frame tick advances `fold_progress`
    /// toward `fold_target`. Missing entry = 1.0 (unfolded).
    pub fold_target: HashMap<CardId, f32>,
    /// Vertical scroll offset in physical pixels.
    pub scroll_y: f32,
    /// Latest known window size. Kept here so layout math is a pure function of state.
    pub window_size: WindowSize,
    /// Device scale factor (points → physical pixels). Updated on ScaleFactorChanged.
    pub scale_factor: f32,
    /// Last known cursor position in physical pixels. None if outside window.
    pub cursor_pos: Option<(f32, f32)>,
    /// Which fold-switch button the user is currently pressing. Identified
    /// by card + target state since a card renders one button per slot.
    /// Drives the rubber-button press-state visual on the pressed chip. On
    /// release *over the same button* the card's fold target is set to that
    /// button's state (and the well begins sliding toward that slot); on
    /// release elsewhere the press cancels.
    pub pressing_button: Option<(CardId, FoldState)>,
}

impl AppState {
    pub fn new(highlighted: HighlightedSource, cards: Vec<Card>) -> Self {
        Self {
            highlighted,
            cards,
            fold_progress: HashMap::new(),
            fold_target: HashMap::new(),
            scroll_y: 0.0,
            window_size: WindowSize { width: 1280, height: 800 },
            scale_factor: 1.0,
            cursor_pos: None,
            pressing_button: None,
        }
    }

    /// Advance fold animations by `dt` seconds. Returns true if any card is
    /// still animating — the event loop uses this to decide Poll vs Wait.
    pub fn tick_animations(&mut self, dt: f32) -> bool {
        let step = (dt / FOLD_DURATION_SECS).clamp(0.0, 1.0);
        let mut any_animating = false;
        // Union of both maps' keys: a target without a progress entry is
        // treated as starting from 1.0 (the default unfolded state).
        let keys: Vec<CardId> = self
            .fold_target
            .keys()
            .chain(self.fold_progress.keys())
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        for id in keys {
            let target = self.fold_target.get(&id).copied().unwrap_or(1.0);
            let current = self.fold_progress.get(&id).copied().unwrap_or(1.0);
            if (current - target).abs() < 1e-4 {
                self.fold_progress.insert(id, target);
                continue;
            }
            let delta = target - current;
            let next = current + delta.signum() * step.min(delta.abs());
            self.fold_progress.insert(id, next);
            any_animating = true;
        }
        any_animating
    }

    /// Set the fold target for `card_id` to a specific discrete state. The
    /// animation advances over subsequent frames via `tick_animations`; the
    /// renderer's well visual interpolates its horizontal position from
    /// `fold_progress` in lockstep. Takes a `FoldState` (not a raw `f32`)
    /// because each click is a directed request to reach a named state —
    /// the multi-button switch never emits an ambiguous "flip it somehow"
    /// intent.
    pub fn set_fold_target(&mut self, card_id: CardId, target: FoldState) {
        self.fold_target.insert(card_id, target.target_progress());
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

    /// Font size in physical pixels, scaled for the current display DPI.
    pub fn effective_font_size(&self) -> f32 {
        BASE_FONT_SIZE * self.scale_factor
    }

    /// Line height in physical pixels, scaled for the current display DPI.
    pub fn effective_line_height(&self) -> f32 {
        BASE_LINE_HEIGHT * self.scale_factor
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

// M2's `visible_line_range` lived here — M3 drives virtualization through
// card layout + culling instead (each card has its own glyphon buffer, and
// glyphon's shape_until_scroll handles per-card laziness). If we re-need a
// pure "first/last visible line" helper for a future non-card view, restore
// from git history and its rstest cases.

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `card_well_position` and `card_slot_index` read the per-card slot
    /// layout. Today's non-snippet cards are all 2-slot; snippets have no
    /// slots. These tests lock in the shape before M3.4 introduces per-card
    /// slot-count variance (HeaderOnly for cards with a docstring).
    #[test]
    fn well_position_interpolates_between_slots_for_a_two_slot_card() {
        use crate::cards::{Card, CardId, CardKind, MethodModifier, Visibility};
        let card = Card {
            id: CardId(0),
            kind: CardKind::Function,
            parent: None,
            depth: 0,
            name: "f".into(),
            visibility: Visibility::Public,
            modifier: MethodModifier::None,
            decorators: vec![],
            header_range: 0..0,
            body_range: None,
            full_range: 0..0,
            header_lines: 0..0,
            body_lines: None,
            full_lines: 0..0,
        };
        assert_eq!(card_fold_states(&card).len(), 2);
        assert_eq!(card_well_position(&card, 0.0), 0.0);
        assert_eq!(card_well_position(&card, 1.0), 1.0);
        assert!((card_well_position(&card, 0.5) - 0.5).abs() < 1e-6);
        // Out-of-range fold_progress clamps back onto the strip.
        assert_eq!(card_well_position(&card, -0.2), 0.0);
        assert_eq!(card_well_position(&card, 1.2), 1.0);
        // Slot indices for the two states land at 0 and 1.
        assert_eq!(card_slot_index(&card, FoldState::Folded), Some(0));
        assert_eq!(card_slot_index(&card, FoldState::Unfolded), Some(1));
    }

    /// Snippet cards have no fold-switch slots; `well_position` collapses to
    /// 0 and `slot_index` returns `None`.
    #[test]
    fn snippet_cards_have_no_fold_slots() {
        use crate::cards::{Card, CardId, CardKind, MethodModifier, Visibility};
        let snippet = Card {
            id: CardId(0),
            kind: CardKind::Snippet,
            parent: None,
            depth: 0,
            name: "import_statement".into(),
            visibility: Visibility::Public,
            modifier: MethodModifier::None,
            decorators: vec![],
            header_range: 0..0,
            body_range: None,
            full_range: 0..0,
            header_lines: 0..0,
            body_lines: None,
            full_lines: 0..0,
        };
        assert!(card_fold_states(&snippet).is_empty());
        assert_eq!(card_well_position(&snippet, 0.5), 0.0);
        assert_eq!(card_slot_index(&snippet, FoldState::Folded), None);
    }
}
