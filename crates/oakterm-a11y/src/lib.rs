//! AccessKit accessibility tree construction for terminal content
//! per Spec-0006. Decoupled from the GUI and daemon; operates on
//! plain text and dimensions.
//!
//! The tree holds one Terminal subtree per pane under a single Window,
//! plus one shared announcement node. Node IDs are namespaced by pane
//! so subtrees never collide: each pane owns the ID block starting at
//! `(pane_id + 1) * PANE_STRIDE`.

use accesskit::{
    Action, Live, Node, NodeId, Rect, Role, TextPosition, TextSelection, Tree, TreeId, TreeUpdate,
};

pub const WINDOW_ID: NodeId = NodeId(0);
/// Shared live-region node; its sole parent is the Window (AccessKit
/// requires exactly one parent per node).
pub const ANNOUNCEMENT_ID: NodeId = NodeId(1);

/// ID block size per pane: terminal node at the block base, rows at
/// base + 1 + row. Rows are u16 (max 65535), so blocks never overlap.
const PANE_STRIDE: u64 = 1 << 20;

#[must_use]
pub fn terminal_node_id(pane_id: u32) -> NodeId {
    NodeId((u64::from(pane_id) + 1) * PANE_STRIDE)
}

#[must_use]
pub fn row_node_id(pane_id: u32, visible_row: u16) -> NodeId {
    NodeId(terminal_node_id(pane_id).0 + 1 + u64::from(visible_row))
}

/// Exact inverse of [`row_node_id`]/[`terminal_node_id`]: which pane a
/// node belongs to, and the visible row when the node is a row. Returns
/// `None` for the window/announcement nodes and any ID the encoders never
/// mint (out-of-range pane or row).
#[must_use]
pub fn decode_node_id(id: NodeId) -> Option<(u32, Option<u16>)> {
    let raw = id.0;
    if raw < PANE_STRIDE {
        return None;
    }
    let pane_id = u32::try_from(raw / PANE_STRIDE - 1).ok()?;
    let rem = raw % PANE_STRIDE;
    if rem == 0 {
        Some((pane_id, None))
    } else {
        let row = u16::try_from(rem - 1).ok()?;
        Some((pane_id, Some(row)))
    }
}

/// A text selection in visible-viewport coordinates. Columns are
/// between-character positions (0 = before the first character), so a
/// selection covering cells `a..=b` has `anchor_col = a`,
/// `focus_col = b + 1`. `usize::MAX` in a column means end-of-row; every
/// consumer clamps columns to the row's text length (or `cols` when no
/// text is available). Rows must already be clamped to the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRange {
    pub anchor_row: u16,
    pub anchor_col: usize,
    pub focus_row: u16,
    pub focus_col: usize,
}

pub struct PaneInput<'a> {
    pub pane_id: u32,
    pub rows: u16,
    pub cols: u16,
    pub row_texts: &'a [String],
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub title: &'a str,
    pub scrollback_lines: u64,
    /// Current viewport offset (0 = live view at the bottom).
    pub scroll_offset: u64,
    pub selection: Option<SelectionRange>,
    /// Pixel origin of the pane within the window, for row bounds.
    pub origin: (f64, f64),
}

pub struct TreeInput<'a> {
    pub panes: &'a [PaneInput<'a>],
    /// Pane whose terminal node receives focus.
    pub focused: u32,
    pub cell_width: f64,
    pub cell_height: f64,
}

