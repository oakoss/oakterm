//! The copy-mode key table (Spec-0008 vim preset): a key press in, a
//! copy-mode command out.
//!
//! Copy mode resolves against the character the layout produced rather
//! than a `KeyChord`, unlike every other dispatch layer. `$` is Shift+4
//! on a US keyboard and lives elsewhere on others; vim binds the
//! character, so a chord built from the unmodified key would bind the
//! wrong thing everywhere but one layout.

use crate::copy_mode::CopySelectionType;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// A key press as copy mode sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyKey {
    /// The character the layout produced, no Ctrl/Alt/Super held.
    Char(char),
    /// Ctrl plus a base character, always lowercase; `copy_key` folds
    /// case, and `ctrl_key` matches lowercase only.
    Ctrl(char),
    Escape,
}

/// What one key press is to copy mode before the table sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyPress {
    /// A key the preset can name.
    Key(CopyKey),
    /// A modifier held on its own. It belongs to the chord being built,
    /// not to the sequence in progress, so a pending prefix survives it.
    ModifierOnly,
    /// A Super chord, an IME composition: nothing copy mode binds. It
    /// still interrupts a pending sequence, as any other key would.
    Unnameable,
}

/// Cursor motions the table can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Motion {
    Left,
    Down,
    Up,
    Right,
    WordForward,
    WordBackward,
    WordEnd,
    LineStart,
    LineEnd,
    FirstNonBlank,
    Top,
    Bottom,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
}

/// What a bound copy-mode key asks the GUI to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyCommand {
    Move(Motion),
    ToggleSelection(CopySelectionType),
    /// Yank the selection and leave copy mode. A no-op without one.
    Yank,
    /// Escape: drop the selection if there is one, otherwise exit.
    ClearOrExit,
    Exit,
    /// `/`, `?`, `n`, `N`. Bound so they cannot fall through to another
    /// meaning, inert until the search overlay lands (TREK-114).
    Search,
}

/// The first key of a multi-key sequence, held until the rest arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingPrefix {
    /// `g`, awaiting the second `g` of `gg`.
    G,
}

/// What the table does with one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VimKey {
    /// Consume the key and wait for the rest of the sequence.
    Pending(PendingPrefix),
    Run(CopyCommand),
    /// Unbound. Copy mode is modal, so the key is dropped, never
    /// forwarded to the PTY (Spec-0008 Key Tables).
    Drop,
}

/// Classify a key press for copy mode.
///
/// `composed` is the layout's character (`V` for Shift+V, `$` for
/// Shift+4); `base` is the same key with modifiers stripped, which is
/// what a Ctrl chord must match on since Ctrl+D composes to a control
/// character rather than to `d`. `None` means nothing copy mode binds.
pub(crate) fn copy_key(mods: ModifiersState, composed: &Key, base: &Key) -> Option<CopyKey> {
    if matches!(composed, Key::Named(NamedKey::Escape)) {
        return Some(CopyKey::Escape);
    }
    if mods.super_key() || mods.alt_key() {
        return None;
    }
    if mods.control_key() {
        return single_char(base).map(|c| CopyKey::Ctrl(c.to_ascii_lowercase()));
    }
    single_char(composed).map(CopyKey::Char)
}

/// Classify a key press, separating a bare modifier keydown from a key
/// copy mode simply does not bind — the two differ in what they do to a
/// sequence in progress.
pub(crate) fn copy_press(mods: ModifiersState, composed: &Key, base: &Key) -> CopyPress {
    if is_modifier(composed) {
        return CopyPress::ModifierOnly;
    }
    copy_key(mods, composed, base).map_or(CopyPress::Unnameable, CopyPress::Key)
}

/// Whether the key is a modifier pressed on its own.
fn is_modifier(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::CapsLock
                | NamedKey::Control
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Hyper
                | NamedKey::Meta
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Shift
                | NamedKey::Super
                | NamedKey::Symbol
                | NamedKey::SymbolLock
        )
    )
}

/// Advance the pending-prefix state machine by one press: the prefix to
/// hold afterwards, and the command to run if the press completed one.
pub(crate) fn advance_copy_key(
    pending: Option<PendingPrefix>,
    press: CopyPress,
) -> (Option<PendingPrefix>, Option<CopyCommand>) {
    match press {
        CopyPress::ModifierOnly => (pending, None),
        CopyPress::Unnameable => (None, None),
        CopyPress::Key(key) => match vim_key(pending, key) {
            VimKey::Pending(prefix) => (Some(prefix), None),
            VimKey::Run(command) => (None, Some(command)),
            VimKey::Drop => (None, None),
        },
    }
}

