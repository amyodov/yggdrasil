# Ygg — Milestones

Sequential milestones from empty project to a working prototype. Read `CLAUDE.md` for philosophy, visual language, and architecture first — this document assumes you know *what* we're building and explains the *order*.

Each milestone is a stable checkpoint: the app runs, does something observable, and is worth committing. Don't try to one-shot multiple milestones; finish one, verify by eye, move on.

## General notes for every milestone

- **CLI-first architecture from day one.** The mode-dispatching shape documented in `CLAUDE.md` (`ygg <file>`, `ygg <dir>`, `ygg <dir> --git`, `ygg diff <ref1> <ref2>`) is established in M1 and extended in each subsequent milestone. Don't write a monolith and refactor later.
- **One state, derived rendering.** Every new feature extends `AppState` and derives visuals from it. No ad-hoc animation event handlers.
- **Tests: maximize coverage per line of test code.** Use `rstest` parameterized tests. One test per logical unit with cases covering all branches. Don't test rendering output.
- **Verify by eye.** After each milestone, visually confirm the result looks right. Screenshots in commit messages welcome.
- **Don't skip ahead on visual polish.** Milestones 1–7 aim at *correct behavior*; M8 is the polish pass. Tempting FUI flourishes before then are a distraction.
- **Architectural foresight is first-class.** Sub-milestones explicitly marked *(architectural)* exist so that later milestones don't force rewrites of earlier work. When implementing one of those, design for the downstream features listed in its description — not just the immediate user-visible goal. Examples: the offscreen RT primitive (M3.1) must land before any plate visuals; the `HeaderModel` ADT (M3.5a) must be language-agnostic from day one, not Python-first-and-refactor-later.

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

### M3 sub-milestones — visual and architectural refinement

M3's first pass shipped cards as lit rectangles with binary fold. Review surfaced that they read as "glowing overlays," not physical plates; that fold targets the wrong line (decorator, not signature); that nested folds don't propagate; that the background animation is invisible; and that the code pane is a border-of-nothing rather than a tangible surface. These sub-milestones refine and extend M3 before M4. They ship in the order listed below — later ones build on earlier foundations.

#### M3.1 — Plate-rendering primitive and offscreen RT architecture *(architectural)*

**Goal**: establish the rendering foundation every subsequent plate (code pane, cards, future tree nodes, future history pages) builds on.

**What ships**:
- Each plate is modeled as a **3D-space quad with its own model matrix**. Camera is orthographic + frontal by default, so plates look 2D today; rotation-ready for free tomorrow.
- **Plate contents render to an offscreen texture (RT)**, then composited onto the plate quad. Scroll = UV offset into the RT. Future curl / rotation / zoom-out-fly-through all sample the same cached raster.
- Glyphon renders into the plate RT (not directly to the swap chain). **Grayscale AA throughout** (not subpixel) so text survives non-integer transforms.
- RT pooling: one RT per visible plate, recycled when a plate leaves visibility. Size-bounded (e.g. plate-height × 3 at normal zoom; extend as scroll progresses). Manual mipmap chain on each RT so angled rendering doesn't shimmer.
- Per-plate dirty flag: only re-render the RT when its contents actually change. Frame-to-frame, compositing the cached texture is cheap.

**Why this is the keystone**: M3.3 (physical plates), M3.6 (code-pane plate), M3.7 (scroll-winding), and any future 3D features (M8 zoom, post-prototype branch graphs, "pages of the history book" rotation) all depend on this primitive. Shipping visuals first and retrofitting RT later is a rewrite; shipping RT first is a one-time cost.

**Out of scope**: actual rotation, actual curl, actual 3D effects — those come in later milestones. M3.1 just establishes the pipeline; default rendering still looks 2D.

**Tests**: RT allocation/pooling/recycling, model-matrix → NDC projection (parameterized with identity / translated / rotated / scaled), dirty-flag invalidation logic.

##### M3.1.1 — Taller-than-plate RT + UV-offset scrolling

**Goal**: scroll becomes a UV-offset sample into a larger-than-plate RT, instead of re-rendering visible content each frame. Follow-up optimization on the M3.1 primitive.

