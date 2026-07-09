//! Action registry (Spec-0009): the central catalog of executable actions the
//! command palette searches and keybinds resolve hints against.
//!
//! The catalog is pure data. Performability is a function of an
//! [`ActionContext`] snapshot rather than live GUI state, so the whole module
//! is unit-testable without an event loop. Execution belongs to the GUI, which
//! maps an [`ActionId`] to its dispatch descriptor; nothing here touches panes
//! or the daemon.

use crate::keybind::{Action, KeybindRegistry};

/// Stable identity of a core action.
///
/// The palette and Lua config address actions by their `snake_case` string
/// ([`ActionId::as_str`]); the enum gives typed, exhaustive handling internally
/// (Spec-0009 stores a `String` id at the boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    SplitPaneRight,
    SplitPaneDown,
    ClosePane,
    FocusPaneLeft,
    FocusPaneRight,
    FocusPaneUp,
    FocusPaneDown,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    ToggleFullscreen,
    ShowCommandPalette,
    ReloadConfig,
}

impl ActionId {
    /// Every core action with a working handler today, in catalog order.
    ///
    /// Copy-mode, resize-mode, floating-pane, and workspace actions register
    /// here as their features land (Spec-0009 lists the full target set);
    /// registering only wired actions keeps the catalog free of entries that
    /// would execute as no-ops.
    pub const ALL: [ActionId; 14] = [
        ActionId::SplitPaneRight,
        ActionId::SplitPaneDown,
        ActionId::ClosePane,
        ActionId::FocusPaneLeft,
        ActionId::FocusPaneRight,
        ActionId::FocusPaneUp,
        ActionId::FocusPaneDown,
        ActionId::NewTab,
        ActionId::CloseTab,
        ActionId::NextTab,
        ActionId::PreviousTab,
        ActionId::ToggleFullscreen,
        ActionId::ShowCommandPalette,
        ActionId::ReloadConfig,
    ];

    /// The boundary identifier used by Lua config and the palette.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ActionId::SplitPaneRight => "split_pane_right",
            ActionId::SplitPaneDown => "split_pane_down",
            ActionId::ClosePane => "close_pane",
            ActionId::FocusPaneLeft => "focus_pane_left",
            ActionId::FocusPaneRight => "focus_pane_right",
            ActionId::FocusPaneUp => "focus_pane_up",
            ActionId::FocusPaneDown => "focus_pane_down",
            ActionId::NewTab => "new_tab",
            ActionId::CloseTab => "close_tab",
            ActionId::NextTab => "next_tab",
            ActionId::PreviousTab => "previous_tab",
            ActionId::ToggleFullscreen => "toggle_fullscreen",
            ActionId::ShowCommandPalette => "show_command_palette",
            ActionId::ReloadConfig => "reload_config",
        }
    }

    /// Human-readable label shown in the palette.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ActionId::SplitPaneRight => "Split Pane Right",
            ActionId::SplitPaneDown => "Split Pane Down",
            ActionId::ClosePane => "Close Pane",
            ActionId::FocusPaneLeft => "Focus Pane Left",
            ActionId::FocusPaneRight => "Focus Pane Right",
            ActionId::FocusPaneUp => "Focus Pane Up",
            ActionId::FocusPaneDown => "Focus Pane Down",
            ActionId::NewTab => "New Tab",
            ActionId::CloseTab => "Close Tab",
            ActionId::NextTab => "Next Tab",
            ActionId::PreviousTab => "Previous Tab",
            ActionId::ToggleFullscreen => "Toggle Fullscreen",
            ActionId::ShowCommandPalette => "Show Command Palette",
            ActionId::ReloadConfig => "Reload Config",
        }
    }

    /// Category for grouping in the palette.
    #[must_use]
    pub fn category(self) -> ActionCategory {
        match self {
            ActionId::SplitPaneRight | ActionId::SplitPaneDown | ActionId::ClosePane => {
                ActionCategory::Pane
            }
            ActionId::FocusPaneLeft
            | ActionId::FocusPaneRight
            | ActionId::FocusPaneUp
            | ActionId::FocusPaneDown => ActionCategory::Navigation,
            ActionId::NewTab | ActionId::CloseTab | ActionId::NextTab | ActionId::PreviousTab => {
                ActionCategory::Tab
            }
            ActionId::ToggleFullscreen | ActionId::ShowCommandPalette => ActionCategory::View,
            ActionId::ReloadConfig => ActionCategory::Config,
        }
    }

    /// Whether this action can execute in `ctx`. The palette excludes
    /// non-performable actions from its results (Spec-0009).
    #[must_use]
    pub fn is_performable(self, ctx: ActionContext) -> bool {
        match self {
            ActionId::SplitPaneRight
            | ActionId::SplitPaneDown
            | ActionId::NewTab
            | ActionId::ToggleFullscreen
            | ActionId::ShowCommandPalette
            | ActionId::ReloadConfig => true,
            // Closing the last pane closes its tab (Spec-0007); the daemon only
            // refuses the truly-last pane — the last pane of the last tab.
            ActionId::ClosePane => ctx.pane_count > 1 || ctx.tab_count > 1,
            // The daemon refuses to close the last tab.
            ActionId::CloseTab | ActionId::NextTab | ActionId::PreviousTab => ctx.tab_count > 1,
            ActionId::FocusPaneLeft => ctx.can_focus_left,
            ActionId::FocusPaneRight => ctx.can_focus_right,
            ActionId::FocusPaneUp => ctx.can_focus_up,
            ActionId::FocusPaneDown => ctx.can_focus_down,
        }
    }
}

