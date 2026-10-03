//! Small data lookups the bundled addon reads at load:
//!
//! - `classes/Info.cpp`: `FillLocalizedClassList(table [, isFemale])`, each `ChrClasses.dbc`
//!   token to its localized name; 1.12 has one name per class, so `isFemale` changes nothing.
//! - `event/Util.cpp`: `C_EventUtils.IsEventValid(name)`, whether the event table holds the name:
//!   the 1.12 events (`reference/1.12-events.tsv`) and the ones the DLL reserves.
//! - `ui/Color.cpp`: `C_UIColor.GetColors()`, `{ baseTag, color }` rows. Deviation: the DLL embeds
//!   a later client's `GlobalColor.dbc`; benilla ships no game data, so each row is the stock 1.12
//!   interface's own color of that tag, read from its global when asked, and a tag 1.12 does not
//!   define has no row. The color goes through `CreateColor` when the addon has defined it.

use std::collections::HashSet;
use std::sync::OnceLock;

use mlua::Value;

use crate::lua::Api;

/// `ChrClasses.dbc`: the localized name (`OFF_CHRCLASSES_NAMES` 0x14) and the token
/// (`OFF_CHRCLASSES_FILENAME` 0x38).
const CHRCLASSES_NAME: usize = 0x14 / 4;
const CHRCLASSES_TOKEN: usize = 0x38 / 4;

