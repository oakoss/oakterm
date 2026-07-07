//! Translates winit keyboard and mouse input into PTY bytes, keybind
//! chords, and mouse modifier bits.

use winit::keyboard::{Key, NamedKey};

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
}
