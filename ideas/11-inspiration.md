---
title: "Inspiration & Prior Art"
status: reference
category: research
description: "What we took from each terminal"
tags: ["research", "ghostty", "kitty", "wezterm", "alacritty", "warp", "tmux", "zellij", "cmux", "foot", "contour", "ptyxis", "rio", "wave", "harpoon"]
---
# Inspiration & Prior Art


What we took from each terminal and tool — and what we deliberately left out.

## Terminals

### Ghostty
**Took:** Platform-native chrome (AppKit/GTK), Zig-level performance as the latency target, simple flat config as baseline
**Left:** Zig as implementation language (Rust ecosystem is better for our plugin/networking needs)

### Alacritty
**Took:** Latency target (<8ms input), minimal-by-default philosophy, "boots fast with zero config"
**Left:** Philosophical refusal to add features (no ligatures since 2017, no tabs, no splits)

### Kitty
**Took:** Graphics protocol (de facto standard for inline images), plugin concept (kittens), remote control protocol
**Left:** Python-only plugin system, custom xterm-kitty TERM type that breaks SSH

### WezTerm
**Took:** Lua config (programmable, proven), built-in multiplexer concept, SSH domain multiplexing, workspaces
**Left:** Lua as plugin language (WASM instead), Electron-weight memory footprint

### Warp
**Took:** Context-aware autocomplete, visual directory picker for cd, project-type detection, command suggestions, session switcher UX
**Left:** Blocks, notebooks, collaboration features, telemetry, login requirement, closed source, "Agentic IDE" direction

### cmux
**Took:** Collapsible sidebar for process oversight, notification badges, per-pane metadata (git branch, ports, status)
**Left:** Agent-only focus (our sidebar is for all processes), embedded browser, macOS-only

### Zellij
**Took:** Floating panes, WASM plugin system (only shipping implementation), discoverable mode bar, declarative layouts
**Left:** KDL config format, high memory baseline (80MB empty)

### Foot
**Took:** Server/client architecture — one daemon, many windows, shared font/glyph cache
**Left:** Wayland-only scope

### Contour
**Took:** Vi-like modal input for copy mode, pixel-smooth scrolling
**Left:** C++ implementation

### Ptyxis
**Took:** Container-first discovery — auto-detect Docker/Podman, spawn shells inside containers
**Left:** GNOME-only scope

### Rio
**Took:** Validates wgpu/WebGPU as a viable rendering approach for terminals
**Left:** CRT shader novelty features

### Wave
**Took:** Proves users want inline file previews and that a terminal can embed non-terminal content in panes
**Left:** Drag-and-drop workspace model, built-in editor, too many concerns in one app

### iTerm2
**Took:** Smart selection (quad-click selects semantic objects), triggers (regex → action), process completion notifications, automatic profile switching per directory
**Left:** No GPU rendering (high CPU/memory), bloated feature set, macOS-only

### Windows Terminal
**Took:** UIA accessibility tree implementation (reference for how to do a11y right), fragment-based extension, per-tab color profiles
**Left:** Windows-only, JSON config, no plugin system

### Tabby
**Took:** Quake console mode (global hotkey dropdown), SSH connection manager concept, serial terminal support
**Left:** Electron (performance), plugin sprawl

## Apps (not terminals, but UX inspiration)

### T3 Chat
**Took:** Local-first instant navigation (state on disk, never wait for network), conversation/workspace forking
**Left:** Chat app UX patterns (model switching, personas, folders)

### T3 Code
**Took:** Thread-per-task model (maps to our workspace concept), context window meter, one-click git workflow chaining
**Left:** Three-panel layout, diff viewer, approval panels (agent supervision UX)

### Conductor
**Took:** Git worktree isolation as a first-class concept, setup scripts per workspace, diff-first review
**Left:** Linear/GitHub integrations, agent dashboard UX, macOS-only

## tmux / Zellij (multiplexers)

### tmux
**Took:** Session/window/pane hierarchy, detach/reattach, scriptability, copy mode with vi bindings
**Left:** Statelessness (no persistence), arcane keybinds, clipboard hell

### Zellij
**Took:** Floating panes, layout system, mode indicators, WASM plugins, session resurrection
**Left:** Memory overhead, mode conflicts with Neovim, KDL config

## Neovim Ecosystem

### Harpoon (ThePrimeagen)
**Took:** Fixed-size bookmark register for instant navigation by index. Muscle memory over fuzzy search. Per-project persistence.
**Left:** File-centric model (we adapted it for panes)

### Neovim `:checkhealth`
**Took:** Single command that runs all diagnostics with actionable fix suggestions. Our `:health` is directly inspired by this.
**Left:** Neovim-specific health checks
