//! Keybind registry: key chord parsing, action types, and binding lookup.
//!
//! `oakterm.keybind(key, action)` registers bindings during config evaluation.
//! The registry stores `(KeyChord, Action)` pairs and supports lookup by chord.
//! Last registration wins on conflict (user config overrides defaults).

use mlua::{Lua, RegistryKey};

/// Named keys that map to winit's `NamedKey` variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedKeyId {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    Enter,
    Backspace,
    Escape,
    Delete,
    Insert,
    Space,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

impl NamedKeyId {
    /// Parse a case-insensitive key name string.
    fn parse(s: &str) -> Option<Self> {
        match s {
            "arrowup" | "up" => Some(Self::ArrowUp),
            "arrowdown" | "down" => Some(Self::ArrowDown),
            "arrowleft" | "left" => Some(Self::ArrowLeft),
            "arrowright" | "right" => Some(Self::ArrowRight),
            "home" => Some(Self::Home),
            "end" => Some(Self::End),
            "pageup" => Some(Self::PageUp),
            "pagedown" => Some(Self::PageDown),
            "tab" => Some(Self::Tab),
            "enter" | "return" => Some(Self::Enter),
            "backspace" => Some(Self::Backspace),
            "escape" | "esc" => Some(Self::Escape),
            "delete" | "del" => Some(Self::Delete),
            "insert" | "ins" => Some(Self::Insert),
            "space" => Some(Self::Space),
            "f1" => Some(Self::F1),
            "f2" => Some(Self::F2),
            "f3" => Some(Self::F3),
            "f4" => Some(Self::F4),
            "f5" => Some(Self::F5),
            "f6" => Some(Self::F6),
            "f7" => Some(Self::F7),
            "f8" => Some(Self::F8),
            "f9" => Some(Self::F9),
            "f10" => Some(Self::F10),
            "f11" => Some(Self::F11),
            "f12" => Some(Self::F12),
            _ => None,
        }
    }
}

/// A physical key location, layout-independent (winit `KeyCode`). Used
/// for position-based binds — a chord that should fire on the same key
/// regardless of the character the layout prints there. Scoped to the
/// number row (TREK-268); extend as other positional binds need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhysicalKeyId {
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
}

/// The key component of a chord: a logical character, a named key, or a
/// physical key location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyName {
    /// Single character: 'a', '1', '/', etc.
    Character(char),
    /// Named key: `ArrowUp`, `F1`, `Enter`, etc.
    Named(NamedKeyId),
    /// Physical key location, matched regardless of layout (Spec-0011
    /// keybind lookup; number-row binds like `oak_mod+1`).
    Physical(PhysicalKeyId),
}

/// A parsed key chord like "ctrl+shift+a" or "super+t".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(clippy::struct_excessive_bools)] // Modifiers are naturally booleans.
pub struct KeyChord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
    pub key: KeyName,
}

/// A set of modifier keys, as parsed from a config string like
/// `"ctrl+shift"`. The `oak_mod` value is one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)] // Modifiers are naturally booleans.
pub struct ModifierSet {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}

impl ModifierSet {
    /// Parse a modifiers-only combo like `"ctrl+shift"` or `"super"`.
    ///
    /// # Errors
    ///
    /// Errors when empty, on duplicate modifiers, or when any part is
    /// not a modifier name.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("modifier combo cannot be empty".to_string());
        }
        let mut mods = Self::default();
        for part in s.split('+') {
            apply_modifier(&mut mods, &part.trim().to_lowercase())?;
        }
        Ok(mods)
    }
}

/// Set the named modifier in `mods`, erroring on duplicates and
/// unknown names. Shared by chord and modifier-combo parsing.
fn apply_modifier(mods: &mut ModifierSet, name: &str) -> Result<(), String> {
    let flag = match name {
        "ctrl" | "control" => &mut mods.ctrl,
        "alt" | "option" | "opt" => &mut mods.alt,
        "shift" => &mut mods.shift,
        "super" | "cmd" | "command" | "win" => &mut mods.super_key,
        other => return Err(format!("unknown modifier '{other}'")),
    };
    if *flag {
        return Err(format!("duplicate modifier '{name}'"));
    }
    *flag = true;
    Ok(())
}

impl KeyChord {
    /// Parse a key chord string.
    ///
    /// Format: `modifier+modifier+key` where modifiers are optional.
    /// Modifier aliases: `ctrl`/`control`, `alt`/`option`/`opt`,
    /// `shift`, `super`/`cmd`/`command`/`win`.
    /// Key names are case-insensitive. Single characters are lowercase.
    ///
    /// # Errors
    ///
    /// Returns an error string if the chord is empty, has unknown
    /// modifiers/keys, or has duplicate modifiers.
    pub fn parse(s: &str) -> Result<Self, String> {
        Self::parse_impl(s, None)
    }

