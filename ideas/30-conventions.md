---
title: "Conventions"
status: draft
category: cross-cutting
description: "Naming, config syntax, keybinds, file structure"
tags: ["naming", "config-syntax", "style-guide"]
---
# Conventions


Naming and formatting standards across the project. When in doubt, check here.

## Config Naming

| Context | Convention | Example |
|---------|-----------|---------|
| Flat config keys | kebab-case | `font-family`, `theme-dark`, `scrollback-lines` |
| Flat config plugin namespace | `plugin-name.key` | `agent.default-provider`, `context-engine.ai-backend` |
| Lua config keys | snake_case | `font_size`, `ssh_domains`, `shell_integration` |
| Lua config plugin namespace | table | `plugins["agent-manager"].default_provider` |
| CLI flags | kebab-case with `--` prefix | `--log-level`, `--log-filter` |

Flat and Lua map 1:1. `font-family` in flat = `font_family` in Lua. The settings palette handles translation.

## Keybind Actions

All keybind action names use kebab-case:

```
split-right, split-down, split-float
toggle-sidebar, grid-view
copy-or-interrupt, paste
scroll-to-prompt-up, scroll-to-prompt-down
harpoon-mark, harpoon-menu
broadcast-toggle
```

## Palette Commands

All palette commands use `:` prefix + kebab-case:

```
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

- Idea docs: `NN-topic.md` — numbered for reading order, not priority
- Theme files: `name.toml` — in `~/.config/phantom/themes/`
- Plugin manifests: `phantom-plugin.toml`
- Config: `config` (flat) or `config.lua` (Lua) — in `~/.config/phantom/`

## Cross-References

When referencing another idea doc, use relative path: `See [Memory Management](15-memory-management.md)`.

When referencing a specific section, describe it: "the tiered scroll buffer (see [Memory Management](15-memory-management.md), Scroll Buffer Strategy section)".

## Frontmatter

Every idea doc has YAML frontmatter:

```yaml
---
title: "Feature Name"
status: draft
category: core
description: "One-line summary for the index"
tags: ["relevant", "keywords"]
---
```

### Status values

- **draft** — still collecting ideas, details may change
- **reviewing** — design is mostly complete, looking for feedback
- **decided** — design is locked, ready for implementation
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
