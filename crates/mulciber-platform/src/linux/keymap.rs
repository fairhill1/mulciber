//! Shared evdev physical-key translation and typed-text composition for the Wayland and X11
//! backends.
//!
//! Wayland keyboards deliver evdev key codes directly; X11 delivers the same codes offset by
//! eight. Both backends therefore share this one table instead of maintaining peer copies.
//!
//! Typed text starts from a backend-specific keysym lookup (the Wayland keymap's `xkb_state`, or
//! Xlib's XKB-aware `XLookupString` on X11) and then runs through the same libxkbcommon Compose
//! state, so dead keys and the Compose key behave alike on both display servers.

use std::env;
use std::ffi::{CString, c_char, c_int};
use std::ptr;

use crate::{KeyCode, Modifiers};

#[repr(C)]
pub(super) struct XkbContext {
    _unused: [u8; 0],
}

#[repr(C)]
struct XkbComposeTable {
    _unused: [u8; 0],
}

#[repr(C)]
struct XkbComposeState {
    _unused: [u8; 0],
}

const XKB_CONTEXT_NO_FLAGS: c_int = 0;
const XKB_COMPOSE_COMPILE_NO_FLAGS: c_int = 0;
const XKB_COMPOSE_STATE_NO_FLAGS: c_int = 0;
const XKB_COMPOSE_FEED_ACCEPTED: c_int = 1;
const XKB_COMPOSE_COMPOSING: c_int = 1;
const XKB_COMPOSE_COMPOSED: c_int = 2;
const XKB_COMPOSE_CANCELLED: c_int = 3;
/// Room for any one Compose result; real sequences produce a few code points at most.
const COMPOSED_TEXT_CAPACITY: usize = 64;

// libxkbcommon is the canonical Linux keymap and Compose implementation: Wayland compositors hand
// clients xkb keymaps, and on X11 it reads the same system Compose files Xlib's input method does.
#[link(name = "xkbcommon")]
unsafe extern "C" {
    fn xkb_context_new(flags: c_int) -> *mut XkbContext;
    fn xkb_context_unref(context: *mut XkbContext);
    fn xkb_keysym_to_utf32(keysym: u32) -> u32;
    fn xkb_compose_table_new_from_locale(
        context: *mut XkbContext,
        locale: *const c_char,
        flags: c_int,
    ) -> *mut XkbComposeTable;
    fn xkb_compose_table_unref(table: *mut XkbComposeTable);
    fn xkb_compose_state_new(table: *mut XkbComposeTable, flags: c_int) -> *mut XkbComposeState;
    fn xkb_compose_state_unref(state: *mut XkbComposeState);
    fn xkb_compose_state_feed(state: *mut XkbComposeState, keysym: u32) -> c_int;
    fn xkb_compose_state_reset(state: *mut XkbComposeState);
    fn xkb_compose_state_get_status(state: *mut XkbComposeState) -> c_int;
    fn xkb_compose_state_get_utf8(
        state: *mut XkbComposeState,
        buffer: *mut c_char,
        size: usize,
    ) -> c_int;
}

/// What one key press types once dead-key and Compose sequences have seen its keysym.
pub(super) enum Composition {
    /// No sequence involves the key; it types its own text.
    Unchanged,
    /// The key started, continued, or cancelled a sequence and types nothing itself.
    Absorbed,
    /// The key completed a sequence that types this text.
    Composed(ComposedText),
}

/// The UTF-8 result of one completed Compose sequence.
pub(super) struct ComposedText {
    bytes: [u8; COMPOSED_TEXT_CAPACITY],
    len: usize,
}

impl ComposedText {
    /// The printable characters of the composed text, in order.
    pub(super) fn characters(&self) -> impl Iterator<Item = char> + '_ {
        printable_characters(&self.bytes[..self.len])
    }
}

