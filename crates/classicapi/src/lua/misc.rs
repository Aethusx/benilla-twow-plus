//! The small standalone backports:
//!
//! - `combat/Attack.cpp`: `StartAttack([unit])`, `StopAttack()`, through benilla's own
//!   StartAttack / StopAttack ([`ExtAttack`]); a unit that does not resolve falls back to the
//!   selection.
//! - `player/Facing.cpp`: `GetPlayerFacing()`, radians.
//! - `group/Info.cpp`: `IsInRaid()`, `IsInGroup()`.
//! - `input/Modifier.cpp`: `IsLeft/RightShift/Control/AltKeyDown()`, `IsModifierKeyDown()`, each
//!   1 or nil, and `MODIFIER_STATE_CHANGED(key, down)` on each edge, from benilla's keyboard state
//!   where the DLL hooks the window's key messages.
//! - `screen/Info.cpp`: `GetPhysicalScreenSize()`, the `gxResolution` CVar's `WxH`, 1024x768
//!   when it does not parse.
//! - `frame/ClickFrame.cpp`: `GetClickFrame(name)`, the global of that name when it is a frame of
//!   that name.
//! - `currency/CoinText.cpp`: `GetCoinTextureString(copper [, height])` and its `C_CurrencyInfo`
//!   twin, each nonzero denomination through its `*_AMOUNT_TEXTURE` GlobalStrings template.
//! - `HookSecureFunc.cpp`: `hooksecurefunc([table,] name, callback)`, the callback run after the
//!   original with its arguments, its errors swallowed, the original's results returned.

use benilla_app::ext::ExtAttack;
use mlua::{Function, Value};

use crate::lua::unit::unit_guid;
use crate::lua::{to_str, Api};

/// The modifier keys by `MODIFIER_STATE_CHANGED` name, in the DLL's bit order.
pub(crate) const MODIFIER_KEYS: [&str; 6] = ["LSHIFT", "RSHIFT", "LCTRL", "RCTRL", "LALT", "RALT"];

/// `hooksecurefunc`, in the client's 5.0 dialect: a vararg reads `arg`, whose `n` keeps nil holes.
const HOOKSECUREFUNC: &str = r#"
return function(G)
local type, pcall, unpack, error = type, pcall, unpack, error
local unhookable = {}
for _, name in {
  "getfenv", "getmetatable", "hooksecurefunc", "ipairs", "issecurevalue", "issecurevariable",
  "next", "pairs", "pcall", "pcallwithenv", "rawget", "rawset", "scrub", "securecall",
  "securecallfunction", "secureexecuterange", "select", "setfenv", "setmetatable", "type",
  "unpack", "wipe", "xpcall",
} do
  unhookable[name] = true
end
local function pack(...)
  return arg
end
return function(a, b, c)
  local t, name, callback
  if type(a) == "table" then
    t, name, callback = a, b, c
  elseif type(a) == "string" then
    t, name, callback = G, a, b
  else
    error("hooksecurefunc(table, name, callback) or hooksecurefunc(name, callback)", 2)
  end
  if type(name) ~= "string" then
    error("hooksecurefunc: name must be a string", 2)
  end
  if type(callback) ~= "function" then
    error("hooksecurefunc: callback must be a function", 2)
  end
  if t == G and unhookable[name] then
    error("hooksecurefunc: function is unhookable", 2)
  end
  local original = t[name]
  if type(original) ~= "function" then
    error("hooksecurefunc: target field is not a function", 2)
  end
  t[name] = function(...)
    local results = pack(original(unpack(arg, 1, arg.n)))
    pcall(callback, unpack(arg, 1, arg.n))
    return unpack(results, 1, results.n)
  end
end
end
"#;

