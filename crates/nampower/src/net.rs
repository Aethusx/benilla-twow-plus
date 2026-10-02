//! The server events nampower reads, as handlers on benilla's net table, which run after
//! benilla's own for the same packet: the cast verdicts, starts, launches, pushback and channel
//! timing the engine keeps time by, and the combat log its events report.

use std::time::Instant;

use benilla_app::ext::{ExtView, NetHandlerApp, SessionEvent, SessionEventKind as K};
use benilla_protocol::messages::PeriodicTick;
use bevy::prelude::*;

use crate::dbc::consts;
use crate::events::{g, n, s, side};
use crate::gate::facts;
use crate::mirror::field;
use crate::Np;

pub fn register(app: &mut App) {
    app.net_handler(K::CastResult, on_cast_result)
        .net_handler(K::SpellFailedOther, on_spell_failed_other)
        .net_handler(K::SpellStart, on_spell_start)
        .net_handler(K::SpellGo, on_spell_go)
        .net_handler(K::SpellDelayed, on_spell_delayed)
        .net_handler(K::ChannelStart, on_channel_start)
        .net_handler(K::ChannelUpdate, on_channel_update)
        .net_handler(K::SpellDamageLog, on_combat_log)
        .net_handler(K::PeriodicAuraLog, on_combat_log)
        .net_handler(K::SpellHealLog, on_combat_log)
        .net_handler(K::SpellEnergizeLog, on_combat_log)
        .net_handler(K::SpellLogMiss, on_combat_log)
        .net_handler(K::ProcResist, on_combat_log)
        .net_handler(K::SpellOrDamageImmune, on_combat_log)
        .net_handler(K::DamageShield, on_combat_log)
        .net_handler(K::EnvironmentalDamageLog, on_combat_log)
        .net_handler(K::SpellDispelLog, on_combat_log)
        .net_handler(K::AttackerState, on_combat_log)
        .net_handler(K::AuraDuration, on_aura_duration)
        .net_handler(K::SpellBook, on_book)
        .net_handler(K::SpellLearned, on_book)
        .net_handler(K::SpellRemoved, on_book)
        .net_handler(K::SpellSuperceded, on_book)
        .net_handler(K::PetSpells, on_book)
        .net_handler(K::SpellModifier, on_book)
        .net_handler(K::ItemTemplate, on_item_template)
        .net_handler(K::QuestDetail, on_quest_dialog)
        .net_handler(K::QuestProgress, on_quest_dialog)
        .net_handler(K::QuestOffer, on_quest_dialog)
        .net_handler(K::QuestComplete, on_quest_dialog);
}

/// `SMSG_CAST_RESULT`: the history verdict and, for a failure, the client's failure path.
fn on_cast_result(In(ev): In<SessionEvent>, np: Res<Np>, view: ExtView) {
    let SessionEvent::CastResult {
        spell_id,
        success,
        reason,
        ..
    } = ev
    else {
        return;
    };
    let now = Instant::now();
    let cooldown = view.cooldown(spell_id, 0, now).remaining_ms;
    let mut st = np.lock();
    let ms = st.ms(now);
    st.engine.cast_result(spell_id, success, ms);
    if !success {
        let item_cooldown = st
            .engine
            .history
            .find_spell(spell_id)
            .and_then(|p| p.item)
            .map_or(0, |i| view.cooldown(spell_id, i.entry, now).remaining_ms);
        st.engine.spell_failed(
            spell_id,
            reason.unwrap_or(0),
            true,
            ms,
            u64::from(cooldown),
            u64::from(item_cooldown),
        );
    }
}

fn on_spell_failed_other(In(ev): In<SessionEvent>, np: Res<Np>) {
    let SessionEvent::SpellFailedOther { caster, spell_id } = ev else {
        return;
    };
    np.lock()
        .emit("SPELL_FAILED_OTHER", || vec![g(caster), n(spell_id)]);
}

/// `SpellType`: 0 normal, 1 channeling, 2 auto-repeating.
fn spell_type(np: &Np, spell_id: u32) -> i64 {
    match np.db.spell(spell_id) {
        Some(r) if r.auto_repeat() => 2,
        Some(r) if r.channeled() => 1,
        _ => 0,
    }
}

