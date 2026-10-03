//! `frame/`: the frame-level backports.
//!
//! - `ScriptArgs.cpp`: modern positional script arguments, every handler also called as
//!   `(self, [event,] arg1..argN)` beside 1.12's `this`/`event`/`argN` globals, through benilla's
//!   handler seam; on by default, `SetModernScriptArgs(false)` turns it off. Deviation: the DLL
//!   passes them only to a handler whose prototype declares a parameter; a parameterless Lua
//!   function ignores them, so the difference is unobservable.
//! - `Modern.cpp`: the later clients' region and frame methods (`SetSize`, `GetSize`,
//!   `IsMouseOver`, `GetRect`, `IsDragging`, `SetShown`, `SetResizeBounds`, `HookScript`,
//!   `GetEffectiveAlpha`), composed from each object's own 1.12 methods as the DLL composes them
//!   from the engine's. Deviation: the DLL's `SetPoint(point)` two-argument fix is not ported; it
//!   changes a stock 1.12 method.

use mlua::{Function, Table, Value};

use crate::lua::{truthy, Api};

/// The methods, in the client's own 5.0 dialect: a vararg reads `arg`, never `...`.
const METHODS: &str = r#"
local M = {}
local error, tonumber, type = error, tonumber, type

function M.SetSize(self, w, h)
  if not tonumber(w) or not tonumber(h) then
    error("Usage: region:SetSize(width, height)", 2)
  end
  self:SetWidth(w)
  self:SetHeight(h)
end

function M.GetSize(self)
  return self:GetWidth(), self:GetHeight()
end

-- The cursor in the region's own coordinates against its rect, each edge pushed out by its
-- offset; false for a region with no resolved rect.
function M.IsMouseOver(self, top, bottom, left, right)
  local scale
  if self.GetEffectiveScale then
    scale = self:GetEffectiveScale()
  else
    local p = self:GetParent()
    scale = p and p:GetEffectiveScale() or 1
  end
  local l, r, t, b = self:GetLeft(), self:GetRight(), self:GetTop(), self:GetBottom()
  if not scale or scale == 0 or not l or not r or not t or not b then
    return false
  end
  local x, y = GetCursorPosition()
  x, y = x / scale, y / scale
  l = l + (tonumber(left) or 0)
  r = r + (tonumber(right) or 0)
  t = t + (tonumber(top) or 0)
  b = b + (tonumber(bottom) or 0)
  return x >= l and x <= r and y >= b and y <= t
end

function M.GetRect(self)
  local l = self:GetLeft()
  if not l then
    return
  end
  return l, self:GetBottom(), self:GetWidth(), self:GetHeight()
end

function M.SetShown(self, shown)
  if shown then
    self:Show()
  else
    self:Hide()
  end
end

function M.SetResizeBounds(self, minW, minH, maxW, maxH)
  if not tonumber(minW) or not tonumber(minH) then
    error("Usage: frame:SetResizeBounds(minWidth, minHeight [, maxWidth, maxHeight])", 2)
  end
  self:SetMinResize(minW, minH)
  if tonumber(maxW) and tonumber(maxH) then
    self:SetMaxResize(maxW, maxH)
  end
end

-- The old handler first, vanilla-style with no arguments; then the hook with positional ones.
function M.HookScript(self, name, handler)
  if type(name) ~= "string" or type(handler) ~= "function" then
    error("Usage: frame:HookScript(\"type\", function)", 2)
  end
  local old = self:GetScript(name)
  local onEvent = name == "OnEvent"
  self:SetScript(name, function()
    if type(old) == "function" then
      old()
    end
    if onEvent then
      handler(this, event, arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8, arg9)
    else
      handler(this, arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8, arg9)
    end
  end)
end

function M.GetEffectiveAlpha(self)
  local a, obj, guard = 1, self, 0
  while obj and guard < 128 do
    a = a * (obj:GetAlpha() or 1)
    obj = obj:GetParent()
    guard = guard + 1
  end
  return a
end

return M
"#;

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    benilla_ui::script::ext_read::set_modern_script_args(api.lua, true);
    api.global("SetModernScriptArgs", |lua, v: Value| {
        benilla_ui::script::ext_read::set_modern_script_args(lua, truthy(&v));
        Ok(())
    })?;
    api.global("GetModernScriptArgs", |lua, ()| {
        Ok(benilla_ui::script::ext_read::modern_script_args(lua))
    })?;

    let m: Table = api
        .lua
        .load(METHODS)
        .set_name("=ClassicAPI frame methods")
        .eval()?;
    use benilla_ui::script::ext_read::{add_frame_method, add_region_method};
    for name in ["SetSize", "GetSize", "IsMouseOver", "GetRect", "SetShown"] {
        add_region_method(api.lua, name, m.get::<Function>(name)?)?;
    }
    for name in ["SetResizeBounds", "HookScript", "GetEffectiveAlpha"] {
        add_frame_method(api.lua, name, m.get::<Function>(name)?)?;
    }
    // `IsDragging`: this region is what a started drag gesture is dragging.
    let dragging = api.lua.create_function(|lua, this: Value| {
        let me = benilla_ui::script::ext_read::frame_id(&this);
        Ok(me.is_some() && me == benilla_ui::script::ext_read::drag_source(lua))
    })?;
    add_region_method(api.lua, "IsDragging", dragging)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::lua::test_support::vm;
    use crate::Ca;

    #[test]
    fn the_methods_reach_frames_and_textures() {
        let script = vm(&crate::Ca::default());
        let _ = Ca::default();
        let out: String = script
            .lua()
            .load(
                r#"
                local f = CreateFrame("Frame", nil, UIParent)
                local t = f:CreateTexture()
                f:SetShown(false)
                local hidden = not f:IsShown()
                f:SetSize(10, 20)
                local w, h = f:GetSize()
                return tostring(hidden) .. " " .. w .. " " .. h .. " "
                  .. type(t.IsMouseOver) .. " " .. type(f.HookScript) .. " " .. tostring(f:IsMouseOver())
                "#,
            )
            .eval()
            .expect("chunk");
        assert_eq!(out, "true 10 20 function function false");
    }
}
