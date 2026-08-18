//! Translates winit keyboard and mouse input into PTY bytes, keybind
//! chords, and mouse modifier bits.

use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// Convert a winit key event to PTY bytes.
#[must_use]
pub(crate) fn key_to_bytes(key: &Key, text: Option<&str>) -> Option<Vec<u8>> {
    if let Some(t) = text {
        if !t.is_empty() {
            return Some(t.as_bytes().to_vec());
        }
    }

    if let Key::Named(named) = key {
        let seq: &[u8] = match named {
            NamedKey::ArrowUp => b"\x1b[A",
            NamedKey::ArrowDown => b"\x1b[B",
            NamedKey::ArrowRight => b"\x1b[C",
            NamedKey::ArrowLeft => b"\x1b[D",
            NamedKey::Home => b"\x1b[H",
            NamedKey::End => b"\x1b[F",
            NamedKey::Insert => b"\x1b[2~",
            NamedKey::Delete => b"\x1b[3~",
            NamedKey::PageUp => b"\x1b[5~",
            NamedKey::PageDown => b"\x1b[6~",
            NamedKey::Escape => b"\x1b",
            NamedKey::Tab => b"\t",
            NamedKey::Enter => b"\r",
            NamedKey::Backspace => b"\x7f",
            NamedKey::F1 => b"\x1bOP",
            NamedKey::F2 => b"\x1bOQ",
            NamedKey::F3 => b"\x1bOR",
            NamedKey::F4 => b"\x1bOS",
            NamedKey::F5 => b"\x1b[15~",
            NamedKey::F6 => b"\x1b[17~",
            NamedKey::F7 => b"\x1b[18~",
            NamedKey::F8 => b"\x1b[19~",
            NamedKey::F9 => b"\x1b[20~",
            NamedKey::F10 => b"\x1b[21~",
            NamedKey::F11 => b"\x1b[23~",
            NamedKey::F12 => b"\x1b[24~",
            _ => return None,
        };
        return Some(seq.to_vec());
    }

    None
}

/// Convert winit modifier state + logical key to a `KeyChord` for registry lookup.
#[must_use]
pub(crate) fn winit_to_chord(
    modifiers: winit::event::Modifiers,
    logical_key: &Key,
) -> Option<oakterm_config::KeyChord> {
    use oakterm_config::{KeyChord, KeyName, NamedKeyId};

    let state = modifiers.state();
    let key = match logical_key {
        Key::Named(named) => {
            let id = match named {
                NamedKey::ArrowUp => NamedKeyId::ArrowUp,
                NamedKey::ArrowDown => NamedKeyId::ArrowDown,
                NamedKey::ArrowLeft => NamedKeyId::ArrowLeft,
                NamedKey::ArrowRight => NamedKeyId::ArrowRight,
                NamedKey::Home => NamedKeyId::Home,
                NamedKey::End => NamedKeyId::End,
                NamedKey::PageUp => NamedKeyId::PageUp,
                NamedKey::PageDown => NamedKeyId::PageDown,
                NamedKey::Tab => NamedKeyId::Tab,
                NamedKey::Enter => NamedKeyId::Enter,
                NamedKey::Backspace => NamedKeyId::Backspace,
                NamedKey::Escape => NamedKeyId::Escape,
                NamedKey::Delete => NamedKeyId::Delete,
                NamedKey::Insert => NamedKeyId::Insert,
                NamedKey::Space => NamedKeyId::Space,
                NamedKey::F1 => NamedKeyId::F1,
                NamedKey::F2 => NamedKeyId::F2,
                NamedKey::F3 => NamedKeyId::F3,
                NamedKey::F4 => NamedKeyId::F4,
                NamedKey::F5 => NamedKeyId::F5,
                NamedKey::F6 => NamedKeyId::F6,
                NamedKey::F7 => NamedKeyId::F7,
                NamedKey::F8 => NamedKeyId::F8,
                NamedKey::F9 => NamedKeyId::F9,
                NamedKey::F10 => NamedKeyId::F10,
                NamedKey::F11 => NamedKeyId::F11,
                NamedKey::F12 => NamedKeyId::F12,
                _ => return None,
            };
            KeyName::Named(id)
        }
        Key::Character(text) => {
            // Only match single-character inputs. Multi-character strings
            // (e.g., IME composition) should not trigger keybinds.
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyName::Character(ch.to_lowercase().next().unwrap_or(ch))
        }
        _ => return None,
    };

    Some(KeyChord {
        ctrl: state.control_key(),
        alt: state.alt_key(),
        shift: state.shift_key(),
        super_key: state.super_key(),
        key,
    })
}