/// The `GlobalColor.dbc` tags, in the DLL's order; only the names.
const COLOR_TAGS: &[&str] = &[
    "NORMAL_FONT_COLOR",
    "WHITE_FONT_COLOR",
    "HIGHLIGHT_FONT_COLOR",
    "RED_FONT_COLOR",
    "DIM_RED_FONT_COLOR",
    "DULL_RED_FONT_COLOR",
    "BLUE_FONT_COLOR",
    "GREEN_FONT_COLOR",
    "GRAY_FONT_COLOR",
    "YELLOW_FONT_COLOR",
    "LIGHTYELLOW_FONT_COLOR",
    "DARKYELLOW_FONT_COLOR",
    "ORANGE_FONT_COLOR",
    "PASSIVE_SPELL_FONT_COLOR",
    "BATTLENET_FONT_COLOR",
    "TRANSMOGRIFY_FONT_COLOR",
    "DISABLED_FONT_COLOR",
    "WARNING_FONT_COLOR",
    "BRIGHTBLUE_FONT_COLOR",
    "LIGHTBLUE_FONT_COLOR",
    "LIGHTGRAY_FONT_COLOR",
    "GOLD_FONT_COLOR",
    "PAPER_FRAME_EXPANDED_COLOR",
    "PAPER_FRAME_COLLAPSED_COLOR",
    "PAPER_FRAME_DARK_COLOR",
    "PAPER_FRAME_TITLE_COLOR",
    "PAPER_FRAME_TEXT_COLOR",
    "INVALID_EQUIPMENT_COLOR",
    "ACTIONBAR_HOTKEY_FONT_COLOR",
    "ARTIFACT_BAR_COLOR",
    "WARBOARD_OPTION_TEXT_COLOR",
    "DEFAULT_CHAT_CHANNEL_COLOR",
    "DIM_GREEN_FONT_COLOR",
    "BLACK_FONT_COLOR",
    "LINK_FONT_COLOR",
    "SEPIA_COLOR",
    "HIGHLIGHT_LIGHT_BLUE",
    "CORRUPTION_COLOR",
    "LORE_TEXT_BODY_COLOR",
    "RARE_MISSION_COLOR",
    "TUTORIAL_FONT_COLOR",
    "DARKGRAY_COLOR",
    "SCENARIO_STAGE_COLOR",
    "SCENARIO_SUBTITLE_COLOR",
    "CHALLENGE_MODE_TOAST_TITLE_COLOR",
    "TRADESKILL_EXPERIENCE_COLOR",
    "SUBSCRIPTION_INTERSTITIAL_COLOR",
    "GLUE_DIALOG_FONT_COLOR",
    "NEW_FEATURE_SHADOW_COLOR",
    "QUEST_OBJECTIVE_FONT_COLOR",
    "QUEST_OBJECTIVE_HIGHLIGHT_FONT_COLOR",
    "QUEST_OBJECTIVE_DISABLED_FONT_COLOR",
    "QUEST_OBJECTIVE_DISABLED_HIGHLIGHT_FONT_COLOR",
    "AREA_NAME_FONT_COLOR",
    "FACTION_RED_COLOR",
    "FACTION_ORANGE_COLOR",
    "FACTION_YELLOW_COLOR",
    "FACTION_GREEN_COLOR",
    "FRIENDS_BNET_NAME_COLOR",
    "FRIENDS_BNET_BACKGROUND_COLOR",
    "FRIENDS_WOW_NAME_COLOR",
    "FRIENDS_WOW_BACKGROUND_COLOR",
    "FRIENDS_GRAY_COLOR",
    "FRIENDS_OFFLINE_BACKGROUND_COLOR",
    "COMMON_GRAY_COLOR",
    "UNCOMMON_GREEN_COLOR",
    "RARE_BLUE_COLOR",
    "EPIC_PURPLE_COLOR",
    "LEGENDARY_ORANGE_COLOR",
    "ARTIFACT_GOLD_COLOR",
    "HEIRLOOM_BLUE_COLOR",
    "PLAYER_FACTION_COLOR_HORDE",
    "PLAYER_FACTION_COLOR_ALLIANCE",
    "TOOLTIP_DEFAULT_COLOR",
    "TOOLTIP_DEFAULT_BACKGROUND_COLOR",
    "KYRIAN_BLUE_COLOR",
    "VENTHYR_RED_COLOR",
    "NIGHT_FAE_BLUE_COLOR",
    "NECROLORD_GREEN_COLOR",
    "ADVENTURES_HEALING_GREEN",
    "ADVENTURES_BUFF_BLUE",
    "ADVENTURES_COMBAT_LOG_GREY",
    "ADVENTURES_COMBAT_LOG_BLUE",
    "ADVENTURES_COMBAT_LOG_ORANGE",
    "ADVENTURES_COMBAT_LOG_YELLOW",
    "ENCOUNTER_JOURNAL_SCROLL_BAR_BACKGROUND_COLOR",
    "RUNEFORGE_LEGEDARY_SPEC_COLOR",
    "SOULBIND_CONDUIT_ENHANCED_COLOR",
    "VERY_DARK_GRAY_COLOR",
    "VERY_LIGHT_GRAY_COLOR",
    "PURE_GREEN_COLOR",
    "PURE_RED_COLOR",
    "TRIVIAL_DIFFICULTY_COLOR",
    "EASY_DIFFICULTY_COLOR",
    "FAIR_DIFFICULTY_COLOR",
    "DIFFICULT_DIFFICULTY_COLOR",
    "IMPOSSIBLE_DIFFICULTY_COLOR",
    "ACHIEVEMENT_INCOMPLETE_COLOR",
    "ACHIEVEMENT_COMPLETE_COLOR",
    "NOT_ON_THREAT_COLOR",
    "NO_THREAT_COLOR",
    "YELLOW_THREAT_COLOR",
    "ORANGE_THREAT_COLOR",
    "RED_THREAT_COLOR",
    "ITEM_POOR_COLOR",
    "ITEM_STANDARD_COLOR",
    "ITEM_GOOD_COLOR",
    "ITEM_WOW_TOKEN_COLOR",
    "ITEM_EPIC_COLOR",
    "ITEM_LEGENDARY_COLOR",
    "ITEM_ARTIFACT_COLOR",
    "ITEM_SCALING_STAT_COLOR",
    "ITEM_SUPERIOR_COLOR",
    "ERROR_COLOR",
    "CONTEXT_FEEDBACK_COLOR",
    "FRIENDLY_STATUS_COLOR",
    "NEUTRAL_STATUS_COLOR",
    "HOSTILE_STATUS_COLOR",
    "SET_ITEM_COLOR",
    "INCREASE_STAT_COLOR",
    "DECREASE_STAT_COLOR",
    "GLYPH_LINK_COLOR",
    "TOY_LINK_COLOR",
    "AZERITE_ESSENCE_COLOR",
    "SPELL_LINK_COLOR",
    "EJ_SPELL_COLOR",
    "ENCHANT_COLOR",
    "TALENT_LINK_COLOR",
    "INSTANCE_LOCK_LINK_COLOR",
    "JOURNAL_LINK_COLOR",
    "BATTLEPET_ABILITY_LINK_COLOR",
    "ENHANCED_CONDUIT_COLOR",
    "SPELL_SUBTEXT_COLOR",
    "TRANSMOGRIFY_COLOR",
    "BIND_TRADE_TOOLTIP_COLOR",
    "USED_IN_TRADESKILL_COLOR",
    "REFUND_TOOLTIP_COLOR",
    "AZERITE_SUBTEXT_COLOR",
    "FRAMESTACK_FRAME_COLOR",
    "FRAMESTACK_HIDDEN_COLOR",
    "FRAMESTACK_REGION_COLOR",
    "ACHIEVEMENT_COLOR",
    "FRIENDS_BROADCAST_TIME_COLOR",
    "FRIENDS_OTHER_NAME_COLOR",
    "DEFAULT_MATERIAL_TEXT_COLOR",
    "DEFAULT_MATERIAL_TITLETEXT_COLOR",
    "STONE_MATERIAL_TITLETEXT_COLOR",
    "STONE_MATERIAL_TEXT_COLOR",
    "PARCHMENT_MATERIAL_TEXT_COLOR",
    "PARCHMENT_MATERIAL_TITLETEXT_COLOR",
    "MARBLE_MATERIAL_TEXT_COLOR",
    "MARBLE_MATERIAL_TITLETEXT_COLOR",
    "SILVER_MATERIAL_TITLETEXT_COLOR",
    "BRONZE_MATERIAL_TITLETEXT_COLOR",
    "SILVER_MATERIAL_TEXT_COLOR",
    "BRONZE_MATERIAL_TEXT_COLOR",
    "PARCHMENTLARGE_MATERIAL_TEXT_COLOR",
    "PARCHMENTLARGE_MATERIAL_TITLETEXT_COLOR",
    "PROGENITOR_MATERIAL_TEXT_COLOR",
    "PROGENITOR_MATERIAL_TITLETEXT_COLOR",
    "ARENA_NAME_FONT_COLOR",
    "AREA_DESCRIPTION_FONT_COLOR",
    "INVASION_FONT_COLOR",
    "INVASION_DESCRIPTION_FONT_COLOR",
    "INACTIVE_COLOR",
    "EDIT_MODE_LAYOUT_LINK_COLOR",
    "LIGHTGREEN_FONT_COLOR",
    "EDIT_MODE_GRID_LINE_COLOR",
    "EDIT_MODE_GRID_CENTER_LINE_COLOR",
    "PANEL_BACKGROUND_COLOR",
    "HEALTHBAR_MY_HEAL_PREDICTION_COLOR",
    "HEALTHBAR_OTHER_HEAL_PREDICTION_COLOR",
    "CINEMATIC_SUBTITLES_BLACK_BACKGROUND_COLOR",
    "CINEMATIC_SUBTITLES_LIGHT_BACKGROUND_COLOR",
    "HEALTHBAR_MY_HEAL_PREDICTION_GRADIENT_COLOR1",
    "HEALTHBAR_MY_HEAL_PREDICTION_GRADIENT_COLOR2",
    "HEALTHBAR_OTHER_HEAL_PREDICTION_GRADIENT_COLOR1",
    "HEALTHBAR_OTHER_HEAL_PREDICTION_GRADIENT_COLOR2",
    "DEBUFF_TYPE_MAGIC_COLOR",
    "DEBUFF_TYPE_POISON_COLOR",
    "DEBUFF_TYPE_CURSE_COLOR",
    "DEBUFF_TYPE_BLEED_COLOR",
    "DEBUFF_TYPE_DISEASE_COLOR",
    "DEBUFF_TYPE_NONE_COLOR",
    "DEBUFF_TYPE_ENRAGE_COLOR",
    "VISITABLE_URL_DEFAULT_CHAT_LINK_COLOR",
    "EVENTTRACE_SECRET_COLOR",
];

