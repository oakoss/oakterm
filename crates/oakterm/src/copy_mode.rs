//! Copy-mode client state (Spec-0008): the cursor and the viewport cache
//! that backs it, in the row space the daemon pins on `EnterCopyMode`.
//!
//! Row 0 is the live grid's top row at entry — the daemon pins there
//! regardless of where the client was scrolled — and negatives run into
//! scrollback, matching `GetScrollback.start_row` and `YankSelection`
//! rows. Rows `0..visible_rows` stay in the client's frozen grid
//! snapshot, so the cache only ever holds negative rows: `GetScrollback`
//! never serves the live grid. A pane entered from scrollback shows rows
//! `[-offset, -offset + visible_rows)`, which is why `viewport_top`
//! exists rather than everything assuming 0.

use crate::copy_keys::PendingPrefix;
use crate::copy_motion::MotionBounds;
use oakterm_protocol::message::ScrollbackData;
use oakterm_protocol::render::DirtyRow;
use std::collections::{BTreeMap, HashMap};
use tracing::warn;

/// Spec-0008's `CopySelectionType` is the wire enum: the client's shapes
/// and the daemon's are the same three by definition, and a second enum
/// beside it would only add a conversion that can drift.
pub(crate) use oakterm_protocol::message::CopySelectionType;

/// Per-request row cap the daemon enforces (Spec-0001 `GetScrollback`).
const MAX_ROWS_PER_REQUEST: u32 = 4096;

/// Cache window as a multiple of the visible rows (Spec-0008 default).
const CACHE_ROWS_PER_SCREEN: u32 = 3;

/// One `GetScrollback` window in copy-mode row space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FillRequest {
    pub(crate) start_row: i64,
    pub(crate) count: u32,
}

impl FillRequest {
    fn end_row(self) -> i64 {
        self.start_row.saturating_add(i64::from(self.count))
    }

    /// Whether two half-open windows share a row. An empty window shares
    /// none: the interval formula alone would report a zero-row window
    /// as overlapping anything spanning its start.
    fn overlaps(self, other: Self) -> bool {
        self.count > 0
            && other.count > 0
            && self.start_row < other.end_row()
            && other.start_row < self.end_row()
    }
}

/// An outstanding fill and whether it has already been retried once.
#[derive(Debug, Clone, Copy)]
struct PendingFill {
    request: FillRequest,
    retried: bool,
}

/// What a failed fill leaves the caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FillFailure {
    /// The serial belongs to no copy-mode fill on this pane.
    Unclaimed,
    /// Re-issue this window once. The daemon reports transient archive
    /// trouble as an error rather than blank rows so it stays retryable.
    Retry(FillRequest),
    /// Give up and leave copy mode: retrying a wedged archive forever
    /// spins, and a hole would let the user yank text never received.
    Abandon,
}

/// Daemon scrollback rows keyed by copy-mode row index.
///
/// The window is derived from the keys rather than stored alongside them:
/// a separately tracked `start`/`count` is one more pair that can disagree
/// with the rows it describes, which is the desync class `PaneView`
/// already guards the viewport offset against.
pub(crate) struct ViewportCache {
    rows: BTreeMap<i64, DirtyRow>,
    capacity: u32,
}

impl ViewportCache {
    fn new(capacity: u32) -> Self {
        Self {
            rows: BTreeMap::new(),
            capacity: capacity.max(1),
        }
    }

    /// Oldest cached row, or `None` when nothing is cached.
    pub(crate) fn start(&self) -> Option<i64> {
        self.rows.keys().next().copied()
    }

    /// One past the newest cached row.
    pub(crate) fn end(&self) -> Option<i64> {
        self.rows.keys().next_back().map(|last| last + 1)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn get(&self, row: i64) -> Option<&DirtyRow> {
        self.rows.get(&row)
    }

    /// The row's cells as characters, empty cells reading as blanks —
    /// the form word motions scan. `None` for an unserved row.
    pub(crate) fn row_chars(&self, row: i64) -> Option<Vec<char>> {
        self.rows.get(&row).map(|dirty| {
            dirty
                .cells
                .iter()
                .map(|cell| {
                    if cell.codepoint == 0 {
                        ' '
                    } else {
                        char::from_u32(cell.codepoint).unwrap_or('\u{FFFD}')
                    }
                })
                .collect()
        })
    }

    /// File a served window, keyed from where the daemon actually started
    /// rather than from where the request asked it to. Returns whether it
    /// was filed.
    ///
    /// A window abutting neither end is DROPPED rather than installed.
    /// Eviction only ever takes from the ends, so a reply that no longer
    /// touches the cache is one a trim moved out from under while it was
    /// in flight — installing it would wipe the rows the cursor is
    /// sitting in and leave it addressing a window far away. Prefetch
    /// re-issues the window if it is still wanted.
    fn insert_window(&mut self, served_start: i64, rows: Vec<DirtyRow>) -> bool {
        if rows.is_empty() {
            return false;
        }
        let served_end = served_start.saturating_add(i64::try_from(rows.len()).unwrap_or(i64::MAX));
        if let (Some(start), Some(end)) = (self.start(), self.end())
            && (served_start > end || served_end < start)
        {
            return false;
        }
        for (i, row) in rows.into_iter().enumerate() {
            let index = i64::try_from(i).unwrap_or(i64::MAX);
            self.rows.insert(served_start.saturating_add(index), row);
        }
        true
    }

    /// Evict from whichever end is farther from `cursor_row` until the
    /// window fits, so the rows the cursor is walking toward survive.
    fn trim_to_capacity(&mut self, cursor_row: i64) {
        while self.rows.len() > self.capacity as usize {
            let (Some(first), Some(last)) = (
                self.rows.keys().next().copied(),
                self.rows.keys().next_back().copied(),
            ) else {
                return;
            };
            let drop_front = cursor_row.saturating_sub(first) >= last.saturating_sub(cursor_row);
            self.rows.remove(if drop_front { &first } else { &last });
        }
    }
}

/// An active selection: the anchor it started from, with the cursor as
/// its other end (Spec-0008 Selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopySelection {
    pub(crate) ty: CopySelectionType,
    pub(crate) anchor_row: i64,
    pub(crate) anchor_col: u16,
}

/// A selection ordered into the inclusive endpoints `YankSelection`
/// carries (Spec-0008 selection range semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct YankRange {
    pub(crate) ty: CopySelectionType,
    pub(crate) start_row: i64,
    pub(crate) start_col: u16,
    pub(crate) end_row: i64,
    pub(crate) end_col: u16,
}

