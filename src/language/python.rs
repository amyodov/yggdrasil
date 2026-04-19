//! Python LanguageModule — wraps the existing Python-specific code
//! (tree-sitter grammar, `cards::extract_cards`, `header::python::build_header`)
//! behind the language-agnostic trait.

use tree_sitter::{Language, Node, Tree};

use super::LanguageModule;
use crate::cards::{extract_cards, Card};
use crate::header::{python, HeaderModel};

/// Zero-sized module singleton. The `LanguageModule` trait is
/// implemented on the type itself (no fields), so the registry can
/// hold a `&'static` pointer without a heap allocation.
pub struct PythonModule;

impl LanguageModule for PythonModule {
    fn name(&self) -> &'static str {
        "Python"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }

    fn grammar(&self) -> Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn highlights_query(&self) -> &'static str {
        tree_sitter_python::HIGHLIGHTS_QUERY
    }

    fn extract_cards(&self, tree: &Tree, source: &str, line_offsets: &[usize]) -> Vec<Card> {
        extract_cards(tree, source, line_offsets)
    }

    fn build_header(&self, node: Node, source: &str) -> Option<HeaderModel> {
        python::build_header(node, source)
    }
}
