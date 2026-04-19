//! Rust LanguageModule — parser and header-builder wired;
//! `extract_cards` is a deliberate stub that returns an empty list.
//!
//! A Rust file opened through the registry today parses cleanly and
//! highlights (when a highlight query is available), but shows no
//! cards because the Card-extraction layer was written against
//! Python's AST shape (module-level def / class walk). Porting it
//! needs the tagged-CardKind refactor (YGG-27 Part 2) plus a
//! Rust-specific `extract_cards` that walks function_item /
//! struct_item / impl_item etc. That work is queued.

use tree_sitter::{Language, Node, Tree};

use super::LanguageModule;
use crate::cards::{whole_file_snippet, Card};
use crate::header::{rust, HeaderModel};

pub struct RustModule;

impl LanguageModule for RustModule {
    fn name(&self) -> &'static str {
        "Rust"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn grammar(&self) -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn highlights_query(&self) -> &'static str {
        tree_sitter_rust::HIGHLIGHTS_QUERY
    }

    fn extract_cards(&self, _tree: &Tree, source: &str, line_offsets: &[usize]) -> Vec<Card> {
        // Placeholder: show the whole file as one Snippet card until
        // the Rust-specific extractor (function_item / struct_item /
        // impl_item walk) lands with YGG-27 Part 2.
        whole_file_snippet(source, line_offsets)
    }

    fn build_header(&self, node: Node, source: &str) -> Option<HeaderModel> {
        rust::build_header(node, source)
    }
}
