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

/// The player's proficiency subclass mask for an item class (`SMSG_SET_PROFICIENCY`,
/// `0xc4d4a0[class]`); `None` before the server announced it.
pub fn proficiency_mask(lua: &Lua, item_class: u32) -> Option<u32> {
    lua.app_data_ref::<Model>()?
        .player_req
        .proficiency
        .get(&item_class)
        .copied()
}

/// An addon's `## <key>:` directive as its manifest gives it, by folder name; `None` for an
/// unknown addon or an absent directive.
pub fn addon_directive(lua: &Lua, addon: &str, key: &str) -> Option<String> {
    let model = lua.app_data_ref::<Model>()?;
    let a = model
        .addons
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(addon))?;
    a.directives
        .iter()
        .rev()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

/// Turn modern script arguments on or off: every handler also gets `(self, [event,]
/// arg1..argN)` as real arguments, beside the `this`/`event`/`argN` globals 1.12 sets. Off by
/// default; a crate on top that backports the modern calling convention turns it on.
pub fn set_modern_script_args(lua: &Lua, on: bool) {
    if let Some(mut m) = lua.app_data_mut::<Model>() {
        m.modern_script_args = on;
    }
}

/// Whether modern script arguments are on.
pub fn modern_script_args(lua: &Lua) -> bool {
    lua.app_data_ref::<Model>()
        .is_some_and(|m| m.modern_script_args)
}

/// An action-bar slot's kind byte (`0x00` spell, `0x40` macro, `0x80` item) and its spell,
/// macro or item id, 1-based; `None` for an empty slot.
pub fn action_slot(lua: &Lua, slot: u32) -> Option<(u8, u32)> {
    lua.app_data_ref::<Model>()?
        .actions
        .get(&slot)
        .map(|a| (a.kind, a.action))
}

/// What the cursor holds, as `GetCursorInfo` reports it.
#[derive(Clone, Debug, PartialEq)]
pub enum CursorView {
    /// An item picked up from a bag or the paperdoll, its link when known.
    Item {
        item_id: u32,
        link: Option<String>,
    },
    Money(u32),
    /// A spell from a book: its 1-based slot, whether the pet's book, its id.
    Spell {
        book_slot: u32,
        pet: bool,
        spell_id: u32,
    },
    /// A macro by its 1-based index.
    Macro(u32),
    /// A vendor row, 1-based.
    Merchant(u32),
    /// An action dragged off the bar: its kind byte and id.
    Action {
        kind: u8,
        action: u32,
    },
    /// Anything else (a pet action, a stabled pet).
    Other,
}

/// The cursor's payload, `None` when it holds nothing.
pub fn cursor(lua: &Lua) -> Option<CursorView> {
    use super::cursor::CursorPayload as P;
    let model = lua.app_data_ref::<Model>()?;
    Some(match model.cursor.as_ref()? {
        P::Item(i) => CursorView::Item {
            item_id: i.item_id,
            link: i.link.clone(),
        },
        P::Money(m) => CursorView::Money(m.copper),
        P::Spell(s) => CursorView::Spell {
            book_slot: s.book_slot,
            pet: s.book_type.eq_ignore_ascii_case("pet"),
            spell_id: s.spell_id,
        },
        P::Macro(m) => CursorView::Macro(m.index),
        P::Merchant(m) => CursorView::Merchant(m.row + 1),
        P::Action(a) => CursorView::Action {
            kind: a.kind,
            action: a.action,
        },
        P::PetAction(_) | P::StablePet(_) => CursorView::Other,
    })
}

/// Add a method every region answers (frames of every kind, textures, font strings, title
/// regions), for a crate on top that backports a later client's region API. An existing method
/// of the same name is left alone: the 1.12 one wins.
pub fn add_region_method(lua: &Lua, name: &str, f: mlua::Function) -> mlua::Result<()> {
    for key in [
        super::REG_FRAME_METHODS,
        super::REG_TEXTURE_METHODS,
        super::REG_FONTSTRING_METHODS,
        super::REG_TITLE_METHODS,
    ] {
        let t: mlua::Table = lua.named_registry_value(key)?;
        if t.raw_get::<mlua::Value>(name)?.is_nil() {
            t.raw_set(name, f.clone())?;
        }
    }
    Ok(())
}

