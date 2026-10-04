//! `cvar/`: `C_CVar.GetCVarInfo`, `DoesCVarExist`, `AreCVarsLoaded`, `GetCVarBool`,
//! `GetCVarBitfield`, `SetCVarBitfield`, `SetTempCVar` and `RemoveTempCVar`, over benilla's CVar
//! table; writes go through the stock `SetCVar`, so they fire and persist as it does.
//!
//! - `GetCVarInfo`: value, default, then `false` for the server-stored account and character
//!   flags, locked-from-user and secure (none exist in 1.12), and the read-only flag.
//! - `GetCVarBool`: empty and `"0"` false, a non-zero number or `"true"` true, anything else false;
//!   nil for an unknown name.
//! - Bitfields: the value's leading decimal digits as a `u64`, bits 1-64.
//! - Temporary values (`Temp.cpp`): the value before the first temporary set is remembered and the
//!   row is marked, so benilla saves the file's value, not the temporary one; `RemoveTempCVar`
//!   puts the remembered value back. A plain `SetCVar` over a temporary value makes it the
//!   player's again, and it saves.
//!
//! Deviation: `cvar/ScriptMemory.cpp` zeroes the engine's 48 MB interface memory watchdog; benilla
//! has no such watchdog, so there is nothing to lift. The glue screen's `GetCVar` family
//! (`Glue.cpp`) belongs to benilla's glue VM, which ClassicAPI does not load into.

use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Value};

use crate::lua::{is_number, to_int, to_str, Api};
use crate::Ca;

/// One temporary value: the name, the value before it, and the value that landed.
#[derive(Clone, Debug)]
pub struct Temp {
    name: String,
    saved: String,
    temp: String,
}

fn live(lua: &Lua, name: &str) -> Option<String> {
    benilla_ui::script::ext_read::cvar(lua, name).map(|c| c.0)
}

/// The coercion `GetCVarBool` applies.
fn truth(value: &str) -> bool {
    let v = value.trim();
    if v.eq_ignore_ascii_case("true") {
        return true;
    }
    v.parse::<f64>().is_ok_and(|n| n != 0.0)
}

