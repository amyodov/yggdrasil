//! Card extraction — turns a tree-sitter Python tree into a flat `Vec<Card>`
//! in source order, with parent/child relationships encoded via `parent` +
//! `depth` fields.
//!
//! Why flat (not nested)? Rendering, layout, hit-testing, and fold-state all
//! want O(1) access-by-id and iteration in source order. Nested structures
//! are an awkward fit. Parent/child is a cheap annotation on the flat list.
//!
//! This file also contains the pure `layout_cards` function — card rectangles
//! as a function of (cards, fold_progress, metrics). Kept alongside the card
//! data so everything card-shape-related lives in one place.

use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CardId(pub u32);

/// Opaque line identifier. Stable within the current scene description
/// (extracted from a given source). Cross-version stability — the same
/// logical line retaining the same ID after an edit — is M6's
/// responsibility (AST-matching diff). Today we seed IDs from the line's
/// 0-based offset in the source file, which is stable within one version
/// and trivially derivable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineId(pub u32);

/// One logical line of text inside a card. Has stable identity within
/// the scene (`id`), a byte range into the source, an index inside its
/// parent card, and per-line animation state hooks: `opacity` (0..1)
/// and `y_offset` (pixels). Today `opacity = 1.0` and `y_offset = 0.0`
/// for every line — the rendering path ignores these fields. M7's
/// scrub-driven diff animations will start reading from them to apply
/// per-line visuals (added/removed flashes, line slides for moves)
/// without retrofitting glyphon's one-buffer-per-card model.
///
/// This is the core M6.0 architectural plant: a data structure the
/// renderer doesn't consume today, but that we can fill with animation
/// state before that pipeline is built, so M6/M7 don't need a separate
/// data-model migration.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields are M6/M7 consumer hooks; not read today.
pub struct LogicalLine {
    pub id: LineId,
    /// Byte range of this line in the source file (excluding trailing
    /// newline where applicable).
    pub byte_range: Range<usize>,
    /// Zero-based index within the parent card's ordered line list.
    pub line_index_in_card: u32,
    /// 0.0 = fully transparent, 1.0 = fully opaque. Default 1.0.
    pub opacity: f32,
    /// Pixels. Default 0.0 (line at its natural position). Positive =
    /// shifted downward.
    pub y_offset: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    /// `class Foo: ...` at any nesting level.
    Class,
    /// `def foo(): ...` at module top level.
    Function,
    /// `def foo(self): ...` inside a class body.
    Method,
    /// Top-level orphan code — anything that isn't a def/class, rendered so
    /// nothing in the file is invisible. Covers imports, module constants,
    /// and control-flow blocks like `if __name__ == "__main__":`.
    Snippet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Default: name does NOT start with `_`.
    Public,
    /// Name starts with `_` (single or double underscore).
    Private,
}

/// Per-method modifiers that change how the card attaches to the class
/// armature (see CLAUDE.md "The class armature"). For M3 we detect these
/// but the visual differentiation is intentionally crude; the final
/// armature comes in M8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodModifier {
    None,
    Classmethod,
    Staticmethod,
    Property,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // `name`, `body_range`, `header_lines`, `decorators` are
// part of the Card shape we commit to from M3 onward. They land in window
// titles, future tooltips, M6 diff presentation, and M8 semantic visuals
// (dashed-outline abstract methods, dataclass-aware attribute rendering);
// keeping them now avoids re-deriving them later.
pub struct Card {
    pub id: CardId,
    pub kind: CardKind,
    pub parent: Option<CardId>,
    pub depth: usize,
    pub name: String,
    pub visibility: Visibility,
    pub modifier: MethodModifier,
    /// All decorators attached to this card, in source order, with `@` and
    /// call arguments stripped. Dotted identifiers are preserved
    /// (`@abc.abstractmethod` → `"abc.abstractmethod"`, `@functools.wraps(f)`
    /// → `"functools.wraps"`). `MethodModifier` is derived from a subset of
    /// these; `is_abstract()` / `is_dataclass()` are derived from the rest.
    /// Empty for snippets and for non-decorated definitions.
    pub decorators: Vec<String>,

    /// Byte range of the `def ... :` or `class ... :` header (the signature line).
    /// Does NOT include preceding decorators.
    pub header_range: Range<usize>,
    /// Byte range of the body (the indented block after the colon). `None` for
    /// empty/placeholder definitions.
    pub body_range: Option<Range<usize>>,
    /// Byte range of the full definition including preceding decorators.
    pub full_range: Range<usize>,

    /// Line range of the header (typically 1 line; can be multi-line for long
    /// signatures). Inclusive-start, exclusive-end.
    pub header_lines: Range<usize>,
    /// Line range of the body. None iff body_range is None.
    pub body_lines: Option<Range<usize>>,
    /// Line range of the full definition including preceding decorators. For
    /// a non-decorated function this equals (header_lines.start..body_lines.end).
    /// For a decorated function, `full_lines.start` is the line of the `@`.
    /// Used by layout to size leaf cards against the actually-rendered text.
    pub full_lines: Range<usize>,

    /// Byte range of the leading docstring, if any. Python docstrings are
    /// the first statement of a function/class body when it's a string
    /// literal; tree-sitter reports this as an `expression_statement`
    /// whose sole child is a `string`. `None` means this card has no
    /// docstring and therefore participates in the 2-state (Folded /
    /// Unfolded) fold switch rather than the 3-state form.
    pub docstring_range: Option<Range<usize>>,
    /// Line range of the docstring (parallel to `docstring_range`), used
    /// by layout to piecewise-scale `body_h` through the HeaderOnly fold
    /// state: `body_h` grows from 0 to `docstring_lines.len() * line_h`
    /// as the fold goes 0 → HeaderOnly, then from docstring height to
    /// full body height as fold goes HeaderOnly → Unfolded.
    pub docstring_lines: Option<Range<usize>>,

