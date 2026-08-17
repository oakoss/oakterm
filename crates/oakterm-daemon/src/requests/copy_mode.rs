//! Copy mode family: `EnterCopyMode` (0x97), `ExitCopyMode` (0x98), and
//! `YankSelection` (0x99) / `YankResponse` (0x9A). See Spec-0008.

use super::scrollback::{ArchiveAccess, HistoryLayout, align_archive_rows, read_archive_rows};
use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, PaneState, SharedPane, lock_live_pane};
use oakterm_protocol::frame::{Frame, MAX_PAYLOAD};
use oakterm_protocol::message::{
    CopyMode, CopySelectionType, ErrorCode, YankResponse, YankSelection,
};
use oakterm_terminal::grid::row::Row;
use oakterm_terminal::scroll::archive_manager::{ArchiveManager, ArchiveReader};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

/// Row-span cap for one yank. Spec-0008 bounds a yank by the 16 MiB frame
/// limit (~160K lines); this caps the archive read that produces it, so a
/// selection spanning millions of rows cannot turn into an unbounded
/// blocking read.
const MAX_YANK_ROWS: u64 = 200_000;

/// `YankResponse` writes a `u32` length ahead of the text, so the text
/// itself has to fit in `MAX_PAYLOAD` minus that prefix. Checking against
/// the whole payload budget would let lengths in the last four bytes past
/// the handler only to fail in `Frame::new` as an opaque internal error.
const MAX_YANK_TEXT_BYTES: usize = MAX_PAYLOAD as usize - 4;

/// A `YankSelection` resolved into absolute history coordinates.
#[derive(Debug, PartialEq, Eq)]
struct YankPlan {
    first_row: u64,
    /// Inclusive.
    last_row: u64,
    ty: CopySelectionType,
    /// Character: the column on `first_row`. Block: the left edge.
    start_col: usize,
    /// Character: the column on `last_row`. Block: the right edge.
    end_col: usize,
}

impl YankPlan {
    fn row_count(&self) -> u64 {
        self.last_row - self.first_row + 1
    }

    /// Trim the plan to rows that exist, `grid_end` being one past the
    /// last live grid row. A shortened *character* selection takes its new
    /// last row whole: the client's end column belongs to a row that is
    /// not there, so the selection runs to the row edge instead. A block
    /// keeps its columns — they define the rectangle's right edge on every
    /// row, not an endpoint on the last one. `None` when nothing in the
    /// plan exists.
    fn clamped_to(mut self, grid_end: u64) -> Option<Self> {
        let last = grid_end.checked_sub(1)?;
        if self.first_row > last {
            return None;
        }
        if self.last_row > last {
            self.last_row = last;
            if self.ty == CopySelectionType::Character {
                self.end_col = usize::MAX;
            }
        }
        Some(self)
    }

    /// Inclusive column range to take from the row at absolute index `abs`.
    fn columns_for(&self, abs: u64) -> (usize, usize) {
        match self.ty {
            CopySelectionType::Line => (0, usize::MAX),
            CopySelectionType::Block => (self.start_col, self.end_col),
            CopySelectionType::Character => {
                let start = if abs == self.first_row {
                    self.start_col
                } else {
                    0
                };
                let end = if abs == self.last_row {
                    self.end_col
                } else {
                    usize::MAX
                };
                (start, end)
            }
        }
    }
}

/// Absolute history index for a copy-mode row. `base` is the client's
/// pinned viewport top. The flag reports a row that fell before the start
/// of history and was clamped onto the oldest row.
fn to_absolute(base: u64, row: i64) -> (u64, bool) {
    let abs = i128::from(base) + i128::from(row);
    (u64::try_from(abs.max(0)).unwrap_or(u64::MAX), abs < 0)
}

/// Both endpoints are inclusive cell coordinates, matching Spec-0003's
/// selection model. Character and line ranges normalize in reading order;
/// a block normalizes each axis on its own, so any drag direction yields
/// the same rectangle.
fn plan_yank(base: u64, req: &YankSelection) -> YankPlan {
    let (a_row, a_clamped) = to_absolute(base, req.start_row);
    let (b_row, b_clamped) = to_absolute(base, req.end_row);
    let ty = req.selection_type;
    if ty == CopySelectionType::Block {
        let (a_col, b_col) = (usize::from(req.start_col), usize::from(req.end_col));
        return YankPlan {
            first_row: a_row.min(b_row),
            last_row: a_row.max(b_row),
            ty,
            start_col: a_col.min(b_col),
            end_col: a_col.max(b_col),
        };
    }
    let a = (a_row, usize::from(req.start_col), a_clamped);
    let b = (b_row, usize::from(req.end_col), b_clamped);
    let (first, last) = if (a.0, a.1) <= (b.0, b.1) {
        (a, b)
    } else {
        (b, a)
    };
    // An endpoint clamped onto the oldest row lost the column it named,
    // so the selection opens to that row's edge — the same reasoning
    // `clamped_to` applies at the other end of history.
    YankPlan {
        first_row: first.0,
        last_row: last.0,
        ty,
        start_col: if first.2 { 0 } else { first.1 },
        end_col: if last.2 { usize::MAX } else { last.1 },
    }
}

/// `None` rows are holes in history (pruned without an archive, or a
/// Spec-0004 overload gap). They contribute an empty line rather than
/// vanishing, so line N of the result stays row `first_row + N`.
fn extract_text<'a>(plan: &YankPlan, rows: impl Iterator<Item = (u64, Option<&'a Row>)>) -> String {
    let lines: Vec<String> = rows
        .map(|(abs, row)| {
            row.map_or_else(String::new, |row| {
                let (start, end) = plan.columns_for(abs);
                row.text_range(start, end)
            })
        })
        .collect();
    lines.join("\n")
}

