# Ygg — Milestones

Sequential milestones from empty project to a working prototype. Read `CLAUDE.md` for philosophy, visual language, and architecture first — this document assumes you know *what* we're building and explains the *order*.

Each milestone is a stable checkpoint: the app runs, does something observable, and is worth committing. Don't try to one-shot multiple milestones; finish one, verify by eye, move on.

## General notes for every milestone

- **CLI-first architecture from day one.** The mode-dispatching shape documented in `CLAUDE.md` (`ygg <file>`, `ygg <dir>`, `ygg <dir> --git`, `ygg diff <ref1> <ref2>`) is established in M1 and extended in each subsequent milestone. Don't write a monolith and refactor later.
- **One state, derived rendering.** Every new feature extends `AppState` and derives visuals from it. No ad-hoc animation event handlers.
- **Tests: maximize coverage per line of test code.** Use `rstest` parameterized tests. One test per logical unit with cases covering all branches. Don't test rendering output.
- **Verify by eye.** After each milestone, visually confirm the result looks right. Screenshots in commit messages welcome.
- **Don't skip ahead on visual polish.** Milestones 1–7 aim at *correct behavior*; M8 is the polish pass. Tempting FUI flourishes before then are a distraction.

---

## Milestone 1 — The stack works end to end

**Goal**: a native window opens, reads a real file from disk, renders its text on GPU, shows an empty overlay where the file tree will live. Proves the full stack (wgpu + egui + winit + file I/O) wires together.

**CLI**: `ygg <path-to-file>` — takes a real file argument. No placeholders, no hardcoded strings.

**What ships**:
- `winit` window, `wgpu` surface, `egui` integration for overlays.
- Reads the file from the given path. If it doesn't exist, a clear error.
- Main canvas area: renders the file's text in monospace, one color, no syntax highlighting yet. Plain white-on-dark.
- Left region (~25% of width): empty egui panel where the file tree will live. A placeholder label "file tree" is fine.
- Right region: the code view.
- Background: near-black. No animation yet.
- Text is scrollable with mouse wheel (vertical). Horizontal overflow: ignore for now, let it clip.

**Out of scope**: syntax highlighting, cards, animations, tree content, git.

**Tests**: CLI parsing (valid path / missing path / directory where file expected → error), basic file reader.

---

## Milestone 2 — Syntax highlighting and virtualized scroll

**Goal**: the code view becomes pleasant to look at and works for large files.

**What ships**:
- Tree-sitter integration with Python grammar. Token → color mapping via a simple theme.
- Colorful Python rendering: keywords, strings, comments, function/class names, types distinctly colored in the luminous palette (not Solarized — pick colors that fit the void aesthetic).
- Scroll virtualization: only visible lines are rendered. Should feel smooth on a 10,000-line file.
- Line numbers in a subtle color on the left of the code.
- Code and (still-empty) tree pane sit over a *shared* background — no visible pane borders. They're regions in one space, not windowed boxes.

**Out of scope**: cards, animations, semantic rules beyond token coloring.

**Tests**: tree-sitter wrapper returns expected token types for sample snippets (parameterized cases covering keywords, strings, comments, decorators, type annotations), line-virtualization logic (given scroll offset + viewport height, correct line range is computed).

---

## Milestone 3 — Cards

**Goal**: functions and classes stop looking like indented text and start looking like structured visual objects.

**What ships**:
- Method/function detection from AST: each top-level function and each method in a class gets wrapped in a visual card.
- Card anatomy (see Visual language in `CLAUDE.md`): keyword badge, name, signature with typed parameters, return type badge, docstring sub-header, body with syntax highlighting.
- Visibility encoding: public methods bright with prominent accent; private methods (`_name`) muted, slightly transparent, smaller glow.
- Classmethod / staticmethod detection via decorators — visually attached more rigidly to the class (start establishing the "armature" idea, even if crude).
- Fold handle per card. Clicking it **rolls up** the body into the header with an animation (the body visually rolls into the signature line; not a snap). Unfold reverses it.
- Class-level wrapping: the class gets its own container card with a left-side "spine" (vertical luminous rail). Methods visually sit inside the class container.
- Cards float over the void with soft luminous edges — no drop shadows, only glow.

