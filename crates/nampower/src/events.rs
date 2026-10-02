//! Nampower's custom events: their names, and the ones read off descriptor changes (aura gains
//! and losses, deaths, the `*_GUID` unit events, and the stock unit events with a guid as the
//! token). An event fires only when some frame registered it, which is how nampower 4.5 enables
//! its gated events; the `NP_Enable*Events` toggles stay readable for older addons.

use benilla_app::ext::ScriptValue;

use crate::dbc::Databases;
use crate::engine::guid_string;
use crate::mirror::{field, Fields, UnitChange};
use crate::State;

/// Every event nampower signals, and the stock unit events it also fires with a guid token.
pub const ALL: &[&str] = &[
    "SPELL_QUEUE_EVENT",
    "SPELL_CAST_EVENT",
    "SPELL_START_SELF",
    "SPELL_START_OTHER",
    "SPELL_GO_SELF",
    "SPELL_GO_OTHER",
    "SPELL_FAILED_SELF",
    "SPELL_FAILED_OTHER",
    "SPELL_DELAYED_SELF",
    "SPELL_DELAYED_OTHER",
    "SPELL_CHANNEL_START",
    "SPELL_CHANNEL_UPDATE",
    "SPELL_DAMAGE_EVENT_SELF",
    "SPELL_DAMAGE_EVENT_OTHER",
    "BUFF_ADDED_SELF",
    "BUFF_REMOVED_SELF",
    "BUFF_ADDED_OTHER",
    "BUFF_REMOVED_OTHER",
    "DEBUFF_ADDED_SELF",
    "DEBUFF_REMOVED_SELF",
    "DEBUFF_ADDED_OTHER",
    "DEBUFF_REMOVED_OTHER",
    "BUFF_UPDATE_DURATION_SELF",
    "DEBUFF_UPDATE_DURATION_SELF",
    "AURA_CAST_ON_SELF",
    "AURA_CAST_ON_OTHER",
    "AUTO_ATTACK_SELF",
    "AUTO_ATTACK_OTHER",
    "SPELL_HEAL_BY_SELF",
    "SPELL_HEAL_BY_OTHER",
    "SPELL_HEAL_ON_SELF",
    "SPELL_ENERGIZE_BY_SELF",
    "SPELL_ENERGIZE_BY_OTHER",
    "SPELL_ENERGIZE_ON_SELF",
    "SPELL_MISS_SELF",
    "SPELL_MISS_OTHER",
    "UNIT_DIED",
    "ENVIRONMENTAL_DMG_SELF",
    "ENVIRONMENTAL_DMG_OTHER",
    "DAMAGE_SHIELD_SELF",
    "DAMAGE_SHIELD_OTHER",
    "SPELL_DISPEL_BY_SELF",
    "SPELL_DISPEL_BY_OTHER",
    "KEY_DOWN",
    "KEY_UP",
    "UNIT_COMBAT_GUID",
    "UNIT_HEALTH_GUID",
    "UNIT_MANA_GUID",
    "UNIT_RAGE_GUID",
    "UNIT_ENERGY_GUID",
    "UNIT_PET_GUID",
    "UNIT_FLAGS_GUID",
    "UNIT_AURA_GUID",
    "UNIT_DYNAMIC_FLAGS_GUID",
    "UNIT_NAME_UPDATE_GUID",
    "UNIT_PORTRAIT_UPDATE_GUID",
    "UNIT_MODEL_CHANGED_GUID",
    "UNIT_INVENTORY_CHANGED_GUID",
    "PLAYER_GUILD_UPDATE_GUID",
    "UNIT_HEALTH",
    "UNIT_MAXHEALTH",
    "UNIT_MANA",
    "UNIT_MAXMANA",
    "UNIT_RAGE",
    "UNIT_ENERGY",
    "UNIT_AURA",
    "UNIT_FLAGS",
    "UNIT_DYNAMIC_FLAGS",
    "UNIT_PET",
    "UNIT_MODEL_CHANGED",
    "UNIT_PORTRAIT_UPDATE",
];

