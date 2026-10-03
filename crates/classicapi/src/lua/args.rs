//! The Lua C API's argument reads, as the DLL's natives make them: `lua_isnumber` takes a numeric
//! string, `lua_tonumber` reads 0 for anything else, a C cast truncates, and `atoi` parses a
//! leading integer.

use mlua::{Lua, Value};

/// `lua_isnumber`: a number, or a string that converts to one.
pub(crate) fn is_number(v: &Value) -> bool {
    num(v).is_some()
}

/// `lua_tonumber` when [`is_number`], else `None`.
pub(crate) fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        Value::String(s) => str_to_number(&s.to_str().ok()?),
        _ => None,
    }
}

/// `lua_tonumber`, 0 for a non-number.
pub(crate) fn to_number(v: &Value) -> f64 {
    num(v).unwrap_or(0.0)
}

/// `static_cast<int>(lua_tonumber(...))`: truncation toward zero, saturating.
pub(crate) fn to_int(v: &Value) -> i64 {
    to_number(v) as i64
}

/// `lua_isstring` and `lua_tostring`: a string, or a number in Lua's `%.14g`.
pub(crate) fn to_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.to_string_lossy()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(fmt_number(*n)),
        _ => None,
    }
}

/// `lua_type(L, i) == LUA_TSTRING`: a string proper, not a number.
pub(crate) fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.to_string_lossy()),
        _ => None,
    }
}

/// `lua_toboolean`: everything but nil and false.
pub(crate) fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Nil | Value::Boolean(false))
}

/// Lua's own number conversion (`luaO_str2d`): decimal or `0x` hex, surrounding space allowed.
pub(crate) fn str_to_number(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c.is_ascii_whitespace());
    if t.is_empty() {
        return None;
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        let v = u64::from_str_radix(hex, 16).ok()? as f64;
        return Some(if neg { -v } else { v });
    }
    // `strtod` takes digits, one point and an exponent; Rust's parser also takes `inf` and
    // `nan`, which Lua does not.
    if !body
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return None;
    }
    t.parse::<f64>().ok()
}

/// C's `atoi`: optional space and sign, then digits; 0 when none.
pub(crate) fn atoi(s: &str) -> i64 {
    let t = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut n: i64 = 0;
    for b in digits.bytes().take_while(u8::is_ascii_digit) {
        n = n.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    if neg {
        -n
    } else {
        n
    }
}

/// Lua's `%.14g` number format, so a number converts to the string Lua would make.
pub(crate) fn fmt_number(n: f64) -> String {
    if n.is_nan() {
        return if n.is_sign_negative() { "-nan" } else { "nan" }.to_string();
    }
    if n.is_infinite() {
        return if n < 0.0 { "-inf" } else { "inf" }.to_string();
    }
    if n == 0.0 {
        return if n.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    // `%g` with precision P = 14: the exponent X of the `%e` form picks the style.
    const P: i32 = 14;
    let sci = format!("{:.*e}", (P - 1) as usize, n);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let x: i32 = exp.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if !(-4..P).contains(&x) {
        let sign = if x < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", trim(mantissa), sign, x.abs())
    } else {
        trim(&format!("{:.*}", (P - 1 - x) as usize, n))
    }
}

/// A Lua string value, or nil for `None` and the empty string when `empty_is_nil`.
pub(crate) fn opt_str(lua: &Lua, s: Option<&str>) -> mlua::Result<Value> {
    match s {
        Some(s) => Ok(Value::String(lua.create_string(s)?)),
        None => Ok(Value::Nil),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi_reads_a_leading_integer() {
        assert_eq!(atoi("  42abc"), 42);
        assert_eq!(atoi("-7"), -7);
        assert_eq!(atoi("abc"), 0);
        assert_eq!(atoi(""), 0);
    }

    #[test]
    fn numbers_format_as_lua_does() {
        assert_eq!(fmt_number(3.0), "3");
        assert_eq!(fmt_number(0.1), "0.1");
        assert_eq!(fmt_number(1.5e20), "1.5e+20");
        assert_eq!(fmt_number(1e-5), "1e-05");
        assert_eq!(fmt_number(123456.789), "123456.789");
        assert_eq!(fmt_number(1.0 / 3.0), "0.33333333333333");
    }

    #[test]
    fn numeric_strings_are_numbers() {
        assert_eq!(str_to_number(" 12 "), Some(12.0));
        assert_eq!(str_to_number("0x10"), Some(16.0));
        assert_eq!(str_to_number("1e3"), Some(1000.0));
        assert_eq!(str_to_number("nan"), None);
        assert_eq!(str_to_number("12a"), None);
        assert_eq!(str_to_number(""), None);
    }
}
