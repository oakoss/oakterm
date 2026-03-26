# Remote Access

Monitor and interact with your terminal from anywhere — phone, tablet, another machine. Built as a plugin, using the tunnel/proxy of your choice.

## Why Plugin, Not Core

Remote access is a networking feature with many possible transport layers. The core shouldn't pick one — it should provide the primitives and let plugins handle the connection.

## How It Works

The terminal's server/client architecture (from Foot) already has a daemon process. The remote access plugin exposes a WebSocket API on that daemon, protected by authentication. A lightweight web client connects and renders the terminal.

```
Your Terminal (daemon)
  │
  ├── Local windows (AppKit/GTK/WinUI) ← normal usage
  │
  └── Remote Access Plugin
      ├── WebSocket API (localhost:PORT)
      │   ├── Auth: token / mTLS
      │   ├── Read: pane list, pane output, sidebar state
      │   └── Write: pane input, focus, commands
      │
      └── Tunnel (your choice)
          ├── Tailscale    ← zero-config, private network
          ├── Pangolin     ← self-hosted tunnel
          ├── Cloudflare Tunnel ← public edge proxy
          ├── ngrok        ← quick public URL
          ├── Plain SSH    ← ssh -L port forwarding
          └── Direct LAN   ← local network, no tunnel
```

## Plugin API Usage

The remote access plugin uses existing primitives:

| Primitive | Usage |
|-----------|-------|
| `pane.list()` | Enumerate all panes for the web client |
| `pane.output(id)` | Stream pane content to the client |
| `pane.input(id)` | Forward keystrokes from the client |
| `pane.focus(id)` | Switch active pane from the client |
| `pane.metadata(id)` | Show sidebar info (status, branch, memory) |
| `sidebar.list()` | Render the sidebar in the web client |
| `notify.list()` | Show pending notifications |
| `network` | Serve the WebSocket API |
| `storage` | Persist auth tokens and client sessions |

## Web Client

A lightweight, bundled web UI — not a full terminal emulator in the browser, but enough to:

- See all panes and their status (sidebar view)
- See live output from any pane (read-only by default)
- Send input to a pane (interactive mode, opt-in)
- See notifications (agent needs approval, build failed)
- Approve/deny agent actions with one tap
- Run palette commands (`:merge`, `:diff`, etc.)

The web client is a static HTML/JS bundle served by the plugin. No external dependencies. Works on any mobile browser.

## Access Modes

| Mode | What you can do |
|------|----------------|
| **Monitor** (default) | See all panes, output, sidebar, notifications. Read-only. |
| **Interactive** | Monitor + send input to panes. Requires explicit enable per-session. |
| **Full** | Interactive + run palette commands, create/close panes. Requires separate auth. |

Users choose the mode when connecting. Monitor is safe to leave on — it can't change anything.

## Authentication

```lua
plugins = {
  ["remote-access"] = {
    enabled = true,
    port = 7890,
    auth = "token",                    -- or "mtls"
    token = "${PHANTOM_REMOTE_TOKEN}", -- env var, never in config file
    allowed_modes = { "monitor", "interactive" },
    -- listen = "127.0.0.1",          -- localhost only by default
    -- listen = "0.0.0.0",            -- all interfaces (for tunnel use)
  },
}
```

- **Token auth** — simple shared secret. Generate with `phantom remote token`. Pass via URL parameter or header.
- **mTLS** — mutual TLS with client certificates. For high-security setups.
- Tokens stored in keychain/credential manager, not in config files.
- Rate limiting on auth failures.

## Tunnel Setup

The plugin doesn't manage tunnels — you use whatever tunnel you already have. Examples:

**Tailscale (recommended for personal use):**
```bash
# Terminal is already on your Tailnet
# Access from phone: http://your-machine:7890
# Zero config, private by default
```

**Cloudflare Tunnel:**
```bash
cloudflared tunnel --url http://localhost:7890
# Get a public URL like https://phantom-xyz.trycloudflare.com
```

**Pangolin (self-hosted):**
```bash
# Configure in your Pangolin dashboard
# Route phantom.yourdomain.com → localhost:7890
```

**SSH port forwarding:**
```bash
ssh -L 7890:localhost:7890 your-server
# Access at http://localhost:7890 on any machine
```

**Local network:**
```lua
-- config: listen on all interfaces
plugins = {
  ["remote-access"] = {
    listen = "0.0.0.0",
    -- access via http://192.168.1.x:7890 from any device on LAN
  },
}
```

## Mobile Use Case: Agent Babysitting

The primary use case: you have 3 agents running on your desktop, you step away.

On your phone:
1. Open browser → your-machine:7890 (via Tailscale)
2. See sidebar: 2 agents working, 1 needs approval
3. Tap the agent that needs approval → see its output
4. Tap "Approve" → agent continues
5. Get push notification when it finishes (if browser notifications are enabled)

You didn't need to ssh in, install tmux, or set up anything. The terminal's plugin served a web page, your tunnel made it reachable.

## What This Is Not

- Not a web-based terminal emulator (like ttyd or Wetty) — it's a remote control for your existing terminal
- Not a screen sharing tool — it renders a lightweight UI, not a pixel-perfect copy of your screen
- Not always-on — the plugin only listens when enabled
- Not a cloud service — everything runs on your machine, you pick the tunnel
