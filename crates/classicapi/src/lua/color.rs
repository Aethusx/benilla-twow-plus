//! `color/Util.cpp`: `C_ColorUtil`, the RGB / HSV / HSL conversions and the `|c` markup helpers.
//! Hue is degrees in 0..360, -1 for an achromatic color; the rest are 0..1.

use mlua::{Table, Value};

use crate::lua::{is_number, to_str, Api};

fn max3(a: f64, b: f64, c: f64) -> f64 {
    a.max(b).max(c)
}

fn min3(a: f64, b: f64, c: f64) -> f64 {
    a.min(b).min(c)
}

fn min_luma(l: f64) -> f64 {
    l.min(1.0 - l)
}

pub(crate) fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (mx, mn) = (max3(r, g, b), min3(r, g, b));
    let d = mx - mn;
    let s = if mx <= 0.0 { 0.0 } else { d / mx };
    if d <= 0.0 {
        return (-1.0, s, mx);
    }
    let mut h = if mx == r {
        60.0 * ((g - b) / d % 6.0)
    } else if mx == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    (h, s, mx)
}

/// The hue in 0..360 and its sextant; `None` for an achromatic color.
fn sextant(h: f64, s: f64) -> Option<(f64, i64)> {
    if s <= 0.0 || h < 0.0 {
        return None;
    }
    let mut h = h % 360.0;
    if h < 0.0 {
        h += 360.0;
    }
    let hh = h / 60.0;
    Some((hh, hh.floor() as i64 % 6))
}

pub(crate) fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    let Some((hh, i)) = sextant(h, s) else {
        return (v, v, v);
    };
    let f = hh - hh.floor();
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

fn hsv_to_hsl(h: f64, sv: f64, v: f64) -> (f64, f64, f64) {
    let l = v * (1.0 - sv / 2.0);
    let sl = if l <= 0.0 || l >= 1.0 {
        0.0
    } else {
        (v - l) / min_luma(l)
    };
    (h, sl, l)
}

fn hsl_to_hsv(h: f64, sl: f64, l: f64) -> (f64, f64, f64) {
    let v = l + sl * min_luma(l);
    let sv = if v <= 0.0 { 0.0 } else { 2.0 * (1.0 - l / v) };
    (h, sv, v)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let Some((hh, i)) = sextant(h, s) else {
        return (l, l, l);
    };
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (hh % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match i {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

/// A 0..1 channel as a byte, clamped, rounded to nearest.
fn byte(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// `aarrggbb` from a color table's raw `a`, `r`, `g`, `b` (alpha 1, the rest 0, when absent).
fn hex(t: &Table) -> mlua::Result<String> {
    let field = |k: &str, d: f64| -> mlua::Result<f64> {
        let v: Value = t.raw_get(k)?;
        Ok(if is_number(&v) {
            crate::lua::to_number(&v)
        } else {
            d
        })
    };
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}",
        byte(field("a", 1.0)?),
        byte(field("r", 0.0)?),
        byte(field("g", 0.0)?),
        byte(field("b", 0.0)?)
    ))
}

type Convert = fn(f64, f64, f64) -> (f64, f64, f64);

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    let conversions: [(&str, &str, Convert); 5] = [
        ("ConvertRGBToHSV", "r, g, b", rgb_to_hsv),
        ("ConvertHSVToRGB", "h, s, v", hsv_to_rgb),
        ("ConvertHSVToHSL", "h, s, v", hsv_to_hsl),
        ("ConvertHSLToHSV", "h, s, l", hsl_to_hsv),
        ("ConvertHSLToRGB", "h, s, l", hsl_to_rgb),
    ];
    for (name, params, f) in conversions {
        let usage = format!("Usage: C_ColorUtil.{name}({params})");
        api.table(
            "C_ColorUtil",
            name,
            move |_, (a, b, c): (Value, Value, Value)| {
                if ![&a, &b, &c].iter().all(|v| is_number(v)) {
                    return Err(mlua::Error::runtime(usage.clone()));
                }
                let n = crate::lua::to_number;
                Ok(f(n(&a), n(&b), n(&c)))
            },
        )?;
    }
    api.table(
        "C_ColorUtil",
        "GenerateTextColorCode",
        |_, v: Value| match v {
            Value::Table(t) => hex(&t),
            _ => Err(mlua::Error::runtime(
                "Usage: C_ColorUtil.GenerateTextColorCode(color)",
            )),
        },
    )?;
    api.table(
        "C_ColorUtil",
        "WrapTextInColor",
        |_, (text, color): (Value, Value)| match (to_str(&text), color) {
            (Some(text), Value::Table(t)) => Ok(format!("|c{}{text}|r", hex(&t)?)),
            _ => Err(mlua::Error::runtime(
                "Usage: C_ColorUtil.WrapTextInColor(text, color)",
            )),
        },
    )?;
    api.table(
        "C_ColorUtil",
        "WrapTextInColorCode",
        |_, (text, code): (Value, Value)| match (to_str(&text), to_str(&code)) {
            (Some(text), Some(code)) => Ok(format!("|c{code}{text}|r")),
            _ => Err(mlua::Error::runtime(
                "Usage: C_ColorUtil.WrapTextInColorCode(text, colorCode)",
            )),
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64, f64), b: (f64, f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9 && (a.2 - b.2).abs() < 1e-9
    }

    #[test]
    fn conversions_round_trip_and_grey_has_no_hue() {
        assert!(close(rgb_to_hsv(1.0, 0.0, 0.0), (0.0, 1.0, 1.0)));
        assert!(close(rgb_to_hsv(0.0, 0.0, 1.0), (240.0, 1.0, 1.0)));
        assert!(close(rgb_to_hsv(1.0, 0.0, 0.5), (330.0, 1.0, 1.0)));
        assert_eq!(rgb_to_hsv(0.5, 0.5, 0.5).0, -1.0);
        for rgb in [(0.2, 0.4, 0.6), (0.9, 0.1, 0.3), (0.5, 0.5, 0.5)] {
            let (h, s, v) = rgb_to_hsv(rgb.0, rgb.1, rgb.2);
            assert!(close(hsv_to_rgb(h, s, v), rgb));
            let (h2, sl, l) = hsv_to_hsl(h, s, v);
            assert!(close(hsl_to_rgb(h2, sl, l), rgb));
            assert!(close(hsl_to_hsv(h2, sl, l), (h, s, v)));
        }
        let lua = mlua::Lua::new();
        let t = lua.create_table().unwrap();
        t.set("r", 1.0).unwrap();
        t.set("g", 0.5).unwrap();
        assert_eq!(hex(&t).unwrap(), "ffff8000");
    }
}
