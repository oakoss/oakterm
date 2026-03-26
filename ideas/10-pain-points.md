# Community Pain Points Addressed

Real complaints from GitHub issues, Hacker News, and developer forums that directly shaped this design.

## Clipboard over SSH + multiplexer is universally broken
**Source:** Every terminal's issue tracker, dozens of tmux complaints
**Solution:** We own both the terminal and multiplexer. OSC-52 passthrough works everywhere — no configuration, no tmux hacks. Clipboard passes through splits, SSH domains, everything.

## AI agents break terminal scrollback
**Source:** Claude Code issues #10769, #36816, #5939; Ghostty discussion #10456
**Solution:** Agent-aware panes. The terminal knows it's an agent process and manages scrollback differently — scroll pinning prevents the agent's output from hijacking your position.

## Font rendering is inconsistent across platforms
**Source:** Alacritty #7118, Kitty #4941, Warp #4692, WezTerm macOS complaints
**Solution:** Platform-native text shaping. Core Text on macOS, HarfBuzz on Linux. Don't fight the OS.

## Unicode/emoji width breaks cursor alignment
**Source:** Windows Terminal #16852, widespread wcwidth() complaints
**Solution:** Unicode 16.0 grapheme cluster width tables, not legacy wcwidth().

## Terminal color detection has no standard
**Source:** Julia Evans' blog post documenting 11 specific color problems
**Solution:** Set COLORTERM=truecolor, respond correctly to DA queries, forward through SSH domains.

## SSH terminfo isn't available on remote hosts
**Source:** Kitty discussion #3873, Kitty issue #713
**Solution:** Use xterm-256color as TERM (universal compatibility), advertise extra capabilities via standard escape sequence queries. No custom terminfo to install.

## tmux sessions don't persist
**Source:** Universal tmux complaint, tmux-resurrect exists as a workaround
**Solution:** Native session persistence. Serialize everything on quit, restore on relaunch.

## tmux keybinds are arcane and undiscoverable
**Source:** Mauricio Poppe's tmux-to-zellij post, countless blog posts
**Solution:** Discoverable status bar (Zellij-style), command palette for everything, `?` overlay for shortcuts.

## Image protocol fragmentation
**Source:** arewesixelyet.com, Kitty vs Sixel debates
**Solution:** Support both Kitty graphics and Sixel. Both work through the built-in multiplexer.

## Config complexity
**Source:** WezTerm discussion #2999 (Lua too complex for basics, too simple for advanced)
**Solution:** Flat key-value file for simple config, Lua for programmable config. Two tiers.

## Warp privacy concerns
**Source:** Warp issues #1346 (telemetry), #900 (login requirement)
**Solution:** Zero telemetry. No login. No account. No phoning home. Ever. AI is BYOK.

## WezTerm stalled releases
**Source:** WezTerm issue #7451
**Lesson:** Maintain a regular release cadence. Don't let the project appear abandoned.

## Alacritty missing features since 2017
**Source:** Alacritty issue #50 (ligatures, open since 2017)
**Lesson:** Ship ligatures from day one. Don't let philosophical minimalism block basic features.

## Zellij memory usage
**Source:** Zellij issue #3594 (80MB empty vs tmux 6MB)
**Lesson:** The multiplexer must be lightweight. Target under 20MB for an empty session.

## Electron terminal performance
**Source:** HN discussions about Hyper and Tabby
**Lesson:** Native rendering is non-negotiable. No Electron. No web views in the core.
