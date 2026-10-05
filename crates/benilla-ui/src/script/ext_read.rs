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

/// The item enclosed in an open inbox row (1-based): its template entry, `None` for no item.
pub fn inbox_item(lua: &Lua, row: usize) -> Option<u32> {
    let model = lua.app_data_ref::<Model>()?;
    let mail = model.mail.as_ref()?;
    mail.inbox
        .get(row.checked_sub(1)?)
        .map(|r| r.item_id)
        .filter(|id| *id != 0)
}

/// The item in the auction sell slot: `(entry, link)`.
pub fn auction_sell_item(lua: &Lua) -> Option<(u32, Option<String>)> {
    let model = lua.app_data_ref::<Model>()?;
    let it = model.auction_sell_item.as_ref()?;
    Some((it.item_id, it.link.clone()))
}

/// The item attached to the outgoing mail: `(entry, link)`.
pub fn send_mail_item(lua: &Lua) -> Option<(u32, Option<String>)> {
    let model = lua.app_data_ref::<Model>()?;
    let it = model.mail_send_item.as_ref()?;
    Some((it.item_id, it.link.clone()))
}

/// A CVar as the registry holds it, `(value, default, read_only)`; `None` for an unknown name,
/// with no warning (a predicate's question, not a script's mistake).
pub fn cvar(lua: &Lua, name: &str) -> Option<(String, String, bool)> {
    let model = lua.app_data_ref::<Model>()?;
    let key = name.to_ascii_lowercase();
    let slot = model.cvars.get(&key)?;
    Some((
        slot.value.clone(),
        slot.default.clone(),
        model.cvars_read_only.contains(&key),
    ))
}

/// Mark a CVar's value as this session's alone, or lift the mark: the host keeps saving the value
/// the file holds, as a session-owned row, while the mark stands.
pub fn mark_cvar_temporary(lua: &Lua, name: &str, temporary: bool) {
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        model.cvar_temp_marks.push((name.to_string(), temporary));
    }
}

/// The open gossip menu.
pub fn gossip_menu(lua: &Lua) -> Option<super::GossipMenu> {
    lua.app_data_ref::<Model>()?.gossip.clone()
}

/// A greeting panel's rows, each with its title.
pub type GreetingRows = Vec<(String, super::quest::GreetingQuest)>;

/// The open questgiver greeting panel: `(greeting, active rows, available rows)`.
pub fn quest_greeting(lua: &Lua) -> Option<(String, GreetingRows, GreetingRows)> {
    let model = lua.app_data_ref::<Model>()?;
    let q = model.quest.as_ref()?;
    if q.panel != super::quest::QuestPanel::Greeting {
        return None;
    }
    let zip = |titles: &[String], rows: &[super::quest::GreetingQuest]| {
        titles.iter().cloned().zip(rows.iter().copied()).collect()
    };
    Some((
        q.greeting.clone(),
        zip(&q.active_titles, &q.active_quests),
        zip(&q.available_titles, &q.available_quests),
    ))
}

/// The reputation snapshot: every faction the player has a slot for, and the watched slot.
pub fn reputation(lua: &Lua) -> Option<super::ReputationState> {
    Some(lua.app_data_ref::<Model>()?.reputation.clone())
}

/// The reputation pane's visible row at a 1-based index.
pub fn reputation_row(lua: &Lua, index: usize) -> Option<super::reputation::VisibleRow> {
    let model = lua.app_data_ref::<Model>()?;
    super::reputation::row_at(&model, index)
}

/// Whether the header keyed by this `Faction.dbc` id is folded; `None` for no such header.
pub fn reputation_header_collapsed(lua: &Lua, faction_id: u32) -> Option<bool> {
    let model = lua.app_data_ref::<Model>()?;
    super::reputation::header_collapsed(&model, faction_id)
}

/// `FactionToggleAtWar` for a reputation slot rather than a visible index.
pub fn toggle_faction_at_war(lua: &Lua, slot: u32) {
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        super::reputation::toggle_at_war(&mut model, slot);
    }
}

/// `SetFactionInactive` / `SetFactionActive` for a reputation slot.
pub fn set_faction_inactive(lua: &Lua, slot: u32, inactive: bool) {
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        super::reputation::set_inactive(&mut model, slot, inactive);
    }
}

/// `SetWatchedFactionIndex` for a reputation slot, `None` to watch nothing.
pub fn watch_faction(lua: &Lua, slot: Option<u32>) {
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        model
            .reputation_sends
            .push(super::ReputationSend::Watch(slot));
    }
}

/// `SetSelectedFaction` for a reputation slot, `None` to clear it.
pub fn select_faction(lua: &Lua, slot: Option<u32>) {
    if let Some(mut model) = lua.app_data_mut::<Model>() {
        model.reputation_selected = slot;
    }
}

/// The social snapshot: friends, ignores and the last `/who` answer.
pub fn social(lua: &Lua) -> Option<super::SocialState> {
    Some(lua.app_data_ref::<Model>()?.social.clone())
}

/// The last `SetWhoToUI` value.
pub fn who_to_ui(lua: &Lua) -> bool {
    lua.app_data_ref::<Model>().is_some_and(|m| m.who_to_ui)
}

/// The world map's displayed sheet, as a crate's map readers name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldMapSheet {
    /// The world sheet, continent 0.
    World,
    /// A whole continent, by its `WorldMapArea` art folder ("Kalimdor").
    Continent(String),
    /// A continent's zone, by its `AreaTable.dbc` id.
    Zone(u32),
    /// An instance map, by its `WorldMapArea` row id.
    Direct(u32),
}

