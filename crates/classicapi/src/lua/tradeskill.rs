//! `tradeskill/Link.cpp`: the trade-skill list link of 2.x, backported as a ClassicAPI-private
//! shape, `|Htrade:<skillLine>:<cur>:<max>:<linker>:<bits>|h[<Profession>]|h`.
//!
//! `bits` is a base64 bitfield, six recipes a character, least significant first, over the
//! skill line's canonical recipe list: every `SkillLineAbility.dbc` row of the line whose spell
//! makes something (creates an item, enchants one, or teaches a recipe), in row order, so every
//! client with the same data agrees on the bit positions. A bit is set when the player knows the
//! spell. `GetTradeSkillListLink` builds it for the open trade-skill window, `GetCraftListLink`
//! for the craft window (the stock `GetTradeSkillLine` / `GetCraftDisplaySkillLine` name the
//! line), and `GetTradeSkillListRecipes` decodes one for the bundled addon's viewer, with each
//! recipe's difficulty tiers, created item, yield and reagents from `Spell.dbc`.

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::lua::{as_string, is_number, to_int, to_str, Api};
use crate::spells::col;
use crate::talents::{sla_col, SKILL_LINE_NAME};
use crate::Ca;

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PER_CHAR: usize = 6;
/// `SPELL_EFFECT_CREATE_ITEM`, `LEARN_SPELL`, `ENCHANT_ITEM`, `ENCHANT_ITEM_TEMPORARY`.
const CRAFT_EFFECTS: [u32; 4] = [24, 36, 53, 54];
const CREATE_ITEM: u32 = 24;
/// `Spell.dbc`'s `EffectItemType[0]`, the created item.
const EFFECT_ITEM_TYPE: usize = 0x19C / 4;
const MAX_REAGENTS: usize = 8;

/// One recipe of a line: spell, grey and green levels.
type Recipe = (u32, u32, u32);

/// `IsCraftRecipe`: one of the spell's effects makes something.
fn is_craft(ca: &Ca, spell: u32) -> bool {
    crate::spells::table(&ca.db)
        .and_then(|t| {
            t.row(spell)
                .map(|r| (0..3).any(|e| CRAFT_EFFECTS.contains(&r.u32(col::EFFECT + e))))
        })
        .unwrap_or(false)
}

/// `BuildRecipeList`: the line's recipes in row order.
fn recipes(ca: &Ca, line: u32) -> Vec<Recipe> {
    let Some(sla) = ca.db.get("SkillLineAbility") else {
        return Vec::new();
    };
    sla.rows()
        .filter(|r| r.u32(sla_col::SKILL) == line)
        .filter_map(|r| {
            let spell = r.u32(sla_col::SPELL);
            is_craft(ca, spell).then(|| {
                (
                    spell,
                    r.u32(sla_col::TRIVIAL_HIGH),
                    r.u32(sla_col::TRIVIAL_LOW),
                )
            })
        })
        .collect()
}

/// `EncodeKnownBits`.
pub(crate) fn encode(known: impl Fn(u32) -> bool, recipes: &[Recipe]) -> String {
    recipes
        .chunks(PER_CHAR)
        .map(|chunk| {
            let v = chunk
                .iter()
                .enumerate()
                .filter(|(_, r)| known(r.0))
                .fold(0usize, |acc, (i, _)| acc | 1 << i);
            BASE64[v] as char
        })
        .collect()
}

/// Whether recipe `i` is set in `bits`; past the end or a bad character is not known.
pub(crate) fn decode_bit(bits: &[u8], i: usize) -> bool {
    bits.get(i / PER_CHAR)
        .and_then(|c| BASE64.iter().position(|b| b == c))
        .is_some_and(|v| v & (1 << (i % PER_CHAR)) != 0)
}

/// The skill line a localized profession name names.
fn line_by_name(ca: &Ca, name: &str) -> Option<u32> {
    ca.db
        .get("SkillLine")?
        .rows()
        .find(|r| r.loc(SKILL_LINE_NAME).eq_ignore_ascii_case(name))
        .map(|r| r.id())
}

