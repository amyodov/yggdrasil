//! Rust builder: tree-sitter-rust AST → `HeaderModel`.
//!
//! Second language after Python. Validates that the ADT is actually
//! language-agnostic by going through a second grammar.
//!
//! **Attribute extraction is deferred.** Tree-sitter-rust reports
//! `#[derive(...)]` / `#[inline]` as preceding sibling `attribute_item`
//! nodes, not as children of the item. Gathering them belongs in the
//! caller (future `extract_cards` for Rust) which can walk sibling
//! context; the builder as defined here only sees the item node.
//! When attribute support lands, this module's signature will grow an
//! `attributes: &[Node]` parameter rather than start walking siblings
//! behind the builder's back.
//!
//! **Docstring extraction is also deferred.** Rust's `///` doc
//! comments are preceding `line_comment` siblings (like attributes).
//! Same treatment applies.

use tree_sitter::Node;

use super::{HeaderModel, KeywordBadge, ParamChip, ParamKind, Prelude, TypeChip};

/// Build a `HeaderModel` from a Rust item node. Accepts:
/// `function_item`, `struct_item`, `enum_item`, `trait_item`,
/// `impl_item`, `mod_item`. Returns `None` for any other node kind.
#[allow(dead_code)] // Consumer wiring arrives under eyes-on review.
pub fn build_header(node: Node, source: &str) -> Option<HeaderModel> {
    let keyword = match node.kind() {
        "function_item" => KeywordBadge::Fn,
        "struct_item" => KeywordBadge::Struct,
        "enum_item" => KeywordBadge::Enum,
        "trait_item" => KeywordBadge::Trait,
        "impl_item" => KeywordBadge::Impl,
        "mod_item" => KeywordBadge::Mod,
        _ => return None,
    };

    let name = extract_name(node, source, keyword);
    let params = if matches!(keyword, KeywordBadge::Fn) {
        parse_parameters(node, source)
    } else {
        Vec::new()
    };
    let return_type = if matches!(keyword, KeywordBadge::Fn) {
        node.child_by_field_name("return_type")
            .map(|n| TypeChip { text: node_text(n, source).to_string() })
    } else {
        None
    };

    Some(HeaderModel {
        prelude: Prelude { decorators: Vec::new(), keyword },
        name,
        params,
        return_type,
        docstring: None,
    })
}

/// Extract the displayed name for a given item kind. Most kinds have
/// a simple `name` field; `impl_item` is the odd one out — it's
/// either `Type` for inherent impls or `Trait for Type` for trait
/// impls. We surface the trait-for-type form so the card label
/// matches what the Rust programmer typed.
fn extract_name(node: Node, source: &str, keyword: KeywordBadge) -> String {
    if matches!(keyword, KeywordBadge::Impl) {
        let type_node = node.child_by_field_name("type");
        let trait_node = node.child_by_field_name("trait");
        return match (trait_node, type_node) {
            (Some(t), Some(ty)) => {
                format!("{} for {}", node_text(t, source), node_text(ty, source))
            }
            (None, Some(ty)) => node_text(ty, source).to_string(),
            _ => String::new(),
        };
    }
    node.child_by_field_name("name")
        .map(|n| node_text(n, source).to_string())
        .unwrap_or_default()
}