/// Build the complete initial accessibility tree per Spec-0006.
#[must_use]
pub fn build_initial_tree(input: &TreeInput<'_>) -> TreeUpdate {
    let total_rows: usize = input.panes.iter().map(|p| p.rows as usize).sum();
    let mut nodes = Vec::with_capacity(2 + input.panes.len() + total_rows);

    let mut window = Node::new(Role::Window);
    let mut window_children: Vec<NodeId> = input
        .panes
        .iter()
        .map(|p| terminal_node_id(p.pane_id))
        .collect();
    window_children.push(ANNOUNCEMENT_ID);
    window.set_children(window_children);
    nodes.push((WINDOW_ID, window));

    for pane in input.panes {
        let cursor_row = if pane.rows == 0 {
            0
        } else {
            pane.cursor_row.min(pane.rows - 1)
        };
        let cursor_row_text = pane
            .row_texts
            .get(usize::from(cursor_row))
            .map_or("", String::as_str);
        nodes.push((
            terminal_node_id(pane.pane_id),
            build_terminal_node(pane, cursor_row_text),
        ));
        for row_idx in 0..pane.rows {
            let text = pane
                .row_texts
                .get(usize::from(row_idx))
                .map_or("", String::as_str);
            let text_run = build_text_run(
                text,
                usize::from(row_idx),
                pane.cols,
                input.cell_width,
                input.cell_height,
                pane.origin,
            );
            nodes.push((row_node_id(pane.pane_id, row_idx), text_run));
        }
    }

    // Announcement node (empty initially)
    let mut announcement = Node::new(Role::Label);
    announcement.set_live(Live::Polite);
    announcement.set_value("");
    nodes.push((ANNOUNCEMENT_ID, announcement));

    let focus = if input.panes.iter().any(|p| p.pane_id == input.focused) {
        terminal_node_id(input.focused)
    } else {
        WINDOW_ID
    };

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(WINDOW_ID)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// Text to announce to screen readers via the live region node.
pub struct Announcement {
    pub text: String,
    pub level: Live,
}

/// Input for an incremental tree update (per-frame). Targets a single
/// pane's subtree; the shared announcement node rides along.
pub struct IncrementalInput<'a> {
    pub pane_id: u32,
    /// Pane whose terminal node receives focus (may differ from `pane_id`).
    /// Callers must pass a pane that exists in the tree — the update's
    /// focus points at its terminal node unconditionally.
    pub focused: u32,
    pub rows: u16,
    pub cols: u16,
    /// Indices of rows whose content changed this frame.
    pub dirty_row_indices: &'a [u16],
    /// Text for each dirty row (parallel to `dirty_row_indices`).
    pub dirty_row_texts: &'a [String],
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_changed: bool,
    /// Text of the cursor's row (for clamping `cursor_col`). Needed because
    /// the cursor row may not be in the dirty set.
    pub cursor_row_text: &'a str,
    /// Current terminal title. Always set so the label isn't lost on
    /// cursor-only updates.
    pub title: &'a str,
    pub title_changed: bool,
    pub scrollback_lines: u64,
    pub scroll_offset: u64,
    pub selection: Option<SelectionRange>,
    pub selection_changed: bool,
    pub announcement: Option<&'a Announcement>,
    pub cell_width: f64,
    pub cell_height: f64,
    pub origin: (f64, f64),
}

/// Build an incremental tree update containing only changed nodes.
#[must_use]
pub fn build_incremental_update(input: &IncrementalInput<'_>) -> TreeUpdate {
    debug_assert_eq!(
        input.dirty_row_indices.len(),
        input.dirty_row_texts.len(),
        "dirty row indices and texts must be parallel"
    );
    let mut nodes = Vec::new();

    // Rebuild dirty row TextRuns.
    for (i, &row_idx) in input.dirty_row_indices.iter().enumerate() {
        let text = input.dirty_row_texts.get(i).map_or("", String::as_str);
        let text_run = build_text_run(
            text,
            usize::from(row_idx),
            input.cols,
            input.cell_width,
            input.cell_height,
            input.origin,
        );
        nodes.push((row_node_id(input.pane_id, row_idx), text_run));
    }

    // AccessKit overwrites entire nodes, so the terminal rebuild must
    // re-set every property (including scroll state and selection).
    if input.cursor_changed || input.title_changed || input.selection_changed {
        let pane = PaneInput {
            pane_id: input.pane_id,
            rows: input.rows,
            cols: input.cols,
            row_texts: &[],
            cursor_row: input.cursor_row,
            cursor_col: input.cursor_col,
            title: input.title,
            scrollback_lines: input.scrollback_lines,
            scroll_offset: input.scroll_offset,
            selection: input.selection,
            origin: input.origin,
        };
        let terminal = build_terminal_node(&pane, input.cursor_row_text);
        nodes.push((terminal_node_id(input.pane_id), terminal));
    }

    // Always push the announcement node so stale text is cleared.
    // Live regions trigger on value *changes*, so clearing to "" after
    // an announcement ensures the next identical text is re-announced.
    let mut ann_node = Node::new(Role::Label);
    if let Some(ann) = input.announcement {
        ann_node.set_live(ann.level);
        ann_node.set_value(ann.text.as_str());
    } else {
        ann_node.set_live(Live::Polite);
        ann_node.set_value("");
    }
    nodes.push((ANNOUNCEMENT_ID, ann_node));

    TreeUpdate {
        nodes,
        tree: None,
        tree_id: TreeId::ROOT,
        focus: terminal_node_id(input.focused),
    }
}

