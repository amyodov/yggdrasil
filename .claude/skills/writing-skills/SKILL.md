---
name: writing-skills
description: Writes or iterates on a Claude Code SKILL.md. Use when authoring a new skill in .claude/skills/, renaming or restructuring an existing one, or reviewing a skill for best-practice compliance. Forces a re-read of the official skill-authoring guidance before writing, so the rules don't drift out of context between sessions.
---

# Writing skills

Before touching any SKILL.md in this repo, fetch and re-read the
official skill-authoring guidance:

**https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices**

Don't rely on memory from a previous session; the rules are specific
and easy to misremember. Skim the doc each time before writing.

## Critical rules from the docs

- **Name**: gerund form (`processing-pdfs`, `writing-skills`);
  lowercase letters + digits + hyphens only; ≤ 64 chars; no reserved
  words (`anthropic`, `claude`).
- **Description**: third person ("Writes…" not "I write…" / "You
  can…"); state both *what* the skill does and *when* to use it;
  front-load the key use case and include trigger phrases; ≤ 1024
  chars.
- **Body voice**: directive, not promotional. Claude is already smart
  — only add context Claude doesn't already have. No metaphors, no
  mission statements, no "this is valuable because…". The consumer
  is another LLM instance; token density is the virtue.
- **Body length**: ≤ 500 lines. Split longer content into separate
  files referenced one level deep from SKILL.md.
- **No time-sensitive information**. Consistent terminology
  throughout. Forward slashes in paths.
- **Include a checklist** at the end for multi-step workflows.

## Checklist before shipping a skill

- [ ] Fetched the best-practices doc in this session.
- [ ] Name is gerund-form, lowercase-hyphen, under 64 chars.
- [ ] Description is third-person, specific, includes trigger phrases.
- [ ] Body voice is directive — no promotional / motivational prose,
      no metaphors that don't earn their tokens.
- [ ] Body under 500 lines; references one level deep if split.
- [ ] Consistent terminology.
- [ ] Checklist at the end (for multi-step skills).
- [ ] If the skill replaces or supersedes CLAUDE.md content, CLAUDE.md
      is trimmed and points at the skill.
