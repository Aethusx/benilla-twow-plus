//! `Description.cpp`: `C_Spell.GetSpellDescription`, the spell's description with its `$` tokens
//! expanded as the engine's formatter (`FUN_FORMAT_SPELL_DESCRIPTION`) expands them for the
//! player, through benilla-formats' token engine over the same tables the tooltip reads.

use std::collections::HashMap;
use std::sync::Arc;

use benilla_formats::{SpellDisplay, TokenContext, TokenNumber};
use mlua::{Lua, Value};

use super::resolve_spell_id;
use crate::dbc::Dbc;
use crate::lua::Api;
use crate::mirror::{field, Mirror};
use crate::{spellmod, spells, talents, Ca};

/// The player's modifier tables as the token engine's appliers.
struct Mods {
    tables: spellmod::Tables,
    family: u32,
    spells: Arc<Dbc>,
}

impl benilla_formats::SpellMods for Mods {
    fn apply_int(&self, d: &SpellDisplay, op: u8, value: i32) -> i32 {
        self.spells.row(d.id).map_or(value, |rec| {
            spellmod::apply_int(&self.tables, self.family, &rec, op, value)
        })
    }

    fn apply_float(&self, d: &SpellDisplay, op: u8, value: f32) -> f32 {
        self.spells.row(d.id).map_or(value, |rec| {
            spellmod::apply(&self.tables, self.family, &rec, op, value)
        })
    }

    fn apply_soft(&self, d: &SpellDisplay, op: u8, value: f32) -> f32 {
        // The software-float applier `0x6e6c30`, the tooltip's form for float points.
        let cell = self
            .spells
            .row(d.id)
            .and_then(|rec| spellmod::cell(&self.tables, self.family, &rec, op));
        match cell {
            Some((flat, pct)) => benilla_formats::soft_modify(value, flat, (pct + 100).max(0)),
            None => value,
        }
    }
}

/// `0x5ea520`: each skill line's value, plus the permanent bonus when the value is positive,
/// plus the temporary one, floored at 0.
fn skill_values(m: &Mirror) -> HashMap<u32, u32> {
    let Some(me) = m.me() else {
        return HashMap::new();
    };
    (0..128)
        .filter_map(|i| {
            let at = field::PLAYER_SKILL_INFO_1_1 + i * 3;
            let line = u32::from(me.half(at, false));
            if line == 0 {
                return None;
            }
            let mut v = i32::from(me.half(at + 1, false));
            if v > 0 {
                v += i32::from(me.half(at + 2, true));
            }
            v += i32::from(me.half(at + 2, false) as i16);
            Some((line, v.max(0) as u32))
        })
        .collect()
}

/// `SStrPrintf` over benilla-ui's template filler, as the app's token seam fills it.
fn printf(template: &str, args: &[TokenNumber]) -> String {
    let args: Vec<_> = args
        .iter()
        .map(|n| match n {
            TokenNumber::Int(v) => benilla_ui::strings::Arg::D(*v),
            TokenNumber::Float(v) => benilla_ui::strings::Arg::F(*v),
        })
        .collect();
    benilla_ui::strings::fill(template, &args)
}

fn description(lua: &Lua, ca: &Ca, id: u32) -> Option<String> {
    let db = &ca.db;
    let catalog = spells::catalog(db)?;
    let d = catalog.get(id)?;
    let text = d.description.as_deref()?;
    let durations = db.catalog(benilla_formats::load_spell_durations)?;
    let radii = db.catalog(benilla_formats::load_spell_radii)?;
    let ranges = spells::range_catalog(db);
    let table = spells::table(db)?;
    // What the tokens read off the player, taken under the lock; the strings come after.
    let (mods, skills, bits, sex) = {
        let st = ca.lock();
        let family = spellmod::player_family(db, &st.mirror);
        let mods = Mods {
            tables: st.mods.clone(),
            family,
            spells: table,
        };
        (
            mods,
            skill_values(&st.mirror),
            talents::player_bits(&st.mirror),
            // `$g`/`$G` branch on `UNIT_FIELD_BYTES_0`'s gender byte (`0x508214`).
            st.mirror.me().map_or(0, |me| me.gender()),
        )
    };
    let skill = |spell: u32| {
        let line = talents::skill_for_spell_bits(db, bits, spell);
        skills.get(&line).copied().unwrap_or(0)
    };
    let lookup = |spell: u32| catalog.get(spell);
    let global = |key: &str| benilla_ui::strings::global(lua, key);
    let ctx = TokenContext {
        durations: &durations,
        radii: &radii,
        ranges: ranges.as_deref(),
        skill: &skill,
        lookup: &lookup,
        mods: Some(&mods),
        unmodified_points: false,
        gender: &|| sex,
        home_area: &|| None,
        global: &global,
        printf: &printf,
    };
    let out = benilla_formats::substitute(text, d, &ctx);
    (!out.is_empty()).then_some(out)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.table("C_Spell", "GetSpellDescription", move |lua, v: Value| {
        let id = u32::try_from(resolve_spell_id(lua, &v)).unwrap_or(0);
        Ok(description(lua, &c, id))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;

    #[test]
    fn frostbolt_reads_with_its_tokens_expanded() {
        let _ = benilla_formats::wow_data_or_skip!();
        let ca = crate::Ca::default();
        let script = vm(&ca);
        let text: Option<String> = script
            .lua()
            // The stock `GlobalStrings.lua` templates the two tokens fill; the test VM loads none.
            .load(
                r#"
                INT_SPELL_DURATION_SEC = "%d sec"
                INT_SPELL_POINTS_SPREAD_TEMPLATE = "%d to %d"
                return C_Spell.GetSpellDescription(116)
                "#,
            )
            .eval()
            .expect("chunk");
        let text = text.expect("Frostbolt has a description");
        assert!(!text.contains('$'), "{text}");
        assert!(text.chars().any(|c| c.is_ascii_digit()), "{text}");
        let none: Option<String> = script
            .lua()
            .load("return C_Spell.GetSpellDescription(0)")
            .eval()
            .expect("chunk");
        assert_eq!(none, None);
    }
}
