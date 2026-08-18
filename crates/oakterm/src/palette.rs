//! Command palette core (Spec-0009): fuzzy matching, prefix filters, and
//! palette state. Pure — no GPU or event-loop types — so behavior is
//! unit-testable; assembly and key routing live in `frame.rs`/`main.rs`.

use oakterm_config::{ActionContext, ActionId, ActionRegistry};

/// Sentinel for "query char can't be matched at this label position".
const UNREACHABLE: i32 = i32::MIN;

/// Result rows visible at once; the window scrolls to keep the selection
/// in view.
pub(crate) const MAX_VISIBLE_RESULTS: usize = 10;

/// What a palette row resolves to when confirmed (Spec-0009). Only `Action`
/// has a provider today; workspaces, layouts, and settings join as their
/// features land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteResultKind {
    Action(ActionId),
    // Constructed once their providers exist; the palette's confirm path
    // and scope parsing are already shaped for them.
    /// Wire-side workspace id (`TabList`); the daemon's `WorkspaceId`
    /// newtype is not a GUI dependency.
    #[allow(dead_code)]
    Workspace(u32),
    #[allow(dead_code)]
    Layout(String),
    #[allow(dead_code)]
    Setting(String),
}

/// A filtered, ranked palette row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteResult {
    pub kind: PaletteResultKind,
    pub label: String,
    /// Keybind hint (actions only).
    pub keybind: Option<String>,
    pub score: i32,
    /// Character positions in the label that matched the query.
    pub match_positions: Vec<usize>,
}

/// Palette state machine. Owns the query, ranked results, and selection;
/// callers pass the registry and an [`ActionContext`] snapshot so refresh
/// stays pure.
///
/// Every call must pass the currently-live registry and context — the
/// state stores neither, so results reflect whatever the caller last
/// supplied. When the registry is replaced (config reload), close the
/// palette rather than continuing to feed it the old one.
#[derive(Debug, Default)]
pub struct PaletteState {
    visible: bool,
    query: String,
    results: Vec<PaletteResult>,
    selected: usize,
    /// First result row shown; moves only when the selection would leave
    /// the visible window, so Up/Down move the cursor, not the list.
    window_start: usize,
    recent: Vec<ActionId>,
}

impl PaletteState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    #[must_use]
    pub fn results(&self) -> &[PaletteResult] {
        &self.results
    }

    /// The raw query text, prefix included, for rendering the input row.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// First result row of the visible window.
    #[must_use]
    pub fn window_start(&self) -> usize {
        self.window_start
    }

    /// Show the palette with a fresh query. Recent-action history survives
    /// across opens.
    pub fn open(&mut self, registry: &ActionRegistry, ctx: ActionContext) {
        self.visible = true;
        self.query.clear();
        self.refresh(registry, ctx);
    }

    /// Hide the palette without executing (Escape).
    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Return the selected result's kind and hide the palette; the caller
    /// executes it. Confirmed actions enter the recent list (deduplicated,
    /// most recent first, capped at five).
    pub fn confirm(&mut self) -> Option<PaletteResultKind> {
        let kind = self.results.get(self.selected)?.kind.clone();
        if let PaletteResultKind::Action(id) = kind {
            self.recent.retain(|&r| r != id);
            self.recent.insert(0, id);
            self.recent.truncate(5);
        }
        self.visible = false;
        Some(kind)
    }

    /// Move the selection up one row, stopping at the top. The window
    /// follows only when the selection crosses its top edge.
    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.window_start = self.window_start.min(self.selected);
    }

    /// Move the selection down one row, stopping at the last result. The
    /// window follows only when the selection crosses its bottom edge.
    pub fn move_down(&mut self) {
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
            if self.selected >= self.window_start + MAX_VISIBLE_RESULTS {
                self.window_start += 1;
            }
        }
    }

    /// Append a typed character to the query and re-filter.
    pub fn input_char(&mut self, c: char, registry: &ActionRegistry, ctx: ActionContext) {
        self.query.push(c);
        self.refresh(registry, ctx);
    }

    /// Delete the last query character and re-filter.
    pub fn backspace(&mut self, registry: &ActionRegistry, ctx: ActionContext) {
        self.query.pop();
        self.refresh(registry, ctx);
    }

    fn refresh(&mut self, registry: &ActionRegistry, ctx: ActionContext) {
        self.selected = 0;
        self.window_start = 0;
        let (scope, q) = parse_query(&self.query);
        self.results = match scope {
            // No providers exist yet for these scopes (workspaces are
            // Phase 1 backlog; layouts land with layout.define(); settings
            // with live config toggle).
            PaletteScope::Workspaces | PaletteScope::Layouts | PaletteScope::Settings => Vec::new(),
            PaletteScope::All | PaletteScope::Actions if q.is_empty() => {
                self.default_view(registry, ctx)
            }
            PaletteScope::All | PaletteScope::Actions => {
                let q = q.to_string();
                let mut matched: Vec<PaletteResult> = registry
                    .performable(ctx)
                    .filter_map(|a| {
                        fuzzy_match(&q, a.label()).map(|m| PaletteResult::from_match(a, m))
                    })
                    .collect();
                // Stable sort: equal (score, length) keeps catalog order.
                matched.sort_by_key(|r| (std::cmp::Reverse(r.score), r.label.len()));
                matched
            }
        };
        debug_assert!(self.selected < self.results.len() || self.results.is_empty());
    }

    /// Spec-0009 Palette Default View: recent actions first (most recent
    /// leading), then all other performable actions grouped by category,
    /// sorted alphabetically within each group.
    fn default_view(&self, registry: &ActionRegistry, ctx: ActionContext) -> Vec<PaletteResult> {
        let mut out: Vec<PaletteResult> = self
            .recent
            .iter()
            .filter_map(|&id| registry.find(id))
            .filter(|a| a.id().is_performable(ctx))
            .map(PaletteResult::unqueried)
            .collect();
        let mut rest: Vec<&oakterm_config::RegisteredAction> = registry
            .performable(ctx)
            .filter(|a| !self.recent.contains(&a.id()))
            .collect();
        rest.sort_by_key(|a| (a.category(), a.label()));
        out.extend(rest.into_iter().map(PaletteResult::unqueried));
        out
    }
}

