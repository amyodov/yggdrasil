//! Blind primitive — the file-tree visualization. Cardboard slats
//! threaded onto brass ropes, hanging in the void on the left of the
//! canvas. Each slat carries its filename. The blind lives in Zone 1
//! (the void), drawn directly into the swap chain — not on a
//! substrate, because the tree is a cluster of discrete hanging
//! objects, not a continuous lit surface.
//!
//! ## Two slat designs — `SlatMode`
//!
//! Phase B ships two *different* designs for "how a slat presents an
//! item", picked at launch by `--debug-slat-mode`:
//!
//! - **Closed** (`ink on plaque`): the slat is a tall cardboard plaque;
//!   the filename is inked directly onto its face in dark ink. Reads
//!   like a library card.
//! - **Open** (`shelf with standing text`): the slat is a thin
//!   horizontal shelf; the filename stands ABOVE it in pale warm
//!   type, as if the name is a free-standing object resting on the
//!   shelf. Reads like items on a display shelf.
//!
//! These are alternate designs to compare visually before we commit
//! to one. They differ in geometry, text color, text position, and
//! (when animation arrives) how the slat and text move.
//! `Alternating` uses one design on odd rows and the other on even
//! rows so both appear together.
//!
//! ## Multiple ropes (standard file-tree shape)
//!
//! Each depth in the tree gets its own rope, spanning only the
//! siblings of one parent folder — exactly how a standard file-tree
//! view draws `│` connectors. The root rope spans all depth-0
//! slats; each expanded folder contributes a NEW rope at
//! `depth + 1`, spanning from its first child to its last descendant
//! at that depth before the next sibling takes over.
//!
//! Each slat has a hole at its own rope's x; the rope is visible only
//! through that hole (rendered as a small brass disc on top of the
//! slat), otherwise hidden behind the slat body.

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

use crate::cli::SlatMode;
use crate::filetree::{flatten, EntryKind, SlatEntry, TreeState};

// ---------------------------------------------------------------------
// Layout / palette constants. Logical points where marked _PT.
// ---------------------------------------------------------------------

/// Outer padding of the blind region from the left pane's edges.
pub const BLIND_MARGIN_PT: f32 = 14.0;
/// Vertical slot per slat (invariant of design or depth). Slats mount
/// at regular y intervals regardless of Closed/Open design.
pub const SLAT_HEIGHT_PT: f32 = 26.0;
/// Vertical gap between adjacent slot rows.
pub const SLAT_GAP_PT: f32 = 4.0;
/// Horizontal indent per depth level (= spacing between adjacent ropes).
pub const SLAT_INDENT_PT: f32 = 16.0;
/// How far each slat extends to the LEFT past its rope's x-center.
/// Ensures the rope threads through a hole INSIDE the slat body,
/// not at the slat's edge.
pub const SLAT_LEFT_OVERHANG_PT: f32 = 7.0;
/// Width of the vertical-squircle rope-hole (Closed mode). Narrow,
/// wider than the rope itself so there's visible void around the
/// rope inside the hole.
pub const HOLE_WIDTH_PT: f32 = 5.0;
/// Hole height as a fraction of the slat height. Centered
/// vertically; leaves ~20% of slat material above the hole and ~20%
/// below. The strips above/below give the rope z-order its
/// foothold: rope-above-hole is drawn IN FRONT of the slat (visible
/// over the slat's top strip), rope-below-hole sits BEHIND the slat
/// (hidden by the bottom strip).
pub const HOLE_HEIGHT_FRACTION: f32 = 0.6;
/// Vertical position of the hole's CENTER as a fraction of the
/// slat's height. 0.5 = centered.
pub const HOLE_CENTER_Y_FRACTION: f32 = 0.5;
/// Extra slat material to the right of the filename text. Picked to
/// roughly match the space on the LEFT of the pill hole (the
/// material between the slat's left edge and the hole's left edge).
const SLAT_RIGHT_MARGIN_PT: f32 = 5.0;
/// Left-edge x of the root rope, relative to the pane's left edge.
const ROOT_ROPE_OFFSET_PT: f32 = BLIND_MARGIN_PT + SLAT_LEFT_OVERHANG_PT;

