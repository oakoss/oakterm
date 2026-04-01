use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use oakterm_terminal::grid::row::Row;
use oakterm_terminal::scroll::HotBuffer;
use oakterm_terminal::search::{SearchEngine, SearchMode};

const ROW_COUNT: usize = 10_000;
const COLS: usize = 80;

/// Build a row with the given text placed once, space-padded to `COLS`.
fn text_row(text: &str) -> Row {
    let mut row = Row::new(COLS);
    for (i, cell) in row.cells.iter_mut().enumerate() {
        let ch = text.as_bytes().get(i).copied().unwrap_or(b' ');
        cell.codepoint = char::from(ch);
    }
    row
}

/// Build a `HotBuffer` with `ROW_COUNT` rows. Every 100th row contains
/// the needle "FINDME"; the rest are filler text.
fn populated_buffer() -> HotBuffer {
    let mut buf = HotBuffer::new(512 * 1024 * 1024);
    for i in 0..ROW_COUNT {
        let row = if i % 100 == 0 {
            text_row("prefix FINDME suffix data")
        } else {
            text_row("the quick brown fox jumps over the lazy dog")
        };
        let _ = buf.push(row);
    }
    buf
}

fn bench_hot_buffer_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("scroll_search");

    // Tight byte budget so pruning fires regularly.
    let row = Row::new(COLS);
    let row_size = std::mem::size_of::<Row>()
        + row.cells.capacity() * std::mem::size_of::<oakterm_terminal::grid::cell::Cell>();
    let max_bytes = row_size * 500;

    group.throughput(Throughput::Elements(1));
    group.bench_function("hot_buffer_push", |b| {
        let mut buf = HotBuffer::new(max_bytes);
        for _ in 0..600 {
            let _ = buf.push(Row::new(COLS));
        }
        b.iter(|| {
            let _ = buf.push(std::hint::black_box(Row::new(COLS)));
        });
    });
    group.finish();
}

fn bench_search_simple(c: &mut Criterion) {
    let buf = populated_buffer();
    let mut group = c.benchmark_group("scroll_search");
    group.throughput(Throughput::Elements(ROW_COUNT as u64));
    group.bench_function("search_simple", |b| {
        b.iter_batched(
            || SearchEngine::new("FINDME", SearchMode::CaseSensitive).unwrap(),
            |mut engine| {
                engine.search(std::hint::black_box(&buf));
                engine.match_count()
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_search_regex(c: &mut Criterion) {
    let buf = populated_buffer();
    let mut group = c.benchmark_group("scroll_search");
    group.throughput(Throughput::Elements(ROW_COUNT as u64));
    group.bench_function("search_regex", |b| {
        b.iter_batched(
            || SearchEngine::new("FIND.*suffix", SearchMode::Regex).unwrap(),
            |mut engine| {
                engine.search(std::hint::black_box(&buf));
                engine.match_count()
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_hot_buffer_push,
    bench_search_simple,
    bench_search_regex
);
criterion_main!(benches);
