use criterion::{Criterion, criterion_group, criterion_main};
use oakterm_terminal::grid::Grid;
use oakterm_terminal::grid::row::Row;

/// Write a single character to a grid cell at a specific position.
fn cell_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("screen_buffer");
    group.bench_function("cell_write", |b| {
        let mut i = 0usize;
        let mut grid = Grid::new(120, 40);
        b.iter(|| {
            let row_idx = i % 40;
            let col_idx = i % 120;
            grid.lines[row_idx].cells[col_idx].codepoint = std::hint::black_box('A');
            i = i.wrapping_add(1);
        });
    });
    group.finish();
}

/// Scroll a full grid up by one line (replicates `do_scroll_up` core logic).
fn line_scroll(c: &mut Criterion) {
    let mut group = c.benchmark_group("screen_buffer");
    group.bench_function("line_scroll", |b| {
        b.iter_batched(
            || {
                let mut grid = Grid::new(120, 40);
                for row in &mut grid.lines {
                    for cell in &mut row.cells {
                        cell.codepoint = 'X';
                    }
                }
                grid
            },
            |mut grid| {
                let cols = grid.cols as usize;
                let bottom = (grid.rows - 1) as usize;
                grid.lines[0..=bottom].rotate_left(1);
                grid.lines[bottom] = Row::new(cols);
                std::hint::black_box(&grid.lines[bottom]);
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// Resize a 120x40 grid to 80x24 and back.
fn resize(c: &mut Criterion) {
    let mut group = c.benchmark_group("screen_buffer");
    group.bench_function("resize", |b| {
        b.iter_batched(
            || Grid::new(120, 40),
            |mut grid| {
                grid.resize(80, 24);
                grid.resize(120, 40);
                std::hint::black_box(grid.cols);
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// Fill an entire 120x40 grid with characters (4800 cell writes).
fn fill_grid(c: &mut Criterion) {
    let mut group = c.benchmark_group("screen_buffer");
    group.bench_function("fill_grid", |b| {
        b.iter_batched(
            || Grid::new(120, 40),
            |mut grid| {
                for row in &mut grid.lines {
                    for cell in &mut row.cells {
                        cell.codepoint = std::hint::black_box('A');
                    }
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, cell_write, line_scroll, resize, fill_grid);
criterion_main!(benches);
