//! Rust LanguageModule.
//!
//! `extract_cards` walks the top-level items of a Rust file, building
//! Cards that line up with Python's spine/armature conventions:
//!
//! - `function_item`: rendered as `Function` at module top-level, or
//!   `Method` when nested inside an `impl` / `trait` (where it sits
//!   visually on the parent's armature).
//! - `struct_item`, `enum_item`: leaf cards with the whole body as
//!   preview — no collapsible children in this first pass.
//! - `trait_item`, `impl_item`, `mod_item`: container cards, rendered
//!   as `Class` for now so they pick up the existing armature spine.
//!   Contained `function_item`s become `Method` children.
//!
//! Reusing `CardKind::Class` for Rust's containers is a pragmatic
//! shortcut. Proper per-kind visuals (struct fields rendered as
//! chip-columns, trait on blueprint paper, impl blocks attached to
//! their target type) wait for YGG-27 Part 2's tagged CardKind
//! rework; today those containers just get the same class-armature
//! rendering as a Python class.
//!
//! Attributes (`#[derive(...)]`) and doc comments (`///`) are
//! preceding-sibling nodes in Rust — not children of the item. We
//! leave them on the floor for now; the header builder's `decorators`
//! field stays empty. When attribute extraction lands, the sibling
//! walk happens here, not inside the header builder.

use tree_sitter::{Language, Node, Tree};

use super::LanguageModule;
use crate::cards::{build_card, Card, CardId, CardKind, Visibility};
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

    fn extract_cards(&self, tree: &Tree, source: &str, line_offsets: &[usize]) -> Vec<Card> {
        let mut out = Vec::new();
        let mut next_id: u32 = 0;
        let mut cursor = tree.root_node().walk();
        for child in tree.root_node().children(&mut cursor) {
            process_item(child, source, line_offsets, None, 0, &mut out, &mut next_id);
        }
        out
    }

    fn build_header(&self, node: Node, source: &str) -> Option<HeaderModel> {
        rust::build_header(node, source)
    }
}

/// Process one Rust AST node at some nesting level. Non-item nodes
/// (comments, attributes, use declarations, expressions) are skipped
/// silently — they aren't Cards in M3's model.
fn process_item(
    node: Node,
    source: &str,
    line_offsets: &[usize],
    parent: Option<CardId>,
    depth: usize,
    out: &mut Vec<Card>,
    next_id: &mut u32,
) {
    let (kind, is_container) = match node.kind() {
        // `function_item` has a body (`fn f() { … }`);
        // `function_signature_item` is a trait's abstract method declaration
        // (`fn f();`) — body is absent but the card is still a Method.
        "function_item" | "function_signature_item" => {
            let kind = if parent.is_some() { CardKind::Method } else { CardKind::Function };
            (kind, false)
        }
        // Containers — reuse CardKind::Class so they pick up the
        // existing armature + child-attachment rendering. A
        // finer-grained visual treatment waits for the tagged
        // CardKind rework.
        "trait_item" | "impl_item" | "mod_item" => (CardKind::Class, true),
        // Leaf data types — treat as a class today so the body
        // renders on its own card without children. The armature
        // still shows; a future pass can hide it for leaf kinds.
        "struct_item" | "enum_item" => (CardKind::Class, false),
        _ => return,
    };

    let name = item_name(node, source);
    let visibility = item_visibility(node, source);

    // Body: every container carries a `body` field (declaration_list
    // for impl/trait/mod, field_declaration_list / enum_variant_list
    // for struct/enum). Functions also carry one (block). If a node
    // has none — e.g. a struct declared as a unit-struct `struct S;`
    // — body_range is None and layout treats it as header-only.
    let body_range = node.child_by_field_name("body").map(|b| b.byte_range());
    let body_start = body_range.as_ref().map(|b| b.start);
    let full_range = node.byte_range();
    let header_end = body_start.unwrap_or(full_range.end);
    let header_range = full_range.start..header_end;

    let id = CardId(*next_id);
    *next_id += 1;
    out.push(build_card(
        id,
        kind,
        parent,
        depth,
        name,
        visibility,
        header_range,
        body_range,
        full_range,
        line_offsets,
    ));

    // Recurse into containers for nested items. Functions inside an
    // impl/trait become Methods; nested mods produce their own
    // container card; nested functions inside an mod body become
    // Functions at the deeper depth.
    if is_container {
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for inner in body.children(&mut cursor) {
                process_item(inner, source, line_offsets, Some(id), depth + 1, out, next_id);
            }
        }
    }
}

/// Name shown on the card header. For most items this is the `name`
/// field directly. `impl Trait for Type` stitches the trait and type
/// together so the card's label matches the source line.
fn item_name(node: Node, source: &str) -> String {
    if node.kind() == "impl_item" {
        let t = node
            .child_by_field_name("trait")
            .map(|n| node_text(n, source));
        let ty = node
            .child_by_field_name("type")
            .map(|n| node_text(n, source));
        return match (t, ty) {
            (Some(t), Some(ty)) => format!("{t} for {ty}"),
            (None, Some(ty)) => ty.to_string(),
            _ => String::new(),
        };
    }
    node.child_by_field_name("name")
        .map(|n| node_text(n, source).to_string())
        .unwrap_or_default()
}