fn on_spell_start(In(ev): In<SessionEvent>, np: Res<Np>, view: ExtView) {
    let SessionEvent::SpellStart {
        caster,
        spell_id,
        cast_flags,
        cast_time_ms,
        target,
        ..
    } = ev
    else {
        return;
    };
    let mut st = np.lock();
    let ours = caster == st.mirror.player && caster != 0;
    if ours {
        if let Some(rec) = np.db.spell(spell_id) {
            let gcd = view
                .spell(spell_id)
                .map_or(u64::from(rec.start_recovery_time()), |d| {
                    u64::from(d.start_recovery_ms)
                });
            st.engine
                .spell_start_self(facts(rec, &np.db), u64::from(cast_time_ms), gcd);
        }
    }
    let event = side(&st, caster, "SPELL_START_SELF", "SPELL_START_OTHER");
    let kind = spell_type(&np, spell_id);
    let duration = if kind == 1 {
        np.db
            .spell(spell_id)
            .and_then(|r| np.db.duration_ms(r.duration_index()))
            .unwrap_or(0)
            .max(0)
    } else {
        0
    };
    st.emit(event, || {
        vec![
            n(0),
            n(spell_id),
            g(caster),
            g(target.unwrap_or(0)),
            n(cast_flags),
            n(cast_time_ms),
            n(duration),
            n(kind),
            benilla_app::ext::ScriptValue::Nil,
        ]
    });
}

fn on_spell_go(In(ev): In<SessionEvent>, np: Res<Np>) {
    let SessionEvent::SpellGo {
        caster,
        spell_id,
        cast_flags,
        hits,
        misses,
        target,
        item_caster,
        ..
    } = ev
    else {
        return;
    };
    let mut st = np.lock();
    let ours = caster != 0 && caster == st.mirror.player;
    let miss_event = side(&st, caster, "SPELL_MISS_SELF", "SPELL_MISS_OTHER");
    for (victim, info) in &misses {
        st.emit(miss_event, || {
            vec![g(caster), g(*victim), n(spell_id), n(*info)]
        });
    }
    let item_id = item_caster
        .and_then(|guid| st.mirror.objects.get(&guid))
        .map_or(0, |f| f.u32(field::OBJECT_ENTRY));
    let event = side(&st, caster, "SPELL_GO_SELF", "SPELL_GO_OTHER");
    st.emit(event, || {
        vec![
            n(item_id),
            n(spell_id),
            g(caster),
            g(target.unwrap_or(0)),
            n(cast_flags),
            n(hits.len() as i64),
            n(misses.len() as i64),
            benilla_app::ext::ScriptValue::Nil,
        ]
    });
    let Some(rec) = np.db.spell(spell_id) else {
        return;
    };
    if ours {
        let ms = st.now_ms();
        st.engine.spell_go_self(facts(rec, &np.db), ms);
    }
    let aura_event = |target: u64, player: u64| {
        if target == player && target != 0 {
            "AURA_CAST_ON_SELF"
        } else {
            "AURA_CAST_ON_OTHER"
        }
    };
    if rec.applies_aura() && (st.wants("AURA_CAST_ON_SELF") || st.wants("AURA_CAST_ON_OTHER")) {
        let player = st.mirror.player;
        let duration = np.db.duration_ms(rec.duration_index()).unwrap_or(0).max(0);
        for i in 0..3 {
            let effect = rec.effect(i);
            if !matches!(
                effect,
                consts::EFFECT_APPLY_AURA
                    | consts::EFFECT_APPLY_AREA_AURA_PARTY
                    | consts::EFFECT_APPLY_AREA_AURA_RAID
                    | consts::EFFECT_APPLY_AREA_AURA_FRIEND
                    | consts::EFFECT_APPLY_AREA_AURA_ENEMY
                    | consts::EFFECT_APPLY_AREA_AURA_PET
            ) {
                continue;
            }
            // `TARGET_UNIT_CASTER` lands on the caster; otherwise every target hit, or the
            // first for a single-target spell, or none.
            let a = rec.implicit_target_a(i);
            let on: Vec<u64> = if a == 1 {
                vec![caster]
            } else if hits.is_empty() || a == 0 {
                vec![0]
            } else if matches!(a, 5 | 6 | 21 | 25 | 27 | 57 | 60) {
                vec![hits[0]]
            } else {
                hits.clone()
            };
            for t in on {
                let event = aura_event(t, player);
                let cap = st.mirror.objects.get(&t).map_or(0, |f| {
                    i64::from(f.buff_capped()) | (i64::from(f.debuff_capped()) << 1)
                });
                st.emit(event, || {
                    vec![
                        n(spell_id),
                        g(caster),
                        g(t),
                        n(effect),
                        n(rec.apply_aura_name(i)),
                        n(rec.amplitude(i)),
                        n(rec.misc_value(i)),
                        n(duration),
                        n(cap),
                    ]
                });
            }
        }
    }
}

