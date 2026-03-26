---
title: "Platform Support"
status: draft
category: cross-cutting
description: "macOS, Linux, Windows — all first-class"
tags: ["macos", "linux", "windows", "wayland", "wsl", "cross-platform"]
---
# Platform Support


All three platforms are first-class. Not "macOS first, others eventually" — all three ship together.

## Platform Matrix

| Component | macOS | Linux | Windows |
|-----------|-------|-------|---------|
| Window chrome | AppKit | GTK4 | WinUI 3 |
| GPU rendering | Metal (via wgpu) | Vulkan (via wgpu) | DX12 (via wgpu) |
| Text shaping | Core Text | HarfBuzz | DirectWrite |
| Font rasterization | Core Text | FreeType | DirectWrite |
| Accessibility | NSAccessibility (VoiceOver) | AT-SPI (Orca) | UIA (NVDA, JAWS) |
| Clipboard | NSPasteboard | Wayland/X11 clipboard | Win32 clipboard |
| Notifications | NSUserNotification | libnotify / D-Bus | Windows Toast |
| Global hotkeys | CGEvent tap | D-Bus / X11 grab | RegisterHotKey |
| Window blur | NSVisualEffectView | Compositor-dependent | Mica / Acrylic |
| System theme detection | NSAppearance | xdg-desktop-portal | Windows.UI.Settings |
| File drag-and-drop | NSPasteboard | GDK/Wayland DnD | OLE DnD |

wgpu handles the GPU abstraction across all three — Metal, Vulkan, and DX12 from a single codebase. The `trait GpuBackend` seam means platform-specific rendering quirks don't leak into the rest of the code.

## macOS

### Native Integration
- AppKit window management — title bar, traffic lights, native tabs (optional)
- Cmd keybindings (Cmd+C, Cmd+V, Cmd+N, Cmd+T) feel native
- Secure keyboard entry support (prevents other apps from reading keystrokes)
- Touch Bar support (if present) — show pane switcher, current branch
- Trackpad smooth scrolling with momentum
- Handoff / Universal Clipboard (paste from iPhone)
- System proxy settings respected for SSH

### macOS-Specific Considerations
- Notarization and signing for Gatekeeper
- Homebrew cask distribution: `brew install --cask phantom`
- `.app` bundle with proper Info.plist
- Spotlight metadata for terminal sessions (optional)
- Respects system text substitution settings

## Linux

### Display Server Support
- **Wayland** — primary target, native support via GTK4
- **X11** — supported via XWayland and native X11 fallback
- Both work, no feature gaps between them

### Wayland-Specific
- Client-side decorations (CSD) via GTK4/libadwaita
- Primary selection (middle-click paste) support
- Fractional scaling and per-monitor DPI
- Input method (IME) support for CJK input via Wayland text-input protocol
- No dead key bugs (Ghostty had issues with ibus 1.5.29 — we test against it)

### X11-Specific
- Server-side decorations option
- XIM input method support
- X11 clipboard + primary selection

### Distribution
- Flatpak (primary — sandboxed, auto-update)
- `.deb` / `.rpm` packages
- AppImage
- AUR (Arch)
- Nix package
- Build from source (single `cargo build`)

### Desktop Integration
- `.desktop` file with proper categories and keywords
- D-Bus interface for scripting
- XDG Base Directory compliance (`~/.config/phantom/`, `~/.local/state/phantom/`)
- File manager "Open Terminal Here" integration
- Respects `$SHELL`, `$EDITOR`, `$BROWSER`

## Windows

### Native Integration
- WinUI 3 for window chrome — native title bar, Mica/Acrylic material
- ConPTY for terminal I/O (the modern Windows pseudo-terminal API)
- DirectWrite for text shaping and rasterization
- Win32 keyboard handling with proper IME support
- Jump list integration (recent sessions, pinned workspaces)
- Windows Terminal-style settings (JSON) migration path

