---
title: "License"
status: decided
category: cross-cutting
description: "MPL 2.0 — core stays open, registry requires open source"
tags: ["mpl-2.0", "open-source", "plugin-licensing"]
---
# License


## MPL 2.0 (Mozilla Public License 2.0)

### Why MPL 2.0

The project has two goals that are in tension:
1. **Always open source** — the core can never be made proprietary
2. **Maximum freedom** — people can take it and do what they want

MPL 2.0 is the best compromise:

- **Core stays open** — modifications to our files must remain open source (file-level copyleft)
- **Plugins can be anything** — WASM plugins are clearly separate files, can be any license (MIT, proprietary, GPL, whatever)
- **Corporate friendly** — companies can use it, contribute to it, and build proprietary plugins without legal friction
- **Patent protection** — explicit patent grant protects users and plugin authors
- **Compatible** — explicitly compatible with Apache 2.0 and GPLv2+

### What it means in practice

| Action | Allowed? |
|--------|----------|
| Use for any purpose | Yes |
| Fork and modify | Yes — modified files must stay MPL 2.0 |
| Add new proprietary files around it | Yes |
| Build proprietary plugins | Yes — WASM plugins are separate files |
| Distribute in a commercial product | Yes |
| Close the source of our files | No — file-level copyleft prevents this |
| Contribute back | No obligation, but modified MPL files must be shared if distributed |

### Why not the alternatives

| License | Problem for us |
|---------|---------------|
| MIT / Apache 2.0 | Someone can fork the entire codebase and close it. "Always open source" is not guaranteed. |
| GPLv3 (Ghostty) | WASM plugins may be considered derivative works — could force all plugins to be GPL. Blocks corporate contributors at Google, Apple, etc. |
| AGPLv3 (cmux) | Most corporate-toxic license. Google bans all AGPL software. Overkill for a local application. |
| BSD | Functionally identical to MIT, no advantage. |

### Precedent

MPL 2.0 is used by Firefox, Thunderbird, and LibreOffice. It's well-understood, battle-tested, and designed specifically for this balance of openness + freedom.

### Plugin License Policy

**Official registry: open source required.**

Any plugin listed in the official registry (`phantom plugin install <name>`) must be open source — MIT, Apache, MPL, GPL, or any OSI-approved license. This ensures:
- The ecosystem is transparent and auditable (critical — plugins run in your terminal)
- Users can inspect what a plugin does before trusting it with capabilities like `pane.output` or `network`
- Plugin authors can build on each other's work
- Security researchers can review plugins for malicious behavior

**Sideloading: unrestricted.**

Users can install any WASM binary from a URL or local path:
```
phantom plugin install --from ./my-plugin.wasm
phantom plugin install --from https://example.com/plugin.wasm
```

Sideloaded plugins show a clear warning: "This plugin is not from the official registry and has not been reviewed." The user accepts the risk. No license restriction on sideloads.

**Summary:**

| Source | License requirement | Review status |
|--------|-------------------|---------------|
| Bundled plugins | MPL 2.0 (same as core) | Maintained by us |
| Official registry | Any OSI-approved open source license | Source link verified |
| Sideloaded | No restriction | Unreviewed, user accepts risk |
| Locale packs (registry) | Any OSI-approved open source license | Source link verified |
| Themes (registry) | Any OSI-approved open source license | Source link verified |

This keeps the official ecosystem open and trustworthy while not preventing anyone from running whatever they want on their own machine.

## Related Docs

- [Plugin System](06-plugins.md) — registry open source requirement
- [Conventions](30-conventions.md) — plugin licensing conventions