/// The sheet `WorldMapFrame` shows: the selection `SetMapZoom` and the click handlers write.
pub fn world_map_sheet(lua: &Lua) -> Option<WorldMapSheet> {
    let model = lua.app_data_ref::<Model>()?;
    let map = &model.worldmap;
    if let Some(row) = map.direct_area {
        return Some(WorldMapSheet::Direct(row));
    }
    let (continent, zone) = map.selection;
    if continent == 0 {
        return Some(WorldMapSheet::World);
    }
    let c = map.continents.get(continent as usize - 1)?;
    Some(match zone {
        0 => WorldMapSheet::Continent(c.map_file.clone()),
        z => WorldMapSheet::Zone(c.zones.get(z as usize - 1)?.area_id),
    })
}

/// `PLAYER_EXPLORED_ZONES_1`'s words, bit n being `AreaTable.dbc` exploreFlag n.
pub fn explored_zones(lua: &Lua) -> Vec<u32> {
    lua.app_data_ref::<Model>()
        .map(|m| m.worldmap.explored.clone())
        .unwrap_or_default()
}

/// The quest log as the interface shows it: its rows in order, headers included, and the quests
/// folded under collapsed headers.
pub fn quest_log(lua: &Lua) -> Option<super::QuestLogState> {
    Some(lua.app_data_ref::<Model>()?.quest_log.clone())
}

/// `GetQuestLogSelection`: the selected 1-based row, 0 for none.
pub fn quest_log_selection(lua: &Lua) -> u32 {
    lua.app_data_ref::<Model>()
        .map_or(0, |m| m.quest_log_selection)
}

/// `GetMouseFoci`' frames as ids: every frame taking the mouse under the cursor, topmost first,
/// the hover frame first.
pub fn mouse_foci(lua: &Lua) -> Vec<u32> {
    lua.app_data_ref::<Model>()
        .map(|m| super::pointer::mouse_foci(&m))
        .unwrap_or_default()
}

/// Turn on the `PreClick`/`PostClick` bracket: a Button accepts both scripts, and a click fires
/// `PreClick`, `OnClick` and `PostClick`, each whether or not the others are set.
pub fn enable_click_bracket(lua: &Lua) {
    if let Some(mut m) = lua.app_data_mut::<Model>() {
        m.click_bracket = true;
    }
}

/// The mouse button of the innermost click, double click or press/release handler running now.
pub fn mouse_button_clicked(lua: &Lua) -> Option<String> {
    lua.app_data_ref::<Model>()?.clicked_button.clone()
}

/// The sender guid of the `CHAT_MSG_*` dispatch running now; `None` outside one or for a line no
/// player sent.
pub fn current_chat_guid(lua: &Lua) -> Option<u64> {
    lua.app_data_ref::<Model>()
        .map(|m| m.chat_guid)
        .filter(|g| *g != 0)
}

/// The frames registered for `event` by `RegisterEvent`, in registration order, as ids.
pub fn frames_registered_for_event(lua: &Lua, event: &str) -> Vec<u32> {
    let Some(m) = lua.app_data_ref::<Model>() else {
        return Vec::new();
    };
    m.event_to_frames
        .get(event)
        .into_iter()
        .flatten()
        .filter_map(|h| m.frame_to_id.get(h).copied())
        .collect()
}

/// The client's item-usable predicate (`0x5ea930`) for a cached template: level, class, race,
/// proficiency, skill, spell, honor, city and reputation gates. `None` while the template is
/// uncached or before the player's requirement state is pushed.
pub fn item_usable(lua: &Lua, item_id: u32) -> Option<bool> {
    let m = lua.app_data_ref::<Model>()?;
    if m.player_req.level == 0 || !m.item_templates.contains_key(&item_id) {
        return None;
    }
    Some(super::item_stats::item_usable_by_id(&m, item_id))
}

/// Lay a crate's override layer over the key bindings: `(key, command)` pairs in
/// `[ALT-][CTRL-][SHIFT-]KEY` spelling that dispatch in place of the key's binding, never stored.
pub fn set_binding_overrides(lua: &Lua, overrides: Vec<(String, String)>) {
    super::keybind::set_overrides(lua, overrides);
}

/// Set a crate's runner for a bound command no `Bindings.xml` declared, called `(command, down)`
/// and answering whether it ran.
pub fn set_binding_command_runner(lua: &Lua, f: mlua::Function) -> mlua::Result<()> {
    super::keybind::set_command_runner(lua, f)
}

/// The open vendor window, `None` when no vendor is open.
pub fn merchant(lua: &Lua) -> Option<super::MerchantState> {
    lua.app_data_ref::<Model>()?.merchant.clone()
}

/// Every registered virtual XML template as `(name, frame type)`, the element tag.
pub fn xml_templates(lua: &Lua) -> Vec<(String, String)> {
    let Some(m) = lua.app_data_ref::<Model>() else {
        return Vec::new();
    };
    let templates = m.framexml_templates.borrow();
    let mut out: Vec<(String, String)> = templates
        .iter()
        .map(|(name, e)| (name.clone(), e.tag.clone()))
        .collect();
    out.sort();
    out
}

/// One registered template by name, matched as `inherits=` resolves it.
pub fn xml_template(lua: &Lua, name: &str) -> Option<crate::framexml::Element> {
    let m = lua.app_data_ref::<Model>()?;
    let templates = m.framexml_templates.borrow();
    templates
        .get(name)
        .or_else(|| {
            templates
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, e)| e)
        })
        .cloned()
}

/// The open taxi map, `None` when no flight master's map is open.
pub fn taxi(lua: &Lua) -> Option<super::TaxiUiState> {
    lua.app_data_ref::<Model>()?.taxi.clone()
}
