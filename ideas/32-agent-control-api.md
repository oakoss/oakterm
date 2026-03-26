---
title: 'Agent Control API'
status: draft
category: core
description: 'CLI for agents to interact with and control the terminal'
tags: ['agents', 'cli', 'api', 'control', 'permissions', 'socket']
---

# Agent Control API

A CLI (`phantom ctl`) that lets agents — or any process running in a pane — interact with the terminal. Not an MCP server. Just a binary that talks to the daemon over its Unix socket.

## Why CLI

- Works with any agent (Claude Code, Codex, Aider, Goose, custom scripts)
- No protocol to implement — it's just a command
- Debuggable — run `phantom ctl` yourself in a shell to test
- Scriptable — works in bash scripts, makefiles, CI
- Already available — the daemon socket exists for the server/client architecture
- Agents can use it via tool_use/bash without any special integration

## The CLI

```bash
phantom ctl <command> [args]
```

The `ctl` subcommand connects to the running daemon via `$PHANTOM_SOCKET` (auto-set in every pane's environment). The daemon knows which pane the request came from.

### Pane Management

```bash
# Create panes
phantom ctl pane create                           # new shell pane (tiled)
phantom ctl pane create --floating                # floating pane
phantom ctl pane create --drawer bottom           # bottom drawer
phantom ctl pane create --popup                   # centered popup
phantom ctl pane create --command "npm test"      # run a command
phantom ctl pane create --popup --command "lazygit"

# List panes
phantom ctl pane list                             # all panes (JSON)
phantom ctl pane list --format table              # human-readable

# Read output from another pane
phantom ctl pane output <pane-id>                 # last 100 lines
phantom ctl pane output <pane-id> --lines 500     # last 500 lines
phantom ctl pane output <pane-id> --follow        # stream new output

# Send input to another pane
phantom ctl pane input <pane-id> "npm run build"
phantom ctl pane input <pane-id> --enter          # press enter

# Focus
phantom ctl pane focus <pane-id>                  # switch view to a pane

# Close
phantom ctl pane close <pane-id>
```

### Self (current pane)

```bash
# Set metadata on the calling pane
phantom ctl self set-title "Building auth module"
phantom ctl self set-status working               # working, needs-input, done, error
phantom ctl self set-color "#a6e3a1"              # tab/sidebar accent color
phantom ctl self set-progress 65                  # progress bar (0-100)
phantom ctl self set-badge "3 files changed"

# Read own pane info
phantom ctl self info                             # JSON: pane-id, cwd, title, status
```

### Notifications

```bash
phantom ctl notify "Build complete"                           # simple notification
phantom ctl notify "Tests failed" --level error               # error badge
phantom ctl notify "Approve changes?" --level warn --sticky   # stays until dismissed
```

### Sidebar

```bash
phantom ctl sidebar set-section "Build" --entries '[...]'     # custom section (JSON)
phantom ctl sidebar add-entry --section agents --label "cleanup" --status working
```

### Prompts (get user input)

```bash
# Show a popup asking the user a question, return their answer
ANSWER=$(phantom ctl prompt "Use sliding window or token bucket?" --choices "sliding,token")
echo "User chose: $ANSWER"

# Yes/no confirmation
phantom ctl confirm "Merge feat/auth to main?"
# Exit code 0 = yes, 1 = no

# Free text input
RESPONSE=$(phantom ctl prompt "Enter the API endpoint:" --input)
```

### Environment

```bash
# Read terminal/pane info
phantom ctl env pane-id                           # current pane ID
phantom ctl env workspace                         # current workspace name
phantom ctl env panes                             # JSON list of all panes
phantom ctl env version                           # terminal version
```

## Permission Model

Not every agent should be able to do everything. Permissions are **per-pane**, set when the pane is created.

```lua
-- When launching an agent
agent_permissions = {
  self = true,          -- can set own title, status, color (always allowed)
  notify = true,        -- can send notifications (default: true)
  pane_create = true,   -- can open new panes (default: false)
  pane_read = false,    -- can read other panes' output (default: false)
  pane_input = false,   -- can send input to other panes (default: false)
  pane_close = false,   -- can close other panes (default: false)
  sidebar = false,      -- can modify sidebar (default: false)
  prompt = true,        -- can ask user for input (default: true)
}
```

Flat config:

```text
agent.permissions.self = true
agent.permissions.notify = true
agent.permissions.pane-create = false
agent.permissions.pane-read = false
agent.permissions.pane-input = false
```

### Default permissions

| Permission    | Default        | Why                                                           |
| ------------- | -------------- | ------------------------------------------------------------- |
| `self`        | Always allowed | An agent should always be able to set its own status          |
| `notify`      | Allowed        | Notifications are passive — they don't control anything       |
| `prompt`      | Allowed        | Asking the user a question is safe — user controls the answer |
| `pane_create` | Denied         | Opening panes is a visible action — opt-in                    |
| `pane_read`   | Denied         | Reading other panes could expose secrets                      |
| `pane_input`  | Denied         | Sending input to other panes could execute commands           |
| `pane_close`  | Denied         | Closing panes could destroy work                              |
| `sidebar`     | Denied         | Modifying the sidebar could be confusing                      |

### Escalation

If an agent tries a denied action, the terminal can prompt the user:

```text
┌──────────────────────────────────────────────────┐
│  Agent "feat/auth" wants to:                     │
│  Read output from pane "dev-server"              │
│                                                  │
│  [Allow Once]  [Allow Always]  [Deny]            │
└──────────────────────────────────────────────────┘
```

"Allow Always" updates the pane's permission config for this session.

## Environment Variables

Every pane gets these environment variables automatically:

```bash
PHANTOM_SOCKET=/tmp/phantom-<uid>/socket    # daemon socket path
PHANTOM_PANE_ID=pane-a1b2c3d4               # this pane's unique ID
PHANTOM_WORKSPACE=work                       # current workspace name
PHANTOM_VERSION=0.7.0                        # terminal version
```

Agents (and scripts) use these to talk to the daemon. If `PHANTOM_SOCKET` is unset, `phantom ctl` knows it's not running inside the terminal and exits with a helpful error.

## Use Cases

### Agent sets its own status as it works

```bash
phantom ctl self set-status working
phantom ctl self set-title "Analyzing codebase"
# ... does work ...
phantom ctl self set-progress 50
phantom ctl self set-title "Writing tests"
# ... does more work ...
phantom ctl self set-status done
phantom ctl self set-badge "4 files, 12 tests"
phantom ctl notify "feat/auth complete" --level success
```

The sidebar and tab automatically reflect these updates in real-time.

### Agent opens a test runner to verify its work

```bash
phantom ctl pane create --drawer bottom --command "npm test"
# waits for tests...
TEST_OUTPUT=$(phantom ctl pane output $TEST_PANE --lines 5)
# reads results, continues working
```

### Agent asks user for a decision

```bash
APPROACH=$(phantom ctl prompt "Rate limiting approach?" --choices "sliding-window,token-bucket,leaky-bucket")
# Agent uses the answer to guide its implementation
```

### Script that sets up a dev environment

```bash
#!/bin/bash
# dev-setup.sh — run inside the terminal
phantom ctl pane create --command "npm run dev" --title "Dev Server"
phantom ctl pane create --drawer bottom --command "vitest --watch" --title "Tests"
phantom ctl pane create --floating --command "docker compose up" --title "Docker"
phantom ctl notify "Dev environment ready"
```

## What This Is Not

- Not an MCP server — it's a CLI. No protocol beyond "run a command, get output."
- Not a REST API — no HTTP, no JSON-RPC. Just Unix socket + CLI.
- Not unrestricted — every dangerous action requires explicit permission.
- Not required — agents work fine without it. It's an enhancement, not a dependency.

## Related Docs

- [Agent Management](07-agent-management.md) — the plugin that manages agent lifecycle
- [Sidebar](04-sidebar.md) — where agent status appears
- [Security](21-security.md) — permission model principles
- [Remote Access](29-remote-access.md) — the daemon socket this CLI connects to
- [Architecture](01-architecture.md) — server/client daemon model
