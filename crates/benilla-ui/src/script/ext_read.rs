//! Views of the script model for a crate on top's natives, which run inside the VM and so cannot
//! reach the host. Each answers what the interface already shows; none changes the model, except
//! [`fire_event`], which dispatches an event at once as an engine verb does, and [`ask_item`],
//! which queues the template query an uncached `GetItemInfo` makes. A crate pins the benilla
//! revision it builds on: these promise no stable API.

use mlua::Lua;

use super::model::Model;

/// The player's and the pet's books as spell ids in slot order, slot 1 first: the flat arrays
/// `GetSpellName(slot, bookType)` indexes.
pub fn spellbook_ids(lua: &Lua) -> (Vec<u32>, Vec<u32>) {
    let Some(model) = lua.app_data_ref::<Model>() else {
        return (Vec::new(), Vec::new());
    };
    (
        model.spellbook.slots.iter().map(|s| s.spell_id).collect(),
        model.pet_book.slots.iter().map(|s| s.spell_id).collect(),
    )
}

/// The book spell `name` names by `CastSpellByName`'s rule ([`super::resolve_spell_by_name`]):
/// `Name` is the highest rank, `Name(Rank N)` that rank; the player's book, then the pet's.
pub fn book_spell_by_name(lua: &Lua, name: &str) -> Option<u32> {
    let model = lua.app_data_ref::<Model>()?;
    if let Some(s) = super::resolve_spell_by_name(&model.spellbook, name) {
        return Some(s.spell_id);
    }
    let pet = super::SpellBookState {
        tabs: Vec::new(),
        slots: model.pet_book.slots.clone(),
    };
    super::resolve_spell_by_name(&pet, name).map(|s| s.spell_id)
}

/// The player's spellbook tabs in order, each with its `SkillLine.dbc` id.
pub fn spellbook_tabs(lua: &Lua) -> Vec<super::SpellTabView> {
    lua.app_data_ref::<Model>()
        .map(|m| m.spellbook.tabs.clone())
        .unwrap_or_default()
}

/// A cached item template, as `GetItemInfo` reads it; `None` while uncached.
pub fn item_template(lua: &Lua, item_id: u32) -> Option<super::ItemTemplateView> {
    lua.app_data_ref::<Model>()?
        .item_templates
        .get(&item_id)
        .cloned()
}

/// Ask the host to query an uncached template, as an uncached `GetItemInfo` does; the answer
/// arrives as a later push.
pub fn ask_item(lua: &Lua, item_id: u32) {
    if item_id == 0 {
        return;
    }
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        if !model.item_templates.contains_key(&item_id) {
            model.item_stat_asks.insert(item_id);
        }
    }
}

/// The suffix of an `ItemRandomProperties` row (`"of the Monkey"`), which `ITEM_SUFFIX_TEMPLATE`
/// joins onto an item's name; `None` for a row with none.
pub fn random_property_suffix(lua: &Lua, id: u32) -> Option<String> {
    lua.app_data_ref::<Model>()?
        .random_properties
        .get(&id)
        .map(|r| r.suffix.clone())
        .filter(|s| !s.is_empty())
}

/// `GetPetActionsUsable()` (`0x4bcf70`): the pet can act.
pub fn pet_actions_usable(lua: &Lua) -> bool {
    lua.app_data_ref::<Model>()
        .is_some_and(|m| m.pet_bar.actions_usable)
}

/// The active tracking aura's spell, `VAR_ACTIVE_TRACKING_SPELL`; `None` when none.
pub fn active_tracking_spell(lua: &Lua) -> Option<u32> {
    lua.app_data_ref::<Model>()?
        .tracking
        .as_ref()
        .map(|t| t.spell_id)
}

/// The plates bound to a unit, `(guid, frame object id)`, in the order the pool made them.
pub fn nameplates(lua: &Lua) -> Vec<(u64, u32)> {
    let Some(model) = lua.app_data_ref::<Model>() else {
        return Vec::new();
    };
    model
        .nameplates
        .live()
        .into_iter()
        .filter_map(|(guid, frame)| Some((guid, *model.frame_to_id.get(&frame)?)))
        .collect()
}

/// A frame's own Lua table by object id; nil for no live frame.
pub fn frame_value(lua: &Lua, id: u32) -> mlua::Value {
    super::ScriptValue::Object(id)
        .into_lua(lua)
        .unwrap_or(mlua::Value::Nil)
}

/// The object id of a frame table; `None` for anything else.
pub fn frame_id(value: &mlua::Value) -> Option<u32> {
    match value {
        mlua::Value::Table(t) => super::object::decode_id(t).ok(),
        _ => None,
    }
}

/// Fire `event` now, from inside a native, to every frame registered for it, as an engine verb
/// that signals an event does (`0x703f50`).
pub fn fire_event(lua: &Lua, event: &str, args: Vec<super::ScriptValue>) {
    super::tick::fire_event_into(lua, event, args);
}

/// Whether any frame registered `event` by name.
pub fn has_listeners(lua: &Lua, event: &str) -> bool {
    lua.app_data_ref::<Model>()
        .and_then(|m| m.event_to_frames.get(event).map(|f| !f.is_empty()))
        .unwrap_or(false)
}

/// The guid `token` names through the stock resolver and every installed extension, raising
/// `Unknown unit name` for a token no grammar recognises, as the stock `Unit*` verbs do
/// (`0x515970`); `Ok(None)` for a token naming nobody.
pub fn unit_guid(lua: &Lua, token: &str) -> mlua::Result<Option<u64>> {
    match lua.app_data_ref::<Model>() {
        Some(model) => model.guid_of(token),
        None => Ok(None),
    }
}

/// The unit snapshot the stock `Unit*` verbs read for `token` (an out-of-range groupmate through
/// the roster), after the same token check; `None` for nobody.
pub fn unit_state(lua: &Lua, token: &str) -> mlua::Result<Option<super::UnitState>> {
    super::unit::check_unit_token(lua, &Some(token.to_string()))?;
    Ok(lua
        .app_data_ref::<Model>()
        .and_then(|m| m.unit(token).cloned()))
}

/// Every guild roster member's name, offline ones included: what `GetGuildRosterInfo` indexes.
pub fn guild_roster_names(lua: &Lua) -> Vec<String> {
    lua.app_data_ref::<Model>()
        .map(|m| m.guild.roster.iter().map(|r| r.name.clone()).collect())
        .unwrap_or_default()
}

/// The aura list the interface shows for `guid`, `(spell id, helpful)` in its order, buffs then
/// debuffs: what `UnitBuff` walks for a unit, the roster's aura block for a groupmate out of
/// range. `None` for a guid with no list.
pub fn unit_aura_list(lua: &Lua, guid: u64) -> Option<Vec<(u32, bool)>> {
    let model = lua.app_data_ref::<Model>()?;
    model
        .unit_auras
        .get(&guid)
        .map(|l| l.iter().map(|a| (a.spell_id, a.helpful)).collect())
}

/// The `GetTime()` instant the player's aura of `spell_id` runs out, from the duration packets;
/// `None` when the player has no such aura, 0 for no timer.
pub fn player_aura_expiration(lua: &Lua, spell_id: u32) -> Option<f64> {
    let model = lua.app_data_ref::<Model>()?;
    model
        .player_auras
        .iter()
        .find(|a| a.spell_id == spell_id)
        .map(|a| a.expiration_time)
}