/// Palette grouping category (Spec-0009). The full set is fixed even though
/// `Workspace` and `Clipboard` have no wired core actions yet. `Ord`
/// follows declaration order, which is the palette's group display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActionCategory {
    Pane,
    Tab,
    Workspace,
    Navigation,
    Clipboard,
    View,
    Config,
}

/// Snapshot of the GUI state that action performability depends on.
///
/// The GUI builds this from live state at query time; keeping performability a
/// function of a plain snapshot rather than the `App` makes it testable without
/// an event loop.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)] // Direction availabilities are naturally booleans.
pub struct ActionContext {
    /// Panes in the focused tab. `close_pane` needs more than one.
    pub pane_count: usize,
    /// Tabs in the active workspace. Tab cycling and `close_tab` need more than
    /// one.
    pub tab_count: usize,
    pub can_focus_left: bool,
    pub can_focus_right: bool,
    pub can_focus_up: bool,
    pub can_focus_down: bool,
}

/// A catalog entry: an action id plus the keybind hint resolved from the active
/// bindings. Display metadata (label, category) is derived from the id via
/// delegating methods, and only [`ActionRegistry`] constructs entries, so a
/// stored entry cannot diverge from [`ActionId`] or carry a hint that was never
/// resolved from the bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredAction {
    id: ActionId,
    keybind_hint: Option<String>,
}

impl RegisteredAction {
    /// Stable identity of this action.
    #[must_use]
    pub fn id(&self) -> ActionId {
        self.id
    }

    /// Display hint (e.g. `"Cmd+P"`), or `None` when the action is unbound.
    #[must_use]
    pub fn keybind_hint(&self) -> Option<&str> {
        self.keybind_hint.as_deref()
    }

    /// Human-readable label shown in the palette.
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.id.label()
    }

    /// Category for grouping in the palette.
    #[must_use]
    pub fn category(&self) -> ActionCategory {
        self.id.category()
    }
}

/// The central action catalog (Spec-0009). The palette searches it; keybinds
/// resolve hints from it; Phase-2 plugins will add to it.
#[derive(Debug, Clone)]
pub struct ActionRegistry {
    actions: Vec<RegisteredAction>,
}

impl ActionRegistry {
    /// Build the catalog of core actions, resolving each action's keybind hint
    /// from `keybinds`.
    #[must_use]
    pub fn core(keybinds: &KeybindRegistry) -> Self {
        let actions = ActionId::ALL
            .iter()
            .map(|&id| RegisteredAction {
                id,
                keybind_hint: resolve_hint(id, keybinds),
            })
            .collect();
        Self { actions }
    }

    /// All registered actions in catalog order.
    #[must_use]
    pub fn actions(&self) -> &[RegisteredAction] {
        &self.actions
    }

    #[must_use]
    pub fn find(&self, id: ActionId) -> Option<&RegisteredAction> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// Actions performable in `ctx`, in catalog order (Spec-0009 excludes
    /// non-performable actions from palette results).
    pub fn performable(&self, ctx: ActionContext) -> impl Iterator<Item = &RegisteredAction> {
        self.actions
            .iter()
            .filter(move |a| a.id.is_performable(ctx))
    }
}

