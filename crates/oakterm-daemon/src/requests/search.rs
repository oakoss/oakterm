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
    // Spec-0001: FindPrompt shares GetScrollback's coordinate space, so it
    // must resolve against the same origin — the copy-mode pin when the
    // client has one, or the live present otherwise.
    let found_offset = find_prompt_in_buffer(
        pane.screens.scrollback(),
        pane.copy_mode_base(conn_id),
        req.from_offset,
        req.direction,
    );
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
    pane.screens.set_search(engine);
    pane.screens.run_search();
    build_search_response(conn_id, &pane.screens, req.pane_id, frame.serial)
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
    if let Some(engine) = pane.screens.search_mut() {
        engine.next();
    } else {
        warn!(conn_id, "SearchNext with no active search");
    }
    build_search_response(conn_id, &pane.screens, req.pane_id, frame.serial)
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
    if let Some(engine) = pane.screens.search_mut() {
        engine.prev();
    } else {
        warn!(conn_id, "SearchPrev with no active search");
    }
    build_search_response(conn_id, &pane.screens, req.pane_id, frame.serial)
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
    pane_id: u32,
    serial: u32,
) -> RequestResult {
    let (total_matches, active_index, active_row_offset, capped) = match screens.search() {
        Some(engine) => {
            let total = u32::try_from(engine.match_count()).unwrap_or(u32::MAX);
            let (idx, offset) = match engine.active_match() {
                Some(m) => {
                    let buf_len = screens.scrollback().len();
                    let neg_offset = m.row as i64 - buf_len as i64;
                    (
                        engine
                            .active_index()
                            .map(|i| u32::try_from(i).unwrap_or(u32::MAX)),
                        neg_offset,
                    )
                }
                None => (None, 0),
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
