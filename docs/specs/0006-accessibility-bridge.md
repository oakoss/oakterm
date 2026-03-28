---
spec: '0006'
title: Accessibility Bridge
status: draft
date: 2026-03-27
adrs: ['0001']
tags: [a11y, core]
---

# 0006. Accessibility Bridge

## Overview

Defines how the GUI process constructs and maintains an AccessKit accessibility tree from the screen buffer (Spec-0003). The tree enables screen readers (NVDA, VoiceOver, Orca) to read terminal content, navigate by character/word/line, track the cursor, and receive announcements of new output. Lazy activation ensures zero overhead when no assistive technology is connected. Implements ADR-0001.

## Contract

### Tree Structure

The AccessKit tree for a terminal window:

```text
Window (Role::Window, WINDOW_NODE_ID)
  └── Terminal (Role::Terminal, TERMINAL_NODE_ID)
        ├── TextRun (Role::TextRun, row_node_id(0))   -- visible row 0
        ├── TextRun (Role::TextRun, row_node_id(1))   -- visible row 1
        ├── ...
        ├── TextRun (Role::TextRun, row_node_id(N-1)) -- visible row N-1
        └── Announcement (Role::Label, ANNOUNCEMENT_NODE_ID)
```

**Window node:** Top-level container. `Role::Window`. Holds the terminal as its only child.

**Terminal node:** `Role::Terminal`. Contains all visible rows as TextRun children plus the announcement node. Properties:

- `label`: pane title (from OSC 0/2)
- `scroll_y`: current viewport offset (0 = bottom of scrollback)
- `scroll_y_min`: 0
- `scroll_y_max`: total scrollback line count
- `row_count`: number of visible rows
- `column_count`: number of columns
- `text_selection`: current cursor position or text selection (see Text Selection below)
- Actions: `Focus`, `ScrollUp`, `ScrollDown`, `SetScrollOffset`, `SetTextSelection`

**TextRun nodes:** One per visible row. `Role::TextRun`. Properties:

- `value`: the row's text content as a UTF-8 string (codepoints only, no escape sequences)
- `character_lengths`: array of `u8` values, one per character, indicating the UTF-8 byte length of each character in `value`
- `word_starts`: array of `u8` character indices where words begin (for word-by-word navigation). Limited to columns 0-255. For terminals wider than 255 columns, word starts beyond column 255 are omitted.
- `bounds`: bounding rectangle of the row in window coordinates (pixel x, y, width, height)

**Announcement node:** `Role::Label` with `Live::Polite`. Used to announce new terminal output to screen readers. Not visible — exists only in the accessibility tree.

### NodeId Strategy

```rust
const WINDOW_NODE_ID: NodeId = NodeId(0);
const TERMINAL_NODE_ID: NodeId = NodeId(1);
const ANNOUNCEMENT_NODE_ID: NodeId = NodeId(2);

/// Row node IDs start at offset 1000 to avoid collision with fixed IDs.
/// Uses the visible row index (0-based), not the absolute scrollback line number.
fn row_node_id(visible_row: usize) -> NodeId {
    NodeId((visible_row as u64) + 1000)
}
```

IDs are stable across updates as long as the grid dimensions don't change. When the grid resizes, all row node IDs are recalculated and a full tree update is sent.

### Adapter Integration

The GUI process creates the AccessKit adapter during window initialization, before the window is made visible.

```rust
/// Create the adapter using winit's event loop proxy.
let adapter = accesskit_winit::Adapter::with_event_loop_proxy(
    &event_loop,
    &window,
    proxy.clone(),  // EventLoopProxy<AccessKitEvent>
);
```

The adapter generates three event types that the GUI must handle:

```rust
enum AccessKitEvent {
    /// AT connected. Build and provide the full initial tree.
    InitialTreeRequested,

    /// AT requested an action (focus, scroll, text selection).
    ActionRequested(ActionRequest),

    /// AT disconnected. Stop building tree updates.
    AccessibilityDeactivated,
}
```

**Note on code examples:** The Rust code blocks below are illustrative. They show the AccessKit API usage pattern and the data flow from screen buffer to accessibility tree. Exact struct fields (e.g., `TreeUpdate` may require additional fields like `tree_id` depending on the AccessKit version) are resolved during implementation.

### Tree Construction

#### Initial Tree

When `InitialTreeRequested` fires, build the complete tree from the current screen buffer state:

```rust
/// `title` comes from TitleChanged notifications (Spec-0001), not the Grid struct.
/// `scrollback_lines` comes from the scroll buffer (Spec-0004).
fn build_initial_tree(grid: &Grid, title: &str, scrollback_lines: u64) -> TreeUpdate {
    let mut nodes = Vec::new();

    // Window node
    let mut window = Node::new(Role::Window);
    window.set_children(vec![TERMINAL_NODE_ID]);
    nodes.push((WINDOW_NODE_ID, window));

    // Terminal node
    let mut terminal = Node::new(Role::Terminal);
    terminal.set_label(title);
    terminal.set_row_count(grid.rows as usize);
    terminal.set_column_count(grid.cols as usize);
    terminal.set_scroll_y(0.0);
    terminal.set_scroll_y_min(0.0);
    terminal.set_scroll_y_max(scrollback_lines as f64);
    terminal.add_action(Action::Focus);
    terminal.add_action(Action::ScrollUp);
    terminal.add_action(Action::ScrollDown);
    terminal.add_action(Action::SetScrollOffset);
    terminal.add_action(Action::SetTextSelection);

    let mut children: Vec<NodeId> = (0..grid.rows as usize)
        .map(row_node_id)
        .collect();
    children.push(ANNOUNCEMENT_NODE_ID);
    terminal.set_children(children);

    // Cursor / selection
    let selection = build_text_selection(grid);
    terminal.set_text_selection(selection);

    nodes.push((TERMINAL_NODE_ID, terminal));

    // Row TextRun nodes
    for row_idx in 0..grid.rows as usize {
        let row = &grid.lines[row_idx];
        let text_run = build_text_run(row, row_idx, grid.cols);
        nodes.push((row_node_id(row_idx), text_run));
    }

    // Announcement node (empty initially)
    let mut announcement = Node::new(Role::Label);
    announcement.set_live(Live::Polite);
    announcement.set_value("");
    nodes.push((ANNOUNCEMENT_NODE_ID, announcement));

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(WINDOW_NODE_ID)),
        focus: TERMINAL_NODE_ID,
    }
}
```

#### Incremental Updates

On each screen buffer change (when the GUI receives `DirtyNotify` and pulls a `RenderUpdate` from the daemon), update only the changed nodes:

```rust
fn build_incremental_update(
    grid: &Grid,
    dirty_rows: &[u16],
    cursor_changed: bool,
    title: Option<&str>,         // Some if title changed
    selection: Option<&Selection>,
    announcement: Option<(&str, Live)>,  // (text, Polite or Assertive)
    cell_width: f64,
    cell_height: f64,
) -> TreeUpdate {
    let mut nodes = Vec::new();

    // Update dirty row TextRuns
    for &row_idx in dirty_rows {
        let row = &grid.lines[row_idx as usize];
        let text_run = build_text_run(row, row_idx as usize, grid.cols);
        nodes.push((row_node_id(row_idx as usize), text_run));
    }

    // Update terminal node if cursor or title changed
    if cursor_changed || title.is_some() {
        let mut terminal = Node::new(Role::Terminal);
        if let Some(t) = title {
            terminal.set_label(t);
        }
        if cursor_changed {
            terminal.set_text_selection(build_text_selection(
                &grid.cursor, selection, grid.rows,
            ));
        }
        // Preserve children and other properties
        let children: Vec<NodeId> = (0..grid.rows as usize)
            .map(row_node_id)
            .chain(std::iter::once(ANNOUNCEMENT_NODE_ID))
            .collect();
        terminal.set_children(children);
        terminal.set_row_count(grid.rows as usize);
        terminal.set_column_count(grid.cols as usize);
        nodes.push((TERMINAL_NODE_ID, terminal));
    }

    // Announce new output via live region.
    // Live level is Polite for normal output, Assertive for bell.
    if let Some((text, live_level)) = announcement {
        let mut ann_node = Node::new(Role::Label);
        ann_node.set_live(live_level);
        ann_node.set_value(text);
        nodes.push((ANNOUNCEMENT_NODE_ID, ann_node));
    }

    TreeUpdate {
        nodes,
        tree: None,  // tree structure unchanged
        focus: TERMINAL_NODE_ID,
    }
}
```

#### Building a TextRun from a Row

```rust
/// `cell_width` and `cell_height` come from font metrics (the TextShaper per ADR-0009).
fn build_text_run(row: &Row, row_idx: usize, cols: u16, cell_width: f64, cell_height: f64) -> Node {
    let mut node = Node::new(Role::TextRun);

    // Extract text content from cells (codepoints only, no formatting)
    let text = row_to_string(row);
    let char_lengths = row_character_lengths(row);
    let word_starts = row_word_starts(row);

    node.set_value(&text);
    node.set_character_lengths(&char_lengths);
    node.set_word_starts(&word_starts);

    // Bounding rectangle (computed from row index and cell dimensions)
    node.set_bounds(Rect {
        x0: 0.0,
        y0: (row_idx as f64) * cell_height,
        x1: (cols as f64) * cell_width,
        y1: ((row_idx + 1) as f64) * cell_height,
    });

    node
}
```