// ---- Closed design (ink-on-plaque) ----------------------------------

/// Plaque (Closed) slat body — warm off-white paper, near-opaque.
const CLOSED_FILE_BG: [f32; 4] = [0.84, 0.82, 0.78, 0.97];
/// Folder plaques sit slightly cooler/darker than files so the eye
/// clusters them without a chrome icon.
const CLOSED_FOLDER_BG: [f32; 4] = [0.76, 0.75, 0.72, 0.97];
/// Plaque corners — small radius, reads as cardstock.
const CLOSED_CORNER_RADIUS_PT: f32 = 3.0;
/// Inner padding the plaque inherits inside its slot.
const CLOSED_VERTICAL_PAD_PT: f32 = 1.5;
/// Ink color for text inked on a plaque.
pub const CLOSED_INK_RGB: (u8, u8, u8) = (48, 44, 40);
/// Ink-text inset from the right edge of the rope-hole.
const CLOSED_TEXT_GAP_PT: f32 = 8.0;

// ---- Open design (shelf with standing text) -------------------------

/// Shelf (Open) strip — a thinner board tone. Reads as wood/cardboard
/// at a distance; the text stands on top of it, not inside it.
const OPEN_SHELF_BG: [f32; 4] = [0.60, 0.54, 0.46, 0.92];
/// Folder shelves sit slightly darker. The distinction is subtle;
/// the visual identity of a folder in Open mode comes more from the
/// trailing slash in the filename.
const OPEN_FOLDER_SHELF_BG: [f32; 4] = [0.52, 0.47, 0.40, 0.92];
/// Shelf thickness in logical points.
const OPEN_SHELF_THICKNESS_PT: f32 = 4.0;
/// Shelf corner radius — slightly softened.
const OPEN_CORNER_RADIUS_PT: f32 = 1.5;
/// Standing-text color for Open mode — pale warm ivory, readable on
/// the dark void behind the shelf.
pub const OPEN_STANDING_RGB: (u8, u8, u8) = (238, 230, 216);
/// Standing-text horizontal inset measured from the rope's x.
const OPEN_TEXT_GAP_PT: f32 = 6.0;

// ---- Slat halo (both designs) ---------------------------------------

// ---- Rope -----------------------------------------------------------

/// Brass cable — slats mount on this. Same brass family as the class
/// armature foil so the void's metal vocabulary stays consistent.
const ROPE_COLOR: [f32; 4] = [0.55, 0.40, 0.20, 1.0];
const ROPE_GLOW: [f32; 4] = [0.62, 0.46, 0.22, 0.24];
pub const ROPE_WIDTH_PT: f32 = 2.0;
const ROPE_GLOW_RADIUS_PT: f32 = 4.0;
/// Tiny overshoot above a rope's first slat / below its last, so the
/// rope reads as threading through the slats rather than being tied
/// off flush with them.
const ROPE_OVERSHOOT_PT: f32 = 3.0;

// ---------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------

/// Which design is drawn for a particular slat. In `SlatMode::Open` /
/// `Closed` all slats share one design; in `Alternating`, even rows
/// use one and odd rows the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlatDesign {
    /// Tall plaque with filename inked on the face.
    Closed,
    /// Thin shelf with filename standing on top.
    Open,
}