/// Walk a function's `parameters` list and emit one `ParamChip` per
/// parameter. Rust has fewer parameter shapes than Python: a regular
/// `pattern: type`, a `self` / `&self` / `&mut self` receiver, and
/// the rare C-variadic `...`. Keyword-only / positional-only dividers
/// don't exist, so `ParamKind::Regular` is the only kind emitted here.
fn parse_parameters(fn_node: Node, source: &str) -> Vec<ParamChip> {
    let Some(params_node) = fn_node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = params_node.walk();
    for c in params_node.children(&mut cursor) {
        match c.kind() {
            "self_parameter" => {
                // Render verbatim: `self`, `&self`, `&mut self`. The
                // source text is already exactly what we want to show.
                let text = node_text(c, source).to_string();
                out.push(ParamChip {
                    name: text,
                    ty: None,
                    default: None,
                    kind: ParamKind::Regular,
                });
            }
            "parameter" => {
                let name = c
                    .child_by_field_name("pattern")
                    .map(|p| node_text(p, source).to_string())
                    .unwrap_or_default();
                let ty = c
                    .child_by_field_name("type")
                    .map(|t| TypeChip { text: node_text(t, source).to_string() });
                out.push(ParamChip { name, ty, default: None, kind: ParamKind::Regular });
            }
            "variadic_parameter" => {
                // C-ABI `...` — render as a Star chip with empty name,
                // matching how Python treats a bare `*` separator.
                out.push(ParamChip {
                    name: String::new(),
                    ty: None,
                    default: None,
                    kind: ParamKind::Star,
                });
            }
            _ => {}
        }
    }
    out
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
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    /// Build a HeaderModel for the first rust item at the tree root.
    fn header_of(src: &str) -> HeaderModel {
        let tree = parse(src);
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if let Some(h) = build_header(child, src) {
                return h;
            }
        }
        panic!("no item node found at root in test source:\n{src}");
    }

    // ---- Keyword + name for each item kind ----------------------

    #[rstest]
    #[case::fn_plain("fn greet() {}\n", KeywordBadge::Fn, "greet")]
    #[case::pub_fn("pub fn open() {}\n", KeywordBadge::Fn, "open")]
    #[case::plain_struct("struct Widget;\n", KeywordBadge::Struct, "Widget")]
    #[case::pub_struct(
        "pub struct Widget { x: i32 }\n",
        KeywordBadge::Struct,
        "Widget",
    )]
    #[case::enum_(
        "enum Color { Red, Green, Blue }\n",
        KeywordBadge::Enum,
        "Color",
    )]
    #[case::trait_(
        "trait Render { fn draw(&self); }\n",
        KeywordBadge::Trait,
        "Render",
    )]
    #[case::module("mod utils {}\n", KeywordBadge::Mod, "utils")]
    fn keyword_and_name(
        #[case] src: &str,
        #[case] expected_kw: KeywordBadge,
        #[case] expected_name: &str,
    ) {
        let h = header_of(src);
        assert_eq!(h.prelude.keyword, expected_kw);
        assert_eq!(h.name, expected_name);
    }

    // ---- impl — special name formatting -------------------------

    #[rstest]
    #[case::inherent(
        "impl Widget { fn open(&self) {} }\n",
        KeywordBadge::Impl,
        "Widget",
    )]
    #[case::trait_impl(
        "impl Render for Widget { fn draw(&self) {} }\n",
        KeywordBadge::Impl,
        "Render for Widget",
    )]
    fn impl_name(
        #[case] src: &str,
        #[case] expected_kw: KeywordBadge,
        #[case] expected_name: &str,
    ) {
        let h = header_of(src);
        assert_eq!(h.prelude.keyword, expected_kw);
        assert_eq!(h.name, expected_name);
    }

    // ---- Function parameters ------------------------------------

    fn param_view(p: &ParamChip) -> (&str, Option<&str>, ParamKind) {
        (p.name.as_str(), p.ty.as_ref().map(|t| t.text.as_str()), p.kind)
    }

    #[rstest]
    #[case::no_params("fn f() {}\n", vec![])]
    #[case::one_param(
        "fn f(x: i32) {}\n",
        vec![("x", Some("i32"), ParamKind::Regular)],
    )]
    #[case::self_receiver(
        "impl Foo { fn m(&self) {} }\n",
        // Navigating into the impl body is beyond this test helper; we
        // special-case the body inspection below.
        vec![],
    )]
    #[case::mixed_params(
        "fn sum(a: i32, b: i32, c: i32) -> i32 { a + b + c }\n",
        vec![
            ("a", Some("i32"), ParamKind::Regular),
            ("b", Some("i32"), ParamKind::Regular),
            ("c", Some("i32"), ParamKind::Regular),
        ],
    )]
    fn params_bare_functions(
        #[case] src: &str,
        #[case] expected: Vec<(&str, Option<&str>, ParamKind)>,
    ) {
        // For the self_receiver case, header_of yields the impl's
        // header which has no params (correct — params belong to the
        // method inside). We separately verify self-handling below.
        let h = header_of(src);
        let got: Vec<_> = h.params.iter().map(param_view).collect();
        assert_eq!(got, expected);
    }

    /// Method inside an impl — builder receives the `function_item`
    /// node directly (what the real extractor will pass).
    #[test]
    fn method_with_self_receiver() {
        let src = "impl Foo { fn bar(&mut self, x: i32) -> bool { true } }\n";
        let tree = parse(src);
        // Walk: source_file → impl_item → declaration_list → function_item.
        let root = tree.root_node();
        let impl_item = root.child(0).unwrap();
        let body = impl_item.child_by_field_name("body").unwrap();
        let mut cursor = body.walk();
        let fn_item = body
            .children(&mut cursor)
            .find(|c| c.kind() == "function_item")
            .unwrap();
        let h = build_header(fn_item, src).unwrap();
        assert_eq!(h.prelude.keyword, KeywordBadge::Fn);
        assert_eq!(h.name, "bar");
        let got: Vec<_> = h.params.iter().map(param_view).collect();
        assert_eq!(
            got,
            vec![
                ("&mut self", None, ParamKind::Regular),
                ("x", Some("i32"), ParamKind::Regular),
            ],
        );
        assert_eq!(h.return_type.as_ref().map(|t| t.text.as_str()), Some("bool"));
    }

    // ---- Return type --------------------------------------------

    #[rstest]
    #[case::no_return("fn f() {}\n", None)]
    #[case::simple("fn f() -> i32 { 0 }\n", Some("i32"))]
    #[case::generic(
        "fn f() -> Option<Vec<String>> { None }\n",
        Some("Option<Vec<String>>"),
    )]
    #[case::result(
        "fn f() -> Result<(), Error> { Ok(()) }\n",
        Some("Result<(), Error>"),
    )]
    fn return_type(#[case] src: &str, #[case] expected: Option<&str>) {
        let h = header_of(src);
        let got = h.return_type.as_ref().map(|t| t.text.as_str());
        assert_eq!(got, expected);
    }

    // ---- Non-item nodes return None -----------------------------

    #[test]
    fn non_item_node_yields_none() {
        let src = "const X: i32 = 5;\n"; // const_item is unsupported (yet).
        let tree = parse(src);
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "const_item" {
                assert!(build_header(child, src).is_none());
            }
        }
    }
}
