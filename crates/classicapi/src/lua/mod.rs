//! ClassicAPI's Lua API, installed on each in-game VM before the interface loads. One module per
//! source folder of the DLL; each registers its natives through [`Api`], which binds every name
//! twice as the DLL's registrars do (`Game.cpp`): at its public place, and by value under
//! `_G.ClassicAPI`, which nothing else writes, so `ClassicAPI.X == X` holds until something
//! replaces the global.
//!
//! The bootstrap chunks are Lua run as `function(CA) ... end`, `CA` being the private table of
//! internal natives, never published.

use benilla_app::ext::UiScript;
use mlua::{IntoLua, IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::Ca;

mod action;
mod addons;
mod args;
mod aura;
mod baselib;
mod color;
mod container;
mod core;
mod creature;
pub(crate) mod cvar;
mod encoding;
pub(crate) mod equipmentset;
pub(crate) mod faction;
mod frame;
mod gossip;
mod info;
pub(crate) mod item;
mod itemscrape;
pub(crate) mod lossofcontrol;
pub(crate) mod luasyntax;
mod macro_api;
pub(crate) mod misc;
mod nameplate;
mod playerinfo;
pub(crate) mod showtooltip;
pub(crate) mod spell;
pub(crate) mod targeting;
pub(crate) mod time;
pub(crate) mod totem;
pub(crate) mod unit;

pub(crate) use args::*;

/// The escape-hatch table every registration is mirrored under.
const MIRROR: &str = "ClassicAPI";

/// The registrar: `_G`, the mirror, and the private table the bootstraps read.
pub(crate) struct Api<'a> {
    pub lua: &'a Lua,
    pub ca: Ca,
    g: Table,
    /// `CA`, the bootstraps' private natives.
    pub private: Table,
}

impl<'a> Api<'a> {
    fn new(lua: &'a Lua, ca: &Ca) -> mlua::Result<Self> {
        Ok(Self {
            lua,
            ca: ca.clone(),
            g: lua.globals(),
            private: lua.create_table()?,
        })
    }

    /// `_G[name]`, a table, created empty when it is not one.
    pub fn namespace(&self, name: &str) -> mlua::Result<Table> {
        ensure_table(self.lua, &self.g, name)
    }

    /// `RegisterGlobalFunction`: `_G[name]`, mirrored.
    pub fn global<A, R, F>(&self, name: &str, f: F) -> mlua::Result<()>
    where
        A: mlua::FromLuaMulti,
        R: IntoLuaMulti,
        F: Fn(&Lua, A) -> mlua::Result<R> + 'static,
    {
        let func = self.timed(name.to_string(), f)?;
        self.g.raw_set(name, func.clone())?;
        self.namespace(MIRROR)?.raw_set(name, func)
    }

    /// The native as a Lua function, timed into [`crate::prof`] when that is on.
    fn timed<A, R, F>(&self, name: String, f: F) -> mlua::Result<mlua::Function>
    where
        A: mlua::FromLuaMulti,
        R: IntoLuaMulti,
        F: Fn(&Lua, A) -> mlua::Result<R> + 'static,
    {
        if !crate::prof::enabled() {
            return self.lua.create_function(f);
        }
        self.lua.create_function(move |lua, a: A| {
            let started = std::time::Instant::now();
            let out = f(lua, a);
            crate::prof::native(&name, started.elapsed());
            out
        })
    }

    /// `RegisterTableFunction`: `_G[ns][name]`, mirrored at `ClassicAPI[ns][name]`.
    pub fn table<A, R, F>(&self, ns: &str, name: &str, f: F) -> mlua::Result<()>
    where
        A: mlua::FromLuaMulti,
        R: IntoLuaMulti,
        F: Fn(&Lua, A) -> mlua::Result<R> + 'static,
    {
        let func = self.timed(format!("{ns}.{name}"), f)?;
        self.namespace(ns)?.raw_set(name, func.clone())?;
        let mirror = self.namespace(MIRROR)?;
        ensure_table(self.lua, &mirror, ns)?.raw_set(name, func)
    }

    /// A Lua function as it is, at `_G[name]` or `_G[ns][name]`, mirrored: for a function a Rust
    /// wrapper would break, such as `coroutine.yield`, which cannot yield across a C call.
    pub fn function(&self, ns: Option<&str>, name: &str, f: mlua::Function) -> mlua::Result<()> {
        let mirror = self.namespace(MIRROR)?;
        match ns {
            None => {
                self.g.raw_set(name, f.clone())?;
                mirror.raw_set(name, f)
            }
            Some(ns) => {
                self.namespace(ns)?.raw_set(name, f.clone())?;
                ensure_table(self.lua, &mirror, ns)?.raw_set(name, f)
            }
        }
    }

