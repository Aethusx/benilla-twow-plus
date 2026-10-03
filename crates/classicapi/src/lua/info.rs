//! Small data lookups the bundled addon reads at load:
//!
//! - `classes/Info.cpp`: `FillLocalizedClassList(table [, isFemale])`, each `ChrClasses.dbc`
//!   token to its localized name; 1.12 has one name per class, so `isFemale` changes nothing.
//! - `event/Util.cpp`: `C_EventUtils.IsEventValid(name)`, whether the event table holds the name:
//!   the 1.12 events (`reference/1.12-events.tsv`) and the ones the DLL reserves.
//! - `ui/Color.cpp`: `C_UIColor.GetColors()`. Deviation: the DLL embeds a snapshot of a later
//!   client's `GlobalColor.dbc`; benilla ships no game data, so the list is empty.

use std::collections::HashSet;
use std::sync::OnceLock;

use mlua::Value;

use crate::lua::Api;

/// `ChrClasses.dbc`: the localized name (`OFF_CHRCLASSES_NAMES` 0x14) and the token
/// (`OFF_CHRCLASSES_FILENAME` 0x38).
const CHRCLASSES_NAME: usize = 0x14 / 4;
const CHRCLASSES_TOKEN: usize = 0x38 / 4;

/// The events the DLL reserves beyond 1.12's.
const RESERVED: &[&str] = &[
    "AUCTION_MULTISELL_FAILURE",
    "AUCTION_MULTISELL_START",
    "AUCTION_MULTISELL_UPDATE",
    "BAG_NEW_ITEMS_UPDATED",
    "BAG_UPDATE_DELAYED",
    "CREATURE_DATA_LOAD_RESULT",
    "CURSOR_CHANGED",
    "EQUIPMENT_SETS_CHANGED",
    "EQUIPMENT_SWAP_FINISHED",
    "EQUIPMENT_SWAP_PENDING",
    "FACTION_STANDING_CHANGED",
    "GAMEOBJECT_DATA_LOAD_RESULT",
    "GET_ITEM_INFO_RECEIVED",
    "GLOBAL_MOUSE_DOWN",
    "GLOBAL_MOUSE_UP",
    "HEARTHSTONE_BOUND",
    "ITEM_DATA_LOAD_RESULT",
    "LEARNED_SPELL_IN_SKILL_LINE",
    "LOOT_HISTORY_FULL_UPDATE",
    "LOOT_HISTORY_ROLL_CHANGED",
    "LOOT_HISTORY_ROLL_COMPLETE",
    "LOOT_SCAN_COMPLETED",
    "LOSS_OF_CONTROL_ADDED",
    "LOSS_OF_CONTROL_UPDATE",
    "MODIFIER_STATE_CHANGED",
    "NAME_PLATE_CREATED",
    "NAME_PLATE_UNIT_ADDED",
    "NAME_PLATE_UNIT_REMOVED",
    "PLAYER_EQUIPMENT_CHANGED",
    "PLAYER_FOCUS_CHANGED",
    "PLAYER_STARTED_LOOKING",
    "PLAYER_STARTED_MOVING",
    "PLAYER_STARTED_TURNING",
    "PLAYER_STOPPED_LOOKING",
    "PLAYER_STOPPED_MOVING",
    "PLAYER_STOPPED_TURNING",
    "PLAYER_SWING",
    "PLAYER_SWING_RANGE_UPDATE",
    "PLAYER_TOTEM_UPDATE",
    "QUEST_ACCEPTED",
    "QUEST_DATA_LOAD_RESULT",
    "QUEST_REMOVED",
    "QUEST_TURNED_IN",
    "SOUNDKIT_FINISHED",
    "UNIT_SPELLCAST_CHANNEL_START",
    "UNIT_SPELLCAST_CHANNEL_STOP",
    "UNIT_SPELLCAST_CHANNEL_UPDATE",
    "UNIT_SPELLCAST_DELAYED",
    "UNIT_SPELLCAST_FAILED",
    "UNIT_SPELLCAST_FAILED_QUIET",
    "UNIT_SPELLCAST_INTERRUPTED",
    "UNIT_SPELLCAST_RETICLE_CLEAR",
    "UNIT_SPELLCAST_RETICLE_TARGET",
    "UNIT_SPELLCAST_SENT",
    "UNIT_SPELLCAST_START",
    "UNIT_SPELLCAST_STOP",
    "UNIT_SPELLCAST_SUCCEEDED",
    "UPDATE_INVENTORY_DURABILITY",
    "UPDATE_SHAPESHIFT_FORM",
    "USER_WAYPOINT_UPDATED",
    "VOICE_CHAT_TTS_PLAYBACK_FINISHED",
    "WEAPON_SLOT_CHANGED",
    "WEAR_EQUIPMENT_SET",
];

fn events() -> &'static HashSet<&'static str> {
    static EVENTS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    EVENTS.get_or_init(|| {
        include_str!("../../../../reference/1.12-events.tsv")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .filter_map(|l| l.split('\t').next())
            .chain(RESERVED.iter().copied())
            .collect()
    })
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.global(
        "FillLocalizedClassList",
        move |_, (t, _female): (Value, Value)| {
            let Value::Table(t) = t else {
                return Err(mlua::Error::runtime(
                    "Usage: FillLocalizedClassList(table [, isFemale])",
                ));
            };
            if let Some(classes) = c.db.get("ChrClasses") {
                for r in classes.rows() {
                    let (token, name) = (r.str(CHRCLASSES_TOKEN), r.loc(CHRCLASSES_NAME));
                    if !token.is_empty() && !name.is_empty() {
                        t.set(token, name)?;
                    }
                }
            }
            Ok(t)
        },
    )?;

    api.table("C_EventUtils", "IsEventValid", |_, v: Value| match v {
        Value::String(s) => Ok(events().contains(s.to_str()?.as_ref())),
        _ => Err(mlua::Error::runtime(
            "Usage: C_EventUtils.IsEventValid(eventName)",
        )),
    })?;

    api.table("C_UIColor", "GetColors", |lua, ()| lua.create_table())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;

    #[test]
    fn events_are_valid_from_the_reference_and_the_reserved_list() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                return tostring(C_EventUtils.IsEventValid("PLAYER_ENTERING_WORLD"))
                  .. tostring(C_EventUtils.IsEventValid("UNIT_SPELLCAST_START"))
                  .. tostring(C_EventUtils.IsEventValid("NOT_AN_EVENT"))
                  .. tostring(C_EventUtils.IsEventValid(""))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "truetruefalsefalse");
    }

    #[test]
    fn the_class_list_maps_tokens_to_names() {
        let _ = benilla_formats::wow_data_or_skip!();
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load("local t = FillLocalizedClassList({}) return t.WARRIOR .. ' ' .. t.MAGE")
            .eval()
            .expect("chunk");
        assert_eq!(out, "Warrior Mage");
    }
}
