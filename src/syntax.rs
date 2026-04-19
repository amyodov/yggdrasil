//! Syntax highlighting — tree-sitter + a luminous-void theme.
//!
//! Grammar and highlight query are supplied by a `LanguageModule`
//! (see `src/language.rs`), so this file is language-agnostic. The
//! token-kind mapping from capture names to `TokenKind` stays here
//! because the palette is shared across languages.
//!
//! The output is a per-byte `Vec<TokenKind>` rather than a list of spans.
//! This is deliberate:
//! - Tree-sitter captures overlap (an outer `function_definition` plus an
//!   inner `identifier`); we need a flat view.
//! - The renderer slices the byte-kinds vector by line for virtualization,
//!   then compresses into glyphon `(text, Attrs)` spans per buffer build.
//! - Memory is 1 byte per source byte — trivial at prototype scale.

use std::ops::Range;

use anyhow::Result;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator, Tree};

use crate::language::LanguageModule;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TokenKind {
    Default,
    Keyword,
    String,
    EscapeInString,
    Comment,
    Number,
    Operator,
    Function,
    FunctionBuiltin,
    Class,
    Constant,
    ConstantBuiltin,
    Type,
    Property,
    Punctuation,
}

/// A parser + highlight query pair for one language. Stateful: reuse across
/// parses to keep the parser's internal scratch buffers warm (M7 will use
/// `Tree::edit` for incremental reparse).
pub struct Highlighter {
    parser: Parser,
    query: Query,
    /// `capture_kind[capture_index]` → TokenKind for that capture in the query.
    capture_kind: Vec<TokenKind>,
}

impl Highlighter {
    /// Build a Highlighter from a language module. The module
    /// supplies both the tree-sitter grammar and the highlight query;
    /// everything else (capture-name → TokenKind mapping) stays
    /// shared.
    pub fn new_for_language(module: &dyn LanguageModule) -> Result<Self> {
        let language = module.grammar();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let query_source = module.highlights_query();
        let query = Query::new(&language, query_source)?;
        let capture_kind: Vec<TokenKind> = query
            .capture_names()
            .iter()
            .map(|name| TokenKind::from_capture_name(name))
            .collect();

        Ok(Self { parser, query, capture_kind })
    }

    /// Convenience for tests: build a Python Highlighter directly.
    /// Production code uses `new_for_language` with the module from
    /// the registry.
    #[cfg(test)]
    pub fn new_python() -> Result<Self> {
        Self::new_for_language(&crate::language::python::PythonModule)
    }

    /// Parse `source` into a tree-sitter `Tree`. Returns `None` if parsing
    /// failed outright (the parser gave up — should be very rare).
    pub fn parse(&mut self, source: &str) -> Option<Tree> {
        self.parser.parse(source, None)
    }

    /// Compute a flat `TokenKind`-per-byte vector from an already-parsed tree.
    /// Bytes not covered by any non-default capture stay `Default`.
    ///
    /// The tree is taken by reference so the caller (M3: also card extraction)
    /// can consume it for multiple purposes without re-parsing.
    pub fn highlight_tree(&self, tree: &Tree, source: &str) -> Vec<TokenKind> {
        let mut kinds = vec![TokenKind::Default; source.len()];

        // Collect every capture first so we can apply them outermost-to-innermost.
        // Tree-sitter doesn't guarantee iteration order matches nesting, so we
        // sort explicitly: widest range first → narrower ranges overwrite,
        // leaving the most specific kind at each byte.
        let mut cursor = QueryCursor::new();
        let mut captures = cursor.captures(&self.query, tree.root_node(), source.as_bytes());

        let mut spans: Vec<(Range<usize>, TokenKind)> = Vec::new();
        while let Some((m, cap_idx)) = captures.next() {
            let cap = m.captures[*cap_idx];
            let r = cap.node.byte_range();
            if r.start >= r.end || r.end > source.len() {
                continue;
            }
            let kind = self.capture_kind[cap.index as usize];
            if kind == TokenKind::Default {
                continue;
            }
            spans.push((r, kind));
        }

        spans.sort_by_key(|(r, _)| std::cmp::Reverse(r.end - r.start));
        for (r, kind) in spans {
            kinds[r.start..r.end].fill(kind);
        }
        kinds
    }

    /// Convenience: parse + highlight in one shot. Used by tests and any
    /// caller that doesn't also want the `Tree`.
    #[cfg(test)]
    pub fn highlight(&mut self, source: &str) -> Vec<TokenKind> {
        match self.parse(source) {
            Some(tree) => self.highlight_tree(&tree, source),
            None => vec![TokenKind::Default; source.len()],
        }
    }
}

