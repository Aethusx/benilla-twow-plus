//! `bindings/`: the direct-action and override binding APIs of 2.0, over benilla's binding seam
//! (`ext_read::set_binding_overrides`, `ext_read::set_binding_command_runner`), and
//! `macro/Execute.cpp`'s saved-macro runner.
//!
//! - `SetBindingSpell/Item/Macro/Click` store the later clients' `SPELL x`, `ITEM x`, `MACRO x`
//!   and `CLICK button[:mouseButton]` commands through the stock `SetBinding`, so its return,
//!   key normalization, `UPDATE_BINDINGS` and saving hold. The runner executes those four
//!   families on a press (a release is consumed and does nothing); a bad one is a no-op.
//! - Override bindings are owner-scoped, never saved: per key, a priority override beats a plain
//!   one and the later of equals wins; clearing an owner uncovers the next override or the stock
//!   binding. Keys are canonicalized to the engine's `ALT-CTRL-SHIFT-` order.
//! - A macro runs through an addon's `RunMacro` when one exists, else by index or name through
//!   `GetMacroInfo`, each line through `ChatEdit_ParseText`, `#` lines skipped.

use std::collections::HashMap;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::lua::{as_string, to_int, to_str, Api};

const SPELL: &str = "SPELL ";
const ITEM: &str = "ITEM ";
const MACRO: &str = "MACRO ";
const CLICK: &str = "CLICK ";

/// One override.
#[derive(Clone, Debug, PartialEq)]
struct Override {
    owner: u32,
    key: String,
    command: String,
    priority: bool,
    seq: u64,
}

/// The override layer.
#[derive(Default)]
pub(crate) struct Overrides {
    entries: Vec<Override>,
    seq: u64,
}

impl Overrides {
    fn set(&mut self, owner: u32, priority: bool, key: String, command: String) {
        self.seq += 1;
        let seq = self.seq;
        match self
            .entries
            .iter_mut()
            .find(|e| e.owner == owner && e.key == key)
        {
            Some(e) => {
                e.command = command;
                e.priority = priority;
                e.seq = seq;
            }
            None => self.entries.push(Override {
                owner,
                key,
                command,
                priority,
                seq,
            }),
        }
    }

    fn remove(&mut self, owner: u32, key: &str) {
        self.entries.retain(|e| !(e.owner == owner && e.key == key));
    }

    fn clear(&mut self, owner: u32) {
        self.entries.retain(|e| e.owner != owner);
    }

    /// `FindEffectiveOverride` per key: priority first, then the latest.
    fn effective(&self) -> Vec<(String, String)> {
        let mut best: HashMap<&str, &Override> = HashMap::new();
        for e in &self.entries {
            let wins = best
                .get(e.key.as_str())
                .is_none_or(|b| (e.priority, e.seq) > (b.priority, b.seq));
            if wins {
                best.insert(&e.key, e);
            }
        }
        let mut out: Vec<(String, String)> = best
            .into_values()
            .map(|e| (e.key.clone(), e.command.clone()))
            .collect();
        out.sort();
        out
    }
}

/// `CanonicalKey`: upper case, the modifiers in the engine's `ALT-CTRL-SHIFT-` order.
pub(crate) fn canonical_key(key: &str) -> String {
    let up = key.to_ascii_uppercase();
    let (mut alt, mut ctrl, mut shift) = (false, false, false);
    let mut rest = up.as_str();
    loop {
        if let Some(r) = rest.strip_prefix("ALT-") {
            alt = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("CTRL-") {
            ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("SHIFT-") {
            shift = true;
            rest = r;
        } else {
            break;
        }
    }
    let mut out = String::new();
    for (on, m) in [(alt, "ALT-"), (ctrl, "CTRL-"), (shift, "SHIFT-")] {
        if on {
            out.push_str(m);
        }
    }
    out.push_str(rest);
    out
}

/// `MakeClickCommand`.
fn click_command(button: &str, mouse: Option<&str>) -> String {
    match mouse {
        Some(m) => format!("{CLICK}{button}:{m}"),
        None => format!("{CLICK}{button}"),
    }
}

fn call_global(lua: &Lua, name: &str, args: impl mlua::IntoLuaMulti) {
    if let Ok(f) = lua.globals().get::<Function>(name) {
        let _ = f.call::<()>(args);
    }
}

/// `Macro::Execute::Text`: each line through `ChatEdit_ParseText` on a throwaway edit box.
fn run_macro_text(lua: &Lua, text: &str) -> mlua::Result<()> {
    let Ok(parse) = lua.globals().get::<Function>("ChatEdit_ParseText") else {
        return Ok(());
    };
    let noop = lua.create_function(|_, _: MultiValue| Ok(()))?;
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        lua.create_function(move |_, _: MultiValue| Ok(noop.clone()))?,
    )?;
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r').trim_start_matches([' ', '\t']);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let edit = lua.create_table()?;
        let owned = line.to_string();
        edit.set(
            "GetText",
            lua.create_function(move |_, _: MultiValue| Ok(owned.clone()))?,
        )?;
        edit.set_metatable(Some(meta.clone()))?;
        let _ = parse.call::<()>((edit, 1));
    }
    Ok(())
}