/// Resolve the display hint for `id`: the last-registered chord that still
/// effectively triggers `id` and has a displayable label. Working from
/// [`KeybindRegistry::effective_bindings`] means a chord later rebound to a
/// different action is never shown as `id`'s hint.
fn resolve_hint(id: ActionId, keybinds: &KeybindRegistry) -> Option<String> {
    keybinds
        .effective_bindings()
        .rev()
        .filter(|(_, action)| action_id_of(action) == Some(id))
        .find_map(|(chord, _)| chord.display_hint())
}

/// Map a configured keybind action back to its catalog id, if it names a core
/// action. Returns `None` for actions absent from the palette catalog (scroll,
/// copy, paste, callbacks, switch-to-tab-N, and not-yet-wired features).
///
/// `SplitPane` directions collapse: left/right → `SplitPaneRight` and up/down →
/// `SplitPaneDown`, mirroring the wire's split-axis dispatch. Focus directions
/// do *not* collapse — each maps to its own `FocusPane*` id.
fn action_id_of(action: &Action) -> Option<ActionId> {
    match action {
        Action::SplitPane { direction, .. } => match direction.as_str() {
            "left" | "right" => Some(ActionId::SplitPaneRight),
            "up" | "down" => Some(ActionId::SplitPaneDown),
            _ => None,
        },
        Action::ClosePane => Some(ActionId::ClosePane),
        Action::FocusPaneDirection(dir) => match dir.as_str() {
            "left" => Some(ActionId::FocusPaneLeft),
            "right" => Some(ActionId::FocusPaneRight),
            "up" => Some(ActionId::FocusPaneUp),
            "down" => Some(ActionId::FocusPaneDown),
            _ => None,
        },
        Action::NewTab => Some(ActionId::NewTab),
        Action::CloseTab => Some(ActionId::CloseTab),
        Action::NextTab => Some(ActionId::NextTab),
        Action::PreviousTab => Some(ActionId::PreviousTab),
        Action::ToggleFullscreen => Some(ActionId::ToggleFullscreen),
        Action::ShowCommandPalette => Some(ActionId::ShowCommandPalette),
        Action::ReloadConfig => Some(ActionId::ReloadConfig),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActionCategory, ActionContext, ActionId, ActionRegistry, RegisteredAction, action_id_of,
    };
    use crate::keybind::{Action, KeyChord, KeyName, KeybindRegistry, PhysicalKeyId};

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).expect("test chord parses")
    }

    #[test]
    fn ids_have_distinct_boundary_strings_and_labels() {
        let mut strs: Vec<&str> = ActionId::ALL.iter().map(|id| id.as_str()).collect();
        let n = strs.len();
        strs.sort_unstable();
        strs.dedup();
        assert_eq!(strs.len(), n, "as_str values must be unique");

        let mut labels: Vec<&str> = ActionId::ALL.iter().map(|id| id.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), n, "labels must be unique");
    }

    #[test]
    fn categories_group_as_specified() {
        use ActionCategory::{Config, Navigation, Pane, Tab, View};
        let expected = [
            (ActionId::SplitPaneRight, Pane),
            (ActionId::SplitPaneDown, Pane),
            (ActionId::ClosePane, Pane),
            (ActionId::FocusPaneLeft, Navigation),
            (ActionId::FocusPaneRight, Navigation),
            (ActionId::FocusPaneUp, Navigation),
            (ActionId::FocusPaneDown, Navigation),
            (ActionId::NewTab, Tab),
            (ActionId::CloseTab, Tab),
            (ActionId::NextTab, Tab),
            (ActionId::PreviousTab, Tab),
            (ActionId::ToggleFullscreen, View),
            (ActionId::ShowCommandPalette, View),
            (ActionId::ReloadConfig, Config),
        ];
        // Covering every ALL member keeps a mis-assigned category from hiding
        // behind a sampled subset.
        assert_eq!(expected.map(|(id, _)| id), ActionId::ALL);
        for (id, category) in expected {
            assert_eq!(id.category(), category, "{}", id.as_str());
        }
    }

    #[test]
    fn unconditional_actions_are_always_performable() {
        let ctx = ActionContext::default();
        for id in [
            ActionId::SplitPaneRight,
            ActionId::SplitPaneDown,
            ActionId::NewTab,
            ActionId::ToggleFullscreen,
            ActionId::ReloadConfig,
        ] {
            assert!(
                id.is_performable(ctx),
                "{} should always perform",
                id.as_str()
            );
        }
    }

    #[test]
    fn close_pane_only_refused_on_the_last_pane_of_the_last_tab() {
        // Truly-last pane: daemon refuses, so not performable.
        let last = ActionContext {
            pane_count: 1,
            tab_count: 1,
            ..Default::default()
        };
        // More panes in this tab: performable.
        let multi_pane = ActionContext {
            pane_count: 2,
            tab_count: 1,
            ..Default::default()
        };
        // One pane here but other tabs exist: closing it closes the tab
        // (Spec-0007), still performable.
        let multi_tab = ActionContext {
            pane_count: 1,
            tab_count: 2,
            ..Default::default()
        };
        assert!(!ActionId::ClosePane.is_performable(last));
        assert!(ActionId::ClosePane.is_performable(multi_pane));
        assert!(ActionId::ClosePane.is_performable(multi_tab));
    }

    #[test]
    fn tab_cycling_and_close_need_more_than_one_tab() {
        let one = ActionContext {
            tab_count: 1,
            ..Default::default()
        };
        let two = ActionContext {
            tab_count: 2,
            ..Default::default()
        };
        for id in [ActionId::CloseTab, ActionId::NextTab, ActionId::PreviousTab] {
            assert!(!id.is_performable(one), "{} on one tab", id.as_str());
            assert!(id.is_performable(two), "{} on two tabs", id.as_str());
        }
    }

    #[test]
    fn focus_actions_gate_on_their_own_direction() {
        // One-hot contexts pin each gate to its own field, so a Left<->Down or
        // Right<->Up wiring swap can't hide behind a diagonal context.
        let left = ActionContext {
            can_focus_left: true,
            ..Default::default()
        };
        let right = ActionContext {
            can_focus_right: true,
            ..Default::default()
        };
        let up = ActionContext {
            can_focus_up: true,
            ..Default::default()
        };
        let down = ActionContext {
            can_focus_down: true,
            ..Default::default()
        };
        let cases = [
            (ActionId::FocusPaneLeft, left),
            (ActionId::FocusPaneRight, right),
            (ActionId::FocusPaneUp, up),
            (ActionId::FocusPaneDown, down),
        ];
        for (id, own) in cases {
            assert!(
                id.is_performable(own),
                "{} on its own direction",
                id.as_str()
            );
            // Not performable under any of the other three one-hot contexts.
            for (_, other) in cases.iter().filter(|(other_id, _)| *other_id != id) {
                assert!(
                    !id.is_performable(*other),
                    "{} must not fire on another direction",
                    id.as_str()
                );
            }
        }
    }

    fn split(direction: &str) -> Action {
        Action::SplitPane {
            direction: direction.into(),
            size: 0.5,
        }
    }

    #[test]
    fn action_id_of_collapses_split_directions_but_not_focus() {
        // Split collapses to the axis representative.
        assert_eq!(
            action_id_of(&split("right")),
            Some(ActionId::SplitPaneRight)
        );
        assert_eq!(action_id_of(&split("left")), Some(ActionId::SplitPaneRight));
        assert_eq!(action_id_of(&split("up")), Some(ActionId::SplitPaneDown));
        assert_eq!(action_id_of(&split("down")), Some(ActionId::SplitPaneDown));
        // Focus does not collapse — each direction is distinct.
        for (dir, id) in [
            ("left", ActionId::FocusPaneLeft),
            ("right", ActionId::FocusPaneRight),
            ("up", ActionId::FocusPaneUp),
            ("down", ActionId::FocusPaneDown),
        ] {
            assert_eq!(
                action_id_of(&Action::FocusPaneDirection(dir.into())),
                Some(id)
            );
        }
    }

    #[test]
    fn action_id_of_rejects_unknown_directions_and_non_catalog_actions() {
        // Unknown direction strings must not collapse to a bogus id.
        assert_eq!(action_id_of(&split("diagonal")), None);
        assert_eq!(
            action_id_of(&Action::FocusPaneDirection("inward".into())),
            None
        );
        // Non-catalog actions, including the plausible-future ones a later dev
        // might be tempted to map.
        assert_eq!(action_id_of(&Action::Copy), None);
        assert_eq!(action_id_of(&Action::ScrollUp(0)), None);
        assert_eq!(
            action_id_of(&Action::SwitchTab(std::num::NonZeroU32::new(1).unwrap())),
            None
        );
    }

    #[test]
    fn core_registry_covers_every_id_in_order() {
        let reg = ActionRegistry::core(&KeybindRegistry::new());
        let ids: Vec<ActionId> = reg.actions().iter().map(RegisteredAction::id).collect();
        assert_eq!(ids, ActionId::ALL.to_vec());
    }

    #[test]
    fn core_resolves_hints_from_bindings_and_leaves_unbound_none() {
        let mut kb = KeybindRegistry::new();
        kb.register(chord("ctrl+shift+t"), Action::NewTab);
        let reg = ActionRegistry::core(&kb);
        assert_eq!(
            reg.find(ActionId::NewTab).unwrap().keybind_hint(),
            Some("Ctrl+Shift+T"),
        );
        // Unbound action has no hint.
        assert_eq!(reg.find(ActionId::ClosePane).unwrap().keybind_hint(), None);
    }

    #[test]
    fn core_with_default_keybinds_resolves_wired_hints() {
        // The GUI startup path: defaults' chord strings must survive the
        // parse -> resolve -> format round trip.
        let reg = ActionRegistry::core(&KeybindRegistry::with_defaults());
        let expected_new_tab = if cfg!(target_os = "macos") {
            "Cmd+T"
        } else {
            "Ctrl+Shift+T"
        };
        assert_eq!(
            reg.find(ActionId::NewTab).unwrap().keybind_hint(),
            Some(expected_new_tab)
        );
        let expected_palette = if cfg!(target_os = "macos") {
            "Cmd+P"
        } else {
            "Ctrl+Shift+P"
        };
        assert_eq!(
            reg.find(ActionId::ShowCommandPalette)
                .unwrap()
                .keybind_hint(),
            Some(expected_palette)
        );
        for id in [
            ActionId::ClosePane,
            ActionId::NextTab,
            ActionId::PreviousTab,
        ] {
            assert!(
                reg.find(id).unwrap().keybind_hint().is_some(),
                "{} should have a default hint",
                id.as_str()
            );
        }
    }

    #[test]
    fn split_hints_resolve_across_direction_variants() {
        // The collapse must work across *different* SplitPane values, not only
        // repeated identical ones: left and right both feed SplitPaneRight,
        // and the later registration wins.
        let mut kb = KeybindRegistry::new();
        kb.register(chord("ctrl+h"), split("left"));
        kb.register(chord("ctrl+l"), split("right"));
        kb.register(chord("ctrl+j"), split("up"));
        let reg = ActionRegistry::core(&kb);
        assert_eq!(
            reg.find(ActionId::SplitPaneRight).unwrap().keybind_hint(),
            Some("Ctrl+L")
        );
        assert_eq!(
            reg.find(ActionId::SplitPaneDown).unwrap().keybind_hint(),
            Some("Ctrl+J")
        );
    }

    #[test]
    fn hint_resolution_takes_the_last_binding() {
        let mut kb = KeybindRegistry::new();
        kb.register(chord("ctrl+t"), Action::NewTab);
        kb.register(chord("super+n"), Action::NewTab);
        let reg = ActionRegistry::core(&kb);
        let expected = if cfg!(target_os = "macos") {
            "Cmd+N"
        } else {
            "Super+N"
        };
        assert_eq!(
            reg.find(ActionId::NewTab).unwrap().keybind_hint(),
            Some(expected)
        );
    }

    #[test]
    fn hint_resolution_skips_a_trailing_physical_binding() {
        // A later physical bind (no display label) must not blank a hint an
        // earlier displayable bind can provide.
        let mut kb = KeybindRegistry::new();
        kb.register(chord("ctrl+t"), Action::NewTab);
        let physical = KeyChord {
            ctrl: true,
            alt: false,
            shift: false,
            super_key: false,
            key: KeyName::Physical(PhysicalKeyId::Digit1),
        };
        kb.register(physical, Action::NewTab);
        let reg = ActionRegistry::core(&kb);
        assert_eq!(
            reg.find(ActionId::NewTab).unwrap().keybind_hint(),
            Some("Ctrl+T")
        );
    }

    #[test]
    fn hint_is_none_when_the_only_binding_is_physical() {
        let mut kb = KeybindRegistry::new();
        let physical = KeyChord {
            ctrl: true,
            alt: false,
            shift: false,
            super_key: false,
            key: KeyName::Physical(PhysicalKeyId::Digit1),
        };
        kb.register(physical, Action::NewTab);
        let reg = ActionRegistry::core(&kb);
        assert_eq!(reg.find(ActionId::NewTab).unwrap().keybind_hint(), None);
    }

    #[test]
    fn hint_ignores_a_chord_rebound_to_another_action() {
        // The same chord is reused for a different action: lookup now runs
        // ClosePane, so New Tab must not still advertise it.
        let mut kb = KeybindRegistry::new();
        kb.register(chord("ctrl+t"), Action::NewTab);
        kb.register(chord("ctrl+t"), Action::ClosePane);
        let reg = ActionRegistry::core(&kb);
        assert_eq!(reg.find(ActionId::NewTab).unwrap().keybind_hint(), None);
        assert_eq!(
            reg.find(ActionId::ClosePane).unwrap().keybind_hint(),
            Some("Ctrl+T")
        );
    }

    #[test]
    fn hint_survives_when_only_one_of_several_chords_is_shadowed() {
        // Shadowing one of an action's chords must not blank the hint its
        // surviving chord still provides — the filter is per-chord, not
        // per-action.
        let mut kb = KeybindRegistry::new();
        kb.register(chord("ctrl+t"), Action::NewTab);
        kb.register(chord("super+n"), Action::NewTab);
        kb.register(chord("ctrl+t"), Action::ClosePane);
        let reg = ActionRegistry::core(&kb);
        let expected = if cfg!(target_os = "macos") {
            "Cmd+N"
        } else {
            "Super+N"
        };
        assert_eq!(
            reg.find(ActionId::NewTab).unwrap().keybind_hint(),
            Some(expected)
        );
        assert_eq!(
            reg.find(ActionId::ClosePane).unwrap().keybind_hint(),
            Some("Ctrl+T")
        );
    }

    #[test]
    fn all_enumerates_every_variant() {
        // The exhaustive match fails to compile if a variant is added, forcing
        // this test (and ALL) to be updated — ALL membership is otherwise the
        // one catalog fact the compiler does not enforce.
        for id in ActionId::ALL {
            match id {
                ActionId::SplitPaneRight
                | ActionId::SplitPaneDown
                | ActionId::ClosePane
                | ActionId::FocusPaneLeft
                | ActionId::FocusPaneRight
                | ActionId::FocusPaneUp
                | ActionId::FocusPaneDown
                | ActionId::NewTab
                | ActionId::CloseTab
                | ActionId::NextTab
                | ActionId::PreviousTab
                | ActionId::ToggleFullscreen
                | ActionId::ShowCommandPalette
                | ActionId::ReloadConfig => {}
            }
        }
        assert_eq!(ActionId::ALL.len(), 14);
    }

    #[test]
    fn performable_filters_to_the_current_context() {
        let reg = ActionRegistry::core(&KeybindRegistry::new());
        let ctx = ActionContext {
            pane_count: 1,
            tab_count: 1,
            ..Default::default()
        };
        let ids: Vec<ActionId> = reg.performable(ctx).map(RegisteredAction::id).collect();
        // Single pane, single tab, no neighbors: only the unconditional actions.
        assert_eq!(
            ids,
            vec![
                ActionId::SplitPaneRight,
                ActionId::SplitPaneDown,
                ActionId::NewTab,
                ActionId::ToggleFullscreen,
                ActionId::ShowCommandPalette,
                ActionId::ReloadConfig,
            ],
        );
    }

    #[test]
    fn performable_yields_the_full_catalog_in_an_open_context() {
        // The ordinary multi-pane case: every gate open, nothing filtered.
        let reg = ActionRegistry::core(&KeybindRegistry::new());
        let ctx = ActionContext {
            pane_count: 2,
            tab_count: 2,
            can_focus_left: true,
            can_focus_right: true,
            can_focus_up: true,
            can_focus_down: true,
        };
        let ids: Vec<ActionId> = reg.performable(ctx).map(RegisteredAction::id).collect();
        assert_eq!(ids, ActionId::ALL.to_vec());
    }

    #[test]
    fn every_catalog_id_is_reachable_from_a_keybind_action() {
        // Guards the reverse direction of action_id_of: a catalog id no Action
        // maps to could never show a keybind hint, and the wildcard arm means
        // the compiler won't catch that omission.
        let actions = [
            split("right"),
            split("down"),
            Action::ClosePane,
            Action::FocusPaneDirection("left".into()),
            Action::FocusPaneDirection("right".into()),
            Action::FocusPaneDirection("up".into()),
            Action::FocusPaneDirection("down".into()),
            Action::NewTab,
            Action::CloseTab,
            Action::NextTab,
            Action::PreviousTab,
            Action::ToggleFullscreen,
            Action::ShowCommandPalette,
            Action::ReloadConfig,
        ];
        let mapped: Vec<ActionId> = actions.iter().filter_map(action_id_of).collect();
        assert_eq!(mapped, ActionId::ALL.to_vec());
    }
}