/// Convert winit modifier state + physical key to a position-based
/// `KeyChord` for registry lookup. Returns `None` for anything but the
/// number row (TREK-268; numpad excluded, see below). Tried as a fallback
/// after the logical [`winit_to_chord`] misses, so digit binds fire on
/// layouts where the base character differs.
#[must_use]
pub(crate) fn physical_to_chord(
    modifiers: winit::event::Modifiers,
    physical_key: PhysicalKey,
) -> Option<oakterm_config::KeyChord> {
    use oakterm_config::{KeyName, PhysicalKeyId};

    let PhysicalKey::Code(code) = physical_key else {
        return None;
    };
    // Number-row keys only. The numpad is deliberately excluded: its
    // physical code is the same regardless of NumLock, so mapping it here
    // would turn NumLock-off keypad navigation (End/PageDown/... under
    // `Numpad1`/`Numpad3`/...) into tab switches. Numpad digits reach the
    // tab binds through the logical path instead — with NumLock on they
    // emit the character 'N', which the logical default catches.
    let digit = match code {
        KeyCode::Digit0 => PhysicalKeyId::Digit0,
        KeyCode::Digit1 => PhysicalKeyId::Digit1,
        KeyCode::Digit2 => PhysicalKeyId::Digit2,
        KeyCode::Digit3 => PhysicalKeyId::Digit3,
        KeyCode::Digit4 => PhysicalKeyId::Digit4,
        KeyCode::Digit5 => PhysicalKeyId::Digit5,
        KeyCode::Digit6 => PhysicalKeyId::Digit6,
        KeyCode::Digit7 => PhysicalKeyId::Digit7,
        KeyCode::Digit8 => PhysicalKeyId::Digit8,
        KeyCode::Digit9 => PhysicalKeyId::Digit9,
        _ => return None,
    };
    let state = modifiers.state();
    Some(oakterm_config::KeyChord {
        ctrl: state.control_key(),
        alt: state.alt_key(),
        shift: state.shift_key(),
        super_key: state.super_key(),
        key: KeyName::Physical(digit),
    })
}

/// What a key press does to an open command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaletteKeyEffect {
    Close,
    Confirm,
    MoveUp,
    MoveDown,
    Backspace,
    /// Printable characters to append to the query.
    Input(String),
    Ignore,
}

/// Interpret a key press for the palette (Spec-0009 Palette Lifecycle).
/// Chorded keys (Ctrl/Super/Alt) other than the Ctrl+p/Ctrl+n aliases are
/// ignored so a modifier chord never leaks its base character into the
/// query; control characters are stripped from text input.
#[must_use]
pub(crate) fn palette_key_effect(
    key: &Key,
    mods: winit::keyboard::ModifiersState,
    text: Option<&str>,
) -> PaletteKeyEffect {
    match key {
        Key::Named(NamedKey::Escape) => PaletteKeyEffect::Close,
        Key::Named(NamedKey::Enter) => PaletteKeyEffect::Confirm,
        Key::Named(NamedKey::ArrowUp) => PaletteKeyEffect::MoveUp,
        Key::Named(NamedKey::ArrowDown) => PaletteKeyEffect::MoveDown,
        Key::Named(NamedKey::Backspace) => PaletteKeyEffect::Backspace,
        Key::Character(s) if mods.control_key() && s.as_str() == "p" => PaletteKeyEffect::MoveUp,
        Key::Character(s) if mods.control_key() && s.as_str() == "n" => PaletteKeyEffect::MoveDown,
        _ => {
            if !mods.control_key() && !mods.super_key() && !mods.alt_key() {
                if let Some(text) = text {
                    let printable: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() {
                        return PaletteKeyEffect::Input(printable);
                    }
                }
            }
            PaletteKeyEffect::Ignore
        }
    }
}