**`row_to_string`:** Concatenates the codepoints from all cells in the row, including grapheme clusters. Wide character continuation cells (`WideCont`) are skipped. Empty trailing cells are trimmed.

**`row_character_lengths`:** For each character in the row string, the number of UTF-8 bytes it occupies. ASCII = 1, most Unicode = 2-4. This enables character-by-character navigation by screen readers.

**`row_word_starts`:** Character indices (not byte indices) where words begin. Words are delimited by whitespace and punctuation. A simple heuristic: a word starts after a whitespace character or at column 0. Shell prompts and command output use this for word-by-word navigation.

### Text Selection

Maps the terminal cursor and selection to AccessKit's `TextSelection`.

```rust
/// Selection is tracked separately from Grid (see Spec-0003).
/// Only selections within the visible viewport are represented in the a11y tree.
/// Scrollback selections (negative row indices) are clamped to the viewport boundary.
fn build_text_selection(
    cursor: &Cursor,
    selection: Option<&Selection>,
    visible_rows: u16,
) -> TextSelection {
    if let Some(sel) = selection {
        // Clamp to visible viewport (row 0..visible_rows)
        let start_row = sel.start.row.clamp(0, visible_rows as i64 - 1) as usize;
        let end_row = sel.end.row.clamp(0, visible_rows as i64 - 1) as usize;
        TextSelection {
            anchor: TextPosition {
                node: row_node_id(start_row),
                character_index: sel.start.col as usize,
            },
            focus: TextPosition {
                node: row_node_id(end_row),
                character_index: sel.end.col as usize,
            },
        }
    } else {
        // Cursor only (zero-width selection)
        let pos = TextPosition {
            node: row_node_id(cursor.row as usize),
            character_index: cursor.col as usize,
        };
        TextSelection {
            anchor: pos,
            focus: pos,
        }
    }
}
```

### Lazy Activation

The accessibility tree is only built and maintained when an assistive technology is connected.

```rust
// In the render loop, after receiving a RenderUpdate:
adapter.update_if_active(|| {
    // This closure is ONLY called if AT is active.
    // When no screen reader is connected, this is a no-op.
    build_incremental_update(
        &grid, &dirty_rows, cursor_changed, title,
        selection.as_ref(), announcement, cell_width, cell_height,
    )
});
```

**State transitions:**

| State        | AT Connected         | Behavior                                                        |
| ------------ | -------------------- | --------------------------------------------------------------- |
| Inactive     | No                   | `update_if_active` skips the closure. Zero cost.                |
| Activating   | Yes (just connected) | `InitialTreeRequested` fires. GUI builds full tree.             |
| Active       | Yes                  | `update_if_active` calls the closure. Incremental updates sent. |
| Deactivating | Disconnecting        | `AccessibilityDeactivated` fires. GUI stops building updates.   |

### Action Handling

When the screen reader requests an action, the GUI translates it to a daemon command.

```rust
/// Scrolling is a GUI-local operation (viewport offset change).
/// The GUI fetches scrollback rows from the daemon via GetScrollback (Spec-0001)
/// when the viewport moves into unfetched territory.
fn handle_action(request: ActionRequest, viewport: &mut ViewportState) {
    match request.action {
        Action::Focus => {
            // Focus the terminal pane. GUI-local.
        }

        Action::ScrollUp => {
            // Scroll viewport up by one page.
            viewport.scroll_by(-(viewport.rows as i64));
        }

        Action::ScrollDown => {
            // Scroll viewport down by one page.
            viewport.scroll_by(viewport.rows as i64);
        }

        Action::SetScrollOffset => {
            if let Some(ActionData::SetScrollOffset(point)) = request.data {
                // Scroll to absolute position.
                viewport.scroll_to(point.y as i64);
            }
        }

        Action::SetTextSelection => {
            if let Some(ActionData::SetTextSelection(selection)) = request.data {
                // AT is requesting a text selection change.
                viewport.update_selection_from_a11y(selection);
            }
        }

        _ => {} // Ignore unsupported actions
    }
}
```

### Live Region Announcements

New terminal output is announced to screen readers via the announcement node.

**What gets announced:**

| Scenario                      | Announcement Content                       | Live Level  |
| ----------------------------- | ------------------------------------------ | ----------- |
| New line(s) of output         | The new line(s) text, concatenated         | `Polite`    |
| Bell character (BEL)          | "Bell" or configured bell text             | `Assertive` |
| Command completed (OSC 133;D) | "Command finished" + exit code if non-zero | `Polite`    |
| Pane title changed            | New title                                  | `Polite`    |