/// One window's xkbcommon context and dead-key/Compose state.
///
/// Every piece is optional: a missing context or a locale without a Compose file leaves
/// composition disabled, so dead keys type nothing and every other key types its own text.
pub(super) struct TextComposer {
    context: *mut XkbContext,
    table: *mut XkbComposeTable,
    state: *mut XkbComposeState,
}

impl TextComposer {
    /// Creates the context and loads the Compose table for the process environment's locale.
    ///
    /// The locale comes from `LC_ALL`, `LC_CTYPE`, then `LANG`, as `setlocale` would resolve it,
    /// without changing the process locale. Unset or `C` falls back to xkbcommon's
    /// `en_US.UTF-8` table. `XCOMPOSEFILE` and `~/.XCompose` are honored by xkbcommon itself.
    pub(super) fn new() -> Self {
        // SAFETY: Context creation has no preconditions; a null result is handled below.
        let context = unsafe { xkb_context_new(XKB_CONTEXT_NO_FLAGS) };
        let mut composer = Self {
            context,
            table: ptr::null_mut(),
            state: ptr::null_mut(),
        };
        if context.is_null() {
            return composer;
        }
        let locale = CString::new(compose_locale()).unwrap_or_else(|_| c"C".to_owned());
        // SAFETY: The context is live and the locale is NUL-terminated; the table takes its own
        // context reference and does not retain the locale string.
        composer.table = unsafe {
            xkb_compose_table_new_from_locale(
                context,
                locale.as_ptr(),
                XKB_COMPOSE_COMPILE_NO_FLAGS,
            )
        };
        if !composer.table.is_null() {
            // SAFETY: The table is live; the state takes its own table reference.
            composer.state =
                unsafe { xkb_compose_state_new(composer.table, XKB_COMPOSE_STATE_NO_FLAGS) };
        }
        composer
    }

    /// The xkbcommon context keymaps for this window are compiled in, or null when unavailable.
    pub(super) const fn context(&self) -> *mut XkbContext {
        self.context
    }

    /// Feeds the keysym of one key press, including auto-repeats, through the Compose state.
    pub(super) fn feed(&self, keysym: u32) -> Composition {
        if self.state.is_null() {
            return Composition::Unchanged;
        }
        // SAFETY: The compose state is live and owned by this value.
        unsafe {
            // Modifier keysyms are ignored and leave a pending sequence untouched.
            if xkb_compose_state_feed(self.state, keysym) != XKB_COMPOSE_FEED_ACCEPTED {
                return Composition::Unchanged;
            }
            match xkb_compose_state_get_status(self.state) {
                XKB_COMPOSE_COMPOSING => Composition::Absorbed,
                XKB_COMPOSE_CANCELLED => {
                    // A key that breaks a sequence is consumed with it rather than typed on its
                    // own; the next key starts fresh.
                    xkb_compose_state_reset(self.state);
                    Composition::Absorbed
                }
                XKB_COMPOSE_COMPOSED => {
                    let mut text = ComposedText {
                        bytes: [0; COMPOSED_TEXT_CAPACITY],
                        len: 0,
                    };
                    let written = xkb_compose_state_get_utf8(
                        self.state,
                        text.bytes.as_mut_ptr().cast(),
                        COMPOSED_TEXT_CAPACITY,
                    );
                    // A result that does not fit is truncated by xkbcommon; drop it rather than
                    // deliver a cut code point.
                    text.len = usize::try_from(written)
                        .ok()
                        .filter(|&len| len < COMPOSED_TEXT_CAPACITY)
                        .unwrap_or(0);
                    xkb_compose_state_reset(self.state);
                    Composition::Composed(text)
                }
                _ => Composition::Unchanged,
            }
        }
    }

    /// Abandons any pending sequence, as when keyboard focus leaves the window.
    pub(super) fn reset(&self) {
        if !self.state.is_null() {
            // SAFETY: The compose state is live and owned by this value.
            unsafe { xkb_compose_state_reset(self.state) };
        }
    }
}

