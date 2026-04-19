//! Python builder: turns a tree-sitter `function_definition` /
//! `class_definition` / `decorated_definition` node into a
//! `HeaderModel`. Maps Python's AST shapes onto the language-agnostic
//! ADT defined in the parent module.
//!
//! The builder is deliberately tolerant: a malformed or unexpected AST
//! shape (missing name, missing parameters list) collapses into an
//! empty-ish HeaderModel rather than panicking. Tree-sitter produces
//! partial trees for broken syntax and we'd rather render a degraded
//! header than crash.

use tree_sitter::Node;

use super::{
    Docstring, DecoratorChip, HeaderModel, KeywordBadge, ParamChip, ParamKind, Prelude, TypeChip,
};

/// Build a `HeaderModel` from a definition node. `node` is either
/// `function_definition`, `class_definition`, or `decorated_definition`
/// (which wraps one of the first two plus decorator siblings).
///
/// Returns `None` if `node` isn't one of those three kinds — a signal
/// for the caller to skip silently rather than render a nonsense card.
#[allow(dead_code)] // Consumer wiring arrives under eyes-on review.
pub fn build_header(node: Node, source: &str) -> Option<HeaderModel> {
    // Unwrap a decorated_definition to reach the underlying definition,
    // collecting the decorator chips in source order while we're here.
    let (def_node, decorators) = match node.kind() {
        "function_definition" | "class_definition" => (node, Vec::new()),
        "decorated_definition" => {
            let mut decorators = Vec::new();
            let mut def = None;
            let mut cursor = node.walk();
            for c in node.children(&mut cursor) {
                match c.kind() {
                    "decorator" => decorators.push(parse_decorator(c, source)),
                    "function_definition" | "class_definition" => def = Some(c),
                    _ => {}
                }
            }
            (def?, decorators)
        }
        _ => return None,
    };

    let keyword = classify_keyword(def_node);
    let name = def_node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .unwrap_or("")
        .to_string();

    let params = match def_node.kind() {
        "function_definition" => parse_parameters(def_node, source),
        _ => Vec::new(),
    };

    let return_type = def_node
        .child_by_field_name("return_type")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(|text| TypeChip { text: text.to_string() });

    let docstring = def_node
        .child_by_field_name("body")
        .and_then(|body| parse_docstring(body, source));

    Some(HeaderModel {
        prelude: Prelude { decorators, keyword },
        name,
        params,
        return_type,
        docstring,
    })
}

/// Decide which keyword badge to attach. Python's `async def` is a
/// `function_definition` whose first child token is `"async"`; plain
/// `def` has `"def"` as the first token.
fn classify_keyword(def_node: Node) -> KeywordBadge {
    match def_node.kind() {
        "class_definition" => KeywordBadge::Class,
        "function_definition" => {
            // Walk named + anonymous children to find the first token.
            // The grammar puts `"async"` (if present) before `"def"`.
            let mut cursor = def_node.walk();
            for c in def_node.children(&mut cursor) {
                if c.kind() == "async" {
                    return KeywordBadge::AsyncDef;
                }
                if c.kind() == "def" {
                    return KeywordBadge::Def;
                }
            }
            KeywordBadge::Def
        }
        _ => KeywordBadge::Def,
    }
}

/// Decorator text with `@` and any call arguments stripped; dotted
/// identifiers preserved. Mirrors `cards::extract_decorators` so both
/// paths see the same shape.
fn parse_decorator(node: Node, source: &str) -> DecoratorChip {
    let raw = node.utf8_text(source.as_bytes()).unwrap_or("");
    let stripped = raw.trim_start_matches('@').trim();
    let dotted = stripped.split('(').next().unwrap_or(stripped).trim();
    DecoratorChip { text: dotted.to_string() }
}

