# Bench Fixtures

Captured byte streams used by the criterion benches in this directory. All
fixtures are checked in so benches are reproducible across machines and CI;
they should stay small (hundreds of KB, not MB).

## `tree_output.bin`

Used by: `benches/tree_replay.rs`

A `tree -C` output of a populated `~/.cargo/registry/src` (~7,500 dirs,
41k files), captured via `script(1)` so the byte stream matches what the
VT parser sees in production. `tree -C` itself forces SGR through pipes,
so this particular fixture would survive a plain redirect, but the
`script(1)` capture path is the right pattern for any future fixture
from a command that gates color on `isatty` (e.g. `ls --color=auto`).
Trimmed to ~200 KB at a newline boundary.

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

The exact registry contents don't matter — any large directory works. The
goal is realistic SGR + box-drawing density, not a specific byte sequence.
