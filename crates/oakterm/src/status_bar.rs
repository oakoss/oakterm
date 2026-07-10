//! Status bar (Spec-0009): pure single-row segment layout shared by the
//! renderer and tests. Left side carries mode, workspace, and tabs; right
//! side carries the focused pane title and clock. Assembly and data
//! sourcing live in `frame.rs`/`main.rs`.

use crate::tab_bar::{self, TabInfo};
use oakterm_renderer::shaper::FontMetrics;

/// Which segment a cell belongs to, for styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Mode,
    Workspace,
    Tab { active: bool },
    Title,
    Clock,
}

/// One populated cell of the status bar row. Cells not present in the
/// layout render as bar background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusCell {
    pub ch: char,
    pub kind: SegmentKind,
}

/// Everything the status bar displays, borrowed from live GUI state.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusContent<'a> {
    /// Active mode name (e.g. "COPY"); `None` in normal mode hides the
    /// indicator (Spec-0009).
    pub mode: Option<&'a str>,
    pub workspace: &'a str,
    pub tabs: &'a [TabInfo],
    pub active_tab: Option<u32>,
    pub pane_title: &'a str,
    /// Pre-formatted wall-clock text (e.g. "14:30").
    pub clock: &'a str,
}

/// Lay out one status bar row: sparse `(col, cell)` pairs in column order.
/// Right side (clock, then title) is placed first; the left side truncates
/// where it would collide.
#[must_use]
pub fn layout_row(content: &StatusContent, cols: u16) -> Vec<(u16, StatusCell)> {
    let mut cells = Vec::new();
    let right_start = place_right(content, cols, &mut cells);
    place_left(content, right_start, &mut cells);
    cells.sort_by_key(|&(col, _)| col);
    cells
}

const TITLE_CLOCK_GAP: u16 = 2;

/// Place the clock at the right edge and the pane title before it.
/// Returns the first column the right side occupies (`cols` when empty),
/// which bounds the left side.
fn place_right(content: &StatusContent, cols: u16, cells: &mut Vec<(u16, StatusCell)>) -> u16 {
    let clock: Vec<char> = content.clock.chars().collect();
    let clock_len = u16::try_from(clock.len()).unwrap_or(u16::MAX);
    let mut right_start = cols;
    if clock_len > 0 && clock_len <= cols {
        right_start = cols - clock_len;
        push_text(cells, right_start, &clock, SegmentKind::Clock);
    }

    let title: Vec<char> = content.pane_title.chars().collect();
    let avail = usize::from(right_start.saturating_sub(TITLE_CLOCK_GAP));
    let shown = title.len().min(avail);
    if shown > 0 {
        let start = right_start - TITLE_CLOCK_GAP - u16::try_from(shown).unwrap_or(u16::MAX);
        push_text(cells, start, &title[..shown], SegmentKind::Title);
        right_start = start;
    }
    right_start
}

/// Place mode, workspace, and the tab strip from the left edge, clipped
/// one column short of `right_start` so the sides never touch.
fn place_left(content: &StatusContent, right_start: u16, cells: &mut Vec<(u16, StatusCell)>) {
    let limit = right_start.saturating_sub(1);
    let mut col: u16 = 0;
    if let Some(mode) = content.mode {
        let text: Vec<char> = format!("[{mode}]").chars().collect();
        col = push_clipped(cells, col, &text, SegmentKind::Mode, limit).saturating_add(1);
    }
    let workspace: Vec<char> = content.workspace.chars().collect();
    if !workspace.is_empty() {
        col = push_clipped(cells, col, &workspace, SegmentKind::Workspace, limit);
        col =
            push_clipped(cells, col, &[' ', '|'], SegmentKind::Workspace, limit).saturating_add(1);
    }
    if col >= limit {
        return;
    }
    let spans = tab_bar::layout_strip(content.tabs, limit - col);
    for (strip_col, cell) in tab_bar::strip_cells(content.tabs, content.active_tab, &spans) {
        let dest = col.saturating_add(strip_col);
        if dest < limit {
            cells.push((
                dest,
                StatusCell {
                    ch: cell.ch,
                    kind: SegmentKind::Tab {
                        active: cell.active,
                    },
                },
            ));
        }
    }
}