pub(super) async fn enter_copy_mode(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = CopyMode::decode(&frame.payload) else {
        warn!(conn_id, "malformed EnterCopyMode payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed EnterCopyMode",
        );
    };
    let Some(mut pane) = lock_live_pane(panes, msg.pane_id).await else {
        warn!(
            conn_id,
            pane_id = msg.pane_id,
            "EnterCopyMode for an unknown pane"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    let replaced = pane.pin_copy_mode(conn_id);
    let base = pane.copy_mode_base(conn_id);
    if let Some(previous) = replaced {
        warn!(
            conn_id,
            pane_id = msg.pane_id,
            previous,
            base,
            "duplicate EnterCopyMode; re-pinning at the current viewport"
        );
    } else {
        debug!(
            conn_id,
            pane_id = msg.pane_id,
            base,
            "copy mode entered, viewport pinned"
        );
    }
    RequestResult::NoResponse
}

pub(super) async fn exit_copy_mode(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = CopyMode::decode(&frame.payload) else {
        warn!(conn_id, "malformed ExitCopyMode payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed ExitCopyMode",
        );
    };
    let Some(mut pane) = lock_live_pane(panes, msg.pane_id).await else {
        warn!(
            conn_id,
            pane_id = msg.pane_id,
            "ExitCopyMode for an unknown pane"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    if pane.unpin_copy_mode(conn_id) {
        debug!(
            conn_id,
            pane_id = msg.pane_id,
            "copy mode exited, viewport unpinned"
        );
    } else {
        // A client that never entered, or one whose pin was already
        // released on a prior exit. Harmless, so it is not an error.
        debug!(
            conn_id,
            pane_id = msg.pane_id,
            "ExitCopyMode without an active pin"
        );
    }
    RequestResult::NoResponse
}

pub(super) async fn yank_selection(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(req) = YankSelection::decode(&frame.payload) else {
        warn!(conn_id, "malformed YankSelection payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed YankSelection",
        );
    };
    let text = match resolve_yank(conn_id, &req, panes).await {
        Ok(text) => text,
        Err(YankFailure::UnknownPane) => {
            warn!(
                conn_id,
                pane_id = req.pane_id,
                "YankSelection for an unknown pane"
            );
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::UnknownPane,
                "unknown pane",
            );
        }
        Err(YankFailure::TooManyRows(rows)) => {
            warn!(
                conn_id,
                pane_id = req.pane_id,
                rows,
                "YankSelection span exceeds the row cap"
            );
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InvalidMessage,
                "yank selection spans too many rows",
            );
        }
        Err(YankFailure::Archive(message)) => {
            return make_error_response(conn_id, frame.serial, ErrorCode::InternalError, message);
        }
    };

    if text.len() > MAX_YANK_TEXT_BYTES {
        warn!(
            conn_id,
            pane_id = req.pane_id,
            bytes = text.len(),
            "yank exceeds the maximum frame payload"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InvalidMessage,
            "yanked text exceeds the maximum frame size",
        );
    }
    debug!(
        conn_id,
        pane_id = req.pane_id,
        bytes = text.len(),
        "yank resolved"
    );

    let resp = YankResponse { text };
    match resp.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode YankResponse");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "YankResponse encode error",
            )
        }
    }
}

enum YankFailure {
    UnknownPane,
    TooManyRows(u64),
    Archive(&'static str),
}

/// Planning, the archive read, and assembly each take the pane lock
/// separately: the archive read blocks until the writer answers (TREK-197)
/// and holding the lock across it would stall this pane's PTY reader.
async fn resolve_yank(
    conn_id: u64,
    req: &YankSelection,
    panes: &Arc<Mutex<PaneManager>>,
) -> Result<String, YankFailure> {
    let planned = {
        let pane = lock_live_pane(panes, req.pane_id)
            .await
            .ok_or(YankFailure::UnknownPane)?;
        let grid_end = pane
            .history_len()
            .saturating_add(u64::from(pane.screens.active_grid().rows));
        let layout = HistoryLayout::of(&pane);
        let cols = usize::from(pane.screens.active_grid().cols);
        let access = ArchiveAccess::of(&pane);
        plan_yank(pane.copy_mode_base(conn_id), req)
            .clamped_to(grid_end)
            .map(|plan| (plan, layout, cols, access))
    };

    // The whole selection sits past the last row that exists.
    let Some((plan, layout, cols, access)) = planned else {
        return Ok(String::new());
    };
    if plan.row_count() > MAX_YANK_ROWS {
        return Err(YankFailure::TooManyRows(plan.row_count()));
    }

    let archive_start = plan.first_row.min(layout.archived);
    let archive_end = plan.last_row.saturating_add(1).min(layout.archived);
    let (first_rows, mut archive_shortfall) = read_archive_span(
        conn_id,
        req.pane_id,
        &access,
        archive_start,
        archive_end,
        cols,
    )
    .await
    .map_err(YankFailure::Archive)?;
    let mut window = ArchiveWindow::new(archive_start, first_rows);

    // Rows can migrate out of the hot buffer and into the archive while
    // the read above runs, landing outside the window it covered. One
    // top-up read collects them; rows that migrate after it blank-fill as
    // before, which is what bounds this to a single extra read.
    let top_up_start = archive_end.max(plan.first_row);
    let mut assembled = None;
    let top_up_end = {
        let pane = lock_live_pane(panes, req.pane_id).await.ok_or_else(|| {
            warn!(conn_id, pane_id = req.pane_id, "pane closed mid-yank");
            YankFailure::UnknownPane
        })?;
        let end = HistoryLayout::of(&pane)
            .archived
            .min(plan.last_row.saturating_add(1));
        // Nothing migrated, which is the common case: assemble under the
        // guard already held rather than dropping and re-taking it.
        if end <= top_up_start {
            assembled = Some(collect_yank_text(&pane, &plan, &window, archive_shortfall));
        }
        end
    };

    let (text, missing) = if let Some(done) = assembled {
        done
    } else {
        let (extra, top_up_shortfall) = read_archive_span(
            conn_id,
            req.pane_id,
            &access,
            top_up_start,
            top_up_end,
            cols,
        )
        .await
        .map_err(YankFailure::Archive)?;
        window.extend(top_up_start, extra);
        archive_shortfall += top_up_shortfall;
        let requested = top_up_end - top_up_start;
        debug!(
            conn_id,
            pane_id = req.pane_id,
            requested,
            recovered = requested - top_up_shortfall,
            "rows migrated into the archive mid-yank; topped up"
        );
        let pane = lock_live_pane(panes, req.pane_id).await.ok_or_else(|| {
            warn!(conn_id, pane_id = req.pane_id, "pane closed mid-yank");
            YankFailure::UnknownPane
        })?;
        collect_yank_text(&pane, &plan, &window, archive_shortfall)
    };
    if let Some(missing) = missing {
        warn_missing_rows(conn_id, req.pane_id, missing, access.reader().cloned());
    }
    Ok(text)
}

/// Walk the plan's rows across the tiers they can live in — disk archive,
/// the pruned-without-an-archive gap, hot buffer, live grid — in that
/// order, which is also ascending absolute index. The plan is already
/// clamped to rows that exist.
fn collect_yank_text(
    pane: &PaneState,
    plan: &YankPlan,
    archive: &ArchiveWindow,
    archive_shortfall: u64,
) -> (String, Option<MissingRows>) {
    let layout = HistoryLayout::of(pane);
    let buf = pane.screens.scrollback();
    let grid = pane.screens.active_grid();

    // Seeded rather than counted below: rows the archive could not supply
    // were blank-filled inside the window, so every lookup for them
    // succeeds and none of them would ever be counted here.
    let mut missing = MissingRows {
        archive_gap: archive_shortfall,
        ..MissingRows::default()
    };
    let mut rows: Vec<(u64, Option<&Row>)> = Vec::new();
    for abs in plan.first_row..=plan.last_row {
        let row = if abs < layout.archived {
            let row = archive.get(abs);
            missing.archive_gap += u64::from(row.is_none());
            row
        } else if abs < layout.hot_first {
            missing.pruned += 1;
            None
        } else if abs < layout.pushed {
            let row = buf.get(usize::try_from(abs - layout.hot_first).unwrap_or(usize::MAX));
            missing.index_miss += u64::from(row.is_none());
            row
        } else {
            let row = grid
                .lines
                .get(usize::try_from(abs - layout.pushed).unwrap_or(usize::MAX));
            missing.index_miss += u64::from(row.is_none());
            row
        };
        rows.push((abs, row));
    }
    if missing.total() > 0 {
        missing.dropped = pane
            .screens
            .archive()
            .map_or(0, ArchiveManager::dropped_rows);
    }
    let missing = (missing.total() > 0).then_some(missing);
    (extract_text(plan, rows.into_iter()), missing)
}

/// Rows the assembled text could not source, split by cause — they mean
/// different things and only one is normal. `pruned` is a permanent hole
/// (pruned with no archive attached); `archive_gap` is a Spec-0004
/// overload loss or a row that migrated in after the top-up; `index_miss`
/// is a row the plan claimed exists in a live tier, which is a bug here,
/// not data loss. `dropped` is the enqueue-side loss count, a free field
/// read taken under the lock for context.
#[derive(Clone, Copy, Default)]
struct MissingRows {
    archive_gap: u64,
    pruned: u64,
    index_miss: u64,
    dropped: u64,
}

impl MissingRows {
    fn total(&self) -> u64 {
        self.archive_gap + self.pruned + self.index_miss
    }
}

/// Report blank-filled rows without making the client wait for it.
///
/// The writer-side loss count is a mailbox round-trip that can take the
/// full query timeout, and this runs on the response path — awaiting it
/// would delay a `YankResponse` by up to 10s to decorate a log line. A
/// detached task can lose the line if the runtime shuts down first, which
/// is the right trade for a diagnostic: at that point the daemon is
/// exiting anyway.
fn warn_missing_rows(
    conn_id: u64,
    pane_id: u32,
    missing: MissingRows,
    reader: Option<ArchiveReader>,
) {
    tokio::spawn(async move {
        let writer_lost = match reader {
            Some(reader) => {
                match tokio::task::spawn_blocking(move || reader.writer_lost_rows()).await {
                    Ok(lost) => lost,
                    Err(e) => {
                        warn!(conn_id, pane_id, error = %e, "archive stats task failed");
                        None
                    }
                }
            }
            None => None,
        };
        warn!(
            conn_id,
            pane_id,
            archive_gap = missing.archive_gap,
            pruned_before_archive = missing.pruned,
            index_miss = missing.index_miss,
            archive_dropped_rows = missing.dropped,
            archive_writer_lost_rows = ?writer_lost,
            "yank could not source every row; blank-filled"
        );
    });
}

/// Archive rows for one yank, plus the absolute index of the first entry.
/// A wrong origin shifts every row and yields wrong text with no error.
struct ArchiveWindow {
    rows: Vec<Row>,
    start: u64,
}

impl ArchiveWindow {
    fn new(start: u64, rows: Vec<Row>) -> Self {
        Self { rows, start }
    }

