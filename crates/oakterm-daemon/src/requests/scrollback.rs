//! Scrollback family: `GetScrollback` (0x73) and the Spec-0004 read plan.

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, PaneState, lock_live_pane};
use crate::wire::row_to_wire;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{ErrorCode, GetScrollback, MSG_SCROLLBACK_DATA, ScrollbackData};
use oakterm_protocol::render::DirtyRow;
use oakterm_terminal::grid::cell::Rgb;
use oakterm_terminal::grid::row::Row;
use oakterm_terminal::scroll::archive_manager::{ArchiveManager, ArchiveReader};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, warn};

/// Per-request row cap. The hot buffer used to bound responses
/// naturally; with the archive a client-controlled count could otherwise
/// drive unbounded reads and allocation.
pub(super) const MAX_SCROLLBACK_ROWS_PER_REQUEST: u32 = 4096;

/// Where a pane's absolute row indices live (Spec-0004 tiers). `[0,
/// archived)` is on disk, `[archived, hot_first)` was pruned while no
/// archive was attached and is gone for good, `[hot_first, pushed)` is the
/// hot buffer, and `[pushed, pushed + grid rows)` is the live grid.
///
/// Anchored on the hot buffer's monotonic push counter rather than
/// `archived + len`, so a row keeps its index for the life of the pane
/// whether or not an archive ever captured it. Pins depend on that:
/// deriving the origin from the archive would let it drift on any pane
/// without one.
pub(super) struct HistoryLayout {
    pub(super) archived: u64,
    pub(super) hot_first: u64,
    pub(super) pushed: u64,
}

impl HistoryLayout {
    pub(super) fn of(pane: &PaneState) -> Self {
        let buf = pane.screens.scrollback();
        let hot_first = buf.first_index();
        let archived = pane
            .screens
            .archive()
            .map_or(0, ArchiveManager::total_rows_received);
        Self {
            // Clamped so `archived <= hot_first <= pushed` holds by
            // construction; every consumer indexes off that ordering.
            archived: archived.min(hot_first),
            hot_first,
            pushed: buf.pushed(),
        }
    }
}

pub(super) async fn get_scrollback(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(req) = GetScrollback::decode(&frame.payload) else {
        warn!(conn_id, "malformed GetScrollback payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed GetScrollback",
        );
    };
    let Some(snapshot) = snapshot_under_lock(conn_id, &req, panes).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    let ScrollbackSnapshot {
        plan,
        access,
        palette,
        cols,
        tail,
    } = snapshot;

    let mut rows: Vec<DirtyRow> = Vec::with_capacity(plan.archive_count + tail.len());
    if plan.archive_count > 0 {
        let found = match read_archive_rows(
            conn_id,
            req.pane_id,
            access,
            plan.archive_start,
            plan.archive_count,
        )
        .await
        {
            Ok(found) => found,
            Err(message) => {
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::InternalError,
                    message,
                );
            }
        };
        let (aligned, _) = align_archive_rows(found, plan.archive_start, plan.archive_count, cols);
        rows.extend(aligned.iter().map(|row| row_to_wire(row, 0, &palette)));
    }
    rows.extend(tail);

    let data = ScrollbackData {
        pane_id: req.pane_id,
        start_row: plan.served_start_row,
        has_more: plan.has_more,
        total_rows: plan.total_rows,
        rows,
    };

    match data.encode() {
        Ok(payload) => match Frame::new(MSG_SCROLLBACK_DATA, frame.serial, payload) {
            Ok(f) => RequestResult::Response(f),
            Err(e) => {
                error!(conn_id, error = %e, "failed to create ScrollbackData frame");
                make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::InternalError,
                    "ScrollbackData frame error",
                )
            }
        },
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode ScrollbackData");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "ScrollbackData encode error",
            )
        }
    }
}

/// Everything a response needs from behind the pane lock, so the archive
/// read can run without it. The hot rows are converted here rather than
/// after that read: a prune while the lock is free would shift them out
/// from under `plan.hot_start`.
///
/// Nothing here is re-read afterwards, so a pane closing mid-read still
/// yields a consistent response — deliberately unlike `resolve_yank`,
/// which re-locks to assemble and so fails a closed pane. Refusing would
/// only lose history the client can no longer obtain.
struct ScrollbackSnapshot {
    plan: ScrollbackReadPlan,
    access: ArchiveAccess,
    palette: [Rgb; 256],
    cols: usize,
    /// Blanks for the pruned-without-an-archive gap, then hot buffer rows
    /// — everything after the archived portion of the window.
    tail: Vec<DirtyRow>,
}

