use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use oakterm_terminal::grid::{Grid, ScreenSet};
use oakterm_terminal::handler::process_bytes;

/// 10 KB of printable ASCII ('A'-'z') mixed with newlines every ~72 chars.
fn make_plain_ascii() -> Vec<u8> {
    let mut buf = Vec::with_capacity(10 * 1024);
    let mut col = 0u16;
    while buf.len() < 10 * 1024 {
        if col >= 72 {
            buf.extend_from_slice(b"\r\n");
            col = 0;
        } else {
            // Printable ASCII range 0x41..=0x7A ('A' through 'z').
            #[allow(clippy::cast_possible_truncation)]
            let offset = (buf.len() % 58) as u8;
            let ch = b'A' + offset;
            buf.push(ch);
            col += 1;
        }
    }
    buf.truncate(10 * 1024);
    buf
}

/// Repeated SGR true-color sequences: `\x1b[38;2;R;G;Bm` with varying colors.
/// Stops at a complete sequence boundary to avoid measuring incomplete-sequence handling.
fn make_sgr_color() -> Vec<u8> {
    let mut buf = Vec::with_capacity(10 * 1024);
    let mut i = 0u8;
    while buf.len() < 10 * 1024 {
        let r = i;
        let g = i.wrapping_add(85);
        let b = i.wrapping_add(170);
        let seq = format!("\x1b[38;2;{r};{g};{b}mX");
        if buf.len() + seq.len() > 10 * 1024 {
            break;
        }
        buf.extend_from_slice(seq.as_bytes());
        i = i.wrapping_add(1);
    }
    buf
}

/// Realistic mix: text, SGR colors, cursor movement, newlines.
/// Stops at a complete line boundary to avoid truncating mid-sequence.
fn make_mixed_realistic() -> Vec<u8> {
    let mut buf = Vec::with_capacity(10 * 1024);
    let mut line = 0u32;
    loop {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"\x1b[1;32m");
        let name = format!("file_{line}.rs");
        chunk.extend_from_slice(name.as_bytes());
        chunk.extend_from_slice(b"\x1b[0m:\x1b[36m");
        let lineno = format!("{}", line * 10 + 1);
        chunk.extend_from_slice(lineno.as_bytes());
        chunk.extend_from_slice(b"\x1b[0m: \x1b[33mwarning\x1b[0m: unused variable `x`\r\n");
        chunk.extend_from_slice(b"\x1b[4G    ^\r\n");
        if buf.len() + chunk.len() > 10 * 1024 {
            break;
        }
        buf.extend_from_slice(&chunk);
        line += 1;
    }
    buf
}

/// Enough lines to cause many scroll-ups on a 24-row grid.
fn make_scrolling() -> Vec<u8> {
    let mut buf = Vec::with_capacity(10 * 1024);
    let mut line = 0u32;
    loop {
        let text = format!("Line {line}: The quick brown fox jumps over the lazy dog\r\n");
        if buf.len() + text.len() > 10 * 1024 {
            break;
        }
        buf.extend_from_slice(text.as_bytes());
        line += 1;
    }
    buf
}

/// Realistic shell session: each command is bracketed by OSC 133 marks
/// (A prompt, B input, C output, D exit) with an OSC 7 cwd update, so the
/// shell-integration scanner runs against the density it sees in practice.
fn make_shell_session() -> Vec<u8> {
    let mut buf = Vec::with_capacity(10 * 1024);
    let mut n = 0u32;
    loop {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"\x1b]7;file://host/home/user/project\x07");
        chunk.extend_from_slice(b"\x1b]133;A\x07user@host:~/project$ ");
        chunk.extend_from_slice(b"\x1b]133;B\x07");
        let cmd = format!("cargo build --bin app_{n}\r\n");
        chunk.extend_from_slice(cmd.as_bytes());
        chunk.extend_from_slice(b"\x1b]133;C\x07");
        chunk.extend_from_slice(b"   Compiling app v0.1.0\r\n    Finished in 1.2s\r\n");
        chunk.extend_from_slice(b"\x1b]133;D;0\x07");
        if buf.len() + chunk.len() > 10 * 1024 {
            break;
        }
        buf.extend_from_slice(&chunk);
        n += 1;
    }
    buf
}

/// Plain output through the `ScreenSet` path, which runs the OSC 7/133 scanner
/// over every byte. Guards against scanner overhead on the common case where
/// PTY output carries no shell-integration marks.
fn bench_screenset_plain_ascii(c: &mut Criterion) {
    let data = make_plain_ascii();
    let mut group = c.benchmark_group("shell_integration");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("screenset_plain_ascii", |b| {
        b.iter_batched(
            || ScreenSet::new(80, 24),
            |mut ss| ss.process_bytes(&data, &mut std::io::sink()),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// Shell-integration-heavy stream through the `ScreenSet` path, exercising the
/// scanner's OSC 7/133 decode and mark-attachment on realistic prompt density.
fn bench_screenset_shell_session(c: &mut Criterion) {
    let data = make_shell_session();
    let mut group = c.benchmark_group("shell_integration");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("screenset_shell_session", |b| {
        b.iter_batched(
            || ScreenSet::new(80, 24),
            |mut ss| ss.process_bytes(&data, &mut std::io::sink()),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_plain_ascii(c: &mut Criterion) {
    let data = make_plain_ascii();
    let mut group = c.benchmark_group("vt_parser");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("plain_ascii", |b| {
        b.iter_batched(
            || Grid::new(80, 24),
            |mut grid| process_bytes(&mut grid, &data, &mut std::io::sink()),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_sgr_color(c: &mut Criterion) {
    let data = make_sgr_color();
    let mut group = c.benchmark_group("vt_parser");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("sgr_color", |b| {
        b.iter_batched(
            || Grid::new(80, 24),
            |mut grid| process_bytes(&mut grid, &data, &mut std::io::sink()),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_mixed_realistic(c: &mut Criterion) {
    let data = make_mixed_realistic();
    let mut group = c.benchmark_group("vt_parser");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("mixed_realistic", |b| {
        b.iter_batched(
            || Grid::new(80, 24),
            |mut grid| process_bytes(&mut grid, &data, &mut std::io::sink()),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_scrolling(c: &mut Criterion) {
    let data = make_scrolling();
    let mut group = c.benchmark_group("vt_parser");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("scrolling", |b| {
        b.iter_batched(
            || Grid::new(80, 24),
            |mut grid| process_bytes(&mut grid, &data, &mut std::io::sink()),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_plain_ascii,
    bench_sgr_color,
    bench_mixed_realistic,
    bench_scrolling,
    bench_screenset_plain_ascii,
    bench_screenset_shell_session
);
criterion_main!(benches);
