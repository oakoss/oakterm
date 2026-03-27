---
spec: '0004'
title: Scroll Buffer & Archive
status: draft
date: 2026-03-26
adrs: ['0006']
tags: [core]
---

# 0004. Scroll Buffer & Archive

## Overview

Defines the two-tier scrollback system: a hot ring buffer in memory for recent lines and a cold disk archive for older lines. Rows age from the visible grid (Spec-0003) into the hot buffer, then into the cold archive. The transition is invisible to the user. This spec also defines alternate screen scrollback capture, configuration, and memory management. Implements ADR-0006.

## Contract

### Hot Ring Buffer

A fixed-capacity circular buffer holding recent scrollback rows in memory.

```rust
struct HotBuffer {
    /// Backing memory allocated via mmap. Freed with munmap on pruning.
    /// Do NOT use Vec or the system allocator for this buffer.
    backing: MmapRegion,

    /// Ring buffer of rows. `[Row]` denotes a contiguous sequence over the
    /// mmap'd region; the actual indirection is implementation-defined.
    rows: [Row],

    /// Index of the oldest row in the ring (rotation offset).
    /// Scrolling adjusts this offset without moving data.
    zero: usize,

    /// Number of active rows (may be less than capacity during startup).
    len: usize,

    /// Maximum capacity in bytes. Corresponds to `scrollback-limit` config.
    max_bytes: usize,

    /// Current memory usage in bytes.
    used_bytes: usize,
}
```

**Zero-copy viewport shift:** Scrolling the viewport changes an offset into the ring buffer. No data is copied or moved. Index access computes `(zero + logical_index) % capacity`. This matches Alacritty's proven `Storage<T>` model.

**Memory backing:** The ring buffer's storage is allocated with `mmap` (or platform equivalent) and freed with `munmap`. This guarantees memory is returned to the OS on pruning, avoiding the arena-pooling leak class that caused Ghostty's 71 GB memory leak. Do not use `Vec`, `Box`, or the global allocator for the ring buffer's backing storage.

**Pruning:** When `used_bytes` exceeds `max_bytes`, the oldest rows are removed from the ring buffer. If the disk archive is enabled, pruned rows are serialized and appended to the archive before removal. If the archive is disabled, pruned rows are discarded. After pruning, the freed mmap region is returned to the OS via `munmap` (or `madvise(MADV_FREE)` for partial page release).

### Cold Disk Archive

Compressed, encrypted, seekable storage for old scrollback rows.

```rust
struct ColdArchive {
    /// Path to the archive file.
    path: PathBuf,

    /// Seekable zstd writer/reader.
    file: File,

    /// Seek table for random access (loaded from file footer).
    seek_table: SeekTable,

    /// Encryption key (ephemeral, per-session, never persisted).
    key: AeadKey,

    /// Monotonic nonce counter for AES-256-GCM.
    nonce_counter: u64,

    /// Current archive size on disk.
    disk_bytes: u64,

    /// Maximum archive size. Corresponds to `scrollback-archive-limit` config.
    max_disk_bytes: u64,

    /// Total row count in the archive.
    row_count: u64,
}
```

**Compression:** Seekable zstd format with 64 KB uncompressed frame size. Each frame is independently decompressible. A seek table at the end of the file maps frame indices to byte offsets, enabling random access without scanning from the beginning. Compression level: 3 (fast compression at ~200 MB/s, decompression at ~1,500 MB/s). Expected compression ratio for terminal output: 5:1 to 10:1.

**Encryption:** AES-256-GCM per frame.

- Key: 32-byte random key generated at session start via a cryptographic RNG.
- Nonce: 12 bytes composed of the monotonic `nonce_counter` (8 bytes, little-endian) padded with 4 zero bytes. Counter increments per frame. The archive is append-only, so nonce reuse is impossible.
- Authentication tag: 16 bytes appended to each encrypted frame.
- The key exists only in process memory. It is never written to disk. When the daemon exits, the key is lost and the archive is unreadable.

**Frame layout on disk (per segment file):**

```text
[encrypted(zstd-compressed row data)] [16-byte GCM tag]
... repeated for each frame in the segment ...
[seek table]
```

Each frame is: compress rows with zstd, then encrypt the compressed bytes with AES-256-GCM. Standard zstd tools cannot read encrypted frames; the seek table is a custom binary structure, not the standard seekable zstd skippable frame.

**Seek table entry:**