#[derive(Debug, Clone)]
pub struct LaidSlat {
    pub entry: SlatEntry,
    pub design: SlatDesign,
    /// Slat body rectangle (x, y, width, height) in window-space pixels.
    pub slat_x: f32,
    pub slat_y: f32,
    pub slat_width: f32,
    pub slat_height: f32,
    /// Slot top-y and slot-height: the full-height row the slat mounts
    /// in, invariant of design. Used by input hit-testing so the
    /// clickable region stays stable even when the visible slat is
    /// thin (Open / shelf).
    pub slot_y: f32,
    pub slot_height: f32,
    /// X position (window-space) of the rope threaded through this
    /// slat. Same as the slat's rope-group column. Used by the
    /// renderer when drawing the hole (if any).
    pub rope_x: f32,
    /// Optional rope-hole. `Some(_)` for Closed (plaque with a cut
    /// hole showing the rope in the upper portion); `None` for Open
    /// (shelf — the rope is just hidden behind the thin shelf, which
    /// reads as "rope threads through" because it's visible above
    /// and below).
    pub hole: Option<Hole>,
    pub bg: [f32; 4],
    pub corner: f32,
    /// Left x (window-space px) of the filename text. Vertical
    /// position is derived by the renderer from `design`, `slot_y`,
    /// `slat_y`, `slat_height`, and the current line height —
    /// line_height lives in AppState at render time, not in layout().
    pub text_left: f32,
    /// Text color — dark ink in Closed, pale ivory in Open.
    pub text_rgb: (u8, u8, u8),
}

/// Rope-hole cut into a slat. Narrow vertical squircle in window-space
/// pixels. Renderer draws a shadow ring + a rope-colored fill to sell
/// the hole as "the rope visible through a cut in the slat material".
#[derive(Debug, Clone, Copy)]
pub struct Hole {
    pub center_x: f32,
    pub center_y: f32,
    pub width: f32,
    pub height: f32,
}

impl LaidSlat {
    /// True if `(x, y)` in window-space falls inside this slat's slot
    /// — the full-height row the slat mounts in.
    pub fn slot_contains(&self, x: f32, y: f32) -> bool {
        x >= self.slat_x
            && x < self.slat_x + self.slat_width
            && y >= self.slot_y
            && y < self.slot_y + self.slot_height
    }
}

/// One rope segment — a continuous brass cable running vertically at a
/// fixed x from `y_top` to `y_bottom`, threaded through every slat
/// whose depth matches this rope's column.
#[derive(Debug, Clone)]
pub struct RopeSegment {
    pub x: f32,
    pub y_top: f32,
    pub y_bottom: f32,
    pub width: f32,
    pub color: [f32; 4],
    pub glow_color: [f32; 4],
    pub glow_radius: f32,
}

/// One frame's blind layout.
#[derive(Debug, Clone)]
pub struct BlindLayout {
    pub slats: Vec<LaidSlat>,
    pub ropes: Vec<RopeSegment>,
}

// ---------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------

