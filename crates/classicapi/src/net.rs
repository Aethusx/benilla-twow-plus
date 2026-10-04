//! The server events the API reads, as handlers on benilla's net table that run after benilla's
//! own.

use benilla_app::ext::{NetHandlerApp, SessionEvent, SessionEventKind as K};
use bevy::prelude::*;

use crate::Ca;

pub fn register(app: &mut App) {
    app.net_handler(K::SpellBook, on_known)
        .net_handler(K::SpellLearned, on_known)
        .net_handler(K::SpellRemoved, on_known)
        .net_handler(K::SpellSuperceded, on_known)
        .net_handler(K::SpellModifier, on_spell_modifier)
        .net_handler(K::MirrorTimerStart, on_mirror_timer)
        .net_handler(K::MirrorTimerPause, on_mirror_timer)
        .net_handler(K::MirrorTimerStop, on_mirror_timer)
        .net_handler(K::SpellGo, on_spell_go)
        .net_handler(K::AttackerState, on_attacker_state)
        .net_handler(K::ChannelUpdate, on_channel_update)
        .net_handler(K::ItemTemplate, on_item_template)
        .net_handler(K::TimeSpeed, on_clock)
        .net_handler(K::ServerUnixTime, on_clock)
        .net_handler(K::SpellCooldowns, on_cooldowns)
        .net_handler(K::SpellStart, on_cast)
        .net_handler(K::SpellDelayed, on_cast)
        .net_handler(K::ChannelStart, on_cast)
        .net_handler(K::SpellFailedOther, on_cast)
        .net_handler(K::CastResult, on_cast)
        .net_handler(K::CreatureName, on_template)
        .net_handler(K::GameObjectInfo, on_template)
        .net_handler(K::QuestTemplate, on_template)
        .net_handler(K::WhoResults, on_who)
        .net_handler(K::PartyMemberStats, on_member_stats);
}

/// `SMSG_PARTY_MEMBER_STATS[_FULL]`: a member's zone, when the packet's mask carries it.
fn on_member_stats(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    if let SessionEvent::PartyMemberStats { guid, info, .. } = ev {
        if let Some(zone) = info.zone {
            ca.lock().mirror.member_zones.insert(guid, u32::from(zone));
        }
    }
}

/// `SMSG_WHO`: an answer, which may be one of `C_FriendList.SendWhoQueryByName`'s.
fn on_who(In(_): In<SessionEvent>, ca: Res<Ca>) {
    ca.lock().friends.on_who_answer();
}

/// The template answers, for `Cache::QueryLoad`'s records and loads.
fn on_template(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    use crate::templates::{Creature, GameObject};
    let mut st = ca.lock();
    match ev {
        SessionEvent::CreatureName {
            entry,
            name,
            subname,
            creature_type,
            pet_family,
            rank,
            display_id,
            ..
        } => {
            let rec = name.map(|name| Creature {
                name,
                subname: subname.unwrap_or_default(),
                creature_type: creature_type.unwrap_or(0),
                family: pet_family,
                rank,
                display_id,
            });
            st.templates.on_creature(entry, rec);
        }
        SessionEvent::GameObjectInfo {
            entry,
            type_id,
            display_id,
            name,
            ..
        } => st.templates.on_gameobject(
            entry,
            GameObject {
                name,
                type_id,
                display_id,
            },
        ),
        SessionEvent::QuestTemplate(q) => st.templates.on_quest(q),
        _ => {}
    }
}

/// The cast tracker's packets (`Spell::Cast`'s dispatch subscriber, `SpellFailed_h` for a
/// refusal); `SMSG_SPELL_GO` and `MSG_CHANNEL_UPDATE` reach it through their own handlers.
fn on_cast(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    let now = crate::cast::now_ms();
    ca.with_cast(|cast, env| match ev {
        SessionEvent::SpellStart {
            caster,
            spell_id,
            cast_time_ms,
            target,
            ..
        } => cast.on_spell_start(
            env,
            caster,
            spell_id,
            cast_time_ms,
            target.unwrap_or(0),
            now,
        ),
        SessionEvent::SpellDelayed { caster, delay_ms } => {
            cast.on_delayed(env.mirror.player, caster, delay_ms)
        }
        SessionEvent::ChannelStart {
            spell_id,
            duration_ms,
        } => cast.on_channel_start(spell_id, duration_ms, now),
        SessionEvent::SpellFailedOther { caster, spell_id } => {
            cast.on_aborted(env, caster, spell_id)
        }
        SessionEvent::CastResult {
            spell_id,
            success: false,
            reason,
            ..
        } => cast.on_failed(spell_id, reason.unwrap_or(0), now),
        _ => {}
    });
}

/// `SMSG_SPELL_COOLDOWN`: a school lockout, for `C_LossOfControl`.
fn on_cooldowns(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    if let SessionEvent::SpellCooldowns { caster, cooldowns } = ev {
        crate::lua::lossofcontrol::on_cooldowns(&ca, caster, &cooldowns);
    }
}

