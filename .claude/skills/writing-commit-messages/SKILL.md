---
name: writing-commit-messages
description: Writes git commit messages for the Yggdrasil repo in the prose-first style. Use when composing any git commit. Subject reads as a headline; body explains purpose (why the change matters to its consumer) and says what is now true that was not before. Implementation detail — magic numbers, shader coefficients, which branch was taken, file paths, linter fixes — stays in the diff, not in the log.
---

# Writing commit messages

The Yggdrasil git log is read far more often than it is written. The
subject and body carry the design-level story of what was decided;
the implementation trace lives in the diff or the PR thread.

## Subject

- One line. Reads as a headline.
- Title-case first word. Period at the end.
- Fits `git log --oneline` in a terminal (~72 chars is the ceiling).
- Names the change in terms a *consumer* of the code would use — what
  the user or a future developer sees, not what the grammar emitted.

**Good subjects:**
- `Glint traces the sky's east-west arc; the foil breathes with the sky.`
- `Rust and Markdown: real card extraction, not placeholder blobs.`
- `M3.5b: reflow engine picks how the header wraps.`

**Bad subjects:**
- `Update src/renderer.rs and src/composite.rs` — inventory, no meaning.
- `Fix bug` — no specificity.
- `Refactor CardKind enum to add Section variant` — HOW framing.

## Body

Required for anything beyond a trivial mechanical change. Explains
the **purpose**:

- Why the change matters to its consumer.
- What is now true that was not before.
- What problem resolved, what experience enabled, what future work
  unlocked.

The consumer is usually the user (visible change), sometimes a
future developer (architectural refactor that unlocks later work).

Prose paragraphs, not bullet lists. Two to four paragraphs is
typical.

### Good: purpose-first

> The virtual star is now an infinity-direction object, not a point
> on the plate: every lens sees the same angle, so the glint rises
> from the east, passes overhead at noon, and sets in the west — the
> physically honest arc. The old absolute-point anchor biased all
> lenses toward the plate's centre-left, which meant the glint drifted
> only through the top-left quadrant regardless of time of day.

### Bad: HOW-narrated

> Changed `sun_x = SUN_ANCHOR_X_PT + SUN_VIRTUAL_DISTANCE_PT *
> sky.direction.x` to `sun_x = lens_x + SUN_VIRTUAL_DISTANCE_PT *
> sky.direction.x`, removed `SUN_ANCHOR_X_PT`, updated the
> `spec_angle` derivation in `lens_pipeline.rs`.

## Avoid the HOW

These do NOT belong in the body:

- Magic numbers, shader coefficients (`0.82 → 0.75`, `smoothstep(0.7, 1.0)`).
- Which branch was taken, which enum variant renamed.
- File paths touched, line counts, diff stats.
- Clippy suggestions applied, linter fixes.
- Migration-style "removed X, added Y" inventories.
- Test-run output, which cases pass.

**Exception**: when the decision itself is the subject (e.g.,
"switched from per-plate anchor to infinity-direction"), a brief
mechanism note is legitimate because the mechanism *is* the point.
Even then — state the decision, don't narrate the edit.

## Trailer

Every commit gets the co-authorship trailer:

```
Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
```

Model ID matches whichever model actually wrote the code. The
trailer produces the honest "amyodov and Claude committed together"
reading in GitHub's UI.

## Eyes-on vs remote-control commits

- **Eyes-on**: commit reflects the *final outcome* of an iteration
  session, not the tweak-by-tweak journey. If we tried four ideas
  and kept the fourth, the message describes what was kept — not
  the three rejected.
- **Remote-control**: one commit per logically self-contained change.
  Each commit still follows the same subject+body+trailer discipline.

## Checklist — run before every `git commit`

- [ ] Subject is a headline: title-case first word, period at end, ≤72 chars.
- [ ] Body says WHY, not HOW.
- [ ] No magic numbers, coefficients, or shader-tuning specifics.
- [ ] No file paths, line counts, diff stats.
- [ ] No linter / clippy narration.
- [ ] Body says what is now true that was not before.
- [ ] Co-Authored-By trailer present with correct model ID.
- [ ] If this is an eyes-on batch: message describes the final shape,
      not the tweak history.