fn on_spell_delayed(In(ev): In<SessionEvent>, np: Res<Np>) {
    let SessionEvent::SpellDelayed { caster, delay_ms } = ev else {
        return;
    };
    let mut st = np.lock();
    if caster == st.mirror.player {
        let ms = st.now_ms();
        st.engine.spell_delayed_self(u64::from(delay_ms), ms);
    }
    let event = side(&st, caster, "SPELL_DELAYED_SELF", "SPELL_DELAYED_OTHER");
    st.emit(event, || vec![g(caster), n(delay_ms)]);
}

fn on_channel_start(In(ev): In<SessionEvent>, np: Res<Np>) {
    let SessionEvent::ChannelStart {
        spell_id,
        duration_ms,
    } = ev
    else {
        return;
    };
    let mut st = np.lock();
    let ms = st.now_ms();
    let spell = np.db.spell(spell_id).map(|r| facts(r, &np.db));
    st.engine.channel_start(spell, u64::from(duration_ms), ms);
    let target = st
        .mirror
        .player_fields()
        .map_or(0, |f| f.guid(field::UNIT_TARGET));
    st.emit("SPELL_CHANNEL_START", || {
        vec![n(spell_id), g(target), n(duration_ms)]
    });
}

fn on_channel_update(In(ev): In<SessionEvent>, np: Res<Np>) {
    let SessionEvent::ChannelUpdate { remaining_ms } = ev else {
        return;
    };
    let mut st = np.lock();
    let spell_id = st.engine.cast.channel_spell_id;
    let target = st
        .mirror
        .player_fields()
        .map_or(0, |f| f.guid(field::UNIT_CHANNEL_OBJECT));
    st.emit("SPELL_CHANNEL_UPDATE", || {
        vec![n(spell_id), g(target), n(remaining_ms)]
    });
    let ms = st.now_ms();
    st.engine.channel_update(u64::from(remaining_ms), ms);
}

