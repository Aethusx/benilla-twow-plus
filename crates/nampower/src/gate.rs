//! The engine behind benilla's cast gate: every press, and every queued cast nampower sends, is
//! classified from `Spell.dbc` and run through [`crate::engine::Engine::attempt`], which holds it,
//! queues it, or lets it past the in-flight guard; the ladder's outcome comes back the same way.

use std::time::Instant;

use benilla_app::ext::{CastAttempt, CastGate, CastOutcome, GateVerdict};

use crate::dbc::{consts, Databases, SpellRec};
use crate::engine::{Attempt, Origin, Outcome, PlayerFacts, SpellFacts};
use crate::mirror::field;
use crate::Np;

pub struct NpGate(pub Np);

/// The facts the engine reads off a `Spell.dbc` row.
pub fn facts(rec: SpellRec, db: &Databases) -> SpellFacts {
    SpellFacts {
        id: rec.id(),
        on_gcd: rec.on_gcd(),
        channeled: rec.channeled(),
        targeting: rec.targeting(),
        on_swing: rec.on_swing(),
        special: rec.special(),
        disabled_while_active: rec.attributes() & consts::ATTR_DISABLED_WHILE_ACTIVE != 0,
        auto_repeat: rec.auto_repeat(),
        mounting: rec.mounting(),
        summon_guardian: rec.effect(0) == consts::EFFECT_SUMMON_GUARDIAN,
        // `SPELL_ATTR_EX_TOGGLE_FARSIGHT`: mind-control-style channels.
        far_sight: rec.attributes_ex() & 0x2000 != 0,
        gcd_category: rec.start_recovery_category(),
        base_duration_ms: db
            .duration_ms(rec.duration_index())
            .map_or(0, |d| d.max(0) as u64),
        amplitudes: [0, 1, 2].map(|i| u64::from(rec.amplitude(i))),
    }
}

impl NpGate {
    fn attempt_of(&self, a: &CastAttempt) -> Option<(Attempt, PlayerFacts)> {
        let np = &self.0;
        let rec = np.db.spell(a.spell_id)?;
        let mut st = np.lock();
        let (origin, explicit) = if a.requeued {
            match st.pending.iter().position(|(id, ..)| *id == a.spell_id) {
                Some(i) => {
                    let (_, origin, target) = st.pending.remove(i)?;
                    (origin, target)
                }
                None => (Origin::Queue { retries: 0 }, a.target),
            }
        } else {
            // A press names no unit of its own unless it is a self-cast.
            (Origin::Press, a.target.filter(|t| Some(*t) == a.caster))
        };
        let player = st
            .mirror
            .player_fields()
            .map(|f| PlayerFacts {
                buff_capped: f.buff_capped(),
                mounted: f.u32(field::UNIT_MOUNT_DISPLAY_ID) != 0,
            })
            .unwrap_or_default();
        let attempt = Attempt {
            spell: facts(rec, &np.db),
            item: a.item,
            target: explicit,
            selection: a.target,
            cast_time_ms: u64::from(a.cast_time_ms),
            gcd_ms: u64::from(a.gcd_ms),
            now: st.ms(a.now),
            origin,
        };
        Some((attempt, player))
    }
}

impl CastGate for NpGate {
    fn attempt(&mut self, a: &CastAttempt) -> GateVerdict {
        let Some((attempt, player)) = self.attempt_of(a) else {
            return GateVerdict::Pass;
        };
        self.0.lock().engine.attempt(&attempt, player)
    }

    fn outcome(&mut self, a: &CastAttempt, outcome: CastOutcome) {
        let outcome = match outcome {
            CastOutcome::Sent => Outcome::Sent,
            CastOutcome::Refused(reason) => Outcome::Refused(reason),
            CastOutcome::Pending => Outcome::Pending,
        };
        self.0
            .lock()
            .engine
            .outcome(outcome, u64::from(a.cooldown_remaining_ms));
    }

    fn targeted_sent(&mut self, spell_id: u32, now: Instant) {
        let mut st = self.0.lock();
        let ms = st.ms(now);
        st.engine.targeted_sent(spell_id, ms);
    }
}
