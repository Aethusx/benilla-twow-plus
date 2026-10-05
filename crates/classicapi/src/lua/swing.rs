//! `combat/Swing.cpp` and `combat/SwingRange.cpp`: the Classic swing-timer API backported from
//! packets 1.12 already receives: `PLAYER_SWING(swingDuration, swingType)`, `Enum.PlayerSwingType`,
//! `C_SwingTimer.EnableRangeCheck`, `IsTargetWithinSwingRange` and
//! `PLAYER_SWING_RANGE_UPDATE(swingType, isInRange, checksRange)`.
//!
//! The timers are state, not a relay: a white hit of ours resets its hand
//! (`SMSG_ATTACKERSTATEUPDATE`, the left-swing bit naming the off hand); an on-next-swing ability
//! resets the main hand, an auto-repeat shot the ranged one less its wind-up, and a cast-time
//! spell we sent both melee hands (`SMSG_SPELL_GO`, by `Spell.dbc`'s flags); our attack start
//! resets the off hand when in reach (`SMSG_ATTACKSTART`); a weapon swapped in combat resets its
//! hand; and our parry cuts the hand nearer its swing as the server's parry haste does. Each
//! change marks its hand, and the frame fires at most one `PLAYER_SWING` per hand with the time
//! left, so a burst of extra attacks is one event.
//!
//! Melee reach is the server's own 2D test, both bounding radii plus 4/3 yd, at least 5; ranged
//! reach is the Auto Shot or Shoot range from the spell range check.

use std::time::{Duration, Instant};

use benilla_ui::script::ScriptValue;
use mlua::Value;

use crate::lua::{is_number, to_int, truthy, Api};
use crate::mirror::{field, Mirror};
use crate::Ca;

pub(crate) const MAIN_HAND: usize = 0;
pub(crate) const OFF_HAND: usize = 1;
pub(crate) const RANGED: usize = 2;

/// `UNIT_FIELD_BASEATTACKTIME`, its off-hand twin, `UNIT_FIELD_RANGEDATTACKTIME`.
const ATTACK_TIME: [usize; 3] = [
    field::UNIT_BASE_ATTACK_TIME,
    field::UNIT_BASE_ATTACK_TIME + 1,
    field::UNIT_RANGED_ATTACK_TIME,
];
/// `HITINFO_LEFTSWING` and `VICTIMSTATE_PARRY`.
const HITINFO_LEFTSWING: u32 = 0x4;
const VICTIMSTATE_PARRY: u32 = 3;
/// `SPELL_ATTR_ON_NEXT_SWING` (both), `SPELL_ATTR_EX2_AUTOREPEAT_FLAG`,
/// `SPELL_ATTR_EX2_NOT_RESET_AUTO_ACTIONS`, `SPELL_INTERRUPT_FLAG_AUTOATTACK`.
const ATTR_ON_NEXT_SWING: u32 = 0x04 | 0x400;
const ATTR_EX2_AUTOREPEAT: u32 = 0x20;
const ATTR_EX2_NOT_RESET_AUTO_ACTIONS: u32 = 0x20000;
const INTERRUPT_AUTOATTACK: u32 = 0x08;
/// How long a cast we sent counts as ours for its `SMSG_SPELL_GO`.
const SENT_TTL: Duration = Duration::from_secs(3);
/// The unarmed swing, for the parry haste with no weapon record.
pub(crate) const UNARMED_MS: u32 = 2000;
/// The probes for ranged reach: Auto Shot (bows, guns, crossbows) and Shoot (wands).
const AUTO_SHOT: i64 = 75;
const SHOOT: i64 = 5019;
const MELEE_REACH_ADD: f32 = 4.0 / 3.0;
const MELEE_FLOOR: f32 = 5.0;

/// A hand's range state: no check (no weapon, no target, not attackable), in or out of reach.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Range {
    #[default]
    NoCheck,
    In,
    Out,
}

/// The swing timers and the range watch.
#[derive(Default)]
pub(crate) struct Swing {
    end: [Option<Instant>; 3],
    dirty: [bool; 3],
    enabled: [bool; 3],
    last: [Range; 3],
    weapons: Option<[u64; 3]>,
    sent: Vec<(u32, Instant)>,
}

/// The player's swing time for a hand, ms; 0 is no weapon for it.
pub(crate) fn attack_time(m: &Mirror, hand: usize) -> u32 {
    m.me().map_or(0, |f| f.u32(ATTACK_TIME[hand]))
}

/// `InMeleeRange`: the server's 2D reach test.
pub(crate) fn in_melee_range(m: &Mirror, target: u64) -> bool {
    let (Some(a), Some(b)) = (m.place(m.player), m.place(target)) else {
        return false;
    };
    let radius = |g| {
        m.object(g)
            .map_or(0.0, |f| f.f32(field::UNIT_BOUNDING_RADIUS))
    };
    let reach = (radius(m.player) + radius(target) + MELEE_REACH_ADD).max(MELEE_FLOOR);
    let (dx, dy) = (a.pos[0] - b.pos[0], a.pos[1] - b.pos[1]);
    dx * dx + dy * dy < reach * reach
}