/// `pub`, `pub(crate)`, `pub(super)`, `pub(in …)` all → Public.
/// Anything else → Private. Rust's visibility is explicit (unlike
/// Python's underscore convention), which makes this a trivial
/// look-for-a-child check.
fn item_visibility(node: Node, _source: &str) -> Visibility {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "visibility_modifier" {
            return Visibility::Public;
        }
    }
    Visibility::Private
}

fn node_text<'s>(node: Node, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
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
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let offs = line_offsets(src);
        RustModule.extract_cards(&tree, src, &offs)
    }

    fn sig(c: &Card) -> (CardKind, &str, Visibility, usize) {
        (c.kind, c.name.as_str(), c.visibility, c.depth)
    }

    // ---- Top-level items get the right CardKind --------------

    #[rstest]
    #[case::fn_pub(
        "pub fn greet() {}\n",
        &[(CardKind::Function, "greet", Visibility::Public, 0)],
    )]
    #[case::fn_private(
        "fn helper() {}\n",
        &[(CardKind::Function, "helper", Visibility::Private, 0)],
    )]
    #[case::struct_(
        "pub struct Widget { x: i32 }\n",
        &[(CardKind::Class, "Widget", Visibility::Public, 0)],
    )]
    #[case::enum_(
        "enum Color { Red, Green }\n",
        &[(CardKind::Class, "Color", Visibility::Private, 0)],
    )]
    #[case::trait_(
        "pub trait Render { fn draw(&self); }\n",
        &[
            (CardKind::Class, "Render", Visibility::Public, 0),
            // The trait's method declaration shows up as a Method child.
            (CardKind::Method, "draw", Visibility::Private, 1),
        ],
    )]
    #[case::mod_(
        "mod utils { pub fn parse() {} }\n",
        &[
            (CardKind::Class, "utils", Visibility::Private, 0),
            (CardKind::Method, "parse", Visibility::Public, 1),
        ],
    )]
    fn top_level_items(
        #[case] src: &str,
        #[case] expected: &[(CardKind, &str, Visibility, usize)],
    ) {
        let cards = extract(src);
        let got: Vec<_> = cards.iter().map(sig).collect();
        let want: Vec<_> = expected.iter().map(|&(k, n, v, d)| (k, n, v, d)).collect();
        assert_eq!(got, want);
    }

    // ---- impl blocks name themselves "Trait for Type" --------

    #[test]
    fn impl_block_with_trait_for_type_name() {
        let cards = extract(
            "impl Render for Widget { pub fn draw(&self) {} fn helper(&self) {} }\n",
        );
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].kind, CardKind::Class);
        assert_eq!(cards[0].name, "Render for Widget");
        assert_eq!(cards[0].depth, 0);
        // Children are Methods of the impl.
        assert_eq!(cards[1].kind, CardKind::Method);
        assert_eq!(cards[1].name, "draw");
        assert_eq!(cards[1].visibility, Visibility::Public);
        assert_eq!(cards[1].depth, 1);
        assert_eq!(cards[2].kind, CardKind::Method);
        assert_eq!(cards[2].name, "helper");
        assert_eq!(cards[2].visibility, Visibility::Private);
        assert_eq!(cards[2].parent, Some(cards[0].id));
    }

    #[test]
    fn impl_inherent_uses_type_name() {
        let cards = extract("impl Widget { fn new() -> Self { Widget } }\n");
        assert_eq!(cards[0].name, "Widget");
    }

    // ---- Nested mod produces its own depth -------------------

    #[test]
    fn nested_mod_deepens_children() {
        let cards = extract("mod a { mod b { fn c() {} } }\n");
        assert_eq!(cards.len(), 3);
        assert_eq!((cards[0].depth, cards[0].name.as_str()), (0, "a"));
        assert_eq!((cards[1].depth, cards[1].name.as_str()), (1, "b"));
        assert_eq!((cards[2].depth, cards[2].name.as_str()), (2, "c"));
        assert_eq!(cards[2].kind, CardKind::Method);
    }

    // ---- Non-item nodes are skipped --------------------------

    #[test]
    fn use_and_const_and_comments_skipped() {
        let cards = extract("use std::io;\nconst X: i32 = 5;\n// comment\nfn main() {}\n");
        let names: Vec<&str> = cards.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["main"]);
    }

    // ---- Unit struct (no body) yields a header-only card -----

    #[test]
    fn unit_struct_has_no_body() {
        let cards = extract("struct Marker;\n");
        assert_eq!(cards.len(), 1);
        assert!(cards[0].body_range.is_none());
    }
}