/// Inputs the keybind dispatch pipeline resolves against (ADR-0011).
pub(crate) struct DispatchContext<'a> {
    pub registry: &'a oakterm_config::KeybindRegistry,
    /// Active modal key table (resize mode), if any. Copy mode outranks
    /// it: a pane can be in copy mode while a table is active, and the
    /// pane's own mode wins.
    pub table: Option<&'a oakterm_config::KeyTable>,
    /// The focused pane is in copy mode. Its table matches the character
    /// a key produced rather than a `KeyChord`, so this layer only
    /// reports that copy mode owns the key; `copy_keys` resolves it.
    /// Set together with `table`, this one decides.
    pub copy_mode: bool,
    /// Configured leader key, if any.
    pub leader: Option<&'a oakterm_config::LeaderKey>,
    /// A leader press is pending its follow-up key. Derived from
    /// `App.leader_pending.is_some()` at construction; recompute the
    /// same way if a second construction site is ever added.
    pub leader_pending: bool,
}

/// Where a keypress resolved in the dispatch layers. Indices point into
/// the source table (`KeybindRegistry` bindings, its leader table, or
/// the active `KeyTable`) for the caller's copy-out dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyDispatch {
    /// The leader chord itself: arm pending (with the configured
    /// follow-up window, in milliseconds) and swallow the key.
    LeaderArm(u64),
    /// A pending leader matched a `leader+X` binding.
    LeaderAction(usize),
    /// A pending leader had no match: the buffered leader key and this
    /// key both go to the PTY (ADR-0011 layer 1).
    LeaderMiss,
    /// A pending leader had no match while the pane is in copy mode:
    /// both keys are dropped instead, since copy mode forwards nothing
    /// to the PTY (Spec-0008 Key Tables).
    LeaderMissDrop,
    /// Copy mode owns the key; resolve it with `copy_keys::copy_key`.
    CopyMode,
    /// The active key table matched.
    TableAction(usize),
    /// Modal table, no match: the key is dropped, not forwarded.
    TableDrop,
    /// A default binding matched.
    Binding(usize),
    /// No layer claimed the key; forward to the PTY.
    Forward,
}

/// Resolve a keypress through ADR-0011's dispatch layers: pending
/// leader, then leader arm, then the modal layer (copy mode or an active
/// key table), then default bindings. Each layer tries the logical chord
/// first and falls back to the physical one (Spec-0011 keybind lookup).
pub(crate) fn resolve_key(
    ctx: &DispatchContext,
    logical: Option<&oakterm_config::KeyChord>,
    physical: Option<&oakterm_config::KeyChord>,
) -> KeyDispatch {
    let either = |f: &dyn Fn(&oakterm_config::KeyChord) -> Option<usize>| {
        logical.and_then(f).or_else(|| physical.and_then(f))
    };

    if ctx.leader_pending {
        let miss = if ctx.copy_mode {
            KeyDispatch::LeaderMissDrop
        } else {
            KeyDispatch::LeaderMiss
        };
        return either(&|c| ctx.registry.lookup_leader_index(c))
            .map_or(miss, KeyDispatch::LeaderAction);
    }
    if let Some(lk) = ctx.leader {
        if logical == Some(&lk.chord) || physical == Some(&lk.chord) {
            return KeyDispatch::LeaderArm(lk.timeout_ms);
        }
    }
    if ctx.copy_mode {
        return KeyDispatch::CopyMode;
    }
    if let Some(table) = ctx.table {
        return either(&|c| table.lookup_index(c))
            .map_or(KeyDispatch::TableDrop, KeyDispatch::TableAction);
    }
    either(&|c| ctx.registry.lookup_index(c)).map_or(KeyDispatch::Forward, KeyDispatch::Binding)
}

/// Encode winit modifier state to xterm mouse modifier bits.
/// Shift=4, Alt/Meta=8, Ctrl=16.
#[must_use]
pub(crate) fn encode_mouse_modifiers(mods: winit::event::Modifiers) -> u8 {
    let s = mods.state();
    let mut bits = 0u8;
    if s.shift_key() {
        bits |= 4;
    }
    if s.alt_key() {
        bits |= 8;
    }
    if s.control_key() {
        bits |= 16;
    }
    bits
}

/// Whether a mouse button event reaches the PTY, and the suppression
/// mask it leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MousePassthrough {
    pub(crate) forward: bool,
    pub(crate) suppressed: u8,
}

