//! The copy-mode key tables (Spec-0008 vim, emacs, and basic presets):
//! a key press in, a copy-mode command out.
//!
//! Copy mode resolves against the character the layout produced rather
//! than a `KeyChord`, unlike every other dispatch layer. `$` is Shift+4
//! on a US keyboard and lives elsewhere on others; vim binds the
//! character, so a chord built from the unmodified key would bind the
//! wrong thing everywhere but one layout.

use crate::copy_mode::{CopySelectionType, SelectionEffect};
use oakterm_config::CopyModePreset;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// A key press as copy mode sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyKey {
    /// The character the layout produced, no Ctrl/Alt/Super held.
    Char(char),
    /// Ctrl plus a base character, always lowercase; `copy_key` folds
    /// case, and the tables match lowercase only.
    Ctrl(char),
    /// Alt plus a character, case-folded: the composed character when it
    /// is bindable, else the base key — macOS Option composes `ƒ` from
    /// Alt+f, hiding the letter. `shift` lets a table catch `Alt+Shift+,`
    /// as `Alt+<` when composition hid the shifted pair too; tables match
    /// `..` on letters (shift-insensitive) and bind `shift: true` only
    /// for that composition-recovery case.
    Alt {
        ch: char,
        shift: bool,
    },
    /// A navigation key with its shift state; the basic preset binds
    /// these, the others drop them.
    Named {
        key: NamedCopyKey,
        shift: bool,
    },
    Escape,
}

/// Navigation keys copy mode can classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamedCopyKey {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
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
    /// The selection effect rides on the motion so each table owns its
    /// selection policy: vim and emacs keep, basic extends on shifted
    /// navigation and clears on unshifted.
    Move {
        motion: Motion,
        selection: SelectionEffect,
    },
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
    if mods.super_key() {
        return None;
    }
    let shift = mods.shift_key();
    if mods.alt_key() {
        if mods.control_key() {
            return None;
        }
        return alt_char(composed, base).map(|ch| CopyKey::Alt { ch, shift });
    }
    if mods.control_key() {
        return single_char(base).map(|c| CopyKey::Ctrl(c.to_ascii_lowercase()));
    }
    if let Some(key) = named_copy_key(composed) {
        return Some(CopyKey::Named { key, shift });
    }
    single_char(composed).map(CopyKey::Char)
}

/// The character an Alt chord binds: the composed character when the
/// layout produced a plain ASCII one, else the base key. macOS Option
/// composes symbols (`ƒ` from Alt+f), so the base is the only usable
/// name there; everywhere else the composed character carries shifted
/// pairs like `<` that the base key cannot.
fn alt_char(composed: &Key, base: &Key) -> Option<char> {
    single_char(composed)
        .filter(char::is_ascii_graphic)
        .or_else(|| single_char(base))
        .map(|c| c.to_ascii_lowercase())
}

