//! Shared evdev physical-key translation for the Wayland and X11 backends.
//!
//! Wayland keyboards deliver evdev key codes directly; X11 delivers the same codes offset by
//! eight. Both backends therefore share this one table instead of maintaining peer copies.

use crate::KeyCode;

/// The evdev codes of the aggregate-modifier keys.
///
/// Modifier keys are reported through [`crate::InputEvent::ModifiersChanged`] rather than as
/// physical key transitions, matching the `AppKit` and Win32 backends.
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
        other => KeyCode::Unidentified(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{evdev_key_code, is_modifier_key};
    use crate::KeyCode;

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
    fn modifier_keys_are_excluded_from_physical_transitions() {
        for code in [29, 42, 54, 56, 58, 97, 100, 125, 126] {
            assert!(is_modifier_key(code));
        }
        assert!(!is_modifier_key(30));
        assert!(!is_modifier_key(57));
    }
}
