use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use oakterm_terminal::grid::row::Row;
use oakterm_terminal::scroll::HotBuffer;
use oakterm_terminal::scroll::archive_manager::ArchiveManager;

const COLS: usize = 93;
const ROWS_PER_ITER: usize = 2000;

/// Hot-buffer limit sized so prune batches have production shape (~10% of
/// the buffer per prune, many rows per batch) while staying small enough
/// that every iteration prunes continuously — the sustained-scroll regime
/// the vtebench parity run measured.
const HOT_LIMIT_BYTES: usize = 2 * 1024 * 1024;

fn ascii_row() -> Row {
    let mut row = Row::new(COLS);
    for (i, cell) in row.cells.iter_mut().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let offset = (i % 94) as u8;
        cell.codepoint = char::from(b'!' + offset);
    }
    row
}

/// The prune path as `ScreenSet::push_to_scrollback` runs it: push into the
/// hot buffer, hand any pruned rows to the archive.
fn push_rows(hot: &mut HotBuffer, archive: Option<&mut ArchiveManager>, template: &Row) {
    let mut archive = archive;
    for _ in 0..ROWS_PER_ITER {
        let pruned = hot.push(template.clone());
        if !pruned.is_empty()
            && let Some(mgr) = archive.as_deref_mut()
        {
            mgr.archive_rows(pruned).expect("archive_rows");
        }
    }
}

/// Fill a hot buffer to its limit so every benchmark iteration runs in the
/// steady pruning state rather than the cheap fill-up phase.
fn prefilled_buffer(template: &Row) -> HotBuffer {
    let mut hot = HotBuffer::new(HOT_LIMIT_BYTES);
    while hot.push(template.clone()).is_empty() {}
    hot
}

fn bench_prune_path(c: &mut Criterion) {
    let template = ascii_row();
    let mut group = c.benchmark_group("scroll_archive");
    group.throughput(Throughput::Elements(ROWS_PER_ITER as u64));

    group.bench_with_input(
        BenchmarkId::new("prune", "archive_off"),
        &template,
        |b, template| {
            let mut hot = prefilled_buffer(template);
            b.iter(|| push_rows(&mut hot, None, std::hint::black_box(template)));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("prune", "archive_on"),
        &template,
        |b, template| {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut hot = prefilled_buffer(template);
            let mut mgr = ArchiveManager::new(dir.path().join("session"), 256 * 1024 * 1024)
                .expect("ArchiveManager");
            b.iter(|| push_rows(&mut hot, Some(&mut mgr), std::hint::black_box(template)));
            mgr.shutdown().expect("shutdown");
        },
    );

    group.finish();
}

criterion_group!(benches, bench_prune_path);
criterion_main!(benches);
