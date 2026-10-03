//! `WOW_CLASSICAPI_PROF=1`: where this crate's time goes. Each second, one log line of the frame
//! system's sections and the ten natives that cost the most, by total time and call count; any
//! single frame whose sections pass 2 ms gets its own line. Off, nothing is measured.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Prof {
    since: Instant,
    natives: HashMap<String, (Duration, u64)>,
    sections: HashMap<&'static str, Duration>,
    frames: u64,
}

static PROF: OnceLock<Option<Mutex<Prof>>> = OnceLock::new();

fn prof() -> Option<&'static Mutex<Prof>> {
    PROF.get_or_init(|| {
        (std::env::var("WOW_CLASSICAPI_PROF").as_deref() == Ok("1")).then(|| {
            Mutex::new(Prof {
                since: Instant::now(),
                natives: HashMap::new(),
                sections: HashMap::new(),
                frames: 0,
            })
        })
    })
    .as_ref()
}

pub fn enabled() -> bool {
    prof().is_some()
}

/// One native call's cost.
pub fn native(name: &str, d: Duration) {
    if let Some(p) = prof() {
        let mut p = p.lock().unwrap_or_else(|e| e.into_inner());
        let e = p.natives.entry(name.to_string()).or_default();
        e.0 += d;
        e.1 += 1;
    }
}

/// A frame's sections, timed by the frame system: logged at once when they pass 2 ms, and
/// summed into the once-a-second line.
pub fn frame(sections: &[(&'static str, Duration)]) {
    let Some(p) = prof() else {
        return;
    };
    let total: Duration = sections.iter().map(|s| s.1).sum();
    if total > Duration::from_millis(2) {
        let parts: Vec<String> = sections
            .iter()
            .map(|(n, d)| format!("{n}={:.2}", d.as_secs_f64() * 1000.0))
            .collect();
        bevy::log::warn!(
            "classicapi prof: slow frame {:.2}ms: {}",
            total.as_secs_f64() * 1000.0,
            parts.join(" ")
        );
    }
    let mut p = p.lock().unwrap_or_else(|e| e.into_inner());
    p.frames += 1;
    for (n, d) in sections {
        *p.sections.entry(n).or_default() += *d;
    }
    if p.since.elapsed() < Duration::from_secs(1) {
        return;
    }
    let frames = p.frames.max(1) as f64;
    let mut secs: Vec<_> = p.sections.iter().map(|(n, d)| (*n, *d)).collect();
    secs.sort_by_key(|s| std::cmp::Reverse(s.1));
    let secs: Vec<String> = secs
        .iter()
        .map(|(n, d)| format!("{n}={:.3}", d.as_secs_f64() * 1000.0 / frames))
        .collect();
    let mut nat: Vec<_> = p.natives.iter().map(|(n, v)| (n.clone(), *v)).collect();
    nat.sort_by_key(|n| std::cmp::Reverse(n.1 .0));
    let nat: Vec<String> = nat
        .iter()
        .take(10)
        .map(|(n, (d, c))| format!("{n} {:.2}ms/{c}", d.as_secs_f64() * 1000.0))
        .collect();
    bevy::log::info!(
        "classicapi prof: {:.0} frames, ms/frame {} | natives/s: {}",
        frames,
        secs.join(" "),
        nat.join(", ")
    );
    p.since = Instant::now();
    p.frames = 0;
    p.sections.clear();
    p.natives.clear();
}

/// The frame system's stopwatch: [`Lap::mark`] closes a section; the frame is reported when the
/// lap drops, early returns included.
pub struct Lap {
    on: bool,
    last: Instant,
    marks: Vec<(&'static str, Duration)>,
}

impl Lap {
    pub fn new() -> Self {
        Self {
            on: enabled(),
            last: Instant::now(),
            marks: Vec::new(),
        }
    }

    pub fn mark(&mut self, name: &'static str) {
        if self.on {
            let now = Instant::now();
            self.marks.push((name, now - self.last));
            self.last = now;
        }
    }
}

impl Default for Lap {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Lap {
    fn drop(&mut self) {
        if self.on {
            frame(&self.marks);
        }
    }
}