**Announcement strategy:** Announce the text of new lines that appear at the bottom of the terminal (scroll region output). Do not announce cursor movement, screen redraws, or alternate screen content — these produce excessive noise.

**Announcement debouncing:** If multiple lines arrive within a single frame, concatenate them into one announcement. Rapid output (e.g., `cat large_file`) is debounced: announce at most once per 100ms. The debounce prevents screen reader queue flooding.

### Grid Resize

When the terminal grid resizes:

1. All row node IDs are recalculated (row count may have changed).
2. The terminal node's `children`, `row_count`, and `column_count` are updated.
3. A full tree update is sent (all rows + terminal node).
4. This is the only case where a non-incremental update is needed after the initial tree.

### Winit Event Integration

The AccessKit adapter must process every winit `WindowEvent`:

```rust
// In the event loop:
fn handle_window_event(&mut self, event: &WinitWindowEvent) {
    // AccessKit must see the event first
    self.adapter.process_event(&self.window, event);

    // Then handle normally
    match event {
        // ... input handling, resize, etc.
    }
}
```

## Behavior

### Normal Operation

1. GUI creates the AccessKit adapter before showing the window.
2. No AT connected: `update_if_active` is a no-op on every frame. Zero overhead.
3. AT connects (VoiceOver started, NVDA launched): `InitialTreeRequested` fires.
4. GUI builds the full tree from the current screen buffer and provides it.
5. Screen reader announces the terminal content. User can navigate by character, word, or line.
6. On each `RenderUpdate` from the daemon, GUI calls `update_if_active` with an incremental update containing only changed rows.
7. New output is announced via the live region node.
8. AT disconnects: `AccessibilityDeactivated` fires. GUI stops building tree updates.

### Empty Terminal

When the terminal has no content (e.g., before the shell starts):

- All TextRun nodes have empty `value` strings.
- The cursor is at row 0, column 0.
- The announcement node is empty.
- The terminal node has a label but no meaningful content.

### Alternate Screen

When the alternate screen is active (Spec-0003 ScreenSet):

- The accessibility tree reflects the alternate grid's content, not the primary grid.
- TextRun nodes are rebuilt from the alternate grid's rows.
- Scroll properties reflect the alternate screen (no scrollback: `scroll_y_max = 0`).
- When switching back to the primary screen, the tree is rebuilt from the primary grid.

### Scrollback Navigation

When the user scrolls through scrollback (viewport offset changes):

- The TextRun nodes reflect the visible rows at the current viewport offset.
- The terminal node's `scroll_y` is updated.
- All visible row TextRuns are rebuilt (the content has changed).
- The screen reader can use `ScrollUp`/`ScrollDown` actions to navigate scrollback.

### Multi-Pane (Future)

Phase 0 supports a single pane. In Phase 1 (multiplexer), each pane gets its own Terminal node in the tree. The window node's children expand to include multiple Terminal nodes. This spec covers the single-pane case; multi-pane a11y is deferred.

## Constraints

- **Inactive overhead:** ~0ns per frame. `update_if_active` checks an enum variant and returns.
- **Active overhead:** Target < 0.5ms per frame for incremental updates. A typical update (1-3 changed rows) builds ~3 Node objects with string properties. The AccessKit platform adapter handles threading and AT communication independently.
- **Initial tree build:** Target < 2ms for a 200x50 grid (250 nodes with text content).
- **Memory (inactive):** Zero. No tree exists in memory when no AT is connected.
- **Memory (active):** Proportional to visible rows. ~26 nodes for 24-row terminal. Each node is sparse (only set properties consume memory).
- **Announcement debounce:** At most one announcement per 100ms during rapid output.
- **Platform support:** Windows (UI Automation) and macOS (NSAccessibility) from Phase 0. Linux (AT-SPI via `accesskit_unix`) when the adapter reaches production stability.

## References

- [ADR 0001: Accessibility in Phase 0](../adrs/0001-accessibility-in-phase-zero.md)
- [Spec 0003: Screen Buffer](0003-screen-buffer.md) — Grid, Row, Cursor, Selection types
- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — DirtyNotify and RenderUpdate
- [17-accessibility.md](../ideas/17-accessibility.md)
- [AccessKit GitHub](https://github.com/AccessKit/accesskit)
- [AccessKit Role::Terminal](https://docs.rs/accesskit/latest/accesskit/enum.Role.html)
- [accesskit_winit crate](https://docs.rs/accesskit_winit/latest/accesskit_winit/)
- [Windows Terminal UIA architecture (PR #1691)](https://github.com/microsoft/terminal/pull/1691)