impl PaletteResult {
    /// A row for a fuzzy-matched action; label, hint, score, and highlight
    /// positions all derive from the same registry entry and match.
    fn from_match(action: &oakterm_config::RegisteredAction, m: FuzzyMatch) -> Self {
        Self {
            kind: PaletteResultKind::Action(action.id()),
            label: action.label().to_string(),
            keybind: action.keybind_hint().map(str::to_string),
            score: m.score,
            match_positions: m.positions,
        }
    }

    /// A row for the default (empty-query) view: no score, no highlighted
    /// positions.
    fn unqueried(action: &oakterm_config::RegisteredAction) -> Self {
        Self {
            kind: PaletteResultKind::Action(action.id()),
            label: action.label().to_string(),
            keybind: action.keybind_hint().map(str::to_string),
            score: 0,
            match_positions: Vec::new(),
        }
    }
}

/// Result-category scope selected by a query prefix (Spec-0009 Prefix
/// Filters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteScope {
    All,
    Actions,
    Workspaces,
    Layouts,
    Settings,
}

/// Split a raw palette query into its scope and the match text (prefix and
/// surrounding whitespace stripped).
#[must_use]
pub fn parse_query(raw: &str) -> (PaletteScope, &str) {
    let (scope, rest) = match raw.chars().next() {
        Some('>') => (PaletteScope::Actions, &raw[1..]),
        Some('@') => (PaletteScope::Workspaces, &raw[1..]),
        Some('#') => (PaletteScope::Layouts, &raw[1..]),
        Some(':') => (PaletteScope::Settings, &raw[1..]),
        _ => (PaletteScope::All, raw),
    };
    (scope, rest.trim())
}

/// A successful fuzzy match of a query against a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Higher is better. Negative scores are possible when gap penalties
    /// outweigh bonuses; callers rank, they don't threshold.
    pub score: i32,
    /// Character-index positions in the label that matched, in order.
    pub positions: Vec<usize>,
}