/// Walk the `parameters` list, emitting a chip per parameter or
/// separator token. Punctuation (`(`, `)`, `,`) and errors are
/// skipped.
fn parse_parameters(def_node: Node, source: &str) -> Vec<ParamChip> {
    let Some(params_node) = def_node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = params_node.walk();
    for c in params_node.children(&mut cursor) {
        match c.kind() {
            "identifier" => {
                let name = text(c, source).to_string();
                out.push(ParamChip { name, ty: None, default: None, kind: ParamKind::Regular });
            }
            "typed_parameter" => {
                // Shape: identifier ':' type
                let name = first_child_kind(c, "identifier")
                    .map(|n| text(n, source).to_string())
                    .unwrap_or_default();
                let ty = c
                    .child_by_field_name("type")
                    .map(|n| TypeChip { text: text(n, source).to_string() });
                out.push(ParamChip { name, ty, default: None, kind: ParamKind::Regular });
            }
            "default_parameter" => {
                // Shape: name '=' value  (field-named by grammar)
                let name = c
                    .child_by_field_name("name")
                    .map(|n| text(n, source).to_string())
                    .unwrap_or_default();
                let default = c
                    .child_by_field_name("value")
                    .map(|n| text(n, source).to_string());
                out.push(ParamChip { name, ty: None, default, kind: ParamKind::Regular });
            }
            "typed_default_parameter" => {
                let name = c
                    .child_by_field_name("name")
                    .map(|n| text(n, source).to_string())
                    .unwrap_or_default();
                let ty = c
                    .child_by_field_name("type")
                    .map(|n| TypeChip { text: text(n, source).to_string() });
                let default = c
                    .child_by_field_name("value")
                    .map(|n| text(n, source).to_string());
                out.push(ParamChip { name, ty, default, kind: ParamKind::Regular });
            }
            "list_splat_pattern" => {
                // *args — the pattern wraps an identifier.
                let name = first_child_kind(c, "identifier")
                    .map(|n| text(n, source).to_string())
                    .unwrap_or_default();
                out.push(ParamChip { name, ty: None, default: None, kind: ParamKind::Star });
            }
            "dictionary_splat_pattern" => {
                let name = first_child_kind(c, "identifier")
                    .map(|n| text(n, source).to_string())
                    .unwrap_or_default();
                out.push(ParamChip { name, ty: None, default: None, kind: ParamKind::Kwargs });
            }
            // Named separator nodes in tree-sitter-python.
            "keyword_separator" => out.push(ParamChip {
                name: String::new(),
                ty: None,
                default: None,
                kind: ParamKind::Star,
            }),
            "positional_separator" => out.push(ParamChip {
                name: String::new(),
                ty: None,
                default: None,
                kind: ParamKind::Slash,
            }),
            _ => {}
        }
    }
    out
}

/// Detect a leading docstring — first non-comment statement of `body`
/// whose kind is `expression_statement` wrapping a single `string`.
/// Returns the range of the expression_statement (so the byte range
/// matches `Card::docstring_range`) and the docstring literal's text.
fn parse_docstring(body: Node, source: &str) -> Option<Docstring> {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "comment" => continue,
            "expression_statement" => {
                let mut inner = child.walk();
                for inner_child in child.children(&mut inner) {
                    if inner_child.kind() == "string" {
                        let byte_range = child.byte_range();
                        let text = inner_child
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .to_string();
                        return Some(Docstring { byte_range, text });
                    }
                }
                return None;
            }
            _ => return None,
        }
    }
    None
}

/// Read a node's source text, returning an empty string on UTF-8
/// error. Tree-sitter guarantees byte-valid ranges so the only
/// failure path is a corrupted source buffer.
fn text<'s>(node: Node, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