/// The character of a single-character key, or `None` for named keys and
/// multi-character IME composition.
fn single_char(key: &Key) -> Option<char> {
    let Key::Character(text) = key else {
        return None;
    };
    let mut chars = text.chars();
    let first = chars.next()?;
    chars.next().is_none().then_some(first)
}

/// Resolve one key against the vim preset. `pending` is the prefix an
/// earlier key armed; the caller stores whatever this returns.
///
/// A pending `g` matches only `g`. Vim discards an unrecognized
/// `g`-sequence rather than re-reading its second key as a command on its
/// own, so `gj` moves nothing.
pub(crate) fn vim_key(pending: Option<PendingPrefix>, key: CopyKey) -> VimKey {
    if pending == Some(PendingPrefix::G) {
        return if key == CopyKey::Char('g') {
            VimKey::Run(CopyCommand::Move(Motion::Top))
        } else {
            VimKey::Drop
        };
    }
    match key {
        CopyKey::Char(c) => char_key(c),
        CopyKey::Ctrl(c) => ctrl_key(c),
        CopyKey::Escape => VimKey::Run(CopyCommand::ClearOrExit),
    }
}

fn char_key(c: char) -> VimKey {
    use CopyCommand::{Exit, Move, Search, ToggleSelection, Yank};
    match c {
        'h' => VimKey::Run(Move(Motion::Left)),
        'j' => VimKey::Run(Move(Motion::Down)),
        'k' => VimKey::Run(Move(Motion::Up)),
        'l' => VimKey::Run(Move(Motion::Right)),
        'w' => VimKey::Run(Move(Motion::WordForward)),
        'b' => VimKey::Run(Move(Motion::WordBackward)),
        'e' => VimKey::Run(Move(Motion::WordEnd)),
        '0' => VimKey::Run(Move(Motion::LineStart)),
        '$' => VimKey::Run(Move(Motion::LineEnd)),
        '^' => VimKey::Run(Move(Motion::FirstNonBlank)),
        'g' => VimKey::Pending(PendingPrefix::G),
        'G' => VimKey::Run(Move(Motion::Bottom)),
        'v' => VimKey::Run(ToggleSelection(CopySelectionType::Character)),
        'V' => VimKey::Run(ToggleSelection(CopySelectionType::Line)),
        'y' => VimKey::Run(Yank),
        'q' => VimKey::Run(Exit),
        '/' | '?' | 'n' | 'N' => VimKey::Run(Search),
        _ => VimKey::Drop,
    }
}

