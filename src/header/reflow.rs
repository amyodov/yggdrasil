//! Block-flow reflow (M3.5b).
//!
//! Takes a `HeaderModel` plus measured block widths plus a maximum
//! available width, and decides how to lay the four blocks across one
//! or more rows. Block 4 (return type) always pins top-right; as the
//! available width shrinks the engine chooses progressively more
//! aggressive wraps:
//!
//! 1. **Single row** — `[1][2][3 3 3 3][4]` all on one line.
//! 2. **Params wrap** — row 1 carries `[1][2][first few params][4]`;
//!    the rest of the params cascade onto indented continuation rows
//!    under the name column.
//! 3. **Name-only + params below** — row 1 is `[1][2]    [4]` (return
//!    still pinned right, empty space between name and return); every
//!    param moves to its own indented continuation row.
//!
//! The engine is a pure function of `(model, widths, max_width)` — no
//! state, no side effects — so transitions as the user resizes the
//! window are a free consequence of re-running the function each
//! frame with the new width. This fits Yggdrasil's "one state, derived
//! rendering" principle: animations happen because the inputs change,
//! not because the layout mutates itself.

use super::HeaderModel;

/// Measured widths of each block, plus the between-block gap and the
/// indent used for continuation rows. All values are in the same unit
/// (typically logical points); the engine doesn't care which one so
/// long as the caller is consistent.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Consumer wiring lands under eyes-on review.
pub struct BlockWidths<'a> {
    /// Width of block 1 (prelude: decorators + keyword badge).
    pub prelude: f32,
    /// Width of block 2 (name identifier).
    pub name: f32,
    /// Width of block 4 (return type). `0.0` for kinds that have no
    /// return (classes, markdown headings).
    pub return_type: f32,
    /// Width of each individual param chip, by index into
    /// `HeaderModel.params`.
    pub params: &'a [f32],
    /// Horizontal gap inserted between adjacent blocks on a single
    /// row. Applied once per pair of adjacent blocks.
    pub inter_block_gap: f32,
    /// How far continuation rows are indented from the card's left
    /// edge. Pins the continuation parameters under block 2's column.
    pub name_column_indent: f32,
}

/// Which of the three layout paths the engine picked. Surfaced
/// alongside the row list so callers (tests, future snapshot checks)
/// can assert on the high-level decision without inspecting row
/// contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LayoutMode {
    SingleRow,
    ParamsWrap,
    NameOnlyWithParamsBelow,
}

/// One laid-out row. Fields say which blocks appear on it; the
/// renderer reads them in a fixed left-to-right order. Indices in
/// `params` point back into `HeaderModel.params`.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub struct HeaderRowLayout {
    pub prelude: bool,
    pub name: bool,
    pub params: Vec<usize>,
    pub return_type: bool,
    /// Logical-point offset from the card's left edge. Zero for row 1
    /// in every mode; equal to `name_column_indent` for continuation
    /// rows.
    pub indent: f32,
}

/// The full result: the chosen mode + the rows to render.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub struct HeaderLayout {
    pub mode: LayoutMode,
    pub rows: Vec<HeaderRowLayout>,
}

