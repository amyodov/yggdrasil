//! Blind primitive — the file-tree visualization. Cardboard slats
//! hanging from a brass wire on the left of the canvas, each slat
//! carrying a filename. The blind lives in Zone 1 (the void), drawn
//! directly into the swap chain — not on a substrate, because the
//! tree is a cluster of discrete hanging objects, not a continuous
//! lit surface.
//!
//! ## Phase B scope
//!
//! Visible static rendering only. Slat rectangles + filename text +
//! brass wire down the left side. No interaction (clicks ignored), no
//! animation (`bootstrap_progress` and `anim_progress` from
//! `TreeState` ignored), no monkey-fist knot affordance for folders
//! yet — folders just render slightly darker and get a trailing slash
//! in the name. Phases C/D bring hit-testing, knot rendering, and
//! the wind/unwind animation.
//!
//! ## Architectural fit
//!
//! Today the slats are computed flat each frame from `flatten(&tree)`.
//! When the container-tree primitive (YGG-51) lands, each slat
//! becomes a leaf node in the container hierarchy with its own
//! transform — at which point bootstrap / wind animations fall out
//! naturally as transforms on slat containers. The flat layout here
//! is the "would have to do this anyway" data the formalized
//! container will wrap.

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

use crate::filetree::{flatten, EntryKind, SlatEntry, TreeState};

// ---------------------------------------------------------------------
// Layout / palette constants. Logical points where marked _PT.
// ---------------------------------------------------------------------

/// Outer padding of the blind region from the left pane's edges.
pub const BLIND_MARGIN_PT: f32 = 14.0;
/// Vertical height of one slat.
pub const SLAT_HEIGHT_PT: f32 = 26.0;
/// Vertical gap between adjacent slats — the eye reads "individual
/// hanging cards" rather than a continuous strip.
pub const SLAT_GAP_PT: f32 = 4.0;
/// Horizontal indent per depth level.
pub const SLAT_INDENT_PT: f32 = 14.0;
/// Horizontal gap between the brass wire and the slats hanging off it.
pub const WIRE_TO_SLAT_GAP_PT: f32 = 6.0;
/// File-slat fill — kraft cardboard, warm tan-brown, near-opaque.
const FILE_SLAT_BG: [f32; 4] = [0.50, 0.36, 0.23, 0.95];
/// Folder-slat fill — a hair darker so the eye clusters folders as
/// "containers" without needing a chrome icon.
const FOLDER_SLAT_BG: [f32; 4] = [0.42, 0.30, 0.20, 0.95];
/// Faint kraft-tinted bloom into the void around each slat. Zone 1
/// rule says objects in the void emit; we keep the alpha very low so
/// slats read as "present in the void" without pretending to be
/// glowing — the cardboard identity dominates.
const SLAT_GLOW: [f32; 4] = [0.55, 0.38, 0.22, 0.16];
const SLAT_GLOW_RADIUS_PT: f32 = 8.0;
const SLAT_CORNER_RADIUS_PT: f32 = 3.0;
/// Brass wire — slats hang from this. Same brass family as the class
/// armature foil so the void's metal vocabulary stays consistent.
const WIRE_COLOR: [f32; 4] = [0.55, 0.40, 0.20, 1.0];
const WIRE_GLOW: [f32; 4] = [0.62, 0.46, 0.22, 0.28];
pub const WIRE_WIDTH_PT: f32 = 2.0;
const WIRE_GLOW_RADIUS_PT: f32 = 5.0;
/// Filename text inset from the slat's left edge.
const TEXT_INSET_X_PT: f32 = 10.0;

/// Filename ink color (warm ivory, near-opaque on cardboard).
pub const FILENAME_RGB: (u8, u8, u8) = (240, 232, 218);

// ---------------------------------------------------------------------
// Layout output
// ---------------------------------------------------------------------

/// One slat after layout. Contains everything needed to draw the
/// rectangle and locate its filename text.
#[derive(Debug, Clone)]
pub struct LaidSlat {
    pub entry: SlatEntry,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub bg: [f32; 4],
    pub glow_color: [f32; 4],
    pub glow_radius: f32,
    pub corner: f32,
}

/// Brass-wire segment running down the left of the blind.
#[derive(Debug, Clone)]
pub struct WireRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: [f32; 4],
    pub glow_color: [f32; 4],
    pub glow_radius: f32,
}

/// One frame's blind layout.
#[derive(Debug, Clone)]
pub struct BlindLayout {
    pub slats: Vec<LaidSlat>,
    pub wire: WireRect,
}