    /// The M6.0 per-line model: one `LogicalLine` per line covered by
    /// `full_lines`, with stable line-identity hooks and per-line
    /// animation state. Populated during extraction; ignored by today's
    /// rendering path. M6/M7 will start reading from these for diff
    /// states and per-line scrub animations without needing to change
    /// the card data model.
    pub lines: Vec<LogicalLine>,
}

// Semantic-rules accessors on Card. `#[allow(dead_code)]` because consumers
// (M8 armature — dashed outline for abstract, dataclass-aware attribute
// rendering) don't exist yet. Tests cover both methods today.
#[allow(dead_code)]
impl Card {
    /// True if any decorator on this card names one of Python's abstract
    /// decorators as its final dotted component — matches `@abstractmethod`,
    /// `@abc.abstractmethod`, `@abstractstaticmethod`, `@abstractclassmethod`,
    /// `@abstractproperty`.
    pub fn is_abstract(&self) -> bool {
        self.decorators.iter().any(|d| {
            let last = d.rsplit('.').next().unwrap_or(d);
            matches!(
                last,
                "abstractmethod"
                    | "abstractstaticmethod"
                    | "abstractclassmethod"
                    | "abstractproperty"
            )
        })
    }

    /// True if this is a `Class` card decorated with `@dataclass` (bare or
    /// dotted — `@dataclasses.dataclass` also counts). Always false for
    /// non-class cards: dataclass decoration on a method is not a Python
    /// idiom and would be ignored by the semantic layer anyway.
    pub fn is_dataclass(&self) -> bool {
        if self.kind != CardKind::Class {
            return false;
        }
        self.decorators.iter().any(|d| {
            let last = d.rsplit('.').next().unwrap_or(d);
            last == "dataclass"
        })
    }
}

/// Walk a parse tree and extract Cards in source order.
pub fn extract_cards(tree: &Tree, source: &str, line_offsets: &[usize]) -> Vec<Card> {
    let mut out = Vec::new();
    let mut next_id: u32 = 0;
    let root = tree.root_node();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        process_statement(child, source, line_offsets, None, 0, &mut out, &mut next_id);
    }
    out
}

/// Process a top-level or class-body statement. Only cares about
/// `function_definition`, `class_definition`, and `decorated_definition`;
/// everything else is ignored (module-level assignments, imports, etc.).
fn process_statement(
    node: Node,
    source: &str,
    line_offsets: &[usize],
    parent: Option<CardId>,
    depth: usize,
    out: &mut Vec<Card>,
    next_id: &mut u32,
) {
    // Unwrap `decorated_definition` down to its definition + decorators list.
    let (def_node, full_start) = match node.kind() {
        "function_definition" | "class_definition" => (node, node.start_byte()),
        "decorated_definition" => {
            let mut cursor = node.walk();
            let mut def = None;
            for c in node.children(&mut cursor) {
                if matches!(c.kind(), "function_definition" | "class_definition") {
                    def = Some(c);
                    break;
                }
            }
            match def {
                Some(d) => (d, node.start_byte()),
                None => return,
            }
        }
        "comment" => return,
        // Anything else at module top level becomes a Snippet card. Skipped
        // inside class bodies (parent.is_some()) — class-level statements
        // (class-vars, dataclass fields) get their own visual treatment in
        // a future semantic-rendering pass, not a generic snippet.
        _ => {
            if parent.is_none() {
                emit_snippet(node, line_offsets, out, next_id);
            }
            return;
        }
    };

    let kind = match def_node.kind() {
        "function_definition" if parent.is_some() => CardKind::Method,
        "function_definition" => CardKind::Function,
        "class_definition" => CardKind::Class,
        _ => return,
    };

    let Some(name_node) = def_node.child_by_field_name("name") else {
        return;
    };
    let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
        return;
    };
    let name = name.to_string();

    // Visibility convention: leading underscore is private. `__dunder__` stays
    // public — dunders are published API, not visibility hints.
    let visibility = if name.starts_with("__") && name.ends_with("__") {
        Visibility::Public
    } else if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Public
    };

    // Body range: find the `body` field (a `block` node for fn/class).
    let (body_range, body_lines) = match def_node.child_by_field_name("body") {
        Some(body) => {
            let br = body.byte_range();
            let bl = byte_range_to_line_range(&br, line_offsets);
            (Some(br), Some(bl))
        }
        None => (None, None),
    };

    // Header range: from `def`/`class` to (but not including) the body.
    // Tree-sitter's `function_definition` covers the whole thing including
    // body, so we clamp to [node_start, body_start).
    let header_end = body_range
        .as_ref()
        .map(|b| b.start)
        .unwrap_or(def_node.end_byte());
    let header_range = def_node.start_byte()..header_end;
    let header_lines = byte_range_to_line_range(&header_range, line_offsets);

    // Full range = decorators (if any) through body end.
    let full_range = full_start..def_node.end_byte();
    let full_lines = byte_range_to_line_range(&full_range, line_offsets);

    // Classify @classmethod/@staticmethod/@property if we're a decorated_definition.
    let modifier = if kind == CardKind::Method {
        detect_method_modifier(node, source)
    } else {
        MethodModifier::None
    };

    // Preserve every decorator's dotted name for the semantic-rules layer
    // (is_abstract, is_dataclass, future M8 visual differentiation). Empty
    // when `node` isn't a decorated_definition.
    let decorators = extract_decorators(node, source);

    // Docstring detection (Python): first non-comment statement of the
    // body is a string literal. Enables the M3.4 three-state fold switch
    // on this card.
    let (docstring_range, docstring_lines) =
        match def_node.child_by_field_name("body") {
            Some(body) => detect_docstring(body, line_offsets),
            None => (None, None),
        };

    // M6.0 per-line model: build a LogicalLine per source line in
    // full_lines. Ignored by rendering today; M6/M7 consumers read from
    // these for diff states and per-line animations.
    let lines = build_logical_lines(&full_range, &full_lines, line_offsets);

    let id = CardId(*next_id);
    *next_id += 1;
    out.push(Card {
        id,
        kind,
        parent,
        depth,
        name,
        visibility,
        modifier,
        decorators,
        header_range,
        body_range: body_range.clone(),
        full_range,
        header_lines,
        body_lines,
        full_lines,
        docstring_range,
        docstring_lines,
        lines,
    });

    // Recurse into class bodies for methods + nested classes.
    if kind == CardKind::Class {
        if let Some(body) = def_node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for stmt in body.children(&mut cursor) {
                process_statement(stmt, source, line_offsets, Some(id), depth + 1, out, next_id);
            }
        }
    }
    // Nested functions inside functions are out of scope for M3.
}

