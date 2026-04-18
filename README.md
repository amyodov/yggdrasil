# Yggdrasil

A spatial viewer for code and its history.

A repository is a multiverse: branches that diverge and converge, commits stacking on commits, proposed changes floating alongside the current state, the whole life of every file preserved in layers. Yggdrasil treats it that way — not as a linear text file opened in a window, but as a tree of existences you can move through, zoom into, and scrub across. Browsing a repo in Yggdrasil is browsing that tree.

The viewer handles every level:

- The current state of a file, as structured spatial objects — not a monospace wall.
- Any past version, reachable by dragging a timeline.
- A PR's proposed changes, laid alongside the base, with motion between them.
- The full history of a file as a continuous scrubbable axis.

## Why physical, why spatial

Today's code views are ideological descendants of ancient terminals — fixed character cells, monospace fonts, layouts built around that grid. Then came the win32api era, and every interface became rectangles inside rectangles. Useful for typing; limiting for reading and understanding at scale, inadequate for the kind of exploration a repository deserves.

Yggdrasil starts from a different lineage: diegetic, spatial, physical — the kind of interface you'd design for a cyberpunk film or a game. The point of the physicality isn't decoration. Tangible objects communicate comprehension faster than text walls. A function is rendered as an object because you can see its shape, its neighbours, and its position without re-parsing. A rename morphs between versions because that maps directly onto how a mind tracks "the same thing, different now."

Semantic over syntactic, throughout.

## Status

**Early prototype.** Opens and renders single Python files. Timeline-scrubbing, PR-review, and repo-wide browsing are planned milestones. See `TASKS.md` for the roadmap.

## Build & run

```sh
cargo run --release -- path/to/file.py
```

The CLI command is `ygg` (short form for the terminal). Requires Rust 1.85. Developed on macOS (Metal backend via `wgpu`); other platforms should work but haven't been smoke-tested.

## Design

Full design doctrine — canonical mode, visual grammar, vocabulary, architecture — lives in [`CLAUDE.md`](CLAUDE.md).

## The name

**Yggdrasil** is the Norse world-tree — roots in the underworld, branches spanning all existence, a living structure connecting realms across time. A repository is, literally, a tree of worlds: branches, versions, merges, PRs, the whole history of every file. The metaphor is direct, not decorative.

Odin hung on Yggdrasil for nine days to obtain knowledge of runes. In this tool, a reader hangs in the tree — scrubbing back through a file's history, comparing branches, examining a proposed change — for however long it takes to understand what's actually there. `ygg` (the CLI binary) is just the short-form keystroke.
