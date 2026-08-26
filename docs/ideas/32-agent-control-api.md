---
title: 'Agent Control API'
status: decided
category: core
description: 'CLI for agents to interact with and control the terminal'
tags: ['agents', 'cli', 'api', 'control', 'permissions', 'socket']
---

# Agent Control API

A CLI (`oakterm ctl`) that lets agents — or any process running in a pane — interact with the terminal. Not an MCP server. Just a binary that talks to the daemon over its Unix socket.

## Why CLI

- Works with any agent (Claude Code, Codex, Aider, Goose, custom scripts)
- No protocol to implement — it's just a command
- Debuggable — run `oakterm ctl` yourself in a shell to test
- Scriptable — works in bash scripts, makefiles, CI
- Already available — the daemon socket exists for the server/client architecture
- Agents can use it via tool_use/bash without any special integration

## The CLI

```bash
oakterm ctl <command> [args]
```

The `ctl` subcommand connects to the running daemon via `$OAKTERM_SOCKET` (auto-set in every pane's environment). The daemon knows which pane the request came from.

Three layers of the same control surface, each for a different caller: a `SKILL.md` (planned, TREK-278) will give agents workflow guidance without reading this doc; the `oakterm ctl` CLI below is what shell scripts and most agents call; long-lived programs can hit the raw daemon socket directly. This doc sketches the surface; the forthcoming Spec-0012 (Agent Control API, TREK-278) formalizes the wire contract, ID scheme, and precedence rules the daemon enforces.

### Pane Management

```bash
# Create panes
oakterm ctl pane create                           # new shell pane (tiled)
oakterm ctl pane create --floating                # floating pane
oakterm ctl pane create --drawer bottom           # bottom drawer
oakterm ctl pane create --popup                   # centered popup
oakterm ctl pane create --command "npm test"      # run a command
oakterm ctl pane create --popup --command "lazygit"

# List panes
oakterm ctl pane list                             # all panes (JSON)
oakterm ctl pane list --format table              # human-readable

# Read output from another pane
oakterm ctl pane read <pane-id>                                  # last 100 lines, --source recent (default)
oakterm ctl pane read <pane-id> --lines 500                      # last 500 lines
oakterm ctl pane read <pane-id> --follow                         # stream new output
oakterm ctl pane read <pane-id> --source visible                 # current viewport only
oakterm ctl pane read <pane-id> --source recent-unwrapped        # scrollback with soft wraps joined back — the form `wait output` matches against

# Send input to another pane — split by what a TTY actually accepts (herdr precedent)
oakterm ctl pane send-text <pane-id> "npm run build"             # literal text, no Enter
oakterm ctl pane send-keys <pane-id> Enter                       # keypresses: Enter, Esc, C-c, ...
oakterm ctl pane send-input <pane-id> "npm run build" Enter      # atomic literal + keys, in order

# Block until a condition is met, instead of polling
oakterm ctl wait output <pane-id> --match "ready on port 3000" --timeout 30000
oakterm ctl wait output <pane-id> --regex "port \d+" --timeout 30000
oakterm ctl wait status <pane-id> --status done --timeout 60000

# Focus
oakterm ctl pane focus <pane-id>                  # switch view to a pane

# Close
oakterm ctl pane close <pane-id>

# Return a hook-claimed pane to plain-shell state (agent exited or was released explicitly)
oakterm ctl pane release <pane-id>                # scoped per ADR-0024 rule 5: only panes an agent claimed on top of a surviving shell; a pane whose child process *is* the agent exits via rule 1 instead
```

Pane IDs have a **dual stable + compact form**, matching herdr's contract: responses always return the stable opaque ID (`OAKTERM_PANE_ID`-shaped, stable for the pane's lifetime); requests accept either the stable ID or the compact human-readable shorthand (e.g. `1-2`), which may renumber when peers close. Agents script against the stable form; humans type the compact one interactively.

### Self (current pane)