/// Emit a single Snippet card covering the entire file. Used by
/// language modules whose real `extract_cards` isn't written yet, so
/// opening e.g. a `.rs` or `.md` still shows the source on the plate
/// as one long read-only card instead of a blank plate. Replace with
/// a language-specific extraction once the tagged-CardKind rework
/// (YGG-27 Part 2) lands.
pub fn whole_file_snippet(source: &str, line_offsets: &[usize]) -> Vec<Card> {
    if source.is_empty() {
        return Vec::new();
    }
    let full_range = 0..source.len();
    let full_lines = byte_range_to_line_range(&full_range, line_offsets);
    let lines = build_logical_lines(&full_range, &full_lines, line_offsets);
    vec![Card {
        id: CardId(0),
        kind: CardKind::Snippet,
        parent: None,
        depth: 0,
        name: String::new(),
        visibility: Visibility::Public,
        modifier: MethodModifier::None,
        decorators: Vec::new(),
        header_range: full_range.clone(),
        body_range: None,
        full_range: full_range.clone(),
        header_lines: full_lines.clone(),
        body_lines: None,
        full_lines,
        docstring_range: None,
        docstring_lines: None,
        lines,
    }]
}

/// Emit a `Snippet` card for a top-level orphan statement. No body / no
/// fold — snippets always render fully.
fn emit_snippet(node: Node, line_offsets: &[usize], out: &mut Vec<Card>, next_id: &mut u32) {
    let full_range = node.byte_range();
    let full_lines = byte_range_to_line_range(&full_range, line_offsets);
    let header_range = full_range.clone();
    let header_lines = full_lines.clone();

    let lines = build_logical_lines(&full_range, &full_lines, line_offsets);

    let id = CardId(*next_id);
    *next_id += 1;
    out.push(Card {
        id,
        kind: CardKind::Snippet,
        parent: None,
        depth: 0,
        // The node kind is a decent placeholder name for snippets until we
        // have something more specific (e.g. the target of `if __name__`).
        name: node.kind().to_string(),
        visibility: Visibility::Public,
        modifier: MethodModifier::None,
        decorators: Vec::new(),
        header_range,
        body_range: None,
        full_range,
        header_lines,
        body_lines: None,
        full_lines,
        docstring_range: None,
        docstring_lines: None,
        lines,
    });
}

/// Build the M6.0 per-line list for a card covering `full_lines` in the
/// source. One `LogicalLine` per source line within that range, with
/// byte range clipped to `full_range` so partial-line edges stay sane.
/// LineId seeds from the 0-based line-in-source — stable within a
/// version and trivially derivable.
fn build_logical_lines(
    full_range: &Range<usize>,
    full_lines: &Range<usize>,
    line_offsets: &[usize],
) -> Vec<LogicalLine> {
    let mut out = Vec::with_capacity(full_lines.end.saturating_sub(full_lines.start));
    for (idx, ln) in full_lines.clone().enumerate() {
        let ls = line_offsets.get(ln).copied().unwrap_or(full_range.start);
        // Use the NEXT line offset as exclusive end; fall back to
        // full_range.end for the last line.
        let le = line_offsets.get(ln + 1).copied().unwrap_or(full_range.end);
        let start = ls.max(full_range.start);
        let end = le.min(full_range.end);
        out.push(LogicalLine {
            id: LineId(ln as u32),
            byte_range: start..end,
            line_index_in_card: idx as u32,
            opacity: 1.0,
            y_offset: 0.0,
        });
    }
    out
}

/// Detect a leading docstring in the given body block. Returns the string
/// statement's byte + line ranges, or `None` if the first non-comment
/// child of the body isn't an `expression_statement` wrapping a `string`.
fn detect_docstring(
    body: Node,
    line_offsets: &[usize],
) -> (Option<Range<usize>>, Option<Range<usize>>) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "comment" => continue, // skip leading comments if any
            "expression_statement" => {
                // Docstring iff the expression statement wraps a single
                // string literal. Anything else (a function call, a name,
                // etc.) means there's no docstring here.
                let mut inner_cursor = child.walk();
                for inner in child.children(&mut inner_cursor) {
                    if inner.kind() == "string" {
                        let range = child.byte_range();
                        let lines = byte_range_to_line_range(&range, line_offsets);
                        return (Some(range), Some(lines));
                    }
                }
                return (None, None);
            }
            _ => return (None, None),
        }
    }
    (None, None)
}

