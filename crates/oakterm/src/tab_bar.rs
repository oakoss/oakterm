//! Client-side tab bar: daemon tab state mirrored from `TabList`
//! (Spec-0001 0xB0) and the pure strip layout the renderer and mouse
//! hit-testing share.

use oakterm_protocol::message::TabList;

/// Longest tab name rendered before truncation, in characters.
const MAX_NAME_CHARS: usize = 20;

/// One tab as the client knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabInfo {
    pub tab_id: u32,
    /// The pane focus moves to when this tab activates.
    pub focused_pane: u32,
    pub name: String,
}

/// Tab state mirrored from the daemon. Refreshed with `ListTabs` after
/// every tab operation this client performs; the daemon pushes no tab
/// topology changes (Spec-0001).
#[derive(Debug, Default)]
pub struct TabsState {
    tabs: Vec<TabInfo>,
    active_tab: Option<u32>,
}

impl TabsState {
    /// Adopt a `TabList`. Returns the previous active tab id.
    pub fn apply(&mut self, list: TabList) -> Option<u32> {
        let previous = self.active_tab;
        self.tabs = list
            .tabs
            .into_iter()
            .map(|t| TabInfo {
                tab_id: t.tab_id,
                focused_pane: t.focused_pane,
                name: t.name,
            })
            .collect();
        self.active_tab = if self.tabs.iter().any(|t| t.tab_id == list.active_tab) {
            Some(list.active_tab)
        } else {
            // Only wire corruption or version skew can produce this; the
            // bar then renders with no active highlight.
            if !self.tabs.is_empty() {
                tracing::warn!(
                    active_tab = list.active_tab,
                    tabs = ?self.tabs.iter().map(|t| t.tab_id).collect::<Vec<_>>(),
                    "TabList active tab not in the tab list"
                );
            }
            None
        };
        previous
    }

    #[must_use]
    pub fn bar_visible(&self) -> bool {
        self.tabs.len() > 1
    }

    #[must_use]
    pub fn active_tab(&self) -> Option<u32> {
        self.active_tab
    }

    #[must_use]
    pub fn tabs(&self) -> &[TabInfo] {
        &self.tabs
    }

    #[must_use]
    pub fn focused_pane_of(&self, tab_id: u32) -> Option<u32> {
        self.tabs
            .iter()
            .find(|t| t.tab_id == tab_id)
            .map(|t| t.focused_pane)
    }
}

/// One cell of the rendered tab strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StripCell {
    pub ch: char,
    /// Cell belongs to the active tab (highlight colors).
    pub active: bool,
}

/// A tab's clickable extent in strip cells: `[start, end)` columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabSpan {
    pub tab_id: u32,
    pub start: u16,
    pub end: u16,
}

/// Lay out the tab strip: ` 1:name ` labels separated by one gap cell,
/// truncated to `cols`. Deterministic from the inputs, so rendering and
/// hit-testing call it independently without shared caches.
#[must_use]
pub fn layout_strip(tabs: &[TabInfo], cols: u16) -> Vec<TabSpan> {
    let mut spans = Vec::with_capacity(tabs.len());
    let mut col: u16 = 0;
    for (i, tab) in tabs.iter().enumerate() {
        if i > 0 {
            col = col.saturating_add(1);
        }
        let width = u16::try_from(label(i, &tab.name).chars().count()).unwrap_or(u16::MAX);
        let end = col.saturating_add(width).min(cols);
        if col >= end {
            break;
        }
        spans.push(TabSpan {
            tab_id: tab.tab_id,
            start: col,
            end,
        });
        col = end;
    }
    spans
}

/// The cells of the strip row for `spans` produced by [`layout_strip`]
/// over the same inputs. Gap cells between tabs are absent from the
/// result; callers default those cells.
#[must_use]
pub fn strip_cells(
    tabs: &[TabInfo],
    active_tab: Option<u32>,
    spans: &[TabSpan],
) -> Vec<(u16, StripCell)> {
    let mut cells = Vec::new();
    for (i, span) in spans.iter().enumerate() {
        // Resolve by id, not position: a mismatched (tabs, spans) pair
        // skips instead of mislabeling columns.
        let Some(tab) = tabs.iter().find(|t| t.tab_id == span.tab_id) else {
            continue;
        };
        let active = active_tab == Some(span.tab_id);
        for (offset, ch) in label(i, &tab.name).chars().enumerate() {
            let offset = u16::try_from(offset).unwrap_or(u16::MAX);
            let col = span.start.saturating_add(offset);
            if col >= span.end {
                break;
            }
            cells.push((col, StripCell { ch, active }));
        }
    }
    cells
}

