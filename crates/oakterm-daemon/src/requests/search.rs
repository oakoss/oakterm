//! Search family: `FindPrompt` (0x75), `SearchScrollback`, `SearchNext`,
//! `SearchPrev`, `SearchClose` (0x77-0x7B).

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, lock_live_pane};
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{
    ErrorCode, FindPrompt, PromptPosition, SearchDirection, SearchNav, SearchResults,
    SearchScrollback,
};
use oakterm_terminal::grid::row::SemanticMark;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, warn};

pub(super) async fn find_prompt(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(req) = FindPrompt::decode(&frame.payload) else {
        warn!(conn_id, "malformed FindPrompt payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed FindPrompt",
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
    if pane.pin_invalidation_pending(conn_id) {
        warn!(
            conn_id,
            pane_id = req.pane_id,
            "prompt search refused: pin invalidated by a resize"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InvalidMessage,
            "copy-mode pin invalidated by a resize",
        );
    }
    // Spec-0001: FindPrompt shares GetScrollback's coordinate space, so it
    // must resolve against the same origin — the copy-mode pin when the
    // client has one, or the live present otherwise.
    let pinned = pane.copy_mode_pins.contains_key(&conn_id);
    // While pinned, results never name rows the frozen page does not
    // show (ADR-0025 clause 8). An Older search starting on the painted
    // page clamps its start to row 0, so ineligible prompts at or above
    // 0 are skipped over rather than hiding older eligible ones; a Newer
    // search's nearest hit at or above 0 correctly means "none".
    let from_offset = if pinned && req.direction == SearchDirection::Older {
        req.from_offset.min(0)
    } else {
        req.from_offset
    };
    let found_offset = find_prompt_in_buffer(
        pane.screens.scrollback(),
        pane.copy_mode_base(conn_id),
        from_offset,
        req.direction,
    )
    .filter(|&offset| !pinned || offset < 0);
    let response = PromptPosition {
        pane_id: req.pane_id,
        offset: found_offset,
    };

    match response.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to create PromptPosition frame");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "PromptPosition frame error",
            )
        }
    }
}

/// Hot-buffer index of pin-space row 0 for a pinned client: matches at
/// or above it belong to the frozen page and are never reported to that
/// client (ADR-0025 clause 8). `None` when unpinned. Computed rather
/// than applied to the engine — the engine is pane-wide shared state,
/// and destructively clamping it would shrink every other client's
/// results too.
fn pin_row_limit(pane: &crate::pane::PaneState, conn_id: u64) -> Option<usize> {
    let &pin = pane.copy_mode_pins.get(&conn_id)?;
    Some(row_limit_for(pin, pane.screens.scrollback().first_index()))
}

/// Hot-buffer index of pin-space row 0 (ADR-0025 clause 8).
fn row_limit_for(pin: u64, first_index: u64) -> usize {
    usize::try_from(pin.saturating_sub(first_index)).unwrap_or(usize::MAX)
}

/// Step the shared nav cursor past matches this pinned client must not
/// see, bounded by one full wrap. The cursor is inherently shared nav
/// state; the match set itself is left intact.
fn skip_ineligible_matches(
    engine: &mut oakterm_terminal::search::SearchEngine,
    limit: usize,
    forward: bool,
) {
    for _ in 0..engine.match_count() {
        if engine.active_match().is_none_or(|m| m.row < limit) {
            return;
        }
        if forward {
            engine.next();
        } else {
            engine.prev();
        }
    }
}

pub(super) async fn search_scrollback(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(req) = SearchScrollback::decode(&frame.payload) else {
        warn!(conn_id, "malformed SearchScrollback payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed SearchScrollback",
        );
    };
    let mode = if req.flags.regex() {
        oakterm_terminal::search::SearchMode::Regex
    } else if req.flags.case_sensitive() {
        oakterm_terminal::search::SearchMode::CaseSensitive
    } else {
        oakterm_terminal::search::SearchMode::SmartCase
    };
    let engine = match oakterm_terminal::search::SearchEngine::new(&req.query, mode) {
        Ok(e) => e,
        Err(e) => {
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::MalformedPayload,
                &format!("invalid search pattern: {e}"),
            );
        }
    };
    let Some(mut pane) = lock_live_pane(panes, req.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    if pane.pin_invalidation_pending(conn_id) {
        warn!(
            conn_id,
            pane_id = req.pane_id,
            "search refused: pin invalidated by a resize"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InvalidMessage,
            "copy-mode pin invalidated by a resize",
        );
    }
    pane.screens.set_search(engine);
    pane.screens.run_search();
    build_search_response(
        conn_id,
        &pane.screens,
        pane.copy_mode_pins.get(&conn_id).copied(),
        req.pane_id,
        frame.serial,
    )
}