/// Order a selection's endpoints. Character and line ranges normalize in
/// reading order; a block normalizes each axis on its own, so any drag
/// direction yields the same rectangle.
///
/// The daemon normalizes again on receipt — this is what lets the client
/// render the selection it is about to send without a second ordering
/// rule that could disagree with the one on the wire.
fn normalize_range(selection: CopySelection, cursor: (i64, u16)) -> YankRange {
    let CopySelection {
        ty,
        anchor_row,
        anchor_col,
    } = selection;
    let (cursor_row, cursor_col) = cursor;
    if ty == CopySelectionType::Block {
        return YankRange {
            ty,
            start_row: anchor_row.min(cursor_row),
            start_col: anchor_col.min(cursor_col),
            end_row: anchor_row.max(cursor_row),
            end_col: anchor_col.max(cursor_col),
        };
    }
    let (start, end) = if (anchor_row, anchor_col) <= (cursor_row, cursor_col) {
        ((anchor_row, anchor_col), (cursor_row, cursor_col))
    } else {
        ((cursor_row, cursor_col), (anchor_row, anchor_col))
    };
    YankRange {
        ty,
        start_row: start.0,
        start_col: start.1,
        end_row: end.0,
        end_col: end.1,
    }
}

/// Copy-mode state for one pane. Lives inside `PaneView` so it follows
/// the pane across focus changes rather than the window.
pub(crate) struct CopyModeState {
    cursor_row: i64,
    cursor_col: u16,
    selection: Option<CopySelection>,
    /// The first key of a sequence (`gg`) awaiting its second. Per-pane
    /// so focusing away and back cannot fire a `g` armed on another pane.
    pending_prefix: Option<PendingPrefix>,
    cache: ViewportCache,
    /// Visible rows at entry, which is both the pinned viewport height
    /// and the chunk size for fills.
    visible_rows: u16,
    /// Pane width at entry. The cursor's column clamp lives here so the
    /// state and the motion bounds cannot disagree about the pane shape.
    cols: u16,
    /// Daemon row at the top of the frozen viewport: `-offset` entering
    /// from scrollback, 0 entering live. The pin is always the live
    /// grid's top, so without this the rows shown and the rows operated
    /// on drift apart by the scroll offset.
    viewport_top: i64,
    /// Fills awaiting a reply, by request serial.
    in_flight: HashMap<u32, PendingFill>,
    /// Oldest row the daemon has shown it can serve, learned when a
    /// window comes back front-clamped or reports no older history.
    history_start: Option<i64>,
}

impl CopyModeState {
    /// Seed at the bottom-left of the frozen viewport (Spec-0008 entry).
    /// `viewport_offset` is the pane's scroll offset at entry; entering
    /// live (0) seeds at `rows - 1` as the spec describes.
    pub(crate) fn new(cols: u16, visible_rows: u16, viewport_offset: u32) -> Self {
        let rows = u32::from(visible_rows.max(1));
        let viewport_top = -i64::from(viewport_offset);
        Self {
            cursor_row: viewport_top + i64::from(rows) - 1,
            cursor_col: 0,
            selection: None,
            pending_prefix: None,
            cache: ViewportCache::new(rows.saturating_mul(CACHE_ROWS_PER_SCREEN)),
            visible_rows: visible_rows.max(1),
            cols: cols.max(1),
            viewport_top,
            in_flight: HashMap::new(),
            history_start: None,
        }
    }

    pub(crate) fn cursor(&self) -> (i64, u16) {
        (self.cursor_row, self.cursor_col)
    }

    pub(crate) fn cache(&self) -> &ViewportCache {
        &self.cache
    }

    /// Daemon row shown at the top of the frozen viewport.
    pub(crate) fn viewport_top(&self) -> i64 {
        self.viewport_top
    }

    /// The newest row copy mode can address: the live grid's bottom row.
    /// Entering scrolled, the rows below the frozen page are addressable
    /// but read as blank — `PaneRows` serves the frozen snapshot, and the
    /// live rows behind it are deliberately not consulted.
    fn last_row(&self) -> i64 {
        i64::from(self.visible_rows) - 1
    }

    /// The oldest row copy mode can address: the oldest cached row, or
    /// the top of the frozen viewport before any fill has landed — those
    /// rows are on screen from the grid snapshot, not from the cache.
    fn first_row(&self) -> i64 {
        self.cache
            .start()
            .unwrap_or(self.viewport_top)
            .min(self.viewport_top)
    }

    /// Move the cursor, clamping into the rows copy mode can show. The
    /// range is the cache plus the frozen viewport, not the cache alone:
    /// the cursor starts on the live grid, which `GetScrollback` never
    /// serves.
    pub(crate) fn set_cursor(&mut self, row: i64, col: u16) {
        self.cursor_row = row.clamp(self.first_row(), self.last_row());
        self.cursor_col = col.min(self.cols - 1);
    }

    /// The region motions resolve inside: the addressable rows plus the
    /// pane's shape.
    pub(crate) fn motion_bounds(&self) -> MotionBounds {
        MotionBounds {
            first_row: self.first_row(),
            last_row: self.last_row(),
            cols: self.cols,
            visible_rows: self.visible_rows,
        }
    }

    /// The prefix key awaiting the rest of its sequence.
    pub(crate) fn pending_prefix(&self) -> Option<PendingPrefix> {
        self.pending_prefix
    }

    pub(crate) fn set_pending_prefix(&mut self, pending: Option<PendingPrefix>) {
        self.pending_prefix = pending;
    }

    /// Start, switch, or cancel a selection (Spec-0008): toggling the
    /// active type cancels it, a different type switches shape and keeps
    /// the anchor where the user put it.
    pub(crate) fn toggle_selection(&mut self, ty: CopySelectionType) {
        match self.selection.map(|selection| selection.ty) {
            Some(active) if active == ty => self.selection = None,
            Some(_) => {
                if let Some(selection) = self.selection.as_mut() {
                    selection.ty = ty;
                }
            }
            None => {
                self.selection = Some(CopySelection {
                    ty,
                    anchor_row: self.cursor_row,
                    anchor_col: self.cursor_col,
                });
            }
        }
    }

    /// Drop the selection, reporting whether there was one. Escape uses
    /// the answer to decide between clearing and leaving copy mode.
    pub(crate) fn clear_selection(&mut self) -> bool {
        self.selection.take().is_some()
    }

