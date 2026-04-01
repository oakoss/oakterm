use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use oakterm_terminal::grid::cell::{CellFlags, Color, NamedColor, UnderlineStyle};
use oakterm_terminal::grid::row::Row;
use oakterm_terminal::scroll::row_codec::{deserialize_row, serialize_row};

const COLS: usize = 80;

/// Build an 80-column row filled with printable ASCII.
fn ascii_row() -> Row {
    let mut row = Row::new(COLS);
    for (i, cell) in row.cells.iter_mut().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let offset = (i % 94) as u8;
        cell.codepoint = char::from(b'!' + offset);
    }
    row
}

/// Build an 80-column row with mixed styles on every cell.
fn styled_row() -> Row {
    let mut row = Row::new(COLS);
    for (i, cell) in row.cells.iter_mut().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let offset = (i % 26) as u8;
        cell.codepoint = char::from(b'A' + offset);
        cell.fg = match i % 3 {
            0 => Color::Named(NamedColor::Red),
            1 => Color::Indexed(42),
            _ => Color::Rgb(255, 128, 0),
        };
        cell.bg = Color::Named(NamedColor::Black);
        if i % 2 == 0 {
            cell.flags = CellFlags::BOLD.union(CellFlags::ITALIC);
        }
        if i % 4 == 0 {
            cell.underline_style = UnderlineStyle::Curly;
        }
    }
    row
}

fn bench_encode(c: &mut Criterion) {
    for (label, row) in [("ascii", ascii_row()), ("styled", styled_row())] {
        let serialized = serialize_row(&row).expect("serialize");
        let mut group = c.benchmark_group("row_codec");
        group.throughput(Throughput::Bytes(serialized.len() as u64));
        group.bench_with_input(BenchmarkId::new("encode", label), &row, |b, row| {
            b.iter(|| serialize_row(std::hint::black_box(row)).unwrap());
        });
        group.finish();
    }
}

fn bench_decode(c: &mut Criterion) {
    for (label, row) in [("ascii", ascii_row()), ("styled", styled_row())] {
        let data = serialize_row(&row).expect("serialize");
        let mut group = c.benchmark_group("row_codec");
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::new("decode", label), &data, |b, data| {
            b.iter(|| deserialize_row(std::hint::black_box(data)).unwrap());
        });
        group.finish();
    }
}

fn bench_round_trip(c: &mut Criterion) {
    for (label, row) in [("ascii", ascii_row()), ("styled", styled_row())] {
        let serialized = serialize_row(&row).expect("serialize");
        let mut group = c.benchmark_group("row_codec");
        group.throughput(Throughput::Bytes(serialized.len() as u64));
        group.bench_with_input(BenchmarkId::new("round_trip", label), &row, |b, row| {
            b.iter(|| {
                let bytes = serialize_row(std::hint::black_box(row)).unwrap();
                deserialize_row(std::hint::black_box(&bytes)).unwrap()
            });
        });
        group.finish();
    }
}

criterion_group!(benches, bench_encode, bench_decode, bench_round_trip);
criterion_main!(benches);
