---
name: run-oakterm
description: Build, launch, and drive the oakterm terminal to verify changes. Use when you need to run the oakterm GUI, smoke-test a renderer/daemon/multiplexer change, or send input to and read output from a running oakterm as an agent.
---

# Run oakterm

"Running" means launching the actual GUI (which spawns the daemon and a shell)
and driving it — not `cargo test`. Use `oakterm-ctl` to send input and read
output; only fall back to screenshots when you must _see_ rendering (color,
emoji, glyph layout, cursor).

Scoped to macOS: the socket path keys off `$TMPDIR` (Linux uses
`$XDG_RUNTIME_DIR/oakterm`), and the visual-verification tooling is macOS-only.

## Build

The GUI finds the daemon as a sibling binary (`<dir>/oakterm-daemon`), so build
both — plus `oakterm-ctl` to drive it:

```bash
cargo build -p oakterm -p oakterm-daemon -p oakterm-ctl
```

## Launch

Use a **short `TMPDIR`**. The daemon's Unix socket path must fit the ~104-char
`sun_path` limit; a long scratchpad `TMPDIR` overflows it and the socket fails
to bind. `/tmp/otr` is short and safe.

Launch in the background by default: `OAKTERM_BACKGROUND=1` starts oakterm
without taking focus, so the user keeps typing where they are. An app that has
never been active doesn't see modifier keys, though, so shortcuts (Cmd+F,
Cmd+[, leader chords) arrive as plain keys. For a test that needs them, launch
without the variable and tell the user first: that launch takes focus once,
and whatever they type next lands in the test shell.

```bash
mkdir -p /tmp/otr
OAKTERM_BACKGROUND=1 TMPDIR=/tmp/otr ./target/debug/oakterm >| /tmp/otr/oakterm.log 2>&1 &
echo $! >| /tmp/otr/oakterm.pid
sleep 2   # daemon starts, PTY spawns on the first client resize
```

The PID file names the instance you launched; the user may be running their
own oakterm, which `pgrep`/`pkill` by name would also reach. The daemon starts
automatically as a child of the GUI. Confirm the shell spawned:

```bash
kill -0 "$(cat /tmp/otr/oakterm.pid)" && grep -c "PTY spawned" /tmp/otr/oakterm.log
```

## Drive it with oakterm-ctl (preferred)

`oakterm-ctl` connects to the same daemon over the socket. **Run it with the
same `TMPDIR`** as the GUI so it resolves the same socket. Repeat the prefix on
every call — a separate shell per command won't keep an `export`. Structured,
exact, no screenshots:

```bash
# List panes (id / size / state / title / cwd); --format json for parsing
TMPDIR=/tmp/otr ./target/debug/oakterm-ctl pane list

# Send input to pane 0 (raw bytes to the PTY; --enter appends CR)
TMPDIR=/tmp/otr ./target/debug/oakterm-ctl pane input 0 "echo hello" --enter

# Read pane 0's current visible screen (or --lines N for scrollback)
TMPDIR=/tmp/otr ./target/debug/oakterm-ctl pane output 0
```

**Verify a round-trip** — send a command, read it back:

```bash
TMPDIR=/tmp/otr ./target/debug/oakterm-ctl pane input 0 "echo SMOKE_OK" --enter
sleep 1
TMPDIR=/tmp/otr ./target/debug/oakterm-ctl pane output 0   # should contain SMOKE_OK
```

`pane input` validates the pane exists and is running first, so a bad id errors
instead of silently no-op'ing.

## GUI keys and screenshots, without taking focus

`pane input` writes to the PTY, so it can't reach GUI-side features: copy mode,
the find bar, keybindings. For those, send keys to the oakterm process itself
and capture its window by id. Neither brings oakterm to the front, so nothing
you send can land in the window the user is working in.

Build the helper once (the binary goes in `/tmp/otr`, not the repo):

```bash
swiftc -O .claude/skills/run-oakterm/uidrive.swift -o /tmp/otr/uidrive
```

Then, with oakterm running:

```bash
PID=$(cat /tmp/otr/oakterm.pid)
WID=$(/tmp/otr/uidrive winid "$PID")
/tmp/otr/uidrive key "$PID" "{cmd+f}" "deepn"   # a chord, then typed text
/tmp/otr/uidrive key "$PID" "{shift+enter}" "{esc}"
screencapture -x -o -l "$WID" /tmp/otr/shot.png  # this window only, even behind others
# then Read /tmp/otr/shot.png; crop with sips if needed:
#   sips --cropToHeightWidth <h> <w> --cropOffset <y> <x> /tmp/otr/shot.png
```

Measured 2026-11-06: oakterm (winit) accepts keys posted with
`CGEventPostToPid` while another app is frontmost, and Cmd+F, typing, Enter
and Escape all worked. Chords need the key in `uidrive.swift`'s table (US ANSI
codes). Add `RUST_LOG=oakterm=debug` to the launch when a GUI action needs
explaining. Text maps through the same table: a character missing from it fails
rather than reaching a key binding as the wrong key. The lasting fix is input
injection through the control socket (OT-330).

oakterm is a bare binary, not a `.app` bundle, so computer-use can't allowlist
it.

## Do not

- **Don't send input with `osascript keystroke`** — it types into whatever
  window is frontmost, which may be the user's, and mangles emoji and
  non-ASCII. Use `oakterm-ctl pane input` for the shell and `uidrive key` for
  the GUI.
- **Don't use `set frontmost` plus a full-screen `screencapture`** — use
  `screencapture -l` with the window id.
- **Don't use `>` to a possibly-existing log under zsh** (noclobber) — use `>|`.

## Cleanup

```bash
kill "$(cat /tmp/otr/oakterm.pid)"
```

The default (non-persist) daemon self-reaps when its last client disconnects.
Don't `pkill` by name: that also closes an oakterm the user is running.