async fn snapshot_under_lock(
    conn_id: u64,
    req: &GetScrollback,
    panes: &Arc<Mutex<PaneManager>>,
) -> Option<ScrollbackSnapshot> {
    let pane = lock_live_pane(panes, req.pane_id).await?;
    let layout = HistoryLayout::of(&pane);
    let plan = plan_scrollback_read(
        &layout,
        pane.copy_mode_base(conn_id),
        req.start_row,
        req.count,
    );
    let grid = pane.screens.active_grid();
    let palette = grid.palette;
    let cols = usize::from(grid.cols);
    Some(ScrollbackSnapshot {
        tail: build_tail_rows(conn_id, req.pane_id, &pane, &plan, cols, &palette),
        access: ArchiveAccess::of(&pane),
        plan,
        palette,
        cols,
    })
}

fn build_tail_rows(
    conn_id: u64,
    pane_id: u32,
    pane: &PaneState,
    plan: &ScrollbackReadPlan,
    cols: usize,
    palette: &[Rgb; 256],
) -> Vec<DirtyRow> {
    let buf = pane.screens.scrollback();
    let mut rows: Vec<DirtyRow> = Vec::with_capacity(plan.gap_count + plan.hot_count);

    if plan.gap_count > 0 {
        warn!(
            conn_id,
            pane_id,
            gap = plan.gap_count,
            "scrollback rows pruned before an archive existed; blank-filled"
        );
        rows.extend(
            std::iter::repeat_with(|| row_to_wire(&Row::new(cols), 0, palette))
                .take(plan.gap_count),
        );
    }

    for i in plan.hot_start..plan.hot_start + plan.hot_count {
        // The plan is clamped to buf.len() under this lock, so a
        // miss is a planning bug — blank-fill to keep the response
        // positionally aligned rather than silently shrinking it.
        if let Some(row) = buf.get(i) {
            rows.push(row_to_wire(row, 0, palette));
        } else {
            warn!(
                conn_id,
                index = i,
                "hot scrollback row missing; blank-filled"
            );
            rows.push(row_to_wire(&Row::new(cols), 0, palette));
        }
    }
    rows
}

/// Read archived rows off the runtime, holding no pane lock: the query
/// blocks until the writer answers or its timeout expires, which must
/// stall neither the pane nor a tokio worker (TREK-197).
///
/// Every failure — a dead or wedged writer, a disk error, a panicking
/// read — surfaces as an error frame, never blank rows: a blank means a
/// permanent gap, but a transient failure must stay retryable.
pub(super) async fn read_archive_rows(
    conn_id: u64,
    pane_id: u32,
    access: ArchiveAccess,
    start: u64,
    count: usize,
) -> Result<Vec<(u64, Row)>, &'static str> {
    let reader = match access {
        ArchiveAccess::Ready(reader) => reader,
        ArchiveAccess::WriterGone => {
            warn!(
                conn_id,
                pane_id, start, count, "archive writer shut down; window unreadable"
            );
            return Err("archive unavailable");
        }
        ArchiveAccess::Absent => {
            // The plan only asks for archived rows when `archived > 0`,
            // which a pane without an archive can never reach.
            error!(
                conn_id,
                pane_id, start, count, "archived rows planned on a pane with no archive"
            );
            return Err("archive unavailable");
        }
    };
    let outcome = tokio::task::spawn_blocking(move || reader.read_range(start, count)).await;
    map_read_outcome(conn_id, pane_id, start, count, outcome)
}

/// How a pane's archive can be reached for a read. The two unreadable
/// states are kept apart because they mean opposite things: a writer that
/// shut down is a routine post-teardown read, while a plan that wants
/// archived rows from a pane holding no archive is a broken invariant.
#[derive(Clone)]
pub(super) enum ArchiveAccess {
    Ready(ArchiveReader),
    WriterGone,
    Absent,
}

impl ArchiveAccess {
    pub(super) fn of(pane: &PaneState) -> Self {
        match pane.screens.archive() {
            None => Self::Absent,
            Some(archive) => archive.reader().map_or(Self::WriterGone, Self::Ready),
        }
    }

    pub(super) fn reader(&self) -> Option<&ArchiveReader> {
        match self {
            Self::Ready(reader) => Some(reader),
            Self::WriterGone | Self::Absent => None,
        }
    }
}

