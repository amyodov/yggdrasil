# Yggdrasil

A spatial code viewer for the age of AI-generated code.

Yggdrasil is a code *reviewer*, not an editor. In 2026, humans write little of the code — agents do. The bottleneck has shifted from authoring to *understanding*: reading diffs produced by Claude Code, Codex, and others; verifying the silicon got it right; approving or rejecting. Existing review tools were built for 15-line hand-typed diffs and don't scale to 40-file, 2000-line agent PRs in 10 minutes.

## Why physical, why spatial

Today's code editors are ideological descendants of ancient terminals — fixed character cells, monospace fonts, layouts built around that grid. Then came the win32api era, and now every interface is rectangles nested inside rectangles. Useful for typing; limiting for *reading* and *understanding* at scale.

Yggdrasil starts from a different lineage: the kind of interface you'd design for a cyberpunk film or a game — **diegetic**, spatial, physical. The point of the physicality isn't decoration. It's that **tangible objects communicate comprehension faster than text walls**. A function card is a card because you can see it as an object — its boundaries, its relationship to the class beside it, its position in the file — without re-parsing its shape every time you skim. A rename animates as a morph between versions because that maps directly onto how a mind tracks "the same thing, different now."

*Semantic over syntactic*, throughout: a class attribute and a dataclass field look different; a `@classmethod` attaches to the class armature rather than to `self`; a folded class visibly pulls its methods into itself; a renamed symbol morphs between versions when you scrub the timeline.

## Status

**Early prototype.** Opens and renders single Python files. Does not yet implement PR review, git history timeline, or semantic diff animations — those are planned milestones. See `TASKS.md` for the roadmap.

## Build & run

```sh
cargo run --release -- path/to/file.py
```

The CLI command is `ygg` (short form for the terminal). Requires Rust 1.85. Developed on macOS (Metal backend via `wgpu`); other platforms should work but haven't been smoke-tested.

## Design

Full design doctrine — canonical mode, visual grammar, vocabulary, architecture — lives in [`CLAUDE.md`](CLAUDE.md).

## The name

Yggdrasil is the Norse world-tree. Odin hung on it for nine days to obtain knowledge of runes. A developer hangs on a PR for however long it takes to understand what the agent wrote. The metaphor is intentional. `ygg` (the CLI binary) is just the short-form keystroke for invoking it from the terminal.