/// The combat log: spell damage, periodic ticks, heals, energizes, misses, shields,
/// environmental damage, dispels and melee rounds.
fn on_combat_log(In(ev): In<SessionEvent>, np: Res<Np>) {
    let mut st = np.lock();
    let effects = |spell_id: u32, aura: u32| {
        let e = np
            .db
            .spell(spell_id)
            .map_or([0; 3], |r| [r.effect(0), r.effect(1), r.effect(2)]);
        s(format!("{},{},{},{aura}", e[0], e[1], e[2]))
    };
    match ev {
        SessionEvent::SpellDamageLog(d) => {
            let event = side(
                &st,
                d.attacker,
                "SPELL_DAMAGE_EVENT_SELF",
                "SPELL_DAMAGE_EVENT_OTHER",
            );
            st.emit(event, || {
                vec![
                    g(d.target),
                    g(d.attacker),
                    n(d.spell_id),
                    n(d.damage),
                    s(format!("{},{},{}", d.absorb, d.blocked, d.resist)),
                    n(d.hit_info),
                    n(d.school),
                    effects(d.spell_id, 0),
                ]
            });
        }
        SessionEvent::PeriodicAuraLog(p) => {
            for tick in &p.ticks {
                match *tick {
                    PeriodicTick::Damage {
                        amount,
                        school,
                        absorb,
                        resist,
                    } => {
                        let event = side(
                            &st,
                            p.caster,
                            "SPELL_DAMAGE_EVENT_SELF",
                            "SPELL_DAMAGE_EVENT_OTHER",
                        );
                        st.emit(event, || {
                            vec![
                                g(p.target),
                                g(p.caster),
                                n(p.spell_id),
                                n(amount),
                                s(format!("{absorb},0,{resist}")),
                                n(0),
                                n(school),
                                effects(p.spell_id, 3),
                            ]
                        });
                    }
                    PeriodicTick::Heal { amount } => {
                        heal(&mut st, p.target, p.caster, p.spell_id, amount, false, true)
                    }
                    PeriodicTick::Energize { power, amount } => {
                        energize(&mut st, p.target, p.caster, p.spell_id, power, amount, true)
                    }
                    PeriodicTick::ManaLeech { .. } => {}
                }
            }
        }
        SessionEvent::SpellHealLog(h) => heal(
            &mut st, h.target, h.healer, h.spell_id, h.amount, h.critical, false,
        ),
        SessionEvent::SpellEnergizeLog(e) => energize(
            &mut st, e.target, e.caster, e.spell_id, e.power, e.amount, false,
        ),
        SessionEvent::SpellLogMiss(m) => {
            let event = side(&st, m.caster, "SPELL_MISS_SELF", "SPELL_MISS_OTHER");
            for (target, info) in m.misses {
                st.emit(event, || {
                    vec![g(m.caster), g(target), n(m.spell_id), n(info)]
                });
            }
        }
        // `SMSG_PROCRESIST` is always a resist (2), `SMSG_SPELLORDAMAGE_IMMUNE` an immune (7).
        SessionEvent::ProcResist(o) => miss(&mut st, o.caster, o.target, o.spell_id, 2),
        SessionEvent::SpellOrDamageImmune(o) => miss(&mut st, o.caster, o.target, o.spell_id, 7),
        SessionEvent::DamageShield(d) => {
            let event = side(&st, d.victim, "DAMAGE_SHIELD_SELF", "DAMAGE_SHIELD_OTHER");
            st.emit(event, || {
                vec![g(d.victim), g(d.attacker), n(d.damage), n(d.school)]
            });
        }
        SessionEvent::EnvironmentalDamageLog(e) => {
            let event = side(
                &st,
                e.victim,
                "ENVIRONMENTAL_DMG_SELF",
                "ENVIRONMENTAL_DMG_OTHER",
            );
            st.emit(event, || {
                vec![
                    g(e.victim),
                    n(e.damage_type),
                    n(e.damage),
                    n(e.absorb),
                    n(e.resist),
                ]
            });
        }
        SessionEvent::SpellDispelLog(d) => {
            let event = side(
                &st,
                d.caster,
                "SPELL_DISPEL_BY_SELF",
                "SPELL_DISPEL_BY_OTHER",
            );
            for spell_id in d.spell_ids {
                st.emit(event, || vec![g(d.caster), g(d.victim), n(spell_id)]);
            }
        }
        SessionEvent::AttackerState(a) => {
            let event = side(&st, a.attacker, "AUTO_ATTACK_SELF", "AUTO_ATTACK_OTHER");
            st.emit(event, || {
                vec![
                    g(a.attacker),
                    g(a.victim),
                    n(a.damage),
                    n(a.hit_info),
                    n(a.victim_state),
                    n(1),
                    n(a.blocked),
                    n(a.absorb),
                    n(a.resist),
                ]
            });
        }
        _ => {}
    }
}

fn miss(st: &mut crate::State, caster: u64, target: u64, spell_id: u32, info: u8) {
    let event = side(st, caster, "SPELL_MISS_SELF", "SPELL_MISS_OTHER");
    st.emit(event, || vec![g(caster), g(target), n(spell_id), n(info)]);
}