**What ships**:
- RT is allocated at `plate_size × N` tall (N ~3 at normal zoom). Scroll = offset the sample UV by `scroll_y / rt_height`.
- Streaming: when the scroll position approaches the edge of the cached RT window, re-render the next "slab" of content into the RT below/above the current view.
- Manual mipmap chain on the RT so sampling at angles (for M3.7's cylinder curl, and future rotations) doesn't shimmer.
- Dirty flag now actually skips re-renders when nothing changed.

**Why not in M3.1**: the M3.1 keystone intentionally shipped the minimum viable architecture — plate-sized RT, re-render every frame. The taller-RT + UV-scroll + mipmaps is a separate optimization that doesn't change the primitive's public API. Slot it before M3.7 (scroll-winding really wants the cylinder curl sampling a pre-rendered texture) or whenever performance pressure shows up first.

**Tests**: streaming threshold correctness (when does the next slab render?), mipmap coverage, UV-scroll accuracy.

#### M3.2 — Icon system with Lucide + SDF atlas *(architectural)*

**Goal**: a first-class icon primitive so every subsequent UI affordance (fold handles, bulk controls, timeline transport, hover cues) uses one rendering path.

**What ships**:
- Lucide icons as the chosen set. MIT-licensed, linear/precise glyphs matching the FUI aesthetic (FontAwesome's chunky typographic style doesn't fit).
- Build-time or init-time step: rasterize the icons we use into a **signed-distance-field (SDF) atlas texture**. Pick a starter subset (fold down, fold up, play, pause, skip-prev, skip-next, step-prev, step-next).
- Runtime rendering: a quad sampled from the atlas through the shapes pipeline with an arbitrary tint color. Works at any scale; DPI-agnostic by construction.
- Integration: fold handles on cards stop being bare geometric shapes and use icons from day one of M3.4.

**Why this early**: every downstream UI element that isn't a plate is an icon on a plate. Fold handles (M3.4), bulk fold controls (post-prototype), timeline transport (M5/M7). One pipeline, not eight.

**Out of scope**: custom (non-Lucide) icons, animated icons, icon color theming beyond tint.

**Tests**: SDF sampling produces correct silhouettes at varying scales (golden-image or SDF-distance-at-known-points), atlas packing correctness, icon lookup by name.

#### M3.3 — Three-zone physicality: lit plate, physical cards

**Goal**: implement the three-zone visual grammar from `CLAUDE.md` — the plate is the luminous surface (Zone 2); cards are 2.5D physical objects on it (Zone 3) with tiny shadows and no outer glow; semantic lights (class spine, state transitions) stay luminous.

**What ships**:

*Plate (Zone 2):*
- **Outer bloom into the void**: baked into `composite.rs` shader — sample the plate RT's alpha mask, expand it by `bloom_radius`, feather to zero. Gives the plate a soft halo in the void without bloating the RT (no extra texture space, rotation-proof).
- **Inner luminance**: panel fill gets a subtle radial gradient (brighter toward center) + top→bottom tint so the plate surface reads as a lit material, not flat paint.
- **Top-edge rim light** along the plate's rounded top edge — implicit-above light cue (faint, 1–2px).
- Plate still reads tinted-glass over the sky at low alpha; sky breathing stays visible.

*Cards (Zone 3):*
- **Remove outer glow** on cards. Card backgrounds become near-opaque tinted "paper" fills.
- **Drop shadow** beneath each card: a softened, dark, low-alpha copy of the card shape, offset ~2–3pt down/right, blurred via the SDF edge fade (large glow radius with dark color ≈ shadow). Drawn before the card fill.
- **Top-edge rim light** (1px bright inner line at card top) — "lit from above."
- Public cards get a fractionally taller shadow than private ones (the spec's "elevated/sitting lower" affordance).

*Semantic lights (stay emissive):*
- Class spine — luminous, unchanged.
- Fold-state color (handle color flip) — unchanged.
- Rolling edge during fold animation — unchanged (a true emissive hairline).
- Future diff flashes, hover state pulses — use the same "glow" path.

**Why composite-shader bloom (not expanded RT)**: baking bloom into the composite pass means (a) no extra RT space needed (plate-sized RT is enough), (b) the bloom travels with any future model-matrix rotation of the plate for free, (c) single sampling point — cheap. The alternative (enlarge RT by `bloom_radius` on each side) bloats every plate's memory footprint and has to track which part of the RT is "plate" vs "pad."

**Out of scope**: light direction changing, plate-to-plate shadows (plates don't shadow each other in the void), animated lighting, real-material simulation (PBR etc.).

**Tests**: shadow offset + rim-light color presence in the instance stream (structural, not pixel-level), composite bloom shader math at known sample points.

**Tests**: plate shader sampling at known positions produces expected luminance profiles (top-brighter than bottom; edge-rim > interior).

##### M3.3.1 — Material textures and tinting *(cross-depends on M3.8.1)*

**Goal**: Zone-3 objects (cards, planks, future `.pyi` blueprints) stop reading as "clean plastic with a gradient" and start reading as actual paper, cardboard, blueprint, graph paper, vellum. This is where the visual vocabulary's *paper* word stops being a metaphor in the doctrine and becomes visible in pixels.

**Ship order note**: depends on M3.8.1 (stars) existing, because the plate's most distinctive material — **linen** — works as a *rhythmic-transparency* pattern that reveals the star layer through weave holes. The linen needs stars already in the backdrop to be meaningful.

**What ships**:
- Procedural material grain functions in the shapes / composite WGSL:
  - `paper_grain` (for regular cards — subtle fiber texture)
  - `cardboard_pulp` (heavier, larger-scale fleck, for planks / headers)
  - `blueprint_rule` (straight grid + slight dither, for future `.pyi` plates)
  - `graph_rule` (rule-lined grid, for test files)
  - `vellum_wash` (very smooth low-frequency, for docstring bands)
  - **`linen_weave` — the most distinctive.** A rhythmic cross-hatch of horizontal + vertical threads, with tiny single-pixel-ish weave holes at regular intervals. Non-ideal: slight thread wobble, occasional imperfections, gives a handmade not-CGI feel.
- New `material` field on `RectInstance` (repurposes the existing padding bytes — no stride change). Fragment shader switches on it to pick a grain function.
- **Decoupled-opacity model for linen**: two different pass-through rules, one per light type:
  - Diffuse atmospheric light (nebula) passes through the whole plate at low alpha (~25%) — threads slightly, holes slightly more. Uniform tinted bleed.
  - Sharp point light (stars, nova-pulses from M3.8.1) passes through *weave holes only*. Threads fully block. This gates the star field behind the plate's pattern, making stars textural rather than attention-grabbing.
- Tinting via layered shapes: a card body gets a grain; an extra translucent colored shape drawn on top creates a "region tint" (e.g., a blue info chip on a cardboard header band). No new primitive — compositing already supports it.
- Default material assignment: regular cards = paper, class cards = heavier paper, code-pane plate = **linen** (vocabulary refinement: was "parchment"; linen has visible weave structure, which is exactly what the shader produces). Future M3.6 plank = cardboard; future `.pyi` plate = blueprint.
- Grain amplitudes conservatively tuned — visible but not "noisy material fighting the text." Users never consciously notice individual grain features; they notice the surface is *material*.

**Why procedural, not sampled**: (a) our dialect is stylized, not photoreal — sampled paper textures push toward photoreal and clash. (b) No asset pipeline, no licensing, no tiling artifacts, DPI-independent by construction. (c) Tinting is trivial over procedural. (d) Scales to new materials in <30 lines of WGSL each. (e) Procedural weave can do the two-channel opacity math in the same shader.

**Out of scope**: file-type → material dispatch (e.g., "opening `.pyi` gives a blueprint plate" — hooks exist, but file-type routing is M4+ work). Semantic-cue-driven tint regions (M3.4/M3.5 will drive these once header anatomy lands).

**Tests**: material grain functions produce bounded, deterministic output for known UVs; material-id → grain-function dispatch; tinted overlay blend matches expected alpha composition; linen-weave pass-through math distinguishes diffuse from point-light channels.

##### M3.3.2 — Card top-edge rim light

**Goal**: apply the "lit from above" rim cue to individual cards, matching what the plate already does. Promised in the three-zone grammar; deferred from M3.3 proper for scoping.

**What ships**:
- A thin bright inner-edge line at each card's top, ~1px (DPI-scaled), fading to invisible within the first `card_inner_pad_y` pixels. Follows the card's rounded corners (same SDF approach as the plate's rim).
- Public cards get the rim slightly brighter than private (adds to the "elevated vs. sitting lower" affordance already in the shadow scale).

**Tests**: rim instance presence in shape output, intensity-by-visibility.

#### M3.4 — Fold anatomy, two-button fold, nested cascade

**Goal**: fold collapses the right part of the card; class fold propagates to nested cards; user has independent control over body vs docstring visibility.

**What ships**:
- Card anatomy splits into three bands: **header** (decorators as chips + signature blocks — always visible), **docstring** (optional, below header; foldable independently), **body** (code; foldable independently).
- **Two fold buttons per card**: body toggle and docstring toggle. Independent state, four total combinations. Both use Lucide icons via M3.2.
- **State model**: each card has `body_progress: f32` and `docstring_progress: f32`, each smoothstep-eased between 0 (hidden) and 1 (shown). Pure derivation — no event handlers animate these, only the per-frame interpolation toward targets.
- **Nested fold cascade**: when a class's body folds, all child cards (methods, nested classes) ride the same eased progress — their y-positions interpolate toward the class-header baseline and their opacity fades in parallel. When fully folded, children are visually collapsed into the header. Reversing the fold unwinds them. Cascade is pure derivation over the hierarchy, not a separate animation system.
- Decorator chips never animate with the body — they're part of the header and stay visible at every fold state.

**Out of scope**: bulk fold controls across all cards (parked to post-prototype), keyboard shortcuts for fold (M8).

**Tests**: fold progress interpolation per card, parent→children cascade (parameterized: class with 1/3/nested class of methods; verify children's y and opacity track parent's body_progress), two-button independent state correctness.

#### M3.5a — `HeaderModel` ADT and Python builder, single-row layout *(architectural)*

**Goal**: establish the language-agnostic header representation and a working Python builder. Layout is trivial one-row for now; the reflow engine is M3.5b.

**What ships**:
- `HeaderModel` ADT with language-agnostic blocks:
  - **Prelude**: decorator chips + keyword badge (`def`/`class`/`async def`/future `fn`/`pub fn`).
  - **Name**: the identifier.
  - **Params**: vector of `ParamChip { name, ty: Option, default: Option, kind: Regular|Star|Slash|Kwargs }`.
  - **Return**: `Option<TypeChip>`.
  - **Docstring**: `Option<Docstring>` — always below all of 1–4 when present; never participates in row reflow.
- Python builder: `python::build_header(ts_node, source) -> HeaderModel`. Maps Python AST shapes onto the universal structure.
- Stubs (documentation only, not implementations) for `rust::build_header`, `typescript::build_header`, `go::build_header` — demonstrating that the ADT covers those languages' function/method shapes.
- Renderer consumes `HeaderModel` directly; no language-specific code in the renderer path.
- Single-row layout: blocks 1–4 sit on one line in order; long signatures overflow horizontally (temporary — fixed in M3.5b). Docstring block sits below.
- Color-coded block backgrounds (muted tints consistent with the luminous-void palette — type cyan, default warm, return-type distinct hue, etc.).

**Why architectural**: this ADT is the seam for every future language. Getting the structure wrong now means rewriting every header-consuming path later. Python-first-and-refactor-later is explicitly rejected by this sub-milestone.

**Out of scope**: multi-row reflow (M3.5b), markdown rendering of docstring (later).

**Tests**: Python builder parameterized across `def`, `async def`, `class`, classmethod, staticmethod, property, dataclass, decorated functions, default values, type annotations, `*args`/`**kwargs`, `/` and `*` separators, functions with docstrings, no-docstring case. Verify `HeaderModel` inputs from a hand-rolled fixture render identically regardless of which builder produced them.

#### M3.5b — Block-flow reflow engine

**Goal**: when the single-row layout would overflow the plate width, the header reformats into multi-row structure.

**What ships**:
- Reflow rules, consumed by the renderer from M3.5a's `HeaderModel`:
  1. If the full row `[1][2][3 3 3 3][4]` fits the plate width: single row.
  2. If params overflow: `[1][2][3a][4]` on row 1 (block 4 pins top-right); subsequent rows `  [3a]`, `  [3a]`, ... (remaining params, indented under block 2's column).
  3. If the name column is also wide: `[1][2]    [4]` on row 1 (name and return together, block 4 still pinned top-right); params become their own indented column below.
- Block 4 (return type) stays top-right regardless of how blocks 2 and 3 wrap.
- Transitions when plate width changes (e.g. future zoom): reflow is smoothly animated via pure derivation on a `layout_width` parameter.
- Uses M3.1's RT — reflow triggers RT redraw.

**Out of scope**: any language not already supported; fold-aware reflow (the header is the header regardless of fold state).

**Tests**: reflow rules parameterized across a matrix of plate-widths × `HeaderModel` shapes (short signature / long params / long name + long params). Each case asserts block positions and row count.

#### M3.6 — Code-pane plate structure

**Goal**: the code pane reads as a physical plate floating in the void, with a persistent filename header and generous edge breathing room.

**What ships**:
- **Fixed header band** at top of plate (filename now; metadata chips in later milestones). Does not scroll. Think of it as a small *plank* sitting above the scrolling parchment — structurally its own object, even before the scroll rods appear in M3.7. Design it so that when M3.7 adds the rods, the plank visibly sits *on top of* the upper rod (see M3.7 for the physical-attachment detail).
- **Plate margins** from window edges: ~2.5–3% of `min(width, height)`. Visual gap between plate and window frame.
- **Plate height** = `min(content_height, window_height − margins)`. Short files produce a plate that hugs their content, not one stretched empty.
- **Scroll clipping** to the plate's rounded corners — no text leaks past the plate edge.
- Built on M3.1 (plate RT).

**Out of scope**: scroll-winding (M3.7), metadata chips beyond filename (M4+), horizontal scroll.

**Tests**: plate-size derivation (parameterized: tall file, short file, window resized), clip-mask correctness at rounded corners.

##### M3.6.1 — Plank-on-rods skeuomorphic connector *(cross-depends on M3.7)*

**Goal**: the filename plank (M3.6) visibly attaches to the scroll's upper rod (M3.7), so the eye reads the whole thing as one physical artifact — plank + rods + wound parchment — not three separate UI elements floating near each other.

**Depends on**: both M3.6 (plank exists) and M3.7 (rods exist). Ships as part of M3.7 in practice; sits here conceptually as a *refinement of the plank*.

**What ships**:
- A thin skeuomorphic connector where the plank meets the upper rod — hairline seam, or a subtle bright rivet on each side, whichever reads as "this plank is bolted to the rods" without being a photoreal screw.
- Plank shadow extends slightly over the top of the upper rod to reinforce "plank sits on top."
- Herald-scroll asymmetry: top has plank + rod, bottom has only rod. The asymmetry is part of the affordance ("this is the top / title end").

**Out of scope**: decorative rivets on other parts of the plate; bottom plank variants.

**Tests**: connector geometry at plate edge (parameterized with/without upper rod materialized).

#### M3.7 — Scroll-winding: pin zones with cylindrical curl

**Goal**: content above/below the visible window appears "wound on a scroll" at top and bottom of the plate — the key tactile metaphor.

**What ships**:
- **Top and bottom pin zones**, each exactly 1 line tall.
- Pin zones only materialize when off-screen content exists in that direction (top-of-file → no top pin; bottom-of-file → no bottom pin). Fade in/out smoothly.
- Content just past the visible window curls onto a cylinder in the pin zone — cylinder UV mapping sampled from the plate RT (M3.1). Foreshortened, faintly lit, not meant to be readable.
- No prominent rod/dowel body — just faint luminous **cap-glow** at the left and right plate edges where the dowel would exit, cueing "something is wound here."
- The header plank (M3.6) visibly attaches to the upper rod via the skeuomorphic connector detailed in **M3.6.1** — M3.7's rods make that connector meaningful.
- Continuous transition as the scroll offset crosses integer line boundaries: a line gradually curves from flat to fully wound.
- Works symmetrically top-rod-and-bottom-rod for the curl; plank (from M3.6) is top-only — herald-scroll asymmetry.

**Out of scope**: tree-aura link (M4.3), other plates getting the same treatment (future — the primitive is reusable), metadata chips in the plank beyond filename (M4+).

**Tests**: cylinder UV function (parameterized progress), pin-zone visibility logic (at top-of-file / mid / bottom-of-file / shorter-than-window cases), cap-glow intensity as a function of wound-content depth, plank-to-rod connector geometry at the top edge.

#### M3.8 — Background visibly alive

**Goal**: the breathing-sky background is perceptually present — subtly alive, not invisible.

**What ships**:
- Plate alpha tuned (~0.6) so the sky reads through tinted-glass-style.
- Breath/cloud amplitudes raised into perceptible range while staying well below "distracting."
- Optional gentle vignette giving the void a center of gravity.
- Plate itself gets a very subtle top→bottom tint (slight blue at top, dimming at bottom) reinforcing the tinted-glass feel.

**Out of scope**: cloud shapes with parallax, weather metaphors, anything narrative.

**Tests**: perceptual delta — not automated; verify by eye. (Consistent with "don't test rendering output" principle.)

##### M3.8.1 — Stars and nova-pulses

**Goal**: tiny bright pinpoints distributed across the void, modulated over time. Baseline dim-static stars with slow twinkle, plus rare nova-pulses (1 star briefly brightens 5–10×, fades over ~1.5s). Eventually gated by the linen-weave transparency (M3.3.1) so only stars behind weave holes come through the plate — until then, they appear everywhere against the nebula.

**What ships**:
- Hash-based sparse star field in `background.rs`: per-star center, phase, baseline brightness.
- Twinkle: slow low-amplitude per-star brightness modulation.
- Nova-pulse: rare, selected via hash + time, 5–10× peak, ~1.5s envelope, then full decay. Target frequency ~1 pulse every 20–40s somewhere in the viewport.
- **Decoupled-opacity contract**: stars are emitted as a *separate output layer* (not blended into the nebula uniformly), so the plate shader (M3.3.1) can apply a different pass-through rule — stars blocked by linen threads, dimmed but let through weave holes.

**Why ship before M3.3.1**: the linen's through-hole visibility is the *thing that contains* the stars. Building stars first means when linen lands, there's something for the holes to reveal. Building linen first means an empty weave pattern with nothing behind it — the payoff is in the pairing.

**Alternative considered**: plasma arcs / transient filaments in the nebula instead of classic stars. Rejected for v1 — more shader complexity, less instantly-readable. Can add later as a second layer.

**Tests**: brightness envelope math (pulse peak, decay), star count per unit area, hash-phase determinism.

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

### M4 sub-milestones

M4's main description covers the first pass (basic tree + code transitions + hover-glow + shared background animation). The sub-milestones below extend it with the "current file never scrolls out of view" behavior and the code↔tree aura link.

#### M4.1 — Basic tree

The main M4 description, implemented as stated.

#### M4.2 — Sticky-pinned current file

**Goal**: the currently-selected file is always visible in the tree, even when the user scrolls it out.

**What ships**:
- When the selected file's natural row scrolls outside the tree viewport, the row **detaches** from the list and floats at the nearest edge (top or bottom of the tree viewport).
- Detach/re-attach animations: smooth lift-off when leaving, smooth snap-back when the natural position returns into view.
- The pinned copy keeps its aura / glow state — it's still visibly "the selected file."
- Pure-derivation state: pin position is a function of scroll offset and the selected file's natural row offset, nothing event-driven.

**Out of scope**: sticky headers for directories (stretch), sticky for multiple selected items (not a selection model yet).

**Tests**: pinning logic (parameterized across scroll positions relative to the selected row), detach/attach transitions.

#### M4.3 — Tree ↔ code aura link *(independently-shippable)*

**Goal**: a visible luminous link between the code you're reading and the file it comes from. Tactile "cable" — you never lose track of what's where.

**What ships**:
- Code plate emits a **left-edge viewport indicator** — a thin luminous segment indicating what vertical slice of the file is currently visible.
- Ambient gradient bridges leftward from the viewport indicator across the void, landing on the tree row for that file.
- Brightens on interaction (scrolling the code, hovering either pane).
- Works with M4.2: when the file is pinned at the tree edge, the aura lands on the pinned copy.
- Reusable primitive: the same "plate → target-row aura" construct should later link any relevant pairs (e.g. commit list in M5, diff-file list in M6).

**Out of scope**: multi-target auras, 3D cable rendering (future).

**Tests**: viewport indicator position as a function of scroll offset + plate height + total file lines (parameterized), target-row resolution (pinned vs natural position).

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
- **Bulk fold HUD overlay**: a floating control to fold all cards' bodies / fold all docstrings / expand all, globally. Deferred — but when implementing the fold model in M3.4 and the icon system in M3.2, leave space for this: fold state is per-card (so a global toggle just iterates targets), icons for the control should be covered by Lucide's set. Don't build the HUD now; do build so nothing blocks it.
- **Actual 3D rotation of plates** (for repo browsing, "pages of the history book," branch-graph tilting). Deferred — but M3.1's offscreen-RT + 3D-quad architecture already makes this a drop-in feature rather than a rewrite.

---

## After the prototype

When M8 ships, the prototype is ready to show: a blog post, a demo video, feedback from the Zed / JetBrains design communities. That's the next decision point: keep iterating solo, look for collaborators, pursue integration with an existing editor, or productize standalone. Don't plan that now — plan it with the prototype in hand.
