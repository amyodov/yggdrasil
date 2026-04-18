# Ygg

A spatial code viewer for the age of AI-generated code.

Ygg is a code *reviewer*, not an editor. In 2026, humans write little of the code — agents do. The bottleneck has shifted from authoring to *understanding*: reading diffs produced by Claude Code, Codex, and others; verifying the silicon got it right; approving or rejecting. Existing review tools were built for 15-line hand-typed diffs and don't scale to 40-file, 2000-line agent PRs in 10 minutes.

Ygg renders code as spatial objects — floating plates, structured cards, a luminous scroll under a cosmic void — so comprehension works at the speed of skimming, not the speed of reading. *Semantic over syntactic*: a class attribute and a dataclass field look different; a `@classmethod` attaches to the class armature rather than to `self`; a renamed symbol morphs between versions when you scrub the timeline.

## Status

**Early prototype.** Opens and renders single Python files. Does not yet implement PR review, git history timeline, or semantic diff animations — those are planned milestones. See `TASKS.md` for the roadmap.

## Build & run

```sh
cargo run --release -- path/to/file.py
```

Requires Rust 1.85. Developed on macOS (Metal backend via `wgpu`); other platforms should work but haven't been smoke-tested.

## Design

The visual language is *diegetic cosmic-library*: ancient physical artifacts (scrolls, cardboard planks, metal rivets, woven linen) floating in a deep-space context. Reference anchors: Fairlight CMI, *Foundation*'s vault, *Sandman*'s Dream-library, *Blade Runner 2049*.

Full design doctrine — canonical mode, three-zone visual grammar, vocabulary, architecture — lives in [`CLAUDE.md`](CLAUDE.md).

## The name

Ygg is short for **Yggdrasil**, the Norse world-tree. Odin hung on it for nine days to obtain knowledge of runes. A developer hangs on a PR for however long it takes to understand what the agent wrote. The metaphor is intentional.