fn ctrl_key(c: char) -> VimKey {
    use CopyCommand::{Move, ToggleSelection};
    match c {
        'd' => VimKey::Run(Move(Motion::HalfPageDown)),
        'u' => VimKey::Run(Move(Motion::HalfPageUp)),
        'f' => VimKey::Run(Move(Motion::PageDown)),
        'b' => VimKey::Run(Move(Motion::PageUp)),
        'v' => VimKey::Run(ToggleSelection(CopySelectionType::Block)),
        _ => VimKey::Drop,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CopyCommand, CopyKey, CopyPress, Motion, PendingPrefix, VimKey, advance_copy_key, copy_key,
        copy_press, vim_key,
    };
    use crate::copy_mode::CopySelectionType;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    fn ch(c: char) -> Key {
        Key::Character(c.to_string().into())
    }

    fn run(key: CopyKey) -> VimKey {
        vim_key(None, key)
    }

    #[test]
    fn the_vim_preset_binds_every_row_of_the_spec_table() {
        use CopyCommand::{Move, Search, ToggleSelection, Yank};
        let cases = [
            (CopyKey::Char('h'), Move(Motion::Left)),
            (CopyKey::Char('j'), Move(Motion::Down)),
            (CopyKey::Char('k'), Move(Motion::Up)),
            (CopyKey::Char('l'), Move(Motion::Right)),
            (CopyKey::Char('w'), Move(Motion::WordForward)),
            (CopyKey::Char('b'), Move(Motion::WordBackward)),
            (CopyKey::Char('e'), Move(Motion::WordEnd)),
            (CopyKey::Char('0'), Move(Motion::LineStart)),
            (CopyKey::Char('$'), Move(Motion::LineEnd)),
            (CopyKey::Char('^'), Move(Motion::FirstNonBlank)),
            (CopyKey::Char('G'), Move(Motion::Bottom)),
            (CopyKey::Ctrl('d'), Move(Motion::HalfPageDown)),
            (CopyKey::Ctrl('u'), Move(Motion::HalfPageUp)),
            (CopyKey::Ctrl('f'), Move(Motion::PageDown)),
            (CopyKey::Ctrl('b'), Move(Motion::PageUp)),
            (
                CopyKey::Char('v'),
                ToggleSelection(CopySelectionType::Character),
            ),
            (CopyKey::Char('V'), ToggleSelection(CopySelectionType::Line)),
            (
                CopyKey::Ctrl('v'),
                ToggleSelection(CopySelectionType::Block),
            ),
            (CopyKey::Char('y'), Yank),
            (CopyKey::Char('q'), CopyCommand::Exit),
            (CopyKey::Escape, CopyCommand::ClearOrExit),
            (CopyKey::Char('/'), Search),
            (CopyKey::Char('?'), Search),
            (CopyKey::Char('n'), Search),
            (CopyKey::Char('N'), Search),
        ];
        for (key, expected) in cases {
            assert_eq!(run(key), VimKey::Run(expected), "{key:?}");
        }
    }

    /// Case matters: the preset binds `v` and `V` to different selection
    /// shapes, so a table that folded case would make line selection
    /// unreachable.
    #[test]
    fn shifted_letters_are_distinct_bindings() {
        assert_ne!(run(CopyKey::Char('v')), run(CopyKey::Char('V')));
        assert_eq!(run(CopyKey::Char('g')), VimKey::Pending(PendingPrefix::G));
        assert_eq!(
            run(CopyKey::Char('G')),
            VimKey::Run(CopyCommand::Move(Motion::Bottom))
        );
    }

    /// Copy mode is modal: an unbound key is consumed, never forwarded.
    #[test]
    fn unbound_keys_drop() {
        for key in [
            CopyKey::Char('z'),
            CopyKey::Char('!'),
            CopyKey::Ctrl('c'),
            CopyKey::Ctrl('a'),
        ] {
            assert_eq!(run(key), VimKey::Drop, "{key:?}");
        }
    }

    #[test]
    fn gg_takes_two_presses_to_reach_the_top() {
        assert_eq!(
            vim_key(None, CopyKey::Char('g')),
            VimKey::Pending(PendingPrefix::G)
        );
        assert_eq!(
            vim_key(Some(PendingPrefix::G), CopyKey::Char('g')),
            VimKey::Run(CopyCommand::Move(Motion::Top))
        );
    }

    /// A `g` sequence interrupted by anything else discards both keys.
    /// Re-reading the second key as its own command would turn a mistyped
    /// `gy` into a yank, and `gq` into an exit.
    #[test]
    fn an_interrupted_g_sequence_swallows_the_second_key() {
        for key in [
            CopyKey::Char('j'),
            CopyKey::Char('y'),
            CopyKey::Char('q'),
            CopyKey::Char('G'),
            CopyKey::Ctrl('d'),
            CopyKey::Escape,
        ] {
            assert_eq!(
                vim_key(Some(PendingPrefix::G), key),
                VimKey::Drop,
                "{key:?} must not run its own command after g"
            );
        }
    }

    /// Two `g` presses in a row that are not a pair: the caller clears
    /// the prefix on a `Drop`, so `g` `j` `g` re-arms rather than firing.
    #[test]
    fn a_dropped_sequence_leaves_the_next_g_free_to_arm_again() {
        let mut pending = None;
        for (key, expected) in [
            (CopyKey::Char('g'), VimKey::Pending(PendingPrefix::G)),
            (CopyKey::Char('j'), VimKey::Drop),
            (CopyKey::Char('g'), VimKey::Pending(PendingPrefix::G)),
            (
                CopyKey::Char('g'),
                VimKey::Run(CopyCommand::Move(Motion::Top)),
            ),
        ] {
            let outcome = vim_key(pending, key);
            assert_eq!(outcome, expected, "{key:?}");
            pending = match outcome {
                VimKey::Pending(p) => Some(p),
                _ => None,
            };
        }
    }

    // --- The prefix state machine the GUI drives ---

    fn key(c: char) -> CopyPress {
        CopyPress::Key(CopyKey::Char(c))
    }

    /// The four transitions the GUI's key handler drives: arm a prefix,
    /// fire the sequence, and the two ways one gets interrupted.
    #[test]
    fn advancing_the_prefix_arms_fires_and_interrupts() {
        assert_eq!(
            advance_copy_key(None, key('g')),
            (Some(PendingPrefix::G), None),
            "arm"
        );
        assert_eq!(
            advance_copy_key(Some(PendingPrefix::G), key('g')),
            (None, Some(CopyCommand::Move(Motion::Top))),
            "fire"
        );
        assert_eq!(
            advance_copy_key(Some(PendingPrefix::G), key('j')),
            (None, None),
            "a bound key interrupts without running"
        );
        assert_eq!(
            advance_copy_key(Some(PendingPrefix::G), CopyPress::Unnameable),
            (None, None),
            "a key the preset cannot name interrupts too"
        );
    }

    /// A bare modifier keydown arrives between the two keys of `gg` on
    /// any capitalized sequence; treating it as a key would leave `g`
    /// unable to reach anything shifted.
    #[test]
    fn a_bare_modifier_press_leaves_a_pending_prefix_armed() {
        let shift = Key::Named(NamedKey::Shift);
        let press = copy_press(ModifiersState::SHIFT, &shift, &shift);
        assert_eq!(press, CopyPress::ModifierOnly);

        let (pending, command) = advance_copy_key(None, key('g'));
        assert_eq!((pending, command), (Some(PendingPrefix::G), None));

        let (pending, command) = advance_copy_key(pending, press);
        assert_eq!(
            (pending, command),
            (Some(PendingPrefix::G), None),
            "the modifier is not a key of the sequence"
        );

        let (pending, command) = advance_copy_key(pending, key('g'));
        assert_eq!(
            (pending, command),
            (None, Some(CopyCommand::Move(Motion::Top)))
        );
    }

    /// Only modifiers get that treatment: an ordinary named key is
    /// unnameable to the preset and interrupts.
    #[test]
    fn a_named_key_is_unnameable_rather_than_a_modifier() {
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(
            copy_press(ModifiersState::empty(), &tab, &tab),
            CopyPress::Unnameable
        );
        let sup = Key::Character("t".into());
        assert_eq!(
            copy_press(ModifiersState::SUPER, &sup, &sup),
            CopyPress::Unnameable
        );
    }

    #[test]
    fn a_composed_character_binds_itself_rather_than_its_unshifted_key() {
        // Shift+4 on a US layout: the base key is '4', the character '$'.
        assert_eq!(
            copy_key(ModifiersState::SHIFT, &ch('$'), &ch('4')),
            Some(CopyKey::Char('$'))
        );
        assert_eq!(
            copy_key(ModifiersState::SHIFT, &ch('V'), &ch('v')),
            Some(CopyKey::Char('V'))
        );
    }

    /// Ctrl composes to a control character, so a Ctrl chord has to read
    /// the base key. Shift folds away: Ctrl+Shift+V is still block.
    #[test]
    fn ctrl_chords_read_the_base_key_and_ignore_shift() {
        assert_eq!(
            copy_key(
                ModifiersState::CONTROL,
                &Key::Character("\u{4}".into()),
                &ch('d')
            ),
            Some(CopyKey::Ctrl('d'))
        );
        assert_eq!(
            copy_key(
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                &ch('V'),
                &ch('V')
            ),
            Some(CopyKey::Ctrl('v'))
        );
    }

    /// Escape arrives as a named key and must resolve whatever modifiers
    /// are held — it is the way out of copy mode.
    #[test]
    fn escape_resolves_under_any_modifier() {
        let esc = Key::Named(NamedKey::Escape);
        for mods in [
            ModifiersState::empty(),
            ModifiersState::SHIFT,
            ModifiersState::CONTROL,
            ModifiersState::SUPER,
            ModifiersState::ALT,
        ] {
            assert_eq!(copy_key(mods, &esc, &esc), Some(CopyKey::Escape));
        }
    }

    /// Super and Alt chords resolve to nothing, so the caller drops them
    /// rather than reading `Cmd+T` as a bare `t`.
    #[test]
    fn super_and_alt_chords_resolve_to_no_copy_key() {
        assert_eq!(copy_key(ModifiersState::SUPER, &ch('t'), &ch('t')), None);
        assert_eq!(copy_key(ModifiersState::ALT, &ch('f'), &ch('f')), None);
    }

    /// Named keys other than Escape and multi-character IME composition
    /// bind nothing in the vim preset.
    #[test]
    fn named_keys_and_ime_composition_resolve_to_no_copy_key() {
        let none = ModifiersState::empty();
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(copy_key(none, &tab, &tab), None);
        let ime = Key::Character("ab".into());
        assert_eq!(copy_key(none, &ime, &ime), None);
    }
}
