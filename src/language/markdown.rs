//! Markdown LanguageModule — parser and heading-builder wired;
//! `extract_cards` and `highlights_query` are stubs.
//!
//! Markdown's card extraction is structurally different from
//! Python/Rust: headings form an implicit hierarchy, with depth
//! derived from the ATX level. A Markdown `extract_cards` walks
//! `section` nodes and builds a Card tree accordingly. Deferred
//! until the tagged-CardKind refactor lands — that refactor gives
//! us a `Section` card kind to target.
//!
//! Tree-sitter-md doesn't ship a highlight query today, so the
//! token stream renders all-Default. A future Pass 2 could add a
//! hand-rolled minimal query (heading markers, emphasis, code
//! spans, links) — orthogonal to this module's Part 1 scope.

use tree_sitter::{Language, Node, Tree};

use super::LanguageModule;
use crate::cards::{whole_file_snippet, Card};
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

    fn extract_cards(&self, _tree: &Tree, source: &str, line_offsets: &[usize]) -> Vec<Card> {
        // Placeholder: show the whole file as one Snippet until the
        // Markdown-specific extractor (section walk, heading depth →
        // Card depth) lands with YGG-27 Part 2.
        whole_file_snippet(source, line_offsets)
    }

    fn build_header(&self, node: Node, source: &str) -> Option<HeaderModel> {
        markdown::build_header(node, source)
    }
}
