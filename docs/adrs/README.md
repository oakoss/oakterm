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

| ADR                                          | Title                           | Status   | Tags                                  |
| -------------------------------------------- | ------------------------------- | -------- | ------------------------------------- |
| [0001](0001-accessibility-in-phase-zero.md)  | Accessibility in Phase 0        | accepted | a11y, renderer                        |
| [0002](0002-performance-philosophy.md)       | Performance Philosophy          | accepted | renderer, core                        |
| [0003](0003-update-check-policy.md)          | Update Check Policy             | accepted | security, core                        |
| [0004](0004-kitty-graphics-in-core.md)       | Kitty Graphics in Core          | accepted | renderer, plugins                     |
| [0005](0005-lua-sandboxed-config.md)         | Lua 5.4 Sandboxed Config        | accepted | config, core                          |
| [0006](0006-scroll-buffer-architecture.md)   | Scroll Buffer Architecture      | accepted | renderer, core                        |
| [0007](0007-daemon-architecture.md)          | Daemon Architecture             | accepted | core, renderer                        |
| [0008](0008-shell-integration-timing.md)     | Shell Integration Timing        | accepted | core                                  |
| [0009](0009-bidi-ligature-preparedness.md)   | BiDi and Ligature Preparedness  | accepted | renderer, core                        |
| [0010](0010-layout-tree-model.md)            | Layout Tree Model               | accepted | core                                  |
| [0011](0011-keybind-dispatch.md)             | Keybind Dispatch Architecture   | accepted | core                                  |
| [0012](0012-copy-mode-scrollback-access.md)  | Copy Mode Scrollback Access     | accepted | core                                  |
| [0013](0013-fig-autocomplete-schema.md)      | Fig Autocomplete Schema         | proposed | context-engine, completion, plugins   |
| [0014](0014-input-classifier.md)             | Input Mode Classification       | proposed | context-engine, ai, shell-integration |
| [0015](0015-command-blocks.md)               | Command Blocks UX               | accepted | renderer, shell-integration, plugins  |
| [0016](0016-tmux-coexistence.md)             | tmux Coexistence Stance         | accepted | core, multiplexer                     |
| [0017](0017-rust-implementation-language.md) | Rust Implementation Language    | accepted | core, renderer, security              |
| [0018](0018-gpu-rendering-wgpu.md)           | GPU Rendering via wgpu          | accepted | renderer, core, abstraction           |
| [0019](0019-wasm-plugin-runtime-wasmtime.md) | WASM Plugin Runtime (Wasmtime)  | accepted | plugins, core, security, abstraction  |
| [0020](0020-daemon-upgrade-version-skew.md)  | Daemon Upgrade and Version Skew | accepted | core                                  |
