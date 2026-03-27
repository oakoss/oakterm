---
adr: '0007'
title: Daemon Architecture
status: accepted
date: 2026-03-26
tags: [core, renderer]
---

# 0007. Daemon Architecture

## Context

OakTerm uses a server/client architecture described in [01-architecture.md](../ideas/01-architecture.md). The daemon owns PTYs, the VT parser, screen buffers, plugins, and config. The client handles GPU rendering, window management, and input. Several details were unspecified:

- Does the daemon exit when the last window closes, or persist for session recovery?
- Are the daemon and client separate processes or threads in the same process?
- What's the IPC mechanism between them?
- Can third-party clients connect to the daemon?

The review audit flagged the IPC/daemon wire protocol as a missing specification blocking Phase 0.

Research into terminal architectures found:

- **Single-process terminals** (Ghostty, Kitty, Alacritty): GPU crashes kill all windows and running processes. No recovery.
- **WezTerm's mux server**: Separates terminal state from GUI as distinct processes. If the GUI crashes, reconnect to the mux server. Proven model but requires manual configuration.
- **Foot's server mode**: Daemon handles all rendering and VT parsing. Shared fonts reduce memory. But all I/O is single-threaded and a server crash kills everything.
- **Windows Terminal**: Moved from multi-process to single-process because coordination bugs outweighed isolation benefits. However, their model was about multi-window coordination, not state/rendering separation.

GPU crashes are real (sleep/resume, driver updates, monitor hot-plug, NVIDIA driver instability) and kill all windows in single-process terminals. Terminal state lives in CPU memory, making the daemon/client boundary clean.

## Options

### Option A: Single process, multi-threaded (Ghostty/Kitty model)

Everything in one process. Threads for I/O, rendering, and plugin host.

**Pros:**

- Simplest architecture. Zero IPC overhead.
- Shared memory by default (same address space).

**Cons:**

- GPU crash kills all windows and running processes.
- No path to session persistence without external tools.
- No path to third-party clients or remote access without bolting on IPC later.

### Option B: Daemon + GUI as separate processes, Unix socket IPC

Daemon owns all terminal state. GUI process handles rendering and input. Connected by Unix domain socket with a binary protocol.

**Pros:**

- GUI crash does not kill running processes or terminal state.
- Session persistence is natural — daemon already survives window close.
- Third-party clients (web, IDE, alternative GUIs) can connect via the same protocol.
- Headless/remote mode is the same daemon without a local GUI client.
- Screen buffer is small (~160KB for 200x50 grid). Unix socket throughput (1-4 GB/s) handles 60fps full-screen updates trivially.

**Cons:**

- More complex than single-process.
- IPC adds latency (Unix socket round-trip: ~0.2μs, negligible).
- Requires a wire protocol specification.

### Option C: Daemon + GUI with shared memory

Same as B but screen buffer shared via mmap instead of socket.

**Pros:**

- Zero-copy screen buffer access (27-220x faster than socket IPC in benchmarks).

**Cons:**

- All the complexity of Option B plus shared memory synchronization.
- macOS has a 4MB default shared memory limit and no `/dev/shm`.
- No terminal has shipped cross-process shared memory successfully.
- Socket IPC is already fast enough for terminal data volumes — shared memory solves a problem that doesn't exist.

## Decision

**Option B — daemon + GUI as separate processes, Unix socket with binary protocol.**

The daemon is the terminal. The GUI is a viewport. This separation enables crash isolation, session persistence, third-party clients, and headless/remote mode from the same architecture.

### Process Model

| Component         | Daemon process     | GUI process                             |
| ----------------- | ------------------ | --------------------------------------- |
| PTY processes     | Owns               | —                                       |
| VT parser         | Owns               | —                                       |
| Screen buffer     | Owns (CPU memory)  | Receives updates                        |
| Scroll buffer     | Owns (ring + disk) | Requests on demand                      |
| Plugins (WASM)    | Owns               | —                                       |
| Lua config        | Owns               | Receives on change                      |
| GPU rendering     | —                  | Owns                                    |
| AccessKit tree    | —                  | Owns (built from screen buffer updates) |
| Window management | —                  | Owns                                    |
| Input handling    | —                  | Forwards to daemon                      |

### Daemon Lifecycle

