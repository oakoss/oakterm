use criterion::{Criterion, criterion_group, criterion_main};

fn frame_render_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_render");
    group.bench_function("placeholder", |b| {
        b.iter(|| {
            // TODO: measure time to GPU submit
            std::hint::black_box(42)
        });
    });
    group.finish();
}

criterion_group!(benches, frame_render_time);
criterion_main!(benches);
