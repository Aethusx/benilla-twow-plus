//! `encoding/`: `C_EncodingUtil` and `Enum.Base64Variant` / `Enum.CompressionMethod`.
//!
//! - `Base64.cpp`: Standard (`+/`, padded) and UrlSafe (`-_`, unpadded); a named variant decodes
//!   strictly, no variant accepts either alphabet and optional padding.
//! - `Hex.cpp`: lower-case pairs; decoding takes either case and rejects odd or non-hex input.
//! - `JSON.cpp`: a table of keys 1..N (none included) is an array, any other an object with
//!   number keys as `%.14g` text; NaN and the infinities are `null`; nesting stops at 64.
//! - `CBOR.cpp`: whole numbers within ±2^53 as integers, others as doubles, strings as text
//!   strings byte for byte, map entries in canonical order (shorter encoded key first, then
//!   bytewise); decoding reads every major type, tags skipped, null and undefined as nil.
//! - `Compress.cpp`: Deflate (raw), Zlib (the default) and Gzip at level 0-9 or -1; decoding with
//!   no method detects Zlib or Gzip.
//!
//! Every failure answers nothing, as the DLL's do.

use std::io::{Read, Write};

use mlua::{Lua, Table, Value};

use crate::lua::{is_number, to_int, to_number, Api};

const MAX_DEPTH: usize = 64;
const MAX_SAFE: f64 = 9_007_199_254_740_992.0;

// ---- Base64 ----

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn encode_base64(data: &[u8], url_safe: bool) -> String {
    let alphabet = if url_safe { URL_SAFE } else { STANDARD };
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(alphabet[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else if !url_safe {
                out.push('=');
            }
        }
    }
    out
}

/// One character's six bits: the variant's alphabet when `strict`, either when not.
fn base64_char(c: u8, url_safe: bool, strict: bool) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => u32::from(c - b'A'),
        b'a'..=b'z' => u32::from(c - b'a') + 26,
        b'0'..=b'9' => u32::from(c - b'0') + 52,
        b'+' if !(strict && url_safe) => 62,
        b'-' if !strict || url_safe => 62,
        b'/' if !(strict && url_safe) => 63,
        b'_' if !strict || url_safe => 63,
        _ => return None,
    })
}

fn decode_base64(src: &[u8], url_safe: bool, strict: bool) -> Option<Vec<u8>> {
    let effective = src.len() - src.iter().rev().take_while(|c| **c == b'=').count();
    let pad = src.len() - effective;
    if pad > 2 || (strict && !url_safe && !src.len().is_multiple_of(4)) {
        return None;
    }
    let body = &src[..effective];
    if body.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    for chunk in body.chunks(4) {
        let v: Vec<u32> = chunk
            .iter()
            .map(|c| base64_char(*c, url_safe, strict))
            .collect::<Option<_>>()?;
        out.push((v[0] << 2 | v[1] >> 4) as u8);
        if v.len() > 2 {
            out.push((v[1] << 4 | v[2] >> 2) as u8);
        }
        if v.len() > 3 {
            out.push((v[2] << 6 | v[3]) as u8);
        }
    }
    Some(out)
}

// ---- Hex ----

fn encode_hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(src: &[u8]) -> Option<Vec<u8>> {
    if !src.len().is_multiple_of(2) {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16);
    src.chunks(2)
        .map(|p| Some((digit(p[0])? << 4 | digit(p[1])?) as u8))
        .collect()
}

// ---- Lua tables ----

/// The length of a table whose keys are exactly 1..N, `None` for any other.
fn array_len(t: &Table) -> mlua::Result<Option<usize>> {
    let (mut seen, mut max) = (0usize, 0usize);
    for pair in t.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        let n = match k {
            Value::Integer(i) => i as f64,
            Value::Number(n) => n,
            _ => return Ok(None),
        };
        if n < 1.0 || n.fract() != 0.0 || n > usize::MAX as f64 {
            return Ok(None);
        }
        seen += 1;
        max = max.max(n as usize);
    }
    Ok((seen == max).then_some(seen))
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

