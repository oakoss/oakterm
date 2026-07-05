//! Scrollback family: `GetScrollback` (0x73) and the Spec-0004 read plan.

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, lock_live_pane};
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
const MAX_SCROLLBACK_ROWS_PER_REQUEST: u32 = 4096;

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
    let buf = pane.screens.scrollback();
    let archive = pane.screens.archive();
    let archived = archive.map_or(0, ArchiveManager::total_rows_received);
    let plan = plan_scrollback_read(archived, buf.len(), req.start_row, req.count);

    let palette = &pane.screens.active_grid().palette;
    let cols = usize::from(pane.screens.active_grid().cols);
    let mut rows: Vec<DirtyRow> = Vec::with_capacity(plan.archive_count + plan.hot_count);

    if plan.archive_count > 0 {
        match read_archive_rows(conn_id, req.pane_id, archive, &plan) {
            Ok(found) => {
                for row in align_archive_rows(found, plan.archive_start, plan.archive_count, cols) {
                    rows.push(row_to_wire(&row, 0, palette));
                }
            }
            Err(message) => {
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::InternalError,
                    message,
                );
            }
        }
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

/// Errors surface as an error frame, never blank rows: a blank means a
/// permanent gap, but a transient failure (wedged writer, EIO) must stay
/// retryable for the client.
fn read_archive_rows(
    conn_id: u64,
    pane_id: u32,
    archive: Option<&ArchiveManager>,
    plan: &ScrollbackReadPlan,
) -> Result<Vec<(u64, Row)>, &'static str> {
    let Some(archive) = archive else {
        // Unreachable in practice: archive_count > 0 implies
        // archived > 0, read from this same binding. Logged
        // loudly in case that invariant ever breaks.
        error!(conn_id, pane_id, "archive vanished mid-request");
        return Err("archive unavailable");
    };
    archive
        .read_range(plan.archive_start, plan.archive_count)
        .map_err(|e| {
            warn!(
                conn_id,
                pane_id,
                start = plan.archive_start,
                count = plan.archive_count,
                error = %e,
                "archive read failed"
            );
            "archive read failed"
        })
}

/// How a `GetScrollback` request maps onto the combined history:
/// absolute index 0 is the oldest row ever pruned to the archive, and
/// the hot buffer follows the archived range (Spec-0004 read path).
struct ScrollbackReadPlan {
    archive_start: u64,
    archive_count: usize,
    hot_start: usize,
    hot_count: usize,
    has_more: bool,
    total_rows: u32,
}

fn plan_scrollback_read(
    archived_rows: u64,
    hot_len: usize,
    start_row: i64,
    count: u32,
) -> ScrollbackReadPlan {
    let count = count.min(MAX_SCROLLBACK_ROWS_PER_REQUEST);
    let total = archived_rows + hot_len as u64;
    // start_row counts back from the present (Spec-0003 selection space).
    let start = total.saturating_add_signed(start_row).min(total);
    let end = start.saturating_add(u64::from(count)).min(total);

    let archive_end = end.min(archived_rows);
    let archive_count = archive_end.saturating_sub(start.min(archive_end));
    let hot_from = start.max(archived_rows);
    let hot_count = end.saturating_sub(hot_from);

    ScrollbackReadPlan {
        archive_start: start.min(archived_rows),
        archive_count: usize::try_from(archive_count).expect("bounded by request cap"),
        hot_start: usize::try_from(hot_from - archived_rows).expect("bounded by hot_len"),
        hot_count: usize::try_from(hot_count).expect("bounded by request cap"),
        has_more: start > 0,
        total_rows: u32::try_from(total).unwrap_or(u32::MAX),
    }
}

/// Positionally align index-tagged archive rows over
/// `[start, start + count)`, blank-filling indices the archive has no
/// row for (gaps from the Spec-0004 overload policy) so the client can
/// consume the response by position.
fn align_archive_rows(found: Vec<(u64, Row)>, start: u64, count: usize, cols: usize) -> Vec<Row> {
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

    #[test]
    fn scrollback_plan_hot_only_matches_legacy_semantics() {
        let plan = plan_scrollback_read(0, 100, -50, 10);
        assert_eq!(plan.archive_count, 0);
        assert_eq!(plan.hot_start, 50);
        assert_eq!(plan.hot_count, 10);
        assert!(plan.has_more);
        assert_eq!(plan.total_rows, 100);
    }

    #[test]
    fn scrollback_plan_spans_archive_and_hot() {
        let plan = plan_scrollback_read(100, 50, -120, 100);
        assert_eq!(plan.archive_start, 30);
        assert_eq!(plan.archive_count, 70);
        assert_eq!(plan.hot_start, 0);
        assert_eq!(plan.hot_count, 30);
        assert!(plan.has_more);
        assert_eq!(plan.total_rows, 150);
    }

    #[test]
    fn scrollback_plan_clamps_before_history_start() {
        let plan = plan_scrollback_read(100, 50, -i64::MAX, 10);
        assert_eq!(plan.archive_start, 0);
        assert_eq!(plan.archive_count, 10);
        assert_eq!(plan.hot_count, 0);
        assert!(!plan.has_more, "nothing older than absolute row 0");
    }

    #[test]
    fn scrollback_plan_count_clamps_to_present() {
        let plan = plan_scrollback_read(0, 20, -5, 100);
        assert_eq!(plan.hot_start, 15);
        assert_eq!(plan.hot_count, 5);
        assert!(plan.has_more);
    }

    #[test]
    fn scrollback_plan_archive_only_request() {
        let plan = plan_scrollback_read(100, 50, -150, 40);
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
        let plan = plan_scrollback_read(1_000_000, 100, -500_000, u32::MAX);
        assert_eq!(
            plan.archive_count + plan.hot_count,
            MAX_SCROLLBACK_ROWS_PER_REQUEST as usize
        );
    }

    #[test]
    fn scrollback_plan_degenerate_inputs() {
        // start_row = 0: zero rows, has_more still signals history exists.
        let plan = plan_scrollback_read(0, 100, 0, 10);
        assert_eq!(plan.hot_count, 0);
        assert!(plan.has_more);

        let plan = plan_scrollback_read(50, 100, -10, 0);
        assert_eq!(plan.archive_count + plan.hot_count, 0);

        let plan = plan_scrollback_read(50, 0, -20, 10);
        assert_eq!(plan.archive_start, 30);
        assert_eq!(plan.archive_count, 10);
        assert_eq!(plan.hot_count, 0);

        let plan = plan_scrollback_read(0, 0, -10, 10);
        assert_eq!(plan.archive_count + plan.hot_count, 0);
        assert!(!plan.has_more);
        assert_eq!(plan.total_rows, 0);
    }
}
