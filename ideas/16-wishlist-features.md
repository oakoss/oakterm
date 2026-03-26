# Wishlist Features

Features from community research (round 2) that we should consider. Organized by confidence level.

## Should Have (strong community demand, fits our design)

### Hints Mode (Vimium for the terminal)
Press a hotkey, all visible URLs/paths/git hashes/IPs get labeled with short key sequences. Type the label to act on it (open, copy, insert at cursor). Kitty and WezTerm have variants. Ghostty #2394 is highly upvoted.

Custom regex patterns per user — match Jira tickets, PR numbers, branch names. Actions per match type: open in browser, copy, insert into prompt.

**Fits:** Plugin (uses pane.output to scan, palette for action selection, keybinds)

### Quake/Dropdown Mode
Global hotkey slides the terminal down from the top of the screen. iTerm2, Warp, Yakuake, Guake all have this. Ghostty #3733 is highly requested.

**Fits:** Core feature (window management)

### Scroll-to-Prompt
Navigate between command prompts in scrollback with a keybinding. Jump to the previous/next `$` prompt instead of scrolling through output.

**Fits:** Shell integration (needs prompt markers via OSC escape sequences)

### Input Broadcast to Multiple Panes
Synchronized typing across selected panes — same command on multiple servers at once. iTerm2, tmux, and Terminator support this.

**Fits:** Multiplexer feature

### Per-Tab/Pane Color Themes
Different color schemes per tab to visually distinguish production vs staging vs local. Windows Terminal #3687.

**Fits:** Multiplexer + config

### Clickable File Paths
`filename.ts:42` opens in your editor at line 42. Requires shell integration to resolve relative paths against the pane's cwd.

**Fits:** Shell integration plugin

### Regex Pattern Highlighting
Persistent rules that color-code output: `error` in red, `warning` in yellow, timestamps dimmed. iTerm2 calls these "triggers."

**Fits:** Plugin (context engine or dedicated plugin)

### Drag and Drop
Drop files from Finder onto the terminal to insert the quoted path at cursor. Also: drag a file path FROM the terminal to GUI apps.

**Fits:** Platform shell layer

### Auto Dark/Light Mode
Switch between themes based on system appearance. Ghostty, WezTerm, and Kitty support variants.

**Fits:** Config/theme system

### Process Completion Notifications
Native OS notification when a long-running command finishes in a background tab/pane. Ghostty 1.3 added this.

**Fits:** Shell integration + notification system

## Could Have (good ideas, lower priority)

### Semantic Zone Selection
Triple-click to select the entire output of a single command, not just a line. WezTerm implements this.

### Collapsible Command Output
Fold/collapse individual command outputs in scrollback. Requires shell integration to mark command boundaries.

### Session Recording
Built-in asciinema-style recording without external tools.

### Minimap/Scrollbar Overview
A minimap showing the full scrollback with markers for commands, errors, and search hits. TermySequence implements this.

### Custom Fragment Shaders
User-written GLSL/Metal shaders for terminal backgrounds (CRT effects, animated gradients). Ghostty and Windows Terminal support this.

### Multi-Clipboard Ring
A popup showing recent clipboard entries to choose from when pasting.

### Per-Directory Profile Switching
Terminal changes profile (colors, font, env) based on which directory you cd into. iTerm2 supports this.

### Silence Detection
Notify when a terminal that was producing output goes quiet — useful for builds/deployments.

### Link Preview on Hover
Tooltip or preview when hovering over a hyperlink. Windows Terminal demoed this.

### Read-Only Mode
Protect against accidental input — useful for production terminals. Contour implements this.

## Won't Have (doesn't fit our philosophy)

### Jupyter-style Command Cells
Breaks the Unix stream model. We address the useful parts (scroll-to-prompt, semantic zones) without fundamentally changing how terminal output works.

### Transaction/Undo for Filesystem Operations
Too much magic. This belongs in the shell or a tool, not the terminal.

### CSS-Based Theming
Lua config + themes is simpler and doesn't require a CSS parser.

### Embedded Macro Toolbar
GUI buttons don't belong in a terminal. The command palette and keybinds serve this purpose.
