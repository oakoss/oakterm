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

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_config::{KeyChord, KeyName, NamedKeyId};
    use winit::event::Modifiers;
    use winit::keyboard::ModifiersState;

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
}