fn heal(
    st: &mut crate::State,
    target: u64,
    caster: u64,
    spell_id: u32,
    amount: u32,
    critical: bool,
    periodic: bool,
) {
    let args = || {
        vec![
            g(target),
            g(caster),
            n(spell_id),
            n(amount),
            n(i64::from(critical)),
            n(i64::from(periodic)),
        ]
    };
    let by = side(st, caster, "SPELL_HEAL_BY_SELF", "SPELL_HEAL_BY_OTHER");
    st.emit(by, args);
    if target == st.mirror.player {
        st.emit("SPELL_HEAL_ON_SELF", args);
    }
}

fn energize(
    st: &mut crate::State,
    target: u64,
    caster: u64,
    spell_id: u32,
    power: u32,
    amount: u32,
    periodic: bool,
) {
    let args = || {
        vec![
            g(target),
            g(caster),
            n(spell_id),
            n(power as i32),
            n(amount),
            n(i64::from(periodic)),
        ]
    };
    let by = side(
        st,
        caster,
        "SPELL_ENERGIZE_BY_SELF",
        "SPELL_ENERGIZE_BY_OTHER",
    );
    st.emit(by, args);
    if target == st.mirror.player {
        st.emit("SPELL_ENERGIZE_ON_SELF", args);
    }
}

/// `SMSG_UPDATE_AURA_DURATION`: our aura's expiry, and the duration events.
fn on_aura_duration(In(ev): In<SessionEvent>, np: Res<Np>) {
    let SessionEvent::AuraDuration { slot, remaining_ms } = ev else {
        return;
    };
    let mut st = np.lock();
    let ms = st.now_ms();
    let i = usize::from(slot);
    if i >= 48 {
        return;
    }
    st.mirror.aura_expiry[i] = if remaining_ms > 0 {
        ms + u64::from(remaining_ms)
    } else {
        0
    };
    let spell_id = st.mirror.player_fields().map_or(0, |f| f.aura(i));
    let expiry = st.mirror.aura_expiry[i];
    let event = if i < 32 {
        "BUFF_UPDATE_DURATION_SELF"
    } else {
        "DEBUFF_UPDATE_DURATION_SELF"
    };
    st.emit(event, || {
        vec![n(i as i64), n(remaining_ms), n(expiry as i64), n(spell_id)]
    });
}

/// The spellbook and the pet's, as `GetSpellIdForName` and the book lookups read them.
fn on_book(In(ev): In<SessionEvent>, np: Res<Np>) {
    let mut st = np.lock();
    st.mirror.mods_dirty = true;
    let book = &mut st.mirror.book;
    match ev {
        SessionEvent::SpellBook { spell_ids, .. } => *book = spell_ids,
        SessionEvent::SpellLearned { spell_id } => {
            if !book.contains(&spell_id) {
                book.push(spell_id);
            }
        }
        SessionEvent::SpellRemoved { spell_id } => book.retain(|id| *id != spell_id),
        SessionEvent::SpellSuperceded {
            old_spell_id,
            new_spell_id,
        } => {
            for id in book.iter_mut() {
                if *id == old_spell_id {
                    *id = new_spell_id;
                }
            }
        }
        SessionEvent::PetSpells(p) => {
            st.mirror.pet_book = p
                .spells
                .iter()
                .map(|e| e.action())
                .filter(|id| *id != 0)
                .collect();
        }
        _ => {}
    }
}

fn on_item_template(In(ev): In<SessionEvent>, np: Res<Np>) {
    let SessionEvent::ItemTemplate {
        entry,
        info: Some(info),
    } = ev
    else {
        return;
    };
    np.lock().mirror.items.insert(entry, *info);
}

fn on_quest_dialog(In(ev): In<SessionEvent>, np: Res<Np>) {
    let quest = match ev {
        SessionEvent::QuestDetail(q) => q.quest_id,
        SessionEvent::QuestProgress(q) => q.quest_id,
        SessionEvent::QuestOffer(q) => q.quest_id,
        SessionEvent::QuestComplete(_) => {
            np.lock().mirror.quest_dialog = None;
            return;
        }
        _ => return,
    };
    np.lock().mirror.quest_dialog = Some(quest);
}
