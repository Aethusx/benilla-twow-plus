//! `UnitXP("FPScap", n)` and `UnitXP("backgroundFPScap", n)` (`FPScap.cpp`): the frame ends no
//! sooner than `1/n` s after the last, in the world only; 0 is no cap, and the cap is held to 500.
//! The wait sleeps to within a millisecond, then spins, as the DLL does.

use std::time::{Duration, Instant};

use bevy::prelude::*;

use crate::Ux;

/// The interval a cap asks for, `None` for no cap.
pub fn interval(cap: f64) -> Option<Duration> {
    (cap >= 1.0).then(|| Duration::from_secs_f64(1.0 / cap.min(500.0)))
}

/// The cap's frame-end wait.
pub fn cap(ux: Res<Ux>, mut next: Local<Option<Instant>>) {
    let (in_world, cap) = {
        let st = ux.lock();
        let s = &st.settings;
        (
            st.player != 0,
            if st.focused {
                s.fps_cap
            } else {
                s.background_fps_cap
            },
        )
    };
    let Some(step) = interval(cap).filter(|_| in_world) else {
        *next = None;
        return;
    };
    let now = Instant::now();
    let target = next.unwrap_or(now);
    if target > now {
        let wait = target - now;
        if wait > Duration::from_millis(2) {
            std::thread::sleep(wait - Duration::from_millis(1));
        }
        while Instant::now() < target {
            std::hint::spin_loop();
        }
    }
    let done = Instant::now();
    // A frame that ran past its slot starts the next slot from now, not from the missed one.
    *next = Some(if done < target + step {
        target + step
    } else {
        done + step
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_hold_to_the_dlls_range() {
        assert_eq!(interval(0.0), None);
        assert_eq!(interval(0.5), None);
        assert_eq!(interval(60.0), Some(Duration::from_secs_f64(1.0 / 60.0)));
        assert_eq!(interval(9000.0), Some(Duration::from_secs_f64(1.0 / 500.0)));
    }
}
