//! `xml/Templates.cpp`: `C_XMLUtil.GetTemplates`, `GetTemplateInfo` and `DoesTemplateExist` over
//! the virtual XML templates the loader registered (`ext_read::xml_templates`).
//!
//! `GetTemplateInfo`'s size is the template's declared `<Size>`, inline or through its
//! `AbsDimension` / `RelDimension` child, unscaled; `keyValues` is always empty (1.12's schema has
//! no `<KeyValues>`), `inherits` the raw attribute, and `sourceLocation` absent.

use benilla_ui::framexml::Element;
use mlua::Value;

use crate::lua::{to_str, Api};

/// `ReadSize`.
fn declared_size(e: &Element) -> (f64, f64) {
    let Some(size) = e
        .children
        .iter()
        .find(|c| c.tag.eq_ignore_ascii_case("Size"))
    else {
        return (0.0, 0.0);
    };
    let mut x = size.attr("x");
    let mut y = size.attr("y");
    if let Some(dim) = size.children.first().filter(|d| {
        d.tag.eq_ignore_ascii_case("AbsDimension") || d.tag.eq_ignore_ascii_case("RelDimension")
    }) {
        x = dim.attr("x").filter(|v| !v.is_empty()).or(x);
        y = dim.attr("y").filter(|v| !v.is_empty()).or(y);
    }
    let n = |v: Option<&str>| v.and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0);
    (n(x), n(y))
}

fn name_arg(v: &Value, usage: &str) -> mlua::Result<String> {
    to_str(v).ok_or_else(|| mlua::Error::runtime(usage.to_string()))
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    const NS: &str = "C_XMLUtil";
    api.table(NS, "GetTemplates", |lua, ()| {
        let out = lua.create_table()?;
        for (i, (name, ty)) in benilla_ui::script::ext_read::xml_templates(lua)
            .into_iter()
            .enumerate()
        {
            let t = lua.create_table()?;
            t.set("name", name)?;
            t.set("type", ty)?;
            out.raw_set(i + 1, t)?;
        }
        Ok(out)
    })?;
    api.table(NS, "GetTemplateInfo", |lua, v: Value| {
        let name = name_arg(&v, "Usage: C_XMLUtil.GetTemplateInfo(\"name\")")?;
        let Some(e) = benilla_ui::script::ext_read::xml_template(lua, &name) else {
            return Ok(Value::Nil);
        };
        let t = lua.create_table()?;
        t.set("type", e.tag.as_str())?;
        let (w, h) = declared_size(&e);
        t.set("width", w)?;
        t.set("height", h)?;
        t.set("keyValues", lua.create_table()?)?;
        if let Some(inherits) = e.attr("inherits").filter(|s| !s.is_empty()) {
            t.set("inherits", inherits)?;
        }
        Ok(Value::Table(t))
    })?;
    api.table(NS, "DoesTemplateExist", |lua, v: Value| {
        let name = name_arg(&v, "Usage: C_XMLUtil.DoesTemplateExist(\"name\")")?;
        Ok(benilla_ui::script::ext_read::xml_template(lua, &name).is_some())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_loaded_template_reads_its_type_size_and_inherits() {
        let ca = crate::Ca::default();
        let script = crate::lua::test_support::vm(&ca);
        let doc = benilla_ui::framexml::parse(
            r#"<Ui>
                  <Button name="ProbeButtonTemplate" virtual="true" inherits="UIPanelButtonTemplate">
                    <Size><AbsDimension x="120" y="22"/></Size>
                  </Button>
                  <Frame name="ProbeFrameTemplate" virtual="true"><Size x="40" y="30"/></Frame>
                </Ui>"#,
        )
        .expect("xml");
        let report = benilla_ui::loader::load(&script, &doc, &|_| None);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let out: String = script
            .lua()
            .load(
                r#"
                local X = C_XMLUtil
                local b = X.GetTemplateInfo("probebuttontemplate")
                local f = X.GetTemplateInfo("ProbeFrameTemplate")
                local n = 0
                for _, t in ipairs(X.GetTemplates()) do
                  if t.name == "ProbeButtonTemplate" and t.type == "Button" then n = n + 1 end
                end
                return b.type .. b.width .. "x" .. b.height .. b.inherits
                  .. " " .. f.width .. "x" .. f.height .. tostring(f.inherits)
                  .. " " .. n .. tostring(X.DoesTemplateExist("NoSuchTemplate"))
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "Button120x22UIPanelButtonTemplate 40x30nil 1false");
    }
}