    /// `RegisterIntegerEnum`: `_G[parent][sub] = { key = value, ... }`, a fresh table.
    pub fn int_enum(&self, parent: &str, sub: &str, entries: &[(&str, i64)]) -> mlua::Result<()> {
        let t = self.lua.create_table()?;
        for (k, v) in entries {
            t.set(*k, *v)?;
        }
        self.namespace(parent)?.set(sub, t)
    }

    /// `SetGlobalNumber`, a raw write.
    pub fn number(&self, name: &str, value: impl IntoLua) -> mlua::Result<()> {
        self.g.raw_set(name, value)
    }
}

/// `parent[name]`, made an empty table when it is not one (`EnsureSubTable`).
fn ensure_table(lua: &Lua, parent: &Table, name: &str) -> mlua::Result<Table> {
    if let Value::Table(t) = parent.raw_get::<Value>(name)? {
        return Ok(t);
    }
    let t = lua.create_table()?;
    parent.raw_set(name, t.clone())?;
    Ok(t)
}

/// Install every module's natives, then run the bootstraps; a failure is reported to the script
/// error handler and leaves the stock interface as it was.
pub fn install(ca: &Ca, script: &mut UiScript) {
    // SuperWoW's resolver owns `0x<hex>` literals when it is loaded; it installs first.
    let superwow: mlua::Value = script
        .lua()
        .globals()
        .get("SUPERWOW_VERSION")
        .unwrap_or(mlua::Value::Nil);
    ca.tokens.lock().guid_literals = matches!(superwow, mlua::Value::Nil);
    script.set_unit_token_extension(ca.tokens.extension());
    script.lua().set_app_data(ca.items.clone());
    addons::register(script);
    let bootstraps = {
        let lua = script.lua();
        let result = (|| -> mlua::Result<Table> {
            let api = Api::new(lua, ca)?;
            core::install(&api)?;
            baselib::install(&api)?;
            luasyntax::install(&api)?;
            spell::install(&api)?;
            nameplate::install(&api)?;
            aura::install(&api)?;
            item::install(&api)?;
            container::install(&api)?;
            equipmentset::install(&api)?;
            time::install(&api)?;
            addons::install(&api)?;
            frame::install(&api)?;
            lossofcontrol::install(&api)?;
            action::install(&api)?;
            misc::install(&api)?;
            info::install(&api)?;
            color::install(&api)?;
            playerinfo::install(&api)?;
            creature::install(&api)?;
            macro_api::install(&api)?;
            showtooltip::install(&api)?;
            itemscrape::install(&api)?;
            encoding::install(&api)?;
            cvar::install(&api)?;
            totem::install(&api)?;
            targeting::install(&api)?;
            gossip::install(&api)?;
            faction::install(&api)?;
            unit::install(&api)?;
            Ok(api.private)
        })();
        match result {
            Ok(private) => private,
            Err(e) => {
                script.report_script_error(&format!("ClassicAPI: {e}"));
                return;
            }
        }
    };
    for (name, src) in core::BOOTSTRAPS {
        if let Err(e) = run_bootstrap(script, name, src, &bootstraps) {
            script.report_script_error(&format!("ClassicAPI: {e}"));
        }
    }
}

/// Run one bootstrap as `function(CA) <src> end`, on the line it starts on so errors keep their
/// line numbers.
fn run_bootstrap(script: &UiScript, name: &str, src: &str, private: &Table) -> mlua::Result<()> {
    let lua = script.lua();
    let wrapped = format!("return function(CA) {src}\nend");
    let f: mlua::Function = lua
        .load(wrapped.as_bytes())
        .set_name(format!("@Interface\\ClassicAPI\\{name}"))
        .eval()?;
    f.call::<()>(private.clone())
}

/// No values: what a native's `return 0` hands Lua.
pub(crate) fn none() -> MultiValue {
    MultiValue::new()
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A bare VM with the API installed and no errors.
    pub fn vm(ca: &Ca) -> UiScript {
        let mut script = UiScript::new().expect("vm");
        install(ca, &mut script);
        assert_eq!(script.errors(), Vec::<String>::new());
        script
    }
}