/// If `node` is a `decorated_definition`, return the dotted identifier of
/// every decorator in source order, with leading `@` and trailing call-args
/// stripped. Preserves dotting: `@abc.abstractmethod` → `"abc.abstractmethod"`,
/// `@functools.wraps(f)` → `"functools.wraps"`. Empty vec for non-decorated
/// nodes.
fn extract_decorators(node: Node, source: &str) -> Vec<String> {
    if node.kind() != "decorated_definition" {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() != "decorator" {
            continue;
        }
        let Ok(text) = c.utf8_text(source.as_bytes()) else {
            continue;
        };
        let raw = text.trim_start_matches('@').trim();
        // Strip call args; keep dotted identifier intact.
        let name = raw.split('(').next().unwrap_or(raw).trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

/// If `node` is a `decorated_definition`, inspect its decorators and return
/// the first modifier we recognize. Otherwise `None`.
fn detect_method_modifier(node: Node, source: &str) -> MethodModifier {
    if node.kind() != "decorated_definition" {
        return MethodModifier::None;
    }
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() != "decorator" {
            continue;
        }
        // Decorator text: "@name" or "@name(args)". Strip the `@` and any
        // trailing call to get the base name.
        let Ok(text) = c.utf8_text(source.as_bytes()) else {
            continue;
        };
        let raw = text.trim_start_matches('@').trim();
        let name = raw.split(['(', '.']).next().unwrap_or(raw).trim();
        match name {
            "classmethod" => return MethodModifier::Classmethod,
            "staticmethod" => return MethodModifier::Staticmethod,
            "property" => return MethodModifier::Property,
            _ => continue,
        }
    }
    MethodModifier::None
}

// ---------------------------------------------------------------------------
// Layout — pure function of (cards, fold_progress, metrics)
// ---------------------------------------------------------------------------

/// One card's rectangle in scene coordinates (physical pixels). `y` is the
/// top of the card. Height splits into header (always visible) + body
/// (scaled by fold progress).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CardRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub header_h: f32,
    /// Current body height (full * fold_progress for leaf cards; sum of
    /// children's effective heights for class cards).
    pub body_h: f32,
    /// Rendering opacity 0..1. Used by the nested-fold cascade: when a class
    /// folds, descendant cards' opacity is multiplied by the class's fold
    /// progress so they fade in/out in lockstep with the parent. Leaf cards
    /// at identity state have opacity = 1.0.
    pub opacity: f32,
}

impl CardRect {
    /// Total on-screen height (header + currently-visible body).
    pub fn total_h(&self) -> f32 {
        self.header_h + self.body_h
    }
}

/// Layout inputs the renderer needs: per-card rects + total scene height.
#[derive(Debug, Clone)]
pub struct Layout {
    pub rects: HashMap<CardId, CardRect>,
    /// Height of the full laid-out scene. Used by scroll-clamping to stop the
    /// user scrolling off the end — currently still uses the line-based
    /// approximation in `AppState::clamp_scroll`; will swap to this in a
    /// follow-up cleanup.
    #[allow(dead_code)]
    pub total_height: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutMetrics {
    /// Per-line pixel height (already DPI-scaled).
    pub line_height: f32,
    /// Card's left edge.
    pub left: f32,
    /// Card's width at depth 0. Nested cards shrink by `depth * depth_indent`.
    pub width: f32,
    /// Pixels added to `left` per depth step (class nesting).
    pub depth_indent: f32,
    /// Vertical gap between consecutive top-level cards (doesn't apply inside
    /// a class, where methods sit flush).
    pub top_level_gap: f32,
    /// Vertical padding inside a card before/after its content.
    pub card_inner_pad_y: f32,
}

/// Compute the layout of all cards. `fold_progress(id)` returns 1.0 for
/// unfolded (default) and 0.0 for fully folded; anything in between is an
/// in-progress fold animation.
pub fn layout_cards(
    cards: &[Card],
    fold_progress: &HashMap<CardId, f32>,
    m: LayoutMetrics,
) -> Layout {
    let mut rects = HashMap::with_capacity(cards.len());
    let mut cursor_y = 0.0f32;

    // Pre-compute child indices per parent so the recursive walk doesn't
    // re-scan the vector. O(n).
    let mut children_of: HashMap<Option<CardId>, Vec<usize>> = HashMap::new();
    for (idx, card) in cards.iter().enumerate() {
        children_of.entry(card.parent).or_default().push(idx);
    }

    let top_level = children_of.get(&None).cloned().unwrap_or_default();
    for (i, &idx) in top_level.iter().enumerate() {
        let height =
            layout_subtree(idx, cards, &children_of, fold_progress, &m, cursor_y, &mut rects);
        cursor_y += height;
        if i + 1 < top_level.len() {
            cursor_y += m.top_level_gap;
        }
    }

    Layout { rects, total_height: cursor_y }
}

/// Recursive helper — lays out `card_idx` and its subtree starting at `y`.
/// Writes into `rects` and returns the total height consumed by the subtree.
fn layout_subtree(
    card_idx: usize,
    cards: &[Card],
    children_of: &HashMap<Option<CardId>, Vec<usize>>,
    fold_progress: &HashMap<CardId, f32>,
    m: &LayoutMetrics,
    y: f32,
    rects: &mut HashMap<CardId, CardRect>,
) -> f32 {
    let card = &cards[card_idx];
    let header_h = header_height(card, m);
    // Linear progress advances at constant rate per second (see
    // AppState::tick_animations). We apply a smoothstep here so the *visual*
    // result eases in and out — the raw progress stays monotonic (good for
    // hit-testing / deciding "am I still animating?") while body height,
    // rolling-edge position and text clipping all follow the eased curve.
    let raw = fold_progress.get(&card.id).copied().unwrap_or(1.0).clamp(0.0, 1.0);
    let progress = smoothstep(raw);

    let x = m.left + (card.depth as f32) * m.depth_indent;
    let width = (m.width - (card.depth as f32) * m.depth_indent).max(0.0);

    // Reserve the card's slot now so children can reference our y if they need
    // to; body_h is filled in after we know our children's total.
    rects.insert(
        card.id,
        CardRect { x, y, width, header_h, body_h: 0.0, opacity: 1.0 },
    );

    let body_full_h = match card.kind {
        CardKind::Class => {
            // Class body = stacked children (methods + nested classes).
            // Lay them out starting right below our header.
            let mut child_y = y + header_h;
            let mut total = 0.0;
            if let Some(kids) = children_of.get(&Some(card.id)) {
                for &k in kids {
                    let h = layout_subtree(k, cards, children_of, fold_progress, m, child_y, rects);
                    child_y += h;
                    total += h;
                }
            }
            total
        }
        CardKind::Function | CardKind::Method => leaf_body_height(card, m),
        // Snippets have no collapsible body — all their text is in the
        // "preamble" (full_lines), which `header_height` already reserves.
        CardKind::Snippet => 0.0,
    };

    // Piecewise body_h for cards that have a docstring (M3.4 three-state
    // fold). progress 0..0.5 fills in the docstring band; 0.5..1.0 fills
    // in the rest of the body. Cards without a docstring use the linear
    // mapping as before.
    let body_h = match card.docstring_lines.as_ref() {
        Some(ds) if matches!(card.kind, CardKind::Function | CardKind::Method) => {
            let docstring_h = (ds.end.saturating_sub(ds.start)) as f32 * m.line_height;
            let docstring_h = docstring_h.min(body_full_h);
            if progress <= 0.5 {
                docstring_h * (progress * 2.0)
            } else {
                docstring_h + (body_full_h - docstring_h) * ((progress - 0.5) * 2.0)
            }
        }
        _ => body_full_h * progress,
    };

    // Fix up our body_h now that we know it.
    if let Some(r) = rects.get_mut(&card.id) {
        r.body_h = body_h;
    }

    // Nested fold cascade (M3.4): when this class is partially folded
    // (progress < 1), shrink every descendant toward the class's body top.
    // Positions, heights, and opacities all multiply by the same `progress`
    // factor — so child cards ride the fold animation in lockstep with
    // the class's shrinking body, rather than hovering at their unfolded
    // positions while only the class border collapses.
    //
    // Applied per-class level. Nested classes compound: a method inside an
    // inner class inside an outer class gets scaled by inner_progress at
    // the inner level, then by outer_progress at the outer level. The math
    // is multiplicative and the visible result is coherent.
    if matches!(card.kind, CardKind::Class) && progress < 0.999 {
        let pivot_y = y + header_h;
        if let Some(kids) = children_of.get(&Some(card.id)) {
            let roots: Vec<CardId> = kids.iter().map(|&k| cards[k].id).collect();
            scale_subtree(&roots, children_of, cards, rects, pivot_y, progress);
        }
    }

    header_h + body_h
}

/// Walk the subtree rooted at each of `roots` (inclusive) and scale every
/// descendant's position, header/body height, and opacity by `scale`.
/// Positions scale relative to `pivot_y` (typically the class's body top).
///
/// Card lookup by id is linear but the overall work is bounded by the
/// number of descendants of a folding class, which is small in practice.
fn scale_subtree(
    roots: &[CardId],
    children_of: &HashMap<Option<CardId>, Vec<usize>>,
    cards: &[Card],
    rects: &mut HashMap<CardId, CardRect>,
    pivot_y: f32,
    scale: f32,
) {
    let mut stack: Vec<CardId> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if let Some(r) = rects.get_mut(&id) {
            let dy = r.y - pivot_y;
            r.y = pivot_y + dy * scale;
            r.header_h *= scale;
            r.body_h *= scale;
            r.opacity *= scale;
        }
        if let Some(child_indices) = children_of.get(&Some(id)) {
            for &ci in child_indices {
                stack.push(cards[ci].id);
            }
        }
    }
}

