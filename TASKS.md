# Ygg — Milestones

Sequential milestones from empty project to a working prototype. Read `CLAUDE.md` for philosophy, visual language, and architecture first — this document assumes you know *what* we're building and explains the *order*.

Each milestone is a stable checkpoint: the app runs, does something observable, and is worth committing. Don't try to one-shot multiple milestones; finish one, verify by eye, move on.

> **Sub-milestone specs and workflow live in Linear** (project `Yggdrasil`, team `Yggdrasil`). Each sub-milestone has a `YGG-<n>` issue with full description, parent links, and `blockedBy` relationships. `TASKS.md` keeps the high-level roadmap — the git-visible story of *where we're going*. When adding a sub-milestone: create it in Linear first, then drop a one-line pointer under the right milestone here.

## General notes for every milestone

- **CLI-first architecture from day one.** The mode-dispatching shape documented in `CLAUDE.md` (`ygg <file>`, `ygg <dir>`, `ygg <dir> --git`, `ygg diff <ref1> <ref2>`) is established in M1 and extended in each subsequent milestone. Don't write a monolith and refactor later.
- **One state, derived rendering.** Every new feature extends `AppState` and derives visuals from it. No ad-hoc animation event handlers.
- **Scrub-readiness check.** Before a sub-milestone is declared done: does every piece of state it introduces survive scrubbing between versions? If a piece of state would be lost when the file changes under the user, either (a) make it derivable from `(timeline_position, scene_description)`, or (b) route it through stable identity — not per-version `CardId`. Rule applies at every granularity: card, line, character. See CLAUDE.md "Scrubbing is a first-class concern at every step."
- **Tests: maximize coverage per line of test code.** Use `rstest` parameterized tests. One test per logical unit with cases covering all branches. Don't test rendering output.
- **Verify by eye.** After each milestone, visually confirm the result looks right. Screenshots in commit messages welcome.
- **Don't skip ahead on visual polish.** Milestones 1–7 aim at *correct behavior*; M8 is the polish pass. Tempting FUI flourishes before then are a distraction.
- **Architectural foresight is first-class.** Sub-milestones marked *(architectural)* exist so later milestones don't force rewrites. Design for downstream features listed in the Linear issue, not just the immediate user-visible goal. Examples: the offscreen RT primitive (M3.1) landed before plate visuals; the `HeaderModel` ADT (M3.5a) must be language-agnostic from day one; per-line text model (M6.0) lands before the diff engine that fills it in.

---

## Milestone 1 — The stack works end to end

**Goal**: a native window opens, reads a real file from disk, renders its text on GPU, shows an empty overlay where the file tree will live. Proves the full stack (wgpu + egui + winit + file I/O) wires together.

**CLI**: `ygg <path-to-file>`.

**Status**: **shipped**.

---

## Milestone 2 — Syntax highlighting and virtualized scroll

**Goal**: the code view becomes pleasant to look at and works for large files. Tree-sitter Python, luminous-palette token colors, scroll virtualization, line numbers, shared background.

**Status**: **shipped**.

---

## Milestone 3 — Cards

**Goal**: functions and classes stop looking like indented text and start looking like structured visual objects. AST-driven card extraction, fold animation, class armature spine, visibility accents, rounded SDF shapes.

**Status**: **shipped** (main milestone). Sub-milestones below refine and extend.

### M3 sub-milestones (tracked in Linear)

