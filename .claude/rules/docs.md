---
paths:
  - 'docs/**/*.md'
---

# Documentation Conventions

## Idea Docs (`docs/ideas/`)

- Follow [docs/ideas/30-conventions.md](docs/ideas/30-conventions.md) — YAML frontmatter, sections for Problem, Design, Configuration, Plugin API, What This Is Not
- **Frontmatter status**: `draft → reviewing → decided → implementing → reference`
- **Frontmatter category**: core, plugin, community-plugin, cross-cutting, research
- An accepted ADR moves the idea doc status from `reviewing` to `decided`

## ADRs (`docs/adrs/`)

- Format: `NNNN-short-title.md`, numbered sequentially, never renumber
- **Status**: `proposed → accepted → [superseded | deprecated]`
- One ADR per decision. Link to the idea docs that surfaced the question.
- Template: [docs/adrs/README.md](docs/adrs/README.md)

## Specs (`docs/specs/`)

- Format: `NNNN-short-title.md`, numbered sequentially
- **Status**: `draft → review → accepted → implementing → complete`
- One spec per bounded concern (API surface, wire protocol, data format)
- Trekker tasks reference specs. Implementation builds what specs define.
- Template: [docs/specs/README.md](docs/specs/README.md)

## Reviews (`docs/reviews/`)

- Format: `YYYY-MM-DD-HHMMSS-short-title.md`, timestamped
- Point-in-time snapshots — findings may become stale
- Surface corrections (fix directly), contradictions (write ADRs), missing specs
- Template: [docs/reviews/README.md](docs/reviews/README.md)

## General

- **Config naming**: snake_case in Lua (per ADR-0005)
- **Plugin manifest naming**: kebab-case in TOML (`oakterm-plugin.toml`)
- **Plugin naming**: lowercase kebab-case registry name, title case display name
- **Theme naming**: lowercase kebab-case file name (`catppuccin-mocha.lua`), title case display name
- **Keybinds**: borrow from OS/VS Code/tmux/vim/browser conventions, never invent new muscle memory
- **Cross-references**: relative paths — check all resolve to real docs
- **Markdown**: fenced code blocks always have a language tag
- **Frontmatter**: always verify it's complete and status is correct
- **Specs**: all types defined, no hand-waving ("TBD", "details later")