```rust
struct SeekTableEntry {
    compressed_offset: u64,  // Byte offset of this frame in the segment file
    compressed_size: u32,    // Size of encrypted+tagged frame on disk
    decompressed_size: u32,  // Size of plaintext compressed data
    first_row_index: u64,    // Cumulative row count at frame start
    row_count: u32,          // Number of rows in this frame
}
```

**Random access:** To read row N from the archive:

1. Find the segment containing row N by checking each segment's row range (first segment's `first_row_index` through last entry's `first_row_index + row_count`).
2. Within the segment, binary search the seek table for the frame where `first_row_index <= N < first_row_index + row_count`.
3. Read the frame from disk at `compressed_offset` (memory-mapped file, so this is a page fault in the warm case, not a read syscall).
4. Decrypt with AES-256-GCM using the frame's nonce (derived from nonce counter).
5. Decompress the zstd data.
6. Deserialize the rows within the frame.
7. Return the requested row(s).

**Latency:** Decompression of a 64 KB frame takes ~43 microseconds. Decryption adds ~20 microseconds. Total: ~63 microseconds per frame read (warm cache). Cold page faults from disk add 100-500 microseconds.

### Row Serialization

When rows move from the hot buffer to the cold archive, full cell data is preserved:

- Text content (codepoints, grapheme clusters)
- Foreground and background colors
- Style attributes (bold, italic, underline, etc.)
- Hyperlinks
- Wide character state
- Soft-wrap flags
- Semantic marks (OSC 133)

**Images:** Kitty graphics image placements attached to archived rows are replaced with placeholder references. Image pixel data is NOT stored in the archive. A separate image cache (implementation-defined) may retain pixel data independently. This prevents archive bloat from large images.

**Encoding:** Row serialization format is implementation-defined (serde + bincode, custom binary, etc.). The spec requires that archived rows are byte-identical to hot buffer rows when deserialized — the archive is a transparent persistence layer, not a lossy transformation.

### Alternate Screen Capture

Per ADR-0006, lines that scroll off the top of the alternate screen viewport are captured to the primary scrollback.

**Mechanism:**

1. When the alternate grid (Spec-0003 ScreenSet) scrolls up and a row exits the top of the scroll region, the daemon checks the `save-alternate-scrollback` config option.
2. If enabled (default: true), the row is appended to the primary screen's hot ring buffer as if it were a normal scrollback line.
3. If the hot buffer is full, normal pruning applies (row may be archived to disk).
4. If disabled, the row is discarded.

**Row metadata:** Alternate-screen-captured rows are standard `Row` structs with no special marker. They are indistinguishable from primary screen scrollback rows in the buffer. The semantic content comes from whatever the alternate screen application wrote.

### Configuration

Four user-facing options (from ADR-0006):

```lua
-- Hot buffer size per surface. Byte-based. Default: 50 MB.
config.scrollback_limit = "50MB"

-- Enable disk-backed cold archive. Default: true.
config.scrollback_archive = true

-- Per-surface disk archive limit. Default: 1 GB.
config.scrollback_archive_limit = "1GB"

-- Capture alternate screen lines to primary scrollback. Default: true.
config.save_alternate_scrollback = true
```

**Flat config equivalents (kebab-case):**

- `scrollback-limit` = `50MB`
- `scrollback-archive` = `true`
- `scrollback-archive-limit` = `1GB`
- `save-alternate-scrollback` = `true`

**Size parsing:** Values like `50MB`, `1GB` are parsed as byte counts. Accepted suffixes: `KB` (1024), `MB` (1024²), `GB` (1024³). No suffix = raw bytes.

### Wire Protocol Integration

The hot buffer and cold archive are accessed by GUI clients through Spec-0001 messages:

- **`GetScrollback { pane_id, start_row, count }`** — client requests scrollback rows. The daemon reads from the hot buffer first, falling back to the cold archive for older rows. The response (`ScrollbackData`) may include `has_more=1` if the request exceeds the 16 MiB frame limit.
- **`GetRenderUpdate { since_seqno }`** — covers the visible grid only. Scrollback rows are not transmitted via `RenderUpdate`. The client fetches them on demand via `GetScrollback`.

