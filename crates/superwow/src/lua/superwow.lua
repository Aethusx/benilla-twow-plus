-- SuperWoW's bootstrap: runs on every in-game VM after the natives are installed and before the
-- stock interface and the addons load. It announces the mod, declares its CVars as an addon
-- declares its own, and wraps the stock verbs SuperWoW extends. Written to the 1.12 grammar: no
-- `...` expressions.

SUPERWOW_VERSION = "2.2"
SUPERWOW_STRING = "SuperWoW 2.2 (benilla)"

-- The CVars; a saved value outranks the default. Upstream documents the range of each, not
-- every default: the ones it leaves out take the stock behaviour's value.
local CVARS = {
	{ "FoV", "1.57" },
	{ "NameplateRange", "20" },
	{ "NameplateMotion", "1" },
	{ "BackgroundSound", "0" },
	{ "UncapSounds", "0" },
	{ "SelectionCircleStyle", "1" },
	{ "LootSparkle", "0" },
	{ "HealingText", "1" },
	{ "ChatBubbleRange", "40" },
	{ "ChatBubblesRaid", "1" },
	{ "ChatBubblesBattleground", "1" },
	{ "ChatBubblesWhisper", "0" },
	{ "ChatBubblesCreatures", "1" },
}
for i = 1, table.getn(CVARS) do
	RegisterCVar(CVARS[i][1], CVARS[i][2])
end

-- UnitExists(unit): 1 and the unit's guid.
local UnitExists_ = UnitExists
function UnitExists(unit)
	local exists = UnitExists_(unit)
	if exists then
		return exists, SW_Guid(unit)
	end
	return exists
end

-- UnitBuff(unit, index): texture, count, spell id.
local UnitBuff_ = UnitBuff
function UnitBuff(unit, index, castable)
	local texture, count = UnitBuff_(unit, index, castable)
	if texture then
		return texture, count, SW_AuraId(unit, index, 1)
	end
end

-- UnitDebuff(unit, index): texture, count, dispel type, spell id.
local UnitDebuff_ = UnitDebuff
function UnitDebuff(unit, index, castable)
	local texture, count, kind = UnitDebuff_(unit, index, castable)
	if texture then
		return texture, count, kind, SW_AuraId(unit, index, nil)
	end
end

-- SetRaidTarget(unit, index, "local"): the mark on our own client alone, which works solo.
local SetRaidTarget_ = SetRaidTarget
function SetRaidTarget(unit, index, scope)
	if scope == "local" then
		return SW_SetLocalMark(unit, index)
	end
	return SetRaidTarget_(unit, index)
end

-- CastSpellByName(name, unit): a unit token or guid as the second argument casts at that unit
-- without touching the target; 1, 0, true and false keep the stock self-cast flag. "CLICK"
-- falls back to the targeting cursor.
local CastSpellByName_ = CastSpellByName
function CastSpellByName(name, unit)
	if type(unit) == "string" and unit ~= "" and not tonumber(unit) and string.lower(unit) ~= "click" then
		if not UnitExists_(unit) and not SW_Guid(unit) then
			return
		end
		if SW_CastAt(name, unit) then
			return
		end
		return CastSpellByName_(name)
	end
	if type(unit) == "string" and string.lower(unit) == "click" then
		return CastSpellByName_(name)
	end
	return CastSpellByName_(name, unit)
end

-- isIndoors(): the stock indoor test, under SuperWoW's spelling.
function isIndoors()
	if IsIndoors then
		return IsIndoors()
	end
end
