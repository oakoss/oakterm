//! Scrollback family: `GetScrollback` (0x73) and the Spec-0004 read plan.

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, PaneState, lock_live_pane};
use crate::wire::row_to_wire;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{ErrorCode, GetScrollback, MSG_SCROLLBACK_DATA, ScrollbackData};
use oakterm_protocol::render::DirtyRow;
use oakterm_terminal::grid::row::Row;
use oakterm_terminal::scroll::archive_manager::ArchiveManager;
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
    let Some(pane) = lock_live_pane(panes, req.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    let base = pane.copy_mode_base(conn_id);
    let layout = HistoryLayout::of(&pane);
    let buf = pane.screens.scrollback();
    let archive = pane.screens.archive();
    let plan = plan_scrollback_read(&layout, base, req.start_row, req.count);

    let rows = match build_scrollback_rows(conn_id, req.pane_id, &pane, &plan, archive, buf) {
        Ok(rows) => rows,
        Err(message) => {
            return make_error_response(conn_id, frame.serial, ErrorCode::InternalError, message);
        }
    };

    let data = ScrollbackData {
        pane_id: req.pane_id,
        start_row: req.start_row,
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

/// Assemble the response rows in absolute-index order: archived, then
/// blanks for the pruned-without-an-archive gap, then hot buffer rows.
fn build_scrollback_rows(
    conn_id: u64,
    pane_id: u32,
    pane: &PaneState,
    plan: &ScrollbackReadPlan,
    archive: Option<&ArchiveManager>,
    buf: &oakterm_terminal::scroll::HotBuffer,
) -> Result<Vec<DirtyRow>, &'static str> {
    let palette = &pane.screens.active_grid().palette;
    let cols = usize::from(pane.screens.active_grid().cols);
    let mut rows: Vec<DirtyRow> =
        Vec::with_capacity(plan.archive_count + plan.gap_count + plan.hot_count);

    if plan.archive_count > 0 {
        let found = read_archive_rows(
            conn_id,
            pane_id,
            archive,
            plan.archive_start,
            plan.archive_count,
        )?;
        for row in align_archive_rows(found, plan.archive_start, plan.archive_count, cols) {
            rows.push(row_to_wire(&row, 0, palette));
        }
    }

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
    Ok(rows)
}

/// Errors surface as an error frame, never blank rows: a blank means a
/// permanent gap, but a transient failure (wedged writer, EIO) must stay
/// retryable for the client.
pub(super) fn read_archive_rows(
    conn_id: u64,
    pane_id: u32,
    archive: Option<&ArchiveManager>,
    start: u64,
    count: usize,
) -> Result<Vec<(u64, Row)>, &'static str> {
    let Some(archive) = archive else {
        // Unreachable in practice: a nonzero count implies archived > 0,
        // read from this same binding. Logged loudly in case that
        // invariant ever breaks.
        error!(conn_id, pane_id, "archive vanished mid-request");
        return Err("archive unavailable");
    };
    archive.read_range(start, count).map_err(|e| {
        warn!(
            conn_id,
            pane_id,
            start,
            count,
            error = %e,
            "archive read failed"
        );
        "archive read failed"
    })
}

/// How a `GetScrollback` request maps onto the combined history:
/// absolute index 0 is the oldest row ever pruned to the archive, and
/// the hot buffer follows the archived range (Spec-0004 read path).
#[derive(Debug)]
struct ScrollbackReadPlan {
    archive_start: u64,
    archive_count: usize,
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
pub(super) fn align_archive_rows(
    found: Vec<(u64, Row)>,
    start: u64,
    count: usize,
    cols: usize,
) -> Vec<Row> {
    let mut found = found.into_iter().peekable();
    let mut out_of_contract: u64 = 0;
    let rows = (start..start + count as u64)
        .map(|idx| {
            // Entries behind the cursor (index regression, duplicates)
            // violate the sorted-unique contract; absorbing one silently
            // would blank every later row in the window.
            while found.peek().is_some_and(|(i, _)| *i < idx) {
                found.next();
                out_of_contract += 1;
            }
            match found.peek() {
                Some((i, _)) if *i == idx => found.next().expect("peeked").1,
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
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let heads: Vec<char> = aligned.iter().map(|r| r.cells[0].codepoint).collect();
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

        let heads: Vec<char> = aligned.iter().map(|r| r.cells[0].codepoint).collect();
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