/// `%.14g`, as the DLL names a number key in a JSON object.
fn g14(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e14 {
        return format!("{}", n as i64);
    }
    let s = format!("{n:.13e}");
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-5..14).contains(&exp) {
        let decimals = (13 - exp).max(0) as usize;
        let fixed = format!("{n:.decimals$}");
        let fixed = fixed.trim_end_matches('0').trim_end_matches('.');
        fixed.to_string()
    } else {
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        format!(
            "{mantissa}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

// ---- JSON ----

fn to_json(v: &Value, depth: usize) -> mlua::Result<Option<serde_json::Value>> {
    use serde_json::Value as J;
    Ok(Some(match v {
        Value::Nil => J::Null,
        Value::Boolean(b) => J::Bool(*b),
        Value::Integer(_) | Value::Number(_) => {
            let n = num(v).unwrap_or(0.0);
            if !n.is_finite() {
                J::Null
            } else if n.fract() == 0.0 && n.abs() < MAX_SAFE {
                J::from(n as i64)
            } else {
                serde_json::Number::from_f64(n).map_or(J::Null, J::Number)
            }
        }
        Value::String(s) => J::String(s.to_string_lossy()),
        Value::Table(t) => {
            if depth >= MAX_DEPTH {
                return Ok(None);
            }
            if let Some(len) = array_len(t)? {
                let mut arr = Vec::with_capacity(len);
                for i in 1..=len {
                    match to_json(&t.raw_get::<Value>(i)?, depth + 1)? {
                        Some(j) => arr.push(j),
                        None => return Ok(None),
                    }
                }
                J::Array(arr)
            } else {
                let mut entries: Vec<(String, serde_json::Value)> = Vec::new();
                for pair in t.clone().pairs::<Value, Value>() {
                    let (k, val) = pair?;
                    let key = match &k {
                        Value::String(s) => s.to_string_lossy(),
                        Value::Integer(_) | Value::Number(_) => g14(num(&k).unwrap_or(0.0)),
                        _ => continue,
                    };
                    match to_json(&val, depth + 1)? {
                        Some(j) => entries.push((key, j)),
                        None => return Ok(None),
                    }
                }
                // Sorted, as picojson's std::map orders them.
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                J::Object(entries.into_iter().collect())
            }
        }
        _ => return Ok(None),
    }))
}

fn from_json(lua: &Lua, j: &serde_json::Value) -> mlua::Result<Value> {
    use serde_json::Value as J;
    Ok(match j {
        J::Null => Value::Nil,
        J::Bool(b) => Value::Boolean(*b),
        J::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        J::String(s) => Value::String(lua.create_string(s)?),
        J::Array(a) => {
            let t = lua.create_table()?;
            for (i, v) in a.iter().enumerate() {
                t.raw_set(i + 1, from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
        J::Object(o) => {
            let t = lua.create_table()?;
            for (k, v) in o {
                t.raw_set(k.as_str(), from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}

// ---- CBOR ----

fn cbor_head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..=23 => out.push(m | n as u8),
        24..=0xff => out.extend([m | 24, n as u8]),
        0x100..=0xffff => {
            out.push(m | 25);
            out.extend((n as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(m | 26);
            out.extend((n as u32).to_be_bytes());
        }
        _ => {
            out.push(m | 27);
            out.extend(n.to_be_bytes());
        }
    }
}

fn to_cbor(v: &Value, out: &mut Vec<u8>, depth: usize) -> mlua::Result<bool> {
    match v {
        Value::Nil => out.push(0xf6),
        Value::Boolean(b) => out.push(if *b { 0xf5 } else { 0xf4 }),
        Value::Integer(_) | Value::Number(_) => {
            let n = num(v).unwrap_or(0.0);
            if n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_SAFE {
                if n >= 0.0 {
                    cbor_head(out, 0, n as u64);
                } else {
                    cbor_head(out, 1, (-n) as u64 - 1);
                }
            } else {
                out.push(0xfb);
                out.extend(n.to_be_bytes());
            }
        }
        Value::String(s) => {
            let b = s.as_bytes();
            cbor_head(out, 3, b.len() as u64);
            out.extend_from_slice(&b);
        }
        Value::Table(t) => {
            if depth >= MAX_DEPTH {
                return Ok(false);
            }
            if let Some(len) = array_len(t)? {
                cbor_head(out, 4, len as u64);
                for i in 1..=len {
                    if !to_cbor(&t.raw_get::<Value>(i)?, out, depth + 1)? {
                        return Ok(false);
                    }
                }
            } else {
                let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
                for pair in t.clone().pairs::<Value, Value>() {
                    let (k, val) = pair?;
                    let (mut kb, mut vb) = (Vec::new(), Vec::new());
                    if !to_cbor(&k, &mut kb, depth + 1)? || !to_cbor(&val, &mut vb, depth + 1)? {
                        return Ok(false);
                    }
                    entries.push((kb, vb));
                }
                entries.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
                cbor_head(out, 5, entries.len() as u64);
                for (k, val) in entries {
                    out.extend(k);
                    out.extend(val);
                }
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

struct Cbor<'a> {
    src: &'a [u8],
    at: usize,
}

impl Cbor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.src.get(self.at..self.at + n)?;
        self.at += n;
        Some(s)
    }

    /// A head: major type and argument, `None` argument for an indefinite length.
    fn head(&mut self) -> Option<(u8, u8, Option<u64>)> {
        let b = *self.take(1)?.first()?;
        let (major, info) = (b >> 5, b & 31);
        let arg = match info {
            0..=23 => Some(u64::from(info)),
            24 => Some(u64::from(self.take(1)?[0])),
            25 => Some(u64::from(u16::from_be_bytes(
                self.take(2)?.try_into().ok()?,
            ))),
            26 => Some(u64::from(u32::from_be_bytes(
                self.take(4)?.try_into().ok()?,
            ))),
            27 => Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?)),
            31 => None,
            _ => return None,
        };
        Some((major, info, arg))
    }

    fn at_break(&self) -> bool {
        self.src.get(self.at) == Some(&0xff)
    }

    fn string(&mut self, major: u8, arg: Option<u64>) -> Option<Vec<u8>> {
        match arg {
            Some(n) => Some(self.take(usize::try_from(n).ok()?)?.to_vec()),
            None => {
                let mut out = Vec::new();
                while !self.at_break() {
                    let (m, _, n) = self.head()?;
                    if m != major {
                        return None;
                    }
                    out.extend(self.take(usize::try_from(n?).ok()?)?);
                }
                self.at += 1;
                Some(out)
            }
        }
    }

    fn value(&mut self, lua: &Lua, depth: usize) -> mlua::Result<Option<Value>> {
        let Some((major, info, arg)) = self.head() else {
            return Ok(None);
        };
        Ok(Some(match major {
            0 => Value::Number(arg.unwrap_or(0) as f64),
            1 => Value::Number(-1.0 - arg.unwrap_or(0) as f64),
            2 | 3 => match self.string(major, arg) {
                Some(b) => Value::String(lua.create_string(&b)?),
                None => return Ok(None),
            },
            4 | 5 => {
                if depth >= MAX_DEPTH {
                    return Ok(None);
                }
                let t = lua.create_table()?;
                let mut i = 0u64;
                loop {
                    match arg {
                        Some(n) if i >= n => break,
                        None if self.at_break() => {
                            self.at += 1;
                            break;
                        }
                        _ => {}
                    }
                    let Some(first) = self.value(lua, depth + 1)? else {
                        return Ok(None);
                    };
                    if major == 5 {
                        let Some(second) = self.value(lua, depth + 1)? else {
                            return Ok(None);
                        };
                        if !first.is_nil() {
                            t.raw_set(first, second)?;
                        }
                    } else {
                        t.raw_set(i + 1, first)?;
                    }
                    i += 1;
                }
                Value::Table(t)
            }
            6 => return self.value(lua, depth),
            7 => match (info, arg) {
                (20, _) => Value::Boolean(false),
                (21, _) => Value::Boolean(true),
                (22 | 23, _) => Value::Nil,
                (25, Some(h)) => Value::Number(half(h as u16)),
                (26, Some(f)) => Value::Number(f64::from(f32::from_bits(f as u32))),
                (27, Some(d)) => Value::Number(f64::from_bits(d)),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        }))
    }
}

/// An IEEE half-precision float.
fn half(h: u16) -> f64 {
    let (sign, exp, frac) = (h >> 15, (h >> 10) & 0x1f, f64::from(h & 0x3ff));
    let v = match exp {
        0 => frac * 2f64.powi(-24),
        31 if frac == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        e => (1.0 + frac / 1024.0) * 2f64.powi(i32::from(e) - 15),
    };
    if sign == 1 {
        -v
    } else {
        v
    }
}

// ---- Compression ----

/// `Enum.CompressionMethod`.
const DEFLATE: i64 = 0;
const ZLIB: i64 = 1;
const GZIP: i64 = 2;

fn compress(data: &[u8], method: i64, level: i64) -> Option<Vec<u8>> {
    let level = flate2::Compression::new(if (0..=9).contains(&level) {
        level as u32
    } else {
        6
    });
    match method {
        DEFLATE => {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), level);
            e.write_all(data).ok()?;
            e.finish().ok()
        }
        ZLIB => {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), level);
            e.write_all(data).ok()?;
            e.finish().ok()
        }
        GZIP => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), level);
            e.write_all(data).ok()?;
            e.finish().ok()
        }
        _ => None,
    }
}

/// `method` `None` detects Zlib or Gzip by the header.
fn decompress(data: &[u8], method: Option<i64>) -> Option<Vec<u8>> {
    let method = method.unwrap_or(if data.starts_with(&[0x1f, 0x8b]) {
        GZIP
    } else {
        ZLIB
    });
    let mut out = Vec::new();
    let ok = match method {
        DEFLATE => flate2::read::DeflateDecoder::new(data).read_to_end(&mut out),
        ZLIB => flate2::read::ZlibDecoder::new(data).read_to_end(&mut out),
        GZIP => flate2::read::GzDecoder::new(data).read_to_end(&mut out),
        _ => return None,
    };
    ok.ok().map(|_| out)
}

// ---- Lua ----

fn bytes_arg(v: &Value, usage: &str) -> mlua::Result<Vec<u8>> {
    match v {
        Value::String(s) => Ok(s.as_bytes().to_vec()),
        Value::Integer(_) | Value::Number(_) => {
            Ok(crate::lua::to_str(v).unwrap_or_default().into_bytes())
        }
        _ => Err(mlua::Error::runtime(usage.to_string())),
    }
}

/// An optional enum argument in `0..=max`; `None` when absent.
fn enum_arg(v: &Value, max: i64, what: &str) -> mlua::Result<Option<i64>> {
    if !is_number(v) {
        return Ok(None);
    }
    let n = to_int(v);
    if !(0..=max).contains(&n) {
        return Err(mlua::Error::runtime(what.to_string()));
    }
    Ok(Some(n))
}

fn bytes(lua: &Lua, b: Option<Vec<u8>>) -> mlua::Result<Option<mlua::String>> {
    b.map(|b| lua.create_string(&b)).transpose()
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let enums: Table = match api.lua.globals().get::<Value>("Enum")? {
        Value::Table(t) => t,
        _ => {
            let t = api.lua.create_table()?;
            api.lua.globals().set("Enum", t.clone())?;
            t
        }
    };
    let variant = api.lua.create_table()?;
    variant.set("Standard", 0)?;
    variant.set("UrlSafe", 1)?;
    enums.set("Base64Variant", variant)?;
    let method = api.lua.create_table()?;
    method.set("Deflate", DEFLATE)?;
    method.set("Zlib", ZLIB)?;
    method.set("Gzip", GZIP)?;
    enums.set("CompressionMethod", method)?;

    const NS: &str = "C_EncodingUtil";
    api.table(NS, "EncodeBase64", |_, (data, v): (Value, Value)| {
        let data = bytes_arg(
            &data,
            "Usage: C_EncodingUtil.EncodeBase64(data [, variant])",
        )?;
        let v = enum_arg(&v, 1, "C_EncodingUtil.EncodeBase64: invalid Base64Variant")?;
        Ok(encode_base64(&data, v == Some(1)))
    })?;
    api.table(NS, "DecodeBase64", |lua, (data, v): (Value, Value)| {
        let data = bytes_arg(
            &data,
            "Usage: C_EncodingUtil.DecodeBase64(data [, variant])",
        )?;
        let v = enum_arg(&v, 1, "C_EncodingUtil.DecodeBase64: invalid Base64Variant")?;
        bytes(lua, decode_base64(&data, v == Some(1), v.is_some()))
    })?;
    api.table(NS, "EncodeHex", |_, data: Value| {
        Ok(encode_hex(&bytes_arg(
            &data,
            "Usage: C_EncodingUtil.EncodeHex(data)",
        )?))
    })?;
    api.table(NS, "DecodeHex", |lua, data: Value| {
        let data = bytes_arg(&data, "Usage: C_EncodingUtil.DecodeHex(hex)")?;
        bytes(lua, decode_hex(&data))
    })?;
    api.table(NS, "SerializeJSON", |lua, args: mlua::MultiValue| {
        let Some(v) = args.into_iter().next() else {
            return Err(mlua::Error::runtime(
                "Usage: C_EncodingUtil.SerializeJSON(value)",
            ));
        };
        match to_json(&v, 0)? {
            Some(j) => bytes(lua, serde_json::to_vec(&j).ok()),
            None => Ok(None),
        }
    })?;
    api.table(NS, "DeserializeJSON", |lua, data: Value| {
        let data = bytes_arg(&data, "Usage: C_EncodingUtil.DeserializeJSON(json)")?;
        match serde_json::from_slice::<serde_json::Value>(&data) {
            Ok(j) => from_json(lua, &j),
            Err(_) => Ok(Value::Nil),
        }
    })?;
    api.table(NS, "SerializeCBOR", |lua, args: mlua::MultiValue| {
        let Some(v) = args.into_iter().next() else {
            return Err(mlua::Error::runtime(
                "Usage: C_EncodingUtil.SerializeCBOR(value)",
            ));
        };
        let mut out = Vec::new();
        let ok = to_cbor(&v, &mut out, 0)?;
        bytes(lua, ok.then_some(out))
    })?;
    api.table(NS, "DeserializeCBOR", |lua, data: Value| {
        let data = bytes_arg(&data, "Usage: C_EncodingUtil.DeserializeCBOR(data)")?;
        Ok(Cbor { src: &data, at: 0 }
            .value(lua, 0)?
            .unwrap_or(Value::Nil))
    })?;
    api.table(
        NS,
        "CompressString",
        |lua, (data, m, level): (Value, Value, Value)| {
            let data = bytes_arg(
                &data,
                "Usage: C_EncodingUtil.CompressString(data [, method, level])",
            )?;
            let m = enum_arg(
                &m,
                2,
                "C_EncodingUtil.CompressString: invalid CompressionMethod",
            )?;
            let level = if is_number(&level) {
                to_number(&level) as i64
            } else {
                -1
            };
            bytes(lua, compress(&data, m.unwrap_or(ZLIB), level))
        },
    )?;
    api.table(NS, "DecompressString", |lua, (data, m): (Value, Value)| {
        let data = bytes_arg(
            &data,
            "Usage: C_EncodingUtil.DecompressString(data [, method])",
        )?;
        let m = enum_arg(
            &m,
            2,
            "C_EncodingUtil.DecompressString: invalid CompressionMethod",
        )?;
        bytes(lua, decompress(&data, m))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::test_support::vm;

    #[test]
    fn base64_and_hex_follow_their_variants() {
        assert_eq!(encode_base64(b"hello?>", false), "aGVsbG8/Pg==");
        assert_eq!(encode_base64(b"hello?>", true), "aGVsbG8_Pg");
        assert_eq!(
            decode_base64(b"aGVsbG8_Pg", true, true).unwrap(),
            b"hello?>"
        );
        // Strict Standard wants its own alphabet and the padding.
        assert!(decode_base64(b"aGVsbG8_Pg==", false, true).is_none());
        assert!(decode_base64(b"aGVsbG8/Pg", false, true).is_none());
        // No variant: either alphabet, padding optional.
        assert_eq!(
            decode_base64(b"aGVsbG8_Pg", false, false).unwrap(),
            b"hello?>"
        );
        assert!(decode_base64(b"a", false, false).is_none());
        assert_eq!(encode_hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(decode_hex(b"00AB10").unwrap(), [0, 0xab, 0x10]);
        assert!(decode_hex(b"0").is_none() && decode_hex(b"zz").is_none());
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(g14(1.5), "1.5");
        assert_eq!(g14(3.0), "3");
    }

    #[test]
    fn json_cbor_and_compression_round_trip() {
        let script = vm(&crate::Ca::default());
        let out: String = script
            .lua()
            .load(
                r#"
                local E = C_EncodingUtil
                local json = E.SerializeJSON({ b = { 1, 2, 3 }, a = 1.5, [3] = "x", e = {} })
                local back = E.DeserializeJSON('{"n":42,"s":"hi","l":[true,null,false]}')
                local cbor = E.SerializeCBOR({ 10, -1, 1.5, "\255", { k = "v" } })
                local c = E.DeserializeCBOR(cbor)
                local z = E.CompressString(string.rep("abc", 100))
                local g = E.CompressString("data", Enum.CompressionMethod.Gzip, 9)
                local r = E.CompressString("raw", Enum.CompressionMethod.Deflate)
                return json .. " | " .. back.n .. back.s .. tostring(back.l[1]) .. tostring(back.l[3])
                  .. " | " .. E.EncodeHex(string.sub(cbor, 1, 4)) .. " " .. c[1] .. c[2] .. c[3]
                  .. E.EncodeHex(c[4]) .. c[5].k
                  .. " | " .. string.len(E.DecompressString(z)) .. E.DecompressString(g)
                  .. E.DecompressString(r, Enum.CompressionMethod.Deflate)
                  .. " " .. tostring(E.DecompressString("junk")) .. tostring(E.DeserializeJSON("{"))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(
            out,
            r#"{"3":"x","a":1.5,"b":[1,2,3],"e":[]} | 42hitruefalse | 850a20fb 10-11.5ffv | 300dataraw nilnil"#
        );
    }
}
