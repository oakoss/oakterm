# Bench Fixtures

Captured byte streams used by the criterion benches in this directory.

## Policy

**Synthetic by default** — see `vt_parser.rs::make_plain_ascii` and the
other generators in that file for the pattern. Full policy and rationale
live in [`.claude/rules/rust.md`](../../../../.claude/rules/rust.md)
(under "Bench Fixtures"). Anything committed here must:

- Trim aggressively (~100 KB target; up to ~250 KB if needed).
- Include a section below documenting the capture command and **why
  synthetic doesn't suffice**.
- Be marked `binary` in `.gitattributes` (extension already covered by
  `*.bin`).

## `tree_output.bin` (~200 KB)

Used by: `benches/tree_replay.rs`

A `tree -C` output of a populated `~/.cargo/registry/src` (~7,500 dirs,
41k files), captured via `script(1)`. Trimmed to ~200 KB at a newline
boundary.

**Why not synthetic:** the existing synthetic generators in `vt_parser.rs`
(`make_sgr_color`, `make_mixed_realistic`) cover SGR throughput in
isolation but don't reproduce the line-by-line SGR-reset density of real
file listings or the Unicode in real crate names — both of which exercise
the row dirty-tracking + cell-encoding paths the TREK-141 round-trip
storm hit. The slightly-over-target size buys stable bench numbers; the
fixture is loaded once at compile time via `include_bytes!`, so larger
input doesn't slow individual iterations.

`tree -C` itself forces SGR through pipes, so the `script(1)` capture
isn't strictly necessary for _this_ fixture — but it's the right pattern
for any future capture from a command that gates color on `isatty`
(e.g. `ls --color=auto`, `git -c color.ui=auto`).

To regenerate:

```bash
script -q /tmp/tree-cap.bin tree -C ~/.cargo/registry/src/<index-dir>

head -c 200000 /tmp/tree-cap.bin > /tmp/tree-trim.bin

python3 -c "
import sys
data = open('/tmp/tree-trim.bin', 'rb').read()
last_nl = data.rfind(b'\n')
trimmed = data[:last_nl+1] if last_nl >= 0 else data
open('crates/oakterm-terminal/benches/fixtures/tree_output.bin', 'wb').write(trimmed)
"
```

The exact registry contents don't matter — any large directory works.
The goal is realistic SGR + box-drawing density, not a specific byte
sequence.
