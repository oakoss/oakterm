use criterion::{Criterion, criterion_group, criterion_main};

fn screen_buffer_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("screen_buffer");
    group.bench_function("placeholder", |b| {
        b.iter(|| {
            // TODO: measure screen buffer update latency
            std::hint::black_box(42)
        });
    });
    group.finish();
}

criterion_group!(benches, screen_buffer_update);
criterion_main!(benches);