- [`YGG-5`](https://linear.app/laerad/issue/YGG-5) **M3.1 — Plate-rendering primitive + offscreen RT architecture** *(architectural, shipped)*. 3D-quad plates with model matrix + per-plate RT; foundation for all later plate features.
  - [`YGG-17`](https://linear.app/laerad/issue/YGG-17) **M3.1.1 — Taller-than-plate RT + UV-offset scrolling + mipmap chain**. Scroll as UV offset; pre-rendered texture cache. Slot before M3.7.
- [`YGG-6`](https://linear.app/laerad/issue/YGG-6) **M3.2 — Icon system (Lucide + SDF atlas)** *(architectural)*. One rendering path for every UI affordance.
- [`YGG-7`](https://linear.app/laerad/issue/YGG-7) **M3.3 — Three-zone physicality: lit plate, physical cards** *(shipped)*. Plate = luminous surface; cards = drop-shadowed paper objects; semantic lights stay emissive.
  - [`YGG-20`](https://linear.app/laerad/issue/YGG-20) **M3.3.1 — Material textures and tinting (linen, paper, cardboard, blueprint, graph, vellum)** *(cross-depends on M3.8.1)*. Procedural grain + two-channel linen opacity (nebula diffuse + stars through weave holes).
  - [`YGG-19`](https://linear.app/laerad/issue/YGG-19) **M3.3.2 — Card top-edge rim light**. Matches the plate's lit-from-above cue.
- [`YGG-8`](https://linear.app/laerad/issue/YGG-8) **M3.4 — Fold anatomy, two-button fold, nested cascade**. Header / docstring / body bands; independent toggles; class-fold propagates to children.
- [`YGG-9`](https://linear.app/laerad/issue/YGG-9) **M3.5a — `HeaderModel` ADT + Python builder, single-row layout** *(architectural)*. Language-agnostic header representation.
- [`YGG-10`](https://linear.app/laerad/issue/YGG-10) **M3.5b — Block-flow reflow engine**. Multi-row header layout when width shrinks.
- [`YGG-11`](https://linear.app/laerad/issue/YGG-11) **M3.6 — Code-pane plate structure**. Fixed filename plank, plate margins, scroll clipping to rounded corners.
  - [`YGG-21`](https://linear.app/laerad/issue/YGG-21) **M3.6.1 — Plank-on-rods skeuomorphic connector** *(cross-depends on M3.7)*. Plank visibly bolted to the upper rod.
- [`YGG-12`](https://linear.app/laerad/issue/YGG-12) **M3.7 — Scroll-winding: pin zones with cylindrical curl**. Content wound onto cylinders at top/bottom; cap-glow at rod ends.
- [`YGG-13`](https://linear.app/laerad/issue/YGG-13) **M3.8 — Background visibly alive** *(shipped)*. Volumetric nebula (far / mid / near cloud layers + turbulence), wide-diapason epoch, vignette, always-Poll event loop.
  - [`YGG-18`](https://linear.app/laerad/issue/YGG-18) **M3.8.1 — Stars and nova-pulses**. Point-light layer gated by linen weave holes (M3.3.1). Ships before M3.3.1.

---

## Milestone 4 — Two panes, real file tree

**Goal**: navigate a directory by clicking files in the left tree; code view transitions smoothly; hover-to-activate regions; tree ↔ code visual linkage.

**CLI**: `ygg <path>` — path can be a file or a directory.

**Status**: not started.

### M4 sub-milestones (tracked in Linear)

- [`YGG-14`](https://linear.app/laerad/issue/YGG-14) **M4.1 — Basic tree**. Real file tree, directory navigation, file-selection transitions.
- [`YGG-15`](https://linear.app/laerad/issue/YGG-15) **M4.2 — Sticky-pinned current file**. Selected file stays visible when scrolled out — detaches from the list and floats at the tree viewport edge.
- [`YGG-16`](https://linear.app/laerad/issue/YGG-16) **M4.3 — Tree ↔ code aura link** *(independently-shippable)*. Canonical first use of the **projection** vocabulary word; reusable primitive for later cross-view identity links.

---

## Milestone 5 — Git history and the timeline (static)

**Goal**: the timeline widget appears and you can pick a commit. No animations between commits — just view the repo at a chosen moment. Gitoxide integration, time-based timeline, transport controls (placeholders), commit significance weighting.

**CLI**: `ygg <path> --git`.

**Status**: not started.

---

## Milestone 6 — Semantic diff (no animation yet)

**Goal**: show the difference between two versions of a file with card-level annotations, statically. GumTree-style AST diff producing typed changes (Added/Deleted/Modified/Moved/Renamed/Copied). Per-card colored accents, no animation yet.

**CLI**: `ygg diff <ref1> <ref2> [<path>]`.

**Status**: not started.

### M6 sub-milestones (tracked in Linear)

- [`YGG-22`](https://linear.app/laerad/issue/YGG-22) **M6.0 — Per-line text model** *(architectural, ships before M4/M5)*. Card text becomes `Vec<LogicalLine>` with stable per-line identity and animation-state hooks. Zero visual change today; unblocks M6 diff-state assignment and M7 per-line animation without retrofitting glyphon's one-buffer-per-card model.

---

## Milestone 7 — Scrubbing and animation

**Goal**: the timeline becomes a video-editor scrubber. Diffs animate as you drag between versions. Global `timeline_position: f32`, pure-function rendering, direction-independent animations (addition/deletion/modification/move/rename/copy).

**Status**: not started.

---

## Milestone 8 — Polish, FUI effects, tree-level animations

**Goal**: the prototype feels *finished* — the aesthetic is cohesive and effects land. Refined glow, class armature fully realized with `self`-node branching, tree-level diff animations (rename letter morph, move slides, copy cell-division), smooth zoom across scales, keyboard shortcuts, semantic-rules layer for `@dataclass`/`@property`/`@abstractmethod`.

**Status**: not started.

**Out of scope (belongs to a post-prototype roadmap)**:
- Nested zoom brackets on the timeline.
- Project-wide (repo-scale) timeline.
- 3D branch graph visualization.
- Multi-language support beyond Python.
- 2.5D alternative visual theme experiment.
- Agent manifest format beyond basic consumption.
- Deep integration with Claude Code / other agents (CLI invocation, MCP server, etc.).
- **Bulk fold HUD overlay** (global "fold all bodies / fold all docstrings / expand all" toggle). Architectural hooks to support it are implicit in the M3.4 fold model and the M3.2 icon system.
- **Actual 3D rotation of plates** (repo browsing, "pages of the history book," branch-graph tilting). M3.1's offscreen-RT + 3D-quad architecture already makes this a drop-in feature rather than a rewrite.

---

## After the prototype

When M8 ships, the prototype is ready to show: a blog post, a demo video, feedback from the Zed / JetBrains design communities. That's the next decision point: keep iterating solo, look for collaborators, pursue integration with an existing editor, or productize standalone. Don't plan that now — plan it with the prototype in hand.