/// Score `query` against `label` (Spec-0009 Fuzzy Matching): every query
/// character must appear in the label in order; `None` otherwise.
/// Case-insensitive. Bonuses: consecutive match +3, match at word boundary
/// +2, match at start of label +1. Penalty: -1 per skipped label character
/// between matches. Among valid alignments the highest-scoring one wins, so
/// `positions` highlights the characters a user would expect (word starts,
/// runs) rather than the leftmost subsequence.
pub fn fuzzy_match(query: &str, label: &str) -> Option<FuzzyMatch> {
    // Fold each character to the first char of its lowercase expansion.
    // Multi-char expansions (İ → "i" + combining dot) would desync the
    // folded indices from the original label and break `positions`.
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let query: Vec<char> = query.chars().map(fold).collect();
    let label_chars: Vec<char> = label.chars().collect();
    let label_lower: Vec<char> = label_chars.iter().map(|&c| fold(c)).collect();

    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }
    if query.len() > label_lower.len() {
        return None;
    }

    let positional_bonus = |j: usize| -> i32 {
        let mut b = 0;
        if j == 0 || label_chars[j - 1] == ' ' {
            b += 2;
        }
        if j == 0 {
            b += 1;
        }
        b
    };

    // dp[i][j]: best score with query[i] matched at label position j;
    // from[i][j]: the position query[i-1] matched at on that best path.
    let n = label_lower.len();
    let mut dp = vec![vec![UNREACHABLE; n]; query.len()];
    let mut from = vec![vec![0usize; n]; query.len()];

    for j in 0..n {
        if label_lower[j] == query[0] {
            dp[0][j] = positional_bonus(j);
        }
    }
    for i in 1..query.len() {
        for j in i..n {
            if label_lower[j] != query[i] {
                continue;
            }
            let mut best = UNREACHABLE;
            let mut best_k = 0;
            for (k, &prev) in dp[i - 1].iter().enumerate().take(j).skip(i - 1) {
                if prev == UNREACHABLE {
                    continue;
                }
                let consecutive = if j == k + 1 { 3 } else { 0 };
                let gap = i32::try_from(j - k - 1).unwrap_or(i32::MAX);
                let score = prev + positional_bonus(j) + consecutive - gap;
                if score > best {
                    best = score;
                    best_k = k;
                }
            }
            dp[i][j] = best;
            from[i][j] = best_k;
        }
    }

    let last = query.len() - 1;
    let (mut j, &score) = dp[last]
        .iter()
        .enumerate()
        .filter(|&(_, &s)| s != UNREACHABLE)
        .max_by_key(|&(pos, &s)| (s, std::cmp::Reverse(pos)))?;

    let mut positions = vec![0; query.len()];
    for i in (0..query.len()).rev() {
        positions[i] = j;
        j = from[i][j];
    }
    Some(FuzzyMatch { score, positions })
}

#[cfg(test)]
mod tests {
    use super::fuzzy_match;

    #[test]
    fn fuzzy_match_finds_a_prefix_and_rejects_a_non_subsequence() {
        let m = fuzzy_match("split", "Split Pane Right").expect("prefix matches");
        assert_eq!(m.positions, vec![0, 1, 2, 3, 4]);

        // 'x' never appears: not a subsequence.
        assert_eq!(fuzzy_match("splix", "Split Pane Right"), None);
        // In-order requirement: characters present but reversed.
        assert_eq!(fuzzy_match("ps", "Split",), None);
    }

    #[test]
    fn fuzzy_match_scores_per_spec_rules() {
        // Worked by hand from Spec-0009: consecutive +3, word boundary +2,
        // start of label +1 (stacks with boundary), -1 per skipped char
        // between matches.

        // s@0 = 2+1; p,l,i,t consecutive = 4*3.
        assert_eq!(fuzzy_match("split", "Split Pane Right").unwrap().score, 15);

        // P@6 boundary = 2; a,n,e consecutive = 3*3. The matcher must prefer
        // this over greedy-leftmost p@1 (score 1) — alignment is optimal.
        let m = fuzzy_match("pane", "Split Pane Right").unwrap();
        assert_eq!(m.score, 11);
        assert_eq!(m.positions, vec![6, 7, 8, 9]);

        // s@0 = 3; R@11 boundary = 2, minus the 10 skipped chars between.
        assert_eq!(fuzzy_match("sr", "Split Pane Right").unwrap().score, -5);
    }

    #[test]
    fn prefixes_scope_the_query_and_are_stripped() {
        use super::{PaletteScope, parse_query};
        assert_eq!(parse_query("> split"), (PaletteScope::Actions, "split"));
        assert_eq!(parse_query("@ work"), (PaletteScope::Workspaces, "work"));
        assert_eq!(parse_query("# dev"), (PaletteScope::Layouts, "dev"));
        assert_eq!(parse_query(": font"), (PaletteScope::Settings, "font"));
        // No prefix searches everything; the query is untouched.
        assert_eq!(parse_query("split"), (PaletteScope::All, "split"));
        // Prefix with no space still scopes.
        assert_eq!(parse_query(">split"), (PaletteScope::Actions, "split"));
        // Empty and prefix-only queries: scope opens with an empty match.
        assert_eq!(parse_query(""), (PaletteScope::All, ""));
        assert_eq!(parse_query(">"), (PaletteScope::Actions, ""));
        assert_eq!(parse_query("@"), (PaletteScope::Workspaces, ""));
    }

