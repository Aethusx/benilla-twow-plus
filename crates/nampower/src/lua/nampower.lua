-- nampower's bootstrap: runs on every in-game VM after the natives are installed and before the
-- stock interface and the addons load. It declares the NP_ CVars as an addon declares its own,
-- wraps the stock verbs nampower extends, and builds the functions that are plain Lua over the
-- 1.12 API. Written to the 1.12 grammar: no `...` expressions, `table.getn` for lengths.

-- The NP_ CVars; a saved value outranks the default.
for i = 1, table.getn(NP_CVARS) do
	RegisterCVar(NP_CVARS[i][1], NP_CVARS[i][2])
end
NP_CVARS = nil

-- A written NP_ value reaches the queue at once.
local SetCVar_ = SetCVar
function SetCVar(name, value, token)
	SetCVar_(name, value, token)
	if type(name) == "string" and string.lower(string.sub(name, 1, 3)) == "np_" then
		NP_OnCVar(name, value)
	end
end

-- A mouseover macro's SpellTargetUnit retargets the queued casts too.
local SpellTargetUnit_ = SpellTargetUnit
function SpellTargetUnit(unit)
	NP_Retarget(unit)
	return SpellTargetUnit_(unit)
end

local BOOKS = { "spell", "pet" }

-- The 1-based slot of a spell name ("Name" or "Name(Rank N)") in one book: the exact rank, else
-- the highest.
local function slotInBook(name, book)
	local want, rank = string.lower(name), nil
	local open = string.find(want, "%(")
	if open and string.sub(want, -1) == ")" then
		rank = string.sub(want, open + 1, -2)
		want = string.gsub(string.sub(want, 1, open - 1), "%s+$", "")
	end
	local found = 0
	local i = 1
	while true do
		local n, r = GetSpellName(i, book)
		if not n then
			break
		end
		if string.lower(n) == want then
			if rank == nil then
				found = i
			elseif string.lower(r or "") == rank then
				return i
			end
		end
		i = i + 1
	end
	return found
end

-- GetSpellSlotTypeIdForName(name): slot, "spell"/"pet"/"unknown", spell id.
function GetSpellSlotTypeIdForName(name)
	if type(name) ~= "string" then
		return 0, "unknown", 0
	end
	for b = 1, table.getn(BOOKS) do
		local slot = slotInBook(name, BOOKS[b])
		if slot > 0 then
			local n, r = GetSpellName(slot, BOOKS[b])
			local id = NP_SpellId(n .. "(" .. (r or "") .. ")")
			if (id or 0) == 0 then
				id = NP_SpellId(n) or 0
			end
			return slot, BOOKS[b], id
		end
	end
	return 0, "unknown", 0
end

-- A spellbook argument as nampower widens it: a slot, a name, or "spellId:N".
local function bookSlot(id, book)
	if type(id) ~= "string" or tonumber(id) then
		return id, book
	end
	if string.sub(id, 1, 8) == "spellId:" then
		local name, rank = GetSpellNameAndRankForId(tonumber(string.sub(id, 9)) or 0)
		if not name then
			return nil, book
		end
		book = book or "spell"
		local slot = slotInBook(name .. "(" .. (rank or "") .. ")", book)
		if slot == 0 then
			slot = slotInBook(name, book)
		end
		return slot > 0 and slot or nil, book
	end
	local slot, kind = GetSpellSlotTypeIdForName(id)
	if slot == 0 then
		return nil, book
	end
	return slot, kind
end

local function widen(name)
	local orig = getglobal(name)
	if not orig then
		return
	end
	setglobal(name, function(id, book, extra)
		local slot, kind = bookSlot(id, book)
		if not slot then
			return nil
		end
		return orig(slot, kind or book, extra)
	end)
end
widen("GetSpellTexture")
widen("GetSpellName")
widen("GetSpellCooldown")
widen("GetSpellAutocast")
widen("ToggleSpellAutocast")
widen("PickupSpell")
widen("CastSpell")
widen("IsCurrentCast")
widen("IsSpellPassive")

-- CastSpellByName(name, unit): a unit token or guid string as the second argument casts at that
-- unit; 1 still casts on self.
local CastSpellByName_ = CastSpellByName
function CastSpellByName(name, unit)
	if type(unit) == "string" and unit ~= "" and not tonumber(unit) then
		local id = NP_SpellId(name)
		if id and id > 0 then
			NP_Cast(id, unit, 0, 0)
			return
		end
	end
	return CastSpellByName_(name, unit)
end

function QueueSpellByName(name)
	local id = NP_SpellId(name)
	if id and id > 0 then
		NP_Cast(id, nil, 1, 0)
	end
end

function CastSpellByNameNoQueue(name, unit)
	local id = NP_SpellId(name)
	if not id or id == 0 then
		return
	end
	if type(unit) ~= "string" or unit == "" or tonumber(unit) then
		unit = (unit and unit ~= 0 and unit ~= "0") and "player" or nil
	end
	NP_Cast(id, unit, 0, 1)
end