```bash
# Set metadata on the calling pane
oakterm ctl self set-title "Building auth module"
oakterm ctl self set-status working               # working | blocked | done [--outcome success|error|cancelled]
oakterm ctl self set-color "#a6e3a1"              # tab/sidebar accent color
oakterm ctl self set-progress 65                  # progress bar (0-100)
oakterm ctl self set-badge "3 files changed"      # free-form detail string, doesn't affect cycling/filtering

# Read own pane info
oakterm ctl self info                             # JSON: pane-id, cwd, title, status
```

`set-status` only accepts the three states a pane can self-report (ADR-0023): `working`, `blocked`, and `done` (paired with `--outcome success|error|cancelled`). `idle` and `unknown` are not self-reportable — `idle` is reached only by daemon-global acknowledgment of a `done` pane, and `unknown` is what the heuristic floor reports when nothing else has spoken.

### Notifications

```bash
oakterm ctl notify "Build complete"                           # simple notification
oakterm ctl notify "Tests failed" --level error               # error badge
oakterm ctl notify "Approve changes?" --level warn --sticky   # stays until dismissed
```

### Sidebar

```bash
oakterm ctl sidebar set-section "Build" --entries '[...]'     # custom section (JSON)
oakterm ctl sidebar add-entry --section agents --label "cleanup" --status working
```

### Prompts (get user input)

```bash
# Show a popup asking the user a question, return their answer
ANSWER=$(oakterm ctl prompt "Use sliding window or token bucket?" --choices "sliding,token")
echo "User chose: $ANSWER"

# Yes/no confirmation
oakterm ctl confirm "Merge feat/auth to main?"
# Exit code 0 = yes, 1 = no

# Free text input
RESPONSE=$(oakterm ctl prompt "Enter the API endpoint:" --input)
```

### Environment

```bash
# Read terminal/pane info
oakterm ctl env pane-id                           # current pane ID
oakterm ctl env workspace                         # current workspace name
oakterm ctl env panes                             # JSON list of all panes
oakterm ctl env version                           # terminal version
```

## Permission Model

Not every agent should be able to do everything. Permissions are **per-pane**, set when the pane is created.

```lua
-- When launching an agent
agent_permissions = {
  self = true,          -- can set own title, status, color, badge (always allowed)
  notify = true,        -- can send notifications (default: true)
  pane_create = true,   -- can open new panes (default: false)
  pane_read = false,    -- can read other panes' output (default: false)
  pane_input = false,   -- can send-text/send-keys/send-input to other panes (default: false)
  pane_close = false,   -- can close other panes (default: false)
  sidebar = false,      -- can modify sidebar (default: false)
  prompt = true,        -- can ask user for input (default: true)
}
```

