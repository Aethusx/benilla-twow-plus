//! `UnitXP("notify", ...)` (`notifyOS.cpp`): flash the taskbar icon, or play a Windows system
//! sound, only while the game is in the background. Windows only, as the DLL; elsewhere the
//! requests are dropped.

use bevy::prelude::*;

use crate::Ux;

/// The system sound aliases the DLL accepts.
pub const SOUNDS: [&str; 8] = [
    "SystemAsterisk",
    "SystemDefault",
    "SystemExclamation",
    "SystemExit",
    "SystemHand",
    "SystemQuestion",
    "SystemStart",
    "SystemWelcome",
];

/// Send the frame's notifications.
pub fn notify(ux: Res<Ux>, windows: Query<&bevy::window::RawHandleWrapper>) {
    let (flash, sound) = {
        let mut st = ux.lock();
        (std::mem::take(&mut st.flash), st.sound.take())
    };
    if flash {
        if let Ok(handle) = windows.single() {
            platform::flash(handle);
        }
    }
    if let Some(sound) = sound {
        platform::sound(&sound);
    }
}

#[cfg(windows)]
mod platform {
    use bevy::window::RawHandleWrapper;
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_SENTRY};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FlashWindowEx, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY,
    };

    pub fn flash(handle: &RawHandleWrapper) {
        let raw_window_handle::RawWindowHandle::Win32(h) = handle.get_window_handle() else {
            return;
        };
        let info = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd: h.hwnd.get() as _,
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        // SAFETY: `info` is a valid, fully initialised FLASHWINFO for the game window's handle.
        unsafe {
            FlashWindowEx(&info);
        }
    }

    pub fn sound(alias: &str) {
        let wide: Vec<u16> = alias.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 alias that outlives the call; SND_ASYNC copies
        // what it needs.
        unsafe {
            PlaySoundW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                SND_ALIAS | SND_ASYNC | SND_SENTRY,
            );
        }
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn flash(_: &bevy::window::RawHandleWrapper) {}
    pub fn sound(_: &str) {}
}
