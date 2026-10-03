//! Read-only views of the script model for a crate on top's natives, which run inside the VM and
//! so cannot reach the host. Each answers what the interface already shows; none changes the
//! model. A crate pins the benilla revision it builds on: these promise no stable API.

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