- **Default:** Daemon exits when the last window closes. Session state is saved to disk for layout restoration on next launch.
- **Opt-in persistence:** `daemon-persist` (Lua: `config.daemon_persist = true`). Daemon survives window close. Opening a new window reconnects to existing sessions. Explicit quit (`oakterm quit` or platform app quit) terminates the daemon.
- Layout restoration (save/restore tabs, splits, working directories on exit/launch) always works regardless of persistence mode.

### Protocol Design

**Two protocol layers over the same Unix socket:**

1. **GUI protocol** — full protocol for rendering clients. Screen buffer updates (dirty regions as binary frames), cursor state, selection state, pane/workspace topology, config changes, scroll buffer requests/responses.
2. **Control protocol** — subset for `oakterm ctl` and automation. Commands and responses only, no screen data. Used by agent processes, scripts, and IDE integrations.

Distinguished by a flag in the connection handshake.

**Protocol versioning:**

- Version number in the initial handshake.
- Major version mismatch: connection rejected with an upgrade message.
- Minor version mismatch: tolerated (additive changes only, new message types ignored by older peers).

**Authentication:**

- Local connections: Unix socket file permissions (`0700`). The socket is at `$OAKTERM_SOCKET`. Default path: `$XDG_RUNTIME_DIR/oakterm/socket` on Linux, `$TMPDIR/oakterm-<uid>/socket` on macOS. The parent directory must be created with `0700` and ownership verified before the socket is bound.
- Remote connections: Token + mTLS over WebSocket (defined in [29-remote-access.md](../ideas/29-remote-access.md), deferred to Phase 4).

### Client Types

The daemon protocol supports multiple client types:

| Client                  | Transport                      | Use case                         |
| ----------------------- | ------------------------------ | -------------------------------- |
| Bundled GUI (`oakterm`) | Unix socket                    | Default local experience         |
| `oakterm ctl`           | Unix socket (control protocol) | Agent/script automation          |
| Web client              | WebSocket/TLS                  | Remote access (Phase 4)          |
| Third-party GUI         | Unix socket                    | Alternative frontends            |
| IDE integration         | Unix socket                    | Embedded terminal panes          |
| Headless                | No client                      | `oakterm --headless` server mode |

### Multi-Client Support

- Each GUI client gets its own set of panes by default.
- The protocol supports multiple clients viewing the same pane (shared session, like tmux attach). This is a Phase 1+ feature but the protocol does not prevent it.

### Crash Recovery

| Scenario                             | Recovery                                                                                                          |
| ------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| GUI process crash (GPU driver, etc.) | Daemon survives. New GUI process reconnects. Running processes and scroll history are intact.                     |
| Daemon crash                         | GUI detects disconnect. Daemon restarts and restores from disk (session persistence). GUI reconnects.             |
| GPU device loss (soft crash)         | GUI rebuilds GPU resources in-process from CPU-side state (wgpu device-loss callback). No process restart needed. |

### Daemon Upgrade

When the user installs a new version while a persistent daemon is running, the new GUI detects a version mismatch via the protocol handshake. Graceful upgrade mechanism deferred to a later ADR, but options include: daemon self-restart with state serialization, or side-by-side version coexistence. The protocol's version handshake ensures incompatible clients and daemons never silently misbehave.

## Consequences

- Phase 0 includes the daemon process, Unix socket listener, binary protocol with version handshake, and a single GUI client.
- The wire protocol specification becomes a separate spec doc (Spec-0001 candidate).
- `oakterm ctl` uses the control protocol layer, not a separate mechanism.
- Third-party clients are possible from day one — the protocol is the public API surface.
- Headless mode (`oakterm --headless`) is the daemon without a GUI client, not a separate binary.
- Update [01-architecture.md](../ideas/01-architecture.md) to formalize the daemon/GUI process split and protocol layers.
- Update [29-remote-access.md](../ideas/29-remote-access.md) to clarify that remote access uses the same daemon protocol over WebSocket instead of Unix socket.
- Update [32-agent-control-api.md](../ideas/32-agent-control-api.md) to clarify that `oakterm ctl` uses the control protocol layer.

## References

- [01-architecture.md](../ideas/01-architecture.md)
- [29-remote-access.md](../ideas/29-remote-access.md)
- [32-agent-control-api.md](../ideas/32-agent-control-api.md)
- [36-terminal-fundamentals.md](../ideas/36-terminal-fundamentals.md)
- [WezTerm multiplexing architecture](https://wezterm.org/multiplexing.html)
- [Foot server/client model](https://codeberg.org/dnkl/foot)
- [wgpu device loss detection (PR #6229)](https://github.com/gfx-rs/wgpu/pull/6229)