function CastSpellNoQueue(spell, book, unit)
	local id
	if type(spell) == "number" or tonumber(spell) then
		local bk = book
		if bk == 0 or bk == nil then
			bk = "spell"
		elseif bk == 1 then
			bk = "pet"
		end
		local n, r = GetSpellName(tonumber(spell), bk)
		if n then
			id = NP_SpellId(n .. "(" .. (r or "") .. ")")
			if (id or 0) == 0 then
				id = NP_SpellId(n)
			end
		end
	else
		id = NP_SpellId(spell)
	end
	if id and id > 0 then
		NP_Cast(id, unit, 0, 1)
	end
end

-- An item use's optional target, once the cursor is up.
local function targetAfterUse(target)
	if target and SpellIsTargeting() then
		SpellTargetUnit(target)
	end
end

function UseItemIdOrName(item, target)
	local bag, slot = FindPlayerItemSlot(item)
	if not slot then
		return 0
	end
	if bag == nil then
		UseInventoryItem(slot + 1)
	else
		UseContainerItem(bag, slot)
	end
	targetAfterUse(target)
	return 1
end

function UseTrinket(want, target)
	local slot = NP_TrinketSlot(want)
	if not slot then
		return -1
	end
	UseInventoryItem(slot)
	targetAfterUse(target)
	return 1
end

-- LearnTalentRank(page, index, rank): the stock LearnTalent learns the next rank, so the rank
-- asked for is reached one learn at a time.
function LearnTalentRank(page, index, rank)
	if type(page) ~= "number" or page < 1 or page > 3 or type(index) ~= "number" or index < 1
		or index > 32 or type(rank) ~= "number" or rank < 1 or rank > 5 then
		error("Usage: LearnTalentRank(talentPage 1-3, talentIndex 1-32, rank 1-5)")
	end
	local name, _, _, _, current = GetTalentInfo(page, index)
	if not name then
		error("LearnTalentRank: no talent at " .. page .. "/" .. index)
	end
	if (current or 0) < rank then
		LearnTalent(page, index)
	end
	return 1
end

-- DisenchantAll: one item every five seconds from the backpack and bags 1-4, by id or name, or
-- weapons and armor of the named qualities. Quest items are never touched; soulbound items only
-- when asked.
local DISENCHANT = { active = false }
local QUALITY = { greens = 2, blues = 3, purples = 4 }

local scanner
local function soulbound(bag, slot)
	if not scanner then
		scanner = CreateFrame("GameTooltip", "NampowerScanTooltip", nil, "GameTooltipTemplate")
	end
	scanner:SetOwner(WorldFrame, "ANCHOR_NONE")
	scanner:SetBagItem(bag, slot)
	local line = getglobal("NampowerScanTooltipTextLeft2")
	local text = line and line:GetText()
	scanner:Hide()
	return text == ITEM_SOULBOUND
end

local function itemId(link)
	local _, _, id = string.find(link or "", "item:(%d+)")
	return tonumber(id)
end

local function nextItem()
	for bag = 0, 4 do
		for slot = 1, GetContainerNumSlots(bag) do
			local id = itemId(GetContainerItemLink(bag, slot))
			if id then
				local class = GetItemStatsField(id, "class")
				local quality = GetItemStatsField(id, "quality")
				local name = GetItemStatsField(id, "displayName")
				local hit
				if DISENCHANT.item then
					hit = id == DISENCHANT.item
						or (name and string.lower(name) == DISENCHANT.item)
				else
					hit = (class == 2 or class == 4) and quality and DISENCHANT.qualities[quality]
				end
				if hit and class ~= 12 and (DISENCHANT.soulbound or not soulbound(bag, slot)) then
					return bag, slot, GetContainerItemLink(bag, slot)
				end
			end
		end
	end
end

local function disenchantNext()
	local bag, slot, link = nextItem()
	if not bag then
		DISENCHANT.active = false
		DEFAULT_CHAT_FRAME:AddMessage("No more items to disenchant.")
		return 0
	end
	DEFAULT_CHAT_FRAME:AddMessage("Disenchanting " .. link .. " move during cast to cancel.")
	CastSpellByName("Disenchant")
	if SpellIsTargeting() then
		PickupContainerItem(bag, slot)
	end
	DISENCHANT.next = GetTime() + 5
	return 1
end

function NP_DisenchantStop()
	if DISENCHANT.active then
		DISENCHANT.active = false
		DEFAULT_CHAT_FRAME:AddMessage("Disenchant interrupted or failed.")
	end
end

local ticker = CreateFrame("Frame")
ticker:SetScript("OnUpdate", function()
	if DISENCHANT.active and GetTime() >= DISENCHANT.next then
		disenchantNext()
	end
end)

function DisenchantAll(what, includeSoulbound)
	DISENCHANT.item = nil
	DISENCHANT.qualities = {}
	if type(what) == "string" and not tonumber(what) then
		local any = false
		for word in string.gfind(string.lower(what), "[^|]+") do
			if QUALITY[word] then
				DISENCHANT.qualities[QUALITY[word]] = true
				any = true
			end
		end
		if not any then
			DISENCHANT.item = string.lower(what)
		end
	else
		DISENCHANT.item = tonumber(what)
	end
	DISENCHANT.soulbound = includeSoulbound and includeSoulbound ~= 0
	DISENCHANT.active = true
	return disenchantNext()
end