/// Height of a card's header area — the lines that stay visible when the
/// card is fully folded.
///
/// - **Class**: just the `class Foo:` signature line (classes render only
///   their header_range; decorators on a class aren't text inside the class
///   card, they belong to the enclosing scope).
/// - **Function/Method**: decorators *plus* the signature. A `@classmethod`
///   function card shows `@classmethod\ndef foo(cls):` when folded — both
///   the decorator context and the signature are structurally important.
/// - Inner padding is added above AND below the visible preamble so the
///   first/last line don't touch the frame.
fn header_height(card: &Card, m: &LayoutMetrics) -> f32 {
    let preamble_lines: usize = match (&card.kind, &card.body_lines) {
        (CardKind::Class, _) => (card.header_lines.end - card.header_lines.start).max(1),
        // For functions/methods, preamble = everything in full_range before
        // the body starts: decorator lines + signature lines.
        (_, Some(body_lines)) => {
            body_lines.start.saturating_sub(card.full_lines.start).max(1)
        }
        (_, None) => (card.full_lines.end - card.full_lines.start).max(1),
    };
    (preamble_lines as f32) * m.line_height + m.card_inner_pad_y * 2.0
}

/// Cubic smoothstep — standard 3t²−2t³. Zero derivative at both endpoints,
/// so the fold animation has no abrupt start or stop.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Full (unfolded) body height for a leaf card (function or method).
///
/// Just the body text lines — no extra padding here. The header already
/// reserves `card_inner_pad_y` above the preamble AND a matching pad below
/// it (via `* 2.0` in `header_height`), so the bottom of that header-pad is
/// exactly where body text starts. `body_h` only adds the collapsible body
/// lines themselves, which matches what the text cursor actually advances
/// through.
fn leaf_body_height(card: &Card, m: &LayoutMetrics) -> f32 {
    let lines = card
        .body_lines
        .as_ref()
        .map(|r| (r.end.saturating_sub(r.start)) as f32)
        .unwrap_or(0.0);
    lines * m.line_height
}

// ---------------------------------------------------------------------------
// Byte-range → line-range utility
// ---------------------------------------------------------------------------

