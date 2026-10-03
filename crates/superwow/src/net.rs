//! `UNIT_CASTEVENT` off the server's packets, as handlers on benilla's net table that run after
//! benilla's own: `SMSG_SPELL_START` is `START` with the cast time, `SMSG_SPELL_GO` is `CAST`, or
//! `CHANNEL` with the channel's duration, a failure is `FAIL`, and each melee swing is `MAINHAND`
//! or `OFFHAND`. Arguments: caster guid, target guid, kind, spell id, duration in ms. The
//! spellbook is mirrored here too, for `CastSpellByName`.

use benilla_app::ext::{NetHandlerApp, SessionEvent, SessionEventKind as K};
use bevy::prelude::*;

use crate::Sw;

/// `HITINFO_LEFTSWING`: the off-hand swung.
const HITINFO_LEFTSWING: u32 = 0x4;

pub fn register(app: &mut App) {
    app.net_handler(K::SpellStart, on_cast_event)
        .net_handler(K::SpellGo, on_cast_event)
        .net_handler(K::SpellFailedOther, on_cast_event)
        .net_handler(K::CastResult, on_cast_event)
        .net_handler(K::AttackerState, on_cast_event)
        .net_handler(K::SpellBook, on_book)
        .net_handler(K::SpellLearned, on_book)
        .net_handler(K::SpellRemoved, on_book)
        .net_handler(K::SpellSuperceded, on_book);
}

fn on_cast_event(In(ev): In<SessionEvent>, sw: Res<Sw>) {
    let mut st = sw.lock();
    if !st.cast_events {
        return;
    }
    match ev {
        SessionEvent::SpellStart {
            caster,
            spell_id,
            cast_time_ms,
            target,
            ..
        } => st.cast_event(caster, target.unwrap_or(0), "START", spell_id, cast_time_ms),
        SessionEvent::SpellGo {
            caster,
            spell_id,
            target,
            ..
        } => {
            let (kind, ms) = match sw.spells.channel_ms(spell_id) {
                Some(ms) => ("CHANNEL", ms),
                None => ("CAST", 0),
            };
            st.cast_event(caster, target.unwrap_or(0), kind, spell_id, ms);
        }
        SessionEvent::SpellFailedOther { caster, spell_id } => {
            st.cast_event(caster, 0, "FAIL", spell_id, 0);
        }
        SessionEvent::CastResult {
            spell_id,
            success: false,
            ..
        } => {
            let me = st.player;
            st.cast_event(me, 0, "FAIL", spell_id, 0);
        }
        SessionEvent::AttackerState(a) => {
            let kind = if a.hit_info & HITINFO_LEFTSWING != 0 {
                "OFFHAND"
            } else {
                "MAINHAND"
            };
            st.cast_event(a.attacker, a.victim, kind, a.melee_spell_id, 0);
        }
        _ => {}
    }
}

fn on_book(In(ev): In<SessionEvent>, sw: Res<Sw>) {
    let mut st = sw.lock();
    let book = &mut st.book;
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
        _ => {}
    }
}