**Out of scope**: semantic-rules layer beyond decorator detection, diff animations, multi-file.

**Tests**: AST → card structure mapping (parameterized: simple function, class with methods, class with classmethod/staticmethod, nested class, decorator stacks), fold state transitions.

---

## Milestone 4 — Two panes, real file tree

**Goal**: you can navigate a directory by clicking files in the left tree.

**CLI**: `ygg <path>` — path can now be a file *or* a directory. Directory opens the tree pane populated; file opens with a tree rooted at its parent.

**What ships**:
- Left pane: a real file tree of the target directory (egui-based, floating over the shared background). Collapse/expand directories. Current file is highlighted.
- Right pane: the code view as built in M1–M3.
- Click a file → code view transitions to that file. Use a short, pleasant transition (fade or slide), not a hard swap.
- Hover-to-activate regions: hovering the tree region makes a soft luminous aura appear around it, signaling "scrolling targets this region." Same for code region.
- Background gets its subtle ambient animation: a very slow, low-contrast drift — something that reads as "alive" without being distracting. Keep amplitude small.
- Visual linkage between tree and code: when a file is selected, the tree node and the code pane share a visual cue (a soft connecting glow, a matching accent color, something — exact form is for Claude Code to propose and iterate on).

**Out of scope**: git, diffs, multi-file side-by-side.

**Tests**: file tree construction from a directory (parameterized with fixture trees: empty, nested, hidden files, symlinks), file selection state transitions.

---

## Milestone 5 — Git history and the timeline (static)

**Goal**: the timeline widget appears and you can pick a commit. No animations between commits yet — just the ability to view the repo at a chosen moment.

**CLI**: `ygg <path> --git` — enables git-aware mode.

**What ships**:
- Gitoxide integration. On load: walk the history of the file currently shown, extract commits that touched it, build a per-file timeline.
- Time-based axis (see Timeline widget in `CLAUDE.md`): commits positioned by real timestamp, not index. Quiet periods stretch; bursts cluster.
- Timeline appears as a floating HUD overlay (egui), bottom of the screen or similar — not a fixed pane. DaVinci Resolve / Premiere styling cues.
- Transport controls: play / pause / skip-to-start / skip-to-end / step-forward / step-back buttons. Play buttons exist in both directions (forward and reverse) — but in M5 they're placeholders; they'll become animated in M7.
- Clicking a position on the timeline = jump to that commit. Code view re-renders at that version of the file.
- Initial significance/weight computation: a simple first pass (delta size + explicit tag markers). Visible as bar height/brightness on the timeline. Full weight algorithm is for later — M5 just wires the concept.
- Indexing: first-load indexing is computed and cached to `.ygg/` next to the repo. Subsequent opens are fast.

**Out of scope**: diff visualization, animations between commits, nested zoom brackets (post-prototype), multi-file project timeline.

**Tests**: git history extraction for a file (fixture repos with known history — linear, with branches, with merges, with rename), weight computation (parameterized cases: trivial commit, large commit, tagged commit, isolated commit after quiet period), timeline position ↔ time mapping.

---

## Milestone 6 — Semantic diff (no animation yet)

**Goal**: show the difference between two versions of a file with card-level annotations, statically.

**CLI**: `ygg diff <ref1> <ref2> [<path>]` — new mode for direct diff viewing.

**What ships**:
- AST-level diff algorithm (GumTree-style) over tree-sitter trees. Produces a list of typed changes: `Added`, `Deleted`, `Modified`, `Moved`, `Renamed`, `Copied` (for files).
- Detection layers, in priority order (see `CLAUDE.md`): (a) explicit agent manifest if present as `.ygg-manifest.json` adjacent to the commit or configured path — *optional infrastructure*, not required; (b) git's `-M`/`-C` for file-level rename/copy; (c) our own AST-matching for intra-file moves.
- Visual encoding on cards (static, no animation yet):
  - Added: green-accented card, bright.
  - Deleted: red-accented card, dimmed, visibly marked as removed.
  - Modified: yellow/amber accent; sub-line changes highlighted within the card body.
  - Moved: blue-accented, with a visual hint of where it came from (a ghost outline at old position, perhaps, or a subtle arrow).
  - Renamed (symbol or file): both names shown with a connecting glow.