impl Swing {
    fn mark(&mut self, hand: usize, ms: u32, now: Instant) {
        if ms == 0 {
            return;
        }
        self.end[hand] = Some(now + Duration::from_millis(u64::from(ms)));
        self.dirty[hand] = true;
    }

    fn remaining(&self, hand: usize, now: Instant) -> Duration {
        self.end[hand].map_or(Duration::ZERO, |e| e.saturating_duration_since(now))
    }

    /// A cast we sent, for the non-triggered gate.
    pub(crate) fn on_sent(&mut self, spell: u32, now: Instant) {
        self.sent.retain(|(_, t)| now.duration_since(*t) < SENT_TTL);
        self.sent.push((spell, now));
    }

    fn took_sent(&mut self, spell: u32, now: Instant) -> bool {
        match self
            .sent
            .iter()
            .position(|(s, t)| *s == spell && now.duration_since(*t) < SENT_TTL)
        {
            Some(i) => {
                self.sent.remove(i);
                true
            }
            None => false,
        }
    }

    /// `SMSG_ATTACKERSTATEUPDATE`: our white hit, or our parry. `delays` is each melee hand's
    /// unhasted weapon delay.
    pub(crate) fn on_attacker_state(
        &mut self,
        m: &Mirror,
        attacker: u64,
        victim: u64,
        hit_info: u32,
        victim_state: u32,
        delays: [u32; 2],
        now: Instant,
    ) {
        if m.player == 0 {
            return;
        }
        if attacker == m.player {
            let hand = if hit_info & HITINFO_LEFTSWING != 0 {
                OFF_HAND
            } else {
                MAIN_HAND
            };
            self.mark(hand, attack_time(m, hand), now);
        } else if victim == m.player && victim_state == VICTIMSTATE_PARRY {
            self.parry_haste(delays, now);
        }
    }

    /// `ApplyParryHaste`: the hand nearer its swing, cut to 20% of its delay from 20-60% left, or
    /// by 40% past 60%; under 20% it is left alone.
    fn parry_haste(&mut self, delays: [u32; 2], now: Instant) {
        let (main, off) = (
            self.remaining(MAIN_HAND, now),
            self.remaining(OFF_HAND, now),
        );
        let hand = if self.end[OFF_HAND].is_some() && off < main {
            OFF_HAND
        } else {
            MAIN_HAND
        };
        if self.end[hand].is_none() {
            return;
        }
        let r = if hand == OFF_HAND { off } else { main }.as_secs_f32() * 1000.0;
        let p20 = delays[hand] as f32 * 0.2;
        let new = if r > p20 && r <= 3.0 * p20 {
            p20
        } else if r > 3.0 * p20 {
            r - 2.0 * p20
        } else {
            return;
        };
        self.end[hand] = Some(now + Duration::from_millis(new as u64));
        self.dirty[hand] = true;
    }

    /// `SMSG_ATTACKSTART`: ours resets the off hand when the target is in reach.
    pub(crate) fn on_attack_start(&mut self, m: &Mirror, attacker: u64, victim: u64, now: Instant) {
        if attacker != 0 && attacker == m.player && in_melee_range(m, victim) {
            self.mark(OFF_HAND, attack_time(m, OFF_HAND), now);
        }
    }

    /// `OnSpellGo` for our cast: its `(Attributes, AttributesEx2, InterruptFlags)` and cast time.
    pub(crate) fn on_spell_go(
        &mut self,
        m: &Mirror,
        spell: u32,
        flags: (u32, u32, u32),
        cast_ms: u32,
        now: Instant,
    ) {
        let (attr, ex2, interrupt) = flags;
        if attr & ATTR_ON_NEXT_SWING != 0 {
            self.mark(MAIN_HAND, attack_time(m, MAIN_HAND), now);
        } else if ex2 & ATTR_EX2_AUTOREPEAT != 0 {
            // The shot's go lands at the end of its wind-up, after the server reset the timer.
            let ranged = attack_time(m, RANGED);
            self.mark(RANGED, ranged.saturating_sub(cast_ms), now);
        } else if interrupt & INTERRUPT_AUTOATTACK != 0
            && ex2 & ATTR_EX2_NOT_RESET_AUTO_ACTIONS == 0
            && self.took_sent(spell, now)
        {
            self.mark(MAIN_HAND, attack_time(m, MAIN_HAND), now);
            self.mark(OFF_HAND, attack_time(m, OFF_HAND), now);
        }
    }