    /// Parse a chord that may use the `oak_mod` pseudo-modifier, which
    /// expands to `oak_mod`'s modifier set. Overlap between `oak_mod`'s
    /// modifiers and explicit ones folds silently (ADR-0011: on a
    /// platform whose `oak_mod` contains Shift, `oak_mod+shift+x`
    /// collapses); explicit duplicates still error.
    ///
    /// # Errors
    ///
    /// Same conditions as [`KeyChord::parse`].
    pub fn parse_with_oak_mod(s: &str, oak_mod: ModifierSet) -> Result<Self, String> {
        Self::parse_impl(s, Some(oak_mod))
    }

    fn parse_impl(s: &str, oak_mod: Option<ModifierSet>) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("key chord cannot be empty".to_string());
        }

        let parts: Vec<&str> = s.split('+').collect();
        let (modifier_parts, key_part) = parts.split_at(parts.len() - 1);
        let key_str = key_part[0].trim();
        if key_str.is_empty() {
            return Err("key chord has no key after modifiers".to_string());
        }

        // Explicit modifiers dup-check among themselves; oak_mod's set
        // ORs in afterward so overlap folds instead of erroring.
        let mut explicit = ModifierSet::default();
        let mut from_oak_mod = ModifierSet::default();
        for &m in modifier_parts {
            let m = m.trim().to_lowercase();
            if m == "oak_mod" {
                let Some(mods) = oak_mod else {
                    return Err("'oak_mod' is not available in this context".to_string());
                };
                from_oak_mod = mods;
                continue;
            }
            apply_modifier(&mut explicit, &m)?;
        }

        let lower = key_str.to_lowercase();
        let key = if let Some(named) = NamedKeyId::parse(&lower) {
            KeyName::Named(named)
        } else {
            let chars: Vec<char> = lower.chars().collect();
            if chars.len() == 1 {
                KeyName::Character(chars[0])
            } else {
                return Err(format!("unknown key '{key_str}'"));
            }
        };

        Ok(Self {
            ctrl: explicit.ctrl || from_oak_mod.ctrl,
            alt: explicit.alt || from_oak_mod.alt,
            shift: explicit.shift || from_oak_mod.shift,
            super_key: explicit.super_key || from_oak_mod.super_key,
            key,
        })
    }

    /// Format the chord as a display hint (e.g. `"Cmd+P"`, `"Ctrl+Shift+\"`).
    ///
    /// Returns `None` for physical-position keys, which have no stable label
    /// outside a keyboard layout.
    #[must_use]
    pub fn display_hint(&self) -> Option<String> {
        let key = match &self.key {
            KeyName::Character(c) => c.to_ascii_uppercase().to_string(),
            KeyName::Named(named) => named_label(*named).to_string(),
            KeyName::Physical(_) => return None,
        };
        let mut parts: Vec<&str> = Vec::new();
        if self.super_key {
            parts.push(SUPER_LABEL);
        }
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        let mut out = parts.join("+");
        if !out.is_empty() {
            out.push('+');
        }
        out.push_str(&key);
        Some(out)
    }
}

#[cfg(target_os = "macos")]
const SUPER_LABEL: &str = "Cmd";
#[cfg(not(target_os = "macos"))]
const SUPER_LABEL: &str = "Super";

fn named_label(named: NamedKeyId) -> &'static str {
    match named {
        NamedKeyId::ArrowUp => "Up",
        NamedKeyId::ArrowDown => "Down",
        NamedKeyId::ArrowLeft => "Left",
        NamedKeyId::ArrowRight => "Right",
        NamedKeyId::Home => "Home",
        NamedKeyId::End => "End",
        NamedKeyId::PageUp => "PageUp",
        NamedKeyId::PageDown => "PageDown",
        NamedKeyId::Tab => "Tab",
        NamedKeyId::Enter => "Enter",
        NamedKeyId::Backspace => "Backspace",
        NamedKeyId::Escape => "Esc",
        NamedKeyId::Delete => "Delete",
        NamedKeyId::Insert => "Insert",
        NamedKeyId::Space => "Space",
        NamedKeyId::F1 => "F1",
        NamedKeyId::F2 => "F2",
        NamedKeyId::F3 => "F3",
        NamedKeyId::F4 => "F4",
        NamedKeyId::F5 => "F5",
        NamedKeyId::F6 => "F6",
        NamedKeyId::F7 => "F7",
        NamedKeyId::F8 => "F8",
        NamedKeyId::F9 => "F9",
        NamedKeyId::F10 => "F10",
        NamedKeyId::F11 => "F11",
        NamedKeyId::F12 => "F12",
    }
}

