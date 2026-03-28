use criterion::{Criterion, criterion_group, criterion_main};

fn vt_parser_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("vt_parser");
    group.bench_function("placeholder", |b| {
        b.iter(|| {
            // TODO: parse a representative byte stream through the VT parser
            std::hint::black_box(42)
        });
    });
    group.finish();
}

criterion_group!(benches, vt_parser_throughput);
criterion_main!(benches);