fn push_text(cells: &mut Vec<(u16, StatusCell)>, start: u16, text: &[char], kind: SegmentKind) {
    for (i, &ch) in text.iter().enumerate() {
        let Ok(offset) = u16::try_from(i) else { break };
        cells.push((start.saturating_add(offset), StatusCell { ch, kind }));
    }
}

/// Push `text` from `start`, stopping at `limit`. Returns the column
/// after the last cell written (or `limit` when clipped).
fn push_clipped(
    cells: &mut Vec<(u16, StatusCell)>,
    start: u16,
    text: &[char],
    kind: SegmentKind,
    limit: u16,
) -> u16 {
    let mut col = start;
    for &ch in text {
        if col >= limit {
            break;
        }
        cells.push((col, StatusCell { ch, kind }));
        col += 1;
    }
    col
}

/// 24-hour local time for the clock segment, e.g. "14:30" (Spec-0009).
pub(crate) fn clock_text() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// Seconds until the next minute boundary (1-60), for scheduling the
/// clock repaint.
pub(crate) fn seconds_to_next_minute() -> u64 {
    use chrono::Timelike;
    60 - u64::from(chrono::Local::now().second().min(59))
}

/// Status bar height in pixels: one cell row when enabled, else 0.
/// `None` metrics (no font yet) also yields 0.
pub(crate) fn status_bar_height(enabled: bool, metrics: Option<&FontMetrics>) -> u32 {
    if enabled {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        metrics.map_or(0, |m| m.cell_height.ceil().max(0.0) as u32)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{SegmentKind, StatusContent, layout_row};

    /// Render the sparse cells into a fixed-width string for worked-example
    /// assertions; empty cells become spaces.
    fn render(content: &StatusContent, cols: u16) -> String {
        let mut row = vec![' '; usize::from(cols)];
        for (col, cell) in layout_row(content, cols) {
            row[usize::from(col)] = cell.ch;
        }
        row.into_iter().collect()
    }

    #[test]
    fn clock_is_right_aligned() {
        let content = StatusContent {
            clock: "14:30",
            ..Default::default()
        };
        assert_eq!(render(&content, 20), "               14:30");
        let cells = layout_row(&content, 20);
        assert!(cells.iter().all(|(_, c)| c.kind == SegmentKind::Clock));
    }

    #[test]
    fn title_sits_before_the_clock_with_a_gap() {
        let content = StatusContent {
            pane_title: "~/project",
            clock: "14:30",
            ..Default::default()
        };
        assert_eq!(render(&content, 24), "        ~/project  14:30");
        let cells = layout_row(&content, 24);
        assert_eq!(
            cells
                .iter()
                .filter(|(_, c)| c.kind == SegmentKind::Title)
                .count(),
            9
        );
    }

    #[test]
    fn title_truncates_to_the_space_left_of_the_clock() {
        let content = StatusContent {
            pane_title: "~/very/long/path/to/somewhere",
            clock: "14:30",
            ..Default::default()
        };
        // 16 cols - 5 clock - 2 gap = 9 title cells, tail dropped.
        assert_eq!(render(&content, 16), "~/very/lo  14:30");
    }

    #[test]
    fn spec_default_layout_worked_example() {
        // Spec-0009 "Default layout" line, adjusted for the strip's
        // per-label padding cells.
        let tabs = [tab(1, "code"), tab(2, "git"), tab(3, "logs")];
        let content = StatusContent {
            mode: Some("COPY"),
            workspace: "work",
            tabs: &tabs,
            active_tab: Some(1),
            pane_title: "~/project",
            clock: "14:30",
        };
        assert_eq!(
            render(&content, 73),
            "[COPY] work |  1:code   2:git   3:logs                   ~/project  14:30"
        );
    }

    #[test]
    fn normal_mode_hides_the_indicator() {
        let tabs = [tab(1, "code")];
        let content = StatusContent {
            workspace: "work",
            tabs: &tabs,
            active_tab: Some(1),
            clock: "14:30",
            ..Default::default()
        };
        assert!(render(&content, 40).starts_with("work |  1:code "));
    }

    #[test]
    fn left_side_truncates_before_the_right_side() {
        let tabs = [tab(1, "a-rather-long-tab-name"), tab(2, "another")];
        let content = StatusContent {
            workspace: "workspace-name",
            tabs: &tabs,
            active_tab: Some(1),
            pane_title: "title",
            clock: "14:30",
            ..Default::default()
        };
        let row = render(&content, 30);
        // Right side intact, one clear gap column before it.
        assert!(row.ends_with(" title  14:30"));
        let right_start = 30 - "title  14:30".len();
        assert_eq!(&row[right_start - 1..right_start], " ");
    }

    #[test]
    fn tab_cells_carry_active_flags() {
        let tabs = [tab(1, "a"), tab(2, "b")];
        let content = StatusContent {
            workspace: "w",
            tabs: &tabs,
            active_tab: Some(2),
            ..Default::default()
        };
        let cells = layout_row(&content, 40);
        let actives: Vec<bool> = cells
            .iter()
            .filter_map(|(_, c)| match c.kind {
                SegmentKind::Tab { active } => Some(active),
                _ => None,
            })
            .collect();
        assert!(actives.contains(&false) && actives.contains(&true));
    }

    fn tab(id: u32, name: &str) -> crate::tab_bar::TabInfo {
        crate::tab_bar::TabInfo {
            tab_id: id,
            focused_pane: id * 10,
            name: name.to_string(),
        }
    }

    #[test]
    fn bar_narrower_than_the_clock_renders_nothing() {
        let tabs = [tab(1, "code")];
        let content = StatusContent {
            workspace: "work",
            tabs: &tabs,
            active_tab: Some(1),
            pane_title: "title",
            clock: "14:30",
            ..Default::default()
        };
        for cols in 1..=4 {
            let cells = layout_row(&content, cols);
            assert!(
                cells.iter().all(|(_, c)| c.kind != SegmentKind::Clock),
                "cols={cols}: a clock that cannot fit whole is dropped, not clipped"
            );
            assert!(cells.iter().all(|&(col, _)| col < cols));
        }
    }

    #[test]
    fn tab_strip_clips_mid_label_against_the_right_side() {
        let tabs = [tab(1, "alpha"), tab(2, "beta")];
        let content = StatusContent {
            workspace: "w",
            tabs: &tabs,
            active_tab: Some(1),
            pane_title: "t",
            clock: "14:30",
            ..Default::default()
        };
        // 20 cols: clock at 15-19, title "t" at 12, limit 11. The tab
        // strip is clipped mid-label to fit before that limit.
        let row = render(&content, 20);
        assert!(row.starts_with("w |  1:al"), "got: {row:?}");
        assert!(row.ends_with("t  14:30"));
        let cells = layout_row(&content, 20);
        let max_tab_col = cells
            .iter()
            .filter(|(_, c)| matches!(c.kind, SegmentKind::Tab { .. }))
            .map(|&(col, _)| col)
            .max()
            .unwrap();
        // One clear column between the clipped strip and the title.
        assert!(max_tab_col < 20 - u16::try_from("t  14:30".len()).unwrap());
    }

    #[test]
    fn seconds_to_next_minute_stays_in_range() {
        let s = super::seconds_to_next_minute();
        assert!((1..=60).contains(&s), "got {s}");
    }

    #[test]
    fn height_is_one_ceiled_cell_row_when_enabled() {
        let Ok(font) = crate::frame::try_init_font(&oakterm_config::ConfigValues::default(), 14.0)
        else {
            return;
        };
        let mut m = *font.metrics();
        m.cell_height = 16.5;
        assert_eq!(super::status_bar_height(true, Some(&m)), 17);
        assert_eq!(super::status_bar_height(false, Some(&m)), 0);
        assert_eq!(super::status_bar_height(true, None), 0);
    }
}
