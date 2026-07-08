---
spec: '0011'
title: Input Encoding & Keyboard Protocol
status: accepted
date: 2026-07-06
adrs: ['0016']
tags: [core]
---

# 0011. Input Encoding & Keyboard Protocol

## Overview

Defines the input side of the terminal: the contract that turns a physical
key press into the bytes written to the PTY. Spec-0002 covers the output path
(PTY bytes → screen) thoroughly; this spec covers the reverse. It formalizes
modifier encoding, DECCKM application-cursor keys, DECKPAM application keypad,
the alt-as-meta policy, the backspace/delete policy, the level of Kitty
keyboard / CSI u / modifyOtherKeys support, and the IME/dead-key composition
model.

The encoding is split across the daemon/client boundary of ADR-0007. The
**client** owns the keyboard: it receives platform key events (winit) and
encodes them to bytes. The **daemon** owns terminal mode state (DECCKM,
DECKPAM, the Kitty flag stack) because those modes are set by escape sequences
in the PTY stream that only the daemon's VT handler sees (Spec-0002). Correct
encoding therefore requires the client to know the daemon's current input
modes: this spec amends Spec-0001's `RenderUpdate` to carry them (see
[Spec-0001 Amendment](#spec-0001-amendment-renderupdate-input-modes)).

This spec formalizes the encoding that `crates/oakterm/src/main.rs`
(`key_to_bytes`) and the daemon's `KeyInput` path
(`crates/oakterm-daemon/src/requests/input.rs`) implement. Where today's code
diverges from the contract, the divergence is a bug against this spec, not a
new design (see [Divergences From Current Code](#divergences-from-current-code)).

## Contract

### Encoding Pipeline

```text
winit KeyEvent ──▶ keybind lookup (key_without_modifiers)  ──▶ [action, consumed]
     │                                                              │ not consumed
     ▼                                                              ▼
 IME filter (preedit never encodes)  ──▶  key_to_bytes(mode state)  ──▶ KeyInput(bytes)
                                                                        │
                                                                        ▼  (Spec-0001 0x64)
                                                                    daemon writes bytes to PTY
```

Two consumers read the same key event: the **keybind registry** (does this
chord trigger an OakTerm action?) and the **PTY encoder** (`key_to_bytes`).
Both must resolve the physical key, not the platform-composed character — see
[Keybind Lookup Layer](#keybind-lookup-layer).

Encoding depends on four pieces of daemon-owned mode state, delivered to the
client in every `RenderUpdate`:

| State                | DEC mode     | Source (daemon)             | Effect on encoding                               |
| -------------------- | ------------ | --------------------------- | ------------------------------------------------ |
| Cursor-key mode      | DECCKM (1)   | `g.modes.get(1)`            | Arrows / Home / End use SS3 instead of CSI       |
| Keypad mode          | DECKPAM (66) | `g.modes.get(66)`           | Numpad keys send SS3 application sequences       |
| Kitty keyboard flags | (flag stack) | top of the per-grid stack   | Selects the Kitty encoding and which keys report |
| modifyOtherKeys lvl  | (xterm)      | `g.modify_other_keys` (0-2) | Disambiguates control chords in legacy mode      |

### Modifier Encoding (CSI 1 ; N)

Modifiers on cursor, editing, and function keys are encoded as an xterm
modifier parameter `N = 1 + bitmask`. The `+1` follows the CSI convention that
a missing parameter equals 1, so the unmodified state is `N = 1` (and the
parameter is omitted entirely).

| Bit value | Modifier           |
| --------- | ------------------ |
| 1         | Shift              |
| 2         | Alt (Meta)         |
| 4         | Ctrl               |
| 8         | Super (xterm Meta) |

Resulting parameter values:

| Modifiers      | N   |
| -------------- | --- |
| (none)         | 1   |
| Shift          | 2   |
| Alt            | 3   |
| Shift+Alt      | 4   |
| Ctrl           | 5   |
| Shift+Ctrl     | 6   |
| Alt+Ctrl       | 7   |
| Shift+Alt+Ctrl | 8   |
| Super          | 9   |

`ESC` denotes byte `0x1B`. `CSI` = `ESC [` (`0x1B 0x5B`). `SS3` = `ESC O`
(`0x1B 0x4F`).

### Cursor & Editing Keys

Two families. CSI-final keys (arrows, Home, End) toggle between CSI and SS3 by
DECCKM when **unmodified**; when modified they always take the CSI `1;N` form.
Tilde keys (Insert, Delete, Page Up/Down) are unaffected by DECCKM and take a
`;N` modifier parameter before the `~`.

**CSI-final keys** (final byte in the last column):

| Key         | Unmodified, DECCKM off | Unmodified, DECCKM on | With modifier N | Final |
| ----------- | ---------------------- | --------------------- | --------------- | ----- |
| Arrow Up    | `ESC [ A`              | `ESC O A`             | `ESC [ 1 ; N A` | `A`   |
| Arrow Down  | `ESC [ B`              | `ESC O B`             | `ESC [ 1 ; N B` | `B`   |
| Arrow Right | `ESC [ C`              | `ESC O C`             | `ESC [ 1 ; N C` | `C`   |
| Arrow Left  | `ESC [ D`              | `ESC O D`             | `ESC [ 1 ; N D` | `D`   |
| Home        | `ESC [ H`              | `ESC O H`             | `ESC [ 1 ; N H` | `H`   |
| End         | `ESC [ F`              | `ESC O F`             | `ESC [ 1 ; N F` | `F`   |

A modified cursor key is always CSI form even when DECCKM is on: `Ctrl+Up` is
`ESC [ 1 ; 5 A` regardless of cursor-key mode. This matches xterm.

**Tilde keys** (parameter `p`, final byte `~`):

| Key       | `p` | Unmodified  | With modifier N |
| --------- | --- | ----------- | --------------- |
| Insert    | 2   | `ESC [ 2 ~` | `ESC [ 2 ; N ~` |
| Delete    | 3   | `ESC [ 3 ~` | `ESC [ 3 ; N ~` |
| Page Up   | 5   | `ESC [ 5 ~` | `ESC [ 5 ; N ~` |
| Page Down | 6   | `ESC [ 6 ~` | `ESC [ 6 ; N ~` |

Home and End are encoded in the CSI-final form above (`ESC [ H` / `ESC [ F`),
not the tilde form (`ESC [ 1 ~` / `ESC [ 4 ~`). OakTerm reports
`TERM=xterm-256color` (Spec-0002), whose terminfo `khome`/`kend` are the
CSI-final sequences.

### Function Keys

F1–F4 are SS3-based (matching VT100 PF1–PF4); F5–F12 are tilde keys. When
modified, F1–F4 switch to the CSI `1;N` form.

| Key | Unmodified   | With modifier N  |
| --- | ------------ | ---------------- |
| F1  | `ESC O P`    | `ESC [ 1 ; N P`  |
| F2  | `ESC O Q`    | `ESC [ 1 ; N Q`  |
| F3  | `ESC O R`    | `ESC [ 1 ; N R`  |
| F4  | `ESC O S`    | `ESC [ 1 ; N S`  |
| F5  | `ESC [ 15 ~` | `ESC [ 15 ; N ~` |
| F6  | `ESC [ 17 ~` | `ESC [ 17 ; N ~` |
| F7  | `ESC [ 18 ~` | `ESC [ 18 ; N ~` |
| F8  | `ESC [ 19 ~` | `ESC [ 19 ; N ~` |
| F9  | `ESC [ 20 ~` | `ESC [ 20 ; N ~` |
| F10 | `ESC [ 21 ~` | `ESC [ 21 ; N ~` |
| F11 | `ESC [ 23 ~` | `ESC [ 23 ; N ~` |
| F12 | `ESC [ 24 ~` | `ESC [ 24 ; N ~` |

The `16`, `22` gap in the F5–F12 parameters is intentional (historical xterm
numbering); OakTerm does not fill it.

### Keypad (DECKPAM / DECNKM)

The numeric keypad has two modes. **Numeric mode** (default, DECKPNM / DECRST 66) sends the ordinary characters printed on the keys. **Application mode**
(DECKPAM, `ESC =`, or DECSET 66) sends SS3 sequences so applications can bind
the numpad distinctly from the top-row digits.

| Numpad key  | Numeric mode  | Application mode |
| ----------- | ------------- | ---------------- |
| 0           | `0`           | `ESC O p`        |
| 1           | `1`           | `ESC O q`        |
| 2           | `2`           | `ESC O r`        |
| 3           | `3`           | `ESC O s`        |
| 4           | `4`           | `ESC O t`        |
| 5           | `5`           | `ESC O u`        |
| 6           | `6`           | `ESC O v`        |
| 7           | `7`           | `ESC O w`        |
| 8           | `8`           | `ESC O x`        |
| 9           | `9`           | `ESC O y`        |
| . (decimal) | `.`           | `ESC O n`        |
| Enter       | `CR` (`0x0D`) | `ESC O M`        |
| +           | `+`           | `ESC O k`        |
| -           | `-`           | `ESC O m`        |
| *           | `*`           | `ESC O j`        |
| /           | `/`           | `ESC O o`        |
| =           | `=`           | `ESC O X`        |

Numpad arrows / navigation keys (Num Lock off) are encoded as their named
equivalents (Arrow, Home, Page Up, …) per the cursor/editing tables, subject
to DECCKM, not DECKPAM.

### Backspace & Delete Policy

| Key            | Bytes        | Notes                                |
| -------------- | ------------ | ------------------------------------ |
| Backspace      | `0x7F` (DEL) | xterm default (`backarrowKey` off).  |
| Ctrl+Backspace | `0x08` (BS)  | Word-erase in many shells/readline.  |
| Alt+Backspace  | `ESC 0x7F`   | Readline `backward-kill-word`.       |
| Delete         | `ESC [ 3 ~`  | Tilde key; takes `;N` when modified. |

Backspace sends DEL (`0x7F`), not BS (`0x08`). This is the modern default for
`xterm-256color` and what readline, bash, and zsh expect; sending `0x08` is the
classic "backspace deletes forward / prints `^H`" misconfiguration. There is no
`backspace_as_delete` config toggle in Phase 1; the value is fixed at `0x7F`.

### Alt-as-Meta vs Alt-Composes

On a physical <kbd>Alt</kbd>+key press the terminal must choose between two
incompatible behaviors:

- **Alt-as-meta:** prefix the key's bytes with `ESC`. `Alt+b` → `ESC b`
  (`0x1B 0x62`), which readline reads as `backward-word`. This is the
  Linux/Windows default and the reason meta bindings work.
- **Alt-composes:** let the OS input layer fold Alt into a composed character.
  On macOS, <kbd>Option</kbd>+<kbd>b</kbd> is `∫`, <kbd>Option</kbd>+<kbd>h</kbd>
  is `˙`; the platform delivers the composed glyph as the key's text and the
  terminal sends its UTF-8 bytes. This is the macOS default and the reason
  macOS users can type `€ # ∆` etc.

The two cannot both hold for the same key press. The industry-standard control
is a config option — Alacritty `option_as_alt` (`None`/`OnlyLeft`/`OnlyRight`/
`Both`), Ghostty `macos-option-as-alt`, WezTerm `send_composed_key_when_*_alt_is_pressed`.

**Policy:**

- **Linux/Windows:** Alt is meta. `Alt+<key>` emits `ESC` + the key's
  unmodified encoding. This is not configurable (there is no compose layer to
  preserve).
- **macOS:** governed by `macos_option_as_alt` (Lua config, snake_case per
  ADR-0005), with the same tri-state as Alacritty:

  | Value     | Meaning                                             |
  | --------- | --------------------------------------------------- |
  | `false`   | Both Option keys compose (Unicode input preserved). |
  | `true`    | Both Option keys are meta (ESC-prefix).             |
  | `"left"`  | Left Option is meta; right Option composes.         |
  | `"right"` | Right Option is meta; left Option composes.         |

  **Default: `false`** (decided 2026-07-06). All three reference terminals
  default the Option key to _compose_ on macOS, preserving Unicode input out
  of the box; a meta default silently breaks `Option+3 → #` and every accented
  character. Users who live in Emacs/readline opt into `true`.

When alt-as-meta is active, the ESC prefix wraps the key's already-computed
bytes: `Alt+Right` → `ESC` + `ESC [ C` = `ESC ESC [ C`. For a plain character
the modifier is expressed as the ESC prefix, not as a CSI `1;N` parameter (the
character has no CSI form in legacy mode).

### Keybind Lookup Layer

Keybind matching and PTY encoding consume the same modifier state, so the
alt-composes decision reaches the keybind side too. A keybind chord must be
built from the **layout key** (winit `key_without_modifiers`), not the
composed `logical_key`. On macOS with Option held, `logical_key` is the
composed glyph (`˙` for <kbd>⌥H</kbd>), so a chord built from it can never
match a binding written as `super+alt+h` — the binding is keyed on `h` but the
event carries `˙`. Resolving the chord against `key_without_modifiers` yields
`h` and the match succeeds. This holds regardless of `macos_option_as_alt`:
keybind resolution always uses the physical layout key; only the PTY-bytes path
honors compose-vs-meta.

### Kitty Keyboard Protocol / CSI u / modifyOtherKeys

The legacy encoding above cannot represent large classes of key presses:
<kbd>Ctrl+Tab</kbd>, <kbd>Ctrl+Enter</kbd>, <kbd>Shift+Enter</kbd>,
<kbd>Ctrl+.</kbd>, key-release events, or which physical key produced a
control byte (<kbd>Ctrl+I</kbd> and <kbd>Tab</kbd> are both `0x09`). Modern
editors (Neovim ≥ 0.10, Helix, fish, Kakoune) detect and prefer a richer
protocol. Three mechanisms exist:

- **xterm modifyOtherKeys** — enabled with `CSI > 4 ; 2 m`. Reports otherwise
  ambiguous chords as `CSI 27 ; N ; codepoint ~`. Level 1 encodes only the
  ambiguous cases; level 2 encodes all modified keys. Narrow; no key-release,
  no key-repeat, no disambiguated modifiers.
- **fixterms / CSI u** (leonerd) — encodes a modified key as
  `CSI codepoint ; N u`. <kbd>Ctrl+I</kbd> = `CSI 105 ; 5 u`,
  <kbd>Ctrl+Enter</kbd> = `CSI 13 ; 5 u`, <kbd>Shift+Tab</kbd> keeps `CSI Z`.
  Same `N = 1 + bitmask` modifier formula as above. Broader than
  modifyOtherKeys but still no event types.
- **Kitty keyboard protocol** — a progressive-enhancement superset of CSI u,
  the de-facto modern standard (kitty, foot, Ghostty, WezTerm, Alacritty all
  implement it). A per-screen flag stack is manipulated by:

  | Sequence               | Meaning                                                |
  | ---------------------- | ------------------------------------------------------ |
  | `CSI ? u`              | Query current flags → terminal replies `CSI ? flags u` |
  | `CSI > flags u`        | Push `flags` onto the stack (flags default 0)          |
  | `CSI = flags ; mode u` | Set/or/and flags on the current level                  |
  | `CSI < number u`       | Pop `number` levels (default 1)                        |

  Progressive-enhancement flag bits:

  | Bit       | Meaning                                   |
  | --------- | ----------------------------------------- |
  | `0b1`     | Disambiguate escape codes                 |
  | `0b10`    | Report event types (press/repeat/release) |
  | `0b100`   | Report alternate keys                     |
  | `0b1000`  | Report all keys as escape codes           |
  | `0b10000` | Report associated text                    |

  Keys encode as `CSI unicode-key ; N : event-type ; text-codepoints u`, with
  the modifier `N = 1 + bitmask` and the same bit values as the modifier table
  above plus super=8, hyper=16, meta=32, caps_lock=64, num_lock=128.
  <kbd>Enter</kbd> (`0x0D`), <kbd>Tab</kbd> (`0x09`), and <kbd>Backspace</kbd>
  (`0x7F`) keep legacy byte encodings even with the protocol on, so a shell
  survives a crashed full-screen app that left the mode set.

**Support-level options** (decided 2026-07-06: Option C):

| Option | Support level                                                                                           | Cost | nvim/helix |
| ------ | ------------------------------------------------------------------------------------------------------- | ---- | ---------- |
| A      | Legacy only (today). No CSI u, modifyOtherKeys, or Kitty.                                               | none | degraded   |
| B      | + modifyOtherKeys levels 1–2.                                                                           | low  | partial    |
| C      | + Kitty keyboard protocol (flags 0b1, 0b10, 0b1000) with legacy as the disabled baseline. **Selected.** | med  | full       |
| D      | Kitty + modifyOtherKeys + CSI u (accept all, advertise Kitty).                                          | high | full       |

**Option C.** Neovim and Helix — named in the task as the driving
consumers — detect and prefer the Kitty protocol; it is the format their
users expect and the one every peer terminal now speaks. ADR-0016 already
commits OakTerm to correct keyboard-protocol _passthrough_ for tmux
coexistence, so the daemon's grid must model the flag stack regardless;
Option C makes OakTerm itself a first-class producer, not only a passthrough.
Flags `0b1 | 0b10 | 0b1000` (disambiguate + event types + all-keys-as-escapes)
cover the editor use cases; `0b100` (alternate keys) and `0b10000` (associated
text) can follow. modifyOtherKeys is a lesser fallback to the same editors;
if it is ever added, it is secondary to Kitty.

The flag stack is **daemon-owned per grid** (set by PTY escape sequences the
daemon parses) but the **client encodes**, so the active top-of-stack flags
must reach the client — see the amendment below. The stack is per-screen: the
Kitty spec resets it on alternate-screen switches, consistent with Spec-0002's
alt-screen model.

### IME & Dead-Key Composition

Text composition (CJK IME, dead keys, Option-compose on macOS) is a
**GUI-local** process. It runs entirely in the client between the platform
input method and the on-screen preedit; it is **not** terminal state and the
daemon has no part in it.

**Invariant (load-bearing): preedit bytes never reach the PTY.** Only a
_committed_ string is encoded to `KeyInput`. While the user is mid-composition
— a half-formed CJK character, a pending dead key (`´` waiting for a vowel), a
macOS Option-compose in flight — the client holds the preedit locally and sends
nothing. A commit produces the final UTF-8, which is sent as ordinary character
input (the [Character Input](#character-input) path). An aborted composition
(Escape / focus loss) discards the preedit and sends nothing.

Concretely, against winit 0.30:

- The client calls `Window::set_ime_allowed(true)` so the platform delivers
  `WindowEvent::Ime` events.
- `Ime::Preedit { text, cursor }` updates a client-local preedit buffer and the
  caret position _within_ it. No PTY bytes.
- `Ime::Commit(text)` clears the preedit buffer and encodes `text` as committed
  character input to the PTY.
- `WindowEvent::KeyboardInput` events that arrive while a preedit is active must
  not be double-encoded: the winit `KeyEvent` for a key consumed by the IME is
  ignored for PTY purposes (the commit is the source of truth).

**Preedit rendering.** The composing text is drawn by the client as an overlay
on the grid at the cursor's cell, styled distinctly (underline) to mark it as
uncommitted. Because it is client-local, it is not part of the daemon's screen
buffer, carries no `seqno`, and never appears in scrollback, selection, or a
`RenderUpdate`. It is painted after the grid, on top of the cell the daemon
reports as the cursor position.

**Cursor positioning during composition.** The hardware cursor the daemon
reports (`RenderUpdate.cursor_x/y`) marks the insertion point. The preedit
overlay begins at that cell and extends rightward; the IME caret (`Ime::Preedit`
`cursor` span) is drawn within the overlay. The client reports the overlay's
screen rectangle back to the platform via `Window::set_ime_cursor_area` so the
OS candidate window (the CJK selection popup) anchors under the composing text.
The daemon's cursor does not move during composition — it moves only when the
commit is written and echoed back through a normal `RenderUpdate`.

### Character Input

A committed character (ordinary typing, or the result of a compose/IME commit)
is encoded as its UTF-8 bytes with no escape framing, except:

- **Ctrl+letter** maps to the C0 control byte: `Ctrl+A` → `0x01` … `Ctrl+Z` →
  `0x1A`. `Ctrl+@` → `0x00`, `Ctrl+[` → `0x1B`, `Ctrl+\` → `0x1C`,
  `Ctrl+]` → `0x1D`, `Ctrl+^` → `0x1E`, `Ctrl+_` → `0x1F`, `Ctrl+Space` →
  `0x00`. In legacy mode these are indistinguishable from their aliases
  (`Ctrl+I` = `Tab` = `0x09`); the Kitty protocol (when enabled) disambiguates
  them.
- **Alt+character** is subject to the [alt-as-meta policy](#alt-as-meta-vs-alt-composes).
- **Enter** sends `CR` (`0x0D`), or `CR LF` (`0x0D 0x0A`) when LNM (ANSI mode 20,
  Spec-0002) is set on the grid.
- **Tab** sends `0x09`; **Shift+Tab** sends `CSI Z` (`ESC [ Z`, CBT).
- **Escape** sends `0x1B`.

## Spec-0001 Amendment (RenderUpdate Input Modes)

`RenderUpdate` (0x72) must carry the daemon-owned input-mode state so the
client can encode keys correctly. Today the client's encoder is blind to
DECCKM, DECKPAM, and the Kitty flag stack, so it cannot honor them (see
[Divergences](#divergences-from-current-code)). Two bytes are inserted into the
`RenderUpdate` payload immediately after `alt_screen` and before
`dirty_row_count`:

```text
Field              Type        Notes
─────────────────  ──────────  ──────────────────────────────────
alt_screen         u8          (existing)
input_flags        u8   NEW    bit0: DECCKM cursor-key mode (g.modes.get(1))
                               bit1: application keypad mode (g.modes.get(66))
                               bits2-3: modifyOtherKeys level (0,1,2)
                               bits4-7: reserved, must be 0
kitty_kbd_flags    u8   NEW    Kitty progressive-enhancement flags at the top of
                               the active grid's flag stack. 0 = protocol
                               disabled (legacy encoding).
dirty_row_count    u16 LE      (existing)
dirty_rows         [DirtyRow]  (existing)
```

This grows the `RenderUpdate` fixed prefix from 25 to 27 bytes.
`RenderUpdate::decode`'s minimum-length check moves from `< 25` to `< 27`, and
`dirty_row_count` reads at offset 25 instead of 23. Populate on the daemon side
in `crates/oakterm-daemon/src/requests/render.rs`:

```rust
let input_flags = u8::from(g.modes.get(1))            // DECCKM
    | (u8::from(g.modes.get(66)) << 1)                // application keypad
    | (g.modify_other_keys << 2);                     // 0..=2
let kitty_kbd_flags = g.kitty_kbd_flags();            // top of stack, 0 if empty
```

`bit1` covers DECKPAM/DECNKM (mode 66); `ESC =` / `ESC >` set the same flag via
Spec-0002's `set_keypad_application_mode`. `modify_other_keys` and
`kitty_kbd_flags()` are new grid state introduced only if the corresponding
support level (modifyOtherKeys, Kitty) is adopted; until then both fields are 0
and the client uses legacy encoding, so the amendment is safe to land ahead of
the Kitty work.

**Versioning classification (decided 2026-07-06): minor bump; renumbered 1.2 → 1.3 → 1.4 as `ListTabs`/`TabList` (1.2) and then the tab-lifecycle ops (1.3, TREK-209) shipped first (minors are assigned in implementation order).**
Spec-0001's governance rules make a field insertion into a hand-rolled binary
layout a _breaking_ (major) change, because the layout is positional and older
decoders read fixed offsets. In Phase 0/1 the client and daemon ship in
lockstep at the same version, so growing the prefix is operationally safe: no
released, independently versioned client exists yet. The layout exception is
recorded alongside the amendment in Spec-0001; once independent clients ship,
positional insertions revert to major bumps as the rule prescribes. The
amendment ships with the protocol-1.4 batch (TREK-140/161/133/134/172/128/232)
so the wire changes land as one bump.

## Behavior

### Precedence

For each key press the client resolves in order:

1. **Keybind** — build the chord from `key_without_modifiers` and look it up. If
   a bound action consumes the key, stop; emit no PTY bytes.
2. **IME** — if a preedit is active or the platform routed the key to the input
   method, the key is consumed by composition; emit no PTY bytes now (a later
   `Ime::Commit` will).
3. **Encode** — otherwise run `key_to_bytes` with the current mode state and
   send `KeyInput`.

An action that declines (returns "not handled", e.g. scroll-down when already
live) falls through to step 3, matching the existing registry contract.

### Empty Encodings

A key press that produces no bytes (a bare modifier press, a consumed keybind, a
preedit keystroke) sends no `KeyInput`. The daemon already treats empty
`key_data` as a no-op; the client should simply not send.

### Mode-State Races

Mode state travels in `RenderUpdate`, so the client's view can briefly lag the
daemon (an application enables DECCKM, and the user presses an arrow before the
next `RenderUpdate` arrives). This is benign: at most a handful of key presses
near a mode switch use the previous encoding, self-correcting on the next
render. The alternative — a synchronous mode query per key — is not worth the
round-trip on the hot input path. Applications tolerate this because real
terminals have the same PTY-echo latency between setting the mode and the user
reacting to the redraw.

### Focus & Bracketed Paste

Bracketed paste (DECSET 2004) already rides in `RenderUpdate.bracketed_paste`
(Spec-0001). Pasted text is wrapped by the client in `ESC [ 200 ~` … `ESC [ 201 ~`
when that flag is set; this is paste handling, not key encoding, and is out of
scope here beyond noting the flag's existing home.

## Constraints

- **Hot path:** `key_to_bytes` runs once per key press (≤ ~20/s human typing,
  higher on autorepeat). It must not allocate for the common single-character
  and short-sequence cases; a fixed stack buffer suffices (longest legacy
  sequence is `ESC [ 1 ; N ~`, ≤ 8 bytes; Kitty sequences with text are bounded
  by the grapheme length).
- **Encoding table is static** for legacy sequences: no per-press branching
  beyond the mode flags. The mode flags are read from the last `RenderUpdate`,
  not fetched.
- **UTF-8 only:** committed text is encoded as UTF-8; the client never sends
  Latin-1 or other legacy 8-bit encodings. This matches Spec-0002's UTF-8 input
  assumption.
- **No 8-bit C1:** control introducers are always the 7-bit `ESC`-prefixed
  forms (`ESC [`, `ESC O`), never the 8-bit C1 bytes (`0x9B`, `0x8F`), which
  collide with UTF-8 continuation bytes.
- **Kitty stack depth:** the per-grid flag stack is bounded (Kitty caps it; a
  small fixed depth such as 16 prevents unbounded growth from a malicious
  stream), consistent with Spec-0002's bounded-resource stance.

## What This Is Not

- **Not mouse encoding.** Mouse reporting (SGR 1006, X10, modes 1000–1007) is
  already specified by Spec-0002's mode inventory and implemented in the
  daemon's `MouseInput` path; this spec is keyboard-only.
- **Not keybind semantics.** Which actions exist and their default chords live
  in `19-smart-keybinds.md` and the config runtime (Spec-0005). This spec only
  fixes how a chord is _resolved_ to a physical key so encoding and binding
  agree.
- **Not the config schema.** `macos_option_as_alt` and any keyboard-protocol
  toggle are named here for their encoding effect; their Lua schema, defaults
  wiring, and validation belong to the config spec.
- **Not terminfo authorship.** OakTerm reports `xterm-256color` (Spec-0002) and
  matches its input sequences; it does not ship a custom terminfo entry.
- **Not the PTY transport.** Framing and delivery of `KeyInput` bytes are
  Spec-0001; this spec defines only the byte _content_.
- **Not paste transformation** beyond noting bracketed-paste's existing flag.

## Divergences From Current Code

The following are bugs against this contract in the code, each owned by an
implementation task:

1. **Arrows ignore DECCKM.** `key_to_bytes`
   (`crates/oakterm/src/main.rs`) hardcodes `ESC [ A` … `ESC [ D` for arrows and
   `ESC [ H` / `ESC [ F` for Home/End, never emitting the SS3 form, so
   application-cursor-mode apps (vim, less, fzf) receive the wrong arrow
   encoding. The daemon's mouse alt-scroll path already consults DECCKM
   (`g.modes.get(1)` in `requests/input.rs`), but the keyboard path — living in
   the client — has no access to the mode. This is the direct motivation for the
   `RenderUpdate` amendment.
2. **No modifier encoding.** `key_to_bytes` drops all modifiers: `Ctrl+Arrow`,
   `Shift+Arrow`, `Alt+Arrow`, and modified function keys all send the bare
   unmodified sequence. Selection-by-word, word-jump, and editor chords do not
   reach applications.
3. **No IME handling.** `WindowEvent::Ime` is never matched and
   `set_ime_allowed(true)` is never called, so winit emits no preedit events and
   dead-key/CJK composition does not work at all. `20-platform-support.md`
   claims working CJK IME and "no dead key bugs" — unimplemented today.
4. **Keybind lookup uses the composed key (CMT-163).** `winit_to_chord` builds
   the chord from `logical_key`, which on macOS with Option held is the composed
   glyph, so `oakterm.keybind("super+alt+h", …)` can never fire. Fix: resolve
   against `key_without_modifiers`.
5. **Application keypad is stored but never encoded.** The handler tracks mode
   66 (`set_keypad_application_mode`), but `key_to_bytes` has no numpad arm, so
   DECKPAM has no effect on output.
6. **`text`-first encoding leaks compose state.** `key_to_bytes` returns the
   winit `text` field before considering modifiers, so control and meta chords
   are encoded inconsistently across platforms depending on whether the platform
   populated `text`. The committed-text path should be reached only after
   keybind and modifier resolution.

## References

- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — `KeyInput`
  (0x64) transport and the amended `RenderUpdate` (0x72)
- [Spec 0002: VT Parser & Terminal Handler](0002-vt-parser.md) — DECCKM,
  DECKPAM, LNM, and the mode inventory this spec encodes against
- [Spec 0005: Lua Config Runtime](0005-lua-config-runtime.md) — home of
  `macos_option_as_alt` and any keyboard-protocol toggle
- [ADR 0007: Daemon Architecture](../adrs/0007-daemon-architecture.md) — the
  client/daemon split that separates encoding from mode state
- [ADR 0016: tmux Coexistence Stance](../adrs/0016-tmux-coexistence.md) —
  commits to correct keyboard-protocol passthrough
- [20-platform-support.md](../ideas/20-platform-support.md) — per-OS keyboard
  model and IME expectations
- [19-smart-keybinds.md](../ideas/19-smart-keybinds.md) — `super` abstraction and
  keybind defaults
- [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
- [fixterms / CSI u](http://www.leonerd.org.uk/hacks/fixterms/)
- [xterm ctlseqs — PC-Style Function Keys, modifyOtherKeys](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