pub fn s(v: impl Into<String>) -> ScriptValue {
    ScriptValue::Str(v.into())
}

pub fn n(v: impl Into<i64>) -> ScriptValue {
    ScriptValue::Int(v.into())
}

pub fn g(guid: u64) -> ScriptValue {
    ScriptValue::Str(guid_string(guid))
}

/// `_SELF` or `_OTHER` by whether `guid` is ours.
pub fn side(st: &State, guid: u64, self_name: &'static str, other: &'static str) -> &'static str {
    if guid != 0 && guid == st.mirror.player {
        self_name
    } else {
        other
    }
}

/// The 1-based slot `UnitBuff`/`UnitDebuff` would show raw `slot` at, counting the shown auras
/// of its half before it; 0 when the aura itself is hidden.
fn lua_slot(fields: &Fields, slot: usize, hidden: &impl Fn(u32) -> bool) -> i64 {
    let spell = fields.aura(slot);
    if spell == 0 || hidden(spell) {
        return 0;
    }
    let start = if slot < 32 { 0 } else { 32 };
    let shown = (start..slot)
        .filter(|&s| {
            let id = fields.aura(s);
            id != 0 && !hidden(id)
        })
        .count();
    shown as i64 + 1
}

fn stacks(fields: &Fields, slot: usize) -> i64 {
    i64::from(fields.byte(field::UNIT_AURA_APPLICATIONS, slot)) + 1
}

fn level(fields: &Fields, slot: usize) -> i64 {
    i64::from(fields.byte(field::UNIT_AURA_LEVELS, slot))
}