/// Compute the blind's layout for the given tree state and pane bounds
/// (window-space, physical pixels). The blind occupies the left
/// `pane_width_px` of the window; slats stack top-to-bottom with
/// per-depth indentation; the wire runs full height down the left.
pub fn layout(
    tree: &TreeState,
    pane_left_px: f32,
    pane_width_px: f32,
    pane_height_px: f32,
    scale_factor: f32,
) -> BlindLayout {
    let sf = scale_factor;
    let margin = BLIND_MARGIN_PT * sf;
    let slat_h = SLAT_HEIGHT_PT * sf;
    let gap = SLAT_GAP_PT * sf;
    let indent = SLAT_INDENT_PT * sf;
    let wire_w = WIRE_WIDTH_PT * sf;
    let wire_to_slat_gap = WIRE_TO_SLAT_GAP_PT * sf;

    let entries = flatten(tree);
    let mut slats = Vec::with_capacity(entries.len());

    let blind_left = pane_left_px + margin;
    let blind_right = pane_left_px + pane_width_px - margin;
    let blind_top = margin;

    let wire_x = blind_left;
    let wire_y = blind_top;
    let wire_h = (pane_height_px - 2.0 * margin).max(0.0);

    let slats_left = wire_x + wire_w + wire_to_slat_gap;

    let mut y = blind_top - tree.scroll_y;
    for entry in entries {
        let depth_indent = entry.depth as f32 * indent;
        let x = slats_left + depth_indent;
        let w = (blind_right - x).max(0.0);
        let bg = match entry.kind {
            EntryKind::Folder => FOLDER_SLAT_BG,
            EntryKind::File => FILE_SLAT_BG,
        };
        slats.push(LaidSlat {
            entry,
            x,
            y,
            width: w,
            height: slat_h,
            bg,
            glow_color: SLAT_GLOW,
            glow_radius: SLAT_GLOW_RADIUS_PT * sf,
            corner: SLAT_CORNER_RADIUS_PT * sf,
        });
        y += slat_h + gap;
    }

    BlindLayout {
        slats,
        wire: WireRect {
            x: wire_x,
            y: wire_y,
            width: wire_w,
            height: wire_h,
            color: WIRE_COLOR,
            glow_color: WIRE_GLOW,
            glow_radius: WIRE_GLOW_RADIUS_PT * sf,
        },
    }
}

/// Build a glyphon buffer holding one slat's filename text.
/// Folders get a trailing slash so the eye reads them as containers
/// even before the knot affordance lands.
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
    buffer.shape_until_scroll(font_system, false);
    buffer
}

/// Filename-text horizontal inset from the slat's left edge in
/// physical pixels.
pub fn text_inset_x(scale_factor: f32) -> f32 {
    TEXT_INSET_X_PT * scale_factor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filetree::{DirectoryEntry, DirectoryListing};
    use std::path::PathBuf;

    fn mock_tree(names: &[(&str, EntryKind)]) -> TreeState {
        let listing = DirectoryListing {
            root: PathBuf::from("/r"),
            entries: names
                .iter()
                .map(|(n, k)| DirectoryEntry {
                    name: n.to_string(),
                    path: PathBuf::from(format!("/r/{n}")),
                    kind: *k,
                })
                .collect(),
        };
        TreeState::new(listing)
    }

    #[test]
    fn layout_places_slats_top_down_with_gaps() {
        let tree = mock_tree(&[
            ("src", EntryKind::Folder),
            ("README.md", EntryKind::File),
        ]);
        let l = layout(&tree, 0.0, 300.0, 800.0, 1.0);
        assert_eq!(l.slats.len(), 2);
        // Top of first slat = blind margin (no scroll).
        assert!((l.slats[0].y - BLIND_MARGIN_PT).abs() < 1e-3);
        // Second slat is one (slat_h + gap) below.
        let expected_second_y = BLIND_MARGIN_PT + SLAT_HEIGHT_PT + SLAT_GAP_PT;
        assert!((l.slats[1].y - expected_second_y).abs() < 1e-3);
        // Folder fill differs from file fill.
        assert_ne!(l.slats[0].bg, l.slats[1].bg);
    }

    #[test]
    fn layout_indents_by_depth() {
        let tree = mock_tree(&[("src", EntryKind::Folder)]);
        let l_root = layout(&tree, 0.0, 300.0, 800.0, 1.0);
        // Root-depth slat x is wire_x + wire_w + wire_to_slat_gap.
        let expected_x = BLIND_MARGIN_PT + WIRE_WIDTH_PT + WIRE_TO_SLAT_GAP_PT;
        assert!((l_root.slats[0].x - expected_x).abs() < 1e-3);
    }

    #[test]
    fn layout_scales_with_dpi() {
        let tree = mock_tree(&[("a.py", EntryKind::File)]);
        let l1 = layout(&tree, 0.0, 300.0, 800.0, 1.0);
        let l2 = layout(&tree, 0.0, 600.0, 1600.0, 2.0);
        // 2x scale factor doubles slat height and margin.
        assert!((l2.slats[0].height - 2.0 * l1.slats[0].height).abs() < 1e-3);
        assert!((l2.slats[0].y - 2.0 * l1.slats[0].y).abs() < 1e-3);
    }

    #[test]
    fn layout_emits_full_height_wire() {
        let tree = mock_tree(&[]);
        let l = layout(&tree, 0.0, 300.0, 800.0, 1.0);
        let expected_h = 800.0 - 2.0 * BLIND_MARGIN_PT;
        assert!((l.wire.height - expected_h).abs() < 1e-3);
    }
}
