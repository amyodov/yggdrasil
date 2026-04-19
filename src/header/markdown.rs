//! Markdown builder: ATX heading → `HeaderModel`. Placeholder; the
//! real implementation lands in the next commit of this series.

#![allow(dead_code)]

use tree_sitter::Node;

use super::HeaderModel;

pub fn build_header(_node: Node, _source: &str) -> Option<HeaderModel> {
    None
}