/// Given a byte range and the `line_offsets` vector (which has a sentinel at
/// `contents.len()`), compute the half-open line range this byte range
/// touches. Line indices are 0-based.
fn byte_range_to_line_range(br: &Range<usize>, line_offsets: &[usize]) -> Range<usize> {
    // `partition_point` is stable and returns the first index `i` where
    // `line_offsets[i] > br.start` → the line `i-1` contains `br.start`.
    let start_line = line_offsets
        .partition_point(|&off| off <= br.start)
        .saturating_sub(1);
    // For the end (exclusive), we want the line that contains `br.end - 1`,
    // plus one (to make the range exclusive).
    let end_line = if br.is_empty() {
        start_line
    } else {
        line_offsets
            .partition_point(|&off| off < br.end)
            .saturating_sub(0) // keep as-is; partition_point already gives exclusive
    };
    start_line..end_line.max(start_line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use tree_sitter::Parser;

    fn line_offsets(s: &str) -> Vec<usize> {
        let mut offs = vec![0];
        for (i, b) in s.bytes().enumerate() {
            if b == b'\n' {
                offs.push(i + 1);
            }
        }
        offs.push(s.len());
        offs
    }

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn extract(src: &str) -> Vec<Card> {
        let tree = parse(src);
        let offs = line_offsets(src);
        extract_cards(&tree, src, &offs)
    }

    // Helper matchers: we assert a small tuple of "what about this card are we
    // testing?" so the noise from the rest of the Card fields stays out.
    fn sig(c: &Card) -> (CardKind, &str, Visibility, MethodModifier, usize) {
        (c.kind, c.name.as_str(), c.visibility, c.modifier, c.depth)
    }

    #[rstest]
    // A module with one plain function — one card, Function, Public.
    #[case::simple_function(
        "def greet():\n    pass\n",
        &[(CardKind::Function, "greet", Visibility::Public, MethodModifier::None, 0usize)],
    )]
    // Private function — name starts with single underscore.
    #[case::private_function(
        "def _helper():\n    pass\n",
        &[(CardKind::Function, "_helper", Visibility::Private, MethodModifier::None, 0)],
    )]
    // Dunder is still "public" (it's published API).
    #[case::dunder_is_public(
        "def __init__(self):\n    pass\n",
        &[(CardKind::Function, "__init__", Visibility::Public, MethodModifier::None, 0)],
    )]
    // Class with two methods — one class card + two method cards at depth 1.
    #[case::class_with_methods(
        "class Widget:\n    def open(self):\n        pass\n    def close(self):\n        pass\n",
        &[
            (CardKind::Class, "Widget", Visibility::Public, MethodModifier::None, 0),
            (CardKind::Method, "open", Visibility::Public, MethodModifier::None, 1),
            (CardKind::Method, "close", Visibility::Public, MethodModifier::None, 1),
        ],
    )]
    // Classmethod detection — decorated_definition with @classmethod.
    #[case::classmethod(
        "class W:\n    @classmethod\n    def make(cls):\n        return cls()\n",
        &[
            (CardKind::Class, "W", Visibility::Public, MethodModifier::None, 0),
            (CardKind::Method, "make", Visibility::Public, MethodModifier::Classmethod, 1),
        ],
    )]
    // Staticmethod + a plain instance method in the same class.
    #[case::staticmethod_and_instance(
        "class W:\n    @staticmethod\n    def util():\n        pass\n    def run(self):\n        pass\n",
        &[
            (CardKind::Class, "W", Visibility::Public, MethodModifier::None, 0),
            (CardKind::Method, "util", Visibility::Public, MethodModifier::Staticmethod, 1),
            (CardKind::Method, "run",  Visibility::Public, MethodModifier::None, 1),
        ],
    )]
    // @property.
    #[case::property(
        "class W:\n    @property\n    def area(self):\n        return 0\n",
        &[
            (CardKind::Class, "W", Visibility::Public, MethodModifier::None, 0),
            (CardKind::Method, "area", Visibility::Public, MethodModifier::Property, 1),
        ],
    )]
    // Decorator stack — @classmethod on top of other decorators still registers
    // the first-matching modifier. @functools.wraps(f) wraps f but doesn't
    // alter kind. Our detector reports the first recognized one.
    #[case::decorator_stack(
        "class W:\n    @someother\n    @classmethod\n    def f(cls):\n        pass\n",
        &[
            (CardKind::Class, "W", Visibility::Public, MethodModifier::None, 0),
            (CardKind::Method, "f", Visibility::Public, MethodModifier::Classmethod, 1),
        ],
    )]
    // Nested class — Inner sits at depth 1, its methods at depth 2.
    #[case::nested_class(
        "class Outer:\n    class Inner:\n        def m(self):\n            pass\n    def outer_m(self):\n        pass\n",
        &[
            (CardKind::Class,  "Outer",   Visibility::Public, MethodModifier::None, 0),
            (CardKind::Class,  "Inner",   Visibility::Public, MethodModifier::None, 1),
            (CardKind::Method, "m",       Visibility::Public, MethodModifier::None, 2),
            (CardKind::Method, "outer_m", Visibility::Public, MethodModifier::None, 1),
        ],
    )]
    // Module-level orphan code becomes Snippet cards so nothing is invisible.
    // Per-node: one card per top-level statement (keep distinction between
    // imports vs constants vs functions).
    #[case::snippets_for_orphan_top_level(
        "import os\nCONST = 1\ndef f():\n    pass\n",
        &[
            (CardKind::Snippet,  "import_statement",     Visibility::Public, MethodModifier::None, 0),
            (CardKind::Snippet,  "expression_statement", Visibility::Public, MethodModifier::None, 0),
            (CardKind::Function, "f",                    Visibility::Public, MethodModifier::None, 0),
        ],
    )]
    fn extraction(
        #[case] src: &str,
        #[case] expected: &[(CardKind, &str, Visibility, MethodModifier, usize)],
    ) {
        let cards = extract(src);
        let got: Vec<_> = cards.iter().map(sig).collect();
        assert_eq!(got, expected.to_vec());
    }

    /// `decorators` preserves every decorator on each card in source order,
    /// with `@` and call args stripped but dotted identifiers intact. The
    /// cases cover the variations the semantic-rules layer needs to recognize.
    #[rstest]
    // Plain function — no decorators.
    #[case::no_decorator(
        "def f():\n    pass\n",
        0, &[] as &[&str]
    )]
    // @classmethod preserved verbatim (same decorator that drives MethodModifier).
    #[case::classmethod_preserved(
        "class W:\n    @classmethod\n    def make(cls):\n        pass\n",
        1, &["classmethod"]
    )]
    // @dataclass on a class.
    #[case::dataclass_on_class(
        "@dataclass\nclass A:\n    x: int = 0\n",
        0, &["dataclass"]
    )]
    // @abstractmethod on a method.
    #[case::abstractmethod_on_method(
        "class A:\n    @abstractmethod\n    def m(self):\n        pass\n",
        1, &["abstractmethod"]
    )]
    // Dotted decorator — full dotted identifier retained.
    #[case::dotted_decorator(
        "class A:\n    @abc.abstractmethod\n    def m(self):\n        pass\n",
        1, &["abc.abstractmethod"]
    )]
    // Call-args stripped but dotted name kept.
    #[case::call_args_stripped(
        "@functools.wraps(f)\ndef g():\n    pass\n",
        0, &["functools.wraps"]
    )]
    // Stacked decorators — source order, all preserved.
    #[case::stacked(
        "class A:\n    @someother\n    @classmethod\n    def f(cls):\n        pass\n",
        1, &["someother", "classmethod"]
    )]
    // Custom decorator passes through unchanged.
    #[case::custom(
        "@my_decorator\ndef f():\n    pass\n",
        0, &["my_decorator"]
    )]
    fn decorators_preserved(
        #[case] src: &str,
        #[case] card_idx: usize,
        #[case] expected: &[&str],
    ) {
        let cards = extract(src);
        let got: Vec<&str> = cards[card_idx].decorators.iter().map(String::as_str).collect();
        assert_eq!(got, expected);
    }

    /// `is_abstract()` and `is_dataclass()` derive from the decorator list.
    /// Last-component match handles both bare and dotted forms; `is_dataclass`
    /// is gated on `CardKind::Class` so it never misfires on methods.
    #[rstest]
    #[case::plain_function(
        "def f():\n    pass\n", 0, false, false
    )]
    #[case::abstractmethod_bare(
        "class A:\n    @abstractmethod\n    def m(self):\n        pass\n",
        1, true, false
    )]
    #[case::abstractmethod_dotted(
        "class A:\n    @abc.abstractmethod\n    def m(self):\n        pass\n",
        1, true, false
    )]
    #[case::abstractclassmethod(
        "class A:\n    @abstractclassmethod\n    def m(cls):\n        pass\n",
        1, true, false
    )]
    #[case::dataclass_bare(
        "@dataclass\nclass A:\n    x: int = 0\n",
        0, false, true
    )]
    #[case::dataclass_dotted(
        "@dataclasses.dataclass\nclass A:\n    x: int = 0\n",
        0, false, true
    )]
    // is_dataclass must be false on methods even if somehow decorated —
    // gating on CardKind::Class.
    #[case::dataclass_on_method_ignored(
        "class A:\n    @dataclass\n    def m(self):\n        pass\n",
        1, false, false
    )]
    fn abstract_and_dataclass_flags(
        #[case] src: &str,
        #[case] card_idx: usize,
        #[case] expect_abstract: bool,
        #[case] expect_dataclass: bool,
    ) {
        let cards = extract(src);
        let c = &cards[card_idx];
        assert_eq!(c.is_abstract(), expect_abstract, "is_abstract()");
        assert_eq!(c.is_dataclass(), expect_dataclass, "is_dataclass()");
    }

    /// Docstring detection (M3.4): Python functions/methods whose body's
    /// first statement is a string literal get a populated
    /// `docstring_range`. Docstring-less cards get `None`.
    #[rstest]
    #[case::plain_function(
        "def f():\n    pass\n",
        0, false
    )]
    #[case::function_with_docstring(
        "def f():\n    \"short\"\n    pass\n",
        0, true
    )]
    #[case::method_with_docstring(
        "class A:\n    def m(self):\n        \"doc\"\n        return 1\n",
        1, true
    )]
    #[case::method_no_docstring(
        "class A:\n    def m(self):\n        return 1\n",
        1, false
    )]
    #[case::first_statement_not_a_string(
        "def f():\n    x = 1\n    return x\n",
        0, false
    )]
    #[case::triple_quoted(
        "def f():\n    \"\"\"triple\"\"\"\n    pass\n",
        0, true
    )]
    fn docstring_detection(
        #[case] src: &str,
        #[case] card_idx: usize,
        #[case] expect_docstring: bool,
    ) {
        let cards = extract(src);
        let c = &cards[card_idx];
        assert_eq!(c.docstring_range.is_some(), expect_docstring, "docstring_range");
        assert_eq!(c.docstring_lines.is_some(), expect_docstring, "docstring_lines");
    }

    /// M6.0 per-line model: a card's `lines` has one `LogicalLine` per
    /// source line in its `full_lines` range, in order, each with a
    /// populated byte range and default opacity/offset. Stable LineIds
    /// for the same source line across re-extractions.
    #[test]
    fn logical_lines_cover_full_range() {
        let src = "def f():\n    \"d\"\n    pass\n";
        let cards = extract(src);
        let c = &cards[0];
        // 3 source lines in this function (def / "d" / pass).
        assert_eq!(c.lines.len(), 3);
        for (i, line) in c.lines.iter().enumerate() {
            assert_eq!(line.line_index_in_card, i as u32);
            assert!(line.byte_range.start >= c.full_range.start);
            assert!(line.byte_range.end <= c.full_range.end);
            assert_eq!(line.opacity, 1.0);
            assert_eq!(line.y_offset, 0.0);
        }
        // LineId for the first line is seeded from the absolute line-in-
        // source: line 0 → LineId(0).
        assert_eq!(c.lines[0].id, LineId(0));
    }

    /// Snippets also get per-line data so diff states/animations can
    /// attach to top-level imports / constants later.
    #[test]
    fn logical_lines_populated_for_snippets() {
        let src = "import os\nCONST = 1\n";
        let cards = extract(src);
        assert_eq!(cards[0].kind, CardKind::Snippet);
        assert_eq!(cards[0].lines.len(), 1);
    }

    /// Parent/child links: methods of a class have that class as parent.
    #[test]
    fn parent_links_are_set() {
        let src = "class A:\n    def m(self):\n        pass\n";
        let cards = extract(src);
        assert_eq!(cards.len(), 2);
        let class_id = cards[0].id;
        assert_eq!(cards[0].parent, None);
        assert_eq!(cards[1].parent, Some(class_id));
    }

    /// Byte ranges cover what we'd expect: full_range >= header_range,
    /// body_range (when present) is disjoint from header_range.
    #[test]
    fn ranges_are_consistent() {
        let src = "def f(x):\n    return x\n";
        let cards = extract(src);
        let c = &cards[0];
        assert!(c.full_range.start <= c.header_range.start);
        assert!(c.header_range.end <= c.full_range.end);
        let body = c.body_range.as_ref().unwrap();
        assert!(body.start >= c.header_range.end);
        assert!(body.end <= c.full_range.end);
    }

    /// Decorators expand the full_range upward: `@dec\ndef f(): ...` —
    /// `full_range.start` points at the `@`, not the `def`.
    #[test]
    fn decorator_extends_full_range_upward() {
        let src = "@classmethod\ndef f(cls):\n    pass\n";
        // Even at top level (not a method), decorated_definition processing
        // should still mark full_range starting at the `@`.
        let cards = extract(src);
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(&src[c.full_range.clone()].chars().next(), &Some('@'));
        assert_eq!(&src[c.header_range.clone()].chars().next(), &Some('d'));
    }

    // ---- Layout tests ----

    fn metrics() -> LayoutMetrics {
        LayoutMetrics {
            line_height: 20.0,
            left: 100.0,
            width: 600.0,
            depth_indent: 24.0,
            top_level_gap: 10.0,
            card_inner_pad_y: 4.0,
        }
    }

    /// Convenience: for each test case we just want the headers' y-values
    /// keyed by name — that's what a human would eyeball.
    fn ys_by_name(cards: &[Card], layout: &Layout) -> Vec<(String, f32)> {
        cards
            .iter()
            .map(|c| (c.name.clone(), layout.rects[&c.id].y))
            .collect()
    }

    /// Two top-level functions, no folds: the second starts at the first's
    /// total height plus the top-level gap.
    #[test]
    fn layout_stacks_top_level_cards() {
        let src = "def a():\n    x = 1\n    y = 2\ndef b():\n    z = 3\n";
        let cards = extract(src);
        let m = metrics();
        let l = layout_cards(&cards, &HashMap::new(), m);
        let ys = ys_by_name(&cards, &l);
        // `a` at y=0. Its total = header (20+8) + 2 body lines (40+4) = 72.
        // `b` follows at 72 + 10 (gap) = 82.
        assert_eq!(ys[0].0, "a");
        assert!((ys[0].1 - 0.0).abs() < 0.01);
        assert_eq!(ys[1].0, "b");
        assert!(ys[1].1 > 70.0 && ys[1].1 < 95.0, "b y = {}", ys[1].1);
    }

    /// Folding a top-level function shrinks everything below it by the folded
    /// body's height.
    #[test]
    fn folding_shifts_subsequent_cards_up() {
        let src = "def a():\n    x = 1\n    y = 2\ndef b():\n    z = 3\n";
        let cards = extract(src);
        let m = metrics();

        let unfolded = layout_cards(&cards, &HashMap::new(), m);
        let mut folds = HashMap::new();
        folds.insert(cards[0].id, 0.0); // fold `a` completely
        let folded = layout_cards(&cards, &folds, m);

        let delta =
            unfolded.rects[&cards[1].id].y - folded.rects[&cards[1].id].y;
        // The height of a's body (just the 2 body lines — `body_h` no longer
        // double-counts inner padding; the header's `pad*2` provides the
        // bottom gutter) should be exactly what b moved up.
        let expected_body_h = 2.0 * m.line_height;
        assert!(
            (delta - expected_body_h).abs() < 0.01,
            "delta={} expected_body_h={}",
            delta,
            expected_body_h,
        );
    }

    /// A class with methods: children cards sit below the class header, at a
    /// larger x (nested indent).
    #[test]
    fn class_contains_method_cards() {
        let src = "class W:\n    def a(self):\n        pass\n    def b(self):\n        pass\n";
        let cards = extract(src);
        let m = metrics();
        let l = layout_cards(&cards, &HashMap::new(), m);

        let class_rect = l.rects[&cards[0].id];
        let method_a = l.rects[&cards[1].id];
        let method_b = l.rects[&cards[2].id];

        // Class is at depth 0, methods at depth 1 → indented by depth_indent.
        assert!((method_a.x - (class_rect.x + m.depth_indent)).abs() < 0.01);
        assert!((method_b.x - (class_rect.x + m.depth_indent)).abs() < 0.01);

        // Methods sit below the class header.
        assert!(method_a.y >= class_rect.y + class_rect.header_h);
        // Second method below first.
        assert!(method_b.y > method_a.y);

        // Class total_height spans through its methods.
        assert!(class_rect.total_h() >= method_b.y + method_b.total_h() - class_rect.y);
    }

    /// Folding a *class* collapses all its methods (their rects remain in the
    /// map but the class's body_h goes to 0).
    #[test]
    fn folding_class_collapses_body() {
        let src = "class W:\n    def a(self):\n        pass\n    def b(self):\n        pass\n";
        let cards = extract(src);
        let m = metrics();
        let mut folds = HashMap::new();
        folds.insert(cards[0].id, 0.0);
        let l = layout_cards(&cards, &folds, m);
        let class_rect = l.rects[&cards[0].id];
        assert!((class_rect.body_h - 0.0).abs() < 0.01);
        // Total scene height is just the class header.
        assert!((l.total_height - class_rect.header_h).abs() < 0.01);
    }
}
