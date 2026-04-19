//! `HeaderModel` — language-agnostic card-header representation.
//!
//! The header of a function/class/method is the dense signature block
//! at the top of its card: decorators, keyword badge, name, parameters,
//! return type, optional docstring. Every source language produces the
//! same shape; the renderer (and M3.5b's reflow engine) consumes it
//! without knowing the original language.
//!
//! Builders live in language-specific submodules (currently only
//! `python`). Rust / TypeScript / Go will each add their own in M8.1+
//! by mapping their tree-sitter ASTs onto the same ADT.
//!
//! Not yet consumed by the renderer — that wiring is part of the same
//! sub-milestone but lands under eyes-on review. Today the model is
//! built alongside the existing `Card` extraction and validated by
//! tests only; shipping as data now gets the architectural seam in
//! place so M3.5b (reflow) and M8.1 (Rust) can plug in without
//! reshaping the ADT.

use std::ops::Range;

/// The full header shape. Every block is language-agnostic: a builder
/// in any language produces one of these, and every consumer reads
/// from it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Renderer consumption arrives under eyes-on review.
pub struct HeaderModel {
    pub prelude: Prelude,
    pub name: String,
    pub params: Vec<ParamChip>,
    pub return_type: Option<TypeChip>,
    pub docstring: Option<Docstring>,
}

/// Block 1 — everything that sits before the identifier: decorators
/// (in source order) plus a keyword badge naming what kind of
/// definition this is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Prelude {
    pub decorators: Vec<DecoratorChip>,
    pub keyword: KeywordBadge,
}

/// One decorator chip. `text` is the decorator's dotted identifier
/// with `@` and any call arguments stripped
/// (`@functools.wraps(f)` → `"functools.wraps"`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct DecoratorChip {
    pub text: String,
}

/// The keyword badge sitting between decorators and name. Values cover
/// Python today plus stubs for later languages — the renderer maps
/// each variant to a fixed label + palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum KeywordBadge {
    /// Python `def`.
    Def,
    /// Python `async def`.
    AsyncDef,
    /// Python `class`.
    Class,
    /// Rust `fn` (stub — first emitter lands in M8.1).
    Fn,
    /// Rust `pub fn` (stub).
    PubFn,
}

/// One parameter column. Per YGG-9 the kind variant covers the four
/// forms that can appear: a regular parameter, a `*` or `/` separator,
/// or the variadic `*args` / `**kwargs` forms. The separator forms
/// carry an empty `name` and no `ty` / `default`; everything else
/// carries at least `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct ParamChip {
    /// Parameter identifier. Empty for separator kinds (`Star` when
    /// acting as a keyword-only divider, `Slash`).
    pub name: String,
    /// Explicit type annotation if present.
    pub ty: Option<TypeChip>,
    /// Default-value expression as source text if present.
    pub default: Option<String>,
    pub kind: ParamKind,
}

/// Which flavour of parameter this column is. Per YGG-9 spec: four
/// variants. `Star` is overloaded by design — it represents either the
/// `*` keyword-only separator (empty name) or the `*args` variadic
/// (non-empty name). Consumers that care about the distinction check
/// `name.is_empty()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ParamKind {
    /// Standard positional-or-keyword parameter.
    Regular,
    /// `*` separator (keyword-only divider) OR `*args` variadic, as
    /// distinguished by `name.is_empty()`.
    Star,
    /// `/` separator (positional-only divider). Name is always empty.
    Slash,
    /// `**kwargs` variadic. Name is non-empty.
    Kwargs,
}

/// A type annotation (parameter type or return type). Stored as raw
/// source text — no structured decomposition yet. Future languages
/// may attach extra hints (e.g. "is_generic", "is_reference") without
/// breaking this shape.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct TypeChip {
    pub text: String,
}

/// Leading docstring of the card's body, when present. Always sits
/// below blocks 1–4 in the rendered header; never participates in
/// row reflow (M3.5b).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Docstring {
    /// Byte range of the whole docstring statement in source (matches
    /// the existing `Card::docstring_range` for identity).
    pub byte_range: Range<usize>,
    /// The docstring literal's raw text, including surrounding quotes.
    /// Stripping quotes and de-indenting is the renderer's concern.
    pub text: String,
}

pub mod python;
pub mod reflow;