- Diff can be viewed for a single file between two refs, or for a whole directory.
- The file tree in diff mode shows which files changed (colored dots or badges per file).

**Out of scope**: animation of these changes (that's M7). Agent-manifest schema beyond "if the file exists, use it" (formalize later).

**Tests**: AST diff algorithm (parameterized: pure add, pure delete, pure modify, method moved within file, method renamed, method moved + modified, class restructured), file-level rename/copy detection wrappers.

---

## Milestone 7 — Scrubbing and animation

**Goal**: the timeline becomes a video-editor scrubber. Diffs animate as you drag between versions.

**What ships**:
- Global `timeline_position: f32` (0.0..1.0 within the current two-commit range, or a continuous value across multiple commits — design this).
- Every card computes its visual state as a pure function of `timeline_position` and the diff operations between the two bounding commits.
- Animations per change type (see Visual language):
  - **Addition**: at 0 → absent; at 1 → full card. Between: slight expansion + green glow flash + fade-in. Reversed scrub plays backward.
  - **Deletion**: at 0 → full card; at 1 → absent. Between: red brighten → compress → fade into darkness. Reversed scrub: darkness → red flash → expand.
  - **Modification**: sub-line fades/flashes within the card.
  - **Move**: blue glow, position interpolates linearly between old and new coordinates.
  - **Move + modify**: both compose — position interpolates and inner content animates simultaneously.
  - **Rename**: letters morph between old and new text.
- Transport controls now actually work: play animates `timeline_position` at a chosen speed, reverse-play decrements it, scrubber drag sets it directly.
- Direction-independence: rewinding a deletion shows the *same red-fade animation* running backward (card materializes out of darkness through a red flash), not a "re-addition" in green.

**Out of scope**: file-tree animations (renames/moves at the tree level) — stretch goal; if trivial to include, include; if not, M8.

**Tests**: interpolation functions per change type (parameterized with progress values 0.0, 0.25, 0.5, 0.75, 1.0 — verify positions, opacities, colors compute correctly), timeline play/pause/reverse state machine.

---

## Milestone 8 — Polish, FUI effects, and tree-level animations

**Goal**: the prototype feels *finished* — the aesthetic is cohesive and effects land. This is the "make it pretty" pass where earlier-deferred flourishes come back.

**What ships**:
- Refined glow effects: cards have proper luminous edges tuned for the void aesthetic; hover states are rich; focus/selection reads clearly.
- The class armature is fully realized: a visibly "metal-luminous" spine for the class, with rigid short connectors to classmethod/staticmethod cards, and softer branch connectors to instance-method cards via a `self` node.
- Background ambient animation is polished to the "breathing sky" feel — never distracting, always alive.
- File-tree–level diff animations: renames morph letters in the tree, moves slide nodes between parents, copies show a cell-division animation from ancestor.
- Smooth zoom from repo-level overview to file-level to method-level — no hard cuts.
- Keyboard shortcuts for common navigation (space for play/pause, arrow keys for step, etc.).
- Semantic-rules layer: handle `@dataclass`, `@property`, `@abstractmethod`, and the class-var vs. instance-field distinction. Visual differentiation applies.
- Final sweep: remove placeholder UI, align typography, ensure color palette is cohesive, check that nothing breaks on large repos.

**Out of scope (belongs to a post-prototype roadmap)**:
- Nested zoom brackets on the timeline.
- Project-wide (repo-scale) timeline.
- 3D branch graph visualization.
- Multi-language support beyond Python.
- 2.5D alternative visual theme experiment.
- Agent manifest format beyond basic consumption.
- Deep integration with Claude Code / other agents (CLI invocation, MCP server, etc.).

---

## After the prototype

When M8 ships, the prototype is ready to show: a blog post, a demo video, feedback from the Zed / JetBrains design communities. That's the next decision point: keep iterating solo, look for collaborators, pursue integration with an existing editor, or productize standalone. Don't plan that now — plan it with the prototype in hand.