/// `Macro::Execute::Saved`: an addon's `RunMacro`, else the macro's body by index or name.
fn run_saved_macro(lua: &Lua, macro_name: &str) -> mlua::Result<()> {
    let index = macro_name.parse::<i64>().ok().filter(|i| *i > 0);
    if let Ok(run) = lua.globals().get::<Function>("RunMacro") {
        let arg = match index {
            Some(i) => Value::Integer(i),
            None => Value::String(lua.create_string(macro_name)?),
        };
        let _ = run.call::<()>(arg);
        return Ok(());
    }
    let index = match index {
        Some(i) => i,
        None => match lua.globals().get::<Function>("GetMacroIndexByName") {
            Ok(f) => f.call::<Value>(macro_name).map(|v| to_int(&v)).unwrap_or(0),
            Err(_) => 0,
        },
    };
    if index <= 0 {
        return Ok(());
    }
    let Ok(info) = lua.globals().get::<Function>("GetMacroInfo") else {
        return Ok(());
    };
    let rets: MultiValue = info.call(index).unwrap_or_default();
    if let Some(body) = rets.get(2).and_then(as_string) {
        run_macro_text(lua, &body)?;
    }
    Ok(())
}

/// `ExecuteClick`: `button[:mouseButton]`, `LeftButton` by default, through the button's `Click`.
fn run_click(lua: &Lua, arg: &str) {
    let (name, mouse) = match arg.rfind(':') {
        Some(i) => (&arg[..i], &arg[i + 1..]),
        None => (arg, "LeftButton"),
    };
    if name.is_empty() || mouse.is_empty() {
        return;
    }
    if let Ok(Value::Table(button)) = lua.globals().get::<Value>(name) {
        if let Ok(click) = button.get::<Function>("Click") {
            let _ = click.call::<()>((button, mouse));
        }
    }
}

/// `ExecuteSpecialCommand`: whether `command` is one of the four families (consumed, malformed
/// or not), running it on a press.
fn run_command(lua: &Lua, command: &str, down: bool) -> mlua::Result<bool> {
    let (family, arg) = [SPELL, ITEM, MACRO, CLICK]
        .into_iter()
        .find_map(|p| command.strip_prefix(p).map(|a| (p, a)))
        .unzip();
    let (Some(family), Some(arg)) = (family, arg) else {
        return Ok(false);
    };
    if !down || arg.is_empty() {
        return Ok(true);
    }
    match family {
        SPELL => call_global(lua, "CastSpellByName", arg),
        ITEM => {
            if let Ok(t) = lua.globals().get::<Table>("C_Item") {
                if let Ok(f) = t.get::<Function>("UseItemByName") {
                    let _ = f.call::<()>(arg);
                }
            }
        }
        MACRO => run_saved_macro(lua, arg)?,
        _ => run_click(lua, arg),
    }
    Ok(true)
}

/// The owner a native's first argument names: a frame's id.
fn owner_arg(v: &Value, usage: &str) -> mlua::Result<u32> {
    benilla_ui::script::ext_read::frame_id(v).ok_or_else(|| mlua::Error::runtime(usage.to_string()))
}

/// Push the effective layer to benilla.
fn publish(lua: &Lua, ca: &crate::Ca) {
    let layer = ca.lock().overrides.effective();
    benilla_ui::script::ext_read::set_binding_overrides(lua, layer);
}