impl Drop for TextComposer {
    fn drop(&mut self) {
        // SAFETY: Each non-null object holds exactly the one reference this value took.
        unsafe {
            if !self.state.is_null() {
                xkb_compose_state_unref(self.state);
            }
            if !self.table.is_null() {
                xkb_compose_table_unref(self.table);
            }
            if !self.context.is_null() {
                xkb_context_unref(self.context);
            }
        }
    }
}

/// Whether a key press with these modifiers types text.
///
/// Linux layouts reach their third level through `ISO_Level3_Shift` (`AltGr`), never through
/// Control, so any held Control makes the press a shortcut that types nothing.
pub(super) const fn modifiers_type_text(modifiers: Modifiers) -> bool {
    !modifiers.control()
}

/// The printable character a keysym types, if any; control characters such as Return, Tab,
/// `BackSpace`, Escape, and Delete are reported only as physical keys.
pub(super) fn keysym_character(keysym: u32) -> Option<char> {
    // SAFETY: Keysym conversion is a pure table lookup that accepts every value.
    let code_point = unsafe { xkb_keysym_to_utf32(keysym) };
    char::from_u32(code_point).filter(|character| is_printable(*character))
}

/// The printable characters of UTF-8 key text; malformed text types nothing.
pub(super) fn printable_characters(bytes: &[u8]) -> impl Iterator<Item = char> + '_ {
    std::str::from_utf8(bytes)
        .unwrap_or_default()
        .chars()
        .filter(|character| is_printable(*character))
}

fn is_printable(character: char) -> bool {
    !character.is_control()
}

/// The `LC_CTYPE` locale name the C library would select, without calling `setlocale`.
fn compose_locale() -> String {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "C".to_owned())
}

/// The evdev codes of the aggregate-modifier keys.
///
/// Modifier keys report both ways: as physical key transitions, so games can bind them (Source
/// ducks on Ctrl), and through [`crate::InputEvent::ModifiersChanged`] for the aggregate state.
/// They don't auto-repeat.
pub(super) const fn is_modifier_key(evdev_code: u32) -> bool {
    matches!(
        evdev_code,
        29 // KEY_LEFTCTRL
        | 42 // KEY_LEFTSHIFT
        | 54 // KEY_RIGHTSHIFT
        | 56 // KEY_LEFTALT
        | 58 // KEY_CAPSLOCK
        | 97 // KEY_RIGHTCTRL
        | 100 // KEY_RIGHTALT
        | 125 // KEY_LEFTMETA
        | 126 // KEY_RIGHTMETA
    )
}