The daemon translates between the logical scrollback row index (negative values in Spec-0003's Selection model) and the physical storage location (hot buffer offset or archive frame index).

## Behavior

### Row Lifecycle

```text
1. VT handler writes to visible grid (Spec-0003)
2. Scroll event pushes top row out of the visible grid
3. Row enters the hot ring buffer (most recent scrollback)
4. Hot buffer exceeds max_bytes → oldest rows pruned
5. If archive enabled: pruned rows serialized → compressed → encrypted → written to archive
6. If archive full (exceeds max_disk_bytes): oldest frames deleted from archive head
7. Archive frames older than the session are cleaned up on daemon exit
```

### Hot Buffer Full

When `used_bytes > max_bytes`:

1. Calculate how many rows to prune to bring usage below `max_bytes * 0.9` (prune 10% headroom to avoid pruning on every scroll).
2. If archive is enabled, serialize the pruned rows into archive frames.
3. Release the mmap region covering the pruned rows via `munmap` or `madvise(MADV_FREE)`.
4. Advance the ring buffer's `zero` offset past the pruned rows.

### Archive Full

The archive is stored as a directory of numbered segment files, not a single file. Each segment contains a fixed number of frames (default: 256 frames per segment, ~16 MB uncompressed). This enables pruning by deleting entire segment files without rewriting.

When `disk_bytes > max_disk_bytes`:

1. Delete the oldest segment file(s) until `disk_bytes` is below `max_disk_bytes * 0.9` (10% headroom).
2. Update the in-memory segment index (no seek table rebuild needed — each segment has its own seek table).
3. Log a debug message noting scrollback was trimmed.

### Disk Space Protection

Before writing a new frame to the archive, check available disk space:

- If free space is below 1 GB or 5% of the filesystem (whichever is larger), stop archiving. New rows that would be archived are discarded instead.
- When disk space recovers above the threshold, archiving resumes.
- This check runs per-frame-write, not per-row, to minimize syscall overhead.

### Daemon Exit

On clean shutdown:

1. Delete all archive files for the current session.
2. The ephemeral encryption key is lost when the process exits, rendering any remaining files unreadable.

On crash:

1. Archive files remain on disk but are encrypted with a key that no longer exists.
2. On next daemon start, detect and delete orphaned archive files (match by PID or session ID in the filename).

### Archive File Location

- **Linux:** `$XDG_RUNTIME_DIR/oakterm/<session-id>/scrollback-<pane-id>/segment-NNNN.bin`
- **macOS:** `$TMPDIR/oakterm-<uid>/<session-id>/scrollback-<pane-id>/segment-NNNN.bin`
- **Windows:** `%LOCALAPPDATA%\oakterm\<session-id>\scrollback-<pane-id>\segment-NNNN.bin`

The parent directory is created with `0700` permissions. `$XDG_RUNTIME_DIR` is tmpfs on most Linux distributions (RAM-backed, cleaned on reboot). `$TMPDIR` on macOS is per-user and cleaned on reboot.

## Constraints

- **Hot buffer default:** 50 MB per surface. At 24 bytes/cell × 200 columns = ~4.8 KB/row, this holds ~10,400 rows.
- **Hot buffer maximum:** 512 MB per surface (configurable). Higher values are accepted but logged with a warning.
- **Archive default:** 1 GB per surface. At 5:1 compression = ~5 GB raw = ~1M rows at 200 columns.
- **Archive maximum:** No hard cap. Disk space protection prevents filesystem exhaustion.
- **Frame read latency:** <100 μs warm (decompression + decryption), <600 μs cold (with page fault).
- **Pruning latency:** Pruning + archiving a batch of rows should complete within one frame time (16.6 ms at 60fps). With zstd level 3 at 200 MB/s compress and ring at 3.3 GB/s encrypt, a 10% prune of 50 MB = 5 MB takes ~25 ms compress + ~1.5 ms encrypt. This may exceed one frame. Pruning should be performed on a background thread to avoid blocking the VT handler.
- **Memory return:** All freed ring buffer memory must be returned to the OS within one second of pruning. Verified by OS-level memory tracking (macOS `footprint`, Linux `/proc/self/statm`).

## References

- [ADR 0006: Scroll Buffer Architecture](../adrs/0006-scroll-buffer-architecture.md)
- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — GetScrollback / ScrollbackData messages
- [Spec 0003: Screen Buffer](0003-screen-buffer.md) — Row type, ScreenSet alternate screen
- [15-memory-management.md](../ideas/15-memory-management.md)
- [Ghostty memory leak fix](https://mitchellh.com/writing/ghostty-memory-leak-fix)
- [VTE scrollback encryption (LWN)](https://lwn.net/Articles/752924/)
- [zstd seekable format](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md)