    /// Append rows read from absolute index `from`, rebasing if nothing
    /// has been collected yet.
    fn extend(&mut self, from: u64, rows: Vec<Row>) {
        if self.rows.is_empty() {
            self.start = from;
        }
        assert!(
            from.checked_sub(self.start) == u64::try_from(self.rows.len()).ok(),
            "an archive window must stay contiguous"
        );
        self.rows.extend(rows);
    }

    fn get(&self, abs: u64) -> Option<&Row> {
        let offset = abs.checked_sub(self.start)?;
        self.rows.get(usize::try_from(offset).ok()?)
    }
}

/// Returns the aligned rows and how many of them are blank fill the
/// archive could not supply. That shortfall is invisible downstream —
/// blank fill is indistinguishable from a blank terminal row — so it has
/// to travel with the rows.
///
/// One read, not a chunk loop: `ArchiveCore::read_range` pulls every
/// touched segment file whole, so splitting a span only re-reads segments.
async fn read_archive_span(
    conn_id: u64,
    pane_id: u32,
    access: &ArchiveAccess,
    start: u64,
    end: u64,
    cols: usize,
) -> Result<(Vec<Row>, u64), &'static str> {
    if start >= end {
        return Ok((Vec::new(), 0));
    }
    let count = usize::try_from(end - start).unwrap_or(usize::MAX);
    let found = read_archive_rows(conn_id, pane_id, access.clone(), start, count).await?;
    let (rows, supplied) = align_archive_rows(found, start, count, cols);
    Ok((rows, u64::try_from(count - supplied).unwrap_or(u64::MAX)))
}