/// Compute the blind's layout for the current frame.
///
/// `filename_widths` maps each entry's path to the measured pixel
/// width of its filename text (glyphon `LayoutRun::line_w`). Slats
/// are content-sized: right edge = text-left + filename_width +
/// right margin. Missing entries fall back to a generous default so
/// layout still works before buffers are measured in the first frame.
pub fn layout(
    tree: &TreeState,
    pane_left_px: f32,
    _pane_width_px: f32,
    _pane_height_px: f32,
    scale_factor: f32,
    slat_mode: SlatMode,
    filename_widths: &std::collections::HashMap<std::path::PathBuf, f32>,
) -> BlindLayout {
    let sf = scale_factor;
    let slot_h = SLAT_HEIGHT_PT * sf;
    let gap = SLAT_GAP_PT * sf;
    let indent = SLAT_INDENT_PT * sf;
    let left_overhang = SLAT_LEFT_OVERHANG_PT * sf;
    let margin = BLIND_MARGIN_PT * sf;
    let rope_w = ROPE_WIDTH_PT * sf;
    let rope_overshoot = ROPE_OVERSHOOT_PT * sf;

    let entries = flatten(tree);
    let group_ids = assign_rope_groups(&entries);

    // X-position of a rope at depth `d` (window-space px).
    let rope_x_at_depth = |d: usize| -> f32 {
        pane_left_px + (ROOT_ROPE_OFFSET_PT * sf) + (d as f32) * indent
    };

    let blind_top = margin;

    let mut slats = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let design = design_for_index(slat_mode, index);
        let rope_x = rope_x_at_depth(entry.depth);

        let slot_y = blind_top - tree.scroll_y + index as f32 * (slot_h + gap);

        let text_w = filename_widths
            .get(&entry.path)
            .copied()
            .unwrap_or(120.0 * sf);
        let right_margin = SLAT_RIGHT_MARGIN_PT * sf;
        let geom = slat_geometry(
            design,
            entry.kind,
            rope_x,
            left_overhang,
            slot_y,
            slot_h,
            sf,
            text_w,
            right_margin,
        );

        // Hole is present only for Closed. Open's shelf is thin enough
        // that the rope "threads" by simply being hidden behind the
        // shelf and visible above + below — no hole needed.
        let hole = match design {
            SlatDesign::Closed => Some(Hole {
                center_x: rope_x,
                center_y: geom.slat_y
                    + geom.slat_h * HOLE_CENTER_Y_FRACTION,
                width: HOLE_WIDTH_PT * sf,
                height: geom.slat_h * HOLE_HEIGHT_FRACTION,
            }),
            SlatDesign::Open => None,
        };

        slats.push(LaidSlat {
            entry: entry.clone(),
            design,
            slat_x: geom.slat_x,
            slat_y: geom.slat_y,
            slat_width: geom.slat_w,
            slat_height: geom.slat_h,
            slot_y,
            slot_height: slot_h,
            rope_x,
            hole,
            bg: geom.bg,
            corner: geom.corner,
            text_left: geom.text_left,
            text_rgb: geom.text_rgb,
        });
    }

    let ropes = compute_rope_segments(&entries, &group_ids, &slats, rope_w, rope_overshoot);

    BlindLayout { slats, ropes }
}

/// Geometry of one slat — output of `slat_geometry`. `slat_w` is a
/// wide sentinel; the renderer clamps to the blind's right edge.
struct SlatGeom {
    slat_x: f32,
    slat_y: f32,
    slat_w: f32,
    slat_h: f32,
    bg: [f32; 4],
    corner: f32,
    text_left: f32,
    text_rgb: (u8, u8, u8),
}

#[allow(clippy::too_many_arguments)]
fn slat_geometry(
    design: SlatDesign,
    kind: EntryKind,
    rope_x: f32,
    left_overhang: f32,
    slot_y: f32,
    slot_h: f32,
    sf: f32,
    text_w: f32,
    right_margin: f32,
) -> SlatGeom {
    let slat_x = rope_x - left_overhang;
    match design {
        SlatDesign::Closed => {
            let pad = CLOSED_VERTICAL_PAD_PT * sf;
            let slat_h = (slot_h - 2.0 * pad).max(2.0);
            let bg = match kind {
                EntryKind::Folder => CLOSED_FOLDER_BG,
                EntryKind::File => CLOSED_FILE_BG,
            };
            let text_left = rope_x + HOLE_WIDTH_PT * 0.5 * sf + CLOSED_TEXT_GAP_PT * sf;
            let slat_w = (text_left - slat_x) + text_w + right_margin;
            SlatGeom {
                slat_x,
                slat_y: slot_y + pad,
                slat_w,
                slat_h,
                bg,
                corner: CLOSED_CORNER_RADIUS_PT * sf,
                text_left,
                text_rgb: CLOSED_INK_RGB,
            }
        }
        SlatDesign::Open => {
            let shelf_h = OPEN_SHELF_THICKNESS_PT * sf;
            let bg = match kind {
                EntryKind::Folder => OPEN_FOLDER_SHELF_BG,
                EntryKind::File => OPEN_SHELF_BG,
            };
            let text_left = rope_x + ROPE_WIDTH_PT * 0.5 * sf + OPEN_TEXT_GAP_PT * sf;
            let slat_w = (text_left - slat_x) + text_w + right_margin;
            SlatGeom {
                slat_x,
                slat_y: slot_y + slot_h - shelf_h,
                slat_w,
                slat_h: shelf_h,
                bg,
                corner: OPEN_CORNER_RADIUS_PT * sf,
                text_left,
                text_rgb: OPEN_STANDING_RGB,
            }
        }
    }
}

