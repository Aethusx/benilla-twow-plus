//! `console/`: the console as Lua globals, over benilla's console ([`benilla_app::ext::ExtConsole`])
//! and its `ConsoleExec`.
//!
//! benilla's console has no screen: a command's output prints as system text in chat, and so does
//! `ConsoleEcho`. What follows from that, each a deviation from the DLL:
//! - `ConsoleIsActive` is always false, and `SetConsoleKey` has no screen to toggle.
//! - `ConsoleGetColorFromType` answers the system chat colour for every one of the nine types, the
//!   colour such a line prints in; `ConsoleGetFontHeight` answers nothing.
//! - benilla's commands carry no category, so every entry reports 5, `Enum.ConsoleCategory.Default`.

use benilla_app::ext::{ExtConsole, ExtConsoleCommand};
use mlua::{IntoLuaMulti, MultiValue, Table, Value};

use crate::lua::{is_number, none, to_int, to_str, Api};

/// The console's line colours (`CONSOLE_COLOR_COUNT`).
const COLOR_TYPES: i64 = 9;
/// `Enum.ConsoleCategory.Default`.
const CATEGORY_DEFAULT: i64 = 5;
/// `CalculateStringEditDistance`'s row cap: beyond it the answer is nil.
const MAX_ROW: usize = 256;

/// The console's state the natives and the frame share.
#[derive(Default)]
pub(crate) struct Console {
    commands: Vec<ExtConsoleCommand>,
    echo: Vec<String>,
}

impl Console {
    /// The frame's exchange: the list in when it moved, the queued lines out.
    pub(crate) fn sync(&mut self, ext: &mut ExtConsole) {
        if self.commands.len() != ext.commands.len() {
            self.commands = ext.commands.clone();
        }
        if !self.echo.is_empty() {
            ext.echo.append(&mut self.echo);
        }
    }
}

/// Levenshtein distance over bytes, two rolling rows indexed by the shorter string; `None` when
/// the shorter is `MAX_ROW` long or more.
fn edit_distance(a: &[u8], b: &[u8]) -> Option<usize> {
    let (long, short) = if b.len() > a.len() { (b, a) } else { (a, b) };
    if short.is_empty() {
        return Some(long.len());
    }
    if short.len() >= MAX_ROW {
        return None;
    }
    let mut prev: Vec<usize> = (0..=short.len()).collect();
    let mut curr = vec![0; short.len() + 1];
    for (i, &ca) in long.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in short.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    Some(prev[short.len()])
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.global(
        "ConsoleGetAllCommands",
        move |lua, ()| -> mlua::Result<Table> {
            let commands = c.lock().console.commands.clone();
            let out = lua.create_table()?;
            for (i, cmd) in commands.into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("command", cmd.name)?;
                t.set("help", cmd.help)?;
                t.set("category", CATEGORY_DEFAULT)?;
                t.set("commandType", if cmd.cvar { 0 } else { 1 })?;
                t.set("scriptContents", "")?;
                t.set("scriptParameters", "")?;
                out.raw_set(i + 1, t)?;
            }
            Ok(out)
        },
    )?;
    let c = api.ca.clone();
    api.global("ConsolePrintAllMatchingCommands", move |_, v: Value| {
        let Some(prefix) = to_str(&v).filter(|p| !p.is_empty()) else {
            return Ok(());
        };
        let prefix = prefix.to_ascii_lowercase();
        let mut st = c.lock();
        let matched: Vec<String> = st
            .console
            .commands
            .iter()
            .filter(|cmd| cmd.name.to_ascii_lowercase().starts_with(&prefix))
            .map(|cmd| cmd.name.clone())
            .collect();
        st.console.echo.extend(matched);
        Ok(())
    })?;
    let c = api.ca.clone();
    api.global("ConsoleEcho", move |_, (msg, _color): (Value, Value)| {
        if let Some(msg) = to_str(&msg) {
            c.lock().console.echo.push(msg);
        }
        Ok(())
    })?;
    api.global("ConsoleIsActive", |_, ()| Ok(false))?;
    api.global(
        "ConsoleGetColorFromType",
        |lua, v: Value| -> mlua::Result<MultiValue> {
            if !is_number(&v) || !(0..COLOR_TYPES).contains(&to_int(&v)) {
                return Value::Nil.into_lua_multi(lua);
            }
            let system: Option<Table> = lua
                .globals()
                .get::<Table>("ChatTypeInfo")
                .and_then(|t| t.get("SYSTEM"))
                .ok();
            let (r, g, b) = match system {
                Some(t) => (
                    t.get::<f64>("r").unwrap_or(1.0),
                    t.get::<f64>("g").unwrap_or(1.0),
                    t.get::<f64>("b").unwrap_or(0.0),
                ),
                None => (1.0, 1.0, 0.0),
            };
            (r, g, b, 1.0).into_lua_multi(lua)
        },
    )?;
    api.global("ConsoleGetFontHeight", |_, ()| Ok(none()))?;
    api.global("SetConsoleKey", |_, _: Value| Ok(()))?;
    api.global(
        "CalculateStringEditDistance",
        |_, (a, b): (Value, Value)| {
            let (Some(a), Some(b)) = (to_str(&a), to_str(&b)) else {
                return Ok(None);
            };
            Ok(edit_distance(a.as_bytes(), b.as_bytes()))
        },
    )?;
    api.int_enum(
        "Enum",
        "ConsoleCommandType",
        &[("Cvar", 0), ("Command", 1), ("Macro", 2), ("Script", 3)],
    )?;
    api.int_enum(
        "Enum",
        "ConsoleCategory",
        &[
            ("Debug", 0),
            ("Graphics", 1),
            ("Console", 2),
            ("Combat", 3),
            ("Game", 4),
            ("Default", 5),
            ("Net", 6),
            ("Sound", 7),
            ("Gm", 8),
            ("Reveal", 9),
            ("None", 10),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_is_levenshtein() {
        assert_eq!(edit_distance(b"kitten", b"sitting"), Some(3));
        assert_eq!(edit_distance(b"", b"abc"), Some(3));
        assert_eq!(edit_distance(b"farclip", b"farclip"), Some(0));
        assert_eq!(edit_distance(&[b'a'; 300], &[b'a'; 300]), None);
    }

    #[test]
    fn the_list_comes_in_and_lines_go_out() {
        let ca = crate::Ca::default();
        let script = crate::lua::test_support::vm(&ca);
        let mut ext = ExtConsole {
            commands: vec![
                ExtConsoleCommand {
                    name: "help".into(),
                    help: "List".into(),
                    cvar: false,
                },
                ExtConsoleCommand {
                    name: "farclip".into(),
                    help: String::new(),
                    cvar: true,
                },
            ],
            echo: vec![],
        };
        ca.lock().console.sync(&mut ext);
        let out: String = script
            .lua()
            .load(
                r#"
                local all = ConsoleGetAllCommands()
                ConsoleEcho("hello", 3)
                ConsolePrintAllMatchingCommands("FAR")
                return table.getn(all) .. all[1].command .. all[1].commandType
                  .. all[2].commandType .. all[2].category
                  .. CalculateStringEditDistance("farclp", "farclip")
                  .. tostring(ConsoleIsActive()) .. tostring(ConsoleGetColorFromType(9))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "2help1051falsenil");
        ca.lock().console.sync(&mut ext);
        assert_eq!(ext.echo, vec!["hello".to_string(), "farclip".to_string()]);
    }
}
