//! Copy-mode client state (Spec-0008): the cursor and the viewport cache
//! that backs it, in the row space the daemon pins on `EnterCopyMode`.
//!
//! Row 0 is the daemon history end the client echoed at entry
//! (`EnterCopyMode.base`, ADR-0025) — the top of the live grid at the
//! instant the painted content was resolved — and negatives run into
//! scrollback, matching `GetScrollback.start_row` and `YankSelection`
//! rows. Painted-page rows resolve from the entry snapshot (ADR-0025
//! clause 6), so the cache only ever holds negative rows: `GetScrollback`
//! never serves the live grid. A pane entered from scrollback shows rows
//! `[-offset, -offset + visible_rows)`, which is why `viewport_top`
//! exists rather than everything assuming 0.

use crate::copy_keys::PendingPrefix;
use crate::copy_motion::{MotionBounds, RowText};
use crate::copy_search::{self, BarKey, BarStyle, Match, SearchDirection, SearchState};
use crate::render_grid::RowSource;
use oakterm_protocol::message::ScrollbackData;
use oakterm_protocol::render::DirtyRow;
use oakterm_terminal::grid::selection::{AnchorSide, Selection, SelectionType};
use std::collections::{BTreeMap, HashMap};
use tracing::warn;

/// Spec-0008's `CopySelectionType` is the wire enum: the client's shapes
/// and the daemon's are the same three by definition, and a second enum
/// beside it would only add a conversion that can drift.
pub(crate) use oakterm_protocol::message::CopySelectionType;

/// What a motion does to the selection, carried on the command so the
/// preset tables own the policy and the dispatcher stays exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionEffect {
    Keep,
    /// Anchor a character selection at the cursor if none is active.
    Extend,
    Clear,
}

/// Per-request row cap the daemon enforces (Spec-0001 `GetScrollback`).
const MAX_ROWS_PER_REQUEST: u32 = 4096;

/// Rows per search chunk. The daemon caps rows, not bytes, and history
/// rows keep their old width, so this budgets 256 columns or more.
fn scan_chunk_rows(cols: u16) -> u32 {
    let row_bytes = u32::from(cols.max(256))
        * u32::try_from(oakterm_protocol::render::WireCell::FIXED_SIZE).unwrap_or(u32::MAX);
    (oakterm_protocol::frame::MAX_PAYLOAD / 2 / row_bytes).clamp(1, MAX_ROWS_PER_REQUEST)
}

/// Cache window as a multiple of the visible rows (Spec-0008 default).
const CACHE_ROWS_PER_SCREEN: u32 = 3;
// The window-centred trim in `apply_fill` keeps the whole display window
// cached only from two screens up.
const _: () = assert!(CACHE_ROWS_PER_SCREEN >= 2);

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

    pub(crate) fn get(&self, row: i64) -> Option<&DirtyRow> {
        self.rows.get(&row)
    }

    /// The row's cells as characters, empty cells reading as blanks —
    /// the form word motions scan. `None` for an unserved row.
    pub(crate) fn row_chars(&self, row: i64) -> Option<Vec<char>> {
        self.rows.get(&row).map(dirty_row_chars)
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

/// How a yank resolves under the ADR-0025 ownership split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum YankPlan {
    /// Wholly below 0: one daemon round trip, as before.
    Daemon(YankRange),
    /// Wholly on the painted page: extracted locally, no round trip.
    Local(String),
    /// Spans the boundary: the daemon serves rows below 0, the local
    /// half is appended after a `\n` when the response lands.
    Stitched { daemon: YankRange, local: String },
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
    /// Top row of the display window (Spec-0008 Display): follows the
    /// cursor, never below `viewport_top`.
    display_top: i64,
    /// Fills awaiting a reply, by request serial.
    in_flight: HashMap<u32, PendingFill>,
    /// Oldest row the daemon has shown it can serve, learned when a
    /// window comes back front-clamped or reports no older history.
    history_start: Option<i64>,
    /// The painted viewport cells at entry (ADR-0025 clause 6): a
    /// dedicated copy, not the still-mutating saved live snapshot. Row
    /// `i` is pin-space row `viewport_top + i`. Rows at or above 0 —
    /// and every painted-page row — resolve here, never from the daemon.
    snapshot: Vec<Vec<char>>,
    /// The base the daemon acked (ADR-0025 clause 2), set when the ack
    /// lands; fills served against any other base are discarded.
    pinned_base: Option<u64>,
    /// The current search, kept after its bar closes (Spec-0008 Search).
    search: Option<SearchState>,
    scan: Option<Scan>,
    seek: Option<Seek>,
    /// The step a landed scan or seek fill produced, for the caller to
    /// collect with [`Self::continue_search`].
    landed: Option<SearchStep>,
}

/// A search reading history the cache lacks: it fetches `awaiting` and
/// scans the chunk without caching it, starting at `from` (inclusive or
/// not) in `direction`. The cursor stays put until a match is found.
#[derive(Debug, Clone, Copy)]
struct Scan {
    direction: SearchDirection,
    from: (i64, u16),
    inclusive: bool,
    /// Whether a miss rings: `n`/`N` do, typing does not.
    ring: bool,
    awaiting: FillRequest,
}

/// A cursor move to a row the cache no longer holds, as when `Escape`
/// returns to a search's origin: the cache is replaced around it.
#[derive(Debug, Clone, Copy)]
struct Seek {
    target: (i64, u16),
    awaiting: FillRequest,
}

/// A scan chunk's text, read as [`RowText`] without entering the cache.
struct Chunk {
    start: i64,
    rows: Vec<Vec<char>>,
}

impl Chunk {
    fn new(data: &ScrollbackData) -> Self {
        Self {
            start: data.start_row,
            rows: data.rows.iter().map(dirty_row_chars).collect(),
        }
    }

    /// One past the chunk's last row.
    fn end(&self) -> i64 {
        self.start + i64::try_from(self.rows.len()).unwrap_or(i64::MAX)
    }
}

impl RowText for Chunk {
    fn row(&self, row: i64) -> Option<Vec<char>> {
        let index = usize::try_from(row.checked_sub(self.start)?).ok()?;
        self.rows.get(index).cloned()
    }
}

/// A served row's cells as characters, empty cells reading as blanks.
fn dirty_row_chars(dirty: &DirtyRow) -> Vec<char> {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchStep {
    Moved,
    /// Send this fill; the step resumes when it lands.
    Fetch(FillRequest),
    NotFound {
        ring: bool,
    },
    /// The bar, the query or the cursor changed with no match to report.
    Updated,
    /// The find bar closed with Escape: leave copy mode.
    Exit,
    Inert,
}

/// What the search bar shows and a screen reader hears; counts cover
/// the held rows only, `more` saying history remains in the direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchStatus {
    pub(crate) query: String,
    pub(crate) direction: SearchDirection,
    pub(crate) style: BarStyle,
    pub(crate) editing: bool,
    pub(crate) index: Option<usize>,
    pub(crate) total: usize,
    pub(crate) more: bool,
}

impl CopyModeState {
    /// Seed at the bottom-left of the frozen viewport (Spec-0008 entry).
    /// `viewport_offset` is the pane's scroll offset at entry; entering
    /// live (0) seeds at `rows - 1` as the spec describes.
    pub(crate) fn new(
        cols: u16,
        visible_rows: u16,
        viewport_offset: u32,
        snapshot: Vec<Vec<char>>,
    ) -> Self {
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
            display_top: viewport_top,
            in_flight: HashMap::new(),
            history_start: None,
            snapshot,
            pinned_base: None,
            search: None,
            scan: None,
            seek: None,
            landed: None,
        }
    }

    /// Record the base the daemon acked; later fills must match it.
    pub(crate) fn set_pinned_base(&mut self, base: u64) {
        self.pinned_base = Some(base);
    }

    /// A painted-page row from the entry snapshot, by pin-space index.
    /// `None` outside the page or past what was captured.
    pub(crate) fn snapshot_row(&self, row: i64) -> Option<&[char]> {
        let idx = usize::try_from(row.checked_sub(self.viewport_top)?).ok()?;
        self.snapshot.get(idx).map(Vec::as_slice)
    }

    #[cfg(test)]
    pub(crate) fn set_snapshot(&mut self, snapshot: Vec<Vec<char>>) {
        self.snapshot = snapshot;
    }
}

/// Motions resolve against the daemon-served cache for rows below the
/// painted page and the entry snapshot on it (ADR-0025 clause 6) —
/// cache first, because rows below 0 are daemon-authoritative even when
/// the page also shows them.
impl RowText for CopyModeState {
    fn row(&self, row: i64) -> Option<Vec<char>> {
        if let Some(chars) = self.cache.row_chars(row) {
            return Some(chars);
        }
        self.snapshot_row(row).map(<[char]>::to_vec)
    }
}

impl CopyModeState {
    pub(crate) fn cursor(&self) -> (i64, u16) {
        (self.cursor_row, self.cursor_col)
    }

    #[cfg(test)]
    pub(crate) fn cache(&self) -> &ViewportCache {
        &self.cache
    }

