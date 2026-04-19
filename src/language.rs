//! Language registry (M6.0a, Part 1).
//!
//! One trait, one static instance per supported language, one
//! dispatcher that picks the right module from a file extension.
//! Adding a new language becomes a three-line operation: implement
//! the trait, add the instance to `REGISTRY`, and the rest of the
//! code finds it through `language::for_path` — no further edits
//! across syntax.rs, card extraction, or main dispatch.
//!
//! **Part 1 scope** — architectural seam only. `extract_cards`
//! remains Python-specific (Rust and Markdown implementations return
//! an empty Vec with a documented "awaiting port" note). The tagged
//! `CardKind` rework — the part that lets Rust traits render on
//! blueprint paper and HTML elements get their own visual — lands as
//! YGG-27 Part 2 and is marked explicitly below.

use std::path::Path;

use tree_sitter::{Language, Node, Tree};

use crate::cards::Card;
use crate::header::HeaderModel;

/// One pluggable language implementation. Every module is a
/// zero-sized struct whose single static instance answers all the
/// per-language questions the rest of the codebase needs.
pub trait LanguageModule: Send + Sync {
    /// Display name (used in error messages).
    fn name(&self) -> &'static str;

    /// File extensions this language claims, without the leading dot.
    /// The first entry is treated as the canonical one for display.
    fn extensions(&self) -> &'static [&'static str];

    /// The tree-sitter grammar this module parses with. Returning a
    /// fresh `Language` on every call is cheap — these are small
    /// value types internally.
    fn grammar(&self) -> Language;

    /// Tree-sitter highlight-query source. Empty string is a valid
    /// return value for languages that don't ship a highlight query
    /// (Markdown today); the resulting token stream is all-Default.
    fn highlights_query(&self) -> &'static str;

    /// Walk a parsed tree and return a flat `Vec<Card>`. Per-module;
    /// different languages have different structural idioms
    /// (imperative def/class vs. Markdown section hierarchy vs.
    /// JSON key-tree).
    fn extract_cards(&self, tree: &Tree, source: &str, line_offsets: &[usize]) -> Vec<Card>;

    /// Build a `HeaderModel` from a definition node. Used by the
    /// renderer (future) and by M3.5b's reflow engine. No current
    /// caller routes through the trait yet — the eyes-on wiring of
    /// `HeaderModel` into the renderer (M3.5a Part 2) will be the
    /// first consumer.
    #[allow(dead_code)]
    fn build_header(&self, node: Node, source: &str) -> Option<HeaderModel>;
}

/// Look up a module by file extension (no leading dot, case-insensitive).
#[allow(dead_code)] // Used by `for_path`; keep public for direct calls too.
pub fn for_extension(ext: &str) -> Option<&'static dyn LanguageModule> {
    let lower = ext.to_ascii_lowercase();
    REGISTRY
        .iter()
        .copied()
        .find(|m| m.extensions().iter().any(|&e| e == lower.as_str()))
}

/// Look up a module for a file path. Returns `None` when the path
/// has no extension or no registered module matches — the caller
/// decides how to surface that (error in main.rs, fall through in
/// future per-file logic).
pub fn for_path(path: &Path) -> Option<&'static dyn LanguageModule> {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(for_extension)
}

/// The static list of every registered language module. Order is
/// display order; extension matching is exact (first-match-wins is
/// irrelevant here because each extension appears in one module).
const REGISTRY: &[&(dyn LanguageModule + 'static)] = &[
    &python::PythonModule,
    &rust::RustModule,
    &markdown::MarkdownModule,
];

pub mod markdown;
pub mod python;
pub mod rust;

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::path::PathBuf;

    #[rstest]
    #[case::py("py", Some("Python"))]
    #[case::py_upper("PY", Some("Python"))]
    #[case::rs("rs", Some("Rust"))]
    #[case::md("md", Some("Markdown"))]
    #[case::markdown("markdown", Some("Markdown"))]
    #[case::unknown("xyz", None)]
    fn extension_dispatch(#[case] ext: &str, #[case] expected: Option<&str>) {
        let got = for_extension(ext).map(|m| m.name());
        assert_eq!(got, expected);
    }

    #[rstest]
    #[case::py_path("foo.py", Some("Python"))]
    #[case::rs_path("src/main.rs", Some("Rust"))]
    #[case::md_path("README.md", Some("Markdown"))]
    #[case::no_extension("Makefile", None)]
    #[case::empty_path("", None)]
    fn path_dispatch(#[case] path: &str, #[case] expected: Option<&str>) {
        let got = for_path(&PathBuf::from(path)).map(|m| m.name());
        assert_eq!(got, expected);
    }

    /// Every registered module should round-trip through its own
    /// canonical extension — catches the "registered but unreachable"
    /// class of bug in a one-liner.
    #[test]
    fn every_module_is_reachable_by_its_canonical_extension() {
        for &m in REGISTRY {
            let ext = m.extensions().first().copied().expect("at least one extension");
            let found = for_extension(ext).expect("roundtrip");
            assert_eq!(found.name(), m.name());
        }
    }
}
