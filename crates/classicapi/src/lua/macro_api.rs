//! `macro/`: the macro API backports.
//!
//! - `Icons.cpp`: `GetMacroIcons(t)`, `GetMacroItemIcons(t)`, `GetLooseMacroIcons(t)`,
//!   `GetLooseMacroItemIcons(t)`, each appending sorted upper-case icon basenames to `t`: the
//!   archives' `Interface\Icons\` files, `Ability_`/`Spell_` for spells and `INV_` for items.
//!   benilla reads no loose files, so the two loose lists are empty, as on a stock install.
//! - `Edit.cpp`: `C_Macro.CreateMacro(name, iconTexture, body, isCharacterMacro)` and
//!   `C_Macro.EditMacro(index | name, name, iconTexture, body)`, the icon a texture path or bare
//!   basename stored as `Interface\Icons\<basename>`, so an `INV_` icon the chooser lacks is
//!   reachable; a non-string icon is none (create) or unchanged (edit).
//! - `RunBody.cpp`: the runner skips `#` lines, where 1.12's sends `#showtooltip` to chat, and
//!   `StopMacro()` ends the running body after its line.

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

/// The icon argument as the stored path: a string's basename under `Interface\Icons\`.
fn icon_texture(v: Option<&Value>) -> Option<String> {
    let s = match v {
        Some(Value::String(s)) => s.to_string_lossy(),
        _ => return None,
    };
    let base = s.rsplit(['\\', '/']).next().unwrap_or("");
    (!base.is_empty()).then(|| format!("Interface\\Icons\\{base}"))
}

fn opt_string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s.to_string_lossy()),
        _ => None,
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    benilla_ui::script::ext_read::set_macro_skip_comments(api.lua, true);
    api.global("StopMacro", |lua, ()| {
        benilla_ui::script::ext_read::request_macro_stop(lua);
        Ok(())
    })?;
    api.table("C_Macro", "CreateMacro", |lua, args: mlua::MultiValue| {
        let a: Vec<Value> = args.into_iter().collect();
        let Some(name) = opt_string(a.first()) else {
            return Err(mlua::Error::runtime(
                "Usage: C_Macro.CreateMacro(name, iconTexture, body, isCharacterMacro)",
            ));
        };
        let body = opt_string(a.get(2)).unwrap_or_default();
        let per_character = a.get(3).is_some_and(crate::lua::truthy);
        Ok(benilla_ui::script::ext_read::create_macro(
            lua,
            &name,
            icon_texture(a.get(1)),
            &body,
            per_character,
        ))
    })?;
    api.table("C_Macro", "EditMacro", |lua, args: mlua::MultiValue| {
        let a: Vec<Value> = args.into_iter().collect();
        let index = match a.first() {
            Some(v) if crate::lua::is_number(v) => crate::lua::to_int(v) as usize,
            Some(Value::String(s)) => {
                benilla_ui::script::ext_read::macro_index_by_name(lua, &s.to_string_lossy())
            }
            _ => {
                return Err(mlua::Error::runtime(
                    "Usage: C_Macro.EditMacro(index, name, iconTexture, body)",
                ))
            }
        };
        Ok(benilla_ui::script::ext_read::edit_macro(
            lua,
            index,
            opt_string(a.get(1)),
            icon_texture(a.get(2)),
            opt_string(a.get(3)),
        ))
    })?;
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
    fn macros_take_an_icon_by_texture_name() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                local i = C_Macro.CreateMacro("Bomb", "Interface\\Icons\\INV_Misc_Bomb_08", "/use Bomb")
                local _, tex = GetMacroInfo(i)
                C_Macro.EditMacro("Bomb", nil, "INV_Sword_25", nil)
                local name, tex2, body = GetMacroInfo(i)
                return tostring(i) .. " " .. tex .. " " .. tex2 .. " " .. name .. " " .. body
                  .. " " .. tostring(C_Macro.EditMacro("Nope", "x"))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            r"1 Interface\Icons\INV_Misc_Bomb_08 Interface\Icons\INV_Sword_25 Bomb /use Bomb nil"
        );
    }

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
