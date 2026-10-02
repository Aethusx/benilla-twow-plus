//! `KEY_DOWN` and `KEY_UP`: every key press and release, as the client's `CSimpleTop` key hooks
//! report them, in its `KEY` codes, with the shift (1), ctrl (2) and alt (4) state.

use bevy::input::keyboard::{KeyCode, KeyboardInput};
use bevy::input::ButtonState;
use bevy::prelude::*;

use crate::events::n;
use crate::Np;

/// The client's `KEY` enum value for a key; `None` for a key it has no code for.
pub fn key_code(key: KeyCode) -> Option<i64> {
    use KeyCode::*;
    let letters = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    if let Some(i) = letters.iter().position(|k| *k == key) {
        return Some(65 + i as i64);
    }
    let digits = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    if let Some(i) = digits.iter().position(|k| *k == key) {
        return Some(48 + i as i64);
    }
    let numpad = [
        Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
    ];
    if let Some(i) = numpad.iter().position(|k| *k == key) {
        return Some(257 + i as i64);
    }
    let f = [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12];
    if let Some(i) = f.iter().position(|k| *k == key) {
        return Some(768 + i as i64);
    }
    Some(match key {
        ShiftLeft | ShiftRight => 0,
        ControlLeft | ControlRight => 1,
        AltLeft | AltRight => 2,
        Space => 32,
        Backquote => 256,
        NumpadAdd => 267,
        NumpadSubtract => 268,
        NumpadMultiply => 269,
        NumpadDivide => 270,
        NumpadDecimal => 271,
        Equal => 272,
        Minus => 273,
        BracketLeft => 274,
        BracketRight => 275,
        Slash => 276,
        Backslash => 277,
        Semicolon => 278,
        Quote => 279,
        Comma => 280,
        Period => 281,
        Escape => 512,
        Enter | NumpadEnter => 513,
        Backspace => 514,
        Tab => 515,
        ArrowLeft => 516,
        ArrowUp => 517,
        ArrowRight => 518,
        ArrowDown => 519,
        Insert => 520,
        Delete => 521,
        Home => 522,
        End => 523,
        PageUp => 524,
        PageDown => 525,
        CapsLock => 526,
        NumLock => 527,
        ScrollLock => 528,
        Pause => 529,
        PrintScreen => 530,
        _ => return None,
    })
}

/// Queue a `KEY_DOWN`/`KEY_UP` for each key event this frame, when a frame registered it.
pub fn key_events(
    np: Res<Np>,
    mut input: MessageReader<KeyboardInput>,
    held: Res<ButtonInput<KeyCode>>,
) {
    let mut st = np.lock();
    if !st.wants("KEY_DOWN") && !st.wants("KEY_UP") {
        input.clear();
        return;
    }
    let meta = i64::from(held.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]))
        | i64::from(held.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])) << 1
        | i64::from(held.any_pressed([KeyCode::AltLeft, KeyCode::AltRight])) << 2;
    let now = st.now_ms();
    for key in input.read() {
        let Some(code) = key_code(key.key_code) else {
            continue;
        };
        let event = match key.state {
            ButtonState::Pressed => "KEY_DOWN",
            ButtonState::Released => "KEY_UP",
        };
        let repeat = i64::from(key.repeat);
        st.emit(event, || vec![n(code), n(meta), n(repeat), n(now as i64)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_the_client_codes() {
        assert_eq!(key_code(KeyCode::KeyA), Some(65));
        assert_eq!(key_code(KeyCode::Digit9), Some(57));
        assert_eq!(key_code(KeyCode::F12), Some(779));
        assert_eq!(key_code(KeyCode::Numpad3), Some(260));
        assert_eq!(key_code(KeyCode::ShiftRight), Some(0));
        assert_eq!(key_code(KeyCode::Enter), Some(513));
        assert_eq!(key_code(KeyCode::MediaPlayPause), None);
    }
}
