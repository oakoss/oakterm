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

```bash
mkdir -p /tmp/otr
TMPDIR=/tmp/otr ./target/debug/oakterm >| /tmp/otr/oakterm.log 2>&1 &
sleep 2   # daemon starts, PTY spawns on the first client resize
```

The daemon starts automatically as a child of the GUI. Confirm both are up and
the shell spawned:

```bash
pgrep -l oakterm            # expect: oakterm AND oakterm-daemon
grep -c "PTY spawned" /tmp/otr/oakterm.log
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

## Visual verification (only when you must SEE rendering)

For color, emoji, glyph layout, or cursor shape — things `pane output` (plain
text) can't show — capture the window and read the image:

```bash
osascript -e 'tell application "System Events" to set frontmost of (first process whose name is "oakterm") to true'
screencapture -x /tmp/otr/shot.png
# then Read /tmp/otr/shot.png; crop with sips if the window is small:
#   sips --cropToHeightWidth <h> <w> --cropOffset <y> <x> /tmp/otr/shot.png
```

oakterm is a bare binary, not a `.app` bundle, so computer-use can't allowlist
it (no `screenshot`/`zoom`/click) and `screencapture` only sees the frontmost
Space — if a fullscreen app is in front, `set frontmost` won't pull oakterm
across it.

## Do not

- **Don't send input with `osascript keystroke`** — it mangles emoji and
  non-ASCII. Use `oakterm-ctl pane input`.
- **Don't use `>` to a possibly-existing log under zsh** (noclobber) — use `>|`.

## Cleanup

```bash
pkill -x oakterm; pkill -x oakterm-daemon
```

The default (non-persist) daemon self-reaps when the last client disconnects,
but `pkill` both to be sure.