/// Children are the pane's rows only; the shared announcement node's sole
/// parent is the Window (AccessKit requires exactly one parent per node).
/// `cursor_row_text` clamps the collapsed-cursor position — incremental
/// callers have no `row_texts`, so it is passed explicitly.
#[allow(clippy::cast_precision_loss)]
fn build_terminal_node(pane: &PaneInput<'_>, cursor_row_text: &str) -> Node {
    let mut terminal = Node::new(Role::Terminal);
    terminal.set_label(pane.title);
    terminal.set_row_count(pane.rows as usize);
    terminal.set_column_count(pane.cols as usize);
    terminal.set_scroll_y(pane.scroll_offset as f64);
    terminal.set_scroll_y_min(0.0);
    terminal.set_scroll_y_max(pane.scrollback_lines as f64);
    terminal.add_action(Action::Focus);
    terminal.add_action(Action::ScrollUp);
    terminal.add_action(Action::ScrollDown);
    terminal.add_action(Action::SetScrollOffset);
    terminal.add_action(Action::SetTextSelection);

    let children: Vec<NodeId> = (0..pane.rows)
        .map(|r| row_node_id(pane.pane_id, r))
        .collect();
    terminal.set_children(children);

    set_text_selection(&mut terminal, pane, cursor_row_text);
    terminal
}

/// Set the terminal node's text selection: the tracked selection when
/// present, otherwise the cursor as a collapsed selection. Character
/// positions are clamped against the row text when available, and to the
/// column count otherwise (incremental callers pass no row texts and must
/// pre-clamp selections against their real text; `cols` is the backstop).
fn set_text_selection(terminal: &mut Node, pane: &PaneInput<'_>, cursor_row_text: &str) {
    let clamp_row = |row: u16| -> u16 {
        if pane.rows == 0 {
            0
        } else {
            row.min(pane.rows - 1)
        }
    };
    let selection = if let Some(sel) = pane.selection {
        let anchor_row = clamp_row(sel.anchor_row);
        let focus_row = clamp_row(sel.focus_row);
        let text_len = |row: u16| {
            pane.row_texts
                .get(usize::from(row))
                .map_or(usize::from(pane.cols), |t| t.chars().count())
        };
        TextSelection {
            anchor: TextPosition {
                node: row_node_id(pane.pane_id, anchor_row),
                character_index: sel.anchor_col.min(text_len(anchor_row)),
            },
            focus: TextPosition {
                node: row_node_id(pane.pane_id, focus_row),
                character_index: sel.focus_col.min(text_len(focus_row)),
            },
        }
    } else {
        let cursor_row = clamp_row(pane.cursor_row);
        let cursor_col = (pane.cursor_col as usize).min(cursor_row_text.chars().count());
        let pos = TextPosition {
            node: row_node_id(pane.pane_id, cursor_row),
            character_index: cursor_col,
        };
        TextSelection {
            anchor: pos,
            focus: pos,
        }
    };
    terminal.set_text_selection(selection);
}