    /// Daemon row shown at the top of the frozen viewport.
    #[cfg(test)]
    pub(crate) fn viewport_top(&self) -> i64 {
        self.viewport_top
    }

    /// The newest row copy mode can address: the bottom of the painted
    /// page (ADR-0025 clause 6). Rows at or above 0 resolve from the
    /// entry snapshot, so nothing below the page is addressable — a page
    /// entirely in scrollback puts this below 0 by the same arithmetic.
    fn last_row(&self) -> i64 {
        self.viewport_top + i64::from(self.visible_rows) - 1
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
        let rows = i64::from(self.visible_rows);
        if self.cursor_row < self.display_top {
            self.display_top = self.cursor_row;
        } else if self.cursor_row >= self.display_top + rows {
            self.display_top = (self.cursor_row - rows + 1).min(self.viewport_top);
        }
    }

    pub(crate) fn display_top(&self) -> i64 {
        self.display_top
    }

    /// Scroll the display window by `delta` rows (negative is up, into
    /// history), bounded by the addressable rows, then pull the cursor
    /// into the window it leaves on screen (Spec-0008 Mouse).
    pub(crate) fn scroll_display(&mut self, delta: i64) {
        let top = self.display_top.saturating_add(delta);
        self.display_top = top.clamp(self.first_row(), self.viewport_top);
        let bottom = self.display_top + i64::from(self.visible_rows) - 1;
        self.cursor_row = self.cursor_row.clamp(self.display_top, bottom);
    }

