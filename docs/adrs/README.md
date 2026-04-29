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

| ADR                                         | Title                          | Status   | Tags                                  |
| ------------------------------------------- | ------------------------------ | -------- | ------------------------------------- |
| [0001](0001-accessibility-in-phase-zero.md) | Accessibility in Phase 0       | accepted | a11y, renderer                        |
| [0002](0002-performance-philosophy.md)      | Performance Philosophy         | accepted | renderer, core                        |
| [0003](0003-update-check-policy.md)         | Update Check Policy            | accepted | security, core                        |
| [0004](0004-kitty-graphics-in-core.md)      | Kitty Graphics in Core         | accepted | renderer, plugins                     |
| [0005](0005-lua-sandboxed-config.md)        | Lua 5.4 Sandboxed Config       | accepted | config, core                          |
| [0006](0006-scroll-buffer-architecture.md)  | Scroll Buffer Architecture     | accepted | renderer, core                        |
| [0007](0007-daemon-architecture.md)         | Daemon Architecture            | accepted | core, renderer                        |
| [0008](0008-shell-integration-timing.md)    | Shell Integration Timing       | accepted | core                                  |
| [0009](0009-bidi-ligature-preparedness.md)  | BiDi and Ligature Preparedness | accepted | renderer, core                        |
| [0010](0010-layout-tree-model.md)           | Layout Tree Model              | proposed | core                                  |
| [0011](0011-keybind-dispatch.md)            | Keybind Dispatch Architecture  | proposed | core                                  |
| [0012](0012-copy-mode-scrollback-access.md) | Copy Mode Scrollback Access    | proposed | core                                  |
| [0013](0013-fig-autocomplete-schema.md)     | Fig Autocomplete Schema        | proposed | context-engine, completion, plugins   |
| [0014](0014-input-classifier.md)            | Input Mode Classification      | proposed | context-engine, ai, shell-integration |