/// The leading decimal digits as a bitfield; anything else reads as no bits.
fn bits(value: &str) -> u64 {
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

fn bit_index(v: &Value) -> Option<u32> {
    is_number(v)
        .then(|| to_int(v))
        .filter(|i| (1..=64).contains(i))
        .map(|i| i as u32 - 1)
}

/// `Script_SetCVar`'s lookup and checks, with its error texts.
fn resolve(lua: &Lua, v: &Value, usage: &str) -> mlua::Result<String> {
    let Some(name) = to_str(v) else {
        return Err(mlua::Error::runtime(usage.to_string()));
    };
    match benilla_ui::script::ext_read::cvar(lua, &name) {
        None => Err(mlua::Error::runtime(format!(
            "Couldn't find CVar named '{name}'"
        ))),
        Some((_, _, true)) => Err(mlua::Error::runtime(format!("\"{name}\" is read only"))),
        Some(_) => Ok(name),
    }
}

fn set(set_cvar: &Option<Function>, name: &str, value: &str) -> mlua::Result<()> {
    match set_cvar {
        Some(f) => f.call::<()>((name, value)),
        None => Ok(()),
    }
}

/// The frame's upkeep: a temporary value the player has since overwritten is theirs, so its entry
/// and mark go.
pub(crate) fn tick(lua: &Lua, ca: &Ca) {
    let gone: Vec<String> = {
        let st = ca.lock();
        st.temp_cvars
            .iter()
            .filter(|t| live(lua, &t.name).is_none_or(|v| v != t.temp))
            .map(|t| t.name.clone())
            .collect()
    };
    if gone.is_empty() {
        return;
    }
    ca.lock()
        .temp_cvars
        .retain(|t| !gone.iter().any(|g| g.eq_ignore_ascii_case(&t.name)));
    for name in gone {
        benilla_ui::script::ext_read::mark_cvar_temporary(lua, &name, false);
    }
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_CVar";
    let set_cvar: Option<Function> = api.lua.globals().get("SetCVar").ok();

    api.table(NS, "GetCVarInfo", |lua, v: Value| {
        let Some(name) = to_str(&v).filter(|_| matches!(v, Value::String(_))) else {
            return Err(mlua::Error::runtime("Usage: C_CVar.GetCVarInfo(\"cvar\")"));
        };
        match benilla_ui::script::ext_read::cvar(lua, &name) {
            Some((value, default, read_only)) => {
                (value, default, false, false, false, false, read_only).into_lua_multi(lua)
            }
            None => Ok(MultiValue::from_vec(vec![Value::Nil])),
        }
    })?;
    api.table(NS, "DoesCVarExist", |lua, v: Value| {
        Ok(match &v {
            Value::String(s) => {
                benilla_ui::script::ext_read::cvar(lua, &s.to_string_lossy()).is_some()
            }
            _ => false,
        })
    })?;
    // The registry is up before any VM runs: benilla seeds the VM's table from it at load.
    api.table(NS, "AreCVarsLoaded", |_, ()| Ok(true))?;
    api.table(NS, "GetCVarBool", |lua, v: Value| {
        let Some(name) = to_str(&v) else {
            return Err(mlua::Error::runtime("Usage: C_CVar.GetCVarBool(\"cvar\")"));
        };
        Ok(live(lua, &name).map(|value| truth(&value)))
    })?;
    api.table(
        NS,
        "GetCVarBitfield",
        |lua, (name, index): (Value, Value)| {
            let (Some(name), Some(bit)) = (to_str(&name), bit_index(&index)) else {
                return Ok(None);
            };
            Ok(live(lua, &name).map(|value| bits(&value) >> bit & 1 == 1))
        },
    )?;
    let setter = set_cvar.clone();
    api.table(
        NS,
        "SetCVarBitfield",
        move |lua, (name, index, on): (Value, Value, Value)| {
            let (Some(name), Some(bit)) = (to_str(&name), bit_index(&index)) else {
                return Ok(false);
            };
            let Some((value, _, false)) = benilla_ui::script::ext_read::cvar(lua, &name) else {
                return Ok(false);
            };
            let mut field = bits(&value);
            if crate::lua::truthy(&on) {
                field |= 1 << bit;
            } else {
                field &= !(1 << bit);
            }
            set(&setter, &name, &field.to_string())?;
            Ok(true)
        },
    )?;

    let (c, setter) = (api.ca.clone(), set_cvar.clone());
    api.table(
        NS,
        "SetTempCVar",
        move |lua, (name, value): (Value, Value)| {
            let name = resolve(lua, &name, "Usage: C_CVar.SetTempCVar(\"cvar\", value)")?;
            let value = to_str(&value).unwrap_or_default();
            let now = live(lua, &name).unwrap_or_default();
            {
                let mut st = c.lock();
                match st
                    .temp_cvars
                    .iter_mut()
                    .find(|t| t.name.eq_ignore_ascii_case(&name))
                {
                    // A plain SetCVar replaced the temporary value: that is the one to restore.
                    Some(t) if t.temp != now => t.saved = now,
                    Some(_) => {}
                    None => st.temp_cvars.push(Temp {
                        name: name.clone(),
                        saved: now,
                        temp: String::new(),
                    }),
                }
            }
            benilla_ui::script::ext_read::mark_cvar_temporary(lua, &name, true);
            set(&setter, &name, &value)?;
            // What landed, not what was asked.
            let landed = live(lua, &name).unwrap_or_default();
            if let Some(t) = c
                .lock()
                .temp_cvars
                .iter_mut()
                .find(|t| t.name.eq_ignore_ascii_case(&name))
            {
                t.temp = landed;
            }
            Ok(())
        },
    )?;
    let (c, setter) = (api.ca.clone(), set_cvar);
    api.table(NS, "RemoveTempCVar", move |lua, name: Value| {
        let name = resolve(lua, &name, "Usage: C_CVar.RemoveTempCVar(\"cvar\")")?;
        let entry = {
            let mut st = c.lock();
            let at = st
                .temp_cvars
                .iter()
                .position(|t| t.name.eq_ignore_ascii_case(&name));
            at.map(|i| st.temp_cvars.remove(i))
        };
        let Some(entry) = entry else {
            return Ok(());
        };
        if live(lua, &name).as_deref() == Some(entry.temp.as_str()) {
            set(&setter, &name, &entry.saved)?;
        }
        benilla_ui::script::ext_read::mark_cvar_temporary(lua, &name, false);
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::test_support::vm;

    #[test]
    fn values_read_as_bools_and_bitfields() {
        assert!(truth("1") && truth("TRUE") && truth("2.5"));
        assert!(!truth("") && !truth("0") && !truth("yes"));
        assert_eq!(bits("5abc"), 5);
        assert_eq!(bits("x"), 0);
    }

    #[test]
    fn temporary_values_restore_and_bitfields_set() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                RegisterCVar("capiTest", "4")
                local C = C_CVar
                local before = tostring(C.GetCVarBitfield("capiTest", 3)) .. tostring(C.GetCVarBitfield("capiTest", 1))
                C.SetCVarBitfield("capiTest", 1, true)
                local after = GetCVar("capiTest")
                C.SetTempCVar("capiTest", "9")
                local temp = GetCVar("capiTest")
                C.RemoveTempCVar("capiTest")
                local info = { C.GetCVarInfo("capiTest") }
                return before .. " " .. after .. " " .. temp .. " " .. GetCVar("capiTest")
                  .. " " .. info[2] .. tostring(info[7]) .. " " .. tostring(C.DoesCVarExist("nope"))
                  .. tostring(C.GetCVarBool("capiTest")) .. tostring(C.GetCVarBool("nope"))
                  .. " " .. tostring(pcall(C.SetTempCVar, "nope", "1"))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "truefalse 5 9 5 4false falsetruenil false");
    }
}