/// The DLL's templates for a client whose GlobalStrings lacks the `*_AMOUNT_TEXTURE` keys.
const COIN_FALLBACK: [(&str, &str); 3] = [
    (
        "GOLD_AMOUNT_TEXTURE",
        r"%d|TInterface\MoneyFrame\UI-MoneyIcons:%d:%d:0:0:4:1:0:1:0:1|t",
    ),
    (
        "SILVER_AMOUNT_TEXTURE",
        r"%d|TInterface\MoneyFrame\UI-MoneyIcons:%d:%d:0:0:4:1:1:2:0:1|t",
    ),
    (
        "COPPER_AMOUNT_TEXTURE",
        r"%d|TInterface\MoneyFrame\UI-MoneyIcons:%d:%d:0:0:4:1:2:3:0:1|t",
    ),
];

/// `WxH` to its two numbers; any non-digits separate them.
fn parse_resolution(s: &str) -> Option<(i64, i64)> {
    let w: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = &s[w.len()..];
    let h: String = rest
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let (w, h) = (w.parse().ok()?, h.parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

fn coin_string(lua: &mlua::Lua, copper: f64, height: i64) -> String {
    let amount = (if copper < 0.0 {
        copper - 0.5
    } else {
        copper + 0.5
    } as i64)
        .clamp(0, 0xFFFF_FFFF);
    let parts = [amount / 10000, amount % 10000 / 100, amount % 100];
    let mut out = String::new();
    for (i, (key, fallback)) in COIN_FALLBACK.iter().enumerate() {
        let n = parts[i];
        if !(n > 0 || (amount == 0 && i == 2)) {
            continue;
        }
        let template =
            benilla_ui::strings::global(lua, key).unwrap_or_else(|| fallback.to_string());
        use benilla_ui::strings::Arg::D;
        out.push_str(&benilla_ui::strings::fill(
            &template,
            &[D(n), D(height), D(height)],
        ));
    }
    out
}

/// 1 or nil, as the stock modifier queries answer.
fn flag(down: bool) -> Option<i64> {
    down.then_some(1)
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let c = api.ca.clone();
    api.global("StartAttack", move |lua, v: Value| {
        let guid = match to_str(&v).filter(|t| !t.is_empty()) {
            Some(token) => unit_guid(lua, &token).ok().flatten().unwrap_or(0),
            None => 0,
        };
        let mut st = c.lock();
        if st.mirror.player != 0 {
            st.attacks.push(ExtAttack(Some(guid)));
        }
        Ok(())
    })?;
    let c = api.ca.clone();
    api.global("StopAttack", move |_, ()| {
        let mut st = c.lock();
        if st.mirror.player != 0 {
            st.attacks.push(ExtAttack(None));
        }
        Ok(())
    })?;

    let c = api.ca.clone();
    api.global("GetPlayerFacing", move |_, ()| {
        let st = c.lock();
        Ok(st
            .mirror
            .place(st.mirror.player)
            .map(|p| f64::from(p.facing)))
    })?;

    let c = api.ca.clone();
    api.global("IsInRaid", move |_, ()| Ok(c.lock().mirror.in_raid))?;
    let c = api.ca.clone();
    api.global("IsInGroup", move |_, ()| {
        let st = c.lock();
        Ok(st.mirror.in_raid || st.mirror.in_group)
    })?;

    for (i, name) in [
        "IsLeftShiftKeyDown",
        "IsRightShiftKeyDown",
        "IsLeftControlKeyDown",
        "IsRightControlKeyDown",
        "IsLeftAltKeyDown",
        "IsRightAltKeyDown",
    ]
    .into_iter()
    .enumerate()
    {
        let c = api.ca.clone();
        api.global(name, move |_, ()| {
            Ok(flag(c.lock().modifiers & (1 << i) != 0))
        })?;
    }
    let c = api.ca.clone();
    api.global("IsModifierKeyDown", move |_, ()| {
        Ok(flag(c.lock().modifiers & 0x3f != 0))
    })?;

    let get_cvar: Option<Function> = api.lua.globals().get("GetCVar").ok();
    api.global("GetPhysicalScreenSize", move |_, ()| {
        let res = match &get_cvar {
            Some(f) => f.call::<Option<String>>("gxResolution").ok().flatten(),
            None => None,
        };
        Ok(res
            .as_deref()
            .and_then(parse_resolution)
            .unwrap_or((1024, 768)))
    })?;

    api.global("GetClickFrame", |lua, args: mlua::MultiValue| {
        let Some(v) = args.into_iter().next() else {
            return Err(mlua::Error::runtime("Usage: GetClickFrame(\"name\")"));
        };
        let Some(name) = to_str(&v) else {
            return Ok(Value::Nil);
        };
        let found: Value = lua.globals().get(name.as_str())?;
        if benilla_ui::script::ext_read::frame_id(&found).is_none() {
            return Ok(Value::Nil);
        }
        let Value::Table(t) = &found else {
            return Ok(Value::Nil);
        };
        let own: Option<String> = mlua::ObjectLike::call_method(t, "GetName", ())
            .ok()
            .flatten();
        Ok(if own.as_deref() == Some(name.as_str()) {
            found
        } else {
            Value::Nil
        })
    })?;

    let coin = |lua: &mlua::Lua, (amount, height): (Value, Value)| {
        if !crate::lua::is_number(&amount) {
            return Err(mlua::Error::runtime(
                "Usage: GetCoinTextureString(amount [, fontHeight])",
            ));
        }
        let h = if crate::lua::is_number(&height) {
            crate::lua::to_int(&height).max(0)
        } else {
            0
        };
        Ok(coin_string(lua, crate::lua::to_number(&amount), h))
    };
    api.global("GetCoinTextureString", coin)?;
    api.table("C_CurrencyInfo", "GetCoinTextureString", coin)?;

    let factory: Function = api
        .lua
        .load(HOOKSECUREFUNC)
        .set_name("=ClassicAPI hooksecurefunc")
        .eval()?;
    let hook: Function = factory.call::<Function>(api.lua.globals())?;
    api.function(None, "hooksecurefunc", hook)?;
    Ok(())
}

/// The modifier mask from the keyboard: bit `i` is [`MODIFIER_KEYS`]`[i]` held.
pub(crate) fn modifier_mask(keys: &bevy::input::ButtonInput<bevy::input::keyboard::KeyCode>) -> u8 {
    use bevy::input::keyboard::KeyCode as K;
    [
        K::ShiftLeft,
        K::ShiftRight,
        K::ControlLeft,
        K::ControlRight,
        K::AltLeft,
        K::AltRight,
    ]
    .iter()
    .enumerate()
    .fold(0, |m, (i, k)| if keys.pressed(*k) { m | 1 << i } else { m })
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;

    #[test]
    fn a_hook_runs_after_the_original_and_keeps_its_results() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                local log = ""
                T = { f = function(a, b, c) log = log .. "f" .. tostring(c); return a, nil, b end }
                hooksecurefunc(T, "f", function(a, b, c) log = log .. "h" .. tostring(c); error("swallowed") end)
                local x, y, z = T.f(1, 2, 3)
                function G1() return "g" end
                hooksecurefunc("G1", function() log = log .. "!" end)
                local g = G1()
                local ok = pcall(hooksecurefunc, "pairs", function() end)
                return log .. " " .. tostring(x) .. tostring(y) .. tostring(z) .. " " .. g .. " " .. tostring(ok)
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "f3h3! 1nil2 g false");
    }

    #[test]
    fn coins_resolutions_and_click_frames() {
        assert_eq!(super::parse_resolution("1920x1080"), Some((1920, 1080)));
        assert_eq!(super::parse_resolution("bad"), None);
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                GOLD_AMOUNT_TEXTURE = "%dg"
                SILVER_AMOUNT_TEXTURE = "%ds"
                COPPER_AMOUNT_TEXTURE = "%dc"
                local f = CreateFrame("Frame", "CAPIClickTest")
                CAPIFake = {}
                return GetCoinTextureString(10203) .. " " .. GetCoinTextureString(0)
                  .. " " .. tostring(GetClickFrame("CAPIClickTest") == f)
                  .. " " .. tostring(GetClickFrame("CAPIFake"))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "1g2s3c 0c true nil");
    }
}