/// Release every pin this client holds, across all panes. Copy-mode pins
/// are the only per-client state the daemon keeps inside a pane, so a
/// client that drops mid-copy-mode would otherwise leave one behind for
/// the life of the pane.
pub(crate) async fn release_client_pins(conn_id: u64, panes: &Arc<Mutex<PaneManager>>) {
    let pane_list: Vec<(u32, SharedPane)> = panes.lock().await.snapshot();
    for (pane_id, pane) in pane_list {
        let mut pane = pane.lock().await;
        if pane.unpin_copy_mode(conn_id) {
            debug!(
                conn_id,
                pane_id, "released copy mode pin on client disconnect"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::scrollback::tests::await_read_in_flight;
    use super::*;
    use oakterm_protocol::message::{ErrorMessage, MSG_YANK_RESPONSE};

    const COLS: usize = 80;

    fn req(
        start_row: i64,
        start_col: u16,
        end_row: i64,
        end_col: u16,
        ty: CopySelectionType,
    ) -> YankSelection {
        YankSelection {
            pane_id: 1,
            start_row,
            start_col,
            end_row,
            end_col,
            selection_type: ty,
        }
    }

    fn row_from(text: &str, cols: usize) -> Row {
        let mut row = Row::new(cols);
        for (cell, ch) in row.cells.iter_mut().zip(text.chars()) {
            cell.codepoint = ch;
        }
        row
    }

    #[test]
    fn plan_maps_copy_mode_rows_onto_the_pinned_base() {
        // Row 0 is the pinned viewport top; negatives run into scrollback.
        let plan = plan_yank(100, &req(-10, 2, 3, 7, CopySelectionType::Character));
        assert_eq!(plan.first_row, 90);
        assert_eq!(plan.last_row, 103);
        assert_eq!(plan.start_col, 2);
        assert_eq!(plan.end_col, 7);
        assert_eq!(plan.row_count(), 14);
    }

    #[test]
    fn plan_normalizes_an_inverted_character_range() {
        let forward = plan_yank(100, &req(-4, 1, -2, 9, CopySelectionType::Character));
        let inverted = plan_yank(100, &req(-2, 9, -4, 1, CopySelectionType::Character));
        assert_eq!(forward, inverted);
    }

    #[test]
    fn plan_normalizes_an_inverted_single_row_character_range() {
        let forward = plan_yank(100, &req(-4, 3, -4, 8, CopySelectionType::Character));
        let inverted = plan_yank(100, &req(-4, 8, -4, 3, CopySelectionType::Character));
        assert_eq!(forward, inverted);
        assert_eq!(forward.start_col, 3);
        assert_eq!(forward.end_col, 8);
    }

    #[test]
    fn plan_clamps_rows_before_the_start_of_history() {
        let plan = plan_yank(10, &req(-9_000, 0, -8_990, 5, CopySelectionType::Line));
        assert_eq!(plan.first_row, 0);
        assert_eq!(plan.last_row, 0, "both endpoints clamp to the oldest row");
        assert_eq!(plan.row_count(), 1);
    }

    #[test]
    fn plan_corners_a_block_selection_independently_per_axis() {
        // Bottom-left to top-right drag: rows and columns each normalize.
        let plan = plan_yank(100, &req(-2, 20, -6, 4, CopySelectionType::Block));
        assert_eq!((plan.first_row, plan.last_row), (94, 98));
        assert_eq!((plan.start_col, plan.end_col), (4, 20));
    }

    #[test]
    fn columns_span_the_full_row_for_a_line_selection() {
        let plan = plan_yank(100, &req(-3, 5, -1, 2, CopySelectionType::Line));
        assert_eq!(plan.columns_for(97), (0, usize::MAX));
        assert_eq!(plan.columns_for(99), (0, usize::MAX));
    }

    #[test]
    fn columns_are_constant_for_a_block_selection() {
        let plan = plan_yank(100, &req(-3, 4, -1, 9, CopySelectionType::Block));
        for abs in 97..=99 {
            assert_eq!(plan.columns_for(abs), (4, 9));
        }
    }

    #[test]
    fn character_columns_open_at_the_first_row_and_close_at_the_last() {
        let plan = plan_yank(100, &req(-3, 4, -1, 9, CopySelectionType::Character));
        assert_eq!(plan.columns_for(97), (4, usize::MAX));
        assert_eq!(plan.columns_for(98), (0, usize::MAX));
        assert_eq!(plan.columns_for(99), (0, 9));
    }

    #[test]
    fn character_columns_on_a_single_row_use_both_endpoints() {
        let plan = plan_yank(100, &req(-1, 4, -1, 9, CopySelectionType::Character));
        assert_eq!(plan.columns_for(99), (4, 9));
    }

    #[test]
    fn extract_joins_rows_with_newlines() {
        let plan = plan_yank(100, &req(-3, 2, -1, 3, CopySelectionType::Character));
        let a = row_from("0123456789", 10);
        let b = row_from("abcdefghij", 10);
        let c = row_from("ABCDEFGHIJ", 10);
        let rows = vec![(97, Some(&a)), (98, Some(&b)), (99, Some(&c))];

        assert_eq!(
            extract_text(&plan, rows.into_iter()),
            "23456789\nabcdefghij\nABCD"
        );
    }

    #[test]
    fn extract_slices_a_block_out_of_every_row() {
        let plan = plan_yank(100, &req(-2, 2, -1, 4, CopySelectionType::Block));
        let a = row_from("0123456789", 10);
        let b = row_from("abcdefghij", 10);
        let rows = vec![(98, Some(&a)), (99, Some(&b))];

        assert_eq!(extract_text(&plan, rows.into_iter()), "234\ncde");
    }

    #[test]
    fn extract_of_no_rows_is_empty() {
        let plan = plan_yank(100, &req(-1, 0, -1, 5, CopySelectionType::Line));
        assert_eq!(extract_text(&plan, std::iter::empty()), "");
    }

    #[test]
    fn extract_keeps_a_blank_row_as_an_empty_line() {
        let plan = plan_yank(100, &req(-2, 0, -1, 9, CopySelectionType::Line));
        let blank = Row::new(10);
        let text = row_from("abc", 10);
        let rows = vec![(98, Some(&blank)), (99, Some(&text))];

        assert_eq!(extract_text(&plan, rows.into_iter()), "\nabc");
    }

    // --- Handlers ---

    async fn pane_with_scrollback(lines: &[&str]) -> (Arc<Mutex<PaneManager>>, u32) {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());
        push_scrollback(&panes, pane_id, lines).await;
        (panes, pane_id)
    }

    async fn push_scrollback(panes: &Arc<Mutex<PaneManager>>, pane_id: u32, lines: &[&str]) {
        let mut pane = lock_live_pane(panes, pane_id).await.expect("pane");
        for line in lines {
            pane.screens.push_to_scrollback(row_from(line, COLS));
        }
    }

    async fn pins(panes: &Arc<Mutex<PaneManager>>, pane_id: u32) -> Vec<u64> {
        let pane = lock_live_pane(panes, pane_id).await.expect("pane");
        let mut ids: Vec<u64> = pane.copy_mode_pins.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    async fn pinned_base(panes: &Arc<Mutex<PaneManager>>, pane_id: u32, conn_id: u64) -> u64 {
        let pane = lock_live_pane(panes, pane_id).await.expect("pane");
        *pane.copy_mode_pins.get(&conn_id).expect("pin")
    }

    fn enter_frame(pane_id: u32) -> Frame {
        CopyMode { pane_id }.to_enter_frame().expect("enter frame")
    }

    fn exit_frame(pane_id: u32) -> Frame {
        CopyMode { pane_id }.to_exit_frame().expect("exit frame")
    }

    fn yank_frame(pane_id: u32, sel: YankSelection) -> Frame {
        YankSelection { pane_id, ..sel }
            .to_frame(7)
            .expect("yank frame")
    }

    fn expect_error(result: RequestResult) -> ErrorCode {
        let RequestResult::Response(frame) = result else {
            panic!("expected an error response");
        };
        let err = ErrorMessage::decode(&frame.payload).expect("decode ErrorMessage");
        ErrorCode::try_from(err.code).expect("known error code")
    }

    fn expect_yanked(result: RequestResult) -> String {
        let RequestResult::Response(frame) = result else {
            panic!("expected a YankResponse");
        };
        assert_eq!(frame.msg_type, MSG_YANK_RESPONSE);
        YankResponse::decode(&frame.payload)
            .expect("decode YankResponse")
            .text
    }

    #[tokio::test]
    async fn enter_pins_the_client_at_the_current_history_end() {
        let (panes, pane_id) = pane_with_scrollback(&["a", "b", "c"]).await;

        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        assert_eq!(pins(&panes, pane_id).await, vec![1]);
        assert_eq!(pinned_base(&panes, pane_id, 1).await, 3);
    }

    #[tokio::test]
    async fn each_client_pins_independently() {
        let (panes, pane_id) = pane_with_scrollback(&["a"]).await;

        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        push_scrollback(&panes, pane_id, &["b", "c"]).await;
        enter_copy_mode(2, &enter_frame(pane_id), &panes).await;

        assert_eq!(pins(&panes, pane_id).await, vec![1, 2]);
        assert_eq!(pinned_base(&panes, pane_id, 1).await, 1);
        assert_eq!(pinned_base(&panes, pane_id, 2).await, 3);
    }

    #[tokio::test]
    /// ADR-0012: Enter records the *current* viewport offset, so a
    /// duplicate enter is an implicit exit plus enter.
    async fn a_second_enter_repins_at_the_current_viewport() {
        let (panes, pane_id) = pane_with_scrollback(&["a"]).await;

        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        push_scrollback(&panes, pane_id, &["b", "c"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        assert_eq!(pinned_base(&panes, pane_id, 1).await, 3);
        assert_eq!(pins(&panes, pane_id).await, vec![1], "still one pin");
    }

    #[tokio::test]
    async fn exit_unpins_only_the_exiting_client() {
        let (panes, pane_id) = pane_with_scrollback(&["a"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        enter_copy_mode(2, &enter_frame(pane_id), &panes).await;

        exit_copy_mode(1, &exit_frame(pane_id), &panes).await;

        assert_eq!(pins(&panes, pane_id).await, vec![2]);
    }

    #[tokio::test]
    async fn exit_without_a_prior_enter_is_accepted() {
        let (panes, pane_id) = pane_with_scrollback(&["a"]).await;

        let result = exit_copy_mode(1, &exit_frame(pane_id), &panes).await;

        assert!(matches!(result, RequestResult::NoResponse));
        assert!(pins(&panes, pane_id).await.is_empty());
    }

    #[tokio::test]
    async fn enter_and_exit_on_an_unknown_pane_error() {
        let (panes, _) = pane_with_scrollback(&["a"]).await;

        assert_eq!(
            expect_error(enter_copy_mode(1, &enter_frame(999), &panes).await),
            ErrorCode::UnknownPane
        );
        assert_eq!(
            expect_error(exit_copy_mode(1, &exit_frame(999), &panes).await),
            ErrorCode::UnknownPane
        );
    }

    #[tokio::test]
    async fn malformed_copy_mode_payloads_error() {
        let (panes, _) = pane_with_scrollback(&["a"]).await;
        let short = Frame::new(0x97, 0, vec![0x00]).expect("frame");

        assert_eq!(
            expect_error(enter_copy_mode(1, &short, &panes).await),
            ErrorCode::MalformedPayload
        );
        assert_eq!(
            expect_error(exit_copy_mode(1, &short, &panes).await),
            ErrorCode::MalformedPayload
        );
        assert_eq!(
            expect_error(yank_selection(1, &short, &panes).await),
            ErrorCode::MalformedPayload
        );
    }

    #[tokio::test]
    async fn disconnect_releases_every_pin_the_client_held() {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let (a, b) = {
            let mut pm = panes.lock().await;
            (
                pm.create(80, 24, String::new(), String::new()),
                pm.create(80, 24, String::new(), String::new()),
            )
        };
        enter_copy_mode(1, &enter_frame(a), &panes).await;
        enter_copy_mode(1, &enter_frame(b), &panes).await;
        enter_copy_mode(2, &enter_frame(a), &panes).await;

        release_client_pins(1, &panes).await;

        assert_eq!(pins(&panes, a).await, vec![2], "other clients keep theirs");
        assert!(pins(&panes, b).await.is_empty());
    }

    #[tokio::test]
    async fn yank_reads_across_the_hot_buffer() {
        let (panes, pane_id) = pane_with_scrollback(&["alpha", "bravo", "charlie"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let frame = yank_frame(pane_id, req(-3, 0, -1, 2, CopySelectionType::Character));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "alpha\nbravo\ncha"
        );
    }

    #[tokio::test]
    async fn yank_reads_the_live_grid_for_non_negative_rows() {
        let (panes, pane_id) = pane_with_scrollback(&["scrolled"]).await;
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.active_grid_mut().lines[0] = row_from("on screen", COLS);
        }
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        // Row -1 is the last scrollback row, row 0 the top of the viewport.
        let frame = yank_frame(pane_id, req(-1, 0, 0, 8, CopySelectionType::Character));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "scrolled\non screen"
        );
    }

    #[tokio::test]
    async fn a_pinned_client_keeps_stable_row_indices_as_output_arrives() {
        let (panes, pane_id) = pane_with_scrollback(&["first", "second"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        let frame = yank_frame(pane_id, req(-2, 0, -2, 4, CopySelectionType::Character));
        let before = expect_yanked(yank_selection(1, &frame, &panes).await);

        push_scrollback(&panes, pane_id, &["third", "fourth"]).await;

        assert_eq!(before, "first");
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "first",
            "the pin holds row -2 in place while output scrolls"
        );
        // An unpinned client resolves the same request against live output.
        assert_eq!(
            expect_yanked(yank_selection(2, &frame, &panes).await),
            "third"
        );
    }

    /// Drives the real prune-into-archive path rather than injecting rows
    /// into the archive directly: only rows that pass through the hot
    /// buffer are counted in the absolute index space, so an injected row
    /// would sit outside it.
    async fn pane_with_archived_prefix(
        dir: &std::path::Path,
        lines: &[&str],
        keep: usize,
    ) -> (Arc<Mutex<PaneManager>>, u32) {
        let (panes, pane_id) = pane_with_scrollback(&[]).await;
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            let archive =
                ArchiveManager::new(dir.join("archive"), 1 << 20).expect("create archive");
            pane.screens.set_archive(archive);

            pane.screens.push_to_scrollback(row_from(lines[0], COLS));
            let row_bytes = pane.screens.scrollback().used_bytes();
            // Prune keeps 90% of the limit, so size it to retain `keep` rows.
            let pruned = pane
                .screens
                .scrollback_mut()
                .set_max_bytes(row_bytes * (keep + 1));
            assert!(pruned.is_empty(), "resize must not drop rows unarchived");
            for line in &lines[1..] {
                pane.screens.push_to_scrollback(row_from(line, COLS));
            }
        }
        (panes, pane_id)
    }

    #[tokio::test]
    async fn yank_spans_the_archive_boundary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        {
            let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            assert_eq!(pane.screens.scrollback().len(), 2, "two rows pruned out");
            assert_eq!(pane.history_len(), 4);
        }
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        // Whole history: two archived rows, then two hot ones.
        let frame = yank_frame(pane_id, req(-4, 0, -1, 0, CopySelectionType::Line));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "one\ntwo\nthree\nfour"
        );

        // Entirely inside the archive.
        let frame = yank_frame(pane_id, req(-4, 0, -3, 0, CopySelectionType::Line));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "one\ntwo"
        );
    }

    /// Shrink the hot buffer so later pushes prune. Returns once the limit
    /// is set; nothing is pruned by the resize itself.
    async fn cap_scrollback_rows(panes: &Arc<Mutex<PaneManager>>, pane_id: u32, keep: usize) {
        let mut pane = lock_live_pane(panes, pane_id).await.expect("pane");
        let row_bytes = pane.screens.scrollback().used_bytes();
        assert!(row_bytes > 0, "need a pushed row to size against");
        let pruned = pane
            .screens
            .scrollback_mut()
            .set_max_bytes(row_bytes * (keep + 1));
        assert!(pruned.is_empty(), "resize must not prune here");
    }

    /// The regression fix 2 guards: with no archive, pruned rows are gone
    /// for good. Deriving the origin from `archived + len` made it slide
    /// backwards, so a pinned row resolved onto whichever row later
    /// occupied that buffer slot and the yank returned wrong text as
    /// success. Anchoring on the monotonic push counter turns it into a
    /// gap instead.
    #[tokio::test]
    async fn a_pin_on_an_archiveless_pane_never_serves_the_wrong_rows() {
        let (panes, pane_id) = pane_with_scrollback(&["target"]).await;
        {
            let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            assert!(pane.screens.archive().is_none(), "no archive attached");
        }
        cap_scrollback_rows(&panes, pane_id, 2).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let frame = yank_frame(pane_id, req(-1, 0, -1, 5, CopySelectionType::Line));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "target"
        );

        push_scrollback(&panes, pane_id, &["a", "b", "c", "d", "e"]).await;

        let text = expect_yanked(yank_selection(1, &frame, &panes).await);
        assert!(
            text.is_empty() || text == "target",
            "pinned row must read as a gap once pruned, got {text:?}"
        );
    }

    /// A block keeps its column rectangle when the span is trimmed to the
    /// grid: resetting the right edge to the row end (correct for a
    /// character selection) would widen every row of the block.
    #[tokio::test]
    async fn a_block_past_the_grid_bottom_keeps_its_columns() {
        let (panes, pane_id) = pane_with_scrollback(&[]).await;
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            for row in 0..3 {
                pane.screens.active_grid_mut().lines[row] = row_from("0123456789", COLS);
            }
        }
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let frame = yank_frame(pane_id, req(0, 4, 999, 8, CopySelectionType::Block));
        let text = expect_yanked(yank_selection(1, &frame, &panes).await);

        let first: Vec<&str> = text.lines().take(3).collect();
        assert_eq!(first, vec!["45678", "45678", "45678"]);
    }

    /// Both endpoints predate history, so neither column names a real
    /// cell; the selection opens to both row edges.
    #[tokio::test]
    async fn a_range_entirely_before_history_opens_to_both_row_edges() {
        let (panes, pane_id) = pane_with_scrollback(&["alpha"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let frame = yank_frame(pane_id, req(-9, 3, -5, 2, CopySelectionType::Character));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "alpha"
        );
    }

    /// A hole in history contributes an empty line rather than vanishing,
    /// so line N of the result stays row `first_row + N`.
    #[tokio::test]
    async fn a_gap_in_history_blank_fills_instead_of_shifting_lines() {
        let (panes, pane_id) = pane_with_scrollback(&["gone"]).await;
        cap_scrollback_rows(&panes, pane_id, 2).await;
        push_scrollback(&panes, pane_id, &["a", "b", "c", "keep"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        // Five rows pushed, only the last two retained: rows -5..-3 are gaps.
        let frame = yank_frame(pane_id, req(-5, 0, -1, 0, CopySelectionType::Line));
        let text = expect_yanked(yank_selection(1, &frame, &panes).await);

        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 5, "one line per requested row: {text:?}");
        assert_eq!(*lines.last().expect("last line"), "keep");
    }

    /// Rows migrating between the live grid and scrollback keeps their
    /// absolute index, so a pin taken before a resize still names them.
    #[tokio::test]
    async fn scrollback_rows_survive_a_resize_under_a_pin() {
        let (panes, pane_id) = pane_with_scrollback(&["first", "second"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        let frame = yank_frame(pane_id, req(-2, 0, -1, 0, CopySelectionType::Line));
        let before = expect_yanked(yank_selection(1, &frame, &panes).await);
        assert_eq!(before, "first\nsecond");

        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.resize_all(80, 10);
        }

        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            before,
            "scrollback rows keep their absolute index across a resize"
        );
    }

    /// Live grid rows do *not* keep their index across a shrink — the grid
    /// captures its trailing rows into scrollback — so a resize that moved
    /// rows drops the pins rather than serving shifted content.
    #[tokio::test]
    async fn a_resize_that_captures_rows_drops_the_pins() {
        let (panes, pane_id) = pane_with_scrollback(&["one"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        enter_copy_mode(2, &enter_frame(pane_id), &panes).await;

        let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
        let before = pane.history_len();
        pane.screens.resize_all(80, 10);
        assert_eq!(pane.invalidate_pins_after_resize(before), 2);
        assert!(pane.copy_mode_pins.is_empty());
    }

    /// The size limit is tied to the encoder's real overhead rather than
    /// asserted as a literal, so a future wire field cannot silently push
    /// the boundary back past `MAX_PAYLOAD`. Measured on a one-byte text
    /// instead of allocating the full 16 MiB.
    #[test]
    fn the_yank_size_limit_reserves_the_length_prefix() {
        let encoded = YankResponse {
            text: "x".to_string(),
        }
        .encode()
        .expect("encode");
        let overhead = encoded.len() - 1;

        assert_eq!(overhead, 4, "u32 length prefix");
        assert_eq!(
            MAX_YANK_TEXT_BYTES + overhead,
            MAX_PAYLOAD as usize,
            "a text at the limit encodes to exactly one full frame"
        );
        assert!(
            MAX_YANK_TEXT_BYTES + 1 + overhead > MAX_PAYLOAD as usize,
            "one byte over must not fit"
        );
    }

    #[tokio::test]
    async fn a_resize_that_moves_no_rows_keeps_the_pins() {
        let (panes, pane_id) = pane_with_scrollback(&["one"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
        let before = pane.history_len();
        // Growing appends blank rows; nothing migrates into scrollback.
        pane.screens.resize_all(80, 40);
        assert_eq!(pane.invalidate_pins_after_resize(before), 0);
        assert_eq!(pane.copy_mode_pins.len(), 1);
    }

    #[tokio::test]
    async fn yank_of_an_inverted_range_matches_the_forward_range() {
        let (panes, pane_id) = pane_with_scrollback(&["alpha", "bravo"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let forward = yank_frame(pane_id, req(-2, 1, -1, 3, CopySelectionType::Character));
        let inverted = yank_frame(pane_id, req(-1, 3, -2, 1, CopySelectionType::Character));

        let text = expect_yanked(yank_selection(1, &forward, &panes).await);
        assert_eq!(text, "lpha\nbrav");
        assert_eq!(
            expect_yanked(yank_selection(1, &inverted, &panes).await),
            text
        );
    }

    #[tokio::test]
    async fn yank_of_a_single_cell_returns_that_cell() {
        let (panes, pane_id) = pane_with_scrollback(&["alpha"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let frame = yank_frame(pane_id, req(-1, 2, -1, 2, CopySelectionType::Character));
        assert_eq!(expect_yanked(yank_selection(1, &frame, &panes).await), "p");
    }

    #[tokio::test]
    async fn yank_past_the_bottom_of_the_grid_is_empty() {
        let (panes, pane_id) = pane_with_scrollback(&["alpha"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        // The pane is 24 rows tall; rows at and past 24 do not exist.
        let frame = yank_frame(pane_id, req(30, 0, 40, 0, CopySelectionType::Line));
        assert_eq!(expect_yanked(yank_selection(1, &frame, &panes).await), "");
    }

    #[tokio::test]
    async fn yank_past_the_bottom_takes_the_last_real_row_whole() {
        let (panes, pane_id) = pane_with_scrollback(&[]).await;
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.active_grid_mut().lines[0] = row_from("hello", COLS);
        }
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let frame = yank_frame(pane_id, req(0, 0, 999, 2, CopySelectionType::Character));
        let text = expect_yanked(yank_selection(1, &frame, &panes).await);

        assert!(text.starts_with("hello\n"));
        assert_eq!(
            text.matches('\n').count(),
            23,
            "clamped to the 24-row grid, not to the requested end row"
        );
    }

    #[tokio::test]
    async fn a_start_before_history_opens_at_the_row_edge() {
        let (panes, pane_id) = pane_with_scrollback(&["one", "two"]).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        // Start column 7 belongs to a row older than anything retained.
        let frame = yank_frame(pane_id, req(-5, 7, -1, 2, CopySelectionType::Character));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "one\ntwo"
        );
    }

    #[test]
    fn clamping_shortens_the_span_and_opens_its_end_column() {
        let plan = plan_yank(100, &req(-2, 1, 50, 4, CopySelectionType::Character))
            .clamped_to(120)
            .expect("some rows exist");
        assert_eq!((plan.first_row, plan.last_row), (98, 119));
        assert_eq!(plan.start_col, 1);
        assert_eq!(plan.end_col, usize::MAX);
    }

    #[test]
    fn clamping_a_span_that_starts_past_the_end_yields_nothing() {
        let past_the_end = req(30, 0, 40, 0, CopySelectionType::Line);
        assert!(plan_yank(100, &past_the_end).clamped_to(120).is_none());
        // An empty history has no last row to clamp onto.
        assert!(plan_yank(0, &past_the_end).clamped_to(0).is_none());
    }

    /// The handler rejects a span over [`MAX_YANK_ROWS`] rather than
    /// letting it become a multi-second archive read and an unencodable
    /// frame. Exercised on the plan directly: reaching the branch through
    /// a pane would need 200K rows of real history.
    #[test]
    fn the_row_cap_covers_a_span_larger_than_a_frame_can_carry() {
        let span = i64::try_from(MAX_YANK_ROWS).expect("cap fits i64");
        let base = MAX_YANK_ROWS + 10;
        let plan = plan_yank(base, &req(-span - 1, 0, 0, 0, CopySelectionType::Line));

        assert!(plan.row_count() > MAX_YANK_ROWS);
        assert!(
            plan.clamped_to(base + 24).expect("rows exist").row_count() > MAX_YANK_ROWS,
            "clamping to the grid must not mask the oversized span"
        );
    }

    /// Wide glyphs occupy two columns but one codepoint. A block must
    /// slice by column, and a range ending on a wide head must not emit
    /// the glyph from a continuation cell outside the range.
    #[tokio::test]
    async fn yank_slices_wide_glyphs_by_column() {
        use oakterm_terminal::grid::cell::WideState;

        let (panes, pane_id) = pane_with_scrollback(&[]).await;
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            for row in 0..2 {
                let line = &mut pane.screens.active_grid_mut().lines[row];
                *line = row_from("ab漢xy", COLS);
                // Lay the wide glyph out properly: head at 2, continuation at 3.
                line.cells[2].codepoint = '漢';
                line.cells[2].wide = WideState::Wide;
                line.cells[3].codepoint = '\0';
                line.cells[3].wide = WideState::WideCont;
                line.cells[4].codepoint = 'x';
                line.cells[5].codepoint = 'y';
            }
        }
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        // Block covering both cells of the glyph.
        let frame = yank_frame(pane_id, req(0, 2, 1, 3, CopySelectionType::Block));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "漢\n漢"
        );

        // Range ending on the head cell still yields the whole glyph once.
        let frame = yank_frame(pane_id, req(0, 0, 0, 2, CopySelectionType::Character));
        assert_eq!(
            expect_yanked(yank_selection(1, &frame, &panes).await),
            "ab漢"
        );

        // Starting on the continuation cell drops the glyph: its head is
        // outside the range.
        let frame = yank_frame(pane_id, req(0, 3, 0, 5, CopySelectionType::Character));
        assert_eq!(expect_yanked(yank_selection(1, &frame, &panes).await), "xy");
    }

    /// A transient archive failure must surface as an error, never as
    /// blank rows — a blank means a permanent hole, but a wedged writer is
    /// retryable.
    #[tokio::test]
    async fn yank_reports_an_archive_read_failure() {
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
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let frame = yank_frame(pane_id, req(-4, 0, -3, 0, CopySelectionType::Line));
        assert_eq!(
            expect_error(yank_selection(1, &frame, &panes).await),
            ErrorCode::InternalError
        );
    }

    /// The cap rejects spans the frame could never carry. The clamp trims
    /// a span to rows that exist first, so reaching this branch needs
    /// history genuinely deeper than the cap — the rows are pruned as they
    /// go, keeping the test's memory flat.
    #[tokio::test]
    async fn yank_over_the_row_cap_is_rejected_by_the_handler() {
        let (panes, pane_id) = pane_with_scrollback(&["a"]).await;
        cap_scrollback_rows(&panes, pane_id, 2).await;
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            for _ in 0..MAX_YANK_ROWS + 10 {
                pane.screens.push_to_scrollback(Row::new(COLS));
            }
            assert!(pane.history_len() > MAX_YANK_ROWS);
        }
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let span = i64::try_from(MAX_YANK_ROWS).expect("cap fits i64");
        let frame = yank_frame(pane_id, req(-span - 5, 0, 0, 0, CopySelectionType::Line));
        assert_eq!(
            expect_error(yank_selection(1, &frame, &panes).await),
            ErrorCode::InvalidMessage
        );
    }

    /// Take the pane lock, failing rather than hanging if it is held.
    /// Without the bound, a lock-held-across-the-read regression deadlocks
    /// until the archive's 10s query timeout and then trips whichever
    /// downstream assertion notices first, blaming the wrong thing.
    async fn lock_pane_promptly(
        panes: &Arc<Mutex<PaneManager>>,
        pane_id: u32,
    ) -> tokio::sync::OwnedMutexGuard<PaneState> {
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            lock_live_pane(panes, pane_id),
        )
        .await
        .expect("pane lock held across the archive read")
        .expect("pane")
    }

    /// The yank's *main* archive read must be off the pane lock, not just
    /// the diagnostic. With the writer parked mid-read the yank cannot
    /// answer, yet the pane has to stay lockable.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stalled_yank_read_does_not_hold_the_pane_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        let mut stall = {
            let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.archive().expect("archive").stall_next_read()
        };

        let frame = yank_frame(pane_id, req(-4, 0, -1, 0, CopySelectionType::Line));
        let yank = tokio::spawn({
            let panes = Arc::clone(&panes);
            async move { yank_selection(1, &frame, &panes).await }
        });
        await_read_in_flight(&mut stall).await;

        assert!(!yank.is_finished(), "the parked read must hold the yank");
        drop(lock_pane_promptly(&panes, pane_id).await);

        stall.release();
        assert_eq!(
            expect_yanked(yank.await.expect("yank task")),
            "one\ntwo\nthree\nfour"
        );
    }

    /// A row can migrate out of the hot buffer and into the archive after
    /// the yank has planned its read, landing outside the window that read
    /// covered. It must be topped up, not blank-filled: it is still in
    /// history, so returning an empty line would lose real output.
    ///
    /// The stalled writer holds the first read open; the rows pushed while
    /// it is parked queue behind it, so the top-up read observes them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_row_archived_mid_yank_is_topped_up_not_blank_filled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        let mut stall = {
            let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.archive().expect("archive").stall_next_read()
        };

        let frame = yank_frame(pane_id, req(-4, 0, -1, 0, CopySelectionType::Line));
        let yank = tokio::spawn({
            let panes = Arc::clone(&panes);
            async move { yank_selection(1, &frame, &panes).await }
        });
        // The rendezvous also orders the pushes below *after* the yank's
        // read: were they to win the race, no top-up would be needed and
        // the test would exercise nothing.
        await_read_in_flight(&mut stall).await;

        // "three" and "four" are pruned into the archive while the yank's
        // first read is still parked.
        push_scrollback(&panes, pane_id, &["five", "six"]).await;
        {
            let pane = lock_pane_promptly(&panes, pane_id).await;
            let archive = pane.screens.archive().expect("archive");
            assert_eq!(archive.dropped_rows(), 0, "the mailbox must not overflow");
            assert!(
                archive.total_rows_received() > 2,
                "the pushes must have archived the rows the yank already planned around"
            );
        }

        stall.release();
        assert_eq!(
            expect_yanked(yank.await.expect("yank task")),
            "one\ntwo\nthree\nfour"
        );
    }

    /// The top-up's other shape, and the one whose failure is silent: the
    /// first read's span was empty, so the top-up's rows are the first
    /// thing collected and the window must rebase onto them. Get it wrong
    /// and every row shifts against its absolute index, yielding
    /// confidently wrong text with no error anywhere.
    ///
    /// Covered here rather than through a concurrent yank because an empty
    /// span issues no read at all, leaving nothing to synchronise on — a
    /// timing-based version could only ever be hopeful.
    /// A row the archive could not supply is blank-filled *inside* the
    /// window, so `ArchiveWindow::get` answers `Some(blank)` for it and no
    /// lookup ever misses. Counting only lookup misses therefore hides the
    /// Spec-0004 overload loss the counter exists to report — the shortfall
    /// has to be carried from the read itself.
    #[tokio::test]
    async fn an_in_window_archive_shortfall_counts_as_a_gap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
        assert_eq!(HistoryLayout::of(&pane).archived, 2, "rows 0..2 archived");

        // The read covered rows 0 and 1 but the archive supplied only row
        // 0; row 1 came back as blank fill, a shortfall of one.
        let plan = plan_yank(4, &req(-4, 0, -3, 0, CopySelectionType::Line));
        let window = ArchiveWindow::new(0, vec![row_from("one", COLS), Row::new(COLS)]);

        let (_, missing) = collect_yank_text(&pane, &plan, &window, 1);

        let missing = missing.expect("a blank-filled archive row is a missing row");
        assert_eq!(missing.archive_gap, 1);
        assert_eq!(missing.index_miss, 0, "not a planning bug");
        assert_eq!(missing.pruned, 0, "the archive had these rows' indices");
    }

    /// The shortfall and the lookup miss count different rows, so a row
    /// must not reach both: blank fill lands inside the window and is
    /// carried by the shortfall, while a row that migrated in after the
    /// top-up lies outside it and is caught by the miss.
    #[tokio::test]
    async fn a_blank_filled_row_is_not_counted_twice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lines = ["one", "two", "three", "four", "five", "six"];
        let (panes, pane_id) = pane_with_archived_prefix(dir.path(), &lines, 2).await;
        let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
        assert!(
            HistoryLayout::of(&pane).archived > 2,
            "row 2 must sit in the archive tier"
        );

        // Window covers rows 0 and 1 with one blank fill; row 2 is
        // archived but was never collected.
        let base = u64::try_from(lines.len()).expect("row count");
        let plan = plan_yank(base, &req(-6, 0, -4, 0, CopySelectionType::Line));
        assert_eq!((plan.first_row, plan.last_row), (0, 2));
        let window = ArchiveWindow::new(0, vec![row_from("one", COLS), Row::new(COLS)]);

        let (_, missing) = collect_yank_text(&pane, &plan, &window, 1);

        let missing = missing.expect("missing rows");
        assert_eq!(
            missing.archive_gap, 2,
            "one blank fill plus one uncollected row, each counted once"
        );
    }

    #[test]
    fn an_empty_archive_window_rebases_onto_the_top_up() {
        let mut window = ArchiveWindow::new(7, Vec::new());

        window.extend(9, vec![row_from("nine", COLS), row_from("ten", COLS)]);

        assert!(window.get(7).is_none(), "the planned origin held nothing");
        assert!(window.get(8).is_none());
        assert_eq!(window.get(9).expect("row 9").text_range(0, 4), "nine");
        assert_eq!(window.get(10).expect("row 10").text_range(0, 3), "ten");
        assert!(window.get(11).is_none());
    }

    #[test]
    fn a_populated_archive_window_extends_without_moving() {
        let mut window = ArchiveWindow::new(4, vec![row_from("four", COLS)]);

        window.extend(5, vec![row_from("five", COLS)]);

        assert!(window.get(3).is_none(), "nothing lies before the origin");
        assert_eq!(window.get(4).expect("row 4").text_range(0, 4), "four");
        assert_eq!(window.get(5).expect("row 5").text_range(0, 4), "five");
    }

    /// Nothing holds the pane open across the archive read any more, so a
    /// close landing mid-read must be caught when the yank comes back for
    /// the lock — an error, never text assembled from a dead pane.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_pane_closed_mid_yank_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) =
            pane_with_archived_prefix(dir.path(), &["one", "two", "three", "four"], 2).await;
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;
        let mut stall = {
            let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens.archive().expect("archive").stall_next_read()
        };

        let frame = yank_frame(pane_id, req(-4, 0, -1, 0, CopySelectionType::Line));
        let yank = tokio::spawn({
            let panes = Arc::clone(&panes);
            async move { yank_selection(1, &frame, &panes).await }
        });
        await_read_in_flight(&mut stall).await;

        lock_pane_promptly(&panes, pane_id).await.closed = true;
        stall.release();

        assert_eq!(
            expect_error(yank.await.expect("yank task")),
            ErrorCode::UnknownPane
        );
    }

    /// The missing-rows diagnostic wants a writer round-trip for the
    /// lost-row count, and a wedged writer makes that round-trip cost the
    /// full query timeout. This yank reads nothing from disk — its rows
    /// fell into a gap that predates the archive — so the stalled writer
    /// can only be reached by the diagnostic, and neither the response nor
    /// the pane lock may wait on it. A log field is never worth either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_missing_rows_diagnostic_delays_neither_response_nor_pane() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (panes, pane_id) = pane_with_scrollback(&["gone"]).await;
        cap_scrollback_rows(&panes, pane_id, 2).await;
        push_scrollback(&panes, pane_id, &["a", "b", "c", "keep"]).await;
        let gate = {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            // Attached after the prunes, so the lost rows are a permanent
            // gap and no archive read is planned.
            pane.screens.set_archive(
                ArchiveManager::new(dir.path().join("archive"), 1 << 20).expect("archive"),
            );
            pane.screens.archive().expect("archive").stall_writer()
        };
        enter_copy_mode(1, &enter_frame(pane_id), &panes).await;

        let frame = yank_frame(pane_id, req(-5, 0, -1, 0, CopySelectionType::Line));
        let answered = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            yank_selection(1, &frame, &panes),
        )
        .await
        .expect("response must not wait on the diagnostic");

        let locked = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            lock_live_pane(&panes, pane_id),
        )
        .await;
        assert!(
            locked
                .expect("pane lock free during the missing-rows diagnostic")
                .is_some(),
            "pane vanished"
        );

        let text = expect_yanked(answered);
        assert_eq!(text.split('\n').count(), 5, "one line per requested row");
        drop(gate);
    }

    #[tokio::test]
    async fn yank_on_an_unknown_pane_errors() {
        let (panes, _) = pane_with_scrollback(&["alpha"]).await;

        let frame = yank_frame(999, req(-1, 0, -1, 0, CopySelectionType::Line));
        assert_eq!(
            expect_error(yank_selection(1, &frame, &panes).await),
            ErrorCode::UnknownPane
        );
    }
}
