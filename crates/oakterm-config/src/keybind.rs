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

/// The key component of a chord (character or named key).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyName {
    /// Single character: 'a', '1', '/', etc.
    Character(char),
    /// Named key: `ArrowUp`, `F1`, `Enter`, etc.
    Named(NamedKeyId),
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
        let s = s.trim();
        if s.is_empty() {
            return Err("key chord cannot be empty".to_string());
        }

        let parts: Vec<&str> = s.split('+').collect();
        if parts.is_empty() {
            return Err("key chord cannot be empty".to_string());
        }

        let (modifier_parts, key_part) = parts.split_at(parts.len() - 1);
        let key_str = key_part[0].trim();
        if key_str.is_empty() {
            return Err("key chord has no key after modifiers".to_string());
        }

        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut super_key = false;

        for &m in modifier_parts {
            let m = m.trim().to_lowercase();
            match m.as_str() {
                "ctrl" | "control" => {
                    if ctrl {
                        return Err("duplicate modifier 'ctrl'".to_string());
                    }
                    ctrl = true;
                }
                "alt" | "option" | "opt" => {
                    if alt {
                        return Err("duplicate modifier 'alt'".to_string());
                    }
                    alt = true;
                }
                "shift" => {
                    if shift {
                        return Err("duplicate modifier 'shift'".to_string());
                    }
                    shift = true;
                }
                "super" | "cmd" | "command" | "win" => {
                    if super_key {
                        return Err("duplicate modifier 'super'".to_string());
                    }
                    super_key = true;
                }
                other => {
                    return Err(format!("unknown modifier '{other}'"));
                }
            }
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
            ctrl,
            alt,
            shift,
            super_key,
            key,
        })
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
    /// Show the command palette.
    ShowCommandPalette,

    /// Lua callback function.
    Callback(RegistryKey),
}

/// Registry of key chord → action bindings.
pub struct KeybindRegistry {
    bindings: Vec<(KeyChord, Action)>,
}

impl KeybindRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bindings: Vec::new(),
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
        let mut reg = Self::new();
        let defaults = [
            ("shift+pageup", Action::ScrollUp(0)),
            ("shift+pagedown", Action::ScrollDown(0)),
            ("shift+home", Action::ScrollUp(999_999)),
            ("shift+end", Action::ScrollDown(999_999)),
            ("super+shift+up", Action::ScrollToPrompt(-1)),
            ("super+shift+down", Action::ScrollToPrompt(1)),
        ];
        for (chord_str, action) in defaults {
            // These are hardcoded strings; parse cannot fail.
            let chord = KeyChord::parse(chord_str).expect("default keybind parse");
            reg.register(chord, action);
        }
        reg
    }

    /// Register a keybind. Last registration for a chord wins.
    pub fn register(&mut self, chord: KeyChord, action: Action) {
        self.bindings.push((chord, action));
    }

    /// Look up the action for a chord. Returns the last match (user
    /// config overrides defaults).
    #[must_use]
    pub fn lookup(&self, chord: &KeyChord) -> Option<&Action> {
        self.bindings
            .iter()
            .rev()
            .find(|(c, _)| c == chord)
            .map(|(_, a)| a)
    }

    /// Look up the index of the matching binding for a chord.
    /// Use with `get()` when you need to release the borrow before acting.
    #[must_use]
    pub fn lookup_index(&self, chord: &KeyChord) -> Option<usize> {
        self.bindings
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (c, _))| c == chord)
            .map(|(i, _)| i)
    }

    /// Get the action at a specific index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Action> {
        self.bindings.get(index).map(|(_, a)| a)
    }

    /// Number of registered bindings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Remove all `Callback` registry keys and clear bindings.
    pub fn cleanup(&mut self, lua: &Lua) {
        for (_, action) in self.bindings.drain(..) {
            if let Action::Callback(key) = action {
                if let Err(e) = lua.remove_registry_value(key) {
                    tracing::warn!(error = %e, "failed to clean up keybind callback");
                }
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
}