/// The clocks `C_DateAndTime` reads: the packed game date (`year·372 + month·31 + day`, both
/// 0-based, the year since 2000) and the server's unix time.
fn on_clock(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    let mut st = ca.lock();
    match ev {
        SessionEvent::TimeSpeed { day_serial, .. } => {
            let s = i64::from(day_serial);
            st.clocks.date = Some((s / 372, s % 372 / 31, s % 31));
        }
        SessionEvent::ServerUnixTime { unix_time } => {
            st.clocks.server = Some((unix_time, std::time::Instant::now()));
        }
        _ => {}
    }
}

/// `SMSG_ITEM_QUERY_SINGLE_RESPONSE`: the record `Item::PeekRecord` reads, or "no such item".
fn on_item_template(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    if let SessionEvent::ItemTemplate { entry, info } = ev {
        ca.items.lock().answer(entry, info.map(|b| *b));
    }
}

/// `SMSG_SPELL_GO`'s hit list, the one place an aura's caster and duration show
/// (`Aura::Source::HandleSpellGo`).
fn on_spell_go(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    let SessionEvent::SpellGo {
        caster,
        spell_id,
        hits,
        ..
    } = ev
    else {
        return;
    };
    ca.with_auras(|auras, env| auras.on_spell_go(env, caster, spell_id, &hits));
    let now = crate::cast::now_ms();
    ca.with_cast(|cast, env| cast.on_spell_go(env, caster, spell_id, now));
    ca.with_totems(|totems, env| {
        if caster == env.mirror.player {
            totems.on_player_spell_go(env, spell_id, std::time::Instant::now());
        }
    });
}

/// `Aura::JudgementRefresh`: a white swing that dealt damage refreshes the attacker's judgements
/// on the victim, as the server's `DealMeleeDamage` does without a word to the client.
fn on_attacker_state(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    let SessionEvent::AttackerState(a) = ev else {
        return;
    };
    if a.damage == 0 || a.attacker == 0 || a.victim == 0 {
        return;
    }
    ca.with_auras(|auras, env| auras.refresh_judgements(env, a.victim, a.attacker));
}

/// `MSG_CHANNEL_UPDATE`: our channel's pushback shortens each target's aura of it too.
fn on_channel_update(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    let SessionEvent::ChannelUpdate { remaining_ms } = ev else {
        return;
    };
    ca.with_auras(|auras, env| {
        let spell = env
            .mirror
            .me()
            .map_or(0, |f| f.u32(crate::mirror::field::UNIT_CHANNEL_SPELL));
        auras.restamp_player_channel(env.mirror.player, spell, remaining_ms);
    });
    let now = crate::cast::now_ms();
    ca.with_cast(|cast, _| cast.on_channel_update(remaining_ms, now));
}

/// The mirror-timer side cache the engine never keeps (`FUN_005E7990`'s three packets).
fn on_mirror_timer(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    let now = std::time::Instant::now();
    let mut st = ca.lock();
    let slot = |kind: u32| (kind < 3).then_some(kind as usize);
    match ev {
        SessionEvent::MirrorTimerStart(t) => {
            if let Some(i) = slot(t.kind) {
                st.mirror_timers[i] = Some(crate::MirrorTimer {
                    kind: t.kind,
                    value: i64::from(t.remaining_ms),
                    max: i64::from(t.duration_ms),
                    scale: i64::from(t.scale),
                    paused: t.paused,
                    spell_id: t.spell_id,
                    base: now,
                });
            }
        }
        SessionEvent::MirrorTimerPause { kind, paused } => {
            if let Some(t) = slot(kind).and_then(|i| st.mirror_timers[i].as_mut()) {
                if paused {
                    t.value = t.live(now);
                    t.paused = true;
                } else if t.paused {
                    t.paused = false;
                    t.base = now;
                }
            }
        }
        SessionEvent::MirrorTimerStop { kind } => {
            if let Some(i) = slot(kind) {
                st.mirror_timers[i] = None;
            }
        }
        _ => {}
    }
}

/// `HandleSetSpellModifier 0x6e9950`: each packet is the cell's absolute total.
fn on_spell_modifier(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    if let SessionEvent::SpellModifier {
        flat,
        mask_bit,
        op,
        value,
    } = ev
    {
        ca.lock().mods.set(flat, mask_bit, op, value);
    }
}

/// The known-spell set: the engine sets a spell's bit on learn and clears it on unlearn.
fn on_known(In(ev): In<SessionEvent>, ca: Res<Ca>) {
    let mut st = ca.lock();
    let known = &mut st.known;
    match ev {
        SessionEvent::SpellBook { spell_ids, .. } => *known = spell_ids,
        SessionEvent::SpellLearned { spell_id } => {
            if !known.contains(&spell_id) {
                known.push(spell_id);
            }
        }
        SessionEvent::SpellRemoved { spell_id } => known.retain(|id| *id != spell_id),
        SessionEvent::SpellSuperceded {
            old_spell_id,
            new_spell_id,
        } => {
            known.retain(|id| *id != old_spell_id);
            if !known.contains(&new_spell_id) {
                known.push(new_spell_id);
            }
        }
        _ => {}
    }
}
