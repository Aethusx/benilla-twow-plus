//! The engine's cooldown query (`FUN_SPELL_QUERY_COOLDOWN`, benilla's `Cooldowns::info`) over the
//! mirrored `SpellHistory` records: the longest of the spell leg (id and item match), the
//! category leg (equal category, or a wildcard row) and the GCD leg (the node's GCD category is
//! the query's `startRecoveryCategory`); a parked record reads its full duration, disabled.

use std::time::{Duration, Instant};

use crate::dbc::Row;
use crate::mirror::Mirror;
use crate::spells::col;

/// `SPELL_EFFECT_ATTACK` and `SPELL_EFFECT_TRADE_SKILL`: an `Effect[0]` of either reads cold.
const EFFECT_ATTACK: u32 = 78;
const EFFECT_TRADE_SKILL: u32 = 47;

/// One read: when the running timer started, how long it is, what is left, and whether it is
/// enabled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Read {
    pub start: Instant,
    pub duration_ms: u32,
    pub remaining_ms: u32,
    pub enabled: bool,
}

impl Read {
    /// The stock `GetSpellCooldown` triple on the `GetTime()` clock: `(start, duration, enable)`,
    /// `(0, 0, 1)` when cold.
    pub fn triple(&self, m: &Mirror) -> (f64, f64, bool) {
        if self.remaining_ms == 0 {
            return (0.0, 0.0, true);
        }
        (
            m.ui_time(self.start),
            f64::from(self.duration_ms) / 1000.0,
            self.enabled,
        )
    }
}

/// The query for a spell (`item` 0) or an item's use spell; `rec` is the spell's record.
pub fn query(m: &Mirror, spell_id: u32, item: u32, rec: Option<&Row>, now: Instant) -> Read {
    let mut best = Read {
        start: now,
        duration_ms: 0,
        remaining_ms: 0,
        enabled: true,
    };
    if rec.is_some_and(|r| matches!(r.u32(col::EFFECT), EFFECT_ATTACK | EFFECT_TRADE_SKILL)) {
        return best;
    }
    let category = rec.map_or(0, |r| r.u32(col::CATEGORY));
    let start_recovery_category = rec.map_or(0, |r| r.u32(col::START_RECOVERY_CATEGORY));
    let ms = |d: Duration| d.as_millis().min(u128::from(u32::MAX)) as u32;
    let mut consider = |timer: (Instant, Duration), remaining: Duration, enabled: bool| {
        let remaining_ms = ms(remaining);
        if remaining_ms > best.remaining_ms {
            best = Read {
                start: timer.0,
                duration_ms: ms(timer.1),
                remaining_ms,
                enabled,
            };
        }
    };
    let left = |t: (Instant, Duration)| (t.0 + t.1).saturating_duration_since(now);
    for r in &m.cooldowns {
        if r.spell_id == spell_id && r.item_id == item {
            if r.on_hold {
                consider(r.recovery, r.recovery.1, false);
            } else {
                consider(r.recovery, left(r.recovery), true);
            }
        }
        if (r.category != 0 && r.category == category) || r.category_wildcard {
            if r.on_hold {
                consider(r.category_recovery, r.category_recovery.1, false);
            } else {
                consider(r.category_recovery, left(r.category_recovery), true);
            }
        }
        if r.gcd_category == start_recovery_category && !r.gcd.1.is_zero() {
            consider(r.gcd, left(r.gcd), true);
        }
    }
    best
}