/// Terminal action triggered by a keybind.
#[derive(Debug)]
pub enum Action {
    // Phase 0 actions (implemented):
    /// Scroll up N lines (0 = one page).
    ScrollUp(u32),
    /// Scroll down N lines (0 = one page).
    ScrollDown(u32),
    /// Jump to previous (-1) or next (1) prompt.
    ScrollToPrompt(i32),
    /// Send raw bytes to the PTY.
    SendString(Vec<u8>),
    /// Copy selection to clipboard.
    Copy,
    /// Paste from clipboard.
    Paste,
    /// Toggle fullscreen mode.
    ToggleFullscreen,
    /// Trigger config reload.
    ReloadConfig,

    // Phase 1 stubs (need multiplexer):
    /// Split pane in given direction with size ratio.
    SplitPane { direction: String, size: f64 },
    /// Close the focused pane.
    ClosePane,
    /// Focus pane in given direction.
    FocusPaneDirection(String),
    /// Open a new tab.
    NewTab,
    /// Close the focused tab.
    CloseTab,
    /// Switch to the tab at a 1-based strip index.
    SwitchTab(std::num::NonZeroU32),
    /// Switch to the next tab, wrapping.
    NextTab,
    /// Switch to the previous tab, wrapping.
    PreviousTab,
    /// Show the command palette.
    ShowCommandPalette,
    /// Enter copy mode on the focused pane.
    EnterCopyMode,

    /// Lua callback function.
    Callback(RegistryKey),
}

/// ADR-0011's platform default for `oak_mod`, used when
/// `oakterm.config.oak_mod` is unset.
#[must_use]
pub fn default_oak_mod() -> &'static str {
    if cfg!(target_os = "macos") {
        "super"
    } else {
        "ctrl+shift"
    }
}

/// Tab-cycling chords are platform conventions, not `oak_mod`
/// derivations: on Linux `oak_mod+Shift+[` folds into `oak_mod+[`,
/// which ADR-0011 reserves for copy mode.
#[cfg(target_os = "macos")]
pub(crate) const NEXT_TAB_CHORD: &str = "super+shift+]";
#[cfg(target_os = "macos")]
pub(crate) const PREVIOUS_TAB_CHORD: &str = "super+shift+[";
#[cfg(not(target_os = "macos"))]
pub(crate) const NEXT_TAB_CHORD: &str = "ctrl+pagedown";
#[cfg(not(target_os = "macos"))]
pub(crate) const PREVIOUS_TAB_CHORD: &str = "ctrl+pageup";

/// A chord-to-action table with last-registration-wins lookup. Backs
/// each ADR-0011 dispatch layer: the leader table, an active modal
/// table (copy mode, resize mode), and the default bindings. What
/// happens on a miss is the dispatcher's policy, not the table's.
#[derive(Debug, Default)]
pub struct KeyTable {
    bindings: Vec<(KeyChord, Action)>,
}

impl KeyTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a binding. Last registration for a chord wins.
    pub fn bind(&mut self, chord: KeyChord, action: Action) {
        self.bindings.push((chord, action));
    }

    /// Look up the action for a chord, last match wins.
    #[must_use]
    pub fn lookup(&self, chord: &KeyChord) -> Option<&Action> {
        self.bindings
            .iter()
            .rev()
            .find(|(c, _)| c == chord)
            .map(|(_, a)| a)
    }

    /// Index of the action for a chord, last match wins. Resolve with
    /// [`KeyTable::get`].
    #[must_use]
    pub fn lookup_index(&self, chord: &KeyChord) -> Option<usize> {
        self.bindings
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (c, _))| c == chord)
            .map(|(i, _)| i)
    }

    /// The action at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Action> {
        self.bindings.get(index).map(|(_, a)| a)
    }
}

/// Registry of key chord → action bindings, plus the separate leader
/// table (`leader+X` bindings, matched only while a leader press is
/// pending; ADR-0011 dispatch layer 1).
pub struct KeybindRegistry {
    bindings: KeyTable,
    leader_bindings: KeyTable,
}

