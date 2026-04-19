//! Markdown builder: heading → `HeaderModel`.
//!
//! Markdown doesn't have defs, classes, or parameters. What it does
//! have are **headings that form an implicit hierarchy**: an `# H1`
//! opens a section of the document, `## H2` a subsection under it,
//! and so on. Mapping that onto the ADT:
//!
//! - `HeaderModel.prelude.keyword = KeywordBadge::Heading(level)`.
//! - `HeaderModel.name` = the heading's inline text.
//! - Everything else (decorators, params, return_type, docstring) is
//!   absent.
//!
//! The caller (future `extract_cards` for markdown) walks the
//! `section` tree and calls `build_header` on each heading. Depth in
//! the resulting Card tree comes from the heading level: an `H2` is a
//! child of the nearest preceding `H1`, an `H3` of the nearest `H2`,
//! and so on. That's the caller's concern, not the builder's.
//!
//! Supports both ATX (`## Heading`) and Setext (underlined with `===`
//! / `---`) forms. Setext is capped to levels 1 and 2 by the Markdown
//! spec.

use tree_sitter::Node;

use super::{HeaderModel, KeywordBadge, Prelude};

/// Build a `HeaderModel` from a heading node. Accepts either
/// `atx_heading` or `setext_heading`; returns `None` for anything
/// else.
#[allow(dead_code)] // Consumer wiring arrives under eyes-on review.
pub fn build_header(node: Node, source: &str) -> Option<HeaderModel> {
    let level = match node.kind() {
        "atx_heading" => atx_level(node)?,
        "setext_heading" => setext_level(node)?,
        _ => return None,
    };
    let name = heading_text(node, source);
    Some(HeaderModel {
        prelude: Prelude {
            decorators: Vec::new(),
            keyword: KeywordBadge::Heading(level),
        },
        name,
        params: Vec::new(),
        return_type: None,
        docstring: None,
    })
}

/// Read the `atx_hN_marker` child of an `atx_heading` to find its
/// level. Tree-sitter-md emits one marker per heading (`atx_h1_marker`
/// … `atx_h6_marker`).
fn atx_level(node: Node) -> Option<u8> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if let Some(n) = marker_level(c.kind()) {
            return Some(n);
        }
    }
    None
}

fn marker_level(kind: &str) -> Option<u8> {
    match kind {
        "atx_h1_marker" => Some(1),
        "atx_h2_marker" => Some(2),
        "atx_h3_marker" => Some(3),
        "atx_h4_marker" => Some(4),
        "atx_h5_marker" => Some(5),
        "atx_h6_marker" => Some(6),
        _ => None,
    }
}

/// Setext headings are either `setext_h1_underline` (`===`) or
/// `setext_h2_underline` (`---`) — no higher levels exist in the spec.
fn setext_level(node: Node) -> Option<u8> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "setext_h1_underline" => return Some(1),
            "setext_h2_underline" => return Some(2),
            _ => {}
        }
    }
    None
}

/// Extract the heading's display text. ATX headings have an `inline`
/// child carrying the title text (minus the leading `# …` markers).
/// Setext headings' text is the paragraph line above the underline.
///
/// We read the raw source of the child rather than re-parsing it with
/// the inline grammar — emphasis / links / code spans render as their
/// literal text in the header, which matches how a reader scans a TOC
/// out loud ("Installation," not "*Installation*"). Full inline
/// formatting is a rendering concern for a later pass if it matters.
fn heading_text(node: Node, source: &str) -> String {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if matches!(c.kind(), "inline" | "paragraph") {
            return node_text(c, source).trim().to_string();
        }
    }
    // Fallback: the whole heading text minus markers. Strip leading
    // `#` markers and whitespace; take until end of line.
    let raw = node_text(node, source);
    raw.trim_start_matches(|c: char| c == '#' || c.is_whitespace())
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

