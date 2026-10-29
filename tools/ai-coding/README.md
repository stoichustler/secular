# Secular AI coding library

This directory is the **only** in-tree skills library for Secular. Grok and
Codex pick it up at session start through the repo-root `AGENTS.md`. Do not
add `.agents/`, `.grok/`, or `.codex/` skill trees.

```text
tools/ai-coding/
  skills/<name>/SKILL.md   # one skill per directory
  rules/                   # extra coding rules
  agents/                  # named subagent prompts
  skills.json              # index of skills in this tree
```

## Adding a skill

1. Create `tools/ai-coding/skills/<name>/SKILL.md` with YAML frontmatter
   (`name`, `description`) and the procedure body.
2. Add an `entries` path for it in `tools/ai-coding/skills.json`.
3. Keep the skill Secular-specific. Do not reintroduce a parallel Fuchsia
   `.agents/` tree.