type ReadOutcome = Result<std::io::Result<Vec<(u64, Row)>>, tokio::task::JoinError>;

fn map_read_outcome(
    conn_id: u64,
    pane_id: u32,
    start: u64,
    count: usize,
    outcome: ReadOutcome,
) -> Result<Vec<(u64, Row)>, &'static str> {
    match outcome {
        Ok(Ok(rows)) => Ok(rows),
        Ok(Err(e)) => {
            warn!(
                conn_id,
                pane_id,
                start,
                count,
                error = %e,
                "archive read failed"
            );
            Err("archive read failed")
        }
        Err(e) => {
            error!(
                conn_id,
                pane_id,
                start,
                count,
                error = %e,
                "archive read task failed"
            );
            Err("archive read failed")
        }
    }
}

/// How a `GetScrollback` request maps onto the combined history:
/// absolute index 0 is the oldest row ever pruned to the archive, and
/// the hot buffer follows the archived range (Spec-0004 read path).
#[derive(Debug)]
struct ScrollbackReadPlan {
    archive_start: u64,
    archive_count: usize,
    /// Origin-relative start actually served. Rows carry no index of
    /// their own, so a client keying off its request would mis-file
    /// every row of a front-clamped window.
    served_start_row: i64,
    /// Blank rows standing in for the pruned-without-an-archive range,
    /// emitted between the archive and hot portions so the response stays
    /// positionally aligned with the requested window.
    gap_count: usize,
    hot_start: usize,
    hot_count: usize,
    has_more: bool,
    total_rows: u32,
}

/// `base` is the origin `start_row` counts from: the live present for a
/// normal client, or the pinned viewport top for one in copy mode
/// (ADR-0012), which keeps its row indices stable as output arrives.
fn plan_scrollback_read(
    layout: &HistoryLayout,
    base: u64,
    start_row: i64,
    count: u32,
) -> ScrollbackReadPlan {
    let count = count.min(MAX_SCROLLBACK_ROWS_PER_REQUEST);
    let total = layout.pushed;
    // start_row counts back from `base` (Spec-0003 selection space); rows
    // are only ever served out of history, so the window clamps to `total`.
    let start = base.saturating_add_signed(start_row).min(total);
    let end = start.saturating_add(u64::from(count)).min(total);

    let archive_end = end.min(layout.archived);
    let archive_start = start.min(archive_end);
    let gap_count = end
        .min(layout.hot_first)
        .saturating_sub(start.max(layout.archived));
    let hot_from = start.max(layout.hot_first);
    let hot_count = end.saturating_sub(hot_from);

    ScrollbackReadPlan {
        archive_start,
        // Both operands count rows of one pane, and the coordinate space
        // is i64 on the wire regardless, so a pane deep enough to
        // saturate could not have named the row in its request either.
        served_start_row: i64::try_from(i128::from(start) - i128::from(base)).unwrap_or(i64::MIN),
        archive_count: usize::try_from(archive_end - archive_start)
            .expect("bounded by request cap"),
        gap_count: usize::try_from(gap_count).expect("bounded by request cap"),
        hot_start: usize::try_from(hot_from - layout.hot_first).expect("bounded by hot_len"),
        hot_count: usize::try_from(hot_count).expect("bounded by request cap"),
        has_more: start > 0,
        total_rows: u32::try_from(total).unwrap_or(u32::MAX),
    }
}