```lua
-- In config.lua
agent_permissions = {
  self = true,          -- can set own title, status, color, badge (always allowed)
  notify = true,        -- can send notifications (default: true)
  prompt = true,        -- can prompt user for input (default: true)
  pane_create = false,  -- can open new panes (default: false)
  pane_read = false,    -- can read other panes' output (default: false)
  pane_input = false,   -- can send-text/send-keys/send-input to other panes (default: false)
  pane_close = false,   -- can close panes (default: false)
  sidebar = false,      -- can modify sidebar (default: false)
}
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

### Risk Scoring

Rather than treating all escalations equally, actions can be scored across risk dimensions to determine whether they need explicit approval or can auto-approve within a permission class.

| Dimension       | Low (0-3)                    | High (7-10)                      |
| --------------- | ---------------------------- | -------------------------------- |
| Destructiveness | Read-only, status updates    | Delete files, kill processes     |
| Scope           | Current pane only            | All panes, system-wide           |
| Reversibility   | Can undo (close a new pane)  | Cannot undo (sent input, rm -rf) |
| Privilege       | Own pane metadata            | Other pane I/O, sidebar          |
| Externality     | No side effects outside term | Network calls, filesystem writes |
| Concurrency     | No contention                | Races with user or other agents  |

A composite score (max 60 across 6 dimensions) determines the governance action. Thresholds are skewed toward caution; an action only needs to average ~5/10 per dimension to require explicit approval:

- **Low risk (0-15)**: auto-approve if the permission class is granted
- **Medium risk (16-30)**: approve with constraints (e.g., log the action, sandbox)
- **High risk (31+)**: require explicit user approval regardless of permission config

An agent with `pane_create = true` can open a floating pane without a prompt, but opening 10 panes in rapid succession (high concurrency score) still triggers approval. The scoring is heuristic and conservative; when in doubt, ask.

## Environment Variables

Every pane gets these environment variables automatically:

```bash
OAKTERM_SOCKET=/tmp/oakterm-<uid>/socket    # daemon socket path
OAKTERM_PANE_ID=pane-a1b2c3d4               # this pane's unique ID
OAKTERM_WORKSPACE=work                       # current workspace name
OAKTERM_VERSION=0.7.0                        # terminal version
```

Agents (and scripts) use these to talk to the daemon. If `OAKTERM_SOCKET` is unset, `oakterm ctl` knows it's not running inside the terminal and exits with a helpful error.

## Use Cases

### Agent sets its own status as it works

```bash
oakterm ctl self set-status working
oakterm ctl self set-title "Analyzing codebase"
# ... does work ...
oakterm ctl self set-progress 50
oakterm ctl self set-title "Writing tests"
# ... does more work ...
oakterm ctl self set-status done --outcome success
oakterm ctl self set-badge "4 files, 12 tests"
oakterm ctl notify "feat/auth complete" --level success
```

The sidebar and tab automatically reflect these updates in real-time.

### Agent opens a test runner to verify its work

```bash
oakterm ctl pane create --drawer bottom --command "npm test"
oakterm ctl wait status $TEST_PANE --status done --timeout 60000
TEST_OUTPUT=$(oakterm ctl pane read $TEST_PANE --lines 5)
# reads results, continues working
```

### Agent asks user for a decision

```bash
APPROACH=$(oakterm ctl prompt "Rate limiting approach?" --choices "sliding-window,token-bucket,leaky-bucket")
# Agent uses the answer to guide its implementation
```

### Script that sets up a dev environment

```bash
#!/bin/bash
# dev-setup.sh — run inside the terminal
oakterm ctl pane create --command "npm run dev" --title "Dev Server"
oakterm ctl pane create --drawer bottom --command "vitest --watch" --title "Tests"
oakterm ctl pane create --floating --command "docker compose up" --title "Docker"
oakterm ctl notify "Dev environment ready"
```

## Prior Art

**[Wave](https://docs.waveterm.dev/wsh-reference)** — its `wsh` CLI is the same design shipped: a single verb space letting shell scripts and agents drive GUI state over the daemon socket with no protocol knowledge (`setmeta`, `notify`, `badge`, `run`, `termscrollback`). Its [Claude Code integration](https://docs.waveterm.dev/claude-code) is lifecycle hooks calling `wsh badge` — a plain CLI covering agent integration without MCP. Two verbs it has that this surface lacks, both candidates:

- `getvar`/`setvar` — persistent key-value variables scoped to block, tab, workspace, or client-wide, giving scripts cross-session state without a dotfile
- `secret` — OS-keychain-backed credential storage (get/set/list/delete), so scripts never keep tokens in plaintext

## What This Is Not

- Not an MCP server — it's a CLI. No protocol beyond "run a command, get output."
- Not a REST API — no HTTP, no JSON-RPC. Just Unix socket + CLI.
- Not unrestricted — every dangerous action requires explicit permission.
- Not required — agents work fine without it. It's an enhancement, not a dependency.

## Related Docs

- [ADR-0021: Agent Control API](../adrs/0021-agent-control-api.md) — the decision: CLI over MCP, and the permission/security model
- [ADR-0023: Agent State Vocabulary](../adrs/0023-agent-state-vocabulary.md) — the lifecycle states `set-status` takes and the outcome field `done` carries
- [ADR-0024: Agent State Sources](../adrs/0024-agent-state-sources.md) — precedence rules self-report participates in; scopes `pane release` to hook-claimed panes
- [Agent Management](07-agent-management.md) — the plugin that manages agent lifecycle
- [Sidebar](04-sidebar.md) — where agent status appears
- [Security](21-security.md) — permission model principles
- [Remote Access](29-remote-access.md) — the daemon socket this CLI connects to
- [Architecture](01-architecture.md) — server/client daemon model
- [Agent Protocol](39-agent-protocol.md) — terminal → agent direction; this doc covers the inverse
