//! Nampower's Lua API, installed on each in-game VM before the interface loads: the native
//! functions, then the bootstrap chunk (`nampower.lua`), which declares the `NP_` CVars, wraps
//! the stock verbs nampower extends, and builds the functions that are plain Lua over the stock
//! API.

use std::time::Instant;

use benilla_app::ext::UiScript;
use mlua::{Lua, Table, Value};

use crate::engine::guid_string;
use crate::{Np, State};

mod casting;
mod cooldowns;
mod files;
mod items;
mod spells;
mod units;

/// The bootstrap chunk.
const BOOTSTRAP: &str = include_str!("nampower.lua");

/// Install every native, then run the bootstrap; a failure is reported to the script error
/// handler and leaves the stock interface as it was.
pub fn install(np: &Np, script: &mut UiScript) {
    let result = (|| -> mlua::Result<()> {
        let lua = script.lua();
        let g = lua.globals();
        spells::install(lua, &g, np)?;
        units::install(lua, &g, np)?;
        items::install(lua, &g, np)?;
        cooldowns::install(lua, &g, np)?;
        casting::install(lua, &g, np)?;
        files::install(lua, &g)?;
        let cvars = lua.create_table()?;
        for (i, (name, default)) in crate::settings::CVARS.iter().enumerate() {
            let row = lua.create_table()?;
            row.set(1, *name)?;
            row.set(2, *default)?;
            cvars.set(i + 1, row)?;
        }
        g.set("NP_CVARS", cvars)?;
        Ok(())
    })();
    if let Err(e) = result {
        script.report_script_error(&format!("nampower: {e}"));
        return;
    }
    if let Err(e) = script.run_chunk_named(BOOTSTRAP.as_bytes(), "Interface\\nampower.lua") {
        script.report_script_error(&format!("nampower: {e}"));
    }
    // The CVars as this VM holds them, saved values included, into the engine.
    let mut st = np.lock();
    for (name, _) in crate::settings::CVARS {
        if let Some(value) = script.cvar(name) {
            st.engine.apply_setting(name, &value);
        }
    }
}

/// A Lua number argument as an integer, rounding as `lua_tonumber` then a C cast would.
pub(crate) fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Number(n) => Some(*n as i64),
        Value::String(s) => s
            .to_str()
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .map(|n| n as i64),
        _ => None,
    }
}

/// A Lua string argument.
pub(crate) fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A truthy `copy` flag: any value but nil, false and 0.
pub(crate) fn flag(v: &Value) -> bool {
    !matches!(v, Value::Nil | Value::Boolean(false)) && int(v) != Some(0)
}

/// A unit argument: a guid number, or a token or guid string in the extended grammar.
pub(crate) fn unit(st: &State, v: &Value) -> Option<u64> {
    match v {
        Value::Integer(i) => (*i != 0).then_some(*i as u64),
        Value::Number(n) => (*n != 0.0).then_some(*n as u64),
        Value::String(s) => st.mirror.resolve(&s.to_str().ok()?),
        _ => None,
    }
}

pub(crate) fn guid_value(lua: &Lua, guid: u64) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(guid_string(guid))?))
}

/// `GetTime()` now, the clock every `...S` field is on.
pub(crate) fn ui_now(lua: &Lua) -> f64 {
    lua.globals()
        .get::<mlua::Function>("GetTime")
        .and_then(|f| f.call::<f64>(()))
        .unwrap_or(0.0)
}

/// `at` on the `GetTime()` clock.
pub(crate) fn ui_time(lua: &Lua, at: Instant) -> f64 {
    let now = Instant::now();
    let ui = ui_now(lua);
    if at <= now {
        ui - now.duration_since(at).as_secs_f64()
    } else {
        ui + at.duration_since(now).as_secs_f64()
    }
}

/// Set `name` to a native function on `g`.
pub(crate) fn set<A, R, F>(lua: &Lua, g: &Table, name: &str, f: F) -> mlua::Result<()>
where
    A: mlua::FromLuaMulti,
    R: mlua::IntoLuaMulti,
    F: Fn(&Lua, A) -> mlua::Result<R> + 'static,
{
    g.set(name, lua.create_function(f)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn np() -> Np {
        Np {
            state: std::sync::Arc::new(std::sync::Mutex::new(State::new())),
            db: std::sync::Arc::new(crate::dbc::Databases::default()),
        }
    }

    #[test]
    fn the_bootstrap_loads_on_a_bare_vm_and_declares_the_cvars() {
        let np = np();
        let mut script = UiScript::new().unwrap();
        install(&np, &mut script);
        assert_eq!(script.errors(), Vec::<String>::new());
        let (a, b, c): (u32, u32, u32) = script.eval("return GetNampowerVersion()").unwrap();
        assert_eq!((a, b, c), crate::VERSION);
        assert_eq!(script.cvar("NP_SpellQueueWindowMs").as_deref(), Some("500"));
    }

    #[test]
    fn set_cvar_reaches_the_engine() {
        let np = np();
        let mut script = UiScript::new().unwrap();
        install(&np, &mut script);
        script
            .run("SetCVar('NP_SpellQueueWindowMs', '250')")
            .unwrap();
        assert_eq!(np.lock().engine.settings.spell_queue_window_ms, 250);
        assert_eq!(script.cvar("NP_SpellQueueWindowMs").as_deref(), Some("250"));
    }
}