impl KeybindRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bindings: KeyTable::new(),
            leader_bindings: KeyTable::new(),
        }
    }

    /// Create a registry pre-populated with default keybinds.
    ///
    /// These match the previously hardcoded scrollback navigation bindings.
    /// User config overrides these since later registrations win on lookup.
    ///
    /// # Panics
    ///
    /// Panics if a hardcoded default chord string fails to parse (indicates
    /// a bug in the default definitions, not a runtime condition).
    #[must_use]
    pub fn with_defaults() -> Self {
        let mods = ModifierSet::parse(default_oak_mod()).expect("platform oak_mod parses");
        Self::with_defaults_for(mods)
    }

    /// Create a registry pre-populated with default keybinds expanded
    /// against the given `oak_mod` modifier set.
    ///
    /// # Panics
    ///
    /// Panics if a hardcoded default chord string fails to parse
    /// (indicates a bug in the default definitions, not a runtime
    /// condition).
    #[must_use]
    pub fn with_defaults_for(oak_mod: ModifierSet) -> Self {
        let mut reg = Self::new();
        let split = |direction: &str| Action::SplitPane {
            direction: direction.to_string(),
            size: 0.5,
        };
        let defaults = [
            ("shift+pageup", Action::ScrollUp(0)),
            ("shift+pagedown", Action::ScrollDown(0)),
            ("shift+home", Action::ScrollUp(999_999)),
            ("shift+end", Action::ScrollDown(999_999)),
            // On a platform whose oak_mod contains Shift these collapse
            // (oak_mod+shift folds; ADR-0011).
            ("oak_mod+shift+up", Action::ScrollToPrompt(-1)),
            ("oak_mod+shift+down", Action::ScrollToPrompt(1)),
            ("oak_mod+\\", split("right")),
            ("oak_mod+-", split("down")),
            ("oak_mod+h", Action::FocusPaneDirection("left".to_string())),
            ("oak_mod+j", Action::FocusPaneDirection("down".to_string())),
            ("oak_mod+k", Action::FocusPaneDirection("up".to_string())),
            ("oak_mod+l", Action::FocusPaneDirection("right".to_string())),
            ("oak_mod+t", Action::NewTab),
            ("oak_mod+w", Action::ClosePane),
            ("oak_mod+p", Action::ShowCommandPalette),
            ("oak_mod+[", Action::EnterCopyMode),
            (NEXT_TAB_CHORD, Action::NextTab),
            (PREVIOUS_TAB_CHORD, Action::PreviousTab),
        ];
        for (chord_str, action) in defaults {
            // These are hardcoded strings; parse cannot fail.
            let chord =
                KeyChord::parse_with_oak_mod(chord_str, oak_mod).expect("default keybind parse");
            reg.register(chord, action);
        }
        // Tab-switch digits fire from either representation of "the N
        // key", registered as the union so every layout works without
        // stealing keypad navigation (TREK-268):
        //   - logical character 'N' — the US number row, and the numpad
        //     with NumLock on (NumLock off emits a named navigation key
        //     that matches neither, so it reaches the PTY);
        //   - physical number-row position — layouts where that key's
        //     base character isn't 'N' (AZERTY etc.), reached via the
        //     lookup's physical fallback.
        let tab_digits = [
            (1u32, PhysicalKeyId::Digit1),
            (2, PhysicalKeyId::Digit2),
            (3, PhysicalKeyId::Digit3),
            (4, PhysicalKeyId::Digit4),
            (5, PhysicalKeyId::Digit5),
            (6, PhysicalKeyId::Digit6),
            (7, PhysicalKeyId::Digit7),
            (8, PhysicalKeyId::Digit8),
            (9, PhysicalKeyId::Digit9),
        ];
        for (i, physical) in tab_digits {
            let index = std::num::NonZeroU32::new(i).expect("1..=9 is nonzero");
            let logical = KeyChord::parse_with_oak_mod(&format!("oak_mod+{i}"), oak_mod)
                .expect("default keybind parse");
            let mut positional = logical.clone();
            positional.key = KeyName::Physical(physical);
            reg.register(logical, Action::SwitchTab(index));
            reg.register(positional, Action::SwitchTab(index));
        }
        reg
    }

    /// Register a keybind. Last registration for a chord wins.
    pub fn register(&mut self, chord: KeyChord, action: Action) {
        self.bindings.bind(chord, action);
    }

    /// Register a leader-table binding (`leader+X`). Last registration
    /// for a chord wins.
    pub fn register_leader(&mut self, chord: KeyChord, action: Action) {
        self.leader_bindings.bind(chord, action);
    }

    /// Index of the leader-table action for a chord, last match wins.
    /// Resolve with [`KeybindRegistry::get_leader`].
    #[must_use]
    pub fn lookup_leader_index(&self, chord: &KeyChord) -> Option<usize> {
        self.leader_bindings.lookup_index(chord)
    }

    /// The leader-table action at `index`.
    #[must_use]
    pub fn get_leader(&self, index: usize) -> Option<&Action> {
        self.leader_bindings.get(index)
    }

    /// Whether any `leader+X` bindings are registered.
    #[must_use]
    pub fn has_leader_bindings(&self) -> bool {
        !self.leader_bindings.bindings.is_empty()
    }

    /// Look up the action for a chord. Returns the last match (user
    /// config overrides defaults).
    #[must_use]
    pub fn lookup(&self, chord: &KeyChord) -> Option<&Action> {
        self.bindings.lookup(chord)
    }

    /// Look up the index of the matching binding for a chord.
    /// Use with `get()` when you need to release the borrow before acting.
    #[must_use]
    pub fn lookup_index(&self, chord: &KeyChord) -> Option<usize> {
        self.bindings.lookup_index(chord)
    }

    /// Get the action at a specific index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Action> {
        self.bindings.get(index)
    }

    /// Iterate over the *effective* `(chord, action)` bindings in registration
    /// order: entries shadowed by a later registration of the same chord are
    /// skipped, so each yielded pair is exactly what [`Self::lookup`] resolves
    /// for its chord. Reverse for last-registered-first.
    #[must_use]
    pub fn effective_bindings(&self) -> impl DoubleEndedIterator<Item = (&KeyChord, &Action)> {
        self.bindings
            .bindings
            .iter()
            .enumerate()
            .filter(|(i, (chord, _))| self.lookup_index(chord) == Some(*i))
            .map(|(_, (c, a))| (c, a))
    }

    /// Number of registered bindings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bindings.bindings.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bindings.bindings.is_empty()
    }

    /// Remove all `Callback` registry keys and clear bindings.
    pub fn cleanup(&mut self, lua: &Lua) {
        let drained = self
            .bindings
            .bindings
            .drain(..)
            .chain(self.leader_bindings.bindings.drain(..));
        for (_, action) in drained {
            if let Action::Callback(key) = action
                && let Err(e) = lua.remove_registry_value(key)
            {
                tracing::warn!(error = %e, "failed to clean up keybind callback");
            }
        }
    }
}