/// The tab under strip column `col`, if any. Gap cells hit nothing.
#[must_use]
pub fn hit_test(spans: &[TabSpan], col: u16) -> Option<u32> {
    spans
        .iter()
        .find(|s| col >= s.start && col < s.end)
        .map(|s| s.tab_id)
}

/// Display label for the tab at 0-based `index`: ` 1:name ` (1-based
/// index), or ` 1 ` when unnamed. Names truncate with an ellipsis.
fn label(index: usize, name: &str) -> String {
    let n = index + 1;
    if name.is_empty() {
        return format!(" {n} ");
    }
    let mut shown: String = name.chars().take(MAX_NAME_CHARS).collect();
    if name.chars().count() > MAX_NAME_CHARS {
        shown.push('…');
    }
    format!(" {n}:{shown} ")
}

#[cfg(test)]
mod tests {
    use super::{StripCell, TabInfo, TabsState, hit_test, label, layout_strip, strip_cells};
    use oakterm_protocol::message::{TabEntry, TabList};

    fn tab(id: u32, name: &str) -> TabInfo {
        TabInfo {
            tab_id: id,
            focused_pane: id * 10,
            name: name.to_string(),
        }
    }

    fn list(active: u32, tabs: &[(u32, &str)]) -> TabList {
        TabList {
            workspace_id: 0,
            workspace_name: "default".to_string(),
            active_tab: active,
            tabs: tabs
                .iter()
                .map(|&(id, name)| TabEntry {
                    tab_id: id,
                    focused_pane: id * 10,
                    name: name.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn apply_tracks_tabs_and_active() {
        let mut state = TabsState::default();
        assert!(!state.bar_visible());
        let previous = state.apply(list(7, &[(0, "vim"), (7, "")]));
        assert_eq!(previous, None);
        assert!(state.bar_visible());
        assert_eq!(state.active_tab(), Some(7));
        assert_eq!(state.focused_pane_of(7), Some(70));
        assert_eq!(state.apply(list(0, &[(0, "vim")])), Some(7));
        assert!(!state.bar_visible());
        assert_eq!(state.active_tab(), Some(0));
    }

    #[test]
    fn apply_rejects_unknown_active_tab() {
        let mut state = TabsState::default();
        state.apply(list(99, &[(0, ""), (1, "")]));
        assert_eq!(state.active_tab(), None);
        assert!(state.bar_visible());
    }

    #[test]
    fn labels_index_from_one_and_truncate() {
        assert_eq!(label(0, ""), " 1 ");
        assert_eq!(label(1, "vim"), " 2:vim ");
        let long = "x".repeat(30);
        let l = label(2, &long);
        assert!(l.starts_with(" 3:"));
        assert!(l.contains('…'));
        assert_eq!(l.chars().count(), 3 + 20 + 2);
    }

    #[test]
    fn strip_spans_are_adjacent_with_gaps() {
        let tabs = [tab(0, "vim"), tab(7, "")];
        let spans = layout_strip(&tabs, 80);
        // " 1:vim " = 7 cells, gap, " 2 " = 3 cells.
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start, spans[0].end), (0, 7));
        assert_eq!((spans[1].start, spans[1].end), (8, 11));
    }

    #[test]
    fn strip_truncates_at_width() {
        let tabs = [tab(0, "vim"), tab(7, "long-name")];
        let spans = layout_strip(&tabs, 10);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].end, 10, "clamped to the strip width");
        let narrow = layout_strip(&tabs, 7);
        assert_eq!(narrow.len(), 1, "no room for a second span");
    }

    #[test]
    fn strip_cells_mark_active_tab() {
        let tabs = [tab(0, "a"), tab(7, "b")];
        let spans = layout_strip(&tabs, 80);
        let cells = strip_cells(&tabs, Some(7), &spans);
        let first = cells.iter().find(|(col, _)| *col == spans[0].start);
        let second = cells.iter().find(|(col, _)| *col == spans[1].start);
        assert_eq!(first.map(|(_, c)| c.active), Some(false));
        assert_eq!(second.map(|(_, c)| c.active), Some(true));
        assert_eq!(
            cells.iter().find(|(col, _)| *col == spans[1].start + 1),
            Some(&(
                spans[1].start + 1,
                StripCell {
                    ch: '2',
                    active: true
                }
            ))
        );
    }

    #[test]
    fn hit_test_resolves_spans_and_gaps() {
        let tabs = [tab(0, "vim"), tab(7, "")];
        let spans = layout_strip(&tabs, 80);
        assert_eq!(hit_test(&spans, 0), Some(0));
        assert_eq!(hit_test(&spans, 6), Some(0));
        assert_eq!(hit_test(&spans, 7), None, "gap cell");
        assert_eq!(hit_test(&spans, 8), Some(7));
        assert_eq!(hit_test(&spans, 11), None, "past the last tab");
    }
}
