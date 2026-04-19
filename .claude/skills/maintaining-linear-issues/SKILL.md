---
name: maintaining-linear-issues
description: Maintains Linear tickets as the agent-to-agent memory channel for the Yggdrasil project. Use when creating or editing a YGG issue, picking up a tracked feature, shipping a commit that closes or affects an issue, recording a design decision as a comment, or noticing that a change invalidates another ticket's assumptions.
---

# Maintaining Linear issues

Linear is this project's agent-to-agent memory channel. Between
eyes-on reviews and a cold-started LLM instance (possibly after
compaction, possibly in a new session), tickets carry the context
that conversations lose. Write for a future instance of yourself;
read the full state on every pickup.

Project: `Yggdrasil`. Team: `Yggdrasil`. IDs: `YGG-<n>`. Tool: the
`linear` MCP server.

## Write for a future instance of yourself

When creating or updating an issue, include:

- The concrete change (what / where / success criteria).
- Design rationale — why this approach; what alternatives were
  considered and rejected.
- Constraints that shaped the decision (performance, API, visual
  grammar, invariants that must hold).
- Pointers: related tickets (`YGG-<n>`), code locations, prior
  commits.
- Specific and concise. Token density matters — the future reader
  re-reads with limited context budget.

Don't include: implementation trace, file paths, clippy notes, line
counts, test-run output. Those live in the diff.

## Pull the full state before acting

Before starting work on any `YGG-<n>`:

1. Read the description.
2. Read all comments in chronological order.
3. Read linked tickets (`blockedBy`, `blocks`, parent, children).
4. Verify the description's assumptions still match the current
   codebase. If they don't, update the description or add a comment
   noting the shift before continuing.

The human may have edited the ticket between visits. Don't cache
from memory; re-read.

## Cross-ticket impact — update affected tickets when something shifts

When a change invalidates or reshapes another ticket's scope, update
that ticket:

- Add a comment: "Context: commit `<SHA>` changed `<thing>`. Now
  `<what's actually true>`. This affects `<specific part of this
  ticket's scope>`."
- If the shift is material, edit the description to match current
  reality.

A ticket whose description no longer matches reality is worse than
no ticket.

## When to create an issue

- Planned sub-milestone from `TASKS.md`: yes.
- Architectural decision (new trait, new module, new primitive): yes.
- Bug fix requiring a design call: yes.
- Visual polish / tuning iteration (<30 min, no design call): no.
- Mechanical bug fix: no.

When in doubt, create. Five lines of description is sufficient.

## Status transitions

- **Backlog → In Progress**: when starting the first commit for this
  issue.
- **In Progress → Done**: when the defining deliverable has shipped.

### Part 1 / Part 2 splits

When scope exceeds one commit:

- If Part 2 ships next: keep the original issue In Progress; the
  Part 1 ship note flags what remains.
- If Part 2 lands later after other work: mark the original Done,
  create a successor issue, cross-link both directions.

## Ship notes — required on every closing commit

Each commit that closes or partially closes an issue gets a comment
on that issue containing:

- Commit SHA (short form).
- What specifically landed.
- What's deferred. If it became a new issue, link it.

One short paragraph per item. Directive, not narrative.

## Record design decisions when they happen

When a judgment call during implementation shapes the feature
(approach A chosen over B, a constraint surfaced that reshaped the
design), add a comment capturing the reason.

Test: imagine being asked "why X instead of Y?" in six months. If
the answer isn't obvious from the commit message and current code,
record it.

## Scope drift — edit, don't annotate

When implementation reveals the original description was wrong, edit
the description. A corrective comment trailing after a stale
description is not an alternative.

## Blockers

Set `blockedBy` at creation when dependencies are known; set `blocks`
on the dependency. Update both if the dependency graph shifts.

## Commit messages

Commit subjects don't cite YGG-IDs — the prose-first commit style is
in CLAUDE.md. Linear comments cite commits; commits don't cite Linear.

## Closing out

When the final part ships: mark Done, add the final ship note,
cross-reference any successor issues. Stop editing the ticket unless
a bug reopens the question.

## Checklist — run at every commit touching a tracked feature

- [ ] Did I read the full ticket state before starting?
- [ ] Does this commit close or partially close a YGG issue? → ship note.
- [ ] Did implementation reveal the description was wrong? → edit it.
- [ ] Did I make a judgment call worth recording? → comment with why.
- [ ] Does this change invalidate another ticket's assumptions? → update that ticket.
- [ ] Status correct for where the work is now?
- [ ] Did this commit reveal new work that needs its own issue?
