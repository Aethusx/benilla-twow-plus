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
        .net_handler(K::SpellModifier, on_spell_modifier);
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
