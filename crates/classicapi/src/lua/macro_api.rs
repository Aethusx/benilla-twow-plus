//! `macro/`: the macro API backports.
//!
//! - `Icons.cpp`: `GetMacroIcons(t)`, `GetMacroItemIcons(t)`, `GetLooseMacroIcons(t)`,
//!   `GetLooseMacroItemIcons(t)`, each appending sorted upper-case icon basenames to `t`: the
//!   archives' `Interface\Icons\` files, `Ability_`/`Spell_` for spells and `INV_` for items.
//!   benilla reads no loose files, so the two loose lists are empty, as on a stock install.

use std::sync::Arc;

use mlua::{Table, Value};

use crate::lua::Api;
use crate::Ca;

/// The archive icons by kind, upper-case basenames, sorted and deduplicated.
#[derive(Default)]
pub struct IconLists {
    pub spell: Vec<String>,
    pub item: Vec<String>,
}

fn load_icon_lists(chain: &mut benilla_formats::Chain) -> Result<IconLists, String> {
    const DIR: &str = "INTERFACE\\ICONS\\";
    let mut out = IconLists::default();
    for entry in chain.list().map_err(|e| e.to_string())? {
        let path = entry.name.replace('/', "\\").to_ascii_uppercase();
        let Some(file) = path.strip_prefix(DIR).filter(|f| !f.contains('\\')) else {
            continue;
        };
        let key = file.split('.').next().unwrap_or("").to_string();
        if key.starts_with("INV_") {
            out.item.push(key);
        } else if key.starts_with("ABILITY_") || key.starts_with("SPELL_") {
            out.spell.push(key);
        }
    }
    for v in [&mut out.spell, &mut out.item] {
        v.sort();
        v.dedup();
    }
    Ok(out)
}

fn icon_lists(ca: &Ca) -> Arc<IconLists> {
    ca.db.catalog(load_icon_lists).unwrap_or_default()
}

/// Append `names` after the table's last non-nil array slot.
fn append(t: &Table, names: &[String]) -> mlua::Result<()> {
    let mut i = 1;
    while !t.raw_get::<Value>(i)?.is_nil() {
        i += 1;
    }
    for n in names {
        t.raw_set(i, n.as_str())?;
        i += 1;
    }
    Ok(())
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    for (name, item, loose) in [
        ("GetMacroIcons", false, false),
        ("GetMacroItemIcons", true, false),
        ("GetLooseMacroIcons", false, true),
        ("GetLooseMacroItemIcons", true, true),
    ] {
        let c = api.ca.clone();
        let usage = format!("Usage: {name}(table)");
        api.global(name, move |_, v: Value| {
            let Value::Table(t) = v else {
                return Err(mlua::Error::runtime(usage.clone()));
            };
            if loose {
                return Ok(());
            }
            let lists = icon_lists(&c);
            append(&t, if item { &lists.item } else { &lists.spell })
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;

    #[test]
    fn the_icon_lists_append_archive_basenames() {
        let _ = benilla_formats::wow_data_or_skip!();
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                local s, i, l = { "KEEP" }, {}, {}
                GetMacroIcons(s)
                GetMacroItemIcons(i)
                GetLooseMacroIcons(l)
                return s[1] .. " " .. string.sub(s[2], 1, 8) .. " "
                  .. string.sub(i[1], 1, 4) .. " " .. tostring(table.getn(i) > 1000)
                  .. " " .. table.getn(l)
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "KEEP ABILITY_ INV_ true 0");
    }
}