/// The stock `SetBinding(key, command)` with a built command.
fn set_binding(lua: &Lua, key: &str, command: String) -> mlua::Result<MultiValue> {
    let f: Function = lua.globals().get("SetBinding")?;
    f.call((key, command))
}

fn strings(a: &Value, b: &Value, usage: &str) -> mlua::Result<(String, String)> {
    match (to_str(a), to_str(b)) {
        (Some(a), Some(b)) => Ok((a, b)),
        _ => Err(mlua::Error::runtime(usage.to_string())),
    }
}

/// An optional mouse button: nil or absent is none, a non-string the usage error.
fn mouse_arg(v: &Value, usage: &str) -> mlua::Result<Option<String>> {
    match v {
        Value::Nil => Ok(None),
        v => to_str(v)
            .map(Some)
            .ok_or_else(|| mlua::Error::runtime(usage.to_string())),
    }
}

/// `ReadOverrideHeader`: `(owner, isPriority, key)`, the priority a boolean proper.
fn header(
    owner: &Value,
    priority: &Value,
    key: &Value,
    usage: &str,
) -> mlua::Result<(u32, bool, String)> {
    let owner = owner_arg(owner, usage)?;
    let Value::Boolean(priority) = priority else {
        return Err(mlua::Error::runtime(usage.to_string()));
    };
    let key = to_str(key).ok_or_else(|| mlua::Error::runtime(usage.to_string()))?;
    Ok((owner, *priority, canonical_key(&key)))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let runner = api
        .lua
        .create_function(|lua, (command, down): (String, bool)| run_command(lua, &command, down))?;
    benilla_ui::script::ext_read::set_binding_command_runner(api.lua, runner)?;

    for (name, prefix, usage) in [
        (
            "SetBindingSpell",
            SPELL,
            "Usage: SetBindingSpell(key, spell)",
        ),
        ("SetBindingItem", ITEM, "Usage: SetBindingItem(key, item)"),
        (
            "SetBindingMacro",
            MACRO,
            "Usage: SetBindingMacro(key, macro)",
        ),
    ] {
        api.global(name, move |lua, (k, v): (Value, Value)| {
            let (key, arg) = strings(&k, &v, usage)?;
            set_binding(lua, &key, format!("{prefix}{arg}"))
        })?;
    }
    api.global(
        "SetBindingClick",
        |lua, (k, b, m): (Value, Value, Value)| {
            const USAGE: &str = "Usage: SetBindingClick(key, buttonName [, mouseButton])";
            let (key, button) = strings(&k, &b, USAGE)?;
            let mouse = mouse_arg(&m, USAGE)?;
            set_binding(lua, &key, click_command(&button, mouse.as_deref()))
        },
    )?;

    let c = api.ca.clone();
    api.global(
        "SetOverrideBinding",
        move |lua, (o, p, k, cmd): (Value, Value, Value, Value)| {
            const USAGE: &str = "Usage: SetOverrideBinding(owner, isPriority, key [, command])";
            let (owner, priority, key) = header(&o, &p, &k, USAGE)?;
            match cmd {
                Value::Nil => c.lock().overrides.remove(owner, &key),
                v => {
                    let command = to_str(&v).ok_or_else(|| mlua::Error::runtime(USAGE))?;
                    c.lock().overrides.set(owner, priority, key, command);
                }
            }
            publish(lua, &c);
            Ok(())
        },
    )?;
    for (name, prefix, usage) in [
        (
            "SetOverrideBindingSpell",
            SPELL,
            "Usage: SetOverrideBindingSpell(owner, isPriority, key, spell)",
        ),
        (
            "SetOverrideBindingItem",
            ITEM,
            "Usage: SetOverrideBindingItem(owner, isPriority, key, item)",
        ),
        (
            "SetOverrideBindingMacro",
            MACRO,
            "Usage: SetOverrideBindingMacro(owner, isPriority, key, macro)",
        ),
    ] {
        let c = api.ca.clone();
        api.global(
            name,
            move |lua, (o, p, k, arg): (Value, Value, Value, Value)| {
                let (owner, priority, key) = header(&o, &p, &k, usage)?;
                let arg = to_str(&arg).ok_or_else(|| mlua::Error::runtime(usage))?;
                c.lock()
                    .overrides
                    .set(owner, priority, key, format!("{prefix}{arg}"));
                publish(lua, &c);
                Ok(())
            },
        )?;
    }
    let c = api.ca.clone();
    api.global(
        "SetOverrideBindingClick",
        move |lua, (o, p, k, b, m): (Value, Value, Value, Value, Value)| {
            const USAGE: &str =
                "Usage: SetOverrideBindingClick(owner, isPriority, key, buttonName [, mouseButton])";
            let (owner, priority, key) = header(&o, &p, &k, USAGE)?;
            let button = to_str(&b).ok_or_else(|| mlua::Error::runtime(USAGE))?;
            let mouse = mouse_arg(&m, USAGE)?;
            c.lock()
                .overrides
                .set(owner, priority, key, click_command(&button, mouse.as_deref()));
            publish(lua, &c);
            Ok(())
        },
    )?;
    let c = api.ca.clone();
    api.global("ClearOverrideBindings", move |lua, o: Value| {
        let owner = owner_arg(&o, "Usage: ClearOverrideBindings(owner)")?;
        c.lock().overrides.clear(owner);
        publish(lua, &c);
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_canonicalize_and_the_layer_picks_priority_then_latest() {
        assert_eq!(canonical_key("shift-ctrl-1"), "CTRL-SHIFT-1");
        assert_eq!(canonical_key("SHIFT--"), "SHIFT--");
        assert_eq!(canonical_key("alt-shift-alt-q"), "ALT-SHIFT-Q");
        let mut o = Overrides::default();
        o.set(1, true, "Q".into(), "A".into());
        o.set(2, false, "Q".into(), "B".into());
        o.set(2, false, "E".into(), "C".into());
        o.set(3, false, "E".into(), "D".into());
        assert_eq!(
            o.effective(),
            vec![("E".into(), "D".into()), ("Q".into(), "A".into())]
        );
        o.clear(1);
        o.remove(3, "E");
        assert_eq!(
            o.effective(),
            vec![("E".into(), "C".into()), ("Q".into(), "B".into())]
        );
    }

    #[test]
    fn a_bound_spell_click_and_macro_run_on_the_press() {
        let ca = crate::Ca::default();
        let script = crate::lua::test_support::vm(&ca);
        script
            .run(
                r##"
                Log = ""
                CastSpellByName = function(s) Log = Log .. "cast:" .. s .. " " end
                Btn = CreateFrame("Button", "Btn")
                Btn:SetScript("OnClick", function() Log = Log .. "click:" .. arg1 .. " " end)
                GetMacroIndexByName = function(n) if n == "Go" then return 2 end return 0 end
                GetMacroInfo = function(i) return "Go", "icon", "#showtooltip\n  /say hi\n/cast X" end
                ChatEdit_ParseText = function(box) box:SetText("x"); Log = Log .. "line:" .. box:GetText() .. " " end
                SetBinding("F", "SPELL Fireball")
                F = CreateFrame("Frame")
                SetOverrideBindingClick(F, false, "shift-g", "Btn", "RightButton")
                "##,
            )
            .unwrap();
        assert!(script.execute_binding("SPELL Fireball", true).unwrap());
        assert!(script.execute_binding("SPELL Fireball", false).unwrap());
        assert!(script
            .execute_binding("CLICK Btn:RightButton", true)
            .unwrap());
        assert!(script.execute_binding("MACRO Go", true).unwrap());
        assert!(!script.execute_binding("NOT A FAMILY", true).unwrap());
        assert_eq!(
            script.eval::<String>("return Log").unwrap(),
            "cast:Fireball click:RightButton line:/say hi line:/cast X "
        );
        assert!(script
            .dispatch_keys()
            .contains(&("SHIFT-G".to_string(), "CLICK Btn:RightButton".to_string())));
        script.run("ClearOverrideBindings(F)").unwrap();
        assert!(!script.dispatch_keys().iter().any(|(k, _)| k == "SHIFT-G"));
    }
    #[test]
    fn the_bundled_bindings_parse() {
        let xml = include_str!("../../addon/!!!ClassicAPI/Bindings.xml");
        let names: Vec<String> = benilla_ui::bindings_xml::parse(xml)
            .expect("parses")
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(names, ["FOCUSTARGET", "TARGETFOCUS", "AOELOOT"]);
    }
}
