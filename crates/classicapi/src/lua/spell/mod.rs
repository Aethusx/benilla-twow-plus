//! `src/spell/`: the spell API. `Spell::Arg` and `Spell::Lookup` are the argument and book
//! helpers here; each source file is a submodule.

use mlua::{Lua, Value};

use super::{as_string, atoi, is_number, to_int, Api};

mod book;
mod cast;
mod castat;
pub(crate) mod data;
mod description;
mod info;
mod state;

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    info::install(api)?;
    data::install(api)?;
    book::install(api)?;
    state::install(api)?;
    cast::install(api)?;
    description::install(api)?;
    castat::install(api)?;
    Ok(())
}

/// `Spell::Arg::ResolveSpellID`: a number is a spell id; a string carrying `spell:` is the id
/// after it, a numeric string its value, and any other string a name resolved through the
/// engine's book resolver. 0 for nothing.
pub(crate) fn resolve_spell_id(lua: &Lua, v: &Value) -> i64 {
    if is_number(v) {
        return to_int(v);
    }
    let Some(s) = as_string(v) else {
        return 0;
    };
    if let Some(at) = s.find("spell:") {
        return atoi(&s[at + 6..]);
    }
    let numeric = atoi(&s);
    if numeric > 0 {
        return numeric;
    }
    name_to_spell_id(lua, &s)
}

/// `Spell::Arg::NameToSpellID`, the engine's `CastSpellByName` resolver over both books.
pub(crate) fn name_to_spell_id(lua: &Lua, name: &str) -> i64 {
    if name.is_empty() {
        return 0;
    }
    benilla_ui::script::ext_read::book_spell_by_name(lua, name).map_or(0, i64::from)
}

/// `Spell::Lookup::SpellbookSlotToID`: the spell at 1-based `slot` of the player's book, or the
/// pet's when `pet`; 0 out of range.
pub(crate) fn book_slot_to_id(lua: &Lua, slot: i64, pet: bool) -> u32 {
    let (player, pet_book) = benilla_ui::script::ext_read::spellbook_ids(lua);
    let book = if pet { pet_book } else { player };
    usize::try_from(slot - 1)
        .ok()
        .and_then(|i| book.get(i).copied())
        .unwrap_or(0)
}

/// `Spell::Lookup::FindSpellbookSlot`: the 1-based slot and whether it is the pet's book.
pub(crate) fn find_book_slot(lua: &Lua, spell_id: i64) -> Option<(usize, bool)> {
    if spell_id <= 0 {
        return None;
    }
    let (player, pet) = benilla_ui::script::ext_read::spellbook_ids(lua);
    if let Some(i) = player.iter().position(|id| i64::from(*id) == spell_id) {
        return Some((i + 1, false));
    }
    pet.iter()
        .position(|id| i64::from(*id) == spell_id)
        .map(|i| (i + 1, true))
}

/// `Spell::Lookup::SpellBankArgToBookType`: 1 is the pet's bank, anything else the player's.
pub(crate) fn bank_is_pet(v: &Value) -> bool {
    is_number(v) && to_int(v) == 1
}

/// `BookTypeIsPet`: the literal `"pet"`, case-insensitively, as 1.12's `GetSpellName` compares.
pub(crate) fn book_type_is_pet(s: &str) -> bool {
    s.eq_ignore_ascii_case("pet")
}
