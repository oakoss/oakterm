---
title: 'Conventions'
status: draft
category: cross-cutting
description: 'Naming, config syntax, keybinds, file structure'
tags: ['naming', 'config-syntax', 'style-guide']
---

# Conventions

Naming and formatting standards across the project. When in doubt, check here.

## Config Naming

| Context                      | Convention                  | Example                                               |
| ---------------------------- | --------------------------- | ----------------------------------------------------- |
| Flat config keys             | kebab-case                  | `font-family`, `theme-dark`, `scrollback-lines`       |
| Flat config plugin namespace | `plugin-name.key`           | `agent.default-provider`, `context-engine.ai-backend` |
| Lua config keys              | snake_case                  | `font_size`, `ssh_domains`, `shell_integration`       |
| Lua config plugin namespace  | table                       | `plugins["agent-manager"].default_provider`           |
| CLI flags                    | kebab-case with `--` prefix | `--log-level`, `--log-filter`                         |

Flat and Lua map 1:1. `font-family` in flat = `font_family` in Lua. The settings palette handles translation.

## Keybind Philosophy

**Use familiar conventions. Don't invent new muscle memory.**

Users come from tmux, vim, VS Code, browsers, and their OS. Default keybinds should feel like second nature, not a new system to learn.

### Borrow from what people already know

| Source            | What we take                                                                    |
| ----------------- | ------------------------------------------------------------------------------- |
| **Their OS**      | Cmd+C/V (macOS), Ctrl+C/V (smart on Linux/Windows), Cmd+T/W for tabs            |
| **VS Code**       | Cmd+Shift+P for command palette, Cmd+, for settings, Ctrl+` for terminal toggle |
| **tmux**          | Ctrl+B prefix concept for multiplexer actions (but optional, not required)      |
| **Vim**           | j/k/h/l, gg/G, /, v/V/y in copy mode                                            |
| **Browsers**      | Ctrl+F for search, Ctrl+Tab for tab switching, Ctrl+Shift+T for reopen tab      |
| **Ghostty/Kitty** | Common terminal keybinds users already have in muscle memory                    |

### Never do this

- Don't bind critical actions to keys that conflict with common shell usage
- Don't require a prefix key for frequent actions (tmux's `Ctrl+B` before every command is the #1 complaint)
- Don't use obscure modifier combos (`Ctrl+Shift+Alt+F5`) for things you do 50 times a day
- Don't change the meaning of keys people already know (`Ctrl+C` must always be able to interrupt)

### Every keybind is remappable

Nothing is hardcoded. If our default conflicts with your workflow, change it.

## Keybind Actions

All keybind action names use kebab-case:

```text
split-right, split-down, split-float
toggle-sidebar, grid-view
copy-or-interrupt, paste
scroll-to-prompt-up, scroll-to-prompt-down
harpoon-mark, harpoon-menu
broadcast-toggle
```

## Palette Commands

All palette commands use `:` prefix + kebab-case:

```text
:agent, :merge, :diff, :agents
:health, :debug, :update
:settings, :keybinds, :theme, :plugins
:broadcast, :harpoon
:service start, :watch
```

## Plugin Naming

- Registry name: lowercase kebab-case (`agent-manager`, `docker-manager`, `browser-lite`)
- Display name: title case for UI (`Agent Manager`, `Docker Manager`)
- Config key: matches registry name (`plugins["agent-manager"]`)

## Theme Naming

- File name: lowercase kebab-case (`catppuccin-mocha.toml`, `high-contrast-dark.toml`)
- Display name: title case for UI (`Catppuccin Mocha`, `High Contrast Dark`)
- Variant field: `"dark"` or `"light"` (lowercase)

## Idea Doc Structure

Each idea doc in `ideas/` should follow this structure where applicable:

```markdown
# Feature Name

One-line description of what this is.

## [Problem / Why]

What problem does this solve? Why does it exist?

## [How It Works / Design]

The core design. Diagrams, ASCII mockups, examples.

## [Configuration]

Config examples in both flat and Lua format.

## [Plugin API / Primitives Used]

For plugins: which API primitives does this use?
For core: what does this expose to plugins?

## [What This Is Not]

Explicit scope boundaries. What we chose not to do and why.
```

Not every doc needs every section. Research docs (`10`, `11`, `16`) have their own format.

## File Naming

- Idea docs: `NN-topic.md` in `ideas/` — numbered for reading order, not priority
- Reviews: `YYYY-MM-DD-HHMMSS-short-title.md` in `docs/reviews/` — timestamped for ordering
- ADRs: `NNNN-short-title.md` in `docs/adrs/` — numbered sequentially, never renumber
- Specs: `NNNN-short-title.md` in `docs/specs/` — numbered sequentially
- Theme files: `name.toml` — in `~/.config/oakterm/themes/`
- Plugin manifests: `oakterm-plugin.toml`
- Config: `config` (flat) or `config.lua` (Lua) — in `~/.config/oakterm/`

## Cross-References

When referencing another idea doc, use relative path: `See [Memory Management](15-memory-management.md)`.

When referencing a specific section, describe it: "the tiered scroll buffer (see [Memory Management](15-memory-management.md), Scroll Buffer Strategy section)".

When referencing an ADR from an idea doc: `See [ADR-0001](../adrs/0001-accessibility-in-phase-zero.md)`.

When referencing a spec from an idea doc: `See [Spec-0001](../specs/0001-plugin-api.md)`.

ADRs and specs reference idea docs with: `See [Architecture](../ideas/01-architecture.md)`.

## Frontmatter

Every idea doc has YAML frontmatter:

```yaml
---
title: 'Feature Name'
status: draft
category: core
description: 'One-line summary for the index'
tags: ['relevant', 'keywords']
---
```

### Status values

- **draft** — still collecting ideas, details may change
- **reviewing** — design is mostly complete, looking for feedback
- **decided** — design is locked by an accepted ADR, ready for spec/implementation
- **implementing** — actively being built
- **reference** — research material, not a design spec

### Category values

- **core** — ships in the binary, not a plugin
- **plugin** — bundled plugin, can be disabled
- **community-plugin** — designed for community to build
- **cross-cutting** — applies across core and plugins
- **research** — background research, not a design spec

### Tags

Lowercase, kebab-case. Used for finding related docs. Common tags:

- Platform: `macos`, `linux`, `windows`, `cross-platform`, `wayland`, `wsl`
- Tech: `rust`, `wgpu`, `wasm`, `wasmtime`, `lua`, `toml`
- Feature area: `ui`, `keybinds`, `fonts`, `themes`, `shell`, `agents`, `notifications`
- Concern: `a11y`, `security`, `memory`, `latency`, `testing`, `ci`

## Related Docs

- [Configuration](09-config.md) — authoritative config syntax reference
- [Plugin System](06-plugins.md) — plugin naming and manifest format
- [Theming](22-theming.md) — theme file naming
- [ADR Conventions](../adrs/README.md) — ADR template and status lifecycle
- [Spec Conventions](../specs/README.md) — spec template and status lifecycle
- [Review Conventions](../reviews/README.md) — review template and format