/// A color global as `(r, g, b, a)`: a table with numeric `r`, `g` and `b`, alpha 1 when absent.
fn color_of(v: &Value) -> Option<(f64, f64, f64, f64)> {
    let Value::Table(t) = v else {
        return None;
    };
    let n = |k: &str| -> Option<f64> {
        let v: Value = t.raw_get(k).ok()?;
        crate::lua::is_number(&v).then(|| crate::lua::to_number(&v))
    };
    Some((n("r")?, n("g")?, n("b")?, n("a").unwrap_or(1.0)))
}

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

    api.table("C_UIColor", "GetColors", |lua, ()| {
        let g = lua.globals();
        let create: Option<mlua::Function> = g.get("CreateColor").ok();
        let rows = lua.create_table()?;
        for tag in COLOR_TAGS {
            let Some((r, gr, b, a)) = color_of(&g.get::<Value>(*tag)?) else {
                continue;
            };
            let plain = || -> mlua::Result<Value> {
                let t = lua.create_table()?;
                t.set("r", r)?;
                t.set("g", gr)?;
                t.set("b", b)?;
                t.set("a", a)?;
                Ok(Value::Table(t))
            };
            let color = match &create {
                Some(f) => match f.call::<Value>((r, gr, b, a)) {
                    Ok(v @ Value::Table(_)) => v,
                    _ => plain()?,
                },
                None => plain()?,
            };
            let row = lua.create_table()?;
            row.set("baseTag", *tag)?;
            row.set("color", color)?;
            rows.raw_set(rows.raw_len() + 1, row)?;
        }
        Ok(rows)
    })?;
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
    fn colors_come_from_the_interface_globals_of_known_tags() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                NORMAL_FONT_COLOR = { r = 1.0, g = 0.82, b = 0 }
                RED_FONT_COLOR = "not a color"
                NOT_A_TAG = { r = 0, g = 0, b = 0 }
                local rows = C_UIColor.GetColors()
                local first = rows[1]
                return table.getn(rows) .. " " .. first.baseTag .. " " .. first.color.g .. " " .. first.color.a
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "1 NORMAL_FONT_COLOR 0.82 1");
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