    /// The selection as ordered inclusive endpoints, or `None` when
    /// there is nothing selected to yank.
    pub(crate) fn yank_range(&self) -> Option<YankRange> {
        self.selection
            .map(|selection| normalize_range(selection, (self.cursor_row, self.cursor_col)))
    }

    /// The entry fill: the frozen viewport plus one screen either side
    /// (Spec-0008), centred on what the user is looking at rather than on
    /// the live grid, and stopping at row 0.
    ///
    /// Capping at 0 is load-bearing: output arriving between the pin and
    /// this request moves grid rows into history, so an uncapped window
    /// would cache rows the frozen snapshot owns and evict real
    /// scrollback to fit them.
    pub(crate) fn initial_fill(&self) -> FillRequest {
        let screen = u32::from(self.visible_rows);
        let start_row = self.viewport_top - i64::from(screen);
        let count = u32::try_from(-start_row).unwrap_or(MAX_ROWS_PER_REQUEST);
        FillRequest {
            start_row,
            count: count
                .min(screen.saturating_mul(CACHE_ROWS_PER_SCREEN))
                .min(MAX_ROWS_PER_REQUEST),
        }
    }

    /// The next chunk to fetch when the cursor has entered the top or
    /// bottom quarter of the cache window (Spec-0008 prefetch), or `None`
    /// when it has not, when the chunk is already cached or in flight, or
    /// when no history remains in that direction.
    pub(crate) fn plan_prefetch(&self) -> Option<FillRequest> {
        let (start, end) = (self.cache.start()?, self.cache.end()?);
        let len = end - start;
        let zone = (len / 4).max(1);
        let chunk = u32::from(self.visible_rows).min(MAX_ROWS_PER_REQUEST);

        let candidate = if self.cursor_row < start + zone {
            // Saturating because `oldest` is a sentinel until the daemon
            // reports where history begins; the true span is irrelevant
            // once it exceeds one chunk.
            let oldest = self.history_start.unwrap_or(i64::MIN);
            if start <= oldest {
                return None;
            }
            let want = i64::from(chunk).min(start.saturating_sub(oldest));
            FillRequest {
                start_row: start - want,
                count: u32::try_from(want).unwrap_or(chunk),
            }
        } else if self.cursor_row >= end - zone {
            // Rows at and past 0 live in the frozen grid snapshot, so
            // there is nothing further for the daemon to serve.
            let want = i64::from(chunk).min(-end);
            if want <= 0 {
                return None;
            }
            FillRequest {
                start_row: end,
                count: u32::try_from(want).unwrap_or(chunk),
            }
        } else {
            return None;
        };

        if candidate.count == 0 || self.is_pending(candidate) {
            return None;
        }
        Some(candidate)
    }

    /// Whether a window is already covered by an in-flight request.
    fn is_pending(&self, candidate: FillRequest) -> bool {
        self.in_flight
            .values()
            .any(|pending| pending.request.overlaps(candidate))
    }

    pub(crate) fn record_fill(&mut self, serial: u32, request: FillRequest) {
        self.in_flight.insert(
            serial,
            PendingFill {
                request,
                retried: false,
            },
        );
    }

    /// Record the re-issue of a window that already failed once, so its
    /// second failure abandons rather than retrying again.
    pub(crate) fn record_retry(&mut self, serial: u32, request: FillRequest) {
        self.in_flight.insert(
            serial,
            PendingFill {
                request,
                retried: true,
            },
        );
    }

    #[cfg(test)]
    pub(crate) fn is_fill_in_flight(&self, serial: u32) -> bool {
        self.in_flight.contains_key(&serial)
    }

    /// Retire a fill the daemon answered with an error. Leaving it in
    /// flight would leave the cache unfillable and, because
    /// `plan_prefetch` suppresses overlapping windows, permanently mute
    /// retries for exactly the region the cursor is moving into.
    ///
    /// `retryable` is false for a failure re-issuing cannot fix, such as
    /// the pane no longer existing.
    pub(crate) fn fail_fill(&mut self, serial: u32, retryable: bool) -> FillFailure {
        let Some(pending) = self.in_flight.remove(&serial) else {
            return FillFailure::Unclaimed;
        };
        if retryable && !pending.retried {
            FillFailure::Retry(pending.request)
        } else {
            FillFailure::Abandon
        }
    }