    #[test]
    fn fuzzy_match_survives_multi_char_lowercase_expansions() {
        // İ (U+0130) lowercases to two chars; folding must stay aligned
        // with the original label instead of panicking on index drift.
        let m = fuzzy_match("İ", "İİ Test").expect("folded match");
        assert_eq!(m.positions, vec![0]);
        assert!(fuzzy_match("i", "İstanbul").is_some());
    }

    use super::{PaletteResultKind, PaletteState};
    use oakterm_config::{ActionContext, ActionRegistry, KeybindRegistry};

    fn registry() -> ActionRegistry {
        ActionRegistry::core(&KeybindRegistry::new())
    }

    /// Single pane, single tab, no focus neighbors: only the five
    /// unconditional actions are performable.
    fn restrictive_ctx() -> ActionContext {
        ActionContext {
            pane_count: 1,
            tab_count: 1,
            ..Default::default()
        }
    }

    fn labels(palette: &PaletteState) -> Vec<&str> {
        palette.results().iter().map(|r| r.label.as_str()).collect()
    }

    #[test]
    fn open_with_empty_query_groups_performable_actions_by_category() {
        let mut p = PaletteState::new();
        p.open(&registry(), restrictive_ctx());
        assert!(p.is_visible());
        // Category order is the catalog's (Pane, Tab, View, Config here);
        // labels sort alphabetically within each group. Non-performable
        // actions (close/focus/tab-cycling) are excluded.
        assert_eq!(
            labels(&p),
            vec![
                "Split Pane Down",
                "Split Pane Right",
                "New Tab",
                "Show Command Palette",
                "Toggle Fullscreen",
                "Reload Config",
            ],
        );
        assert_eq!(p.selected_index(), 0);
    }

    /// Everything open: all 14 catalog actions performable.
    fn open_ctx() -> ActionContext {
        ActionContext {
            pane_count: 2,
            tab_count: 2,
            can_focus_left: true,
            can_focus_right: true,
            can_focus_up: true,
            can_focus_down: true,
            copy_mode_supported: true,
        }
    }

    #[test]
    fn typing_filters_and_ranks_by_score_then_label_length() {
        let reg = registry();
        let mut p = PaletteState::new();
        p.open(&reg, open_ctx());
        for c in "tab".chars() {
            p.input_char(c, &reg, open_ctx());
        }
        // All four tab actions score 8 (boundary T + consecutive a, b);
        // the tie breaks on label length, shortest first.
        assert_eq!(
            labels(&p),
            vec!["New Tab", "Next Tab", "Close Tab", "Previous Tab"],
        );
        assert!(p.results().iter().all(|r| !r.match_positions.is_empty()));
        assert_eq!(p.selected_index(), 0);

        // Backspacing to an empty query restores the default view.
        for _ in 0.."tab".len() {
            p.backspace(&reg, open_ctx());
        }
        assert_eq!(labels(&p).len(), 15);
    }

    #[test]
    fn selection_clamps_at_the_edges_and_resets_on_input() {
        let reg = registry();
        let mut p = PaletteState::new();
        p.open(&reg, restrictive_ctx()); // 6 results
        p.move_up(); // already at the top
        assert_eq!(p.selected_index(), 0);
        for _ in 0..10 {
            p.move_down(); // clamps at the last result
        }
        assert_eq!(p.selected_index(), 5);
        p.move_up();
        assert_eq!(p.selected_index(), 4);
        // Any edit resets the selection to the top.
        p.input_char('n', &reg, restrictive_ctx());
        assert_eq!(p.selected_index(), 0);
    }

    /// Type `label` into an open palette and confirm the top result.
    fn run_action(p: &mut PaletteState, reg: &ActionRegistry, label: &str) -> PaletteResultKind {
        p.open(reg, open_ctx());
        for c in label.chars() {
            p.input_char(c, reg, open_ctx());
        }
        assert_eq!(labels(p)[0], label, "typed label should rank first");
        p.confirm().expect("a result is selected")
    }

    #[test]
    fn confirm_returns_the_selection_closes_and_records_recents() {
        use oakterm_config::ActionId;

        let reg = registry();
        let mut p = PaletteState::new();
        let kind = run_action(&mut p, &reg, "New Tab");
        assert_eq!(kind, PaletteResultKind::Action(ActionId::NewTab));
        assert!(!p.is_visible());

        // The recent action leads the default view on reopen, undoubled.
        p.open(&reg, open_ctx());
        assert_eq!(labels(&p)[0], "New Tab");
        assert_eq!(labels(&p).len(), 15);
    }