/// Lay out `model` inside `max_width`. Pure function of its inputs.
///
/// The engine tries paths 1 → 2 → 3 in order and commits to the first
/// that fits. Path 3 is the unconditional fallback — if even a single
/// param is wider than the continuation row can hold it will still be
/// placed on its own row (it'll clip visually, but the structure is
/// preserved for future hover-expand / tooltip behavior).
#[allow(dead_code)] // Consumer wiring lands under eyes-on review.
pub fn reflow(model: &HeaderModel, widths: &BlockWidths, max_width: f32) -> HeaderLayout {
    let gap = widths.inter_block_gap;
    let n_params = model.params.len();
    let params_total: f32 = widths.params.iter().sum();
    // Gaps between params: (n-1). Plus a gap on each side to bracket
    // the run against neighbours. We account for those separately
    // below.
    let inter_param_gaps = gap * (n_params.saturating_sub(1) as f32);
    let has_return = model.return_type.is_some();

    // Path 1: everything on one row.
    // Layout on row 1: [prelude] gap [name] gap [params] gap [return].
    // Gaps present between every pair of adjacent blocks that both
    // exist.
    let single_row = {
        let mut total = widths.prelude + widths.name + params_total + inter_param_gaps;
        // Count the inter-block gaps: between prelude-name (always if
        // both non-zero), between name-params (if any params), between
        // params-return (if both). We just add up to 3 * gap in the
        // common case (all four present); clients that omit a block
        // pass zero width for it and the single row still works.
        if widths.prelude > 0.0 {
            total += gap;
        }
        if n_params > 0 {
            total += gap;
        }
        if has_return {
            total += gap + widths.return_type;
        }
        total
    };
    if single_row <= max_width {
        return HeaderLayout {
            mode: LayoutMode::SingleRow,
            rows: vec![HeaderRowLayout {
                prelude: true,
                name: true,
                params: (0..n_params).collect(),
                return_type: has_return,
                indent: 0.0,
            }],
        };
    }

    // Path 2: try to keep [prelude][name]...[return] on row 1 and wrap
    // params.
    //
    // Row 1 fixed share: prelude + gap + name + gap + ... + gap + return.
    // We reserve at least one gap between name and the first-row param
    // AND a gap between the last first-row param and return. If zero
    // first-row params fit, we fall through to path 3.
    let row1_fixed = widths.prelude
        + if widths.prelude > 0.0 { gap } else { 0.0 }
        + widths.name
        + if has_return { gap + widths.return_type } else { 0.0 };
    // Room left on row 1 for some first-row params. Need a gap between
    // name and the first param on this row, so subtract one more gap
    // when we actually place any params there.
    let row1_param_budget = (max_width - row1_fixed - gap).max(0.0);

    let cont_budget = (max_width - widths.name_column_indent).max(0.0);

    if row1_fixed + gap <= max_width && fits_on_continuation(widths.params, gap, cont_budget) {
        let (first_row_params, rest) = greedy_pack(widths.params, gap, row1_param_budget);
        if !first_row_params.is_empty() {
            let mut rows = Vec::with_capacity(1 + rest.len().div_ceil(1));
            rows.push(HeaderRowLayout {
                prelude: true,
                name: true,
                params: first_row_params,
                return_type: has_return,
                indent: 0.0,
            });
            for batch in pack_rows(&rest, widths.params, gap, cont_budget) {
                rows.push(HeaderRowLayout {
                    prelude: false,
                    name: false,
                    params: batch,
                    return_type: false,
                    indent: widths.name_column_indent,
                });
            }
            return HeaderLayout { mode: LayoutMode::ParamsWrap, rows };
        }
    }

    // Path 3: row 1 has [prelude][name] (and [return] pinned right
    // via empty space in between); every param moves to its own
    // continuation row.
    let mut rows = Vec::new();
    rows.push(HeaderRowLayout {
        prelude: true,
        name: true,
        params: Vec::new(),
        return_type: has_return,
        indent: 0.0,
    });
    let all_indices: Vec<usize> = (0..n_params).collect();
    for batch in pack_rows(&all_indices, widths.params, gap, cont_budget) {
        rows.push(HeaderRowLayout {
            prelude: false,
            name: false,
            params: batch,
            return_type: false,
            indent: widths.name_column_indent,
        });
    }
    HeaderLayout { mode: LayoutMode::NameOnlyWithParamsBelow, rows }
}

/// Greedy-pack as many params as will fit into `budget`, starting
/// from index 0. Returns `(packed, remaining_indices)`. Gaps between
/// packed params count against the budget; a leading gap is the
/// caller's concern.
fn greedy_pack(param_widths: &[f32], gap: f32, budget: f32) -> (Vec<usize>, Vec<usize>) {
    let mut packed = Vec::new();
    let mut used = 0.0;
    for (i, &w) in param_widths.iter().enumerate() {
        let needed = if packed.is_empty() { w } else { used + gap + w };
        if needed <= budget {
            used = needed;
            packed.push(i);
        } else {
            let rest: Vec<usize> = (i..param_widths.len()).collect();
            return (packed, rest);
        }
    }
    (packed, Vec::new())
}

