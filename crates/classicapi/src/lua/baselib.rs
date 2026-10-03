//! `baselib/`, `table/` and `coroutine/`: the Lua 5.1 library members 1.12's Lua 5.0 lacks, and
//! WoW's later string helpers.
//!
//! `coroutine` comes back as the VM's own 5.1 library, which the state still holds in its
//! `_LOADED` registry table: the DLL rebuilds the same six functions over 5.0's linked-in thread
//! machinery. Deviation: `table/Length.cpp`'s `luaL_getn` border repair is not ported; it changes
//! the stock `table.getn` every addon reads, which is benilla's own 5.0 table size.

use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Table, Value, Variadic};

use crate::lua::{is_number, to_int, to_number, Api};

/// The C runtime's `isspace` set, `strtrim`'s default.
const TRIM_DEFAULT: &[u8] = b" \t\n\x0b\x0c\r";

fn bytes(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::String(s) => Some(s.as_bytes().to_vec()),
        Value::Integer(_) | Value::Number(_) => crate::lua::to_str(v).map(String::into_bytes),
        _ => None,
    }
}

fn arg(args: &Variadic<Value>, i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Nil)
}

/// `strsplit(separators, str [, pieces])`: split on any separator byte, at most `pieces` pieces.
fn strsplit(lua: &Lua, args: Variadic<Value>) -> mlua::Result<MultiValue> {
    let (Some(sep), Some(s)) = (bytes(&arg(&args, 0)), bytes(&arg(&args, 1))) else {
        return Err(mlua::Error::runtime(
            "Usage: strsplit(\"separators\", str [, pieces])",
        ));
    };
    let pieces = if is_number(&arg(&args, 2)) {
        to_int(&arg(&args, 2))
    } else {
        0
    };
    let mut out = Vec::new();
    let mut start = 0;
    if pieces == 0 || pieces > 1 {
        for (i, b) in s.iter().enumerate() {
            if !sep.contains(b) {
                continue;
            }
            out.push(Value::String(lua.create_string(&s[start..i])?));
            start = i + 1;
            if out.len() as i64 == pieces - 1 {
                break;
            }
        }
    }
    out.push(Value::String(lua.create_string(&s[start..])?));
    Ok(MultiValue::from_vec(out))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let lua = api.lua;
    let g = lua.globals();

    // `select(n, ...)`: `"#"` counts; a negative n counts back from the end.
    api.global("select", |_, args: Variadic<Value>| {
        let n = args.len() as i64;
        if n < 1 {
            return Err(mlua::Error::runtime(
                "bad argument #1 to 'select' (number expected, got no value)",
            ));
        }
        if let Value::String(s) = &args[0] {
            if s.as_bytes().first() == Some(&b'#') {
                return Ok(MultiValue::from_vec(vec![Value::Integer(n - 1)]));
            }
        }
        if !is_number(&args[0]) {
            return Err(mlua::Error::runtime(
                "bad argument #1 to 'select' (number expected)",
            ));
        }
        let mut i = to_int(&args[0]);
        if i < 0 {
            i += n;
        } else if i > n {
            i = n;
        }
        if i < 1 {
            return Err(mlua::Error::runtime(
                "bad argument #1 to 'select' (index out of range)",
            ));
        }
        Ok(args.iter().skip(i as usize).cloned().collect())
    })?;

    // `unpack(t [, i [, j]])`: 5.1's range, `j` defaulting to the 5.0 size.
    let getn: Function = g.get::<Table>("table")?.get("getn")?;
    api.global("unpack", move |_, args: Variadic<Value>| {
        let Value::Table(t) = arg(&args, 0) else {
            return Err(mlua::Error::runtime(
                "bad argument #1 to 'unpack' (table expected)",
            ));
        };
        let opt = |i: usize, fallback: i64| -> mlua::Result<i64> {
            match arg(&args, i) {
                Value::Nil => Ok(fallback),
                v if is_number(&v) => Ok(to_int(&v)),
                _ => Err(mlua::Error::runtime(format!(
                    "bad argument #{} to 'unpack' (number expected)",
                    i + 1
                ))),
            }
        };
        let first = opt(1, 1)?;
        let last = opt(2, getn.call::<i64>(t.clone())?)?;
        if first > last {
            return Ok(MultiValue::new());
        }
        if last - first >= 8000 {
            return Err(mlua::Error::runtime("too many results to unpack"));
        }
        (first..=last).map(|k| t.raw_get::<Value>(k)).collect()
    })?;

    // `xpcall(f, handler, ...)`: the arguments pass through, through 5.0's own xpcall.
    let xpcall: Function = lua
        .load(
            "local orig, unpack, error = xpcall, unpack, error\n\
             return function(...)\n\
               if arg.n < 2 then error(\"bad argument #2 to 'xpcall' (value expected)\", 2) end\n\
               local f, h, a = arg[1], arg[2], arg\n\
               return orig(function() return f(unpack(a, 3, a.n)) end, h)\n\
             end",
        )
        .set_name("=ClassicAPI xpcall")
        .eval()?;
    api.function(None, "xpcall", xpcall)?;

    // `collectgarbage(opt)`: 5.1's options over 5.0's collector.
    let collect: Option<Function> = g.get("collectgarbage").ok();
    let gcinfo: Option<Function> = g.get("gcinfo").ok();
    api.global("collectgarbage", move |lua, args: MultiValue| {
        let first = args.front().cloned().unwrap_or(Value::Nil);
        if let Value::String(s) = &first {
            let opt = s.to_string_lossy();
            return match opt.as_str() {
                "count" => match &gcinfo {
                    Some(f) => Ok(f.call::<MultiValue>(())?.into_iter().take(1).collect()),
                    None => 0.into_lua_multi(lua),
                },
                "collect" => {
                    if let Some(f) = &collect {
                        f.call::<()>(0)?;
                    }
                    0.into_lua_multi(lua)
                }
                "step" => true.into_lua_multi(lua),
                "stop" | "restart" | "setpause" | "setstepmul" => 0.into_lua_multi(lua),
                _ => Err(mlua::Error::runtime(format!(
                    "bad argument #1 to 'collectgarbage' (invalid option '{opt}')"
                ))),
            };
        }
        match &collect {
            Some(f) => f.call::<MultiValue>(args),
            None => Ok(MultiValue::new()),
        }
    })?;

    // `math.fmod` (5.0's `math.mod`), `math.modf`, `math.huge`.
    let math: Table = g.get("math")?;
    if let Ok(Value::Function(m)) = math.get::<Value>("mod") {
        api.function(Some("math"), "fmod", m)?;
    }
    api.table("math", "modf", |_, v: Value| {
        if !is_number(&v) {
            return Err(mlua::Error::runtime("Usage: math.modf(x)"));
        }
        let x = to_number(&v);
        Ok((x.trunc(), x - x.trunc()))
    })?;
    math.set("huge", f64::INFINITY)?;

    // `Mixin(object, ...)` and `CreateFromMixins(...)`: every pair of each table copied in.
    api.global("Mixin", |_, args: Variadic<Value>| {
        let Some(Value::Table(obj)) = args.first().cloned() else {
            return Err(mlua::Error::runtime("Usage: Mixin(object, ...)"));
        };
        for src in args.iter().skip(1) {
            if let Value::Table(src) = src {
                for pair in src.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    obj.set(k, v)?;
                }
            }
        }
        Ok(obj)
    })?;
    api.global("CreateFromMixins", |lua, args: Variadic<Value>| {
        let obj = lua.create_table()?;
        for src in args.iter() {
            if let Value::Table(src) = src {
                for pair in src.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    obj.set(k, v)?;
                }
            }
        }
        Ok(obj)
    })?;

    // `string.match(s, pattern [, init])`: `string.find`'s captures, or the whole match.
    let string: Table = g.get("string")?;
    let find: Function = string.get("find")?;
    api.table(
        "string",
        "match",
        move |lua, (s, p, init): (Value, Value, Value)| {
            let out: Vec<Value> = find
                .call::<MultiValue>((s.clone(), p, init))?
                .into_iter()
                .collect();
            match out.len() {
                0 | 1 => Ok(Value::Nil.into_lua_multi(lua)?),
                2 => {
                    let (a, b) = (to_int(&out[0]), to_int(&out[1]));
                    let s = bytes(&s).unwrap_or_default();
                    let piece = if b >= a && a >= 1 {
                        &s[(a - 1) as usize..(b as usize).min(s.len())]
                    } else {
                        &[][..]
                    };
                    Value::String(lua.create_string(piece)?).into_lua_multi(lua)
                }
                _ => Ok(out.into_iter().skip(2).collect()),
            }
        },
    )?;
    // `string.gmatch`, 5.0's `gfind`.
    if let Ok(Value::Function(gfind)) = string.get::<Value>("gfind") {
        api.function(Some("string"), "gmatch", gfind)?;
    }
    let reverse = |lua: &Lua, v: Value| -> mlua::Result<mlua::String> {
        let Some(mut b) = bytes(&v) else {
            return Err(mlua::Error::runtime("Usage: string.reverse(s)"));
        };
        b.reverse();
        lua.create_string(&b)
    };
    api.table("string", "reverse", reverse)?;
    api.global("strrev", reverse)?;
    api.table("string", "split", strsplit)?;
    api.global("strsplit", strsplit)?;

    api.global("strjoin", |lua, args: Variadic<Value>| {
        let Some(delim) = bytes(&arg(&args, 0)) else {
            return Err(mlua::Error::runtime("Usage: strjoin(delimiter, ...)"));
        };
        let mut out = Vec::new();
        for (i, v) in args.iter().skip(1).enumerate() {
            if i > 0 {
                out.extend_from_slice(&delim);
            }
            out.extend(bytes(v).unwrap_or_default());
        }
        lua.create_string(&out)
    })?;
    api.global("strtrim", |lua, (s, chars): (Value, Value)| {
        let Some(s) = bytes(&s) else {
            return Err(mlua::Error::runtime("Usage: strtrim(str [, chars])"));
        };
        let set = bytes(&chars).unwrap_or_else(|| TRIM_DEFAULT.to_vec());
        let start = s.iter().position(|b| !set.contains(b)).unwrap_or(s.len());
        let end = s
            .iter()
            .rposition(|b| !set.contains(b))
            .map_or(start, |e| e + 1);
        lua.create_string(&s[start..end.max(start)])
    })?;
    api.global("strreplace", |lua, (s, f, r): (Value, Value, Value)| {
        let (Some(s), Some(f), Some(r)) = (bytes(&s), bytes(&f), bytes(&r)) else {
            return Err(mlua::Error::runtime(
                "Usage: strreplace(str, find, replace)",
            ));
        };
        if f.is_empty() {
            return (lua.create_string(&s)?, 0).into_lua_multi(lua);
        }
        let (mut out, mut i, mut n) = (Vec::with_capacity(s.len()), 0, 0);
        while i < s.len() {
            if s[i..].starts_with(&f) {
                out.extend_from_slice(&r);
                i += f.len();
                n += 1;
            } else {
                out.push(s[i]);
                i += 1;
            }
        }
        (lua.create_string(&out)?, n).into_lua_multi(lua)
    })?;

    // `table.count(t)` -> total pairs, array-shaped integer keys, the largest positive integer key.
    api.table("table", "count", |_, v: Value| {
        let Value::Table(t) = v else {
            return Err(mlua::Error::runtime("Usage: table.count(table)"));
        };
        let (mut total, mut ints, mut max) = (0i64, Vec::new(), 0.0f64);
        for pair in t.pairs::<Value, Value>() {
            let (k, _) = pair?;
            total += 1;
            if let Some(k) = match k {
                Value::Integer(i) => Some(i as f64),
                Value::Number(n) => Some(n),
                _ => None,
            } {
                if k == k.floor() {
                    ints.push(k);
                    if k >= 1.0 && k > max {
                        max = k;
                    }
                }
            }
        }
        let array = ints
            .iter()
            .filter(|k| **k >= 1.0 && **k <= total as f64)
            .count();
        Ok((total, array, max))
    })?;
    api.table("table", "maxn", |_, v: Value| {
        let Value::Table(t) = v else {
            return Err(mlua::Error::runtime(
                "bad argument #1 to 'maxn' (table expected)",
            ));
        };
        let mut max = 0.0f64;
        for pair in t.pairs::<Value, Value>() {
            let n = match pair?.0 {
                Value::Integer(i) => i as f64,
                Value::Number(n) => n,
                _ => continue,
            };
            if n > max {
                max = n;
            }
        }
        Ok(max)
    })?;
    let setn: Option<Function> = g.get::<Table>("table")?.get("setn").ok();
    api.table("table", "wipe", move |_, v: Value| {
        let Value::Table(t) = v else {
            return Err(mlua::Error::runtime("Usage: table.wipe(table)"));
        };
        let keys: Vec<Value> = t
            .pairs::<Value, Value>()
            .map(|p| p.map(|(k, _)| k))
            .collect::<mlua::Result<_>>()?;
        for k in keys {
            t.raw_set(k, Value::Nil)?;
        }
        if let Some(setn) = &setn {
            setn.call::<()>((t.clone(), 0))?;
        }
        Ok(t)
    })?;

    // `coroutine`: the VM's own 5.1 library, back from `_LOADED`.
    let loaded: Table = lua.named_registry_value("_LOADED")?;
    if let Ok(co) = loaded.get::<Table>("coroutine") {
        for name in ["create", "resume", "yield", "status", "wrap", "running"] {
            if let Ok(f) = co.get::<Function>(name) {
                api.function(Some("coroutine"), name, f)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;
    use crate::Ca;

    #[test]
    fn the_51_members_answer_as_51_does() {
        let script = vm(&Ca::default());
        let out: String = script
            .lua()
            .load(
                r##"
                local a, b = select(2, "x", "y", "z")
                local co = coroutine.create(function(v) local w = coroutine.yield(v + 1) return w * 2 end)
                local _, first = coroutine.resume(co, 1)
                local _, second = coroutine.resume(co, 10)
                local ok, sum = xpcall(function(p, q) return p + q end, function(e) return e end, 2, 3)
                local x, y, z = strsplit(",", "a,b,c")
                local p, q = unpack({1, 2, 3, 4}, 2, 3)
                return table.concat({
                    select("#", 1, nil, 3), a, b, first, second, tostring(ok), sum,
                    x, y, z, p, q, string.match("key=val", "(%w+)=(%w+)"),
                    string.match("hello", "ll"), strtrim("  hi  "), strjoin("-", "a", "b"),
                    string.reverse("abc"), coroutine.status(co),
                }, " ")
                "##,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "3 y z 2 20 true 5 a b c 2 3 key ll hi a-b cba dead");
    }
}