fn design_for_index(mode: SlatMode, index: usize) -> SlatDesign {
    match mode {
        SlatMode::Closed => SlatDesign::Closed,
        SlatMode::Open => SlatDesign::Open,
        SlatMode::Alternating => {
            if index % 2 == 0 {
                SlatDesign::Closed
            } else {
                SlatDesign::Open
            }
        }
    }
}

/// Walk the flattened entries and assign each one a rope-group id.
/// Sibling entries (same depth, same parent subtree) share an id;
/// each time depth returns to a previously-visited level, a NEW group
/// starts there (siblings of a later parent don't share the earlier
/// parent's rope). This matches how standard file-tree views draw
/// vertical connectors.
fn assign_rope_groups(entries: &[SlatEntry]) -> Vec<usize> {
    let mut group_ids = Vec::with_capacity(entries.len());
    let mut next_group = 0usize;
    // Stack of (depth, group_id). `last()` is the current group at the
    // greatest open depth. Popping removes groups whose subtree has
    // ended.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for entry in entries {
        while stack.last().is_some_and(|(d, _)| *d > entry.depth) {
            stack.pop();
        }
        if stack.last().is_none_or(|(d, _)| *d != entry.depth) {
            stack.push((entry.depth, next_group));
            next_group += 1;
        }
        group_ids.push(stack.last().unwrap().1);
    }
    group_ids
}