/// `BuildLink` for the window a stock getter names: `(name, rank, max)`, nil or "UNKNOWN" closed.
fn build_link(lua: &Lua, ca: &Ca, getter: &str) -> mlua::Result<Option<String>> {
    let Ok(f) = lua.globals().get::<Function>(getter) else {
        return Ok(None);
    };
    let rets: MultiValue = f.call(()).unwrap_or_default();
    let Some(name) = rets.front().and_then(as_string) else {
        return Ok(None);
    };
    if name.is_empty() || name == "UNKNOWN" {
        return Ok(None);
    }
    let Some(line) = line_by_name(ca, &name) else {
        return Ok(None);
    };
    let list = recipes(ca, line);
    if list.is_empty() {
        return Ok(None);
    }
    let (bits, rank, who) = {
        let st = ca.lock();
        let bits = encode(|s| st.known.contains(&s), &list);
        let rank =
            crate::talents::skill_rank(&st.mirror, line).map_or((0, 0), |(cur, max, bonus)| {
                let add = |v: u16| {
                    if v != 0 {
                        u32::from(v) + u32::from(bonus)
                    } else {
                        0
                    }
                };
                (add(cur), add(max))
            });
        let who = st
            .mirror
            .names
            .get(&st.mirror.player)
            .cloned()
            .unwrap_or_default();
        (bits, rank, who)
    };
    Ok(Some(format!(
        "|cffffd000|Htrade:{line}:{}:{}:{who}:{bits}|h[{name}]|h|r",
        rank.0, rank.1
    )))
}

fn recipe_table(lua: &Lua, ca: &Ca, r: &Recipe, known: bool) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("spellID", r.0)?;
    t.set("isKnown", known)?;
    t.set("trivialLevel", r.1)?;
    t.set("greenLevel", r.2)?;
    let rec = crate::spells::table(&ca.db).and_then(|s| {
        let row = s.row(r.0)?;
        let made = (0..3)
            .find(|e| row.u32(col::EFFECT + e) == CREATE_ITEM)
            .map_or(0, |e| {
                row.i32(col::EFFECT_BASE_POINTS + e) + row.i32(col::EFFECT_BASE_DICE + e)
            });
        let reagents: Vec<(i32, i32)> = (0..MAX_REAGENTS)
            .map(|i| (row.i32(col::REAGENT + i), row.i32(col::REAGENT_COUNT + i)))
            .take_while(|(id, _)| *id != 0)
            .collect();
        Some((row.i32(EFFECT_ITEM_TYPE), made, reagents))
    });
    let (item, made, reagents) = rec.unwrap_or_default();
    t.set("createdItem", item)?;
    t.set("numMade", made)?;
    let list = lua.create_table()?;
    for (i, (id, count)) in reagents.into_iter().enumerate() {
        let e = lua.create_table()?;
        e.set("itemID", id)?;
        e.set("count", count)?;
        list.raw_set(i + 1, e)?;
    }
    t.set("reagents", list)?;
    Ok(t)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_TradeSkillUI";
    let c = api.ca.clone();
    api.table(NS, "GetTradeSkillListLink", move |lua, ()| {
        build_link(lua, &c, "GetTradeSkillLine")
    })?;
    let c = api.ca.clone();
    api.table(NS, "GetCraftListLink", move |lua, ()| {
        build_link(lua, &c, "GetCraftDisplaySkillLine")
    })?;
    let c = api.ca.clone();
    api.table(
        NS,
        "GetTradeSkillListRecipes",
        move |lua, (line, bits): (Value, Value)| {
            let (true, Some(bits)) = (is_number(&line), to_str(&bits)) else {
                return Err(mlua::Error::runtime(
                    "Usage: C_TradeSkillUI.GetTradeSkillListRecipes(skillLineID, bits)",
                ));
            };
            let out = lua.create_table()?;
            for (i, r) in recipes(&c, to_int(&line) as u32).iter().enumerate() {
                out.raw_set(
                    i + 1,
                    recipe_table(lua, &c, r, decode_bit(bits.as_bytes(), i))?,
                )?;
            }
            Ok(out)
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bits_round_trip_six_a_character() {
        let list: Vec<Recipe> = (1..=8).map(|s| (s, 0, 0)).collect();
        let bits = encode(|s| s == 1 || s == 6 || s == 8, &list);
        // Recipes 1 and 6 in the first character (0b100001 = 33 = 'h'), 8 in the second (0b10).
        assert_eq!(bits, "hC");
        let known: Vec<bool> = (0..9).map(|i| decode_bit(bits.as_bytes(), i)).collect();
        assert_eq!(
            known,
            [true, false, false, false, false, true, false, true, false]
        );
    }

    #[test]
    fn a_line_lists_its_recipes_with_items_and_reagents() {
        let _ = benilla_formats::wow_data_or_skip!();
        let script = crate::lua::test_support::vm(&crate::Ca::default());
        // Cooking (185): Charred Wolf Meat (2538) makes item 2679 from one Stringy Wolf Meat (2672).
        let out: String = script
            .lua()
            .load(
                r#"
                for _, r in ipairs(C_TradeSkillUI.GetTradeSkillListRecipes(185, "")) do
                  if r.spellID == 2538 then
                    return r.createdItem .. " " .. r.numMade .. " " .. r.reagents[1].itemID
                      .. "x" .. r.reagents[1].count .. " " .. tostring(r.isKnown)
                  end
                end
                return "missing"
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "2679 1 2672x1 false");
    }
}