/// Route one mouse button event to the PTY or swallow it. `swallow_press`
/// is the caller's reason — a Shift bypass, copy mode — and it only ever
/// decides a press: the release pairs with its own press through the
/// mask, since a reason that appears or clears mid-click would otherwise
/// hand a mouse-mode application half a click.
pub(crate) fn button_passthrough(
    pressed: bool,
    swallow_press: bool,
    suppressed: u8,
    bit: u8,
) -> MousePassthrough {
    if pressed {
        return MousePassthrough {
            forward: !swallow_press,
            suppressed: if swallow_press {
                suppressed | bit
            } else {
                suppressed
            },
        };
    }
    MousePassthrough {
        forward: suppressed & bit == 0,
        suppressed: suppressed & !bit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_config::{KeyChord, KeyName, NamedKeyId};
    use winit::event::Modifiers;
    use winit::keyboard::ModifiersState;

    mod resolve {
        use super::super::{DispatchContext, KeyDispatch, resolve_key};
        use oakterm_config::{Action, KeyChord, KeyTable, KeybindRegistry, LeaderKey};

        fn chord(s: &str) -> KeyChord {
            KeyChord::parse(s).unwrap()
        }

        fn registry() -> KeybindRegistry {
            let mut reg = KeybindRegistry::new();
            reg.register(chord("ctrl+t"), Action::NewTab);
            reg.register_leader(chord("%"), Action::NewTab);
            reg
        }

        fn leader(s: &str) -> LeaderKey {
            LeaderKey::new(chord(s), 1000).unwrap()
        }

        fn ctx<'a>(
            reg: &'a KeybindRegistry,
            table: Option<&'a KeyTable>,
            leader: Option<&'a LeaderKey>,
            pending: bool,
        ) -> DispatchContext<'a> {
            DispatchContext {
                registry: reg,
                table,
                copy_mode: false,
                leader,
                leader_pending: pending,
            }
        }

        fn copy_ctx<'a>(
            reg: &'a KeybindRegistry,
            leader: Option<&'a LeaderKey>,
        ) -> DispatchContext<'a> {
            DispatchContext {
                registry: reg,
                table: None,
                copy_mode: true,
                leader,
                leader_pending: false,
            }
        }

        /// Copy mode owns every key, including ones the defaults bind:
        /// it is modal, and its table matches characters rather than
        /// chords, so the layer reports ownership and stops.
        #[test]
        fn copy_mode_claims_every_key_ahead_of_the_default_bindings() {
            let reg = registry();
            let c = copy_ctx(&reg, None);
            for chord_str in ["j", "ctrl+t", "escape"] {
                assert_eq!(
                    resolve_key(&c, Some(&chord(chord_str)), None),
                    KeyDispatch::CopyMode,
                    "{chord_str}"
                );
            }
            // A key that produces no chord at all still belongs to copy
            // mode; the caller reads the character off the event.
            assert_eq!(resolve_key(&c, None, None), KeyDispatch::CopyMode);
        }

        /// ADR-0011 puts the leader layer above the modal one, so a tmux
        /// convert keeps their prefix while reading scrollback.
        #[test]
        fn the_leader_still_arms_and_fires_inside_copy_mode() {
            let reg = registry();
            let leader = leader("ctrl+b");
            let c = copy_ctx(&reg, Some(&leader));
            assert_eq!(
                resolve_key(&c, Some(&chord("ctrl+b")), None),
                KeyDispatch::LeaderArm(1000)
            );

            let mut pending = copy_ctx(&reg, Some(&leader));
            pending.leader_pending = true;
            assert!(matches!(
                resolve_key(&pending, Some(&chord("%")), None),
                KeyDispatch::LeaderAction(_)
            ));
        }

        /// A leader miss outside copy mode sends both keys to the PTY.
        /// Inside it, copy mode forwards nothing, so both are dropped —
        /// otherwise a mistyped prefix types into the shell behind a
        /// modal reader.
        #[test]
        fn a_leader_miss_inside_copy_mode_drops_instead_of_forwarding() {
            let reg = registry();
            let leader = leader("ctrl+b");
            let mut pending = copy_ctx(&reg, Some(&leader));
            pending.leader_pending = true;

            assert_eq!(
                resolve_key(&pending, Some(&chord("q")), None),
                KeyDispatch::LeaderMissDrop
            );

            let outside = ctx(&reg, None, Some(&leader), true);
            assert_eq!(
                resolve_key(&outside, Some(&chord("q")), None),
                KeyDispatch::LeaderMiss
            );
        }

        /// Copy mode and a modal key table can both be live: copy mode is
        /// the pane's own mode, so it wins.
        #[test]
        fn copy_mode_outranks_an_active_key_table() {
            let reg = registry();
            let mut table = KeyTable::new();
            table.bind(chord("h"), Action::ScrollUp(1));
            let both = DispatchContext {
                registry: &reg,
                table: Some(&table),
                copy_mode: true,
                leader: None,
                leader_pending: false,
            };

            assert_eq!(
                resolve_key(&both, Some(&chord("h")), None),
                KeyDispatch::CopyMode,
                "the table binds h, but the pane is reading scrollback"
            );
        }

        #[test]
        fn default_binding_matches_and_misses_forward() {
            let reg = registry();
            let c = ctx(&reg, None, None, false);
            assert!(matches!(
                resolve_key(&c, Some(&chord("ctrl+t")), None),
                KeyDispatch::Binding(_)
            ));
            assert_eq!(
                resolve_key(&c, Some(&chord("ctrl+x")), None),
                KeyDispatch::Forward
            );
        }

        #[test]
        fn physical_fallback_reaches_every_layer() {
            let reg = registry();
            let c = ctx(&reg, None, None, false);
            // Logical misses, physical hits the default binding.
            assert!(matches!(
                resolve_key(&c, Some(&chord("ctrl+y")), Some(&chord("ctrl+t"))),
                KeyDispatch::Binding(_)
            ));
        }

        #[test]
        fn leader_chord_arms_and_pending_matches_the_leader_table() {
            let reg = registry();
            let leader = leader("ctrl+b");
            let c = ctx(&reg, None, Some(&leader), false);
            assert_eq!(
                resolve_key(&c, Some(&chord("ctrl+b")), None),
                KeyDispatch::LeaderArm(1000)
            );

            let pending = ctx(&reg, None, Some(&leader), true);
            assert!(matches!(
                resolve_key(&pending, Some(&chord("%")), None),
                KeyDispatch::LeaderAction(_)
            ));
            assert_eq!(
                resolve_key(&pending, Some(&chord("q")), None),
                KeyDispatch::LeaderMiss
            );
        }

        #[test]
        fn pending_leader_shadows_every_other_layer() {
            let reg = registry();
            let leader = leader("ctrl+b");
            let pending = ctx(&reg, None, Some(&leader), true);
            // ctrl+t is a default binding, but a pending leader owns it.
            assert_eq!(
                resolve_key(&pending, Some(&chord("ctrl+t")), None),
                KeyDispatch::LeaderMiss
            );
        }

        #[test]
        fn active_table_matches_or_drops_and_shadows_defaults() {
            let reg = registry();
            let mut table = KeyTable::new();
            table.bind(chord("h"), Action::ScrollUp(1));
            let c = ctx(&reg, Some(&table), None, false);
            assert!(matches!(
                resolve_key(&c, Some(&chord("h")), None),
                KeyDispatch::TableAction(_)
            ));
            // Modal: an unmatched key drops — even one bound in defaults.
            assert_eq!(
                resolve_key(&c, Some(&chord("ctrl+t")), None),
                KeyDispatch::TableDrop
            );
        }

        #[test]
        fn leader_arm_shadows_an_active_table() {
            // ADR-0011 orders the leader layer above the key table: the
            // leader chord arms even while a modal table is active.
            let reg = registry();
            let leader = leader("ctrl+b");
            let mut table = KeyTable::new();
            table.bind(chord("ctrl+b"), Action::ScrollUp(1));
            let c = ctx(&reg, Some(&table), Some(&leader), false);
            assert_eq!(
                resolve_key(&c, Some(&chord("ctrl+b")), None),
                KeyDispatch::LeaderArm(1000)
            );
            // And a pending leader shadows the table entirely.
            let pending = ctx(&reg, Some(&table), Some(&leader), true);
            assert!(matches!(
                resolve_key(&pending, Some(&chord("%")), None),
                KeyDispatch::LeaderAction(_)
            ));
        }

        #[test]
        fn physical_fallback_reaches_leader_and_table_layers() {
            let reg = registry();
            let leader = leader("ctrl+b");
            // Pending leader: logical misses, physical matches leader+%.
            let pending = ctx(&reg, None, Some(&leader), true);
            assert!(matches!(
                resolve_key(&pending, Some(&chord("q")), Some(&chord("%"))),
                KeyDispatch::LeaderAction(_)
            ));
            // Table: logical misses, physical matches the table bind.
            let mut table = KeyTable::new();
            table.bind(chord("h"), Action::ScrollUp(1));
            let modal = ctx(&reg, Some(&table), None, false);
            assert!(matches!(
                resolve_key(&modal, Some(&chord("q")), Some(&chord("h"))),
                KeyDispatch::TableAction(_)
            ));
        }

        #[test]
        fn no_chord_at_all_forwards_or_drops_per_layer() {
            let reg = registry();
            let c = ctx(&reg, None, None, false);
            assert_eq!(resolve_key(&c, None, None), KeyDispatch::Forward);
            let mut table = KeyTable::new();
            table.bind(chord("h"), Action::ScrollUp(1));
            let modal = ctx(&reg, Some(&table), None, false);
            assert_eq!(resolve_key(&modal, None, None), KeyDispatch::TableDrop);
        }
    }

    fn named(key: NamedKey) -> Key {
        Key::Named(key)
    }

    fn mods(state: ModifiersState) -> Modifiers {
        Modifiers::from(state)
    }

    #[test]
    fn text_takes_precedence_over_named_key() {
        assert_eq!(
            key_to_bytes(&named(NamedKey::ArrowUp), Some("a")),
            Some(b"a".to_vec())
        );
    }

    #[test]
    fn empty_text_falls_through_to_named_key() {
        assert_eq!(
            key_to_bytes(&named(NamedKey::ArrowUp), Some("")),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn character_key_uses_text_bytes() {
        assert_eq!(
            key_to_bytes(&Key::Character("é".into()), Some("é")),
            Some("é".as_bytes().to_vec())
        );
    }

    #[test]
    fn arrows_encode_csi_sequences() {
        let cases = [
            (NamedKey::ArrowUp, &b"\x1b[A"[..]),
            (NamedKey::ArrowDown, b"\x1b[B"),
            (NamedKey::ArrowRight, b"\x1b[C"),
            (NamedKey::ArrowLeft, b"\x1b[D"),
        ];
        for (key, expected) in cases {
            assert_eq!(key_to_bytes(&named(key), None), Some(expected.to_vec()));
        }
    }

    #[test]
    fn navigation_keys_encode_csi_sequences() {
        let cases = [
            (NamedKey::Home, &b"\x1b[H"[..]),
            (NamedKey::End, b"\x1b[F"),
            (NamedKey::Insert, b"\x1b[2~"),
            (NamedKey::Delete, b"\x1b[3~"),
            (NamedKey::PageUp, b"\x1b[5~"),
            (NamedKey::PageDown, b"\x1b[6~"),
        ];
        for (key, expected) in cases {
            assert_eq!(key_to_bytes(&named(key), None), Some(expected.to_vec()));
        }
    }

    #[test]
    fn editing_keys_encode_control_bytes() {
        assert_eq!(
            key_to_bytes(&named(NamedKey::Enter), None),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            key_to_bytes(&named(NamedKey::Backspace), None),
            Some(b"\x7f".to_vec())
        );
        assert_eq!(
            key_to_bytes(&named(NamedKey::Escape), None),
            Some(b"\x1b".to_vec())
        );
        assert_eq!(
            key_to_bytes(&named(NamedKey::Tab), None),
            Some(b"\t".to_vec())
        );
    }

    #[test]
    fn function_keys_encode_ss3_and_csi_sequences() {
        let cases = [
            (NamedKey::F1, &b"\x1bOP"[..]),
            (NamedKey::F2, b"\x1bOQ"),
            (NamedKey::F3, b"\x1bOR"),
            (NamedKey::F4, b"\x1bOS"),
            (NamedKey::F5, b"\x1b[15~"),
            (NamedKey::F6, b"\x1b[17~"),
            (NamedKey::F7, b"\x1b[18~"),
            (NamedKey::F8, b"\x1b[19~"),
            (NamedKey::F9, b"\x1b[20~"),
            (NamedKey::F10, b"\x1b[21~"),
            (NamedKey::F11, b"\x1b[23~"),
            (NamedKey::F12, b"\x1b[24~"),
        ];
        for (key, expected) in cases {
            assert_eq!(key_to_bytes(&named(key), None), Some(expected.to_vec()));
        }
    }

    #[test]
    fn unmapped_keys_produce_no_bytes() {
        assert_eq!(key_to_bytes(&named(NamedKey::CapsLock), None), None);
        assert_eq!(key_to_bytes(&named(NamedKey::Shift), None), None);
        assert_eq!(key_to_bytes(&Key::Character("a".into()), None), None);
    }

    #[test]
    fn chord_from_character_with_modifiers() {
        let chord = winit_to_chord(
            mods(ModifiersState::CONTROL | ModifiersState::SHIFT),
            &Key::Character("a".into()),
        );
        assert_eq!(
            chord,
            Some(KeyChord {
                ctrl: true,
                alt: false,
                shift: true,
                super_key: false,
                key: KeyName::Character('a'),
            })
        );
    }

    #[test]
    fn chord_lowercases_character_keys() {
        let chord = winit_to_chord(mods(ModifiersState::SHIFT), &Key::Character("A".into()));
        assert_eq!(chord.map(|c| c.key), Some(KeyName::Character('a')));
    }

    #[test]
    fn chord_from_named_key() {
        let chord = winit_to_chord(mods(ModifiersState::SUPER), &named(NamedKey::Enter));
        assert_eq!(
            chord,
            Some(KeyChord {
                ctrl: false,
                alt: false,
                shift: false,
                super_key: true,
                key: KeyName::Named(NamedKeyId::Enter),
            })
        );
    }

    #[test]
    fn chord_rejects_multi_char_and_unmapped_keys() {
        assert_eq!(
            winit_to_chord(mods(ModifiersState::empty()), &Key::Character("ab".into())),
            None
        );
        assert_eq!(
            winit_to_chord(mods(ModifiersState::empty()), &named(NamedKey::CapsLock)),
            None
        );
    }

    #[test]
    fn physical_chord_maps_digit_row_with_modifiers() {
        use oakterm_config::PhysicalKeyId;
        let chord = physical_to_chord(
            mods(ModifiersState::SUPER),
            PhysicalKey::Code(KeyCode::Digit3),
        );
        assert_eq!(
            chord,
            Some(KeyChord {
                ctrl: false,
                alt: false,
                shift: false,
                super_key: true,
                key: KeyName::Physical(PhysicalKeyId::Digit3),
            })
        );
    }

    #[test]
    fn physical_chord_excludes_the_numpad() {
        // The numpad is intentionally not positional: with NumLock off its
        // keys are navigation, so it resolves through the logical path.
        assert_eq!(
            physical_to_chord(
                mods(ModifiersState::SUPER),
                PhysicalKey::Code(KeyCode::Numpad1)
            ),
            None
        );
    }

    #[test]
    fn physical_chord_rejects_non_digit_and_unidentified_keys() {
        // Letters carry no positional binding — they stay logical.
        assert_eq!(
            physical_to_chord(
                mods(ModifiersState::empty()),
                PhysicalKey::Code(KeyCode::KeyA)
            ),
            None
        );
        assert_eq!(
            physical_to_chord(
                mods(ModifiersState::empty()),
                PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
            ),
            None
        );
    }

    #[test]
    fn mouse_modifier_bits_match_xterm_encoding() {
        assert_eq!(encode_mouse_modifiers(mods(ModifiersState::empty())), 0);
        assert_eq!(encode_mouse_modifiers(mods(ModifiersState::SHIFT)), 4);
        assert_eq!(encode_mouse_modifiers(mods(ModifiersState::ALT)), 8);
        assert_eq!(encode_mouse_modifiers(mods(ModifiersState::CONTROL)), 16);
        assert_eq!(
            encode_mouse_modifiers(mods(
                ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL
            )),
            28
        );
    }

    #[test]
    fn palette_named_keys_map_to_their_effects() {
        use PaletteKeyEffect as E;
        let none = ModifiersState::empty();
        for (key, expected) in [
            (NamedKey::Escape, E::Close),
            (NamedKey::Enter, E::Confirm),
            (NamedKey::ArrowUp, E::MoveUp),
            (NamedKey::ArrowDown, E::MoveDown),
            (NamedKey::Backspace, E::Backspace),
        ] {
            assert_eq!(palette_key_effect(&named(key), none, None), expected);
        }
        // Emacs-style aliases require Ctrl.
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(
            palette_key_effect(&Key::Character("p".into()), ctrl, None),
            E::MoveUp
        );
        assert_eq!(
            palette_key_effect(&Key::Character("n".into()), ctrl, None),
            E::MoveDown
        );
    }

    #[test]
    fn palette_text_input_is_gated_on_unmodified_keys() {
        use PaletteKeyEffect as E;
        let none = ModifiersState::empty();
        assert_eq!(
            palette_key_effect(&Key::Character("t".into()), none, Some("t")),
            E::Input("t".to_string())
        );
        // A chord's base character must not leak into the query: Cmd+P
        // (the palette's own bind), Ctrl+X, Alt+F all ignore.
        for state in [
            ModifiersState::SUPER,
            ModifiersState::CONTROL,
            ModifiersState::ALT,
        ] {
            assert_eq!(
                palette_key_effect(&Key::Character("p".into()), state, Some("p")),
                if state == ModifiersState::CONTROL {
                    E::MoveUp // the alias, not text input
                } else {
                    E::Ignore
                }
            );
        }
        // Control characters are stripped; nothing left means ignore.
        assert_eq!(
            palette_key_effect(&named(NamedKey::Tab), none, Some("\t")),
            E::Ignore
        );
        // Shift alone still types (capitals, punctuation).
        assert_eq!(
            palette_key_effect(
                &Key::Character("T".into()),
                ModifiersState::SHIFT,
                Some("T")
            ),
            E::Input("T".to_string())
        );
    }

    /// A swallowed press must not deliver its release to a mouse-mode
    /// application either: press and release pair by the mask, so a
    /// reason that appears or clears with the button held cannot produce
    /// half a click. Both reasons share the mask, so both are checked.
    #[test]
    fn a_click_pairs_its_press_and_release_across_a_reason_change() {
        const LEFT: u8 = 1;

        // Forwarded press, reason appears before the release (copy mode
        // entered, or Shift taken up): the release must still go.
        let press = button_passthrough(true, false, 0, LEFT);
        assert!(press.forward);
        let release = button_passthrough(false, true, press.suppressed, LEFT);
        assert!(release.forward, "orphaned press without this");

        // Swallowed press, reason gone by the release (copy mode left,
        // or Shift released): the release is swallowed with its press.
        let press = button_passthrough(true, true, 0, LEFT);
        assert!(!press.forward);
        assert_eq!(press.suppressed, LEFT);
        let release = button_passthrough(false, false, press.suppressed, LEFT);
        assert!(!release.forward, "release without a press it belongs to");
        assert_eq!(release.suppressed, 0, "the bit clears with the release");
    }

    /// The Shift bypass and copy mode are the same decision to this
    /// layer: one mask, one rule, whichever reason set the bit.
    #[test]
    fn both_suppression_reasons_share_one_mask() {
        const LEFT: u8 = 1;
        const MIDDLE: u8 = 2;

        // Shift swallows a left press; copy mode swallows a middle one.
        let shift_press = button_passthrough(true, true, 0, LEFT);
        let copy_press = button_passthrough(true, true, shift_press.suppressed, MIDDLE);
        assert_eq!(copy_press.suppressed, LEFT | MIDDLE);

        // Each release clears only its own bit, and neither forwards.
        let left_up = button_passthrough(false, false, copy_press.suppressed, LEFT);
        assert!(!left_up.forward);
        assert_eq!(left_up.suppressed, MIDDLE);
        let middle_up = button_passthrough(false, false, left_up.suppressed, MIDDLE);
        assert!(!middle_up.forward);
        assert_eq!(middle_up.suppressed, 0);
    }

    /// Suppression is per button: a swallowed middle click must not
    /// swallow a left release that was never suppressed.
    #[test]
    fn button_suppression_does_not_leak_between_buttons() {
        const LEFT: u8 = 1;
        const MIDDLE: u8 = 2;

        let middle = button_passthrough(true, true, 0, MIDDLE);
        let left_release = button_passthrough(false, true, middle.suppressed, LEFT);

        assert!(left_release.forward);
        assert_eq!(left_release.suppressed, MIDDLE, "the middle bit survives");
    }
}
