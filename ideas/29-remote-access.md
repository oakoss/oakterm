---
title: "Remote Access & Headless Mode"
status: draft
category: cross-cutting
description: "Headless daemon, native client connection, web client, tunnel-agnostic"
tags: ["remote", "headless", "websocket", "mobile", "tailscale", "tunnel", "server", "daemon"]
---
# Remote Access & Headless Mode

Run the terminal daemon on a server. Connect to it from your desktop terminal like it's local.

## The Model

```
┌─────────────────────────────────┐     ┌─────────────────────────────┐
│  Your Mac (client)              │     │  Proxmox Server (daemon)    │
│                                 │     │                             │
│  Phantom Terminal               │     │  phantom --headless         │
│  ├── Local panes (shells, etc.) │     │  ├── 3 agents running       │
│  │                              │     │  ├── docker compose         │
│  └── Remote tab: homelab ───────┼────→│  ├── test watcher           │
│      Looks and feels local      │     │  └── dev server :3000       │
│      Sidebar shows remote panes │     │                             │
│      Harpoon works across both  │     │  Listening on :7890         │
└─────────────────────────────────┘     └─────────────────────────────┘
```

The remote panes appear in your sidebar alongside local panes. You can split a remote pane next to a local one. Harpoon bookmarks can mix local and remote. It's all panes.

## Two Modes

### 1. Headless Daemon (server-side)

```bash
phantom --headless
```

Runs the full daemon without a window — no GPU, no display server, no GTK/AppKit/WinUI. Just the multiplexer, plugin host, scroll buffers, and network API.

Works on:
- Ubuntu Server (no desktop environment)
- Any headless Linux (Alpine, Debian, RHEL)
- Docker containers
- VMs, cloud instances, Proxmox LXCs
- CI/CD runners

The abstraction layer makes this possible:
- `trait GpuBackend` → `NullBackend` (no rendering)
- `trait PlatformShell` → `HeadlessShell` (no windows)
- `trait TextShaper` → `NullShaper` (no font rendering — clients handle it)
- `trait AccessibilityBridge` → `NullBridge` (no screen reader on a server)

Everything else — multiplexer, plugins, config, VT parser, scroll buffer — runs identically.

```bash
# On your server
phantom --headless --port 7890 --auth-token "$PHANTOM_TOKEN"

# Or daemonize it
phantom --headless --port 7890 --auth-token "$PHANTOM_TOKEN" --daemon
# Writes PID to ~/.local/state/phantom/daemon.pid
```

### 2. Client Connection (desktop-side)

From your Mac/Linux/Windows terminal, connect to the remote daemon:

```
:connect homelab
```

Or in config:

```
remote-domain.homelab.host = proxmox.local
remote-domain.homelab.port = 7890
remote-domain.homelab.auth = token
remote-domain.homelab.token = ${PHANTOM_HOMELAB_TOKEN}
```

```lua
remote_domains = {
  {
    name = "homelab",
    host = "proxmox.local",
    port = 7890,
    auth = "token",
    -- token read from env var, never in config
  },
  {
    name = "prod",
    host = "prod.example.com",
    port = 7890,
    auth = "mtls",
    cert = "~/.config/phantom/certs/prod-client.pem",
  },
}
```

### What connecting looks like

```
:connect homelab

┌──────────────────┬─────────────────────────────┐
│ LOCAL            │                             │
│ ● scratch        │  ~/project $ _              │
│──────────────────│                             │
│ HOMELAB 🔗       │                             │
│ ◉ feat/auth  ❓  │                             │
│ ◉ add-tests  ⟳  │                             │
│ ▶ docker compose │                             │
│ 👁 vitest  14/14  │                             │
│ ● server-shell   │                             │
└──────────────────┴─────────────────────────────┘
```

Remote panes show under their domain name with a connection indicator. Click one, it fills the main view. Split it next to a local pane. Everything works — harpoon, notifications, memory display, `:debug`.

### What the protocol handles