fn node_text<'s>(node: Node, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use tree_sitter::{Parser, Tree};

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_md::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    /// Recursively find the first heading node (either ATX or Setext)
    /// in the tree. Tree-sitter-md wraps headings in `section` nodes,
    /// so a simple top-level scan wouldn't find them.
    fn find_first_heading(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
        if matches!(node.kind(), "atx_heading" | "setext_heading") {
            return Some(node);
        }
        let mut cursor = node.walk();
        for c in node.children(&mut cursor) {
            if let Some(h) = find_first_heading(c) {
                return Some(h);
            }
        }
        None
    }

    fn header_of(src: &str) -> HeaderModel {
        let tree = parse(src);
        let heading = find_first_heading(tree.root_node())
            .unwrap_or_else(|| panic!("no heading node in:\n{src}"));
        build_header(heading, src).expect("builder should accept this heading")
    }

    // ---- ATX headings (all six levels) --------------------------

    #[rstest]
    #[case::h1("# Installation\n", 1, "Installation")]
    #[case::h2("## Quick start\n", 2, "Quick start")]
    #[case::h3("### Details\n", 3, "Details")]
    #[case::h4("#### Notes\n", 4, "Notes")]
    #[case::h5("##### More\n", 5, "More")]
    #[case::h6("###### Deepest\n", 6, "Deepest")]
    fn atx_heading_levels(
        #[case] src: &str,
        #[case] expected_level: u8,
        #[case] expected_name: &str,
    ) {
        let h = header_of(src);
        assert_eq!(h.prelude.keyword, KeywordBadge::Heading(expected_level));
        assert_eq!(h.name, expected_name);
        assert!(h.params.is_empty());
        assert!(h.return_type.is_none());
        assert!(h.docstring.is_none());
    }

    // ---- Setext headings ----------------------------------------

    #[rstest]
    #[case::h1("Installation\n============\n", 1, "Installation")]
    #[case::h2("Quick start\n-----------\n", 2, "Quick start")]
    fn setext_heading_levels(
        #[case] src: &str,
        #[case] expected_level: u8,
        #[case] expected_name: &str,
    ) {
        let h = header_of(src);
        assert_eq!(h.prelude.keyword, KeywordBadge::Heading(expected_level));
        assert_eq!(h.name, expected_name);
    }

    // ---- Inline formatting preserved as literal text ------------

    #[rstest]
    #[case::emphasis("# *Emphasized*\n", "*Emphasized*")]
    #[case::code_span("# The `main` function\n", "The `main` function")]
    #[case::link("# See [docs](./d.md)\n", "See [docs](./d.md)")]
    fn inline_formatting_kept_as_source(#[case] src: &str, #[case] expected_name: &str) {
        let h = header_of(src);
        assert_eq!(h.name, expected_name);
    }

    // ---- Trailing whitespace trimmed ----------------------------

    #[test]
    fn trailing_whitespace_trimmed() {
        let src = "# Installation   \n";
        let h = header_of(src);
        assert_eq!(h.name, "Installation");
    }

    // ---- Non-heading nodes reject -------------------------------

    #[test]
    fn non_heading_node_yields_none() {
        let src = "Just a paragraph of text.\n";
        let tree = parse(src);
        // Walk to any non-heading leaf and confirm the builder refuses.
        fn walk(n: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
            if !matches!(n.kind(), "atx_heading" | "setext_heading") && n.kind() != "document" {
                return Some(n);
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                if let Some(x) = walk(c) {
                    return Some(x);
                }
            }
            None
        }
        if let Some(n) = walk(tree.root_node()) {
            assert!(build_header(n, src).is_none());
        }
    }

    // ---- Multiple headings: builder sees one at a time ----------

    #[test]
    fn multiple_headings_processed_independently() {
        let src = "# Top\n\n## Sub\n\n### Deep\n";
        let tree = parse(src);
        // Collect all headings in source order.
        fn collect<'a>(n: tree_sitter::Node<'a>, out: &mut Vec<tree_sitter::Node<'a>>) {
            if matches!(n.kind(), "atx_heading" | "setext_heading") {
                out.push(n);
                return;
            }
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                collect(c, out);
            }
        }
        let mut headings = Vec::new();
        collect(tree.root_node(), &mut headings);
        assert_eq!(headings.len(), 3);
        let results: Vec<_> = headings
            .into_iter()
            .map(|n| build_header(n, src).unwrap())
            .collect();
        assert_eq!(
            results.iter().map(|h| h.prelude.keyword).collect::<Vec<_>>(),
            vec![
                KeywordBadge::Heading(1),
                KeywordBadge::Heading(2),
                KeywordBadge::Heading(3),
            ],
        );
        assert_eq!(
            results.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            vec!["Top", "Sub", "Deep"],
        );
    }
}