/// Add a method every frame answers, whatever its kind, the 1.12 one winning as above.
pub fn add_frame_method(lua: &Lua, name: &str, f: mlua::Function) -> mlua::Result<()> {
    let t: mlua::Table = lua.named_registry_value(super::REG_FRAME_METHODS)?;
    if t.raw_get::<mlua::Value>(name)?.is_nil() {
        t.raw_set(name, f)?;
    }
    Ok(())
}

/// The frame a started drag gesture is dragging (its `OnDragStart` fired), by script id.
pub fn drag_source(lua: &Lua) -> Option<u32> {
    let model = lua.app_data_ref::<Model>()?;
    let d = model.drag.as_ref().filter(|d| d.started)?;
    model.frame_to_id.get(&d.source).copied()
}

/// A macro by 1-based index: `(name, icon texture, body)`.
pub fn macro_view(lua: &Lua, index: u32) -> Option<(String, Option<String>, String)> {
    let model = lua.app_data_ref::<Model>()?;
    let m = model.macros.get(index as usize)?;
    Some((m.name.clone(), m.texture.clone(), m.body.clone()))
}

/// A macro's cached cast as the app derived it from the body (`[rec+0x564]`).
pub fn macro_binding(lua: &Lua, index: u32) -> Option<super::MacroBinding> {
    lua.app_data_ref::<Model>()?
        .macro_bindings
        .get(&index)
        .copied()
}

/// Moves on every macro seed and edit.
pub fn macros_generation(lua: &Lua) -> u64 {
    lua.app_data_ref::<Model>()
        .map_or(0, |m| m.macros_generation)
}

/// Put `f` in front of a GameTooltip method, for a crate that backports a later client's
/// behaviour of it; the method it replaces is returned for `f` to fall through to.
pub fn replace_tooltip_method(
    lua: &Lua,
    name: &str,
    f: mlua::Function,
) -> mlua::Result<Option<mlua::Function>> {
    let t: mlua::Table = lua.named_registry_value(super::tooltip::REG_TOOLTIP_METHODS)?;
    let old: Option<mlua::Function> = t.raw_get(name)?;
    t.raw_set(name, f)?;
    Ok(old)
}

/// Make the macro runner treat `#` lines as comments, as 3.3.5's does, where 1.12's sends them on
/// (a `#showtooltip` line becomes a `/say`).
pub fn set_macro_skip_comments(lua: &Lua, on: bool) {
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        model.macro_skip_comments = on;
    }
}

/// `StopMacro`: the running macro body dispatches no further line.
pub fn request_macro_stop(lua: &Lua) {
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        model.macro_stop = true;
    }
}

/// `CreateMacro` with the icon as a texture path; the new 1-based index.
pub fn create_macro(
    lua: &Lua,
    name: &str,
    texture: Option<String>,
    body: &str,
    per_character: bool,
) -> Option<usize> {
    let mut model = lua.app_data_mut::<Model>()?;
    super::macros::create_macro(&mut model, name, texture, body, false, per_character)
}

/// `EditMacro` with the icon as a texture path; `None` fields are left alone, as is the local flag.
pub fn edit_macro(
    lua: &Lua,
    index: usize,
    name: Option<String>,
    texture: Option<String>,
    body: Option<String>,
) -> Option<usize> {
    let mut model = lua.app_data_mut::<Model>()?;
    super::macros::edit_macro(&mut model, index, name, texture.map(Some), body, None)
}

/// A macro's 1-based index by name, 0 for none (`GetMacroIndexByName`).
pub fn macro_index_by_name(lua: &Lua, name: &str) -> usize {
    lua.app_data_ref::<Model>()
        .map_or(0, |m| m.macros.index_by_name(name))
}