/// Events read off this frame's descriptor changes.
pub fn unit_diffs(st: &mut State, db: &Databases, changes: &[UnitChange]) {
    if changes.is_empty() {
        return;
    }
    let hidden = |id: u32| db.spell(id).is_some_and(|r| r.aura_hidden());
    let settings = st.engine.settings.clone();
    for change in changes {
        let guid = change.guid;
        let Some(new) = st.mirror.objects.get(&guid).cloned() else {
            continue;
        };
        let Some(old) = &change.old else {
            continue;
        };
        let ours = guid == st.mirror.player;
        let mut aura_changed = false;
        for slot in 0..48 {
            let (was, is) = (old.aura(slot), new.aura(slot));
            let (was_n, is_n) = (stacks(old, slot), stacks(&new, slot));
            if was == is && (is == 0 || was_n == is_n) {
                continue;
            }
            aura_changed = true;
            let buff = slot < 32;
            let (added, removed) = match (buff, ours) {
                (true, true) => ("BUFF_ADDED_SELF", "BUFF_REMOVED_SELF"),
                (true, false) => ("BUFF_ADDED_OTHER", "BUFF_REMOVED_OTHER"),
                (false, true) => ("DEBUFF_ADDED_SELF", "DEBUFF_REMOVED_SELF"),
                (false, false) => ("DEBUFF_ADDED_OTHER", "DEBUFF_REMOVED_OTHER"),
            };
            let args = |f: &Fields, spell: u32, state: i64| {
                vec![
                    g(guid),
                    n(lua_slot(f, slot, &hidden)),
                    n(spell),
                    n(stacks(f, slot)),
                    n(level(f, slot)),
                    n(slot as i64),
                    n(state),
                ]
            };
            if was == is {
                // A stack change on the same aura.
                let event = if is_n > was_n { added } else { removed };
                st.emit(event, || args(&new, is, 2));
                continue;
            }
            if was != 0 {
                st.emit(removed, || args(old, was, 1));
            }
            if is != 0 {
                st.emit(added, || args(&new, is, 0));
            }
        }
        let changed = |i: usize| old.u32(i) != new.u32(i);
        if old.u32(field::UNIT_HEALTH) > 0 && new.u32(field::UNIT_HEALTH) == 0 {
            st.emit("UNIT_DIED", || vec![g(guid)]);
        }
        let flags = st.mirror.token_flags(guid);
        let guid_args = || {
            let (player, target, mouseover, pet, party, raid) = flags;
            vec![
                g(guid),
                n(i64::from(player)),
                n(i64::from(target)),
                n(i64::from(mouseover)),
                n(i64::from(pet)),
                n(party),
                n(raid),
            ]
        };
        let health = changed(field::UNIT_HEALTH) || changed(field::UNIT_MAX_HEALTH);
        let mana = changed(field::UNIT_POWER1) || changed(field::UNIT_POWER1 + 6);
        let rage = changed(field::UNIT_POWER1 + 1);
        let energy = changed(field::UNIT_POWER1 + 3);
        let pet = old.guid(field::UNIT_SUMMON) != new.guid(field::UNIT_SUMMON);
        let flags_moved = changed(field::UNIT_FLAGS);
        let dynamic = changed(field::UNIT_DYNAMIC_FLAGS);
        let model = changed(field::UNIT_DISPLAY_ID);
        let inventory = (0..19).any(|i| changed(field::PLAYER_VISIBLE_ITEM_1 + 2 + 12 * i));
        let guild = changed(191);
        let fire = [
            (health, "UNIT_HEALTH_GUID"),
            (mana, "UNIT_MANA_GUID"),
            (rage, "UNIT_RAGE_GUID"),
            (energy, "UNIT_ENERGY_GUID"),
            (pet, "UNIT_PET_GUID"),
            (flags_moved, "UNIT_FLAGS_GUID"),
            (aura_changed, "UNIT_AURA_GUID"),
            (dynamic, "UNIT_DYNAMIC_FLAGS_GUID"),
            (model, "UNIT_MODEL_CHANGED_GUID"),
            (model, "UNIT_PORTRAIT_UPDATE_GUID"),
            (inventory, "UNIT_INVENTORY_CHANGED_GUID"),
            (guild, "PLAYER_GUILD_UPDATE_GUID"),
        ];
        for (moved, event) in fire {
            if moved {
                st.emit(event, guid_args);
            }
        }
        // The stock unit events again with the guid as the token, for addons that track units
        // by guid; the filter keeps the high-frequency ones to their `_GUID` twins.
        if settings.enable_unit_events_guid && !settings.enable_unit_events_guid_filtering {
            let token = [(health, "UNIT_HEALTH"), (health, "UNIT_MAXHEALTH")]
                .into_iter()
                .chain([
                    (mana, "UNIT_MANA"),
                    (mana, "UNIT_MAXMANA"),
                    (rage, "UNIT_RAGE"),
                    (energy, "UNIT_ENERGY"),
                    (aura_changed, "UNIT_AURA"),
                    (flags_moved, "UNIT_FLAGS"),
                    (dynamic, "UNIT_DYNAMIC_FLAGS"),
                    (pet, "UNIT_PET"),
                    (model, "UNIT_MODEL_CHANGED"),
                    (model, "UNIT_PORTRAIT_UPDATE"),
                ]);
            for (moved, event) in token {
                if moved {
                    st.emit(event, || vec![g(guid)]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lua_slot_counts_shown_auras_of_the_same_half() {
        let mut v = vec![0u32; field::UNIT_END];
        v[2] = 0x9;
        v[field::UNIT_AURA] = 10;
        v[field::UNIT_AURA + 1] = 11; // hidden
        v[field::UNIT_AURA + 3] = 12;
        v[field::UNIT_AURA + 32] = 20;
        v[field::UNIT_AURA + 33] = 21;
        let f = Fields::from_vec(v);
        let hidden = |id: u32| id == 11;
        assert_eq!(lua_slot(&f, 0, &hidden), 1);
        assert_eq!(lua_slot(&f, 1, &hidden), 0);
        assert_eq!(lua_slot(&f, 3, &hidden), 2);
        assert_eq!(lua_slot(&f, 33, &hidden), 2);
    }
}
