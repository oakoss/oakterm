//! Replay a captured `tree -C` PTY output through the VT parser.
//!
//! Stresses `process_bytes` with realistic flood-output: SGR color sequences
//! plus box-drawing on every line. Captured via `script(1)` so the byte
//! stream matches what the parser sees in production — important for any
//! future fixture generated from a command that gates color/escapes on
//! `isatty` (e.g. `ls --color=auto`, `git -c color.ui=auto`). Use to spot
//! regressions in parser throughput when changing the VT handler,
//! scrollback prune path, or row dirty tracking.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use oakterm_terminal::grid::Grid;
use oakterm_terminal::handler::process_bytes;

const TREE_OUTPUT: &[u8] = include_bytes!("fixtures/tree_output.bin");

fn bench_tree_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_replay");
    group.throughput(Throughput::Bytes(TREE_OUTPUT.len() as u64));
    group.bench_function("process_bytes_80x24", |b| {
        b.iter_batched(
            || Grid::new(80, 24),
            |mut grid| process_bytes(&mut grid, TREE_OUTPUT, &mut std::io::sink()),
            criterion::BatchSize::SmallInput,
        );
    });
    // Wider grid (matches typical full-screen terminal): more cells per line,
    // more work per row when SGR sequences shift colors.
    group.bench_function("process_bytes_200x50", |b| {
        b.iter_batched(
            || Grid::new(200, 50),
            |mut grid| process_bytes(&mut grid, TREE_OUTPUT, &mut std::io::sink()),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_tree_replay);
criterion_main!(benches);