/// Maps an evdev key code to its portable physical-key identity.
#[allow(clippy::too_many_lines)]
pub(super) const fn evdev_key_code(evdev_code: u32) -> KeyCode {
    match evdev_code {
        1 => KeyCode::Escape,
        2 => KeyCode::Digit1,
        3 => KeyCode::Digit2,
        4 => KeyCode::Digit3,
        5 => KeyCode::Digit4,
        6 => KeyCode::Digit5,
        7 => KeyCode::Digit6,
        8 => KeyCode::Digit7,
        9 => KeyCode::Digit8,
        10 => KeyCode::Digit9,
        11 => KeyCode::Digit0,
        12 => KeyCode::Minus,
        13 => KeyCode::Equal,
        14 => KeyCode::Backspace,
        15 => KeyCode::Tab,
        16 => KeyCode::KeyQ,
        17 => KeyCode::KeyW,
        18 => KeyCode::KeyE,
        19 => KeyCode::KeyR,
        20 => KeyCode::KeyT,
        21 => KeyCode::KeyY,
        22 => KeyCode::KeyU,
        23 => KeyCode::KeyI,
        24 => KeyCode::KeyO,
        25 => KeyCode::KeyP,
        26 => KeyCode::BracketLeft,
        27 => KeyCode::BracketRight,
        28 => KeyCode::Enter,
        30 => KeyCode::KeyA,
        31 => KeyCode::KeyS,
        32 => KeyCode::KeyD,
        33 => KeyCode::KeyF,
        34 => KeyCode::KeyG,
        35 => KeyCode::KeyH,
        36 => KeyCode::KeyJ,
        37 => KeyCode::KeyK,
        38 => KeyCode::KeyL,
        39 => KeyCode::Semicolon,
        40 => KeyCode::Quote,
        41 => KeyCode::Backquote,
        43 => KeyCode::Backslash,
        44 => KeyCode::KeyZ,
        45 => KeyCode::KeyX,
        46 => KeyCode::KeyC,
        47 => KeyCode::KeyV,
        48 => KeyCode::KeyB,
        49 => KeyCode::KeyN,
        50 => KeyCode::KeyM,
        51 => KeyCode::Comma,
        52 => KeyCode::Period,
        53 => KeyCode::Slash,
        55 => KeyCode::NumpadMultiply,
        57 => KeyCode::Space,
        59 => KeyCode::F1,
        60 => KeyCode::F2,
        61 => KeyCode::F3,
        62 => KeyCode::F4,
        63 => KeyCode::F5,
        64 => KeyCode::F6,
        65 => KeyCode::F7,
        66 => KeyCode::F8,
        67 => KeyCode::F9,
        68 => KeyCode::F10,
        71 => KeyCode::Numpad7,
        72 => KeyCode::Numpad8,
        73 => KeyCode::Numpad9,
        74 => KeyCode::NumpadSubtract,
        75 => KeyCode::Numpad4,
        76 => KeyCode::Numpad5,
        77 => KeyCode::Numpad6,
        78 => KeyCode::NumpadAdd,
        79 => KeyCode::Numpad1,
        80 => KeyCode::Numpad2,
        81 => KeyCode::Numpad3,
        82 => KeyCode::Numpad0,
        83 => KeyCode::NumpadDecimal,
        87 => KeyCode::F11,
        88 => KeyCode::F12,
        96 => KeyCode::NumpadEnter,
        98 => KeyCode::NumpadDivide,
        102 => KeyCode::Home,
        103 => KeyCode::ArrowUp,
        104 => KeyCode::PageUp,
        105 => KeyCode::ArrowLeft,
        106 => KeyCode::ArrowRight,
        107 => KeyCode::End,
        108 => KeyCode::ArrowDown,
        109 => KeyCode::PageDown,
        110 => KeyCode::Insert,
        111 => KeyCode::Delete,
        117 => KeyCode::NumpadEqual,
        183 => KeyCode::F13,
        184 => KeyCode::F14,
        185 => KeyCode::F15,
        186 => KeyCode::F16,
        187 => KeyCode::F17,
        188 => KeyCode::F18,
        189 => KeyCode::F19,
        190 => KeyCode::F20,
        29 => KeyCode::ControlLeft,
        42 => KeyCode::ShiftLeft,
        54 => KeyCode::ShiftRight,
        56 => KeyCode::AltLeft,
        58 => KeyCode::CapsLock,
        97 => KeyCode::ControlRight,
        100 => KeyCode::AltRight,
        125 => KeyCode::SuperLeft,
        126 => KeyCode::SuperRight,
        other => KeyCode::Unidentified(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Composition, TextComposer, evdev_key_code, is_modifier_key, keysym_character,
        modifiers_type_text, printable_characters,
    };
    use crate::{KeyCode, Modifiers};

    #[test]
    fn keysyms_type_printable_characters_only() {
        assert_eq!(keysym_character(0x0061), Some('a')); // a
        assert_eq!(keysym_character(0x0041), Some('A')); // A
        assert_eq!(keysym_character(0x00fc), Some('\u{fc}')); // udiaeresis
        assert_eq!(keysym_character(0x06c1), Some('\u{430}')); // Cyrillic_a
        assert_eq!(keysym_character(0x20ac), Some('\u{20ac}')); // EuroSign
        assert_eq!(keysym_character(0xffb1), Some('1')); // KP_1
        assert_eq!(keysym_character(0x0020), Some(' ')); // space
        for control in [
            0xff0d, // Return
            0xff08, // BackSpace
            0xff09, // Tab
            0xff1b, // Escape
            0xffff, // Delete
            0xff8d, // KP_Enter
            0xff52, // Up
            0xffe1, // Shift_L
            0xfe52, // dead_circumflex
            0,      // NoSymbol
        ] {
            assert_eq!(keysym_character(control), None, "keysym {control:#x}");
        }
    }

    #[test]
    fn control_suppresses_text_but_shift_and_caps_lock_do_not() {
        assert!(modifiers_type_text(Modifiers::default()));
        assert!(modifiers_type_text(Modifiers::from_bits(
            Modifiers::SHIFT | Modifiers::CAPS_LOCK | Modifiers::ALT
        )));
        assert!(!modifiers_type_text(Modifiers::from_bits(
            Modifiers::CONTROL
        )));
        assert!(!modifiers_type_text(Modifiers::from_bits(
            Modifiers::CONTROL | Modifiers::ALT
        )));
    }

    #[test]
    fn utf8_key_text_drops_control_characters() {
        let text: String = printable_characters("a\r\u{e9}\t\u{7f}".as_bytes()).collect();
        assert_eq!(text, "a\u{e9}");
        assert_eq!(printable_characters(&[0xff, 0x41]).count(), 0);
    }

    /// Depends on the host's Compose data; skips when no table loads, as on a minimal system.
    #[test]
    fn dead_keys_compose_when_the_locale_has_a_compose_table() {
        let composer = TextComposer::new();
        if composer.state.is_null() {
            return;
        }
        assert!(matches!(composer.feed(0x0061), Composition::Unchanged)); // a
        assert!(matches!(composer.feed(0xfe52), Composition::Absorbed)); // dead_circumflex
        assert!(matches!(composer.feed(0xffe1), Composition::Unchanged)); // Shift_L is ignored
        let Composition::Composed(text) = composer.feed(0x0065) else {
            panic!("dead_circumflex then e should compose");
        };
        assert_eq!(text.characters().collect::<String>(), "\u{ea}");
        assert!(matches!(composer.feed(0xfe52), Composition::Absorbed));
        composer.reset();
        assert!(matches!(composer.feed(0x0065), Composition::Unchanged));
    }

    #[test]
    fn physical_key_mapping_distinguishes_navigation_and_numpad_keys() {
        assert_eq!(evdev_key_code(17), KeyCode::KeyW);
        assert_eq!(evdev_key_code(103), KeyCode::ArrowUp);
        assert_eq!(evdev_key_code(72), KeyCode::Numpad8);
        assert_eq!(evdev_key_code(96), KeyCode::NumpadEnter);
        assert_eq!(evdev_key_code(28), KeyCode::Enter);
        assert_eq!(evdev_key_code(190), KeyCode::F20);
        assert_eq!(evdev_key_code(240), KeyCode::Unidentified(240));
    }

    #[test]
    fn modifier_keys_have_physical_identities() {
        for code in [29, 42, 54, 56, 58, 97, 100, 125, 126] {
            assert!(is_modifier_key(code));
            assert!(!matches!(evdev_key_code(code), KeyCode::Unidentified(_)));
        }
        assert_eq!(evdev_key_code(29), KeyCode::ControlLeft);
        assert_eq!(evdev_key_code(97), KeyCode::ControlRight);
        assert_eq!(evdev_key_code(54), KeyCode::ShiftRight);
        assert!(!is_modifier_key(30));
        assert!(!is_modifier_key(57));
    }
}