fn named_copy_key(key: &Key) -> Option<NamedCopyKey> {
    let Key::Named(named) = key else {
        return None;
    };
    match named {
        NamedKey::ArrowUp => Some(NamedCopyKey::Up),
        NamedKey::ArrowDown => Some(NamedCopyKey::Down),
        NamedKey::ArrowLeft => Some(NamedCopyKey::Left),
        NamedKey::ArrowRight => Some(NamedCopyKey::Right),
        NamedKey::PageUp => Some(NamedCopyKey::PageUp),
        NamedKey::PageDown => Some(NamedCopyKey::PageDown),
        NamedKey::Home => Some(NamedCopyKey::Home),
        NamedKey::End => Some(NamedCopyKey::End),
        _ => None,
    }
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

/// Movement that leaves the selection alone — every vim and emacs row.
fn mv(motion: Motion) -> CopyCommand {
    CopyCommand::Move {
        motion,
        selection: SelectionEffect::Keep,
    }
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
/// Only the vim preset has multi-key sequences; the other tables resolve
/// every press on its own and clear any prefix a preset switch left.
pub(crate) fn advance_copy_key(
    preset: CopyModePreset,
    pending: Option<PendingPrefix>,
    press: CopyPress,
) -> (Option<PendingPrefix>, Option<CopyCommand>) {
    match press {
        CopyPress::ModifierOnly => (pending, None),
        CopyPress::Unnameable => (None, None),
        CopyPress::Key(key) => match preset {
            CopyModePreset::Vim => match vim_key(pending, key) {
                VimKey::Pending(prefix) => (Some(prefix), None),
                VimKey::Run(command) => (None, Some(command)),
                VimKey::Drop => (None, None),
            },
            CopyModePreset::Emacs => (None, emacs_key(key)),
            CopyModePreset::Basic => (None, basic_key(key)),
        },
    }
}

/// The character of a single-character key, or `None` for named keys and
/// multi-character IME composition. Space is a named key in winit but a
/// character to the tables — emacs binds Ctrl+Space.
fn single_char(key: &Key) -> Option<char> {
    let Key::Character(text) = key else {
        return matches!(key, Key::Named(NamedKey::Space)).then_some(' ');
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
            VimKey::Run(mv(Motion::Top))
        } else {
            VimKey::Drop
        };
    }
    match key {
        CopyKey::Char(c) => char_key(c),
        CopyKey::Ctrl(c) => ctrl_key(c),
        CopyKey::Escape => VimKey::Run(CopyCommand::ClearOrExit),
        CopyKey::Alt { .. } | CopyKey::Named { .. } => VimKey::Drop,
    }
}

/// Resolve one key against the emacs preset (Spec-0008). `None` drops
/// the key; copy mode is modal in every preset.
pub(crate) fn emacs_key(key: CopyKey) -> Option<CopyCommand> {
    use CopyCommand::{ClearOrExit, Exit, Search, ToggleSelection, Yank};
    match key {
        CopyKey::Ctrl('n') => Some(mv(Motion::Down)),
        CopyKey::Ctrl('p') => Some(mv(Motion::Up)),
        CopyKey::Ctrl('f') => Some(mv(Motion::Right)),
        CopyKey::Ctrl('b') => Some(mv(Motion::Left)),
        CopyKey::Ctrl('a') => Some(mv(Motion::LineStart)),
        CopyKey::Ctrl('e') => Some(mv(Motion::LineEnd)),
        CopyKey::Ctrl('v') => Some(mv(Motion::PageDown)),
        CopyKey::Ctrl(' ') => Some(ToggleSelection(CopySelectionType::Character)),
        CopyKey::Ctrl('g') => Some(Exit),
        CopyKey::Ctrl('s' | 'r') => Some(Search),
        CopyKey::Alt { ch: 'f', .. } => Some(mv(Motion::WordEnd)),
        CopyKey::Alt { ch: 'b', .. } => Some(mv(Motion::WordBackward)),
        CopyKey::Alt { ch: 'v', .. } => Some(mv(Motion::PageUp)),
        CopyKey::Alt { ch: 'w', .. } => Some(Yank),
        // `Alt+Shift+,` and `Alt+Shift+.` are `M-<`/`M->` on layouts
        // where Option composition hid the shifted character.
        CopyKey::Alt { ch: '<', .. }
        | CopyKey::Alt {
            ch: ',',
            shift: true,
        } => Some(mv(Motion::Top)),
        CopyKey::Alt { ch: '>', .. }
        | CopyKey::Alt {
            ch: '.',
            shift: true,
        } => Some(mv(Motion::Bottom)),
        CopyKey::Escape => Some(ClearOrExit),
        _ => None,
    }
}

/// Resolve one key against the basic preset (Spec-0008): shifted
/// navigation extends the selection, unshifted navigation clears it.
pub(crate) fn basic_key(key: CopyKey) -> Option<CopyCommand> {
    use CopyCommand::{ClearOrExit, Move, Search, Yank};
    match key {
        CopyKey::Named { key, shift } => {
            let motion = match key {
                NamedCopyKey::Up => Motion::Up,
                NamedCopyKey::Down => Motion::Down,
                NamedCopyKey::Left => Motion::Left,
                NamedCopyKey::Right => Motion::Right,
                NamedCopyKey::PageUp => Motion::PageUp,
                NamedCopyKey::PageDown => Motion::PageDown,
                NamedCopyKey::Home => Motion::Top,
                NamedCopyKey::End => Motion::Bottom,
            };
            let selection = if shift {
                SelectionEffect::Extend
            } else {
                SelectionEffect::Clear
            };
            Some(Move { motion, selection })
        }
        CopyKey::Ctrl('c') => Some(Yank),
        CopyKey::Ctrl('f') => Some(Search),
        // Clear-then-exit: no toggle key exists here, so Escape is the
        // only cancel a Shift+arrow selection has.
        CopyKey::Escape => Some(ClearOrExit),
        _ => None,
    }
}

fn char_key(c: char) -> VimKey {
    use CopyCommand::{Exit, Search, ToggleSelection, Yank};
    match c {
        'h' => VimKey::Run(mv(Motion::Left)),
        'j' => VimKey::Run(mv(Motion::Down)),
        'k' => VimKey::Run(mv(Motion::Up)),
        'l' => VimKey::Run(mv(Motion::Right)),
        'w' => VimKey::Run(mv(Motion::WordForward)),
        'b' => VimKey::Run(mv(Motion::WordBackward)),
        'e' => VimKey::Run(mv(Motion::WordEnd)),
        '0' => VimKey::Run(mv(Motion::LineStart)),
        '$' => VimKey::Run(mv(Motion::LineEnd)),
        '^' => VimKey::Run(mv(Motion::FirstNonBlank)),
        'g' => VimKey::Pending(PendingPrefix::G),
        'G' => VimKey::Run(mv(Motion::Bottom)),
        'v' => VimKey::Run(ToggleSelection(CopySelectionType::Character)),
        'V' => VimKey::Run(ToggleSelection(CopySelectionType::Line)),
        'y' => VimKey::Run(Yank),
        'q' => VimKey::Run(Exit),
        '/' | '?' | 'n' | 'N' => VimKey::Run(Search),
        _ => VimKey::Drop,
    }
}

fn ctrl_key(c: char) -> VimKey {
    use CopyCommand::ToggleSelection;
    match c {
        'd' => VimKey::Run(mv(Motion::HalfPageDown)),
        'u' => VimKey::Run(mv(Motion::HalfPageUp)),
        'f' => VimKey::Run(mv(Motion::PageDown)),
        'b' => VimKey::Run(mv(Motion::PageUp)),
        'v' => VimKey::Run(ToggleSelection(CopySelectionType::Block)),
        _ => VimKey::Drop,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CopyCommand, CopyKey, CopyPress, Motion, NamedCopyKey, PendingPrefix, VimKey,
        advance_copy_key, basic_key, copy_key, copy_press, emacs_key, mv, vim_key,
    };
    use crate::copy_mode::{CopySelectionType, SelectionEffect};
    use oakterm_config::CopyModePreset;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    fn ch(c: char) -> Key {
        Key::Character(c.to_string().into())
    }

    fn run(key: CopyKey) -> VimKey {
        vim_key(None, key)
    }

    #[test]
    fn the_vim_preset_binds_every_row_of_the_spec_table() {
        use CopyCommand::{Search, ToggleSelection, Yank};
        let cases = [
            (CopyKey::Char('h'), mv(Motion::Left)),
            (CopyKey::Char('j'), mv(Motion::Down)),
            (CopyKey::Char('k'), mv(Motion::Up)),
            (CopyKey::Char('l'), mv(Motion::Right)),
            (CopyKey::Char('w'), mv(Motion::WordForward)),
            (CopyKey::Char('b'), mv(Motion::WordBackward)),
            (CopyKey::Char('e'), mv(Motion::WordEnd)),
            (CopyKey::Char('0'), mv(Motion::LineStart)),
            (CopyKey::Char('$'), mv(Motion::LineEnd)),
            (CopyKey::Char('^'), mv(Motion::FirstNonBlank)),
            (CopyKey::Char('G'), mv(Motion::Bottom)),
            (CopyKey::Ctrl('d'), mv(Motion::HalfPageDown)),
            (CopyKey::Ctrl('u'), mv(Motion::HalfPageUp)),
            (CopyKey::Ctrl('f'), mv(Motion::PageDown)),
            (CopyKey::Ctrl('b'), mv(Motion::PageUp)),
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
        assert_eq!(run(CopyKey::Char('G')), VimKey::Run(mv(Motion::Bottom)));
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
            VimKey::Run(mv(Motion::Top))
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
            (CopyKey::Char('g'), VimKey::Run(mv(Motion::Top))),
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
        let vim = CopyModePreset::Vim;
        assert_eq!(
            advance_copy_key(vim, None, key('g')),
            (Some(PendingPrefix::G), None),
            "arm"
        );
        assert_eq!(
            advance_copy_key(vim, Some(PendingPrefix::G), key('g')),
            (None, Some(mv(Motion::Top))),
            "fire"
        );
        assert_eq!(
            advance_copy_key(vim, Some(PendingPrefix::G), key('j')),
            (None, None),
            "a bound key interrupts without running"
        );
        assert_eq!(
            advance_copy_key(vim, Some(PendingPrefix::G), CopyPress::Unnameable),
            (None, None),
            "a key the preset cannot name interrupts too"
        );
    }

    /// A bare modifier keydown arrives between the two keys of `gg` on
    /// any capitalized sequence; treating it as a key would leave `g`
    /// unable to reach anything shifted.
    #[test]
    fn a_bare_modifier_press_leaves_a_pending_prefix_armed() {
        let vim = CopyModePreset::Vim;
        let shift = Key::Named(NamedKey::Shift);
        let press = copy_press(ModifiersState::SHIFT, &shift, &shift);
        assert_eq!(press, CopyPress::ModifierOnly);

        let (pending, command) = advance_copy_key(vim, None, key('g'));
        assert_eq!((pending, command), (Some(PendingPrefix::G), None));

        let (pending, command) = advance_copy_key(vim, pending, press);
        assert_eq!(
            (pending, command),
            (Some(PendingPrefix::G), None),
            "the modifier is not a key of the sequence"
        );

        let (pending, command) = advance_copy_key(vim, pending, key('g'));
        assert_eq!((pending, command), (None, Some(mv(Motion::Top))));
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

    /// Super chords resolve to nothing, so the caller drops them rather
    /// than reading `Cmd+T` as a bare `t`. Ctrl+Alt binds nothing in any
    /// preset and resolves to nothing too.
    #[test]
    fn super_and_ctrl_alt_chords_resolve_to_no_copy_key() {
        assert_eq!(copy_key(ModifiersState::SUPER, &ch('t'), &ch('t')), None);
        assert_eq!(
            copy_key(
                ModifiersState::CONTROL | ModifiersState::ALT,
                &ch('f'),
                &ch('f')
            ),
            None
        );
    }

    /// Alt chords read the composed character when it is plain ASCII
    /// (Linux, Windows) and fall back to the base key when Option
    /// composed a symbol (macOS).
    #[test]
    fn alt_chords_prefer_the_composed_character_and_fall_back_to_base() {
        assert_eq!(
            copy_key(ModifiersState::ALT, &ch('f'), &ch('f')),
            Some(CopyKey::Alt {
                ch: 'f',
                shift: false
            })
        );
        assert_eq!(
            copy_key(ModifiersState::ALT, &ch('ƒ'), &ch('f')),
            Some(CopyKey::Alt {
                ch: 'f',
                shift: false
            }),
            "macOS Option composition falls back to the base key"
        );
        assert_eq!(
            copy_key(
                ModifiersState::ALT | ModifiersState::SHIFT,
                &ch('<'),
                &ch(',')
            ),
            Some(CopyKey::Alt {
                ch: '<',
                shift: true
            }),
            "a composed shifted pair carries the character"
        );
        assert_eq!(
            copy_key(
                ModifiersState::ALT | ModifiersState::SHIFT,
                &ch('¯'),
                &ch(',')
            ),
            Some(CopyKey::Alt {
                ch: ',',
                shift: true
            }),
            "composition hiding the pair leaves base + shift"
        );
    }

    /// Arrows and the other navigation keys classify with their shift
    /// state; the basic preset needs both.
    #[test]
    fn navigation_keys_classify_with_shift_state() {
        let up = Key::Named(NamedKey::ArrowUp);
        assert_eq!(
            copy_key(ModifiersState::empty(), &up, &up),
            Some(CopyKey::Named {
                key: NamedCopyKey::Up,
                shift: false
            })
        );
        assert_eq!(
            copy_key(ModifiersState::SHIFT, &up, &up),
            Some(CopyKey::Named {
                key: NamedCopyKey::Up,
                shift: true
            })
        );
        let home = Key::Named(NamedKey::Home);
        assert_eq!(
            copy_key(ModifiersState::empty(), &home, &home),
            Some(CopyKey::Named {
                key: NamedCopyKey::Home,
                shift: false
            })
        );
    }

    /// Ctrl+Space must classify: emacs binds it to start a selection.
    /// winit names Space rather than composing it to a character.
    #[test]
    fn ctrl_space_classifies_for_the_emacs_preset() {
        let space = Key::Named(NamedKey::Space);
        assert_eq!(
            copy_key(ModifiersState::CONTROL, &space, &space),
            Some(CopyKey::Ctrl(' '))
        );
    }

    /// Named keys outside the navigation set and multi-character IME
    /// composition bind nothing in any preset.
    #[test]
    fn named_keys_and_ime_composition_resolve_to_no_copy_key() {
        let none = ModifiersState::empty();
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(copy_key(none, &tab, &tab), None);
        let ime = Key::Character("ab".into());
        assert_eq!(copy_key(none, &ime, &ime), None);
    }

    // --- Emacs preset ---

    fn alt(ch: char) -> CopyKey {
        CopyKey::Alt { ch, shift: false }
    }

    #[test]
    fn the_emacs_preset_binds_every_row_of_the_spec_table() {
        use CopyCommand::{Exit, Search, ToggleSelection, Yank};
        let cases = [
            (CopyKey::Ctrl('n'), mv(Motion::Down)),
            (CopyKey::Ctrl('p'), mv(Motion::Up)),
            (CopyKey::Ctrl('f'), mv(Motion::Right)),
            (CopyKey::Ctrl('b'), mv(Motion::Left)),
            (alt('f'), mv(Motion::WordEnd)),
            (alt('b'), mv(Motion::WordBackward)),
            (CopyKey::Ctrl('a'), mv(Motion::LineStart)),
            (CopyKey::Ctrl('e'), mv(Motion::LineEnd)),
            (alt('<'), mv(Motion::Top)),
            (alt('>'), mv(Motion::Bottom)),
            (CopyKey::Ctrl('v'), mv(Motion::PageDown)),
            (alt('v'), mv(Motion::PageUp)),
            (
                CopyKey::Ctrl(' '),
                ToggleSelection(CopySelectionType::Character),
            ),
            (alt('w'), Yank),
            (CopyKey::Ctrl('g'), Exit),
            (CopyKey::Ctrl('s'), Search),
            (CopyKey::Ctrl('r'), Search),
            (CopyKey::Escape, CopyCommand::ClearOrExit),
        ];
        for (key, expected) in cases {
            assert_eq!(emacs_key(key), Some(expected), "{key:?}");
        }
    }

    /// `M-<`/`M->` reach the buffer ends even when Option composition
    /// hid the shifted pair and classification fell back to the base.
    #[test]
    fn emacs_buffer_ends_survive_macos_option_composition() {
        assert_eq!(
            emacs_key(CopyKey::Alt {
                ch: ',',
                shift: true
            }),
            Some(mv(Motion::Top))
        );
        assert_eq!(
            emacs_key(CopyKey::Alt {
                ch: '.',
                shift: true
            }),
            Some(mv(Motion::Bottom))
        );
        assert_eq!(emacs_key(alt(',')), None, "unshifted comma stays unbound");
        assert_eq!(emacs_key(alt('.')), None, "unshifted period stays unbound");
    }

    /// Emacs is modal too: keys the preset does not bind are dropped,
    /// including everything the vim preset would run.
    #[test]
    fn emacs_drops_unbound_keys() {
        for key in [
            CopyKey::Char('j'),
            CopyKey::Char('y'),
            CopyKey::Char('q'),
            CopyKey::Ctrl('c'),
            CopyKey::Named {
                key: NamedCopyKey::Up,
                shift: false,
            },
        ] {
            assert_eq!(emacs_key(key), None, "{key:?}");
        }
    }

    // --- Basic preset ---

    fn named(key: NamedCopyKey, shift: bool) -> CopyKey {
        CopyKey::Named { key, shift }
    }

    fn clearing(motion: Motion) -> CopyCommand {
        CopyCommand::Move {
            motion,
            selection: SelectionEffect::Clear,
        }
    }

    fn extending(motion: Motion) -> CopyCommand {
        CopyCommand::Move {
            motion,
            selection: SelectionEffect::Extend,
        }
    }

    #[test]
    fn the_basic_preset_binds_every_row_of_the_spec_table() {
        use CopyCommand::{ClearOrExit, Search, Yank};
        use NamedCopyKey::{Down, End, Home, Left, PageDown, PageUp, Right, Up};
        let cases = [
            (named(Up, false), clearing(Motion::Up)),
            (named(Down, false), clearing(Motion::Down)),
            (named(Left, false), clearing(Motion::Left)),
            (named(Right, false), clearing(Motion::Right)),
            (named(PageUp, false), clearing(Motion::PageUp)),
            (named(PageDown, false), clearing(Motion::PageDown)),
            (named(Home, false), clearing(Motion::Top)),
            (named(End, false), clearing(Motion::Bottom)),
            (named(Up, true), extending(Motion::Up)),
            (named(Down, true), extending(Motion::Down)),
            (named(Left, true), extending(Motion::Left)),
            (named(Right, true), extending(Motion::Right)),
            (CopyKey::Ctrl('c'), Yank),
            (CopyKey::Ctrl('f'), Search),
            (CopyKey::Escape, ClearOrExit),
        ];
        for (key, expected) in cases {
            assert_eq!(basic_key(key), Some(expected), "{key:?}");
        }
    }

    /// Shifted paging extends like shifted arrows do: GUI convention
    /// anchors on any Shift+navigation, not just arrows.
    #[test]
    fn basic_shifted_paging_extends_the_selection_too() {
        use NamedCopyKey::{End, Home, PageDown, PageUp};
        for (key, motion) in [
            (PageUp, Motion::PageUp),
            (PageDown, Motion::PageDown),
            (Home, Motion::Top),
            (End, Motion::Bottom),
        ] {
            assert_eq!(
                basic_key(named(key, true)),
                Some(extending(motion)),
                "{key:?}"
            );
        }
    }

    #[test]
    fn basic_drops_unbound_keys() {
        for key in [
            CopyKey::Char('j'),
            CopyKey::Char('y'),
            CopyKey::Char('q'),
            CopyKey::Ctrl('g'),
            alt('w'),
        ] {
            assert_eq!(basic_key(key), None, "{key:?}");
        }
    }

    /// The non-vim presets have no multi-key sequences, so a prefix a
    /// preset switch left behind clears instead of haunting dispatch.
    #[test]
    fn non_vim_presets_clear_a_leftover_prefix() {
        for preset in [CopyModePreset::Emacs, CopyModePreset::Basic] {
            assert_eq!(
                advance_copy_key(preset, Some(PendingPrefix::G), key('g')),
                (None, None),
                "{preset:?}"
            );
        }
    }

    /// Each preset routes to its own table: `Ctrl+f` means something
    /// different in all three, so a swapped dispatch arm cannot pass.
    #[test]
    fn advance_routes_each_preset_to_its_own_table() {
        let ctrl_f = CopyPress::Key(CopyKey::Ctrl('f'));
        assert_eq!(
            advance_copy_key(CopyModePreset::Vim, None, ctrl_f),
            (None, Some(mv(Motion::PageDown)))
        );
        assert_eq!(
            advance_copy_key(CopyModePreset::Emacs, None, ctrl_f),
            (None, Some(mv(Motion::Right)))
        );
        assert_eq!(
            advance_copy_key(CopyModePreset::Basic, None, ctrl_f),
            (None, Some(CopyCommand::Search))
        );
    }
}
