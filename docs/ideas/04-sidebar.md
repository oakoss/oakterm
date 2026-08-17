---
title: 'Sidebar'
status: draft
category: plugin
description: 'Collapsible process dashboard — agents, services, watchers, shells'
tags: ['ui', 'process-dashboard', 'notifications', 'agents', 'services']
---

# Sidebar

A collapsible process dashboard, docked left by default and configurable to either edge (`sidebar_position = "left" | "right"`). `Ctrl+B` to toggle.

Not a file tree. Not a session list. It shows **things that are running** — grouped by what they are. It doubles as the workspace switcher: click an entry and the main view swaps to it.

## States

**Collapsed** — icon strip with status badges:

```text
┌──┬────────────────────────────────────┐
│◉❓│                                    │
│◉ │  ~/project $ _                     │
│▶ │                                    │
│👁✓│                                    │
│● │                                    │
└──┴────────────────────────────────────┘
```

**Expanded** — names, status, metadata per entry:

```text
┌──────────────────┬─────────────────────────────┐
│ AGENTS           │                             │
│ ◉ feat/auth      │                             │
│   claude  ❓      │  ~/project $ _              │
│   ██████░░ 62%   │                             │
│──────────────────│                             │
│ SERVICES         │                             │
│ ▶ next dev       │                             │
│   :3000 ✓        │                             │
│──────────────────│                             │
│ WATCHERS         │                             │
│ 👁 vitest --watch │                             │
│   14/14 passing  │                             │
│──────────────────│                             │
│ SHELLS           │                             │
│ ● scratch        │                             │
└──────────────────┴─────────────────────────────┘
```

## Categories

**Agents** — autonomous processes that produce code and need review

- Status: five lifecycle states — `working` / `blocked` / `done` / `idle` / `unknown` — plus an outcome (`success` / `error` / `cancelled`) attached at completion ([ADR-0023](../adrs/0023-agent-state-vocabulary.md)). `done` moves to `idle` on daemon-global acknowledgment (any client viewing the pane clears it everywhere); the attention cycle drains `done+error`, then `blocked`, then `done+success`. `done+success` and `done+cancelled` age out via the recency window even if never acknowledged; `done+error` and `blocked` never age out.
- Context window %, branch, files changed
- Memory usage (child process RSS) with growth indicator

**Services** — long-running processes you want to keep alive

- Ports they're listening on (auto-detected)
- Health: running / crashed / restarting
- Memory usage
- Restart on crash

**Watchers** — processes that produce rolling status

- Test runners: pass/fail count
- Type checkers: error count
- Bundlers: build status

**Shells** — interactive sessions

## Memory Visibility

Every sidebar entry can show memory usage of its child process. This makes it immediately clear what's consuming resources — the terminal or something running inside it.

```text
┌──────────────────┐
│ AGENTS           │
│ ◉ feat/auth      │
│   claude  ❓      │
│   ██████░░ 62%   │
│   890 MB ⚠ ↑     │  ← child process memory, growing
│──────────────────│
│ SERVICES         │
│ ▶ next dev       │
│   :3000 ✓        │
│   142 MB         │  ← stable, no warning
│──────────────────│
│ TERMINAL    48 MB│  ← our own memory, always at the bottom
└──────────────────┘
```

The `⚠ ↑` indicator means the process memory is growing abnormally. A notification fires if it exceeds the configured threshold. See `ideas/15-memory-management.md` for the full memory strategy.

## Workspace Switching and Folders

As the workspace switcher, the sidebar lists workspaces and swaps the main view on click. For people who accumulate many workspaces (one per project, plus scratch contexts), flat listing stops scaling — so workspaces can be organized into **folders**: collapsible, named groups in the sidebar (Arc's spaces-and-folders pattern).

```text
┌──────────────────┐
│ WORKSPACES       │
│ ▼ work           │
│   ● api          │
│   ● frontend     │
│ ▼ oss            │
│   ● oakterm      │
│ ▶ archive        │  ← collapsed folder
└──────────────────┘
```

Folders are sidebar-side organization only — the daemon's `Workspace → Tab → Panes` model (03) is untouched; a folder is presentation metadata (name, member workspace ids, collapsed state) owned by the sidebar's data model and saved with the session. This keeps grouping out of the wire protocol and the mux invariants. Whether folder metadata lives client-side or daemon-side (so N clients see the same folders) is an open question for the ADR.

## How Things Get Into the Sidebar

1. **Explicitly** — `:agent claude`, `:service start "npm run dev"`, `:watch "vitest"`
2. **Automatically** — project detection matches files to commands
3. **Promotion** — `Ctrl+Shift+S` promotes a running shell process to a tracked entry

## Notifications

Each category has its own notification logic:

| Category | Notifies when                                                  |
| -------- | -------------------------------------------------------------- |
| Agent    | Enters `blocked`, reaches `done+error`, reaches `done+success` |
| Service  | Crashes, port conflict, restart loop                           |
| Watcher  | Tests go red, type errors appear, build fails                  |
| Shell    | Process exits (configurable)                                   |

`Cmd+Shift+U` cycles through everything that needs attention, draining agent entries in ADR-0023's priority order: `done+error`, then `blocked`, then `done+success` ([ADR-0023](../adrs/0023-agent-state-vocabulary.md)).

## Interaction

- Click entry → main view swaps to that pane
- Split main area → watch multiple panes side by side
- `Ctrl+G` → grid/expose view of all panes
- Drag to reorder
- Done agents auto-hide after merge

## The Sidebar Is a Plugin

The sidebar is a view into the data model. Someone who doesn't want it disables it. Someone who wants a bottom bar writes one. The core provides panes, metadata, and notifications. Plugins provide the presentation.

## Container Support (from Ptyxis)

A Docker/Podman plugin adds a CONTAINERS section to the sidebar. Auto-discovers running containers. Click to attach a shell. Shows image, status, ports.

## Related Docs

- [Plugin System](06-plugins.md) — sidebar data model (core) and sidebar-ui (plugin)
- [Agent Management](07-agent-management.md) — AGENTS section
- [ADR-0023](../adrs/0023-agent-state-vocabulary.md) — agent state vocabulary and attention-cycle ordering
- [Memory Management](15-memory-management.md) — per-pane memory display
- [Harpoon](27-harpoon.md) — pane bookmarks complement the sidebar
- [Remote Access](29-remote-access.md) — remote panes appear in sidebar
