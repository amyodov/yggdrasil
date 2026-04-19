//! Markdown LanguageModule.
//!
//! `extract_cards` walks the `section` tree produced by
//! tree-sitter-md. Each section owns one heading and its content,
//! plus nested sections for deeper headings. The Card tree mirrors
//! this:
//!
//! - A section with no nested subsections → leaf card (`Function`)
//!   whose body range covers all prose between the heading and the
//!   section's end. The reader sees the heading plus the paragraph
//!   text below it rendered as a single foldable card.
//! - A section with nested subsections → container card (`Class`).
//!   The renderer stacks children under it, forming a visible
//!   section hierarchy. Prose between the heading and the first
//!   subsection is not separately surfaced today — that's a gap a
//!   future pass can close by emitting a prose-Snippet child.
//!
//! Card depth comes from heading level directly: `H1 → 0`, `H2 → 1`,
//! and so on. Sections parsed at document top-level are all `H1`s;
//! deeper levels appear as nested sections in the grammar.
//!
//! Tree-sitter-md doesn't ship a highlight query, so the token
//! stream is all-`Default`. A hand-rolled query (heading markers,
//! emphasis, code spans, links) is a future pass — orthogonal to
//! card extraction.

use tree_sitter::{Language, Node, Tree};

use super::LanguageModule;
use crate::cards::{build_card, Card, CardId, CardKind, Visibility};
use crate::header::{markdown, HeaderModel};

pub struct MarkdownModule;

impl LanguageModule for MarkdownModule {
    fn name(&self) -> &'static str {
        "Markdown"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["md", "markdown"]
    }

    fn grammar(&self) -> Language {
        tree_sitter_md::LANGUAGE.into()
    }

    fn highlights_query(&self) -> &'static str {
        ""
    }

    fn extract_cards(&self, tree: &Tree, source: &str, line_offsets: &[usize]) -> Vec<Card> {
        let mut out = Vec::new();
        let mut next_id: u32 = 0;
        let mut cursor = tree.root_node().walk();
        for child in tree.root_node().children(&mut cursor) {
            if child.kind() == "section" {
                process_section(child, source, line_offsets, None, 0, &mut out, &mut next_id);
            }
        }
        out
    }

    fn build_header(&self, node: Node, source: &str) -> Option<HeaderModel> {
        markdown::build_header(node, source)
    }
}

/// Walk one section. Emits a Card for this section, then recurses
/// into any nested sections. Kind depends on whether the section has
/// child sections.
fn process_section(
    section: Node,
    source: &str,
    line_offsets: &[usize],
    parent: Option<CardId>,
    depth: usize,
    out: &mut Vec<Card>,
    next_id: &mut u32,
) {
    let Some(heading) = find_child(section, &["atx_heading", "setext_heading"]) else {
        return;
    };
    let nested_sections: Vec<Node> = {
        let mut cursor = section.walk();
        section
            .children(&mut cursor)
            .filter(|c| c.kind() == "section")
            .collect()
    };

    let name = heading_text(heading, source);
    let full_range = section.byte_range();
    let header_range = heading.byte_range();
    let (kind, body_range) = if nested_sections.is_empty() {
        // Leaf section — the body is the prose under the heading.
        // Render as Function so the text is actually visible on the
        // card (Class bodies get stacked children instead).
        let body = header_range.end..full_range.end;
        let body = if body.start < body.end { Some(body) } else { None };
        (CardKind::Function, body)
    } else {
        // Container section — children stacked under it. Body range
        // set to None so layout uses the class-style child stack.
        (CardKind::Class, None)
    };

    let id = CardId(*next_id);
    *next_id += 1;
    out.push(build_card(
        id,
        kind,
        parent,
        depth,
        name,
        Visibility::Public,
        header_range,
        body_range,
        full_range,
        line_offsets,
    ));

    for sub in nested_sections {
        process_section(sub, source, line_offsets, Some(id), depth + 1, out, next_id);
    }
}

// `manual_find` suggestion doesn't apply: the iterator form would keep
// the cursor borrow alive across the return, which the borrow checker
// rejects.
#[allow(clippy::manual_find)]
fn find_child<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if kinds.contains(&c.kind()) {
            return Some(c);
        }
    }
    None
}

/// Extract a heading's display text — the `inline` child of an ATX
/// heading, or the paragraph content of a Setext heading. Markdown
/// inline markup (`*em*`, `` `code` ``, `[link](...)`) is left as
/// source text so a TOC-style reader sees it literally.
fn heading_text(heading: Node, source: &str) -> String {
    let mut cursor = heading.walk();
    for c in heading.children(&mut cursor) {
        if matches!(c.kind(), "inline" | "paragraph") {
            return node_text(c, source).trim().to_string();
        }
    }
    let raw = node_text(heading, source);
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

    fn extract(src: &str) -> Vec<Card> {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_md::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let offs = line_offsets(src);
        MarkdownModule.extract_cards(&tree, src, &offs)
    }

    // ---- One heading at each level: simple flat cases --------

    #[test]
    fn single_h1_is_leaf_function() {
        let cards = extract("# Installation\n\nRun the thing.\n");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, CardKind::Function);
        assert_eq!(cards[0].name, "Installation");
        assert_eq!(cards[0].depth, 0);
        assert!(cards[0].body_range.is_some());
    }

    #[test]
    fn h1_with_h2_children_becomes_class() {
        let cards = extract("# Top\n\nProse.\n\n## Sub\n\nInner.\n");
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].kind, CardKind::Class);
        assert_eq!(cards[0].name, "Top");
        assert_eq!(cards[0].depth, 0);
        // H1 with children has no body_range (layout stacks children instead).
        assert!(cards[0].body_range.is_none());
        // H2 is a child Function leaf.
        assert_eq!(cards[1].kind, CardKind::Function);
        assert_eq!(cards[1].name, "Sub");
        assert_eq!(cards[1].depth, 1);
        assert_eq!(cards[1].parent, Some(cards[0].id));
    }

    // ---- Deep nesting: H1 → H2 → H3 --------------------------

    #[test]
    fn three_level_hierarchy() {
        let src = "# Doc\n\n## A\n\n### A1\n\nContent.\n\n### A2\n\nMore.\n";
        let cards = extract(src);
        assert_eq!(cards.len(), 4);
        let shape: Vec<(CardKind, &str, usize)> = cards
            .iter()
            .map(|c| (c.kind, c.name.as_str(), c.depth))
            .collect();
        assert_eq!(
            shape,
            vec![
                (CardKind::Class, "Doc", 0),
                (CardKind::Class, "A", 1),
                (CardKind::Function, "A1", 2),
                (CardKind::Function, "A2", 2),
            ],
        );
    }

    // ---- Multiple H1 siblings at document top-level ----------

    #[test]
    fn sibling_h1s_are_both_top_level() {
        let src = "# First\n\nAlpha.\n\n# Second\n\nBeta.\n";
        let cards = extract(src);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].name, "First");
        assert_eq!(cards[0].depth, 0);
        assert_eq!(cards[0].parent, None);
        assert_eq!(cards[1].name, "Second");
        assert_eq!(cards[1].depth, 0);
        assert_eq!(cards[1].parent, None);
    }

    // ---- Inline formatting preserved as literal text ---------

    #[test]
    fn inline_formatting_kept_in_heading_name() {
        let cards = extract("# The `main` function\n\nBody.\n");
        assert_eq!(cards[0].name, "The `main` function");
    }
}