    /// File a `ScrollbackData` reply into the cache. Returns false when
    /// the serial matches no outstanding fill, which leaves the response
    /// to the caller's ordinary scrollback path.
    pub(crate) fn apply_fill(&mut self, serial: u32, data: &ScrollbackData) -> bool {
        let Some(PendingFill { request, .. }) = self.in_flight.remove(&serial) else {
            return false;
        };
        if data.start_row > request.start_row || !data.has_more {
            self.history_start = Some(data.start_row);
        }
        if data.start_row != request.start_row {
            warn!(
                requested = request.start_row,
                served = data.start_row,
                "scrollback window clamped; keying rows from the served start"
            );
        }
        // The cursor needs no re-clamp: trimming evicts from the end
        // farther from it, and a non-abutting window is discarded.
        if self.cache.insert_window(data.start_row, data.rows.clone()) {
            self.cache.trim_to_capacity(self.cursor_row);
        } else if !data.rows.is_empty() {
            warn!(
                served = data.start_row,
                rows = data.rows.len(),
                "scrollback window no longer abuts the cache; discarding"
            );
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CopyModeState, CopySelectionType, FillFailure, FillRequest, ViewportCache, YankRange,
    };
    use oakterm_protocol::message::ScrollbackData;
    use oakterm_protocol::render::{DirtyRow, WireCell};

    /// A row whose first cell carries `tag`, so a mis-keyed window is
    /// visible as content rather than only as an index.
    fn row(tag: char) -> DirtyRow {
        DirtyRow {
            row_index: 0,
            cells: vec![WireCell {
                codepoint: tag as u32,
                fg_r: 0,
                fg_g: 0,
                fg_b: 0,
                fg_type: 0,
                bg_r: 0,
                bg_g: 0,
                bg_b: 0,
                bg_type: 0,
                flags: 0,
                extra: vec![],
            }],
            semantic_mark: 0,
            mark_metadata: vec![],
        }
    }

    fn tag_of(row: &DirtyRow) -> char {
        char::from_u32(row.cells[0].codepoint).expect("tag")
    }

    fn rows(tags: &str) -> Vec<DirtyRow> {
        tags.chars().map(row).collect()
    }

    fn served(start_row: i64, tags: &str, has_more: bool) -> ScrollbackData {
        ScrollbackData {
            pane_id: 1,
            start_row,
            has_more,
            total_rows: 1000,
            rows: rows(tags),
        }
    }

    /// Fill the cache with `tags` starting at `start`, bypassing the
    /// request bookkeeping the prefetch tests exercise separately.
    fn filled(state: &mut CopyModeState, start: i64, tags: &str, has_more: bool) {
        let count = u32::try_from(tags.chars().count()).expect("small");
        state.record_fill(
            1,
            FillRequest {
                start_row: start,
                count,
            },
        );
        assert!(state.apply_fill(1, &served(start, tags, has_more)));
    }

    // --- Keying against a clamped window (Spec-0001 served start) ---

    /// The item this cache cannot be built without: a front-clamped
    /// window's rows belong at the start the daemon served, not the one
    /// requested. Keying off the request puts real rows under indices
    /// naming entirely different rows.
    #[test]
    fn a_clamped_window_keys_from_the_served_start() {
        let mut state = CopyModeState::new(80, 4, 0);
        // Asked for rows -20..-16; only three rows of history exist, so
        // the daemon serves -3..0 instead.
        state.record_fill(
            7,
            FillRequest {
                start_row: -20,
                count: 4,
            },
        );
        assert!(state.apply_fill(7, &served(-3, "abc", false)));

        assert_eq!(state.cache().start(), Some(-3));
        assert_eq!(state.cache().end(), Some(0));
        assert_eq!(state.cache().get(-3).map(tag_of), Some('a'));
        assert_eq!(state.cache().get(-1).map(tag_of), Some('c'));

        // The keys the discarded request-keyed scheme would have used.
        assert!(
            state.cache().get(-20).is_none() && state.cache().get(-18).is_none(),
            "request-keyed indices must hold nothing"
        );
    }

    /// Non-vacuity guard for the test above: the two schemes disagree
    /// only because the response's start really does differ from the
    /// request's, so a regression that reverted to echoing would be
    /// caught here rather than passing silently.
    #[test]
    fn request_keying_and_served_keying_disagree_on_a_clamped_window() {
        let request = FillRequest {
            start_row: -20,
            count: 4,
        };
        let response = served(-3, "abc", false);
        assert_ne!(
            response.start_row, request.start_row,
            "the clamp is what makes the two schemes differ"
        );

        let by_request: Vec<i64> = (0..3).map(|i| request.start_row + i).collect();
        let by_response: Vec<i64> = (0..3).map(|i| response.start_row + i).collect();
        assert_eq!(by_request, vec![-20, -19, -18]);
        assert_eq!(by_response, vec![-3, -2, -1]);
    }

    /// An unclamped window keys the same either way, so the change is not
    /// a blanket reinterpretation of every response.
    #[test]
    fn an_unclamped_window_keys_where_it_was_requested() {
        let mut state = CopyModeState::new(80, 4, 0);
        state.record_fill(
            7,
            FillRequest {
                start_row: -3,
                count: 3,
            },
        );
        assert!(state.apply_fill(7, &served(-3, "abc", true)));
        assert_eq!(state.cache().start(), Some(-3));
    }

    // --- Serial correlation ---

    /// Prefetch puts several fills in flight at once, so a reply is
    /// matched by serial. An unmatched serial must be refused rather than
    /// filed at whatever start it happens to carry.
    #[test]
    fn a_reply_with_an_unknown_serial_is_refused() {
        let mut state = CopyModeState::new(80, 4, 0);
        state.record_fill(
            11,
            FillRequest {
                start_row: -8,
                count: 4,
            },
        );

        assert!(!state.apply_fill(12, &served(-8, "abcd", true)));
        assert!(state.cache().is_empty());
        assert!(
            state.is_fill_in_flight(11),
            "the real fill is still pending"
        );
    }

    /// Out-of-order replies land where their own request asked, not where
    /// the most recent one did.
    #[test]
    fn out_of_order_replies_file_against_their_own_request() {
        let mut state = CopyModeState::new(80, 64, 0);
        state.record_fill(
            20,
            FillRequest {
                start_row: -4,
                count: 4,
            },
        );
        state.record_fill(
            21,
            FillRequest {
                start_row: -8,
                count: 4,
            },
        );

        assert!(state.apply_fill(21, &served(-8, "wxyz", true)));
        assert!(state.apply_fill(20, &served(-4, "abcd", true)));

        assert_eq!(state.cache().get(-8).map(tag_of), Some('w'));
        assert_eq!(state.cache().get(-4).map(tag_of), Some('a'));
        assert_eq!(state.cache().len(), 8);
    }

    // --- Failed fills ---

    /// An initial fill that errors must not strand entry: leaving the
    /// serial in flight would keep the pane in copy mode with an empty
    /// cache and no outstanding request to fill it.
    #[test]
    fn an_error_retires_the_initial_fill_and_the_retry_fills_the_cache() {
        let mut state = CopyModeState::new(80, 4, 0);
        let fill = state.initial_fill();
        state.record_fill(7, fill);

        assert_eq!(state.fail_fill(7, true), FillFailure::Retry(fill));

        assert!(!state.is_fill_in_flight(7), "the dead serial is retired");
        state.record_retry(8, fill);
        assert!(state.apply_fill(8, &served(-4, "abcd", true)));
        assert!(!state.cache().is_empty(), "the retry landed rows");
    }

    /// The suppression bug: `plan_prefetch` refuses windows overlapping
    /// anything in flight, so a failed prefetch left in the map mutes
    /// every retry for exactly the region the cursor is moving into.
    #[test]
    fn an_error_retires_a_prefetch_so_the_window_can_be_asked_for_again() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -12, "abcdefghijkl", true);
        state.set_cursor(-11, 0);
        let fill = state.plan_prefetch().expect("boundary reached");
        state.record_fill(30, fill);
        assert_eq!(state.plan_prefetch(), None, "suppressed while in flight");

        assert_eq!(state.fail_fill(30, true), FillFailure::Retry(fill));