/// Pack `indices` (each referring to a param) into as many rows as
/// needed so each row fits `budget`. A single param wider than the
/// budget still gets its own row (it'll overflow visually, but we
/// preserve one-param-per-row so the data structure stays sane).
fn pack_rows(
    indices: &[usize],
    param_widths: &[f32],
    gap: f32,
    budget: f32,
) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut used = 0.0;
    for &idx in indices {
        let w = param_widths[idx];
        let needed = if current.is_empty() { w } else { used + gap + w };
        if current.is_empty() || needed <= budget {
            if current.is_empty() {
                used = w;
            } else {
                used = needed;
            }
            current.push(idx);
        } else {
            rows.push(std::mem::take(&mut current));
            used = w;
            current.push(idx);
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

/// Sanity check: at least one param fits on a continuation row. If
/// not, path 2 collapses to path 3 because there's no point wrapping
/// to an indent where nothing fits either. (Path 3 then prints each
/// oversize param alone and lets it overflow.)
fn fits_on_continuation(param_widths: &[f32], _gap: f32, budget: f32) -> bool {
    param_widths.iter().any(|&w| w <= budget) || param_widths.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{KeywordBadge, ParamChip, ParamKind, Prelude, TypeChip};
    use rstest::rstest;

    /// Build a mock HeaderModel with `n` params. Contents don't matter
    /// — reflow only looks at counts (and `return_type.is_some()`), the
    /// widths come from `BlockWidths`.
    fn mock_model(n_params: usize, has_return: bool) -> HeaderModel {
        let params = (0..n_params)
            .map(|i| ParamChip {
                name: format!("p{i}"),
                ty: None,
                default: None,
                kind: ParamKind::Regular,
            })
            .collect();
        let return_type = if has_return {
            Some(TypeChip { text: "T".to_string() })
        } else {
            None
        };
        HeaderModel {
            prelude: Prelude { decorators: vec![], keyword: KeywordBadge::Def },
            name: "f".to_string(),
            params,
            return_type,
            docstring: None,
        }
    }

    /// Default widths for tests: everything integer so arithmetic is
    /// exact. prelude=10, name=10, return=10, gap=2, indent=15.
    fn widths(param_widths: &[f32]) -> BlockWidths<'_> {
        BlockWidths {
            prelude: 10.0,
            name: 10.0,
            return_type: 10.0,
            params: param_widths,
            inter_block_gap: 2.0,
            name_column_indent: 15.0,
        }
    }

    // ---- Path 1: single-row --------------------------------------

    #[rstest]
    // Wide plate — 4 × 5-pt params + all blocks fit easily.
    #[case::fits_easily(200.0, 4)]
    // Exactly enough: prelude 10 + gap 2 + name 10 + gap 2 + 4*5 + 3*2 + gap 2 + return 10 = 62.
    #[case::exact_fit(62.0, 4)]
    fn single_row_when_fits(#[case] max_width: f32, #[case] n_params: usize) {
        let model = mock_model(n_params, true);
        let pw: Vec<f32> = vec![5.0; n_params];
        let result = reflow(&model, &widths(&pw), max_width);
        assert_eq!(result.mode, LayoutMode::SingleRow);
        assert_eq!(result.rows.len(), 1);
        let r = &result.rows[0];
        assert!(r.prelude && r.name && r.return_type);
        assert_eq!(r.params, (0..n_params).collect::<Vec<_>>());
        assert_eq!(r.indent, 0.0);
    }

    // ---- Path 2: params wrap -------------------------------------

    #[test]
    fn params_wrap_first_row_then_continuation() {
        // prelude(10)+gap(2)+name(10)+gap(2)+???+gap(2)+return(10) = 36 reserved
        // cont budget = 60 - 15 = 45 → lots of room.
        // At max=45, row1 param budget = 45 - 36 - 2 = 7. Can fit one 5-pt param.
        let model = mock_model(5, true);
        let pw = vec![5.0; 5];
        let result = reflow(&model, &widths(&pw), 45.0);
        assert_eq!(result.mode, LayoutMode::ParamsWrap);
        // Row 1 has prelude/name/return + one param.
        assert_eq!(result.rows[0].params, vec![0]);
        assert!(result.rows[0].return_type);
        // Row 2+ are continuation rows at the name column indent.
        for r in &result.rows[1..] {
            assert!(!r.prelude && !r.name && !r.return_type);
            assert_eq!(r.indent, 15.0);
        }
        // All five params accounted for.
        let mut all: Vec<usize> = Vec::new();
        for r in &result.rows {
            all.extend(&r.params);
        }
        assert_eq!(all, vec![0, 1, 2, 3, 4]);
    }

    // ---- Path 3: name-only + params below ------------------------

    #[test]
    fn name_only_when_row1_cant_host_any_params() {
        // max=37: prelude(10)+gap(2)+name(10)+gap(2)+return(10) = 34; +gap(2)
        // = 36 is the minimum to place ANY param next to name with
        // separator. Budget 37 leaves only 1pt for the first param —
        // less than any 5-pt param. So path 2 is rejected and path 3
        // runs.
        let model = mock_model(3, true);
        let pw = vec![5.0; 3];
        let result = reflow(&model, &widths(&pw), 37.0);
        assert_eq!(result.mode, LayoutMode::NameOnlyWithParamsBelow);
        assert_eq!(result.rows[0].params, Vec::<usize>::new());
        assert!(result.rows[0].return_type);
        let mut all: Vec<usize> = Vec::new();
        for r in &result.rows[1..] {
            assert_eq!(r.indent, 15.0);
            all.extend(&r.params);
        }
        assert_eq!(all, vec![0, 1, 2]);
    }

    // ---- Return type on row 1 in every mode ----------------------

    #[rstest]
    #[case::wide(200.0, LayoutMode::SingleRow)]
    #[case::medium(45.0, LayoutMode::ParamsWrap)]
    #[case::narrow(37.0, LayoutMode::NameOnlyWithParamsBelow)]
    fn return_type_always_on_row_1(#[case] max_width: f32, #[case] expected: LayoutMode) {
        let model = mock_model(5, true);
        let pw = vec![5.0; 5];
        let result = reflow(&model, &widths(&pw), max_width);
        assert_eq!(result.mode, expected);
        assert!(result.rows[0].return_type);
        for r in &result.rows[1..] {
            assert!(!r.return_type);
        }
    }

    // ---- No-param cases ------------------------------------------

    #[test]
    fn zero_params_is_always_single_row() {
        let model = mock_model(0, true);
        let result = reflow(&model, &widths(&[]), 100.0);
        assert_eq!(result.mode, LayoutMode::SingleRow);
        assert!(result.rows[0].params.is_empty());
    }

    // ---- No return (e.g. class / heading) ------------------------

    #[test]
    fn no_return_type_omitted_everywhere() {
        let model = mock_model(3, false);
        let pw = vec![5.0; 3];
        let result = reflow(&model, &widths(&pw), 200.0);
        assert_eq!(result.mode, LayoutMode::SingleRow);
        assert!(!result.rows[0].return_type);
    }

    // ---- Oversize param still emitted (one per row) --------------

    #[test]
    fn oversize_param_still_gets_its_own_row() {
        let model = mock_model(2, false);
        // Param 0 is 30-pt wide — wider than both row1 budget and
        // continuation budget at max=40.
        let pw = vec![30.0, 5.0];
        let result = reflow(&model, &widths(&pw), 40.0);
        // Shouldn't drop the param — it gets a row even if it'll
        // overflow visually.
        let mut all: Vec<usize> = Vec::new();
        for r in &result.rows {
            all.extend(&r.params);
        }
        assert_eq!(all, vec![0, 1]);
    }
}