/// Collapse `(entries, group_ids, slats)` into one `RopeSegment` per
/// rope group, with the right x-column and y-span.
fn compute_rope_segments(
    entries: &[SlatEntry],
    group_ids: &[usize],
    slats: &[LaidSlat],
    rope_width: f32,
    overshoot: f32,
) -> Vec<RopeSegment> {
    use std::collections::HashMap;
    let mut ranges: HashMap<usize, (f32, f32, f32)> = HashMap::new();
    for (i, gid) in group_ids.iter().enumerate() {
        let slat = &slats[i];
        let top = slat.slot_y;
        let bottom = slat.slot_y + slat.slot_height;
        let x = slat.rope_x;
        let e = ranges.entry(*gid).or_insert((x, top, bottom));
        e.0 = x;
        e.1 = e.1.min(top);
        e.2 = e.2.max(bottom);
    }
    let _ = entries; // entries parameter kept for symmetry with the
                     // ropes' potential future refinement (e.g. tying
                     // off at parent folder positions).
    let mut out: Vec<RopeSegment> = ranges
        .into_iter()
        .map(|(_, (x, top, bottom))| RopeSegment {
            x: x - rope_width * 0.5,
            y_top: top - overshoot,
            y_bottom: bottom + overshoot,
            width: rope_width,
            color: ROPE_COLOR,
            glow_color: ROPE_GLOW,
            glow_radius: ROPE_GLOW_RADIUS_PT,
        })
        .collect();
    out.sort_by(|a, b| {
        a.x.partial_cmp(&b.x)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.y_top.partial_cmp(&b.y_top).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

/// True if `(cursor_x, cursor_y)` falls over any slat's slot.
pub fn hit_test_slat(layout: &BlindLayout, cursor_x: f32, cursor_y: f32) -> bool {
    layout
        .slats
        .iter()
        .any(|s| s.slot_contains(cursor_x, cursor_y))
}

/// Build a glyphon buffer holding one slat's filename.
/// Folders get a trailing slash so they read as containers.
pub fn build_filename_buffer(
    font_system: &mut FontSystem,
    name: &str,
    kind: EntryKind,
    font_size: f32,
    line_height: f32,
) -> Buffer {
    let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
    let display = match kind {
        EntryKind::Folder => format!("{name}/"),
        EntryKind::File => name.to_string(),
    };
    buffer.set_text(
        font_system,
        &display,
        Attrs::new().family(Family::Monospace),
        Shaping::Advanced,
    );
    if !buffer.lines.is_empty() {
        let last = buffer.lines.len() - 1;
        buffer.shape_until_cursor(font_system, glyphon::Cursor::new(last, 0), false);
    }
    buffer
}

/// Expand every folder in the tree (debug-only bootstrap for the
/// `--debug-expand-all` flag).
pub fn expand_all(tree: &mut TreeState) {
    use std::path::PathBuf;
    let mut pending: Vec<PathBuf> = tree
        .root
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::Folder)
        .map(|e| e.path.clone())
        .collect();
    while let Some(folder) = pending.pop() {
        if tree.expanded.contains(&folder) {
            continue;
        }
        tree.expanded.insert(folder.clone());
        tree.ensure_expanded_walked();
        if let Some(listing) = tree.children.get(&folder) {
            for entry in &listing.entries {
                if entry.kind == EntryKind::Folder {
                    pending.push(entry.path.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filetree::{DirectoryEntry, DirectoryListing};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn mock_listing(root: &str, entries: &[(&str, EntryKind)]) -> DirectoryListing {
        DirectoryListing {
            root: PathBuf::from(root),
            entries: entries
                .iter()
                .map(|(n, k)| DirectoryEntry {
                    name: n.to_string(),
                    path: PathBuf::from(format!("{root}/{n}")),
                    kind: *k,
                })
                .collect(),
        }
    }

    fn mock_tree_flat(entries: &[(&str, EntryKind)]) -> TreeState {
        TreeState::new(mock_listing("/r", entries))
    }

    #[test]
    fn closed_is_plaque_with_ink_color() {
        let tree = mock_tree_flat(&[("a.py", EntryKind::File)]);
        let l = layout(&tree, 0.0, 300.0, 800.0, 1.0, SlatMode::Closed, &HashMap::new());
        let s = &l.slats[0];
        assert_eq!(s.design, SlatDesign::Closed);
        // Plaque is tall — close to full slot height (minus padding).
        assert!(s.slat_height > SLAT_HEIGHT_PT * 0.8);
        assert_eq!(s.text_rgb, CLOSED_INK_RGB);
    }

    #[test]
    fn open_is_shelf_with_standing_text_color() {
        let tree = mock_tree_flat(&[("a.py", EntryKind::File)]);
        let l = layout(&tree, 0.0, 300.0, 800.0, 1.0, SlatMode::Open, &HashMap::new());
        let s = &l.slats[0];
        assert_eq!(s.design, SlatDesign::Open);
        // Shelf is a thin strip at the bottom of the slot.
        assert!(s.slat_height < SLAT_HEIGHT_PT * 0.3);
        let slot_bottom = s.slot_y + s.slot_height;
        assert!((s.slat_y + s.slat_height - slot_bottom).abs() < 1e-3);
        assert_eq!(s.text_rgb, OPEN_STANDING_RGB);
    }

    #[test]
    fn alternating_swaps_design_per_row() {
        let tree = mock_tree_flat(&[
            ("a.py", EntryKind::File),
            ("b.py", EntryKind::File),
            ("c.py", EntryKind::File),
        ]);
        let l = layout(&tree, 0.0, 300.0, 800.0, 1.0, SlatMode::Alternating, &HashMap::new());
        assert_eq!(l.slats[0].design, SlatDesign::Closed);
        assert_eq!(l.slats[1].design, SlatDesign::Open);
        assert_eq!(l.slats[2].design, SlatDesign::Closed);
    }

    #[test]
    fn deeper_slats_use_further_right_rope_column() {
        let root = mock_listing(
            "/r",
            &[("a", EntryKind::Folder), ("tail.py", EntryKind::File)],
        );
        let a_children = mock_listing("/r/a", &[("inner.py", EntryKind::File)]);
        let mut tree = TreeState::new(root);
        tree.children.insert(PathBuf::from("/r/a"), a_children);
        tree.expanded.insert(PathBuf::from("/r/a"));

        let l = layout(&tree, 0.0, 400.0, 800.0, 1.0, SlatMode::Closed, &HashMap::new());
        assert_eq!(l.slats.len(), 3);
        // slats[0] = a (depth 0), slats[1] = inner.py (depth 1), slats[2] = tail.py (depth 0).
        assert!((l.slats[0].rope_x - l.slats[2].rope_x).abs() < 1e-3);
        assert!(l.slats[1].rope_x > l.slats[0].rope_x);
    }

    #[test]
    fn closed_has_tall_centered_hole_open_has_none() {
        let tree = mock_tree_flat(&[("a.py", EntryKind::File)]);
        let l_closed = layout(&tree, 0.0, 300.0, 800.0, 1.0, SlatMode::Closed, &HashMap::new());
        let s = &l_closed.slats[0];
        let hole = s.hole.expect("Closed must carry a hole");
        // Hole is vertically centered in the slat.
        let slat_mid = s.slat_y + s.slat_height * 0.5;
        assert!((hole.center_y - slat_mid).abs() < 1e-3);
        // Hole spans ~60% of the slat's height, leaving top/bottom
        // strips for the 3-z-level rope-threading visual.
        assert!(hole.height > s.slat_height * 0.5);
        assert!(hole.height < s.slat_height * 0.75);
        assert!(hole.width < hole.height);

        let l_open = layout(&tree, 0.0, 300.0, 800.0, 1.0, SlatMode::Open, &HashMap::new());
        assert!(l_open.slats[0].hole.is_none());
    }

    #[test]
    fn rope_groups_separate_sibling_subtrees() {
        // Layout:
        //  /r/a (folder, depth 0)
        //      /r/a/x.py (depth 1) -- a's children group
        //      /r/a/y.py (depth 1) -- same group
        //  /r/b (folder, depth 0)
        //      /r/b/z.py (depth 1) -- DIFFERENT group from a's children
        let root = mock_listing(
            "/r",
            &[("a", EntryKind::Folder), ("b", EntryKind::Folder)],
        );
        let a_children = mock_listing(
            "/r/a",
            &[("x.py", EntryKind::File), ("y.py", EntryKind::File)],
        );
        let b_children = mock_listing("/r/b", &[("z.py", EntryKind::File)]);
        let mut tree = TreeState::new(root);
        tree.children.insert(PathBuf::from("/r/a"), a_children);
        tree.children.insert(PathBuf::from("/r/b"), b_children);
        tree.expanded.insert(PathBuf::from("/r/a"));
        tree.expanded.insert(PathBuf::from("/r/b"));

        let entries = flatten(&tree);
        let groups = assign_rope_groups(&entries);
        // entries order: a, x, y, b, z
        // a and b share group 0 (depth 0 siblings).
        // x and y share group 1 (a's children).
        // z is group 2 (b's children — separate rope from a's).
        let by_name: HashMap<&str, usize> = entries
            .iter()
            .zip(groups.iter())
            .map(|(e, g)| (e.name.as_str(), *g))
            .collect();
        assert_eq!(by_name["a"], by_name["b"]);
        assert_eq!(by_name["x.py"], by_name["y.py"]);
        assert_ne!(by_name["x.py"], by_name["z.py"]);
        assert_ne!(by_name["a"], by_name["x.py"]);
    }

    #[test]
    fn ropes_one_per_group_with_correct_span() {
        // Two siblings at depth 0; the rope spans both.
        let tree = mock_tree_flat(&[
            ("a.py", EntryKind::File),
            ("b.py", EntryKind::File),
        ]);
        let l = layout(&tree, 0.0, 300.0, 800.0, 1.0, SlatMode::Closed, &HashMap::new());
        assert_eq!(l.ropes.len(), 1);
        let r = &l.ropes[0];
        // Rope y_top is above the first slot (overshoot); y_bottom past the last.
        assert!(r.y_top < l.slats[0].slot_y);
        let last_bottom = l.slats[1].slot_y + l.slats[1].slot_height;
        assert!(r.y_bottom > last_bottom);
    }
}