/// Positionally align index-tagged archive rows over
/// `[start, start + count)`, blank-filling indices the archive has no
/// row for (gaps from the Spec-0004 overload policy) so the client can
/// consume the response by position.
///
/// Returns the rows alongside how many the archive actually supplied —
/// the difference is blank fill, which the client cannot tell from real
/// blank output, so callers that report on the read need the count.
pub(super) fn align_archive_rows(
    found: Vec<(u64, Row)>,
    start: u64,
    count: usize,
    cols: usize,
) -> (Vec<Row>, usize) {
    let mut found = found.into_iter().peekable();
    let mut out_of_contract: u64 = 0;
    let mut supplied = 0usize;
    let rows: Vec<Row> = (start..start + count as u64)
        .map(|idx| {
            // Entries behind the cursor (index regression, duplicates)
            // violate the sorted-unique contract; absorbing one silently
            // would blank every later row in the window.
            while found.peek().is_some_and(|(i, _)| *i < idx) {
                found.next();
                out_of_contract += 1;
            }
            match found.peek() {
                Some((i, _)) if *i == idx => {
                    supplied += 1;
                    found.next().expect("peeked").1
                }
                _ => Row::new(cols),
            }
        })
        .collect();
    out_of_contract += found.count() as u64;
    if out_of_contract > 0 {
        warn!(
            out_of_contract,
            start, count, "archive rows outside the requested window; indexing bug suspected"
        );
    }
    if supplied < count {
        // Expected when the archive dropped rows under load, and the only
        // signal that the blanks below are missing data rather than blank
        // terminal output.
        warn!(
            missing = count - supplied,
            start, count, "archive supplied fewer rows than the window; blank-filled"
        );
    }
    (rows, supplied)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use oakterm_protocol::message::ErrorMessage;
    use oakterm_terminal::scroll::archive_manager::ReadStall;
    use std::time::Duration;

    const COLS: usize = 80;

    /// A pane whose whole history is still reachable: everything pruned
    /// out of the hot buffer was captured by the archive.
    fn layout(archived_rows: u64, hot_len: usize) -> HistoryLayout {
        HistoryLayout {
            archived: archived_rows,
            hot_first: archived_rows,
            pushed: archived_rows + hot_len as u64,
        }
    }

    /// A client not in copy mode: the origin is the live present.
    fn plan_unpinned(
        archived_rows: u64,
        hot_len: usize,
        start_row: i64,
        count: u32,
    ) -> ScrollbackReadPlan {
        let layout = layout(archived_rows, hot_len);
        let base = layout.pushed;
        plan_scrollback_read(&layout, base, start_row, count)
    }

    /// A pinned client keeps the origin it entered copy mode with, so the
    /// same `start_row` names the same rows after new output arrives.
    #[test]
    fn scrollback_plan_resolves_against_a_pinned_base() {
        let at_entry = plan_unpinned(0, 100, -10, 5);

        // Twenty rows later, the live present has moved but the pin has not.
        let pinned = plan_scrollback_read(&layout(0, 120), 100, -10, 5);
        assert_eq!(pinned.hot_start, at_entry.hot_start);
        assert_eq!(pinned.hot_count, 5);

        // The same request without the pin slides forward with the output.
        let unpinned = plan_unpinned(0, 120, -10, 5);
        assert_eq!(unpinned.hot_start, at_entry.hot_start + 20);
    }

    /// Rows that were on screen when the pin was taken (`start_row >= 0`)
    /// become readable from scrollback once output pushes them off.
    #[test]
    fn scrollback_plan_serves_rows_that_scrolled_past_a_pin() {
        let plan = plan_scrollback_read(&layout(0, 110), 100, 2, 3);
        assert_eq!(plan.hot_start, 102);
        assert_eq!(plan.hot_count, 3);
    }

    /// An archiveless pane prunes rows into nothing. Their indices stay
    /// claimed, so later rows keep their absolute positions and the
    /// response blank-fills the hole instead of shifting.
    #[test]
    fn scrollback_plan_blank_fills_rows_pruned_without_an_archive() {
        // 100 rows pushed, oldest 60 pruned with no archive attached.
        let layout = HistoryLayout {
            archived: 0,
            hot_first: 60,
            pushed: 100,
        };

        let plan = plan_scrollback_read(&layout, 100, -70, 20);
        assert_eq!(plan.archive_count, 0);
        assert_eq!(plan.gap_count, 20, "rows 30..50 are gone");
        assert_eq!(plan.hot_count, 0, "the window ends before row 60");

        // A window straddling the gap boundary splits into both parts.
        let plan = plan_scrollback_read(&layout, 100, -50, 20);
        assert_eq!(plan.gap_count, 10);
        assert_eq!(plan.hot_start, 0);
        assert_eq!(plan.hot_count, 10);
        assert_eq!(plan.total_rows, 100, "gap rows still count as history");
    }

    /// Archive attached partway through: on-disk rows, then a gap for what
    /// was pruned before it existed, then the hot buffer.
    #[test]
    fn scrollback_plan_spans_archive_gap_and_hot() {
        let layout = HistoryLayout {
            archived: 20,
            hot_first: 50,
            pushed: 100,
        };

        let plan = plan_scrollback_read(&layout, 100, -90, 60);
        assert_eq!(plan.archive_start, 10);
        assert_eq!(plan.archive_count, 10, "rows 10..20 are on disk");
        assert_eq!(plan.gap_count, 30, "rows 20..50 were dropped");
        assert_eq!(plan.hot_start, 0);
        assert_eq!(plan.hot_count, 20, "rows 50..70 are hot");
    }

    #[test]
    fn scrollback_plan_hot_only_matches_legacy_semantics() {
        let plan = plan_unpinned(0, 100, -50, 10);
        assert_eq!(plan.archive_count, 0);
        assert_eq!(plan.hot_start, 50);
        assert_eq!(plan.hot_count, 10);
        assert!(plan.has_more);
        assert_eq!(plan.total_rows, 100);
    }

    #[test]
    fn scrollback_plan_spans_archive_and_hot() {
        let plan = plan_unpinned(100, 50, -120, 100);
        assert_eq!(plan.archive_start, 30);
        assert_eq!(plan.archive_count, 70);
        assert_eq!(plan.hot_start, 0);
        assert_eq!(plan.hot_count, 30);
        assert!(plan.has_more);
        assert_eq!(plan.total_rows, 150);
    }

    #[test]
    fn scrollback_plan_clamps_before_history_start() {
        let plan = plan_unpinned(100, 50, -i64::MAX, 10);
        assert_eq!(plan.archive_start, 0);
        assert_eq!(plan.archive_count, 10);
        assert_eq!(plan.hot_count, 0);
        assert!(!plan.has_more, "nothing older than absolute row 0");
    }

    /// A window that fits inside history is served exactly where it was
    /// asked for — the case that must stay indistinguishable from the
    /// echo this field replaced.
    #[test]
    fn an_unclamped_window_serves_the_requested_start() {
        assert_eq!(plan_unpinned(0, 100, -50, 10).served_start_row, -50);
        assert_eq!(
            plan_scrollback_read(&layout(0, 120), 100, -10, 5).served_start_row,
            -10
        );
    }

    /// The front clamp is what makes the served start load-bearing: rows
    /// arrive positionally, so a client keying off `-70` here would file
    /// absolute rows 0..10 as if they were rows 30..40.
    #[test]
    fn a_front_clamped_window_serves_a_later_start_than_requested() {
        let plan = plan_unpinned(100, 50, -70, 10);
        assert_eq!(plan.served_start_row, -70, "150 rows of history absorbs it");

        // Reaching past the oldest row: the window starts at absolute 0,
        // which is 150 rows before the live present.
        let clamped = plan_unpinned(100, 50, -400, 10);
        assert_eq!(clamped.archive_start, 0);
        assert_eq!(clamped.served_start_row, -150);
        assert_ne!(
            clamped.served_start_row, -400,
            "echoing the request is the mis-keying bug"
        );
    }

    /// Under a pin the served start stays in the pin's coordinate space,
    /// so it composes with the client's cache keys rather than the live
    /// present's.
    #[test]
    fn a_pinned_front_clamped_window_serves_in_pin_coordinates() {
        // Pinned at absolute 40 with only 40 rows of history behind it.
        let plan = plan_scrollback_read(&layout(0, 60), 40, -100, 10);
        assert_eq!(plan.hot_start, 0, "clamped onto the oldest row");
        assert_eq!(plan.served_start_row, -40);
    }

    #[test]
    fn scrollback_plan_count_clamps_to_present() {
        let plan = plan_unpinned(0, 20, -5, 100);
        assert_eq!(plan.hot_start, 15);
        assert_eq!(plan.hot_count, 5);
        assert!(plan.has_more);
    }

    #[test]
    fn scrollback_plan_archive_only_request() {
        let plan = plan_unpinned(100, 50, -150, 40);
        assert_eq!(plan.archive_start, 0);
        assert_eq!(plan.archive_count, 40);
        assert_eq!(plan.hot_count, 0);
        assert!(!plan.has_more);
    }

    #[test]
    fn align_archive_rows_blank_fills_gaps() {
        let mut row_a = Row::new(4);
        row_a.cells[0].codepoint = 'A';
        let mut row_b = Row::new(4);
        row_b.cells[0].codepoint = 'B';

        let aligned = align_archive_rows(vec![(12, row_a), (14, row_b)], 10, 6, 4);

        let heads: Vec<char> = aligned.0.iter().map(|r| r.cells[0].codepoint).collect();
        assert_eq!(heads, vec!['\0', '\0', 'A', '\0', 'B', '\0']);
    }

    #[test]
    fn align_archive_rows_skips_out_of_contract_indices() {
        let mut stale = Row::new(4);
        stale.cells[0].codepoint = 'S';
        let mut row_a = Row::new(4);
        row_a.cells[0].codepoint = 'A';

        // An entry below the window must not wedge alignment of later rows.
        let aligned = align_archive_rows(vec![(9, stale), (12, row_a)], 10, 4, 4);

        let heads: Vec<char> = aligned.0.iter().map(|r| r.cells[0].codepoint).collect();
        assert_eq!(heads, vec!['\0', '\0', 'A', '\0']);
    }

    #[test]
    fn scrollback_plan_clamps_count_to_request_cap() {
        let plan = plan_unpinned(1_000_000, 100, -500_000, u32::MAX);
        assert_eq!(
            plan.archive_count + plan.hot_count,
            MAX_SCROLLBACK_ROWS_PER_REQUEST as usize
        );
    }

    // --- Handler ---

    fn row_from(text: &str) -> Row {
        let mut row = Row::new(COLS);
        for (cell, ch) in row.cells.iter_mut().zip(text.chars()) {
            cell.codepoint = ch;
        }
        row
    }

    fn scrollback_frame(pane_id: u32, start_row: i64, count: u32) -> Frame {
        Frame::new(
            oakterm_protocol::message::MSG_GET_SCROLLBACK,
            9,
            GetScrollback {
                pane_id,
                start_row,
                count,
            }
            .encode(),
        )
        .expect("frame")
    }

    async fn pane_with_scrollback(lines: &[&str]) -> (Arc<Mutex<PaneManager>>, u32) {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());
        let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
        for line in lines {
            pane.screens.push_to_scrollback(row_from(line));
        }
        drop(pane);
        (panes, pane_id)
    }

    /// Drives the real prune-into-archive path: only rows that pass
    /// through the hot buffer are counted in the absolute index space.
    async fn pane_with_archived_prefix(
        dir: &std::path::Path,
        lines: &[&str],
        keep: usize,
    ) -> (Arc<Mutex<PaneManager>>, u32) {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            let archive =
                ArchiveManager::new(dir.join("archive"), 1 << 20).expect("create archive");
            pane.screens.set_archive(archive);

            pane.screens.push_to_scrollback(row_from(lines[0]));
            let row_bytes = pane.screens.scrollback().used_bytes();
            let pruned = pane
                .screens
                .scrollback_mut()
                .set_max_bytes(row_bytes * (keep + 1));
            assert!(pruned.is_empty(), "resize must not drop rows unarchived");
            for line in &lines[1..] {
                pane.screens.push_to_scrollback(row_from(line));
            }
        }
        (panes, pane_id)
    }

    fn response_data(result: RequestResult) -> ScrollbackData {
        let RequestResult::Response(frame) = result else {
            panic!("expected a ScrollbackData response");
        };
        assert_eq!(frame.msg_type, MSG_SCROLLBACK_DATA);
        ScrollbackData::decode(&frame.payload).expect("decode ScrollbackData")
    }

    fn response_rows(result: RequestResult) -> Vec<String> {
        response_data(result)
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|c| char::from_u32(c.codepoint).unwrap_or('\0'))
                    .collect::<String>()
                    .trim_end_matches('\0')
                    .to_string()
            })
            .collect()
    }

    #[tokio::test]
    async fn get_scrollback_spans_the_archive_boundary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;

        let result = get_scrollback(1, &scrollback_frame(pane_id, -4, 4), &panes).await;

        assert_eq!(response_rows(result), vec!["one", "two", "three", "four"]);
    }

    /// End to end through the handler: a request reaching past the oldest
    /// retained row reports where it was actually served, and the rows
    /// that come back line up with that start rather than the request.
    #[tokio::test]
    async fn the_response_reports_the_start_it_served_not_the_one_requested() {
        let (panes, pane_id) = pane_with_scrollback(&["one", "two", "three"]).await;

        let served =
            response_data(get_scrollback(1, &scrollback_frame(pane_id, -10, 10), &panes).await);

        assert_eq!(served.start_row, -3, "only three rows precede the origin");
        assert_eq!(served.rows.len(), 3);
        let keyed_from_request: Vec<i64> = (0..3).map(|i| -10 + i).collect();
        let keyed_from_response: Vec<i64> = (0..3).map(|i| served.start_row + i).collect();
        assert_eq!(keyed_from_response, vec![-3, -2, -1]);
        assert_ne!(
            keyed_from_request, keyed_from_response,
            "the pre-fix echo keyed these rows ten rows too early"
        );
    }

    /// A window wholly inside history keeps reporting the requested
    /// start, so the field's new meaning is not a blanket rewrite.
    #[tokio::test]
    async fn an_unclamped_response_still_reports_the_requested_start() {
        let (panes, pane_id) = pane_with_scrollback(&["one", "two", "three"]).await;

        let served =
            response_data(get_scrollback(1, &scrollback_frame(pane_id, -2, 2), &panes).await);

        assert_eq!(served.start_row, -2);
    }

    /// The genuine disk-failure path: the writer is alive and answers,
    /// but the read itself fails. Distinct from the shut-down case below,
    /// which is refused before any read is attempted — both produce an
    /// error frame, so only a test that actually reaches `read_range` can
    /// tell the arms apart.
    #[tokio::test]
    async fn get_scrollback_reports_a_failing_disk_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive_dir = dir.path().join("archive");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            let archive = pane.screens.archive_mut().expect("archive");
            archive.flush_pending().expect("flush");
            archive.seal_active_segment().expect("seal");
        }
        // The writer keeps its segment metadata, so the read is attempted
        // and fails on the missing file rather than being skipped.
        let mut removed = 0;
        for entry in std::fs::read_dir(&archive_dir).expect("read archive dir") {
            let entry = entry.expect("dir entry");
            if entry.file_name().to_string_lossy().starts_with("segment-") {
                std::fs::remove_file(entry.path()).expect("remove segment");
                removed += 1;
            }
        }
        assert!(removed > 0, "need a sealed segment to break");

        let RequestResult::Response(frame) =
            get_scrollback(1, &scrollback_frame(pane_id, -4, 4), &panes).await
        else {
            panic!("expected an error response");
        };
        let err = ErrorMessage::decode(&frame.payload).expect("decode ErrorMessage");
        assert_eq!(
            ErrorCode::try_from(err.code).expect("known code"),
            ErrorCode::InternalError
        );
    }

    /// A transient archive failure must surface as an error frame, never
    /// as blank rows — a blank means a permanent hole, but a dead or
    /// wedged writer is retryable.
    #[tokio::test]
    async fn get_scrollback_reports_an_archive_read_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens
                .archive_mut()
                .expect("archive")
                .shutdown()
                .expect("shutdown");
        }

        let RequestResult::Response(frame) =
            get_scrollback(1, &scrollback_frame(pane_id, -4, 4), &panes).await
        else {
            panic!("expected an error response");
        };
        let err = ErrorMessage::decode(&frame.payload).expect("decode ErrorMessage");
        assert_eq!(
            ErrorCode::try_from(err.code).expect("known code"),
            ErrorCode::InternalError
        );
    }

    /// Wait until a read has actually reached the archive writer. A fixed
    /// sleep would only distinguish "finished" from "not finished": a task
    /// that had not yet reached the read would leave the pane lock free
    /// for the innocent reason that nobody had taken it, and every lock
    /// assertion below it would pass vacuously.
    pub(in crate::requests) async fn await_read_in_flight(stall: &mut ReadStall) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !stall.read_arrived() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no archive read reached the writer"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// The contract has two halves, and this is the half a `worker_threads
    /// = 4` test cannot see: the read must be on `spawn_blocking`, not on
    /// a runtime worker. With a single worker, a read left inline occupies
    /// the only thread the runtime has and all async work stops — the
    /// starvation this whole task exists to prevent.
    ///
    /// Driven from a plain thread rather than `#[tokio::test]` because
    /// every observation has to happen from outside the runtime: a starved
    /// worker cannot run the assertions that would notice it starving, so
    /// an in-runtime check only sees the aftermath and blames whatever it
    /// trips over next.
    #[test]
    fn a_stalled_archive_read_does_not_occupy_a_runtime_worker() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) = rt.block_on(pane_with_archived_prefix(
            dir.path(),
            &["one", "two", "three", "four"],
            2,
        ));
        let mut stall = rt.block_on(async {
            let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.archive().expect("archive").stall_next_read()
        });

        // A canary that can only tick if the runtime still has a worker.
        let ticks = Arc::new(AtomicU64::new(0));
        rt.spawn({
            let ticks = Arc::clone(&ticks);
            async move {
                loop {
                    ticks.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        });
        let request = rt.spawn({
            let panes = Arc::clone(&panes);
            let frame = scrollback_frame(pane_id, -4, 4);
            async move { get_scrollback(1, &frame, &panes).await }
        });

        while !stall.read_arrived() {
            std::thread::sleep(Duration::from_millis(1));
        }
        let before = ticks.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200));
        let after = ticks.load(Ordering::SeqCst);
        assert!(
            after > before,
            "runtime worker starved by the archive read: the canary made no progress \
             in 200ms while the read was parked"
        );

        stall.release();
        assert_eq!(
            response_rows(rt.block_on(request).expect("request task")),
            vec!["one", "two", "three", "four"]
        );
    }

    /// The proof that the archive read no longer runs under the pane
    /// lock: with the writer parked, the request cannot answer, yet the
    /// pane stays lockable. The budget is far under the archive's 10s
    /// query timeout, so the lock-held shape fails here instead of
    /// hanging.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stalled_archive_read_does_not_hold_the_pane_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        let mut stall = {
            let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.archive().expect("archive").stall_next_read()
        };

        let request = tokio::spawn({
            let panes = Arc::clone(&panes);
            let frame = scrollback_frame(pane_id, -4, 4);
            async move { get_scrollback(1, &frame, &panes).await }
        });
        await_read_in_flight(&mut stall).await;

        assert!(
            !request.is_finished(),
            "the stalled writer must still be blocking the read"
        );
        let locked =
            tokio::time::timeout(Duration::from_millis(500), lock_live_pane(&panes, pane_id)).await;
        assert!(
            locked
                .expect("pane lock free during the archive read")
                .is_some(),
            "pane vanished"
        );

        stall.release();
        let rows = response_rows(request.await.expect("request task"));
        assert_eq!(rows, vec!["one", "two", "three", "four"]);
    }

    /// Why this handler needs no top-up, unlike a yank: the hot rows are
    /// converted while the lock is held, so rows migrating into the
    /// archive during the read can be neither served twice nor lost.
    /// Rebuilding the tail afterwards would read a hot buffer that had
    /// moved on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rows_migrating_into_the_archive_mid_read_are_served_exactly_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        let mut stall = {
            let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.archive().expect("archive").stall_next_read()
        };

        let request = tokio::spawn({
            let panes = Arc::clone(&panes);
            let frame = scrollback_frame(pane_id, -4, 4);
            async move { get_scrollback(1, &frame, &panes).await }
        });
        await_read_in_flight(&mut stall).await;

        // "three" and "four" migrate out of the hot buffer and into the
        // archive while the snapshot's archive read is still parked.
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            for line in ["five", "six"] {
                pane.screens.push_to_scrollback(row_from(line));
            }
            assert!(
                pane.screens
                    .archive()
                    .expect("archive")
                    .total_rows_received()
                    > 2,
                "the pushes must archive rows the request already snapshotted"
            );
        }

        stall.release();
        assert_eq!(
            response_rows(request.await.expect("request task")),
            vec!["one", "two", "three", "four"]
        );
    }

    /// Covers the mapping only: a `JoinError` — which a panicking blocking
    /// task produces — must become an error rather than a silent empty
    /// result the client would read as a permanent gap. The production
    /// wiring around it is not exercised here; nothing in `read_range`
    /// panics on purpose, so a real one cannot be provoked without a hook
    /// that would only test itself.
    #[tokio::test]
    async fn a_join_error_maps_to_an_error_not_an_empty_read() {
        let join_error = tokio::spawn(async { panic!("boom") })
            .await
            .expect_err("task panicked");

        assert_eq!(
            map_read_outcome(1, 2, 0, 4, Err(join_error)),
            Err("archive read failed")
        );
    }

    #[test]
    fn scrollback_plan_degenerate_inputs() {
        // start_row = 0: zero rows, has_more still signals history exists.
        let plan = plan_unpinned(0, 100, 0, 10);
        assert_eq!(plan.hot_count, 0);
        assert!(plan.has_more);

        let plan = plan_unpinned(50, 100, -10, 0);
        assert_eq!(plan.archive_count + plan.hot_count, 0);

        let plan = plan_unpinned(50, 0, -20, 10);
        assert_eq!(plan.archive_start, 30);
        assert_eq!(plan.archive_count, 10);
        assert_eq!(plan.hot_count, 0);

        let plan = plan_unpinned(0, 0, -10, 10);
        assert_eq!(plan.archive_count + plan.hot_count, 0);
        assert!(!plan.has_more);
        assert_eq!(plan.total_rows, 0);
    }
}
