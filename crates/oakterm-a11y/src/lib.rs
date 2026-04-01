//! AccessKit accessibility tree construction for terminal content
//! per Spec-0006. Decoupled from the GUI and daemon; operates on
//! plain text and dimensions.

use accesskit::{
    Action, Live, Node, NodeId, Rect, Role, TextPosition, TextSelection, Tree, TreeId, TreeUpdate,
};

pub const WINDOW_ID: NodeId = NodeId(0);
pub const TERMINAL_ID: NodeId = NodeId(1);
pub const ANNOUNCEMENT_ID: NodeId = NodeId(2);
const ROW_ID_OFFSET: u64 = 1000;

#[must_use]
pub fn row_node_id(visible_row: usize) -> NodeId {
    NodeId(visible_row as u64 + ROW_ID_OFFSET)
}

pub struct TreeInput<'a> {
    pub rows: u16,
    pub cols: u16,
    pub row_texts: &'a [String],
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub title: &'a str,
    pub scrollback_lines: u64,
    pub cell_width: f64,
    pub cell_height: f64,
}

/// Build the complete initial accessibility tree per Spec-0006.
#[must_use]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::cast_precision_loss
)]
pub fn build_initial_tree(input: &TreeInput<'_>) -> TreeUpdate {
    let mut nodes = Vec::with_capacity(3 + input.rows as usize);

    // Window node
    let mut window = Node::new(Role::Window);
    window.set_children(vec![TERMINAL_ID]);
    nodes.push((WINDOW_ID, window));

    // Terminal node
    let mut terminal = Node::new(Role::Terminal);
    terminal.set_label(input.title);
    terminal.set_row_count(input.rows as usize);
    terminal.set_column_count(input.cols as usize);
    terminal.set_scroll_y(0.0);
    terminal.set_scroll_y_min(0.0);
    terminal.set_scroll_y_max(input.scrollback_lines as f64);
    terminal.add_action(Action::Focus);
    terminal.add_action(Action::ScrollUp);
    terminal.add_action(Action::ScrollDown);
    terminal.add_action(Action::SetScrollOffset);
    terminal.add_action(Action::SetTextSelection);

    let mut children: Vec<NodeId> = (0..input.rows as usize).map(row_node_id).collect();
    children.push(ANNOUNCEMENT_ID);
    terminal.set_children(children);

    // Cursor as text selection
    let cursor_row = if input.rows == 0 {
        0
    } else {
        (input.cursor_row as usize).min(input.rows as usize - 1)
    };
    // Clamp cursor_col to the row text length so the selection doesn't
    // point past the end of the trimmed text.
    let row_text_len = input
        .row_texts
        .get(cursor_row)
        .map_or(0, |t| t.chars().count());
    let cursor_col = (input.cursor_col as usize).min(row_text_len);
    terminal.set_text_selection(TextSelection {
        anchor: TextPosition {
            node: row_node_id(cursor_row),
            character_index: cursor_col,
        },
        focus: TextPosition {
            node: row_node_id(cursor_row),
            character_index: cursor_col,
        },
    });

    nodes.push((TERMINAL_ID, terminal));

    // TextRun nodes per visible row
    for row_idx in 0..input.rows as usize {
        let text = input.row_texts.get(row_idx).map_or("", String::as_str);
        let text_run = build_text_run(
            text,
            row_idx,
            input.cols,
            input.cell_width,
            input.cell_height,
        );
        nodes.push((row_node_id(row_idx), text_run));
    }

    // Announcement node (empty initially)
    let mut announcement = Node::new(Role::Label);
    announcement.set_live(Live::Polite);
    announcement.set_value("");
    nodes.push((ANNOUNCEMENT_ID, announcement));

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(WINDOW_ID)),
        tree_id: TreeId::ROOT,
        focus: TERMINAL_ID,
    }
}

#[allow(clippy::cast_precision_loss)]
fn build_text_run(
    text: &str,
    row_idx: usize,
    cols: u16,
    cell_width: f64,
    cell_height: f64,
) -> Node {
    let mut node = Node::new(Role::TextRun);
    node.set_value(text);
    node.set_character_lengths(character_lengths(text));
    node.set_word_starts(word_starts(text));
    node.set_bounds(Rect {
        x0: 0.0,
        y0: row_idx as f64 * cell_height,
        x1: f64::from(cols) * cell_width,
        y1: (row_idx + 1) as f64 * cell_height,
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
    fn initial_tree_node_count() {
        let texts: Vec<String> = (0..24).map(|_| String::new()).collect();
        let input = TreeInput {
            rows: 24,
            cols: 80,
            row_texts: &texts,
            cursor_row: 0,
            cursor_col: 0,
            title: "test",
            scrollback_lines: 0,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        // Window + Terminal + 24 rows + Announcement = 27
        assert_eq!(update.nodes.len(), 27);
    }

    #[test]
    fn initial_tree_has_root() {
        let texts = vec![String::new()];
        let input = TreeInput {
            rows: 1,
            cols: 80,
            row_texts: &texts,
            cursor_row: 0,
            cursor_col: 0,
            title: "term",
            scrollback_lines: 100,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        assert!(update.tree.is_some());
        assert_eq!(update.focus, TERMINAL_ID);
    }

    #[test]
    fn initial_tree_text_run_content() {
        let texts = vec!["hello world".to_string()];
        let input = TreeInput {
            rows: 1,
            cols: 80,
            row_texts: &texts,
            cursor_row: 0,
            cursor_col: 5,
            title: "test",
            scrollback_lines: 0,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        // Find the row 0 node
        let row_node = update
            .nodes
            .iter()
            .find(|(id, _)| *id == row_node_id(0))
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
    fn initial_tree_terminal_children() {
        let texts: Vec<String> = (0..3).map(|_| String::new()).collect();
        let input = TreeInput {
            rows: 3,
            cols: 80,
            row_texts: &texts,
            cursor_row: 0,
            cursor_col: 0,
            title: "",
            scrollback_lines: 0,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = build_initial_tree(&input);
        let terminal = update
            .nodes
            .iter()
            .find(|(id, _)| *id == TERMINAL_ID)
            .map(|(_, n)| n)
            .expect("terminal node missing");
        let children = terminal.children();
        // 3 rows + announcement
        assert_eq!(children.len(), 4);
        assert_eq!(children[0], row_node_id(0));
        assert_eq!(children[1], row_node_id(1));
        assert_eq!(children[2], row_node_id(2));
        assert_eq!(children[3], ANNOUNCEMENT_ID);
    }

    #[test]
    fn row_node_id_offset() {
        assert_eq!(row_node_id(0), NodeId(1000));
        assert_eq!(row_node_id(23), NodeId(1023));
    }
}