#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn build_text_run(
    text: &str,
    row_idx: usize,
    cols: u16,
    cell_width: f64,
    cell_height: f64,
    origin: (f64, f64),
) -> Node {
    let mut node = Node::new(Role::TextRun);
    node.set_value(text);
    node.set_character_lengths(character_lengths(text));
    node.set_word_starts(word_starts(text));
    node.set_bounds(Rect {
        x0: origin.0,
        y0: origin.1 + row_idx as f64 * cell_height,
        x1: origin.0 + f64::from(cols) * cell_width,
        y1: origin.1 + (row_idx + 1) as f64 * cell_height,
    });
    node
}

/// UTF-8 byte length per character in the string.
#[must_use]
pub fn character_lengths(text: &str) -> Vec<u8> {
    // UTF-8 char lengths are 1-4, always fit in u8.
    #[allow(clippy::cast_possible_truncation)]
    text.chars().map(|c| c.len_utf8() as u8).collect()
}

/// Character indices where words begin (whitespace/punctuation delimited).
#[must_use]
pub fn word_starts(text: &str) -> Vec<u8> {
    let mut starts = Vec::new();
    let mut prev_is_boundary = true;
    for (i, c) in text.chars().enumerate() {
        if i > 255 {
            break; // u8 index limit per spec
        }
        let is_boundary = c.is_whitespace() || c.is_ascii_punctuation();
        if !is_boundary && prev_is_boundary {
            // Safe: loop breaks at i > 255.
            #[allow(clippy::cast_possible_truncation)]
            starts.push(i as u8);
        }
        prev_is_boundary = is_boundary;
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_input(pane_id: u32, rows: u16, row_texts: &[String]) -> PaneInput<'_> {
        PaneInput {
            pane_id,
            rows,
            cols: 80,
            row_texts,
            cursor_row: 0,
            cursor_col: 0,
            title: "test",
            scrollback_lines: 0,
            scroll_offset: 0,
            selection: None,
            origin: (0.0, 0.0),
        }
    }

    fn incremental_input<'a>(
        dirty_row_indices: &'a [u16],
        dirty_row_texts: &'a [String],
    ) -> IncrementalInput<'a> {
        IncrementalInput {
            pane_id: 1,
            focused: 1,
            rows: 24,
            cols: 80,
            dirty_row_indices,
            dirty_row_texts,
            cursor_row: 0,
            cursor_col: 0,
            cursor_changed: false,
            cursor_row_text: "",
            title: "",
            title_changed: false,
            scrollback_lines: 0,
            scroll_offset: 0,
            selection: None,
            selection_changed: false,
            announcement: None,
            cell_width: 8.0,
            cell_height: 16.0,
            origin: (0.0, 0.0),
        }
    }

    #[test]
    fn character_lengths_ascii() {
        assert_eq!(character_lengths("hello"), vec![1, 1, 1, 1, 1]);
    }

    #[test]
    fn character_lengths_multibyte() {
        // é = 2 bytes, 漢 = 3 bytes
        assert_eq!(character_lengths("é漢"), vec![2, 3]);
    }

    #[test]
    fn character_lengths_empty() {
        assert_eq!(character_lengths(""), Vec::<u8>::new());
    }

    #[test]
    fn word_starts_sentence() {
        // "hello world" → words start at 0 and 6
        assert_eq!(word_starts("hello world"), vec![0, 6]);
    }

    #[test]
    fn word_starts_leading_spaces() {
        assert_eq!(word_starts("  hello"), vec![2]);
    }

    #[test]
    fn word_starts_single_word() {
        assert_eq!(word_starts("hello"), vec![0]);
    }

    #[test]
    fn word_starts_all_spaces() {
        assert_eq!(word_starts("   "), Vec::<u8>::new());
    }

    #[test]
    fn word_starts_punctuation() {
        // "ls -la /tmp" → words at 0, 4, 8
        // l(0)s(1) (2)-(3)l(4)a(5) (6)/(7)t(8)m(9)p(10)
        assert_eq!(word_starts("ls -la /tmp"), vec![0, 4, 8]);
    }

    #[test]
    fn word_starts_empty() {
        assert_eq!(word_starts(""), Vec::<u8>::new());
    }

    #[test]
    fn node_id_namespacing_round_trips() {
        assert_eq!(decode_node_id(terminal_node_id(0)), Some((0, None)));
        assert_eq!(decode_node_id(row_node_id(0, 0)), Some((0, Some(0))));
        assert_eq!(decode_node_id(row_node_id(7, 23)), Some((7, Some(23))));
        assert_eq!(decode_node_id(WINDOW_ID), None);
        assert_eq!(decode_node_id(ANNOUNCEMENT_ID), None);
    }

    #[test]
    fn node_ids_disjoint_across_panes() {
        // Max row index (u16) in pane N stays below pane N+1's block.
        assert!(row_node_id(0, u16::MAX).0 < terminal_node_id(1).0);
    }

    #[test]
    fn decode_node_id_boundaries() {
        // Last ID below the first pane block is not a pane node.
        assert_eq!(decode_node_id(NodeId((1 << 20) - 1)), None);
        // Pane-id overflow (u64::MAX / STRIDE - 1 exceeds u32) is rejected.
        assert_eq!(decode_node_id(NodeId(u64::MAX)), None);
        // Largest mintable pane id round-trips.
        assert_eq!(
            decode_node_id(terminal_node_id(u32::MAX)),
            Some((u32::MAX, None))
        );
        // Remainders above u16::MAX are IDs row_node_id never mints; decode
        // rejects them instead of trusting consumers to no-op.
        assert_eq!(
            decode_node_id(NodeId(terminal_node_id(0).0 + 1 + 70_000)),
            None
        );
        // The largest mintable row round-trips.
        assert_eq!(
            decode_node_id(row_node_id(0, u16::MAX)),
            Some((0, Some(u16::MAX)))
        );
    }

    #[test]
    fn initial_tree_node_count() {
        let texts: Vec<String> = (0..24).map(|_| String::new()).collect();
        let panes = [pane_input(0, 24, &texts)];
        let input = TreeInput {
            panes: &panes,
            focused: 0,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        // Window + Terminal + 24 rows + Announcement = 27
        assert_eq!(update.nodes.len(), 27);
    }

    #[test]
    fn initial_tree_has_root_and_focus() {
        let texts = vec![String::new()];
        let panes = [pane_input(3, 1, &texts)];
        let input = TreeInput {
            panes: &panes,
            focused: 3,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        assert!(update.tree.is_some());
        assert_eq!(update.focus, terminal_node_id(3));
    }

    #[test]
    fn initial_tree_two_panes_shared_announcement() {
        let texts_a: Vec<String> = (0..2).map(|_| String::new()).collect();
        let texts_b: Vec<String> = (0..3).map(|_| String::new()).collect();
        let panes = [pane_input(1, 2, &texts_a), pane_input(4, 3, &texts_b)];
        let input = TreeInput {
            panes: &panes,
            focused: 4,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        // Window + 2 terminals + 5 rows + announcement = 9
        assert_eq!(update.nodes.len(), 9);
        let window = &update
            .nodes
            .iter()
            .find(|(id, _)| *id == WINDOW_ID)
            .expect("window")
            .1;
        assert_eq!(
            window.children(),
            &[terminal_node_id(1), terminal_node_id(4), ANNOUNCEMENT_ID]
        );
        assert_eq!(update.focus, terminal_node_id(4));
        // Each terminal's children live in its own ID block; the shared
        // announcement's only parent is the Window.
        let term_b = &update
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(4))
            .expect("terminal 4")
            .1;
        assert_eq!(
            term_b.children(),
            &[row_node_id(4, 0), row_node_id(4, 1), row_node_id(4, 2)]
        );
    }

    #[test]
    fn every_node_has_exactly_one_parent() {
        // AccessKit rejects trees where a node is referenced by two parents;
        // walk every child reference in the update and assert uniqueness.
        let texts_a: Vec<String> = (0..2).map(|_| String::new()).collect();
        let texts_b: Vec<String> = (0..3).map(|_| String::new()).collect();
        let panes = [pane_input(0, 2, &texts_a), pane_input(2, 3, &texts_b)];
        let input = TreeInput {
            panes: &panes,
            focused: 0,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        let mut seen = std::collections::HashSet::new();
        for (_, node) in &update.nodes {
            for child in node.children() {
                assert!(seen.insert(*child), "node {child:?} has two parents");
            }
        }
        // Every non-root node in the update is someone's child.
        for (id, _) in &update.nodes {
            if *id != WINDOW_ID {
                assert!(seen.contains(id), "node {id:?} is unparented");
            }
        }
    }

    #[test]
    fn initial_tree_unknown_focus_falls_back_to_window() {
        let texts = vec![String::new()];
        let panes = [pane_input(0, 1, &texts)];
        let input = TreeInput {
            panes: &panes,
            focused: 9,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        assert_eq!(build_initial_tree(&input).focus, WINDOW_ID);
    }

    #[test]
    fn initial_tree_text_run_content() {
        let texts = vec!["hello world".to_string()];
        let panes = [pane_input(0, 1, &texts)];
        let input = TreeInput {
            panes: &panes,
            focused: 0,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        let row_node = update
            .nodes
            .iter()
            .find(|(id, _)| *id == row_node_id(0, 0))
            .map(|(_, n)| n)
            .expect("row 0 node missing");
        assert_eq!(row_node.value(), Some("hello world"));
        assert_eq!(
            row_node.character_lengths(),
            &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]
        );
        assert_eq!(row_node.word_starts(), &[0, 6]);
    }

    #[test]
    fn initial_tree_carries_scroll_state() {
        let texts = vec![String::new()];
        let mut pane = pane_input(0, 1, &texts);
        pane.scrollback_lines = 500;
        pane.scroll_offset = 42;
        let panes = [pane];
        let input = TreeInput {
            panes: &panes,
            focused: 0,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        let terminal = &update
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(0))
            .expect("terminal")
            .1;
        assert_eq!(terminal.scroll_y(), Some(42.0));
        assert_eq!(terminal.scroll_y_max(), Some(500.0));
    }

    #[test]
    fn text_run_bounds_offset_by_origin() {
        let node = build_text_run("x", 2, 10, 8.0, 16.0, (100.0, 50.0));
        let bounds = node.bounds().expect("bounds");
        assert!((bounds.x0 - 100.0).abs() < f64::EPSILON);
        assert!((bounds.y0 - (50.0 + 32.0)).abs() < f64::EPSILON);
    }

    // --- Incremental update tests ---

    #[test]
    fn incremental_only_dirty_rows() {
        let texts = vec!["changed".to_string()];
        let input = incremental_input(&[5], &texts);
        let update = build_incremental_update(&input);
        assert!(update.tree.is_none());
        // Dirty row + announcement (cleared).
        assert_eq!(update.nodes.len(), 2);
        assert_eq!(update.nodes[0].0, row_node_id(1, 5));
        assert_eq!(update.nodes[0].1.value(), Some("changed"));
    }

    #[test]
    fn incremental_cursor_change_includes_terminal() {
        let texts = vec!["hello".to_string()];
        let mut input = incremental_input(&[0], &texts);
        input.cursor_col = 3;
        input.cursor_changed = true;
        input.cursor_row_text = "hello";
        input.title = "test";
        let update = build_incremental_update(&input);
        // Dirty row + terminal node + announcement.
        assert_eq!(update.nodes.len(), 3);
        let has_terminal = update
            .nodes
            .iter()
            .any(|(id, _)| *id == terminal_node_id(1));
        assert!(has_terminal);
    }

    #[test]
    fn incremental_cursor_clamped_to_cursor_row_text() {
        let mut input = incremental_input(&[], &[]);
        input.cursor_col = 70;
        input.cursor_changed = true;
        input.cursor_row_text = "short";
        let update = build_incremental_update(&input);
        let sel = update.nodes[0].1.text_selection().expect("selection");
        assert_eq!(sel.anchor.character_index, 5);
    }

    #[test]
    fn incremental_no_change_omits_terminal() {
        let input = incremental_input(&[], &[]);
        let update = build_incremental_update(&input);
        // Only the announcement node (cleared to "").
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(update.nodes[0].0, ANNOUNCEMENT_ID);
    }

    #[test]
    fn incremental_title_change_includes_terminal() {
        let mut input = incremental_input(&[], &[]);
        input.title = "new title";
        input.title_changed = true;
        let update = build_incremental_update(&input);
        // Terminal node + announcement.
        assert_eq!(update.nodes.len(), 2);
        assert_eq!(update.nodes[0].0, terminal_node_id(1));
        assert_eq!(update.nodes[0].1.label(), Some("new title"));
    }

    #[test]
    fn incremental_terminal_rebuild_carries_scroll_state() {
        let mut input = incremental_input(&[], &[]);
        input.cursor_changed = true;
        input.scrollback_lines = 300;
        input.scroll_offset = 17;
        let update = build_incremental_update(&input);
        let terminal = &update.nodes[0].1;
        assert_eq!(update.nodes[0].0, terminal_node_id(1));
        assert_eq!(terminal.scroll_y(), Some(17.0));
        assert_eq!(terminal.scroll_y_min(), Some(0.0));
        assert_eq!(terminal.scroll_y_max(), Some(300.0));
    }

    #[test]
    fn incremental_selection_change_includes_terminal() {
        let mut input = incremental_input(&[], &[]);
        input.selection = Some(SelectionRange {
            anchor_row: 1,
            anchor_col: 2,
            focus_row: 3,
            focus_col: 4,
        });
        input.selection_changed = true;
        let update = build_incremental_update(&input);
        assert_eq!(update.nodes[0].0, terminal_node_id(1));
        let sel = update.nodes[0].1.text_selection().expect("selection");
        assert_eq!(sel.anchor.node, row_node_id(1, 1));
        assert_eq!(sel.anchor.character_index, 2);
        assert_eq!(sel.focus.node, row_node_id(1, 3));
        assert_eq!(sel.focus.character_index, 4);
    }

    #[test]
    fn incremental_selection_cols_bounded_by_cols() {
        // Incremental rebuilds have no row texts; the Line-selection
        // usize::MAX sentinel must still be bounded (callers pre-clamp
        // against real text, cols is the crate-side backstop).
        let mut input = incremental_input(&[], &[]);
        input.selection = Some(SelectionRange {
            anchor_row: 0,
            anchor_col: 0,
            focus_row: 1,
            focus_col: usize::MAX,
        });
        input.selection_changed = true;
        let update = build_incremental_update(&input);
        let sel = update.nodes[0].1.text_selection().expect("selection");
        assert_eq!(sel.focus.character_index, 80);
    }

    #[test]
    fn incremental_focus_tracks_focused_pane() {
        let mut input = incremental_input(&[], &[]);
        input.pane_id = 2;
        input.focused = 5;
        let update = build_incremental_update(&input);
        assert_eq!(update.focus, terminal_node_id(5));
    }

    #[test]
    fn incremental_multiple_dirty_rows() {
        let texts = vec!["aaa".to_string(), "bbb".to_string()];
        let mut input = incremental_input(&[2, 7], &texts);
        input.rows = 10;
        input.cols = 40;
        let update = build_incremental_update(&input);
        // 2 dirty rows + announcement.
        assert_eq!(update.nodes.len(), 3);
        assert_eq!(update.nodes[0].0, row_node_id(1, 2));
        assert_eq!(update.nodes[1].0, row_node_id(1, 7));
    }

    #[test]
    fn incremental_announcement_polite() {
        let ann = Announcement {
            text: "hello world".into(),
            level: Live::Polite,
        };
        let mut input = incremental_input(&[], &[]);
        input.announcement = Some(&ann);
        let update = build_incremental_update(&input);
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(update.nodes[0].0, ANNOUNCEMENT_ID);
        assert_eq!(update.nodes[0].1.value(), Some("hello world"));
        assert_eq!(update.nodes[0].1.live(), Some(Live::Polite));
    }

    #[test]
    fn incremental_announcement_assertive() {
        let ann = Announcement {
            text: "Bell".into(),
            level: Live::Assertive,
        };
        let mut input = incremental_input(&[], &[]);
        input.announcement = Some(&ann);
        let update = build_incremental_update(&input);
        assert_eq!(update.nodes[0].1.live(), Some(Live::Assertive));
    }

    #[test]
    fn incremental_no_announcement_clears_node() {
        let input = incremental_input(&[], &[]);
        let update = build_incremental_update(&input);
        // Announcement node is always pushed (cleared to "" when no announcement).
        assert_eq!(update.nodes.len(), 1);
        assert_eq!(update.nodes[0].0, ANNOUNCEMENT_ID);
        assert_eq!(update.nodes[0].1.value(), Some(""));
    }

    #[test]
    fn selection_clamped_to_row_text() {
        let texts = vec!["ab".to_string(), "cdef".to_string()];
        let mut pane = pane_input(0, 2, &texts);
        pane.selection = Some(SelectionRange {
            anchor_row: 0,
            anchor_col: 99,
            focus_row: 1,
            focus_col: 99,
        });
        let panes = [pane];
        let input = TreeInput {
            panes: &panes,
            focused: 0,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        let terminal = &update
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(0))
            .expect("terminal")
            .1;
        let sel = terminal.text_selection().expect("selection");
        assert_eq!(sel.anchor.character_index, 2);
        assert_eq!(sel.focus.character_index, 4);
    }

    struct NoOpChanges;
    impl accesskit_consumer::TreeChangeHandler for NoOpChanges {
        fn node_added(&mut self, _: &accesskit_consumer::Node) {}
        fn node_updated(&mut self, _: &accesskit_consumer::Node, _: &accesskit_consumer::Node) {}
        fn focus_moved(
            &mut self,
            _: Option<&accesskit_consumer::Node>,
            _: Option<&accesskit_consumer::Node>,
        ) {
        }
        fn node_removed(&mut self, _: &accesskit_consumer::Node) {}
    }

    /// AccessKit's consumer panics on structurally invalid trees (duplicate
    /// parents, unparented nodes, missing focus). Feed real updates through
    /// it so tree-legality breaks fail in CI, not on AT activation.
    #[test]
    fn consumer_accepts_initial_and_incremental_updates() {
        let texts_a: Vec<String> = (0..2).map(|_| "ab".to_string()).collect();
        let texts_b: Vec<String> = (0..3).map(|_| "cd".to_string()).collect();
        for panes in [
            vec![pane_input(0, 2, &texts_a)],
            vec![pane_input(0, 2, &texts_a), pane_input(3, 3, &texts_b)],
        ] {
            let input = TreeInput {
                panes: &panes,
                focused: 0,
                cell_width: 8.0,
                cell_height: 16.0,
            };
            let mut tree = accesskit_consumer::Tree::new(build_initial_tree(&input), true);

            let dirty_texts = vec!["changed".to_string()];
            let mut inc = incremental_input(&[1], &dirty_texts);
            inc.pane_id = 0;
            inc.focused = 0;
            inc.rows = 2;
            inc.cursor_changed = true;
            inc.selection = Some(SelectionRange {
                anchor_row: 0,
                anchor_col: 1,
                focus_row: 1,
                focus_col: usize::MAX,
            });
            inc.selection_changed = true;
            let ann = Announcement {
                text: "new output".into(),
                level: Live::Polite,
            };
            inc.announcement = Some(&ann);
            tree.update_and_process_changes(build_incremental_update(&inc), &mut NoOpChanges);
        }
    }
}