/// Find the first child node of a given kind. Used to locate the
/// identifier inside a wrapper like `list_splat_pattern`. Written as
/// an explicit loop because the iterator form would keep the cursor
/// borrow alive across the return, which the borrow checker rejects.
#[allow(clippy::manual_find)]
fn first_child_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == kind {
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use tree_sitter::{Parser, Tree};

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    /// Build a HeaderModel for the first function/class/decorated
    /// definition found at the tree root.
    fn header_of(src: &str) -> HeaderModel {
        let tree = parse(src);
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "function_definition" | "class_definition" | "decorated_definition" => {
                    return build_header(child, src).expect("builder should accept this node");
                }
                _ => {}
            }
        }
        panic!("no definition node found at root in test source:\n{src}");
    }

    // ---- Prelude + name + keyword classification --------------------

    #[rstest]
    #[case::plain_def("def f(): pass\n", KeywordBadge::Def, "f")]
    #[case::async_def("async def g(): pass\n", KeywordBadge::AsyncDef, "g")]
    #[case::plain_class("class Widget: pass\n", KeywordBadge::Class, "Widget")]
    fn keyword_and_name(
        #[case] src: &str,
        #[case] expected_kw: KeywordBadge,
        #[case] expected_name: &str,
    ) {
        let h = header_of(src);
        assert_eq!(h.prelude.keyword, expected_kw);
        assert_eq!(h.name, expected_name);
        assert!(h.prelude.decorators.is_empty());
    }

    // ---- Decorators preserved in source order -----------------------

    #[rstest]
    #[case::single("@decor\ndef f(): pass\n", &["decor"])]
    #[case::with_args(
        "@functools.wraps(original)\ndef f(): pass\n",
        &["functools.wraps"],
    )]
    #[case::dotted_no_args(
        "@abc.abstractmethod\ndef f(): pass\n",
        &["abc.abstractmethod"],
    )]
    #[case::stacked(
        "@first\n@second\n@third\ndef f(): pass\n",
        &["first", "second", "third"],
    )]
    #[case::class_decorator("@dataclass\nclass C: pass\n", &["dataclass"])]
    fn decorators_preserved(#[case] src: &str, #[case] expected: &[&str]) {
        let h = header_of(src);
        let got: Vec<&str> = h.prelude.decorators.iter().map(|d| d.text.as_str()).collect();
        assert_eq!(got, expected);
    }

    // ---- Parameter shapes -------------------------------------------

    /// Matcher tuple used by the params test: what we care about per chip.
    fn chip(p: &ParamChip) -> (&str, Option<&str>, Option<&str>, ParamKind) {
        (
            p.name.as_str(),
            p.ty.as_ref().map(|t| t.text.as_str()),
            p.default.as_deref(),
            p.kind,
        )
    }

    #[rstest]
    // Empty — no params, no separators.
    #[case::no_params("def f(): pass\n", vec![])]
    // Bare regular param.
    #[case::bare_regular(
        "def f(x): pass\n",
        vec![("x", None, None, ParamKind::Regular)],
    )]
    // Typed, no default.
    #[case::typed(
        "def f(x: int): pass\n",
        vec![("x", Some("int"), None, ParamKind::Regular)],
    )]
    // Default, no type.
    #[case::default_only(
        "def f(x=5): pass\n",
        vec![("x", None, Some("5"), ParamKind::Regular)],
    )]
    // Typed + default.
    #[case::typed_default(
        "def f(x: int = 5): pass\n",
        vec![("x", Some("int"), Some("5"), ParamKind::Regular)],
    )]
    // *args.
    #[case::star_args(
        "def f(*args): pass\n",
        vec![("args", None, None, ParamKind::Star)],
    )]
    // **kwargs.
    #[case::double_star_kwargs(
        "def f(**kwargs): pass\n",
        vec![("kwargs", None, None, ParamKind::Kwargs)],
    )]
    // Keyword-only separator: a bare `*` with no name.
    #[case::keyword_only_sep(
        "def f(a, *, b): pass\n",
        vec![
            ("a", None, None, ParamKind::Regular),
            ("", None, None, ParamKind::Star),
            ("b", None, None, ParamKind::Regular),
        ],
    )]
    // Positional-only separator: `/` with no name.
    #[case::positional_only_sep(
        "def f(a, /, b): pass\n",
        vec![
            ("a", None, None, ParamKind::Regular),
            ("", None, None, ParamKind::Slash),
            ("b", None, None, ParamKind::Regular),
        ],
    )]
    // Full combination — regular, slash, regular, star separator, kw-only, **kwargs.
    #[case::full_combination(
        "def f(a, b, /, c, *, d, **kw): pass\n",
        vec![
            ("a", None, None, ParamKind::Regular),
            ("b", None, None, ParamKind::Regular),
            ("", None, None, ParamKind::Slash),
            ("c", None, None, ParamKind::Regular),
            ("", None, None, ParamKind::Star),
            ("d", None, None, ParamKind::Regular),
            ("kw", None, None, ParamKind::Kwargs),
        ],
    )]
    fn params(
        #[case] src: &str,
        #[case] expected: Vec<(&str, Option<&str>, Option<&str>, ParamKind)>,
    ) {
        let h = header_of(src);
        let got: Vec<_> = h.params.iter().map(chip).collect();
        assert_eq!(got, expected);
    }

    // ---- Return type ------------------------------------------------

    #[rstest]
    #[case::no_return("def f(): pass\n", None)]
    #[case::simple_return("def f() -> int: pass\n", Some("int"))]
    #[case::complex_return(
        "def f() -> dict[str, list[int]]: pass\n",
        Some("dict[str, list[int]]"),
    )]
    fn return_type(#[case] src: &str, #[case] expected: Option<&str>) {
        let h = header_of(src);
        let got = h.return_type.as_ref().map(|t| t.text.as_str());
        assert_eq!(got, expected);
    }

    // ---- Docstrings -------------------------------------------------

    #[rstest]
    #[case::no_docstring("def f():\n    pass\n", false)]
    #[case::single_quoted("def f():\n    'one-liner'\n    pass\n", true)]
    #[case::triple_quoted(
        "def f():\n    \"\"\"Multi\n    line.\n    \"\"\"\n    pass\n",
        true,
    )]
    #[case::no_ds_with_comment(
        "def f():\n    # just a comment\n    pass\n",
        false,
    )]
    #[case::class_docstring(
        "class C:\n    \"\"\"About C.\"\"\"\n    pass\n",
        true,
    )]
    fn docstring_detection(#[case] src: &str, #[case] expected_present: bool) {
        let h = header_of(src);
        assert_eq!(h.docstring.is_some(), expected_present);
        if let Some(ds) = h.docstring {
            // Byte range lands inside the source and points at a string
            // literal.
            assert!(ds.byte_range.end <= src.len());
            assert!(ds.text.contains('\'') || ds.text.contains('"'));
        }
    }

    // ---- End-to-end: a realistic decorated async function -----------

    #[test]
    fn realistic_decorated_async_function() {
        let src = "\
@auth_required
@cache.memoize(ttl=300)
async def fetch_user(
    user_id: int,
    /,
    *,
    include_email: bool = False,
) -> User:
    \"\"\"Look up a user by ID.\"\"\"
    return await db.get(user_id)
";
        let h = header_of(src);
        assert_eq!(h.prelude.keyword, KeywordBadge::AsyncDef);
        assert_eq!(h.name, "fetch_user");
        assert_eq!(
            h.prelude
                .decorators
                .iter()
                .map(|d| d.text.as_str())
                .collect::<Vec<_>>(),
            vec!["auth_required", "cache.memoize"],
        );
        let kinds: Vec<ParamKind> = h.params.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![ParamKind::Regular, ParamKind::Slash, ParamKind::Star, ParamKind::Regular],
        );
        assert_eq!(h.params[3].default.as_deref(), Some("False"));
        assert_eq!(h.params[3].ty.as_ref().map(|t| t.text.as_str()), Some("bool"));
        assert_eq!(h.return_type.as_ref().map(|t| t.text.as_str()), Some("User"));
        assert!(h.docstring.is_some());
    }

    // ---- Non-definition nodes return None --------------------------

    #[test]
    fn non_definition_node_yields_none() {
        let src = "x = 5\n";
        let tree = parse(src);
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            // An expression_statement, assignment, etc. is not a definition.
            assert!(build_header(child, src).is_none());
        }
    }
}