pub(super) async fn search_next(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(req) = SearchNav::decode(&frame.payload) else {
        warn!(conn_id, "malformed SearchNext payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed SearchNext",
        );
    };
    let Some(mut pane) = lock_live_pane(panes, req.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    if pane.pin_invalidation_pending(conn_id) {
        warn!(
            conn_id,
            pane_id = req.pane_id,
            "search refused: pin invalidated by a resize"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InvalidMessage,
            "copy-mode pin invalidated by a resize",
        );
    }
    let limit = pin_row_limit(&pane, conn_id);
    if let Some(engine) = pane.screens.search_mut() {
        engine.next();
        if let Some(limit) = limit {
            skip_ineligible_matches(engine, limit, true);
        }
    } else {
        warn!(conn_id, "SearchNext with no active search");
    }
    build_search_response(
        conn_id,
        &pane.screens,
        pane.copy_mode_pins.get(&conn_id).copied(),
        req.pane_id,
        frame.serial,
    )
}

pub(super) async fn search_prev(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(req) = SearchNav::decode(&frame.payload) else {
        warn!(conn_id, "malformed SearchPrev payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed SearchPrev",
        );
    };
    let Some(mut pane) = lock_live_pane(panes, req.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    if pane.pin_invalidation_pending(conn_id) {
        warn!(
            conn_id,
            pane_id = req.pane_id,
            "search refused: pin invalidated by a resize"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InvalidMessage,
            "copy-mode pin invalidated by a resize",
        );
    }
    let limit = pin_row_limit(&pane, conn_id);
    if let Some(engine) = pane.screens.search_mut() {
        engine.prev();
        if let Some(limit) = limit {
            skip_ineligible_matches(engine, limit, false);
        }
    } else {
        warn!(conn_id, "SearchPrev with no active search");
    }
    build_search_response(
        conn_id,
        &pane.screens,
        pane.copy_mode_pins.get(&conn_id).copied(),
        req.pane_id,
        frame.serial,
    )
}

pub(super) async fn search_close(panes: &Arc<Mutex<PaneManager>>) -> RequestResult {
    // Idempotent — close search on all panes (no pane_id in payload).
    let all_panes = panes.lock().await.snapshot();
    for (_, pane) in all_panes {
        pane.lock().await.screens.clear_search();
    }
    RequestResult::NoResponse
}

#[allow(clippy::cast_possible_wrap)]
fn build_search_response(
    conn_id: u64,
    screens: &oakterm_terminal::grid::ScreenSet,
    pin: Option<u64>,
    pane_id: u32,
    serial: u32,
) -> RequestResult {
    let (total_matches, active_index, active_row_offset, capped) = match screens.search() {
        Some(engine) => {
            let buf = screens.scrollback();
            // Counts and offsets resolve against the pin while one is
            // held (ADR-0025 clause 8), the live present otherwise. The
            // filtering is per-response: the engine is pane-wide shared
            // state, and clamping it would shrink other clients' results.
            let limit = pin.map(|p| row_limit_for(p, buf.first_index()));
            let total = match limit {
                Some(limit) => engine.matches().iter().filter(|m| m.row < limit).count(),
                None => engine.match_count(),
            };
            let total = u32::try_from(total).unwrap_or(u32::MAX);
            let (idx, offset) = match engine.active_match() {
                Some(m) if limit.is_none_or(|limit| m.row < limit) => {
                    let end = pin.unwrap_or_else(|| buf.pushed());
                    let abs = i128::from(buf.first_index()) + m.row as i128;
                    let neg_offset = i64::try_from(abs - i128::from(end)).unwrap_or(i64::MIN);
                    (
                        engine
                            .active_index()
                            .map(|i| u32::try_from(i).unwrap_or(u32::MAX)),
                        neg_offset,
                    )
                }
                // The active match sits on the frozen page; the client
                // merges visible-page matches itself.
                _ => (None, 0),
            };
            (total, idx, offset, engine.is_capped())
        }
        None => (0, None, 0, false),
    };

    let response = SearchResults {
        pane_id,
        total_matches,
        active_index,
        active_row_offset,
        capped,
        visible_matches: Vec::new(),
    };

    match response.to_frame(serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to create SearchResults frame");
            make_error_response(
                conn_id,
                serial,
                ErrorCode::InternalError,
                "SearchResults frame error",
            )
        }
    }
}

/// Returns `Some(negative_offset)` if found, `None` otherwise. The offset
/// uses the same coordinate space as `GetScrollback.start_row`.
/// Offsets are relative to `base` in the absolute row space; the buffer
/// holds `[buf.first_index(), buf.first_index() + buf.len())` of it.
fn find_prompt_in_buffer(
    buf: &oakterm_terminal::scroll::HotBuffer,
    base: u64,
    from_offset: i64,
    direction: SearchDirection,
) -> Option<i64> {
    let hot_first = i128::from(buf.first_index());
    let base = i128::from(base);
    let from_idx =
        usize::try_from((base + i128::from(from_offset) - hot_first).clamp(0, buf.len() as i128))
            .unwrap_or(0);

    let found_idx = match direction {
        SearchDirection::Older => (0..from_idx).rev().find(|&i| {
            buf.get(i)
                .is_some_and(|r| r.semantic_mark == SemanticMark::PromptStart)
        }),
        SearchDirection::Newer => {
            let start = (from_idx + 1).min(buf.len());
            (start..buf.len()).find(|&i| {
                buf.get(i)
                    .is_some_and(|r| r.semantic_mark == SemanticMark::PromptStart)
            })
        }
    };

    found_idx.and_then(|idx| i64::try_from(hot_first + idx as i128 - base).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_terminal::grid::row::Row;
    use oakterm_terminal::scroll::HotBuffer;

    /// A client not in copy mode: the origin is the live present.
    fn find_prompt_unpinned(
        buf: &HotBuffer,
        from_offset: i64,
        direction: SearchDirection,
    ) -> Option<i64> {
        find_prompt_in_buffer(buf, buf.pushed(), from_offset, direction)
    }

    /// The handler's Older-clamp for pinned clients (ADR-0025 clause 8):
    /// starting the scan at pin-space row 0 skips ineligible prompts at
    /// or above 0 rather than letting the nearest one hide an older
    /// eligible prompt below 0.
    #[test]
    fn a_pinned_older_scan_skips_prompts_the_page_owns() {
        let mut buf = buffer_with_prompts(10, &[3]);
        let pinned_base = buf.pushed();
        // A prompt lands after the pin, at pin-space rows >= 0.
        for i in 0..5 {
            let mut row = Row::new(80);
            if i == 2 {
                row.semantic_mark = SemanticMark::PromptStart;
            }
            buf.push(row);
        }

        // Unclamped from a painted-page offset, the post-pin prompt is
        // the nearest hit — the case the handler clamps away.
        assert_eq!(
            find_prompt_in_buffer(&buf, pinned_base, 4, SearchDirection::Older),
            Some(2)
        );
        // The handler clamps the start to 0, restoring the eligible result.
        assert_eq!(
            find_prompt_in_buffer(&buf, pinned_base, 0, SearchDirection::Older),
            Some(-7)
        );
    }

    /// A pinned client's offsets must name the same rows `GetScrollback`
    /// would return for them, so `FindPrompt` resolves against the pin too.
    #[test]
    fn find_prompt_resolves_against_a_pinned_base() {
        let mut buf = buffer_with_prompts(10, &[3]);
        let pinned_base = buf.pushed();

        // The prompt sits at offset -7 from the present at pin time.
        assert_eq!(
            find_prompt_in_buffer(&buf, pinned_base, 0, SearchDirection::Older),
            Some(-7)
        );

        for _ in 0..5 {
            buf.push(Row::new(80));
        }

        assert_eq!(
            find_prompt_in_buffer(&buf, pinned_base, 0, SearchDirection::Older),
            Some(-7),
            "the pin holds the prompt's offset steady as output arrives"
        );
        assert_eq!(
            find_prompt_unpinned(&buf, 0, SearchDirection::Older),
            Some(-12),
            "an unpinned client sees it recede with the present"
        );
    }

    /// The search family refuses reads while an invalidation push is
    /// outstanding, like scrollback and yank (ADR-0025 clause 5).
    #[tokio::test]
    async fn a_search_with_an_undelivered_invalidation_is_refused() {
        use crate::pane::PaneManager;
        use oakterm_protocol::message::{ErrorMessage, MSG_ERROR, SearchFlags};
        use std::sync::Arc;
        use tokio::sync::Mutex as TokioMutex;

        let panes = Arc::new(TokioMutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens
                .push_to_scrollback(oakterm_terminal::grid::row::Row::new(80));
            pane.pin_copy_mode(1, 1).expect("in range");
            pane.screens
                .push_to_scrollback(oakterm_terminal::grid::row::Row::new(80));
            assert_eq!(pane.invalidate_pins_after_resize(1), 1);
        }

        let search = SearchScrollback {
            pane_id,
            query: "x".to_string(),
            flags: SearchFlags(0),
        };
        let frame = Frame::new(0x77, 9, search.encode().expect("encode")).expect("frame");
        let RequestResult::Response(reply) = search_scrollback(1, &frame, &panes).await else {
            panic!("expected an error response");
        };
        assert_eq!(reply.msg_type, MSG_ERROR);
        let err = ErrorMessage::decode(&reply.payload).expect("decode");
        assert_eq!(err.code, ErrorCode::InvalidMessage as u32);
    }

    /// `FindPrompt` and `SearchPrev` carry the same refusal as the rest of
    /// the read family — the two guards the spec names that had no test.
    #[tokio::test]
    async fn find_prompt_and_search_prev_refuse_with_an_undelivered_invalidation() {
        use crate::pane::PaneManager;
        use oakterm_protocol::message::{ErrorMessage, MSG_ERROR};
        use std::sync::Arc;
        use tokio::sync::Mutex as TokioMutex;

        let panes = Arc::new(TokioMutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            pane.screens
                .push_to_scrollback(oakterm_terminal::grid::row::Row::new(80));
            pane.pin_copy_mode(1, 1).expect("in range");
            pane.screens
                .push_to_scrollback(oakterm_terminal::grid::row::Row::new(80));
            assert_eq!(pane.invalidate_pins_after_resize(1), 1);
        }

        let fp = FindPrompt {
            pane_id,
            from_offset: 0,
            direction: SearchDirection::Older,
        };
        let frame = Frame::new(0x75, 9, fp.encode()).expect("frame");
        let RequestResult::Response(reply) = find_prompt(1, &frame, &panes).await else {
            panic!("expected an error response");
        };
        assert_eq!(reply.msg_type, MSG_ERROR);
        let err = ErrorMessage::decode(&reply.payload).expect("decode");
        assert_eq!(err.code, ErrorCode::InvalidMessage as u32);

        let nav = SearchNav { pane_id };
        let frame = Frame::new(0x7A, 10, nav.encode()).expect("frame");
        let RequestResult::Response(reply) = search_prev(1, &frame, &panes).await else {
            panic!("expected an error response");
        };
        assert_eq!(reply.msg_type, MSG_ERROR);
        let err = ErrorMessage::decode(&reply.payload).expect("decode");
        assert_eq!(err.code, ErrorCode::InvalidMessage as u32);
    }

    /// The nav skip steps a pinned client's cursor past frozen-page
    /// matches instead of leaving it parked there reporting nothing —
    /// bounded by one full wrap, so an all-ineligible set terminates.
    #[tokio::test]
    async fn nav_skips_frozen_page_matches_for_a_pinned_client() {
        use crate::pane::PaneManager;
        use oakterm_protocol::message::{SearchFlags, SearchResults};
        use std::sync::Arc;
        use tokio::sync::Mutex as TokioMutex;

        let panes = Arc::new(TokioMutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            for text in ["hit one", "hit two", "hit three"] {
                let mut row = oakterm_terminal::grid::row::Row::new(80);
                for (i, ch) in text.chars().enumerate() {
                    row.cells[i].codepoint = ch;
                }
                pane.screens.push_to_scrollback(row);
            }
            // Rows 2.. are on the pinned client's frozen page.
            pane.pin_copy_mode(1, 2).expect("in range");
        }

        let search = SearchScrollback {
            pane_id,
            query: "hit".to_string(),
            flags: SearchFlags(0),
        };
        let frame = Frame::new(0x77, 9, search.encode().expect("encode")).expect("frame");
        let _ = search_scrollback(1, &frame, &panes).await;

        // `next` from the newest eligible match wraps over the frozen-page
        // match back onto an eligible one rather than parking on it.
        let nav = SearchNav { pane_id };
        let frame = Frame::new(0x79, 10, nav.encode()).expect("frame");
        let RequestResult::Response(reply) = search_next(1, &frame, &panes).await else {
            panic!("expected SearchResults");
        };
        let results = SearchResults::decode(&reply.payload).expect("decode");
        assert!(
            results.active_index.is_some(),
            "nav must land on an eligible match, not park on a frozen-page one"
        );
        assert!(results.active_row_offset < 0, "eligible = below the pin");
    }

    /// One pinned client's clamped search must not shrink the shared
    /// engine: an unpinned client's follow-up nav still sees every match
    /// (ADR-0025 clause 8 is per-response, the engine is pane-wide).
    #[tokio::test]
    async fn a_pinned_search_does_not_shrink_another_clients_results() {
        use crate::pane::PaneManager;
        use oakterm_protocol::message::{SearchFlags, SearchResults};
        use std::sync::Arc;
        use tokio::sync::Mutex as TokioMutex;

        let panes = Arc::new(TokioMutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            for text in ["hit one", "miss", "hit two", "hit three"] {
                let mut row = oakterm_terminal::grid::row::Row::new(80);
                for (i, ch) in text.chars().enumerate() {
                    row.cells[i].codepoint = ch;
                }
                pane.screens.push_to_scrollback(row);
            }
            // Conn 1 pinned after "hit one" and "miss": rows 2..4 are on
            // its frozen page in pin space.
            pane.pin_copy_mode(1, 2).expect("in range");
        }

        let search = SearchScrollback {
            pane_id,
            query: "hit".to_string(),
            flags: SearchFlags(0),
        };
        let frame = Frame::new(0x77, 9, search.encode().expect("encode")).expect("frame");
        let RequestResult::Response(reply) = search_scrollback(1, &frame, &panes).await else {
            panic!("expected SearchResults");
        };
        let pinned = SearchResults::decode(&reply.payload).expect("decode");
        assert_eq!(pinned.total_matches, 1, "conn 1 sees only its history");

        let nav = SearchNav { pane_id };
        let frame = Frame::new(0x79, 10, nav.encode()).expect("frame");
        let RequestResult::Response(reply) = search_next(2, &frame, &panes).await else {
            panic!("expected SearchResults");
        };
        let unpinned = SearchResults::decode(&reply.payload).expect("decode");
        assert_eq!(
            unpinned.total_matches, 3,
            "the shared engine kept every match for the unpinned client"
        );
    }

    /// Push rows into a buffer, marking specific indices as `PromptStart`.
    fn buffer_with_prompts(total: usize, prompt_indices: &[usize]) -> HotBuffer {
        let mut buf = HotBuffer::new(10 * 1024 * 1024);
        for i in 0..total {
            let mut row = Row::new(80);
            if prompt_indices.contains(&i) {
                row.semantic_mark = SemanticMark::PromptStart;
            }
            buf.push(row);
        }
        buf
    }

    #[test]
    fn find_prompt_backward_finds_nearest() {
        // Rows: [P, _, _, P, _, _, _, _, _, _]  (P at 0 and 3)
        let buf = buffer_with_prompts(10, &[0, 3]);
        // Search backward from offset -5 (index 5)
        let result = find_prompt_unpinned(&buf, -5, SearchDirection::Older);
        // Nearest prompt before index 5 is at index 3 → offset = 3 - 10 = -7
        assert_eq!(result, Some(-7));
    }

    #[test]
    fn find_prompt_backward_skips_current() {
        // Rows: [_, _, _, P, _, _]  (P at 3)
        let buf = buffer_with_prompts(6, &[3]);
        // Search backward from index 3 (offset -3): should skip index 3 itself
        let result = find_prompt_unpinned(&buf, -3, SearchDirection::Older);
        assert_eq!(result, None);
    }

    #[test]
    fn find_prompt_forward_finds_nearest() {
        // Rows: [_, _, _, _, P, _, _, P, _, _]  (P at 4 and 7)
        let buf = buffer_with_prompts(10, &[4, 7]);
        // Search forward from offset -8 (index 2)
        let result = find_prompt_unpinned(&buf, -8, SearchDirection::Newer);
        // Nearest prompt after index 2 is at index 4 → offset = 4 - 10 = -6
        assert_eq!(result, Some(-6));
    }

    #[test]
    fn find_prompt_forward_skips_current() {
        // Rows: [_, _, _, P, _, _]  (P at 3)
        let buf = buffer_with_prompts(6, &[3]);
        // Search forward from index 3 (offset -3): should skip index 3
        let result = find_prompt_unpinned(&buf, -3, SearchDirection::Newer);
        assert_eq!(result, None);
    }

    #[test]
    fn find_prompt_empty_buffer() {
        let buf = HotBuffer::new(1024);
        assert_eq!(find_prompt_unpinned(&buf, 0, SearchDirection::Older), None);
        assert_eq!(find_prompt_unpinned(&buf, 0, SearchDirection::Newer), None);
    }

    #[test]
    fn find_prompt_no_prompts_in_buffer() {
        let buf = buffer_with_prompts(10, &[]);
        assert_eq!(find_prompt_unpinned(&buf, -5, SearchDirection::Older), None);
        assert_eq!(find_prompt_unpinned(&buf, -5, SearchDirection::Newer), None);
    }

    #[test]
    fn find_prompt_offset_clamped_to_zero() {
        // offset more negative than buffer length → clamped to index 0
        let buf = buffer_with_prompts(5, &[2]);
        let result = find_prompt_unpinned(&buf, -100, SearchDirection::Newer);
        assert_eq!(result, Some(-3)); // index 2 → 2 - 5 = -3
    }

    #[test]
    fn find_prompt_at_live_view() {
        // offset 0 means live view (from_idx = buf.len())
        let buf = buffer_with_prompts(5, &[1, 3]);
        // Backward from live should find the last prompt (index 3)
        let result = find_prompt_unpinned(&buf, 0, SearchDirection::Older);
        assert_eq!(result, Some(-2)); // index 3 → 3 - 5 = -2
        // Forward from live: nothing after buf.len()
        let result = find_prompt_unpinned(&buf, 0, SearchDirection::Newer);
        assert_eq!(result, None);
    }

    #[test]
    fn find_prompt_offset_roundtrip() {
        // Verify the offset produced by find_prompt_in_buffer converts back
        // to the correct viewport_offset via checked_neg + u32::try_from.
        let buf = buffer_with_prompts(100, &[25, 50, 75]);
        let offset = find_prompt_unpinned(&buf, -30, SearchDirection::Older)
            .expect("should find prompt at index 50");
        // from_idx = 100 + (-30) = 70; nearest prompt before 70 is at index 50
        assert_eq!(offset, -50); // 50 - 100 = -50
        // Client conversion: negate to get positive viewport_offset
        let viewport = offset
            .checked_neg()
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        assert_eq!(viewport, 50);
    }

    #[test]
    fn osc_133_prompt_marks_reach_scrollback_and_are_found() {
        // End-to-end: an OSC 133;A prompt fed through the VT parser must be
        // decoded onto its row, scroll into the hot buffer, and be locatable
        // by find_prompt_in_buffer — the path scroll-to-prompt depends on.
        use oakterm_terminal::grid::ScreenSet;

        let mut ss = ScreenSet::new(80, 3);
        let mut sink = Vec::new();
        ss.process_bytes(b"\x1b]133;A\x07prompt$ \r\n", &mut sink);
        // Scroll the marked prompt row off the top into scrollback.
        for _ in 0..5 {
            ss.process_bytes(b"output line\r\n", &mut sink);
        }

        assert!(
            !ss.scrollback().is_empty(),
            "prompt row should have scrolled into the hot buffer"
        );
        let found = find_prompt_unpinned(ss.scrollback(), 0, SearchDirection::Older);
        assert!(
            found.is_some(),
            "find_prompt_in_buffer should locate the decoded OSC 133;A mark"
        );
    }
}
