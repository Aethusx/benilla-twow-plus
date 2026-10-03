# SuperWoW on benilla

SuperWoW's Lua API, built on benilla. SuperWoW is `SuperWoWhook.dll` from
[balakethelock/SuperWoW](https://github.com/balakethelock/SuperWoW). Its code is closed and not
licensed for reuse, so nothing here comes from it. This crate is written from the project's
public documentation: the wiki's *Features* and *Changelog* pages, as of release 2.2. Addons see
`SUPERWOW_VERSION = "2.2"`.

```text
cargo run -p superwow --bin benilla-superwow    # SuperWoW alone
cargo run -p benilla-mods --bin benilla-mods    # with nampower and UnitXP SP3
```

## Layout

| file | what |
| --- | --- |
| `tokens.rs` | the unit-token grammar: guid strings, `mark1`-`mark8`, and `target`/`pet`/`owner` hops |
| `lua/mod.rs`, `lua/superwow.lua` | the natives, and the bootstrap that wraps the stock verbs |
| `net.rs` | `UNIT_CASTEVENT`, and the spellbook mirror |
| `spells.rs` | `Spell.dbc` reads for `SpellInfo` and the channel durations |
| `files.rs` | `ImportFile`, `ExportFile`, `CombatLogAdd` |
| `lib.rs` | the shared state, the frame system, the `FoV` and `NameplateRange` CVars |

`cargo test -p superwow` runs the grammar and the API against a bare VM.

## What it touches in benilla

Beyond nampower's and UnitXP's hooks (see their READMEs):

| file | change |
| --- | --- |
| `benilla-ui/src/script/unit/mod.rs` | `UnitTokenExtension`, `set_unit_token_extension`, `set_extra_unit_guids`, `unit_token_guid_in`; `check_unit_token` takes the VM |
| `benilla-ui/src/script/model.rs` | `Model::unit` and `Model::guid_of` fall back to the extension |
| `benilla-ui/src/script/aura/mod.rs` | `unit_aura_spell_id`, `player_buff_spell_id`; `auras_of` resolves through `Model::guid_of` |
| `benilla-ui/src/script/{inspect,spellbook}.rs` | resolve through `Model::guid_of` |
| `benilla-app/src/ui_aura/units.rs` | the extra guids join the aura walk |
| `benilla-app/src/ext.rs` | `ExtWorld::speeds`, `log_stamp` |

With no extension installed, every stock token resolves as before. The extension is only asked
about a token after the stock resolver has found nobody for it, or has rejected it.

## Ported

- **Unit tokens.** Every stock unit function also accepts a guid string, `mark1` to `mark8`, and
  an `owner` (or `pet`) suffix: `UnitName("targetowner")`, `UnitHealth("0xF130...")`,
  `UnitBuff("mark8", 1)`.
- **Extended returns.**
  - `UnitExists` also returns the guid.
  - `UnitBuff` and `UnitDebuff` also return the spell id.
  - `SetRaidTarget(unit, i, "local")` sets a mark on your client only.
  - `CastSpellByName(name, unit)` casts at that unit without changing your target.
- **New functions:**
  - `GetPlayerBuffID`, `SpellInfo`, `UnitPosition`, `CanLootUnit`
  - `IsSwimming`, `IsMounted`, `isIndoors`, `GetSpeed`
  - `ImportFile`, `ExportFile`, `CombatLogAdd`
  - `SetAutoloot`, `Clickthrough`, `TrackUnit`, `UntrackUnit`, `SetMouseoverUnit`
- **Events:** `UNIT_CASTEVENT`, with the kinds `START`, `CAST`, `CHANNEL`, `FAIL`, `MAINHAND`
  and `OFFHAND`.
- **Globals:** `SUPERWOW_VERSION`, `SUPERWOW_STRING`.
- **CVars that take effect:**
  - `FoV`: scales benilla's projection by the ratio to the 1.57 default.
  - `NameplateRange`: 10-80 yd. It overrides nampower's range when set to anything but 20.

## Where it differs, or isn't built yet

- **Files.** `ImportFile`/`ExportFile` use `benilla-config/Imports/<name>.txt` (the same folder
  as nampower). `CombatLogAdd` writes to `benilla-config/Logs`. The DLL writes into the game
  folder, but benilla never writes into the install.
- **New units.** A unit's token resolves from the frame after it is first streamed. Snapshots and
  aura lists cover every unit currently streamed.
- **Undocumented CVar defaults.** Upstream gives the range of most of its CVars but not every
  default. Those listed in `superwow.lua` use the stock behaviour's value.
- **Registered only, no effect yet:** `BackgroundSound`, `UncapSounds`, `SelectionCircleStyle`,
  `LootSparkle`, `HealingText`, `NameplateMotion` and the `ChatBubble*` CVars.
- **Kept as state, no effect yet:**
  - `SetAutoloot` and `Clickthrough` keep their switch but don't change looting or picking.
  - `TrackUnit` keeps the list but draws nothing on the minimap.
  - `SetMouseoverUnit` keeps the unit, but the stock `mouseover` token still follows the
    pointer.
- **`CastSpellByName(name, "CLICK")`** falls back to the targeting cursor.
- **Not ported:**
  - `RAW_COMBATLOG`, `CREATE_CHATBUBBLE`
  - `UnitNameplate`, `frame:GetName(1)` on nameplates
  - `GetWeaponEnchantID`, the `GetWeaponEnchantInfo(unit)` form
  - The mail and quest link functions, the map coordinate functions, `CursorPosition`
  - The macro `/tooltip` linking, the charges form of `GetContainerItemInfo`, and the druid
    `UnitMana` pair
  - The client fixes the DLL makes to the reference client (combat log owner names, buff bar,
    macro length). They are not API, and benilla's own behaviour stands.