impl Default for KeybindRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform_mods() -> ModifierSet {
        ModifierSet::parse(default_oak_mod()).unwrap()
    }

    #[test]
    fn modifier_set_parses_combos_and_rejects_keys() {
        let m = ModifierSet::parse("ctrl+shift").unwrap();
        assert!(m.ctrl && m.shift && !m.alt && !m.super_key);
        let m = ModifierSet::parse("super").unwrap();
        assert!(m.super_key && !m.ctrl);
        assert!(
            ModifierSet::parse("ctrl+x").is_err(),
            "keys are not modifiers"
        );
        assert!(ModifierSet::parse("").is_err());
        assert!(ModifierSet::parse("ctrl+ctrl").is_err());
    }

    #[test]
    fn oak_mod_token_expands_to_the_configured_set() {
        let mods = ModifierSet::parse("ctrl+shift").unwrap();
        let chord = KeyChord::parse_with_oak_mod("oak_mod+t", mods).unwrap();
        assert!(chord.ctrl && chord.shift && !chord.super_key);
        assert_eq!(chord.key, KeyName::Character('t'));
    }

    #[test]
    fn oak_mod_overlapping_explicit_modifier_folds_instead_of_erroring() {
        // ADR-0011's OAK_MOD_SHIFT collapse: oak_mod+shift on a platform
        // whose oak_mod already contains shift.
        let mods = ModifierSet::parse("ctrl+shift").unwrap();
        let chord = KeyChord::parse_with_oak_mod("oak_mod+shift+up", mods).unwrap();
        assert!(chord.ctrl && chord.shift);
        assert_eq!(chord.key, KeyName::Named(NamedKeyId::ArrowUp));
        // Explicit duplicates still error.
        assert!(KeyChord::parse_with_oak_mod("oak_mod+shift+shift+up", mods).is_err());
    }

    #[test]
    fn oak_mod_token_errors_in_plain_parse() {
        assert!(KeyChord::parse("oak_mod+t").is_err());
    }

    #[test]
    fn with_defaults_follows_the_oak_mod_set() {
        let mods = ModifierSet::parse("ctrl+alt").unwrap();
        let reg = KeybindRegistry::with_defaults_for(mods);
        let chord = KeyChord::parse("ctrl+alt+t").unwrap();
        assert!(matches!(reg.lookup(&chord), Some(Action::NewTab)));
        let prompt = KeyChord::parse("ctrl+alt+shift+up").unwrap();
        assert!(matches!(
            reg.lookup(&prompt),
            Some(Action::ScrollToPrompt(-1))
        ));
    }

    #[test]
    fn key_table_lookup_is_last_registration_wins() {
        let mut table = KeyTable::new();
        let h = KeyChord::parse("h").unwrap();
        table.bind(h.clone(), Action::ScrollUp(1));
        table.bind(h.clone(), Action::ScrollDown(1));
        assert!(matches!(table.lookup(&h), Some(Action::ScrollDown(1))));
        assert!(table.lookup(&KeyChord::parse("j").unwrap()).is_none());
    }

    #[test]
    fn cleanup_drains_callbacks_from_both_tables() {
        let lua = Lua::new();
        let f1 = lua.create_function(|_, ()| Ok(())).unwrap();
        let f2 = lua.create_function(|_, ()| Ok(())).unwrap();
        let mut reg = KeybindRegistry::new();
        reg.register(
            KeyChord::parse("a").unwrap(),
            Action::Callback(lua.create_registry_value(f1).unwrap()),
        );
        reg.register_leader(
            KeyChord::parse("b").unwrap(),
            Action::Callback(lua.create_registry_value(f2).unwrap()),
        );
        reg.cleanup(&lua);
        assert!(reg.is_empty());
        assert!(
            !reg.has_leader_bindings(),
            "leader callbacks must drain too or their registry keys leak on reload"
        );
    }

    #[test]
    fn leader_bindings_are_a_separate_namespace() {
        let mut reg = KeybindRegistry::new();
        let chord = KeyChord::parse("5").unwrap();
        reg.register_leader(chord.clone(), Action::ClosePane);
        assert!(reg.lookup(&chord).is_none());
        assert!(reg.has_leader_bindings());
        let idx = reg.lookup_leader_index(&chord).unwrap();
        assert!(matches!(reg.get_leader(idx), Some(Action::ClosePane)));
    }

    #[test]
    fn parse_simple_character() {
        let chord = KeyChord::parse("a").unwrap();
        assert!(!chord.ctrl);
        assert!(!chord.shift);
        assert_eq!(chord.key, KeyName::Character('a'));
    }

    #[test]
    fn parse_ctrl_c() {
        let chord = KeyChord::parse("ctrl+c").unwrap();
        assert!(chord.ctrl);
        assert!(!chord.shift);
        assert_eq!(chord.key, KeyName::Character('c'));
    }

    #[test]
    fn parse_super_shift_t() {
        let chord = KeyChord::parse("super+shift+t").unwrap();
        assert!(chord.super_key);
        assert!(chord.shift);
        assert!(!chord.ctrl);
        assert_eq!(chord.key, KeyName::Character('t'));
    }

    #[test]
    fn parse_cmd_alias() {
        let chord = KeyChord::parse("cmd+k").unwrap();
        assert!(chord.super_key);
        assert_eq!(chord.key, KeyName::Character('k'));
    }

    #[test]
    fn parse_command_alias() {
        let chord = KeyChord::parse("command+k").unwrap();
        assert!(chord.super_key);
    }

    #[test]
    fn parse_option_alias() {
        let chord = KeyChord::parse("option+a").unwrap();
        assert!(chord.alt);
    }

    #[test]
    fn parse_named_key() {
        let chord = KeyChord::parse("shift+pageup").unwrap();
        assert!(chord.shift);
        assert_eq!(chord.key, KeyName::Named(NamedKeyId::PageUp));
    }

    #[test]
    fn parse_f_key() {
        let chord = KeyChord::parse("ctrl+f5").unwrap();
        assert!(chord.ctrl);
        assert_eq!(chord.key, KeyName::Named(NamedKeyId::F5));
    }

    #[test]
    fn parse_arrow_aliases() {
        assert_eq!(
            KeyChord::parse("up").unwrap().key,
            KeyName::Named(NamedKeyId::ArrowUp)
        );
        assert_eq!(
            KeyChord::parse("arrowup").unwrap().key,
            KeyName::Named(NamedKeyId::ArrowUp)
        );
    }

    #[test]
    fn parse_case_insensitive() {
        let chord = KeyChord::parse("Ctrl+Shift+PageUp").unwrap();
        assert!(chord.ctrl);
        assert!(chord.shift);
        assert_eq!(chord.key, KeyName::Named(NamedKeyId::PageUp));
    }

    #[test]
    fn parse_space() {
        let chord = KeyChord::parse("ctrl+space").unwrap();
        assert!(chord.ctrl);
        assert_eq!(chord.key, KeyName::Named(NamedKeyId::Space));
    }

    #[test]
    fn parse_escape_aliases() {
        assert_eq!(
            KeyChord::parse("esc").unwrap().key,
            KeyName::Named(NamedKeyId::Escape)
        );
        assert_eq!(
            KeyChord::parse("escape").unwrap().key,
            KeyName::Named(NamedKeyId::Escape)
        );
    }

    #[test]
    fn parse_empty_error() {
        assert!(KeyChord::parse("").is_err());
    }

    #[test]
    fn parse_unknown_modifier_error() {
        let err = KeyChord::parse("hyper+a").unwrap_err();
        assert!(err.contains("unknown modifier"), "got: {err}");
    }

    #[test]
    fn parse_unknown_key_error() {
        let err = KeyChord::parse("ctrl+banana").unwrap_err();
        assert!(err.contains("unknown key"), "got: {err}");
    }

    #[test]
    fn parse_duplicate_modifier_error() {
        let err = KeyChord::parse("ctrl+ctrl+a").unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn registry_lookup_last_wins() {
        let mut reg = KeybindRegistry::new();
        let chord = KeyChord::parse("ctrl+c").unwrap();
        reg.register(chord.clone(), Action::Copy);
        reg.register(chord.clone(), Action::ReloadConfig);
        let action = reg.lookup(&chord).unwrap();
        assert!(matches!(action, Action::ReloadConfig));
    }

    #[test]
    fn registry_lookup_miss() {
        let reg = KeybindRegistry::new();
        let chord = KeyChord::parse("ctrl+c").unwrap();
        assert!(reg.lookup(&chord).is_none());
    }

    #[test]
    fn registry_len() {
        let mut reg = KeybindRegistry::new();
        assert!(reg.is_empty());
        reg.register(KeyChord::parse("ctrl+a").unwrap(), Action::Copy);
        reg.register(KeyChord::parse("ctrl+b").unwrap(), Action::Paste);
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn display_hint_renders_modifiers_and_keys() {
        let hint = |s: &str| KeyChord::parse(s).unwrap().display_hint();
        assert_eq!(hint("ctrl+shift+\\").as_deref(), Some("Ctrl+Shift+\\"));
        assert_eq!(hint("ctrl+p").as_deref(), Some("Ctrl+P"));
        assert_eq!(hint("shift+pageup").as_deref(), Some("Shift+PageUp"));
        assert_eq!(hint("alt+enter").as_deref(), Some("Alt+Enter"));
        // A bare key must not pick up a stray leading '+'.
        assert_eq!(hint("a").as_deref(), Some("A"));
    }

    #[test]
    fn display_hint_labels_every_named_key() {
        // Pins each named key's display string; a transposed pair (Home/End,
        // Insert/Delete) passes the compiler's exhaustiveness check.
        for (input, expected) in [
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("home", "Home"),
            ("end", "End"),
            ("pageup", "PageUp"),
            ("pagedown", "PageDown"),
            ("tab", "Tab"),
            ("enter", "Enter"),
            ("backspace", "Backspace"),
            ("esc", "Esc"),
            ("delete", "Delete"),
            ("insert", "Insert"),
            ("space", "Space"),
            ("f1", "F1"),
            ("f5", "F5"),
            ("f12", "F12"),
        ] {
            assert_eq!(
                KeyChord::parse(input).unwrap().display_hint().as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn display_hint_super_uses_the_platform_label() {
        let expected = if cfg!(target_os = "macos") {
            "Cmd+T"
        } else {
            "Super+T"
        };
        assert_eq!(
            KeyChord::parse("super+t")
                .unwrap()
                .display_hint()
                .as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn display_hint_orders_all_modifiers_canonically() {
        // Pins the Super -> Ctrl -> Alt -> Shift order against a reordering of
        // the emit blocks; the multi-modifier assertions elsewhere only pin
        // adjacent pairs.
        let expected = if cfg!(target_os = "macos") {
            "Cmd+Ctrl+Alt+Shift+A"
        } else {
            "Super+Ctrl+Alt+Shift+A"
        };
        assert_eq!(
            KeyChord::parse("super+ctrl+alt+shift+a")
                .unwrap()
                .display_hint()
                .as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn display_hint_translates_non_identity_named_keys() {
        // Arrows and Escape are deliberately not their enum names.
        let hint = |s: &str| KeyChord::parse(s).unwrap().display_hint();
        assert_eq!(hint("ctrl+up").as_deref(), Some("Ctrl+Up"));
        assert_eq!(hint("ctrl+left").as_deref(), Some("Ctrl+Left"));
        assert_eq!(hint("esc").as_deref(), Some("Esc"));
    }

    #[test]
    fn display_hint_declines_physical_keys() {
        let physical = KeyChord {
            ctrl: false,
            alt: false,
            shift: false,
            super_key: true,
            key: KeyName::Physical(PhysicalKeyId::Digit1),
        };
        assert_eq!(physical.display_hint(), None);
    }

    #[test]
    fn effective_bindings_skip_shadowed_and_reverse() {
        // Both directions matter: forward is registration order, rev() gives
        // last-registered-first for override resolution.
        let mut reg = KeybindRegistry::new();
        let a = KeyChord::parse("ctrl+a").unwrap();
        let b = KeyChord::parse("ctrl+b").unwrap();
        let c = KeyChord::parse("ctrl+c").unwrap();
        reg.register(a.clone(), Action::Copy);
        reg.register(b.clone(), Action::Paste);
        // Shadows the first ctrl+a: only the NewTab entry is effective.
        reg.register(a.clone(), Action::NewTab);
        reg.register(c.clone(), Action::CloseTab);
        let forward: Vec<(&KeyChord, &Action)> = reg.effective_bindings().collect();
        let forward_chords: Vec<&KeyChord> = forward.iter().map(|(chord, _)| *chord).collect();
        assert_eq!(forward_chords, vec![&b, &a, &c]);
        assert!(matches!(forward[1].1, Action::NewTab));
        let backward: Vec<&KeyChord> = reg
            .effective_bindings()
            .rev()
            .map(|(chord, _)| chord)
            .collect();
        assert_eq!(backward, vec![&c, &a, &b]);
    }

    #[test]
    fn defaults_use_oak_mod_for_prompt_navigation() {
        let reg = KeybindRegistry::with_defaults();
        let up = KeyChord::parse_with_oak_mod("oak_mod+shift+up", platform_mods()).unwrap();
        assert!(matches!(reg.lookup(&up), Some(Action::ScrollToPrompt(-1))));
        let down = KeyChord::parse_with_oak_mod("oak_mod+shift+down", platform_mods()).unwrap();
        assert!(matches!(reg.lookup(&down), Some(Action::ScrollToPrompt(1))));
    }

    #[test]
    fn defaults_include_tab_keybinds() {
        let reg = KeybindRegistry::with_defaults();
        let new_tab = KeyChord::parse_with_oak_mod("oak_mod+t", platform_mods()).unwrap();
        assert!(matches!(reg.lookup(&new_tab), Some(Action::NewTab)));
        let close = KeyChord::parse_with_oak_mod("oak_mod+w", platform_mods()).unwrap();
        assert!(matches!(reg.lookup(&close), Some(Action::ClosePane)));
        let next = KeyChord::parse(NEXT_TAB_CHORD).unwrap();
        assert!(matches!(reg.lookup(&next), Some(Action::NextTab)));
        let prev = KeyChord::parse(PREVIOUS_TAB_CHORD).unwrap();
        assert!(matches!(reg.lookup(&prev), Some(Action::PreviousTab)));
    }

    #[test]
    fn defaults_include_split_and_focus_binds() {
        // ADR-0011's default table rows for the wired split/focus actions.
        let reg = KeybindRegistry::with_defaults();
        let split_right = KeyChord::parse_with_oak_mod("oak_mod+\\", platform_mods()).unwrap();
        assert!(matches!(
            reg.lookup(&split_right),
            Some(Action::SplitPane { direction, .. }) if direction == "right"
        ));
        let split_down = KeyChord::parse_with_oak_mod("oak_mod+-", platform_mods()).unwrap();
        assert!(matches!(
            reg.lookup(&split_down),
            Some(Action::SplitPane { direction, .. }) if direction == "down"
        ));
        for (key, dir) in [("h", "left"), ("j", "down"), ("k", "up"), ("l", "right")] {
            let chord =
                KeyChord::parse_with_oak_mod(&format!("oak_mod+{key}"), platform_mods()).unwrap();
            assert!(matches!(
                reg.lookup(&chord),
                Some(Action::FocusPaneDirection(d)) if d == dir
            ));
        }
    }

    #[test]
    fn defaults_include_the_command_palette() {
        let reg = KeybindRegistry::with_defaults();
        let chord = KeyChord::parse_with_oak_mod("oak_mod+p", platform_mods()).unwrap();
        assert!(matches!(
            reg.lookup(&chord),
            Some(Action::ShowCommandPalette)
        ));
    }

    /// ADR-0011 reserves `oak_mod+[` for copy mode, which is why tab
    /// cycling takes a platform convention instead: on Linux
    /// `oak_mod+shift+[` folds into this very chord.
    #[test]
    fn defaults_include_copy_mode_without_colliding_with_tab_cycling() {
        let reg = KeybindRegistry::with_defaults();
        let chord = KeyChord::parse_with_oak_mod("oak_mod+[", platform_mods()).unwrap();
        assert!(matches!(reg.lookup(&chord), Some(Action::EnterCopyMode)));

        let previous_tab = KeyChord::parse(PREVIOUS_TAB_CHORD).unwrap();
        assert_ne!(chord, previous_tab);
        assert!(matches!(
            reg.lookup(&previous_tab),
            Some(Action::PreviousTab)
        ));
    }

    #[test]
    fn default_tab_switch_binds_match_logical_and_physical_digits() {
        // The oak_mod+[1-9] defaults are registered both logically (US
        // number row / numpad) and by physical position (layout-robust,
        // TREK-268). Both chord shapes must resolve to the same switch.
        let reg = KeybindRegistry::with_defaults();
        let digits = [
            (1u32, PhysicalKeyId::Digit1),
            (2, PhysicalKeyId::Digit2),
            (3, PhysicalKeyId::Digit3),
            (4, PhysicalKeyId::Digit4),
            (5, PhysicalKeyId::Digit5),
            (6, PhysicalKeyId::Digit6),
            (7, PhysicalKeyId::Digit7),
            (8, PhysicalKeyId::Digit8),
            (9, PhysicalKeyId::Digit9),
        ];
        for (i, physical) in digits {
            let logical =
                KeyChord::parse_with_oak_mod(&format!("oak_mod+{i}"), platform_mods()).unwrap();
            assert!(matches!(reg.lookup(&logical), Some(Action::SwitchTab(n)) if n.get() == i));
            let mut positional = logical.clone();
            positional.key = KeyName::Physical(physical);
            assert!(matches!(reg.lookup(&positional), Some(Action::SwitchTab(n)) if n.get() == i));
        }
    }
}