        assert_eq!(
            state.plan_prefetch(),
            Some(fill),
            "the window must be requestable again after its fill failed"
        );
    }

    /// One retry, then give up. Retrying forever against a wedged archive
    /// would spin, and a permanent hole in the cache is worse than
    /// leaving copy mode.
    #[test]
    fn a_second_failure_of_the_same_window_abandons_copy_mode() {
        let mut state = CopyModeState::new(80, 4, 0);
        let fill = state.initial_fill();
        state.record_fill(7, fill);

        assert_eq!(state.fail_fill(7, true), FillFailure::Retry(fill));
        state.record_retry(8, fill);

        assert_eq!(state.fail_fill(8, true), FillFailure::Abandon);
        assert!(!state.is_fill_in_flight(8));
    }

    /// A retry cannot fix a pane that no longer exists, so an
    /// unretryable failure abandons on the first error.
    #[test]
    fn an_unretryable_failure_abandons_without_a_retry() {
        let mut state = CopyModeState::new(80, 4, 0);
        state.record_fill(7, state.initial_fill());

        assert_eq!(state.fail_fill(7, false), FillFailure::Abandon);
    }

    /// Errors answering anything else on the connection must not retire
    /// a copy-mode fill that is still legitimately outstanding.
    #[test]
    fn an_error_for_another_request_leaves_copy_mode_fills_alone() {
        let mut state = CopyModeState::new(80, 4, 0);
        let fill = state.initial_fill();
        state.record_fill(7, fill);

        assert_eq!(state.fail_fill(99, true), FillFailure::Unclaimed);
        assert!(state.is_fill_in_flight(7));
    }

    // --- Prefetch boundaries ---

    #[test]
    fn prefetch_fires_at_the_top_quarter_of_the_window() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -12, "abcdefghijkl", true);

        state.set_cursor(-5, 0);
        assert_eq!(state.plan_prefetch(), None, "mid-window is quiet");

        state.set_cursor(-11, 0);
        assert_eq!(
            state.plan_prefetch(),
            Some(FillRequest {
                start_row: -16,
                count: 4
            })
        );
    }

    #[test]
    fn prefetch_fires_at_the_bottom_quarter_of_the_window() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -20, "abcdefgh", true);

        state.set_cursor(-13, 0);
        assert_eq!(
            state.plan_prefetch(),
            Some(FillRequest {
                start_row: -12,
                count: 4
            })
        );
    }

    /// The window the cursor is heading into must be asked for once. A
    /// second plan while the first is unanswered would double the traffic
    /// and race two writes onto the same keys.
    #[test]
    fn prefetch_does_not_duplicate_an_in_flight_window() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -12, "abcdefghijkl", true);
        state.set_cursor(-11, 0);

        let first = state.plan_prefetch().expect("boundary reached");
        state.record_fill(30, first);

        assert_eq!(
            state.plan_prefetch(),
            None,
            "the same window must not be asked for twice"
        );
    }

    /// Suppression is per-window, not a blanket mute: a boundary at the
    /// other end still plans while the first fill is outstanding.
    #[test]
    fn prefetch_at_the_far_boundary_is_not_suppressed() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -20, "abcdefgh", true);
        state.set_cursor(-19, 0);
        let older = state.plan_prefetch().expect("top boundary");
        state.record_fill(31, older);

        state.set_cursor(-13, 0);
        assert_eq!(
            state.plan_prefetch(),
            Some(FillRequest {
                start_row: -12,
                count: 4
            })
        );
    }

    /// `has_more == false` says the window began at the oldest row that
    /// exists, so walking into the top zone must stop asking.
    #[test]
    fn prefetch_stops_at_the_start_of_history() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -8, "abcdefgh", false);

        state.set_cursor(-8, 0);
        assert_eq!(state.plan_prefetch(), None);
    }

    /// A short older chunk is asked for exactly, not rounded past the
    /// oldest row into a window the daemon would only clamp again.
    /// Reached by letting the capacity trim retire the front of a window
    /// whose clamped reply already taught the cache where history begins.
    #[test]
    fn prefetch_shortens_the_last_chunk_before_the_start_of_history() {
        let mut state = CopyModeState::new(80, 4, 0);
        state.record_fill(
            1,
            FillRequest {
                start_row: -40,
                count: 13,
            },
        );
        assert!(state.apply_fill(1, &served(-13, "abcdefghijklm", true)));
        assert_eq!(state.cache().start(), Some(-12), "capacity retired row -13");

        state.set_cursor(-12, 0);
        assert_eq!(
            state.plan_prefetch(),
            Some(FillRequest {
                start_row: -13,
                count: 1
            }),
            "only row -13 remains above the window"
        );
    }

    /// Rows at and past 0 are on the frozen grid, which the client
    /// already holds; the daemon serves none of them.
    #[test]
    fn prefetch_never_reaches_past_the_pinned_viewport_top() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -4, "abcd", true);

        state.set_cursor(-1, 0);
        assert_eq!(state.plan_prefetch(), None);
    }

    // --- Cursor ---

    #[test]
    fn the_cursor_seeds_at_the_bottom_left_of_the_viewport() {
        assert_eq!(CopyModeState::new(80, 24, 0).cursor(), (23, 0));
    }

    /// Entering from scrollback: the daemon pins row 0 at the live grid's
    /// top no matter where the client is scrolled, so a state that
    /// ignored the offset would seed the cursor `offset` rows below
    /// anything on screen — the user would operate on rows they cannot
    /// see. At offset 10 a 24-row pane displays rows -10..=13.
    #[test]
    fn entering_from_scrollback_seeds_the_cursor_on_a_displayed_row() {
        let state = CopyModeState::new(80, 24, 10);
        let (row, col) = state.cursor();

        assert_eq!((row, col), (13, 0), "bottom of the frozen viewport");
        assert!(
            (-10..=13).contains(&row),
            "cursor row {row} is off the frozen viewport"
        );
        assert_ne!(row, 23, "23 is the live grid bottom, ten rows below view");
    }

    /// The scrolled-entry floor is the frozen viewport's top, not 0:
    /// those rows are on screen out of the grid snapshot before any fill
    /// lands, so clamping to 0 would refuse to move the cursor onto rows
    /// the user is already looking at.
    #[test]
    fn entering_from_scrollback_lets_the_cursor_reach_the_viewport_top() {
        let mut state = CopyModeState::new(80, 24, 10);

        state.set_cursor(-10, 0);
        assert_eq!(state.cursor(), (-10, 0));

        state.set_cursor(-11, 0);
        assert_eq!(state.cursor(), (-10, 0), "clamped at the viewport top");
    }

    /// The live grid rows below the frozen viewport still exist in the
    /// daemon's space, so the state must not clamp them away — scrolling
    /// the view down to reach them is the renderer's job (TREK-114).
    #[test]
    fn the_live_grid_below_a_scrolled_viewport_stays_addressable() {
        let mut state = CopyModeState::new(80, 24, 10);
        state.set_cursor(23, 0);
        assert_eq!(state.cursor(), (23, 0));

        state.set_cursor(24, 0);
        assert_eq!(state.cursor(), (23, 0), "one past the last live row");
    }

    /// The clamp spans the cache *and* the frozen viewport: clamping to
    /// cached rows alone would yank the cursor off the live grid it
    /// starts on, since the cache only ever holds negative rows.
    #[test]
    fn the_cursor_clamps_across_the_cache_and_the_viewport() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -6, "abcdef", true);

        state.set_cursor(-99, 2);
        assert_eq!(state.cursor(), (-6, 2), "clamped to the oldest cached row");

        state.set_cursor(99, 0);
        assert_eq!(state.cursor(), (3, 0), "clamped to the viewport bottom");

        state.set_cursor(0, 0);
        assert_eq!(state.cursor(), (0, 0), "the viewport top is addressable");
    }

    /// The column clamp belongs to the state, so a column past the pane
    /// edge cannot survive in the cursor whatever asked for it, and the
    /// bounds motions resolve against report the same width.
    #[test]
    fn the_cursor_column_clamps_to_the_pane_width() {
        let mut state = CopyModeState::new(10, 4, 0);

        state.set_cursor(0, 9);
        assert_eq!(state.cursor(), (0, 9), "the last column is addressable");

        state.set_cursor(0, 99);
        assert_eq!(state.cursor(), (0, 9));
        assert_eq!(state.motion_bounds().cols, 10, "one source of truth");
    }

    /// A degenerate width still leaves one addressable column rather than
    /// underflowing the clamp.
    #[test]
    fn a_zero_width_pane_pins_the_column_at_zero() {
        let mut state = CopyModeState::new(0, 4, 0);
        state.set_cursor(0, 7);
        assert_eq!(state.cursor(), (0, 0));
    }

    /// Before any fill lands there is no scrollback to walk into, so the
    /// cursor stays inside the frozen viewport.
    #[test]
    fn the_cursor_clamps_to_the_viewport_with_an_empty_cache() {
        let mut state = CopyModeState::new(80, 4, 0);
        state.set_cursor(-5, 0);
        assert_eq!(state.cursor(), (0, 0));
    }

    /// Trimming never evicts the cursor's own row, whichever end it sits
    /// at — the property that lets `apply_fill` skip a re-clamp. Both
    /// ends are exercised because the eviction side is chosen by which
    /// end is farther from the cursor.
    #[test]
    fn trimming_never_evicts_the_cursor_row() {
        for cursor in [-12i64, -7, -1] {
            let mut cache = ViewportCache::new(4);
            assert!(cache.insert_window(-12, rows("abcdefghijkl")));

            cache.trim_to_capacity(cursor);

            assert_eq!(cache.len(), 4);
            assert!(
                cache.get(cursor).is_some(),
                "cursor row {cursor} evicted; window is {:?}..{:?}",
                cache.start(),
                cache.end()
            );
        }
    }

    /// A fill that lands must drain its serial. `plan_prefetch` suppresses
    /// windows overlapping anything in flight, so an entry that never
    /// drains permanently mutes refills for the region the cursor is
    /// walking into — the same failure the error path guards against, on
    /// the path that actually runs.
    #[test]
    fn a_successful_fill_drains_its_in_flight_entry() {
        let mut state = CopyModeState::new(80, 4, 0);
        let fill = FillRequest {
            start_row: -12,
            count: 12,
        };
        state.record_fill(7, fill);

        assert!(state.apply_fill(7, &served(-12, "abcdefghijkl", true)));

        assert!(!state.is_fill_in_flight(7), "the answered serial is gone");
        state.set_cursor(-11, 0);
        assert!(
            state.plan_prefetch().is_some(),
            "a drained fill must stop suppressing the next window"
        );
    }

    // --- Selection ---

    /// The anchor is the cursor position at the moment the selection
    /// started, and the cursor is its other end: moving after starting
    /// extends rather than dragging the whole thing.
    #[test]
    fn starting_a_selection_anchors_at_the_cursor_and_extends_with_it() {
        let mut state = CopyModeState::new(80, 8, 0);
        filled(&mut state, -8, "abcdefgh", true);
        state.set_cursor(-4, 2);

        state.toggle_selection(CopySelectionType::Character);
        state.set_cursor(-2, 5);

        assert_eq!(
            state.yank_range(),
            Some(YankRange {
                ty: CopySelectionType::Character,
                start_row: -4,
                start_col: 2,
                end_row: -2,
                end_col: 5,
            })
        );
    }

    /// Toggling the same shape cancels; a different one switches without
    /// moving the anchor, so `v` then `V` selects the same span by line.
    #[test]
    fn toggling_the_same_type_cancels_and_a_different_one_switches() {
        let mut state = CopyModeState::new(80, 8, 0);
        state.set_cursor(1, 3);
        state.toggle_selection(CopySelectionType::Character);
        state.set_cursor(4, 6);

        state.toggle_selection(CopySelectionType::Line);
        let switched = state.yank_range().expect("still selecting");
        assert_eq!(switched.ty, CopySelectionType::Line);
        assert_eq!(
            (switched.start_row, switched.start_col),
            (1, 3),
            "the anchor survives the switch"
        );

        state.toggle_selection(CopySelectionType::Line);
        assert_eq!(state.yank_range(), None, "the same type cancels");
    }

    /// Cycling through all three shapes and back to the first must end
    /// cancelled, not stuck selecting: each toggle either switches or
    /// cancels, never both.
    #[test]
    fn cycling_the_selection_types_ends_where_it_started() {
        let mut state = CopyModeState::new(80, 8, 0);
        state.toggle_selection(CopySelectionType::Character);
        state.toggle_selection(CopySelectionType::Line);
        state.toggle_selection(CopySelectionType::Block);
        assert!(state.yank_range().is_some());

        state.toggle_selection(CopySelectionType::Block);
        assert_eq!(state.yank_range(), None);

        // And the next start anchors afresh at wherever the cursor is.
        state.set_cursor(2, 7);
        state.toggle_selection(CopySelectionType::Character);
        let range = state.yank_range().expect("a fresh selection");
        assert_eq!((range.start_row, range.start_col), (2, 7));
    }

    #[test]
    fn clearing_reports_whether_there_was_a_selection() {
        let mut state = CopyModeState::new(80, 8, 0);
        assert!(!state.clear_selection(), "nothing to clear");
        state.toggle_selection(CopySelectionType::Character);
        assert!(state.clear_selection());
        assert_eq!(state.yank_range(), None);
    }

    /// A selection dragged backwards yanks the same text as one dragged
    /// forwards: character ranges order in reading order.
    #[test]
    fn a_backwards_character_selection_normalizes_in_reading_order() {
        let mut state = CopyModeState::new(80, 8, 0);
        filled(&mut state, -8, "abcdefgh", true);
        state.set_cursor(-2, 5);
        state.toggle_selection(CopySelectionType::Character);
        state.set_cursor(-4, 2);

        assert_eq!(
            state.yank_range(),
            Some(YankRange {
                ty: CopySelectionType::Character,
                start_row: -4,
                start_col: 2,
                end_row: -2,
                end_col: 5,
            })
        );
    }

    /// Within one row the column decides the order, which a row-only
    /// comparison would get backwards.
    #[test]
    fn a_backwards_selection_inside_one_row_orders_by_column() {
        let mut state = CopyModeState::new(80, 8, 0);
        state.set_cursor(3, 9);
        state.toggle_selection(CopySelectionType::Character);
        state.set_cursor(3, 1);

        let range = state.yank_range().expect("selecting");
        assert_eq!((range.start_col, range.end_col), (1, 9));
    }

    /// Line selections share the reading-order path with character ones;
    /// only the block branch departs from it.
    #[test]
    fn a_backwards_line_selection_normalizes_in_reading_order() {
        let mut state = CopyModeState::new(80, 8, 0);
        filled(&mut state, -8, "abcdefgh", true);
        state.set_cursor(-2, 5);
        state.toggle_selection(CopySelectionType::Line);
        state.set_cursor(-4, 2);

        assert_eq!(
            state.yank_range(),
            Some(YankRange {
                ty: CopySelectionType::Line,
                start_row: -4,
                start_col: 2,
                end_row: -2,
                end_col: 5,
            })
        );
    }

    /// A block normalizes each axis on its own, so dragging up-and-right
    /// yields the same rectangle as down-and-left. Reading-order
    /// normalization would swap both endpoints together and invert the
    /// column range.
    #[test]
    fn a_block_selection_normalizes_each_axis_independently() {
        let mut state = CopyModeState::new(80, 8, 0);
        filled(&mut state, -8, "abcdefgh", true);
        state.set_cursor(-2, 9);
        state.toggle_selection(CopySelectionType::Block);
        state.set_cursor(-5, 3);

        assert_eq!(
            state.yank_range(),
            Some(YankRange {
                ty: CopySelectionType::Block,
                start_row: -5,
                start_col: 3,
                end_row: -2,
                end_col: 9,
            })
        );
    }

    /// A selection that never moved is a single cell, not an empty one:
    /// both endpoints are inclusive.
    #[test]
    fn an_unmoved_selection_covers_the_anchor_cell() {
        let mut state = CopyModeState::new(80, 8, 0);
        state.set_cursor(2, 4);
        state.toggle_selection(CopySelectionType::Character);

        assert_eq!(
            state.yank_range(),
            Some(YankRange {
                ty: CopySelectionType::Character,
                start_row: 2,
                start_col: 4,
                end_row: 2,
                end_col: 4,
            })
        );
    }

    // --- Cached row text ---

    #[test]
    fn cached_rows_read_out_as_characters_with_blanks_for_empty_cells() {
        let mut cache = ViewportCache::new(8);
        let mut blank = row('x');
        blank.cells[0].codepoint = 0;
        cache.insert_window(-2, vec![row('h'), blank]);

        assert_eq!(cache.row_chars(-2), Some(vec!['h']));
        assert_eq!(cache.row_chars(-1), Some(vec![' ']));
        assert_eq!(cache.row_chars(-99), None, "an unserved row has no text");
    }

    // --- Window overlap ---

    /// Suppression is by overlap, not equality: a prefetch that merely
    /// intersects an outstanding window would race two writes onto the
    /// same keys.
    #[test]
    fn overlap_covers_every_way_two_windows_can_touch() {
        let base = FillRequest {
            start_row: -10,
            count: 5,
        };
        let case = |start_row, count| FillRequest { start_row, count };

        assert!(base.overlaps(base), "identical");
        assert!(base.overlaps(case(-8, 2)), "contained");
        assert!(case(-12, 20).overlaps(base), "containing");
        assert!(base.overlaps(case(-12, 4)), "partial from below");
        assert!(base.overlaps(case(-7, 4)), "partial from above");

        assert!(!base.overlaps(case(-15, 5)), "abutting below, disjoint");
        assert!(!base.overlaps(case(-5, 5)), "abutting above, disjoint");
        assert!(!base.overlaps(case(-40, 2)), "far below");
        assert!(!base.overlaps(case(40, 2)), "far above");
    }

    /// Overlap is symmetric; a one-sided test would let suppression
    /// depend on which window happened to be recorded first.
    #[test]
    fn overlap_is_symmetric() {
        let a = FillRequest {
            start_row: -10,
            count: 5,
        };
        let b = FillRequest {
            start_row: -7,
            count: 4,
        };
        assert_eq!(a.overlaps(b), b.overlaps(a));

        let far = FillRequest {
            start_row: 100,
            count: 1,
        };
        assert_eq!(a.overlaps(far), far.overlaps(a));
    }

    /// A zero-row window touches nothing, so it can neither suppress nor
    /// be suppressed.
    #[test]
    fn an_empty_window_overlaps_nothing() {
        let empty = FillRequest {
            start_row: -10,
            count: 0,
        };
        let real = FillRequest {
            start_row: -12,
            count: 5,
        };
        assert!(!empty.overlaps(real));
        assert!(!real.overlaps(empty));
    }

    // --- Cache window ---

    #[test]
    fn the_window_is_derived_from_the_rows_it_holds() {
        let mut cache = ViewportCache::new(16);
        assert_eq!((cache.start(), cache.end()), (None, None));

        cache.insert_window(-4, rows("abcd"));
        assert_eq!((cache.start(), cache.end()), (Some(-4), Some(0)));
        assert_eq!(cache.len(), 4);
    }

    #[test]
    fn an_adjacent_window_extends_the_cache() {
        let mut cache = ViewportCache::new(16);
        cache.insert_window(-4, rows("abcd"));
        cache.insert_window(-8, rows("wxyz"));

        assert_eq!((cache.start(), cache.end()), (Some(-8), Some(0)));
        assert_eq!(cache.get(-8).map(tag_of), Some('w'));
        assert_eq!(cache.get(-4).map(tag_of), Some('a'));
    }

    /// A window abutting neither end is a reply a trim moved out from
    /// under while it was in flight. Installing it would leave a hole
    /// under the derived bounds; CLEARING to make room would throw away
    /// the rows the cursor is sitting in and strand it against a window
    /// far away. Dropping it keeps the cache usable, and prefetch
    /// re-issues the window if it is still wanted.
    #[test]
    fn a_disjoint_window_is_discarded_rather_than_replacing_the_cache() {
        let mut cache = ViewportCache::new(16);
        assert!(cache.insert_window(-4, rows("abcd")));

        assert!(!cache.insert_window(-40, rows("wxyz")));

        assert_eq!((cache.start(), cache.end()), (Some(-4), Some(0)));
        assert_eq!(cache.len(), 4, "the cursor's rows survive");
        assert_eq!(cache.get(-4).map(tag_of), Some('a'));
        assert!(cache.get(-40).is_none(), "the stale window is not filed");
    }

    /// The whole point of dropping rather than clearing: the cursor is
    /// still on a cached row afterwards, so a yank reads what the user
    /// pointed at. `set_cursor` is a range clamp and would not have
    /// caught this — `first_row`/`last_row` span the ends without
    /// requiring the rows between them to exist.
    #[test]
    fn a_stale_reply_leaves_the_cursor_on_a_cached_row() {
        let mut state = CopyModeState::new(80, 4, 0);
        filled(&mut state, -8, "abcdefgh", true);
        state.set_cursor(-5, 0);

        state.record_fill(
            9,
            FillRequest {
                start_row: -40,
                count: 4,
            },
        );
        assert!(state.apply_fill(9, &served(-40, "wxyz", true)));

        let (row, _) = state.cursor();
        assert_eq!(row, -5, "the cursor did not move");
        assert!(
            state.cache().get(row).is_some(),
            "cursor row {row} lost its cached row"
        );
    }

    #[test]
    fn an_overlapping_window_overwrites_the_shared_rows() {
        let mut cache = ViewportCache::new(16);
        cache.insert_window(-4, rows("abcd"));
        cache.insert_window(-2, rows("YZ"));

        assert_eq!(cache.get(-2).map(tag_of), Some('Y'));
        assert_eq!(cache.len(), 4);
    }

    #[test]
    fn trimming_keeps_the_rows_nearest_the_cursor() {
        let mut cache = ViewportCache::new(4);
        cache.insert_window(-8, rows("abcdefgh"));

        cache.trim_to_capacity(-8);
        assert_eq!(cache.len(), 4);
        assert_eq!((cache.start(), cache.end()), (Some(-8), Some(-4)));
    }

    #[test]
    fn trimming_from_the_other_side_keeps_the_newer_rows() {
        let mut cache = ViewportCache::new(4);
        cache.insert_window(-8, rows("abcdefgh"));

        cache.trim_to_capacity(-1);
        assert_eq!((cache.start(), cache.end()), (Some(-4), Some(0)));
    }

    #[test]
    fn an_empty_window_leaves_the_cache_untouched() {
        let mut cache = ViewportCache::new(4);
        cache.insert_window(-4, rows("ab"));
        cache.insert_window(-9, vec![]);

        assert_eq!((cache.start(), cache.end()), (Some(-4), Some(-2)));
    }

    /// Entering live, everything below the viewport top is grid, so the
    /// window is the one screen of scrollback above it.
    #[test]
    fn the_entry_fill_stops_at_the_pinned_grid_top() {
        let state = CopyModeState::new(80, 24, 0);
        assert_eq!(
            state.initial_fill(),
            FillRequest {
                start_row: -24,
                count: 24
            }
        );
    }

    /// Scrolled entry: the window still starts one screen above the
    /// frozen page and still stops at row 0, so it covers the scrollback
    /// half of what is on screen without reaching into the grid.
    #[test]
    fn the_entry_fill_from_scrollback_covers_the_page_up_to_row_zero() {
        let fill = CopyModeState::new(80, 24, 10).initial_fill();

        assert_eq!(
            fill,
            FillRequest {
                start_row: -34,
                count: 34
            }
        );
        assert_eq!(fill.end_row(), 0);
    }

    /// Deep in scrollback the cache capacity binds before row 0 does, and
    /// the three screens land centred on the frozen page.
    #[test]
    fn a_deep_entry_fill_is_bounded_by_the_cache_capacity() {
        let fill = CopyModeState::new(80, 24, 100).initial_fill();

        assert_eq!(
            fill,
            FillRequest {
                start_row: -124,
                count: 72
            }
        );
        assert!(
            fill.start_row <= -100 && fill.end_row() >= -76,
            "window {}..{} misses the visible page",
            fill.start_row,
            fill.end_row()
        );
    }

    /// The invariant behind the cap, over every entry offset: the cache
    /// holds scrollback, and the frozen snapshot owns everything else.
    #[test]
    fn no_entry_fill_ever_reaches_a_grid_row() {
        for offset in [0u32, 1, 5, 23, 24, 25, 100, 10_000] {
            let fill = CopyModeState::new(80, 24, offset).initial_fill();
            assert!(
                fill.end_row() <= 0,
                "offset {offset} asks for grid row {}",
                fill.end_row() - 1
            );
        }
    }

    /// The trigger the cap exists for: output between the pin and the
    /// entry request moves grid rows into history, so the daemon would
    /// happily serve them. Every row it returns for the capped window
    /// still lands at a negative key.
    #[test]
    fn an_entry_fill_after_output_caches_no_grid_rows() {
        let mut state = CopyModeState::new(80, 4, 0);
        let fill = state.initial_fill();
        state.record_fill(1, fill);

        // History has advanced past the pin, so the whole window exists.
        let count = usize::try_from(fill.count).expect("small");
        let rows: String = std::iter::repeat_n('x', count).collect();
        assert!(state.apply_fill(1, &served(fill.start_row, &rows, true)));

        assert!(!state.cache().is_empty(), "the fill landed");
        assert!(
            state.cache().end().is_some_and(|end| end <= 0),
            "cached a grid row: window ends at {:?}",
            state.cache().end()
        );
    }

    /// A pane taller than the daemon's per-request cap must not ask for
    /// more than one response can carry.
    #[test]
    fn the_entry_fill_respects_the_per_request_row_cap() {
        let state = CopyModeState::new(80, u16::MAX, 0);
        assert_eq!(state.initial_fill().count, super::MAX_ROWS_PER_REQUEST);
        assert_eq!(
            CopyModeState::new(80, u16::MAX, u32::MAX)
                .initial_fill()
                .count,
            super::MAX_ROWS_PER_REQUEST
        );
    }
}
