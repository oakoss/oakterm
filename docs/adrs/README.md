# Architecture Decision Records

Decisions that resolve open questions from the idea docs. Each ADR records what was decided, what alternatives were considered, and why.

## Format

```text
NNNN-short-title.md
```

Numbered sequentially. Never renumber. Superseded ADRs stay in place with updated status.

## Status Lifecycle

```text
proposed → accepted → [superseded | deprecated]
```

- **proposed** — written, not yet agreed on
- **accepted** — decision is final, implementation can proceed
- **superseded** — replaced by a newer ADR (link to it)
- **deprecated** — no longer relevant

## Template

Copy [0000-template.md](0000-template.md) and renumber.

## Index

| ADR                                         | Title                      | Status   | Tags              |
| ------------------------------------------- | -------------------------- | -------- | ----------------- |
| [0001](0001-accessibility-in-phase-zero.md) | Accessibility in Phase 0   | accepted | a11y, renderer    |
| [0002](0002-performance-philosophy.md)      | Performance Philosophy     | accepted | renderer, core    |
| [0003](0003-update-check-policy.md)         | Update Check Policy        | accepted | security, core    |
| [0004](0004-kitty-graphics-in-core.md)      | Kitty Graphics in Core     | accepted | renderer, plugins |
| [0005](0005-lua-sandboxed-config.md)        | Lua 5.4 Sandboxed Config   | accepted | config, core      |
| [0006](0006-scroll-buffer-architecture.md)  | Scroll Buffer Architecture | accepted | renderer, core    |