    /// The frame: a weapon swapped in combat resets its hand, then one `PLAYER_SWING` per marked
    /// hand with time left.
    pub(crate) fn fires(
        &mut self,
        m: &Mirror,
        now: Instant,
    ) -> Vec<(&'static str, Vec<ScriptValue>)> {
        let weapons = m
            .me()
            .map(|_| [16, 17, 18].map(|slot| crate::items::equipment_slot(m, slot).unwrap_or(0)));
        let in_combat = m
            .me()
            .is_some_and(|f| f.u32(field::UNIT_FLAGS) & 0x0008_0000 != 0);
        if let (Some(old), Some(new)) = (self.weapons, weapons) {
            if in_combat {
                for hand in 0..3 {
                    if old[hand] != new[hand] {
                        self.mark(hand, attack_time(m, hand), now);
                    }
                }
            }
        }
        self.weapons = weapons;
        let mut out = Vec::new();
        for hand in 0..3 {
            if !std::mem::take(&mut self.dirty[hand]) {
                continue;
            }
            let left = self.remaining(hand, now);
            if left.is_zero() {
                continue;
            }
            out.push((
                "PLAYER_SWING",
                vec![
                    ScriptValue::Number(left.as_secs_f64()),
                    ScriptValue::Int(hand as i64),
                ],
            ));
        }
        out
    }

    /// The range watch's frame: `PLAYER_SWING_RANGE_UPDATE` per enabled hand whose state moved.
    pub(crate) fn range_fires(
        &mut self,
        states: [Range; 3],
    ) -> Vec<(&'static str, Vec<ScriptValue>)> {
        let mut out = Vec::new();
        for (hand, state) in states.into_iter().enumerate() {
            if !self.enabled[hand] || state == self.last[hand] {
                continue;
            }
            self.last[hand] = state;
            let flag = |on: bool| {
                if on {
                    ScriptValue::Int(1)
                } else {
                    ScriptValue::Nil
                }
            };
            out.push((
                "PLAYER_SWING_RANGE_UPDATE",
                vec![
                    ScriptValue::Int(hand as i64),
                    flag(state == Range::In),
                    flag(state != Range::NoCheck),
                ],
            ));
        }
        out
    }

    pub(crate) fn any_range_check(&self) -> bool {
        self.enabled.iter().any(|e| *e)
    }
}

/// The ranged probe: the shot in flight, else by the ranged weapon's ammo type.
fn ranged_probe(ca: &Ca, auto_repeat: Option<u32>) -> i64 {
    if let Some(s) = auto_repeat.filter(|s| *s != 0) {
        return i64::from(s);
    }
    let entry = {
        let st = ca.lock();
        crate::items::equipment_slot(&st.mirror, 18).map(|g| crate::items::item_id(&st.mirror, g))
    };
    match entry.and_then(|e| ca.items.lock().peek(e)) {
        Some(r) if r.ammo_type == 0 => SHOOT,
        _ => AUTO_SHOT,
    }
}