### WSL Integration
- Detect installed WSL distributions
- Launch panes directly into WSL distros
- Sidebar shows WSL distros as launchable environments
- File path translation between Windows and WSL (`\\wsl$\` ↔ `/mnt/c/`)

```lua
wsl = {
  auto_detect = true,
  default_distro = "Ubuntu",
}
```

### PowerShell & CMD
- PowerShell Core and Windows PowerShell support
- CMD support
- Git Bash support
- Shell integration scripts for PowerShell

### Windows-Specific Considerations
- MSIX packaging for Microsoft Store distribution
- Winget: `winget install phantom`
- Scoop: `scoop install phantom`
- Chocolatey: `choco install phantom`
- Portable mode (no install, run from USB)
- Proper handling of Ctrl vs Cmd (there is no Cmd on Windows — Ctrl is the primary modifier)

### Keyboard Model

The biggest cross-platform pain point. macOS uses Cmd for system shortcuts and Ctrl for terminal control. Windows/Linux use Ctrl for both.

Our approach:

| Action | macOS | Windows/Linux |
|--------|-------|---------------|
| Copy (smart) | Cmd+C | Ctrl+C (with selection) |
| Paste | Cmd+V | Ctrl+V |
| New tab | Cmd+T | Ctrl+Shift+T |
| Close pane | Cmd+W | Ctrl+Shift+W |
| Command palette | Cmd+Shift+P | Ctrl+Shift+P |
| Interrupt (SIGINT) | Ctrl+C | Ctrl+C (no selection) |
| Sidebar toggle | Ctrl+B | Ctrl+B |
| Split pane | Ctrl+\ | Ctrl+\ |

Keybindings are platform-aware by default. Config uses logical names:

```lua
keybinds = {
  { key = "super+c", action = "copy-or-interrupt" },  -- Cmd on mac, Ctrl on win/linux
  { key = "ctrl+c", action = "sigint" },               -- always Ctrl
}
```

`super` maps to Cmd on macOS and Ctrl on Windows/Linux. `ctrl` always means the physical Ctrl key. This matches how users think on each platform.

## Cross-Platform Guarantees

- Same config file works on all platforms (platform-specific sections optional)
- Same plugins work on all platforms (WASM is portable)
- Same keybind config works everywhere (via `super` abstraction)
- Feature parity — no platform gets a feature the others don't (except platform-specific integrations like WSL or Touch Bar)
- CI tests on all three platforms for every PR
- Same release cadence for all platforms

## Headless Linux (Servers, Containers, CI)

`phantom --headless` runs the daemon without a display server, GPU, or window manager.

Works on:
- Ubuntu Server, Debian, Alpine, RHEL (no desktop environment)
- Docker containers
- Proxmox LXCs and VMs
- CI/CD runners (GitHub Actions, GitLab CI)
- Any Linux with a recent kernel

Uses Null implementations for all platform traits:
- `NullBackend` (no GPU rendering)
- `HeadlessShell` (no window management)
- `NullShaper` / `NullRasterizer` (no font rendering — clients handle it)

Everything else runs identically — multiplexer, plugins, VT parser, scroll buffers, agent management. Connect from your desktop terminal or web client.

See [Remote Access & Headless Mode](29-remote-access.md) for the full spec.

## What the Abstraction Layer Enables

From [Abstraction Layer](13-abstraction.md), these traits make cross-platform work:

```
trait PlatformShell    → AppKit / GTK4 / WinUI 3 / HeadlessShell
trait TextShaper       → Core Text / HarfBuzz / DirectWrite / NullShaper
trait FontRasterizer   → Core Text / FreeType / DirectWrite / NullRasterizer
trait ClipboardProvider → NSPasteboard / Wayland+X11 / Win32
trait GpuBackend       → Metal / Vulkan / DX12 (all via wgpu) / NullBackend
```

Everything above these traits is shared code. The platform layer is thin — window creation, input events, clipboard, notifications. The renderer, multiplexer, plugin host, and config system are 100% cross-platform.

## Related Docs

- [Abstraction Layer](13-abstraction.md) — traits for each platform
- [Renderer](02-renderer.md) — per-platform text shaping and GPU backends
- [Accessibility](17-accessibility.md) — per-platform screen reader support
- [Remote Access](29-remote-access.md) — headless Linux support
- [Smart Keybinds](19-smart-keybinds.md) — platform-aware keybind defaults