| Capability | How |
|-----------|-----|
| Pane output streaming | VT byte stream over WebSocket — the client renders it locally with its own GPU |
| Pane input | Keystrokes sent over WebSocket to the daemon |
| Sidebar state | Structured data (JSON) — sections, entries, badges |
| Notifications | Push events from daemon to client |
| Plugin state | Remote plugins run on the daemon, their sidebar/palette entries sync to client |
| Scroll buffer | Client requests scroll regions on demand, daemon sends from its buffer |
| File operations | Plugins on the daemon access the server filesystem, not the client's |

The client does its own rendering — the server doesn't need a GPU. The protocol sends VT output (same bytes a PTY would produce) and the client's local renderer handles fonts, ligatures, images, everything.

## Difference from SSH Domains

SSH domains (in the multiplexer) open an SSH connection and run a remote shell. The remote machine runs your shell, not the daemon. There's no plugin host, no sidebar, no agent management on the remote side.

Remote domains connect to a full Phantom daemon. The remote side has its own plugins, sidebar state, agent management, scroll buffers. The client synchronizes with that state.

| Feature | SSH Domain | Remote Domain |
|---------|-----------|---------------|
| Remote side runs | Your shell (bash/zsh) | Full Phantom daemon |
| Plugins | Local only | Both local and remote |
| Sidebar | Local state only | Merged local + remote |
| Agent management | Not available remotely | Full remote agent lifecycle |
| Session persistence | Reconnects SSH | Daemon keeps running, client reconnects |
| Requires on remote | SSH server | Phantom binary |

You'll use both. SSH domains for quick shell access to machines where you don't have Phantom installed. Remote domains for your homelab, dev servers, and CI machines where you want the full experience.

## Web Client (for mobile/lightweight access)

The daemon also serves a web client for when you don't have the desktop terminal:

```
https://proxmox.local:7890  (via Tailscale, Cloudflare Tunnel, etc.)
```

The web client is lighter than the native client — monitor mode by default, interactive on opt-in. Good for checking on agents from your phone.

| Client | Rendering | Full features | Offline |
|--------|-----------|--------------|---------|
| Desktop terminal | Local GPU | Yes — full sidebar, harpoon, splits | Yes (local panes) |
| Web client | Browser | Monitor + basic interaction | No |

## Tunneling

The daemon listens on localhost by default. You bring your own tunnel:

| Tunnel | Best for |
|--------|----------|
| **Tailscale** | Personal use — zero config, private network, already on your devices |
| **Pangolin** | Self-hosted — your own tunnel infrastructure |
| **Cloudflare Tunnel** | Public edge — fast, no port forwarding |
| **SSH port forward** | Simple — `ssh -L 7890:localhost:7890 server` |
| **WireGuard** | Direct VPN — low latency |
| **Direct LAN** | Home network — `--listen 0.0.0.0` |

## Authentication

| Method | Best for |
|--------|----------|
| **Token** | Personal use — generate with `phantom remote token`, pass via env var |
| **mTLS** | Team/production — mutual TLS with client certificates |

Tokens are never stored in config files — always via environment variable or credential manager.

## Implementation

### What's core

- Headless mode (`--headless`) — NullBackend implementations for all platform traits
- Daemon mode (`--daemon`) — background process management, PID file
- Remote domain configuration (`remote_domains` in config)
- Client-side remote pane rendering (VT stream from WebSocket, rendered locally)
- Protocol definition (WebSocket + message format for pane I/O, sidebar sync, notifications)

### What's a plugin

- Web client serving (the HTML/JS bundle that runs in a browser)
- Advanced tunnel management (auto-start Tailscale, configure Cloudflare)
- Multi-daemon dashboard (managing connections to many servers)

## Security

- All connections encrypted (TLS)
- Auth required on every connection
- Rate limiting on auth failures
- Daemon logs all connections with timestamps and client info
- `:debug security` shows active remote connections
- Configurable: `remote-allow-interactive = false` for monitor-only access

## Related Docs

- [Architecture](01-architecture.md) — server/client daemon model
- [Abstraction Layer](13-abstraction.md) — Null implementations for headless traits
- [Multiplexer](03-multiplexer.md) — SSH domains (different from remote domains)
- [Security](21-security.md) — auth and encryption
- [Platform Support](20-platform-support.md) — headless Linux support