/// `Evaluate`: a hand's range state against `target`, given whether it may be attacked.
pub(crate) fn evaluate(
    ca: &Ca,
    hand: usize,
    target: Option<u64>,
    attackable: bool,
    auto_repeat: Option<u32>,
) -> Range {
    let (time, melee) = {
        let st = ca.lock();
        let m = &st.mirror;
        (attack_time(m, hand), target.map(|t| in_melee_range(m, t)))
    };
    let Some(target) = target else {
        return Range::NoCheck;
    };
    if time == 0 || !attackable {
        return Range::NoCheck;
    }
    if hand != RANGED {
        return if melee == Some(true) {
            Range::In
        } else {
            Range::Out
        };
    }
    let probe = ranged_probe(ca, auto_repeat);
    match crate::lua::spell::data::player_vs_unit(ca, probe, Some(target)) {
        Some(true) => Range::In,
        Some(false) => Range::Out,
        None => Range::NoCheck,
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    api.int_enum(
        "Enum",
        "PlayerSwingType",
        &[("MainHand", 0), ("OffHand", 1), ("Ranged", 2)],
    )?;
    let target_state = |lua: &mlua::Lua, ca: &Ca, hand: usize| -> Range {
        let target = benilla_ui::script::ext_read::unit_guid(lua, "target")
            .ok()
            .flatten();
        let attackable = lua
            .globals()
            .get::<mlua::Function>("UnitCanAttack")
            .and_then(|f| f.call::<Value>(("player", "target")))
            .is_ok_and(|v| truthy(&v));
        let auto_repeat = ca.lock().auto_repeat;
        evaluate(ca, hand, target, attackable, auto_repeat)
    };
    let c = api.ca.clone();
    api.table(
        "C_SwingTimer",
        "EnableRangeCheck",
        move |lua, (t, on): (Value, Value)| {
            if !is_number(&t) {
                return Err(mlua::Error::runtime(
                    "Usage: C_SwingTimer.EnableRangeCheck(swingType, enable)",
                ));
            }
            let Ok(hand) = usize::try_from(to_int(&t)) else {
                return Ok(());
            };
            if hand > RANGED {
                return Ok(());
            }
            let on = truthy(&on);
            // Seeded without a fire: a caller reads the state right after enabling.
            let state = if on {
                target_state(lua, &c, hand)
            } else {
                Range::NoCheck
            };
            let mut st = c.lock();
            st.swing.enabled[hand] = on;
            st.swing.last[hand] = state;
            Ok(())
        },
    )?;
    let c = api.ca.clone();
    api.table(
        "C_SwingTimer",
        "IsTargetWithinSwingRange",
        move |lua, t: Value| {
            if !is_number(&t) {
                return Err(mlua::Error::runtime(
                    "Usage: C_SwingTimer.IsTargetWithinSwingRange(swingType)",
                ));
            }
            let hand = usize::try_from(to_int(&t)).ok().filter(|h| *h <= RANGED);
            Ok(match hand.map(|h| target_state(lua, &c, h)) {
                Some(Range::In) => Some(true),
                Some(Range::Out) => Some(false),
                _ => None,
            })
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::{Fields, Place};

    fn mirror(main: u32, off: u32) -> Mirror {
        let mut m = Mirror::default();
        m.player = 1;
        let mut cells = vec![0u32; 1300];
        cells[field::OBJECT_TYPE] = 0x19;
        cells[ATTACK_TIME[MAIN_HAND]] = main;
        cells[ATTACK_TIME[OFF_HAND]] = off;
        m.objects.insert(1, Fields::from_vec(cells));
        m.places.insert(1, Place::default());
        m
    }

    #[test]
    fn hits_reset_their_hand_and_a_burst_fires_once() {
        let m = mirror(2600, 1800);
        let mut s = Swing::default();
        let now = Instant::now();
        s.on_attacker_state(&m, 1, 9, 0, 1, [2600, 1800], now);
        s.on_attacker_state(&m, 1, 9, 0, 1, [2600, 1800], now);
        s.on_attacker_state(&m, 1, 9, HITINFO_LEFTSWING, 1, [2600, 1800], now);
        let fires = s.fires(&m, now);
        assert_eq!(fires.len(), 2);
        assert_eq!(
            fires[0].1,
            vec![ScriptValue::Number(2.6), ScriptValue::Int(0)]
        );
        assert_eq!(fires[1].1[1], ScriptValue::Int(1));
        assert!(s.fires(&m, now).is_empty());
    }

    #[test]
    fn a_parry_cuts_the_nearer_hand_and_casts_reset_by_their_flags() {
        let m = mirror(2000, 0);
        let mut s = Swing::default();
        let now = Instant::now();
        s.on_attacker_state(&m, 1, 9, 0, 1, [2000, 0], now);
        s.fires(&m, now);
        // 1.0 s left of 2.0: inside 20-60% (0.4-1.2 s), cut to 0.4 s.
        let later = now + Duration::from_millis(1000);
        s.on_attacker_state(&m, 9, 1, 0, VICTIMSTATE_PARRY, [2000, 0], later);
        let f = s.fires(&m, later);
        assert_eq!(f[0].1, vec![ScriptValue::Number(0.4), ScriptValue::Int(0)]);
        // A cast-time spell resets only when we sent it.
        s.on_spell_go(&m, 133, (0, 0, INTERRUPT_AUTOATTACK), 0, later);
        assert!(s.fires(&m, later).is_empty());
        s.on_sent(133, later);
        s.on_spell_go(&m, 133, (0, 0, INTERRUPT_AUTOATTACK), 0, later);
        assert_eq!(s.fires(&m, later).len(), 1);
        // Heroic Strike's next-swing flag resets the main hand.
        s.on_spell_go(&m, 78, (0x400, 0, 0), 0, later);
        assert_eq!(s.fires(&m, later).len(), 1);
    }

    #[test]
    fn the_range_watch_fires_on_change_only() {
        let mut s = Swing::default();
        s.enabled[MAIN_HAND] = true;
        assert!(s.range_fires([Range::NoCheck; 3]).is_empty());
        let f = s.range_fires([Range::In, Range::NoCheck, Range::NoCheck]);
        assert_eq!(
            f[0].1,
            vec![
                ScriptValue::Int(0),
                ScriptValue::Int(1),
                ScriptValue::Int(1)
            ]
        );
        let f = s.range_fires([Range::Out, Range::NoCheck, Range::NoCheck]);
        assert_eq!(
            f[0].1,
            vec![ScriptValue::Int(0), ScriptValue::Nil, ScriptValue::Int(1)]
        );
        assert!(s.range_fires([Range::Out, Range::In, Range::In]).is_empty());
    }
}