    #[test]
    fn recents_dedupe_to_front_and_cap_at_five() {
        let reg = registry();
        let mut p = PaletteState::new();
        for label in [
            "Split Pane Right",
            "Split Pane Down",
            "Close Pane",
            "New Tab",
            "Close Tab",
            "Next Tab",
        ] {
            run_action(&mut p, &reg, label);
        }
        // Re-running an already-recent action moves it to the front.
        run_action(&mut p, &reg, "New Tab");

        p.open(&reg, open_ctx());
        assert_eq!(
            &labels(&p)[..5],
            &[
                "New Tab",
                "Next Tab",
                "Close Tab",
                "Close Pane",
                "Split Pane Down",
            ],
        );
        assert_eq!(labels(&p).len(), 15);
    }

    #[test]
    fn prefix_scopes_apply_and_unprovided_scopes_return_nothing() {
        let reg = registry();
        let mut p = PaletteState::new();
        p.open(&reg, open_ctx());
        for c in ">tab".chars() {
            p.input_char(c, &reg, open_ctx());
        }
        assert_eq!(
            labels(&p),
            vec!["New Tab", "Next Tab", "Close Tab", "Previous Tab"],
        );

        // Workspaces/layouts/settings have no providers yet: empty results
        // (the GUI renders "No matching actions").
        for (prefix, query) in [('@', "work"), ('#', "dev"), (':', "font")] {
            p.open(&reg, open_ctx());
            p.input_char(prefix, &reg, open_ctx());
            for c in query.chars() {
                p.input_char(c, &reg, open_ctx());
            }
            assert!(labels(&p).is_empty(), "{prefix} scope should be empty");
        }
    }

    #[test]
    fn filtering_excludes_non_performable_actions() {
        let reg = registry();
        let mut p = PaletteState::new();
        p.open(&reg, restrictive_ctx());
        // Close Pane and Close Tab both match "close" but neither is
        // performable with one pane in one tab.
        for c in "close".chars() {
            p.input_char(c, &reg, restrictive_ctx());
        }
        assert!(labels(&p).is_empty());
    }

    #[test]
    fn window_follows_selection_only_at_its_edges() {
        let reg = registry();
        let mut p = PaletteState::new();
        p.open(&reg, open_ctx()); // 15 results, 10-row window
        assert_eq!(p.window_start(), 0);

        // Down to the window's bottom edge: no scroll yet.
        for _ in 0..9 {
            p.move_down();
        }
        assert_eq!((p.selected_index(), p.window_start()), (9, 0));
        // Crossing the edge scrolls one row per step.
        p.move_down();
        assert_eq!((p.selected_index(), p.window_start()), (10, 1));
        p.move_down();
        p.move_down();
        p.move_down();
        p.move_down();
        assert_eq!((p.selected_index(), p.window_start()), (14, 5));
        // Clamped at the last result; the window stays put.
        p.move_down();
        assert_eq!((p.selected_index(), p.window_start()), (14, 5));

        // Moving up moves the cursor within the window first...
        for _ in 0..9 {
            p.move_up();
        }
        assert_eq!((p.selected_index(), p.window_start()), (5, 5));
        // ...and only scrolls once the selection crosses the top edge.
        p.move_up();
        assert_eq!((p.selected_index(), p.window_start()), (4, 4));

        // Any edit resets the window with the selection.
        p.input_char('t', &reg, open_ctx());
        assert_eq!(p.window_start(), 0);
    }

    #[test]
    fn zero_results_are_inert_for_navigation_and_confirm() {
        let reg = registry();
        let mut p = PaletteState::new();
        p.open(&reg, restrictive_ctx());
        for c in "close".chars() {
            p.input_char(c, &reg, restrictive_ctx());
        }
        assert!(p.results().is_empty());
        p.move_down();
        p.move_up();
        assert_eq!(p.selected_index(), 0);
        assert_eq!(p.confirm(), None);
    }

    #[test]
    fn query_is_exposed_for_rendering() {
        let reg = registry();
        let mut p = PaletteState::new();
        p.open(&reg, open_ctx());
        assert_eq!(p.query(), "");
        p.input_char('>', &reg, open_ctx());
        p.input_char('t', &reg, open_ctx());
        assert_eq!(p.query(), ">t");
    }
}