impl TokenKind {
    /// Map a tree-sitter highlight capture name (e.g. `keyword`,
    /// `function.builtin`) to a token kind. Unknown names fall back to
    /// `Default`, which means "no override".
    fn from_capture_name(name: &str) -> Self {
        // Dotted suffixes we care about go first; the base form is the fallback.
        match name {
            "function.builtin" => TokenKind::FunctionBuiltin,
            "function.method" => TokenKind::Function,
            "constant.builtin" => TokenKind::ConstantBuiltin,
            "punctuation.special" => TokenKind::Punctuation,
            _ => match name.split('.').next().unwrap_or(name) {
                "keyword" => TokenKind::Keyword,
                "string" => TokenKind::String,
                "escape" => TokenKind::EscapeInString,
                "comment" => TokenKind::Comment,
                "number" => TokenKind::Number,
                "operator" => TokenKind::Operator,
                "function" => TokenKind::Function,
                "constructor" => TokenKind::Class,
                "constant" => TokenKind::Constant,
                "type" => TokenKind::Type,
                "property" => TokenKind::Property,
                "punctuation" => TokenKind::Punctuation,
                // Known-but-unstyled captures ("variable", "embedded", ...).
                _ => TokenKind::Default,
            },
        }
    }

    /// sRGB color for this token kind, under the luminous-void palette.
    /// Chosen by eye to sit over a near-black background: keywords/functions
    /// punch through; comments and operators sink; strings sit warm.
    pub fn color(self) -> (u8, u8, u8) {
        match self {
            TokenKind::Default => (220, 222, 230),
            TokenKind::Keyword => (116, 199, 236),
            TokenKind::String => (230, 190, 110),
            TokenKind::EscapeInString => (255, 220, 150),
            TokenKind::Comment => (100, 115, 140),
            TokenKind::Number => (158, 226, 176),
            TokenKind::Operator => (160, 180, 200),
            TokenKind::Function => (210, 180, 255),
            TokenKind::FunctionBuiltin => (180, 200, 255),
            TokenKind::Class => (255, 200, 170),
            TokenKind::Constant => (255, 180, 120),
            TokenKind::ConstantBuiltin => (200, 160, 220),
            TokenKind::Type => (140, 220, 220),
            TokenKind::Property => (210, 210, 240),
            TokenKind::Punctuation => (170, 170, 185),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// One parameterized test per concern — each case asks "given this
    /// snippet, does the byte range `[start, end)` resolve to the expected
    /// kind?" We pick *one* representative byte of the substring of interest
    /// (its first byte) to keep assertions focused; the fill invariant is
    /// checked separately by the byte-length-matches test.
    ///
    /// Cases cover every category the M2 spec explicitly calls out:
    /// keywords, strings, comments, decorators, type annotations, plus a
    /// couple of obvious extras (numbers, functions, constructors).
    #[rstest]
    #[case::keyword_def("def foo():\n    pass\n", "def", TokenKind::Keyword)]
    #[case::keyword_class("class Foo:\n    pass\n", "class", TokenKind::Keyword)]
    #[case::keyword_if("if x: pass\n", "if", TokenKind::Keyword)]
    #[case::function_name("def greet(n): return n\n", "greet", TokenKind::Function)]
    #[case::class_name("class Widget: pass\n", "Widget", TokenKind::Class)]
    #[case::string_single("x = 'hello'\n", "'hello'", TokenKind::String)]
    #[case::string_double("x = \"hi\"\n", "\"hi\"", TokenKind::String)]
    #[case::number_int("x = 42\n", "42", TokenKind::Number)]
    #[case::number_float("x = 3.14\n", "3.14", TokenKind::Number)]
    #[case::comment_line("# note\nx = 1\n", "# note", TokenKind::Comment)]
    #[case::type_annotation("def f(x: int) -> int: return x\n", "int", TokenKind::Type)]
    #[case::decorator_as_function("@classmethod\ndef f(cls): pass\n", "classmethod", TokenKind::Function)]
    #[case::constant_builtin_none("x = None\n", "None", TokenKind::ConstantBuiltin)]
    #[case::operator_plus("x = 1 + 2\n", "+", TokenKind::Operator)]
    fn highlights_python(#[case] source: &str, #[case] needle: &str, #[case] expected: TokenKind) {
        let mut h = Highlighter::new_python().expect("parser loads");
        let kinds = h.highlight(source);

        assert_eq!(kinds.len(), source.len(), "kinds vec matches source byte length");

        let start = source
            .find(needle)
            .unwrap_or_else(|| panic!("needle {:?} not in source", needle));
        let end = start + needle.len();

        // Assert every byte of the substring has the expected kind — catches
        // the case where a sub-range (e.g. an inner identifier) leaks through.
        for i in start..end {
            assert_eq!(
                kinds[i], expected,
                "byte {i} of {:?} (char {:?}) expected {expected:?}, got {:?}",
                source,
                &source[i..i + 1],
                kinds[i],
            );
        }
    }
}