    /// The cache wins over the page, as in [`RowText`]: rows below 0 are
    /// daemon-authoritative, and the page can carry the scroll indicator.
    pub(crate) fn display_row(&self, visible: u16) -> RowSource<'_> {
        let row = self.display_top + i64::from(visible);
        if let Some(cached) = self.cache.get(row) {
            return RowSource::Wire(cached);
        }
        u16::try_from(row - self.viewport_top).map_or(RowSource::Blank, RowSource::Own)
    }

    pub(crate) fn display_cursor(&self) -> Option<(u16, u16)> {
        let row = u16::try_from(self.cursor_row - self.display_top).ok()?;
        (row < self.visible_rows).then_some((self.cursor_col, row))
    }

    /// Put the cursor on `cursor` and, given an anchor, select characters
    /// from it, both `(window row, col)` cells of a window whose top row is
    /// `window_top`; no anchor leaves a caret.
    pub(crate) fn select_in_window(
        &mut self,
        window_top: i64,
        cursor: (u16, u16),
        anchor: Option<(u16, u16)>,
    ) {
        self.selection = None;
        let row = |r: u16| window_top + i64::from(r);
        let cursor_row = row(cursor.0);
        if let Some(anchor) = anchor {
            self.set_cursor(row(anchor.0), anchor.1);
            self.anchor_selection(CopySelectionType::Character);
        }
        self.set_cursor(cursor_row, cursor.1);
    }

    /// The selection as a Spec-0003 `Selection` in display coordinates,
    /// covering exactly the cells a yank would take. It runs from the
    /// anchor to the cursor, with sides that make both cells inclusive, so
    /// the a11y caret lands on the cursor whichever way it moved. Rows may
    /// fall outside the window; only the visible ones paint.
    pub(crate) fn display_selection(&self) -> Option<Selection> {
        let selection = self.selection?;
        let ty = match selection.ty {
            CopySelectionType::Character => SelectionType::Normal,
            CopySelectionType::Line => SelectionType::Line,
            CopySelectionType::Block => SelectionType::Block,
        };
        let anchor = (
            selection.anchor_row - self.display_top,
            selection.anchor_col,
        );
        let cursor = (self.cursor_row - self.display_top, self.cursor_col);
        let (anchor_side, cursor_side) = if anchor <= cursor {
            (AnchorSide::Left, AnchorSide::Right)
        } else {
            (AnchorSide::Right, AnchorSide::Left)
        };
        let mut display = Selection::new(ty, anchor.0, anchor.1, anchor_side);
        display.update(cursor.0, cursor.1, cursor_side);
        Some(display)
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
            None => self.anchor_selection(ty),
        }
    }

    /// Apply a motion's selection effect before the cursor moves.
    pub(crate) fn apply_selection_effect(&mut self, effect: SelectionEffect) {
        match effect {
            SelectionEffect::Keep => {}
            SelectionEffect::Extend => {
                if self.selection.is_none() {
                    self.anchor_selection(CopySelectionType::Character);
                }
            }
            SelectionEffect::Clear => self.selection = None,
        }
    }

    fn anchor_selection(&mut self, ty: CopySelectionType) {
        self.selection = Some(CopySelection {
            ty,
            anchor_row: self.cursor_row,
            anchor_col: self.cursor_col,
        });
    }

    /// Open the search bar for a new search (Spec-0008 Search).
    pub(crate) fn start_search(&mut self, direction: SearchDirection, style: BarStyle) {
        self.search = Some(SearchState::new(direction, style, self.cursor()));
        self.scan = None;
        self.seek = None;
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn search_editing(&self) -> bool {
        self.open_bar().is_some()
    }

    /// The style of the bar taking keys, if one is open.
    pub(crate) fn open_bar(&self) -> Option<BarStyle> {
        self.search
            .as_ref()
            .filter(|search| search.editing)
            .map(|search| search.style)
    }

    /// Apply one key typed into the open bar.
    pub(crate) fn edit_search(&mut self, key: BarKey) -> SearchStep {
        let Some(search) = &mut self.search else {
            return SearchStep::Inert;
        };
        match key {
            BarKey::Insert(c) => {
                search.push(c);
                self.search_from_origin()
            }
            BarKey::Backspace => {
                search.pop();
                self.search_from_origin()
            }
            // A scan still running finishes: Enter accepts what it finds.
            BarKey::Accept => {
                search.editing = false;
                SearchStep::Updated
            }
            BarKey::Cancel => {
                search.editing = false;
                let origin = search.origin;
                self.scan = None;
                self.go_to(origin)
            }
            BarKey::Repeat(direction) => {
                search.direction = direction;
                self.search_on(direction)
            }
            BarKey::Next { reverse } => {
                let direction = if reverse {
                    search.direction.reversed()
                } else {
                    search.direction
                };
                self.search_on(direction)
            }
            BarKey::Leave => {
                search.editing = false;
                self.scan = None;
                self.seek = None;
                SearchStep::Exit
            }
            BarKey::Ignore => SearchStep::Inert,
        }
    }

    /// `n` (or `N`, `reverse`): the next match of the last search.
    pub(crate) fn search_again(&mut self, reverse: bool) -> SearchStep {
        let Some(direction) = self
            .search
            .as_ref()
            .filter(|search| search.engine().is_some())
            .map(|search| search.direction)
        else {
            return SearchStep::Inert;
        };
        let direction = if reverse {
            direction.reversed()
        } else {
            direction
        };
        self.search_on(direction)
    }

    /// The step a scan or seek fill produced when it landed, once.
    pub(crate) fn continue_search(&mut self) -> SearchStep {
        self.landed.take().unwrap_or(SearchStep::Inert)
    }

    /// An edited query jumps from where the search started, staying on a
    /// match that still fits; an emptied one returns there.
    fn search_from_origin(&mut self) -> SearchStep {
        let Some(search) = &self.search else {
            return SearchStep::Inert;
        };
        let (origin, direction) = (search.origin, search.direction);
        if search.engine().is_none() {
            self.scan = None;
            return self.go_to(origin);
        }
        // The find bar reads its whole starting row, so a match on the
        // bottom line is found going up (Spec-0008 Search, Find).
        let from = match search.style {
            BarStyle::Preset => origin,
            BarStyle::Find => (origin.0, u16::MAX),
        };
        self.scan_from(from, direction, true, false)
    }

    fn search_on(&mut self, direction: SearchDirection) -> SearchStep {
        let cursor = self.cursor();
        self.scan_from(cursor, direction, false, true)
    }

    /// Scan the held rows from `from`; past them, ask for the next chunk
    /// of history and scan it when it lands, leaving the cursor put.
    fn scan_from(
        &mut self,
        from: (i64, u16),
        direction: SearchDirection,
        inclusive: bool,
        ring: bool,
    ) -> SearchStep {
        self.scan = None;
        self.seek = None;
        let Some(engine) = self.search.as_ref().and_then(SearchState::engine) else {
            return SearchStep::Inert;
        };
        let bounds = (self.history_start.unwrap_or(i64::MIN), self.last_row());
        match copy_search::find(&*self, engine, from, direction, inclusive, bounds) {
            copy_search::Find::Found(hit) => {
                self.set_cursor(hit.row, hit.start);
                SearchStep::Moved
            }
            copy_search::Find::Exhausted => SearchStep::NotFound { ring },
            copy_search::Find::Missing(row) => {
                // The starting row keeps its column; a later row is read
                // whole from its near edge.
                let from = if row == from.0 {
                    (from, inclusive)
                } else {
                    let edge = match direction {
                        SearchDirection::Forward => 0,
                        SearchDirection::Backward => u16::MAX,
                    };
                    ((row, edge), true)
                };
                let awaiting = self.scan_chunk(row, direction);
                self.scan = Some(Scan {
                    direction,
                    from: from.0,
                    inclusive: from.1,
                    ring,
                    awaiting,
                });
                self.fetch(awaiting)
            }
        }
    }

    /// Ask for a scan or seek window, or hold it until the pin lands: a
    /// fill before the ack would resolve against live rows (ADR-0025
    /// clause 2). The pin handler sends the held window via `awaited_fill`.
    fn fetch(&self, awaiting: FillRequest) -> SearchStep {
        if self.pinned_base.is_some() {
            SearchStep::Fetch(awaiting)
        } else {
            SearchStep::Updated
        }
    }

    /// The window a scan or seek is waiting on.
    pub(crate) fn awaited_fill(&self) -> Option<FillRequest> {
        self.scan
            .map(|scan| scan.awaiting)
            .or(self.seek.map(|seek| seek.awaiting))
    }

    /// The largest window toward `direction` that starts at `row`: a long
    /// scan costs one round trip per chunk (Spec-0008 Search latency).
    fn scan_chunk(&self, row: i64, direction: SearchDirection) -> FillRequest {
        let chunk = i64::from(scan_chunk_rows(self.cols));
        let (start_row, count) = match direction {
            SearchDirection::Backward => {
                let floor = self.history_start.unwrap_or(i64::MIN);
                let start = (row - chunk + 1).max(floor);
                (start, row - start + 1)
            }
            // The page reads from the entry snapshot, so a forward chunk
            // stops at its top.
            SearchDirection::Forward => (row, chunk.min(self.viewport_top.min(0) - row).max(1)),
        };
        FillRequest {
            start_row,
            count: u32::try_from(count).unwrap_or(MAX_ROWS_PER_REQUEST),
        }
    }

    /// Move the cursor to `target`, fetching the window around it first
    /// when neither the cache nor the page holds that row.
    fn go_to(&mut self, target: (i64, u16)) -> SearchStep {
        self.seek = None;
        if target.0 >= self.viewport_top || self.cache.get(target.0).is_some() {
            self.set_cursor(target.0, target.1);
            return SearchStep::Updated;
        }
        let screen = i64::from(self.visible_rows);
        let floor = self.history_start.unwrap_or(i64::MIN);
        let start_row = (target.0 - screen).max(floor);
        let count = (i64::from(CACHE_ROWS_PER_SCREEN) * screen).min(-start_row);
        let awaiting = FillRequest {
            start_row,
            count: u32::try_from(count).unwrap_or(MAX_ROWS_PER_REQUEST),
        };
        self.seek = Some(Seek { target, awaiting });
        self.fetch(awaiting)
    }

    /// A scan chunk landed: scan it, and on a match make it the cache so
    /// the cursor can stand there; otherwise go on past it.
    fn scan_landed(&mut self, data: &ScrollbackData) -> SearchStep {
        let Some(scan) = self.scan.take() else {
            return SearchStep::Inert;
        };
        let Some(engine) = self.search.as_ref().and_then(SearchState::engine) else {
            return SearchStep::Inert;
        };
        let chunk = Chunk::new(data);
        let served_end = chunk.end();
        // A reply may run past the frozen page's last row.
        let bounds = (data.start_row, (served_end - 1).min(self.last_row()));
        match copy_search::find(
            &chunk,
            engine,
            scan.from,
            scan.direction,
            scan.inclusive,
            bounds,
        ) {
            copy_search::Find::Found(hit) => {
                self.adopt_window(data, (hit.row, hit.start));
                SearchStep::Moved
            }
            copy_search::Find::Missing(_) | copy_search::Find::Exhausted => match scan.direction {
                SearchDirection::Backward
                    if self
                        .history_start
                        .is_some_and(|start| data.start_row <= start) =>
                {
                    SearchStep::NotFound { ring: scan.ring }
                }
                SearchDirection::Backward => self.scan_from(
                    (data.start_row - 1, u16::MAX),
                    scan.direction,
                    true,
                    scan.ring,
                ),
                SearchDirection::Forward => {
                    self.scan_from((served_end, 0), scan.direction, true, scan.ring)
                }
            },
        }
    }

    fn seek_landed(&mut self, data: &ScrollbackData) -> SearchStep {
        let Some(seek) = self.seek.take() else {
            return SearchStep::Inert;
        };
        self.adopt_window(data, seek.target);
        SearchStep::Updated
    }

    /// Take a scan or seek window into the cache, merging when it touches
    /// the held rows and replacing them when it does not, and put the
    /// cursor at `at`.
    fn adopt_window(&mut self, data: &ScrollbackData, at: (i64, u16)) {
        if !self.cache.insert_window(data.start_row, data.rows.clone()) {
            self.cache.rows.clear();
            self.cache.insert_window(data.start_row, data.rows.clone());
        }
        self.set_cursor(at.0, at.1);
        let rows = i64::from(self.visible_rows);
        self.cache.trim_to_capacity(self.display_top + rows / 2);
        self.display_top = self.display_top.clamp(self.first_row(), self.viewport_top);
    }

    pub(crate) fn search_status(&self) -> Option<SearchStatus> {
        let search = self.search.as_ref()?;
        let (index, total) = search.engine().map_or((None, 0), |engine| {
            // The cached rows above the page, then the page: a cache that
            // jumped far up leaves a gap not worth walking every frame.
            let cached = self
                .cache
                .rows
                .keys()
                .copied()
                .take_while(|&row| row < self.viewport_top);
            let held = copy_search::held_matches(
                self,
                engine,
                cached.chain(self.viewport_top..=self.last_row()),
            );
            let cursor = self.cursor();
            let index = held.iter().position(|hit| (hit.row, hit.start) == cursor);
            (index, held.len())
        });
        let scanning = self
            .scan
            .is_some_and(|scan| scan.direction == search.direction);
        let more = scanning
            || match search.direction {
                SearchDirection::Backward => self
                    .history_start
                    .is_none_or(|start| self.first_row() > start),
                SearchDirection::Forward => self
                    .cache
                    .end()
                    .is_some_and(|end| end < self.viewport_top.min(0)),
            };
        Some(SearchStatus {
            query: search.query().to_string(),
            direction: search.direction,
            style: search.style,
            editing: search.editing,
            index,
            total,
            more,
        })
    }

    /// Matches on the display window's rows, with rows window-relative.
    pub(crate) fn display_matches(&self) -> Vec<Match> {
        let Some(engine) = self.search.as_ref().and_then(SearchState::engine) else {
            return Vec::new();
        };
        (0..self.visible_rows)
            .filter_map(|visible| {
                let row = self.display_top + i64::from(visible);
                let chars = RowText::row(self, row)?;
                Some(
                    copy_search::row_matches(engine, row, &chars)
                        .into_iter()
                        .map(move |hit| Match {
                            row: i64::from(visible),
                            ..hit
                        }),
                )
            })
            .flatten()
            .collect()
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

    /// Where the selection's text comes from (ADR-0025 clause 7): rows
    /// below 0 are daemon-served, rows at or above 0 come from the entry
    /// snapshot, and a selection spanning the boundary splits at −1/0 —
    /// the daemon half open to the row edge for character and line
    /// shapes, the true end column applied on the local half.
    pub(crate) fn yank_plan(&self) -> Option<YankPlan> {
        let range = self.yank_range()?;
        if range.end_row < 0 {
            return Some(YankPlan::Daemon(range));
        }
        if range.start_row >= 0 {
            return Some(YankPlan::Local(self.extract_from_snapshot(
                range.start_row,
                range.start_col,
                range.end_row,
                range.end_col,
                range.ty,
            )));
        }
        let daemon_end_col = match range.ty {
            CopySelectionType::Character | CopySelectionType::Line => u16::MAX,
            CopySelectionType::Block => range.end_col,
        };
        let local_start_col = match range.ty {
            CopySelectionType::Character | CopySelectionType::Line => 0,
            CopySelectionType::Block => range.start_col,
        };
        Some(YankPlan::Stitched {
            daemon: YankRange {
                ty: range.ty,
                start_row: range.start_row,
                start_col: range.start_col,
                end_row: -1,
                end_col: daemon_end_col,
            },
            local: self.extract_from_snapshot(
                0,
                local_start_col,
                range.end_row,
                range.end_col,
                range.ty,
            ),
        })
    }

    /// Selection text from the entry snapshot, mirroring the daemon's
    /// format: per-row trailing spaces trimmed, rows joined with `\n`,
    /// unshown rows contributing empty lines so line N is always row N.
    fn extract_from_snapshot(
        &self,
        start_row: i64,
        start_col: u16,
        end_row: i64,
        end_col: u16,
        ty: CopySelectionType,
    ) -> String {
        let mut lines = Vec::new();
        let mut row = start_row;
        while row <= end_row {
            let (from, to) = match ty {
                CopySelectionType::Line => (0, u16::MAX),
                CopySelectionType::Block => (start_col, end_col),
                CopySelectionType::Character => {
                    let from = if row == start_row { start_col } else { 0 };
                    let to = if row == end_row { end_col } else { u16::MAX };
                    (from, to)
                }
            };
            let text = self.snapshot_row(row).map_or_else(String::new, |chars| {
                let from = usize::from(from).min(chars.len());
                let to = usize::from(to).saturating_add(1).min(chars.len());
                chars
                    .get(from..to)
                    .unwrap_or(&[])
                    .iter()
                    .collect::<String>()
            });
            lines.push(text.trim_end_matches(' ').to_string());
            row += 1;
        }
        lines.join("\n")
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

    /// The next chunk to fetch when the window has entered the top or
    /// bottom quarter of the cache window (Spec-0008 prefetch), or `None`
    /// when it has not, when the chunk is already cached or in flight, or
    /// when no history remains in that direction.
    pub(crate) fn plan_prefetch(&self) -> Option<FillRequest> {
        // The cache can sit apart from the display after a search jump;
        // fetch the gap on screen in one request before extending edges.
        if let Some(hole) = self.display_hole() {
            return (!self.is_pending(hole)).then_some(hole);
        }
        let (start, end) = (self.cache.start()?, self.cache.end()?);
        // A cache apart from the display is not grown toward it row by row;
        // `display_hole` fetches what scrolls into view.
        if end < self.display_top || start > self.display_top + i64::from(self.visible_rows) {
            return None;
        }
        let len = end - start;
        let zone = (len / 4).max(1);
        let chunk = u32::from(self.visible_rows).min(MAX_ROWS_PER_REQUEST);

        // The window's edges, not the cursor: the wheel scrolls the window
        // past a cursor that stays put (Spec-0008 Display).
        let candidate = if self.display_top < start + zone {
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
        } else if self.display_top + i64::from(self.visible_rows) > end - zone {
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

    /// The span of display rows neither the cache nor the page holds.
    fn display_hole(&self) -> Option<FillRequest> {
        let floor = self.history_start.unwrap_or(i64::MIN);
        let top = self.display_top.max(floor);
        let bottom = (self.display_top + i64::from(self.visible_rows)).min(self.viewport_top);
        let mut missing = (top..bottom).filter(|row| self.cache.get(*row).is_none());
        let first = missing.next()?;
        let last = missing.next_back().unwrap_or(first);
        Some(FillRequest {
            start_row: first,
            count: u32::try_from(last - first + 1).unwrap_or(MAX_ROWS_PER_REQUEST),
        })
    }

    fn fills_display_hole(&self, data: &ScrollbackData) -> bool {
        let Some(hole) = self.display_hole() else {
            return false;
        };
        let served_end = data.start_row + i64::try_from(data.rows.len()).unwrap_or(i64::MAX);
        data.start_row <= hole.start_row && served_end > hole.start_row
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
        // A different base means the live fallback answered after a
        // racing resize dropped the pin (ADR-0025 clause 5); discard and
        // let the CopyModeInvalidated push tear the session down. No
        // acked base yet fails closed for the same reason: every
        // legitimate fill is issued after the ack sets it.
        if self.pinned_base != Some(data.base) {
            warn!(
                pinned = ?self.pinned_base,
                served = data.base,
                "scrollback fill resolved against a different base; discarding"
            );
            return true;
        }
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
        // Owned by whichever scan or seek awaits this request now.
        if self.scan.is_some_and(|scan| scan.awaiting == request) {
            self.landed = Some(self.scan_landed(data));
            return true;
        }
        if self.seek.is_some_and(|seek| seek.awaiting == request) {
            self.landed = Some(self.seek_landed(data));
            return true;
        }
        // A window the cache does not touch replaces it only when it fills
        // a hole in the display window (`plan_prefetch`).
        let accepted = self.cache.insert_window(data.start_row, data.rows.clone())
            || (self.fills_display_hole(data) && {
                self.cache.rows.clear();
                self.cache.insert_window(data.start_row, data.rows.clone())
            });
        // Centred on the window, not the cursor: the wheel leaves the
        // cursor at a window edge, where a cursor-centred trim evicts each
        // prefetched window and refetches it forever.
        if accepted {
            let rows = i64::from(self.visible_rows);
            self.cache.trim_to_capacity(self.display_top + rows / 2);
            self.scroll_display(0);
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
        CopyModeState, CopySelectionType, FillFailure, FillRequest, SearchStep, SelectionEffect,
        ViewportCache, YankPlan, YankRange,
    };
    use crate::copy_search::BarStyle;
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
            base: 0,
            rows: rows(tags),
        }
    }

    /// Fill the cache with `tags` starting at `start`, bypassing the
    /// request bookkeeping the prefetch tests exercise separately.
    /// Fills simulate a post-ack session: `served()` builds replies at
    /// base 0, so the acked base is pinned to match before applying.
    fn filled(state: &mut CopyModeState, start: i64, tags: &str, has_more: bool) {
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 64, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
        state.record_fill(7, state.initial_fill());

        assert_eq!(state.fail_fill(7, false), FillFailure::Abandon);
    }

    /// Errors answering anything else on the connection must not retire
    /// a copy-mode fill that is still legitimately outstanding.
    #[test]
    fn an_error_for_another_request_leaves_copy_mode_fills_alone() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
        let fill = state.initial_fill();
        state.record_fill(7, fill);

        assert_eq!(state.fail_fill(99, true), FillFailure::Unclaimed);
        assert!(state.is_fill_in_flight(7));
    }

    // --- Prefetch boundaries ---

    #[test]
    fn prefetch_fires_at_the_top_quarter_of_the_window() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
        filled(&mut state, -20, "abcdefgh", true);

        state.set_cursor(-16, 0);
        state.set_cursor(-13, 0);
        assert_eq!(state.display_top(), -16, "every displayed row is held");
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
        filled(&mut state, -4, "abcd", true);

        state.set_cursor(-1, 0);
        assert_eq!(state.plan_prefetch(), None);
    }

    // --- Cursor ---

    #[test]
    fn the_cursor_seeds_at_the_bottom_left_of_the_viewport() {
        assert_eq!(CopyModeState::new(80, 24, 0, Vec::new()).cursor(), (23, 0));
    }

    /// Entering from scrollback: the daemon pins row 0 at the live grid's
    /// top no matter where the client is scrolled, so a state that
    /// ignored the offset would seed the cursor `offset` rows below
    /// anything on screen — the user would operate on rows they cannot
    /// see. At offset 10 a 24-row pane displays rows -10..=13.
    #[test]
    fn entering_from_scrollback_seeds_the_cursor_on_a_displayed_row() {
        let state = CopyModeState::new(80, 24, 10, Vec::new());
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
        let mut state = CopyModeState::new(80, 24, 10, Vec::new());
        state.set_pinned_base(0);

        state.set_cursor(-10, 0);
        assert_eq!(state.cursor(), (-10, 0));

        state.set_cursor(-11, 0);
        assert_eq!(state.cursor(), (-10, 0), "clamped at the viewport top");
    }

    /// Copy mode addresses exactly the painted page (ADR-0025 clause 6):
    /// entering scrolled by 10, the bottom addressable row is the page's
    /// bottom, not the live grid hidden below it.
    #[test]
    fn rows_below_the_painted_page_are_not_addressable() {
        let mut state = CopyModeState::new(80, 24, 10, Vec::new());
        state.set_pinned_base(0);
        state.set_cursor(13, 0);
        assert_eq!(state.cursor(), (13, 0), "the page's bottom row");

        state.set_cursor(14, 0);
        assert_eq!(state.cursor(), (13, 0), "one past the painted page");
    }

    /// The clamp spans the cache *and* the frozen viewport: clamping to
    /// cached rows alone would yank the cursor off the live grid it
    /// starts on, since the cache only ever holds negative rows.
    #[test]
    fn the_cursor_clamps_across_the_cache_and_the_viewport() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(10, 4, 0, Vec::new());
        state.set_pinned_base(0);

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
        let mut state = CopyModeState::new(0, 4, 0, Vec::new());
        state.set_pinned_base(0);
        state.set_cursor(0, 7);
        assert_eq!(state.cursor(), (0, 0));
    }

    /// Before any fill lands there is no scrollback to walk into, so the
    /// cursor stays inside the frozen viewport.
    #[test]
    fn the_cursor_clamps_to_the_viewport_with_an_empty_cache() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
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

    /// Shift+arrow in the basic preset extends before each move: the
    /// first press anchors, later presses must not re-anchor.
    #[test]
    fn extend_anchors_once_and_keeps_the_anchor_after() {
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
        state.set_cursor(2, 3);

        state.apply_selection_effect(SelectionEffect::Extend);
        state.set_cursor(4, 6);
        state.apply_selection_effect(SelectionEffect::Extend);

        let range = state.yank_range().expect("selecting");
        assert_eq!(range.ty, CopySelectionType::Character);
        assert_eq!(
            (range.start_row, range.start_col),
            (2, 3),
            "the second extend keeps the original anchor"
        );
        assert_eq!((range.end_row, range.end_col), (4, 6));
    }

    /// The three effects in one pass: keep leaves a selection alone,
    /// clear drops it, and keep never anchors one.
    #[test]
    fn keep_and_clear_effects_leave_and_drop_the_selection() {
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);

        state.apply_selection_effect(SelectionEffect::Keep);
        assert_eq!(state.yank_range(), None, "keep never anchors");

        state.apply_selection_effect(SelectionEffect::Extend);
        state.apply_selection_effect(SelectionEffect::Keep);
        assert!(state.yank_range().is_some(), "keep leaves it alone");

        state.apply_selection_effect(SelectionEffect::Clear);
        assert_eq!(state.yank_range(), None);
    }

    #[test]
    fn clearing_reports_whether_there_was_a_selection() {
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
        assert!(!state.clear_selection(), "nothing to clear");
        state.toggle_selection(CopySelectionType::Character);
        assert!(state.clear_selection());
        assert_eq!(state.yank_range(), None);
    }

    /// A selection dragged backwards yanks the same text as one dragged
    /// forwards: character ranges order in reading order.
    #[test]
    fn a_backwards_character_selection_normalizes_in_reading_order() {
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let state = CopyModeState::new(80, 24, 0, Vec::new());
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
        let fill = CopyModeState::new(80, 24, 10, Vec::new()).initial_fill();

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
        let fill = CopyModeState::new(80, 24, 100, Vec::new()).initial_fill();

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
            let fill = CopyModeState::new(80, 24, offset, Vec::new()).initial_fill();
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
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
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
        let state = CopyModeState::new(80, u16::MAX, 0, Vec::new());
        assert_eq!(state.initial_fill().count, super::MAX_ROWS_PER_REQUEST);
        assert_eq!(
            CopyModeState::new(80, u16::MAX, u32::MAX, Vec::new())
                .initial_fill()
                .count,
            super::MAX_ROWS_PER_REQUEST
        );
    }

    // --- ADR-0025 ownership split ---

    fn snap(rows: &[&str]) -> Vec<Vec<char>> {
        rows.iter().map(|r| r.chars().collect()).collect()
    }

    /// Snapshot rows are keyed in pin space: entering scrolled by 2, the
    /// page's first row is −2, and rows outside the page resolve to
    /// nothing even when the cache misses them too.
    #[test]
    fn snapshot_rows_resolve_in_pin_space() {
        let mut state = CopyModeState::new(80, 3, 2, Vec::new());
        state.set_pinned_base(0);
        state.set_snapshot(snap(&["top", "mid", "bot"]));

        assert_eq!(state.snapshot_row(-2), Some(&['t', 'o', 'p'][..]));
        assert_eq!(state.snapshot_row(0), Some(&['b', 'o', 't'][..]));
        assert_eq!(state.snapshot_row(-3), None, "above the page");
        assert_eq!(state.snapshot_row(1), None, "below the page");
    }

    /// A selection wholly on the painted page never reaches the daemon
    /// (ADR-0025 clause 7): the text extracts locally, per-row trailing
    /// spaces trimmed and the end column applied inclusively.
    #[test]
    fn a_yank_wholly_on_the_page_is_local() {
        let mut state = CopyModeState::new(80, 3, 0, Vec::new());
        state.set_pinned_base(0);
        state.set_snapshot(snap(&["alpha beta ", "gamma", "delta"]));

        state.set_cursor(0, 2);
        state.toggle_selection(CopySelectionType::Character);
        state.set_cursor(1, 2);

        assert_eq!(
            state.yank_plan(),
            Some(YankPlan::Local("pha beta\ngam".to_string()))
        );
    }

    /// A selection wholly in scrollback keeps the one-round-trip shape.
    #[test]
    fn a_yank_wholly_in_scrollback_goes_to_the_daemon() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(0);
        filled(&mut state, -8, "abcdefgh", true);
        state.set_cursor(-6, 1);
        state.toggle_selection(CopySelectionType::Character);
        state.set_cursor(-4, 2);

        let Some(YankPlan::Daemon(range)) = state.yank_plan() else {
            panic!("expected a daemon yank");
        };
        assert_eq!((range.start_row, range.end_row), (-6, -4));
    }

    /// A boundary-spanning character selection splits at −1/0: the
    /// daemon half opens to the row edge, the local half starts at
    /// column 0 and carries the true end column (ADR-0025 clause 7).
    #[test]
    fn a_spanning_yank_splits_at_the_boundary() {
        let mut state = CopyModeState::new(80, 3, 0, Vec::new());
        state.set_pinned_base(0);
        state.set_snapshot(snap(&["first", "second", "third"]));
        filled(&mut state, -4, "wxyz", true);

        state.set_cursor(-2, 3);
        state.toggle_selection(CopySelectionType::Character);
        state.set_cursor(1, 2);

        let Some(YankPlan::Stitched { daemon, local }) = state.yank_plan() else {
            panic!("expected a stitched yank");
        };
        assert_eq!((daemon.start_row, daemon.start_col), (-2, 3));
        assert_eq!((daemon.end_row, daemon.end_col), (-1, u16::MAX));
        assert_eq!(local, "first\nsec");
    }

    /// A block selection applies the same column rectangle to both
    /// halves rather than opening the daemon half to the row edge.
    #[test]
    fn a_spanning_block_yank_keeps_the_rectangle_on_both_halves() {
        let mut state = CopyModeState::new(80, 3, 0, Vec::new());
        state.set_pinned_base(0);
        state.set_snapshot(snap(&["first", "second", "third"]));
        filled(&mut state, -4, "wxyz", true);

        state.set_cursor(-1, 1);
        state.toggle_selection(CopySelectionType::Block);
        state.set_cursor(1, 3);

        let Some(YankPlan::Stitched { daemon, local }) = state.yank_plan() else {
            panic!("expected a stitched yank");
        };
        assert_eq!((daemon.start_col, daemon.end_col), (1, 3));
        assert_eq!(local, "irs\neco", "the rectangle on rows 0 and 1");
    }

    /// No acked base fails closed: a fill that somehow lands before the
    /// ack has nothing to validate against and is discarded, keeping the
    /// clause-5 guard airtight rather than resting on distant callers.
    #[test]
    fn a_fill_before_the_ack_is_discarded() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.record_fill(
            7,
            FillRequest {
                start_row: -4,
                count: 4,
            },
        );

        let claimed = state.apply_fill(7, &served(-4, "abcd", true));

        assert!(claimed, "the serial is consumed");
        assert!(state.cache().is_empty(), "nothing filed without an ack");
    }

    /// A fill answered against a different base is the live fallback
    /// after a racing resize (ADR-0025 clause 5): the reply is claimed
    /// but its rows are discarded, never filed into the cache.
    #[test]
    fn a_fill_from_a_different_base_is_discarded() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        state.set_pinned_base(10);
        state.record_fill(
            7,
            FillRequest {
                start_row: -4,
                count: 4,
            },
        );

        let claimed = state.apply_fill(
            7,
            &ScrollbackData {
                pane_id: 0,
                start_row: -4,
                has_more: true,
                total_rows: 12,
                base: 12,
                rows: vec![row('x'), row('y'), row('z'), row('w')],
            },
        );

        assert!(claimed, "the serial is consumed");
        assert!(state.cache().is_empty(), "wrong-base rows never filed");
    }

    /// Rows the snapshot does not hold contribute empty lines, so line N
    /// of the result is always row N of the range (Spec-0008 gap rule).
    #[test]
    fn local_extraction_blank_fills_missing_rows() {
        let mut state = CopyModeState::new(80, 2, 0, Vec::new());
        state.set_pinned_base(0);
        state.set_snapshot(snap(&["only"]));

        state.set_cursor(0, 0);
        state.toggle_selection(CopySelectionType::Line);
        state.set_cursor(1, 0);

        assert_eq!(
            state.yank_plan(),
            Some(YankPlan::Local("only\n".to_string()))
        );
    }

    // --- Display window (Spec-0008 Display) ---

    #[test]
    fn the_display_window_follows_the_cursor_and_never_drops_below_the_page() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        filled(&mut state, -12, "abcdefghijkl", true);
        assert_eq!(state.display_top(), 0);

        state.set_cursor(2, 0);
        assert_eq!(state.display_top(), 0, "inside the window: no move");

        state.set_cursor(-6, 0);
        assert_eq!(state.display_top(), -6, "above: the top follows");

        state.set_cursor(-4, 0);
        assert_eq!(state.display_top(), -6, "back inside: stays");

        state.set_cursor(3, 0);
        assert_eq!(state.display_top(), 0, "down to the page bottom");
    }

    #[test]
    fn scrolling_the_display_clamps_to_history_and_pulls_the_cursor_along() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        filled(&mut state, -12, "abcdefghijkl", true);
        assert_eq!(state.cursor(), (3, 0));

        state.scroll_display(-6);
        assert_eq!(state.display_top(), -6);
        assert_eq!(
            state.cursor(),
            (-3, 0),
            "cursor clamps to the window bottom"
        );

        state.scroll_display(-100);
        assert_eq!(state.display_top(), -12, "stops at the oldest cached row");

        state.scroll_display(100);
        assert_eq!(state.display_top(), 0, "stops at the entry page");
        assert_eq!(state.cursor(), (0, 0), "cursor clamps to the window top");
    }

    #[test]
    fn display_rows_come_from_the_page_then_the_cache_then_blank() {
        use crate::render_grid::RowSource;
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        filled(&mut state, -2, "yz", true);
        state.set_cursor(-3, 0);
        assert_eq!(state.display_top(), -2, "clamped to the oldest cached row");
        state.scroll_display(0);

        assert!(matches!(state.display_row(0), RowSource::Wire(r) if tag_of(r) == 'y'));
        assert!(matches!(state.display_row(1), RowSource::Wire(r) if tag_of(r) == 'z'));
        assert_eq!(state.display_row(2), RowSource::Own(0));
        assert_eq!(state.display_row(3), RowSource::Own(1));

        let mut sparse = CopyModeState::new(80, 4, 0, Vec::new());
        filled(&mut sparse, -8, "ab", true);
        sparse.set_cursor(-8, 0);
        assert_eq!(
            sparse.display_row(2),
            RowSource::Blank,
            "row -6 is uncached history"
        );
    }

    /// Entering from scrollback, rows below 0 are both on the page and in
    /// the cache; the display reads the cache, as yanks and motions do, so
    /// a scroll indicator painted on the page never shows in copy mode.
    #[test]
    fn cached_rows_win_over_the_page_copy_of_the_same_row() {
        use crate::render_grid::RowSource;
        let mut state = CopyModeState::new(80, 4, 2, Vec::new());
        filled(&mut state, -8, "abcdefgh", false);
        assert_eq!(state.display_top(), -2);
        assert!(matches!(state.display_row(0), RowSource::Wire(r) if tag_of(r) == 'g'));
        assert!(matches!(state.display_row(1), RowSource::Wire(r) if tag_of(r) == 'h'));
        assert_eq!(
            state.display_row(2),
            RowSource::Own(2),
            "row 0 is live, never cached"
        );
    }

    #[test]
    fn the_display_cursor_is_window_relative() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        filled(&mut state, -12, "abcdefghijkl", true);
        state.set_cursor(-6, 5);
        assert_eq!(state.display_cursor(), Some((5, 0)));
        state.set_cursor(-4, 7);
        assert_eq!(state.display_cursor(), Some((7, 2)));
    }

    #[test]
    fn the_display_selection_covers_the_yank_range_in_window_rows() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        filled(&mut state, -12, "abcdefghijkl", true);
        state.set_cursor(2, 6);
        state.toggle_selection(CopySelectionType::Character);
        state.set_cursor(1, 3);

        let selection = state.display_selection().expect("selected");
        assert!(selection.contains(1, 3) && selection.contains(2, 6));
        assert!(!selection.contains(1, 2) && !selection.contains(2, 7));
        assert!(selection.contains(1, 79), "character spans to row end");

        state.toggle_selection(CopySelectionType::Block);
        let block = state.display_selection().expect("selected");
        assert!(block.contains(1, 3) && block.contains(2, 6) && block.contains(1, 6));
        assert!(!block.contains(1, 7) && !block.contains(2, 2));

        state.toggle_selection(CopySelectionType::Line);
        let line = state.display_selection().expect("selected");
        assert!(line.contains(1, 0) && line.contains(2, 79));
        assert!(!line.contains(0, 0) && !line.contains(3, 0));

        state.set_cursor(-6, 0);
        let scrolled = state.display_selection().expect("selected");
        assert!(
            scrolled.contains(0, 0) && scrolled.contains(8, 0),
            "rows follow the window: -6 is row 0, the anchor at 2 is row 8"
        );
    }

    /// Answer the planned prefetch the way the daemon would, tagging each
    /// served row; `false` once nothing is planned.
    fn answer_prefetch(state: &mut CopyModeState, serial: u32) -> bool {
        let Some(request) = state.plan_prefetch() else {
            return false;
        };
        state.record_fill(serial, request);
        let count = usize::try_from(request.count).expect("small");
        let tags: String = "abcdefghijklmnopqrstuvwxyz"
            .chars()
            .cycle()
            .take(count)
            .collect();
        assert!(state.apply_fill(serial, &served(request.start_row, &tags, true)));
        true
    }

    /// The wheel leaves the cursor at the window bottom, where a trim
    /// centred on the cursor would evict each prefetched window forever.
    #[test]
    fn prefetch_settles_after_the_wheel_scrolls_up() {
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
        let tags: String = "abcdefghijklmnopqrstuvwx".to_string();
        filled(&mut state, -24, &tags, true);
        state.scroll_display(-20);
        assert_eq!(
            state.cursor().0 - state.display_top(),
            7,
            "cursor at the bottom"
        );

        let rounds = (2..50)
            .take_while(|&serial| answer_prefetch(&mut state, serial))
            .count();
        assert!(rounds < 10, "prefetch kept re-planning: {rounds} rounds");
        for visible in 0..8 {
            assert_ne!(
                state.display_row(visible),
                crate::render_grid::RowSource::Blank
            );
        }
    }

    /// The wheel leaves the cursor at the window top, so the window bottom
    /// can pass the cache end with the cursor nowhere near it.
    #[test]
    fn prefetch_fetches_below_a_window_scrolled_down_past_the_cursor() {
        let mut state = CopyModeState::new(80, 8, 0, Vec::new());
        state.set_pinned_base(0);
        filled(&mut state, -40, "abcdefghijklmnopqrstuvwx", true);
        state.set_cursor(-40, 0);
        state.scroll_display(17);
        assert_eq!(state.cursor().0, state.display_top(), "cursor at the top");
        assert!(
            state
                .plan_prefetch()
                .is_some_and(|fill| fill.start_row == -16),
            "the rows below the cache are fetched: {:?}",
            state.plan_prefetch()
        );
    }

    #[test]
    fn an_at_selection_puts_the_cursor_on_its_focus() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        filled(&mut state, -12, "abcdefghijkl", true);
        state.set_cursor(-6, 0);

        state.select_in_window(state.display_top(), (1, 3), Some((2, 6)));
        assert_eq!(state.cursor(), (-5, 3), "focus at window row 1");
        let selection = state.display_selection().expect("selected");
        assert_eq!(
            (selection.start.row, selection.start.col),
            (2, 6),
            "the selection runs from the anchor"
        );
        assert_eq!((selection.end.row, selection.end.col), (1, 3));

        state.select_in_window(state.display_top(), (3, 2), Some((3, 2)));
        let one = state.display_selection().expect("one cell is a selection");
        assert!(one.contains(3, 2), "the cell paints");
        assert!(
            !one.contains(3, 1) && !one.contains(3, 3),
            "its neighbours do not"
        );

        state.select_in_window(state.display_top(), (3, 2), None);
        assert_eq!(state.cursor(), (-3, 2));
        assert!(
            state.display_selection().is_none(),
            "a caret drops the selection"
        );

        // AT read a window 2 rows lower than the one now on screen: its
        // row 0 is the current row 2.
        let top = state.display_top();
        state.select_in_window(top + 2, (0, 1), None);
        assert_eq!(state.cursor(), (top + 2, 1));
    }

    // --- Search (Spec-0008 Search) ---

    /// A four-row page over eight cached history rows; `x` sits on rows
    /// -7 and -3 of history and on rows 0 (column 4) and 2 of the page.
    fn searchable(has_more: bool) -> CopyModeState {
        let mut state = CopyModeState::new(8, 4, 0, Vec::new());
        state.set_snapshot(snap(&["one x", "two", "x three", "four"]));
        filled(&mut state, -8, "axbcdxef", has_more);
        state
    }

    #[test]
    fn typing_a_query_jumps_from_where_the_search_started() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        assert_eq!(state.cursor(), (3, 0));
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        assert!(state.search_editing());

        assert_eq!(state.edit_search(BarKey::Insert('x')), SearchStep::Moved);
        assert_eq!(state.cursor(), (2, 0), "nearest match above the start");
        assert_eq!(state.edit_search(BarKey::Backspace), SearchStep::Updated);
        assert_eq!(
            state.cursor(),
            (3, 0),
            "an empty query returns to the start"
        );
        state.edit_search(BarKey::Insert('x'));
        assert_eq!(state.edit_search(BarKey::Accept), SearchStep::Updated);
        assert!(!state.search_editing());
        assert_eq!(
            state.cursor(),
            (2, 0),
            "Enter leaves the cursor on the match"
        );
    }

    #[test]
    fn escape_returns_to_the_start_and_keeps_the_query_for_n() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        state.edit_search(BarKey::Insert('x'));
        state.edit_search(BarKey::Cancel);
        assert_eq!(state.cursor(), (3, 0));
        assert!(!state.search_editing());
        assert_eq!(state.search_again(false), SearchStep::Moved);
        assert_eq!(state.cursor(), (2, 0));
    }

    #[test]
    fn n_walks_the_matches_and_n_reverses() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(false);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        state.edit_search(BarKey::Insert('x'));
        state.edit_search(BarKey::Accept);
        let mut visited = Vec::new();
        for _ in 0..3 {
            assert_eq!(state.search_again(false), SearchStep::Moved);
            visited.push(state.cursor());
        }
        assert_eq!(visited, vec![(0, 4), (-3, 0), (-7, 0)]);
        assert_eq!(
            state.search_again(false),
            SearchStep::NotFound { ring: true },
            "history starts at -8"
        );
        assert_eq!(state.cursor(), (-7, 0), "a miss leaves the cursor put");
        assert_eq!(state.search_again(true), SearchStep::Moved);
        assert_eq!(state.cursor(), (-3, 0), "N searches the other way");
    }

    #[test]
    fn search_chunks_fit_the_payload_limit() {
        use super::scan_chunk_rows;
        use crate::copy_search::{BarKey, SearchDirection};
        use oakterm_protocol::frame::MAX_PAYLOAD;
        assert_eq!(scan_chunk_rows(80), 2048);
        assert_eq!(scan_chunk_rows(256), 2048);
        for cols in [1, 80, 256, 300, 1000, u16::MAX] {
            let rows = u64::from(scan_chunk_rows(cols));
            let wider = u64::from(cols.max(256)) * 2;
            assert!(
                rows * wider * 16 <= u64::from(MAX_PAYLOAD),
                "{cols} columns, history twice as wide"
            );
        }

        // The scan asks for chunks of that size.
        let mut wide = CopyModeState::new(300, 4, 0, Vec::new());
        wide.set_pinned_base(0);
        wide.set_snapshot(snap(&["a", "b", "c", "d"]));
        wide.start_search(SearchDirection::Backward, BarStyle::Preset);
        let SearchStep::Fetch(chunk) = wide.edit_search(BarKey::Insert('q')) else {
            panic!("expected a chunk request");
        };
        assert_eq!(chunk.count, scan_chunk_rows(300));
    }

    /// Past the cache the scan asks for a chunk and leaves the cursor
    /// put; a cache fill landing meanwhile resumes nothing, and the chunk
    /// becomes the cache when it holds the match.
    #[test]
    fn a_search_past_the_cache_scans_a_fetched_chunk() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        state.set_cursor(-7, 0);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        let rows = super::scan_chunk_rows(8);
        let chunk = FillRequest {
            start_row: -9 - i64::from(rows) + 1,
            count: rows,
        };
        assert_eq!(
            state.edit_search(BarKey::Insert('w')),
            SearchStep::Fetch(chunk)
        );
        assert_eq!(state.cursor(), (-7, 0), "the cursor stays put");
        assert_eq!(state.search_status().map(|s| s.more), Some(true));

        let prefetch = FillRequest {
            start_row: -10,
            count: 2,
        };
        state.record_fill(8, prefetch);
        assert!(state.apply_fill(8, &served(-10, "yz", true)));
        assert_eq!(
            state.continue_search(),
            SearchStep::Inert,
            "an unrelated fill resumes nothing"
        );

        state.record_fill(9, chunk);
        assert!(state.apply_fill(9, &served(-12, "awyz", false)));
        assert_eq!(state.continue_search(), SearchStep::Moved);
        assert_eq!(state.cursor(), (-11, 0));
        assert_eq!(
            state.cache().get(-11).map(tag_of),
            Some('w'),
            "the chunk is the cache now"
        );
    }

    #[test]
    fn a_typed_miss_past_history_reports_without_a_bell() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        let SearchStep::Fetch(chunk) = state.edit_search(BarKey::Insert('q')) else {
            panic!("expected a chunk request");
        };
        state.record_fill(9, chunk);
        assert!(state.apply_fill(9, &served(-12, "abcd", false)));
        assert_eq!(
            state.continue_search(),
            SearchStep::NotFound { ring: false }
        );
        assert_eq!(state.cursor(), (3, 0), "the cursor never left");
    }

    /// Returning to a row the cache no longer holds replaces the cache
    /// around it, as Escape does for an evicted origin.
    #[test]
    fn going_to_an_evicted_row_seeks_it() {
        let mut state = searchable(true);
        let SearchStep::Fetch(window) = state.go_to((-20, 3)) else {
            panic!("expected a seek window");
        };
        assert_eq!(
            window,
            FillRequest {
                start_row: -24,
                count: 12
            }
        );
        state.record_fill(5, window);
        assert!(state.apply_fill(5, &served(-24, "abcdefghijkl", true)));
        assert_eq!(state.continue_search(), SearchStep::Updated);
        assert_eq!(state.cursor(), (-20, 3));

        assert!(
            matches!(state.go_to((-5, 1)), SearchStep::Fetch(_)),
            "a row newer than the cache is not held either"
        );
    }

    /// After a jump the cache sits apart from rows the display scrolls
    /// onto; prefetch asks for that gap and the reply replaces the cache.
    #[test]
    fn prefetch_fills_a_gap_on_screen_in_one_request() {
        let mut state = searchable(true);
        let SearchStep::Fetch(window) = state.go_to((-20, 3)) else {
            panic!("expected a seek window");
        };
        state.record_fill(5, window);
        assert!(state.apply_fill(5, &served(-24, "abcdefghijkl", true)));
        state.scroll_display(10);
        assert_eq!(state.display_top(), -10);
        let hole = FillRequest {
            start_row: -10,
            count: 4,
        };
        assert_eq!(state.plan_prefetch(), Some(hole));
        state.record_fill(
            7,
            FillRequest {
                start_row: -8,
                count: 2,
            },
        );
        assert!(state.apply_fill(7, &served(-8, "mn", true)));
        assert_eq!(
            state.cache().get(-20).map(tag_of),
            Some('e'),
            "a reply below the hole's top leaves the cache alone"
        );

        let mut on_page = searchable(true);
        let SearchStep::Fetch(far) = on_page.go_to((-20, 3)) else {
            panic!("expected a seek window");
        };
        on_page.record_fill(8, far);
        assert!(on_page.apply_fill(8, &served(-24, "abcdefghijkl", true)));
        on_page.set_cursor(3, 0);
        assert_eq!(on_page.display_top(), 0);
        assert_eq!(
            on_page.plan_prefetch(),
            None,
            "a cache apart from the page is not crawled toward it"
        );

        state.record_fill(6, hole);
        assert!(state.apply_fill(6, &served(-10, "wxyz", true)));
        assert_eq!(state.cache().get(-10).map(tag_of), Some('w'));
        assert_eq!(
            state.plan_prefetch(),
            Some(FillRequest {
                start_row: -14,
                count: 4
            }),
            "with the hole filled, prefetch extends the edges again"
        );
    }

    /// A search typed before the entry ack holds its history window until
    /// the pin lands, so no fill resolves against live rows.
    #[test]
    fn a_search_before_the_pin_waits_for_it() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = CopyModeState::new(8, 4, 0, Vec::new());
        state.set_snapshot(snap(&["one", "two", "six", "ten"]));
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        assert_eq!(state.edit_search(BarKey::Insert('q')), SearchStep::Updated);
        let chunk = state.awaited_fill().expect("the scan awaits a window");
        assert_eq!(chunk.start_row + i64::from(chunk.count), 0);

        state.set_pinned_base(0);
        state.record_fill(3, chunk);
        assert!(state.apply_fill(3, &served(-2, "aq", false)));
        assert_eq!(state.continue_search(), SearchStep::Moved);
        assert_eq!(state.cursor(), (-1, 0));
        assert_eq!(state.awaited_fill(), None);
    }

    /// A chunk the search asked for before it changed belongs to nobody:
    /// it neither moves the cursor nor ends the new search.
    #[test]
    fn a_chunk_for_a_replaced_search_resumes_nothing() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        let SearchStep::Fetch(chunk) = state.edit_search(BarKey::Insert('q')) else {
            panic!("expected a chunk request");
        };
        state.record_fill(9, chunk);
        assert_eq!(state.edit_search(BarKey::Cancel), SearchStep::Updated);
        assert!(state.apply_fill(9, &served(-12, "aqyz", true)));
        assert_eq!(state.continue_search(), SearchStep::Inert);
        assert_eq!(state.cursor(), (3, 0));

        let SearchStep::Fetch(seek) = state.go_to((-40, 0)) else {
            panic!("expected a seek window");
        };
        state.record_fill(10, seek);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        assert_eq!(state.edit_search(BarKey::Insert('x')), SearchStep::Moved);
        assert!(state.apply_fill(10, &served(-44, "abcdefghijkl", true)));
        assert_eq!(
            state.continue_search(),
            SearchStep::Inert,
            "a new search drops the seek"
        );
        assert_eq!(state.cursor(), (2, 0));
    }

    /// A match in a chunk that touches the cache keeps the rows beside it.
    #[test]
    fn a_match_next_to_the_cache_merges_with_it() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        let SearchStep::Fetch(chunk) = state.edit_search(BarKey::Insert('q')) else {
            panic!("expected a chunk request");
        };
        state.record_fill(9, chunk);
        assert!(state.apply_fill(9, &served(-9, "q", false)));
        assert_eq!(state.continue_search(), SearchStep::Moved);
        assert_eq!(state.cursor(), (-9, 0));
        assert_eq!(state.cache().get(-8).map(tag_of), Some('a'));
        assert_eq!(state.display_top(), -9);

        // At the history floor with rows below the match unheld, the gap
        // is still fetched: edge extension upward has nowhere to go.
        let mut floor = searchable(true);
        let SearchStep::Fetch(window) = floor.go_to((-30, 0)) else {
            panic!("expected a seek window");
        };
        floor.record_fill(4, window);
        assert!(floor.apply_fill(4, &served(-30, "q", false)));
        assert_eq!(floor.display_top(), -30);
        assert_eq!(
            floor.plan_prefetch(),
            Some(FillRequest {
                start_row: -29,
                count: 3
            })
        );
    }

    /// Entered scrolled into history, a forward scan stops at the frozen
    /// page: rows past it are not addressable even when a reply holds them.
    #[test]
    fn a_forward_scan_stops_at_the_frozen_page() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = CopyModeState::new(8, 4, 97, Vec::new());
        state.set_snapshot(snap(&["p", "q", "r", "s"]));
        filled(&mut state, -101, "abcd", true);
        let SearchStep::Fetch(window) = state.go_to((-120, 0)) else {
            panic!("expected a seek window");
        };
        state.record_fill(5, window);
        assert!(state.apply_fill(5, &served(-124, "abcdefghijkl", true)));
        assert_eq!(state.cursor(), (-120, 0));

        state.start_search(SearchDirection::Forward, BarStyle::Preset);
        let chunk = FillRequest {
            start_row: -112,
            count: 15,
        };
        assert_eq!(
            state.edit_search(BarKey::Insert('z')),
            SearchStep::Fetch(chunk)
        );
        state.record_fill(6, chunk);
        let mut tags = "a".repeat(111);
        tags.push('z');
        assert!(state.apply_fill(6, &served(-112, &tags, true)));
        assert_eq!(
            state.continue_search(),
            SearchStep::NotFound { ring: false },
            "the z at row -1 lies below the page"
        );
        assert_eq!(state.cursor(), (-120, 0));
    }

    #[test]
    fn highlights_are_window_relative_in_history() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        state.edit_search(BarKey::Insert('x'));
        state.edit_search(BarKey::Accept);
        state.set_cursor(-7, 0);
        assert_eq!(state.display_top(), -7);
        let highlights: Vec<(i64, u16)> = state
            .display_matches()
            .into_iter()
            .map(|hit| (hit.row, hit.start))
            .collect();
        assert_eq!(highlights, vec![(0, 0)], "row -7 is window row 0");
    }

    #[test]
    fn a_repeat_key_in_the_bar_moves_on_in_its_direction() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        state.edit_search(BarKey::Insert('x'));
        assert_eq!(state.cursor(), (2, 0));
        assert_eq!(
            state.edit_search(BarKey::Repeat(SearchDirection::Backward)),
            SearchStep::Moved
        );
        assert_eq!(state.cursor(), (0, 4));
        assert!(state.search_editing(), "the bar stays open");
    }

    /// With the cache far above the page, the count still covers both.
    #[test]
    fn the_status_counts_a_cache_apart_from_the_page() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        let SearchStep::Fetch(window) = state.go_to((-2000, 0)) else {
            panic!("expected a seek window");
        };
        state.record_fill(5, window);
        assert!(state.apply_fill(5, &served(window.start_row, "xbxdxfghijkl", true)));
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        state.edit_search(BarKey::Insert('x'));
        let status = state.search_status().expect("searching");
        assert_eq!(status.total, 5, "three cached rows and two page rows");

        // Entered scrolled back, the cache holds page rows too; each row
        // counts once.
        let mut scrolled = CopyModeState::new(8, 4, 4, Vec::new());
        scrolled.set_snapshot(snap(&["x", "x", "x", "x"]));
        filled(&mut scrolled, -8, "xxxxxxxx", true);
        scrolled.start_search(SearchDirection::Backward, BarStyle::Preset);
        scrolled.edit_search(BarKey::Insert('x'));
        let status = scrolled.search_status().expect("searching");
        assert_eq!(status.total, 8);
        assert_eq!(scrolled.cursor(), (-1, 0));
        assert_eq!(status.index, Some(7));
    }

    /// The find bar's Enter goes on up, Shift+Enter back down, without
    /// turning the search around; Escape asks to leave copy mode.
    #[test]
    fn the_find_bar_steps_both_ways_and_escape_leaves() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(false);
        state.start_search(SearchDirection::Backward, BarStyle::Find);
        state.edit_search(BarKey::Insert('x'));
        assert_eq!(state.cursor(), (2, 0));
        let next = |reverse| BarKey::Next { reverse };
        assert_eq!(state.edit_search(next(false)), SearchStep::Moved);
        assert_eq!(state.cursor(), (0, 4));
        assert_eq!(state.edit_search(next(false)), SearchStep::Moved);
        assert_eq!(state.cursor(), (-3, 0));
        assert_eq!(state.edit_search(next(true)), SearchStep::Moved);
        assert_eq!(state.cursor(), (0, 4), "Shift+Enter steps back down");
        assert_eq!(state.edit_search(next(false)), SearchStep::Moved);
        assert_eq!(state.cursor(), (-3, 0), "Enter still goes up");
        assert!(state.search_editing(), "the find bar stays open");

        assert_eq!(state.edit_search(BarKey::Leave), SearchStep::Exit);
        assert!(!state.search_editing());
    }

    /// The bar reports the style and direction it opened with, which pick
    /// its keys in the app and its prefix on screen.
    #[test]
    fn the_bar_reports_its_style_and_direction() {
        use crate::copy_search::{BarKey, SearchDirection};
        for (style, direction) in [
            (BarStyle::Find, SearchDirection::Backward),
            (BarStyle::Preset, SearchDirection::Forward),
            (BarStyle::Preset, SearchDirection::Backward),
        ] {
            let mut state = searchable(true);
            state.start_search(direction, style);
            assert_eq!(state.open_bar(), Some(style));
            state.edit_search(BarKey::Insert('x'));
            let status = state.search_status().expect("searching");
            assert_eq!((status.style, status.direction), (style, direction));
        }
    }

    /// The find bar reads its whole starting row: a match right of the
    /// cursor on the bottom line is the first one up.
    #[test]
    fn the_find_bar_finds_the_bottom_line() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = CopyModeState::new(8, 4, 0, Vec::new());
        state.set_snapshot(snap(&["one", "two", "six", "ab zz"]));
        state.start_search(SearchDirection::Backward, BarStyle::Find);
        assert_eq!(state.edit_search(BarKey::Insert('z')), SearchStep::Moved);
        assert_eq!(state.cursor(), (3, 4));

        // Emptying the query returns to where the bar opened, not to the
        // end of that row.
        let mut state = searchable(true);
        state.set_cursor(1, 2);
        state.start_search(SearchDirection::Backward, BarStyle::Find);
        assert_eq!(state.edit_search(BarKey::Backspace), SearchStep::Updated);
        assert_eq!(state.cursor(), (1, 2));
        state.edit_search(BarKey::Insert('x'));
        assert_eq!(state.edit_search(BarKey::Backspace), SearchStep::Updated);
        assert_eq!(state.cursor(), (1, 2));
    }

    #[test]
    fn the_status_and_highlights_cover_the_held_rows() {
        use crate::copy_search::{BarKey, SearchDirection};
        let mut state = searchable(true);
        state.start_search(SearchDirection::Backward, BarStyle::Preset);
        state.edit_search(BarKey::Insert('x'));
        let status = state.search_status().expect("searching");
        assert_eq!(
            (
                status.query.as_str(),
                status.index,
                status.total,
                status.more
            ),
            ("x", Some(3), 4, true),
            "(2, 0) is the last of four held matches; older history remains"
        );
        let highlights: Vec<(i64, u16, u16)> = state
            .display_matches()
            .into_iter()
            .map(|hit| (hit.row, hit.start, hit.end))
            .collect();
        assert_eq!(highlights, vec![(0, 4, 5), (2, 0, 1)]);
    }

    #[test]
    fn prefetch_fires_when_the_window_scrolls_into_the_top_quarter() {
        let mut state = CopyModeState::new(80, 4, 0, Vec::new());
        filled(&mut state, -12, "abcdefghijkl", true);
        state.set_cursor(-5, 0);
        assert_eq!(state.plan_prefetch(), None);

        // The cursor stays near the window bottom; the top reaches the zone.
        state.scroll_display(-6);
        assert_eq!(state.display_top(), -11);
        assert_eq!(
            state.plan_prefetch(),
            Some(FillRequest {
                start_row: -16,
                count: 4
            })
        );
    }
}
