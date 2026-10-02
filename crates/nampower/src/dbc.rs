//! The client databases nampower reads, raw: a `WDBC` file is a header, fixed-width records of
//! 4-byte cells and a string block, so a column is an index and a cell reads as `u32`, `i32`,
//! `f32` or a string offset. Opened once from the player's own patch chain.

use std::collections::HashMap;

/// One `WDBC` file: its records by id (column 0) and its string block.
pub struct Dbc {
    fields: usize,
    cells: Vec<u32>,
    strings: Vec<u8>,
    by_id: HashMap<u32, usize>,
}

impl Dbc {
    /// Parse `bytes`; `None` for a file that is not `WDBC` or is cut short.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let word = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        if bytes.get(0..4)? != b"WDBC" {
            return None;
        }
        let records = word(4)? as usize;
        let fields = word(8)? as usize;
        let record_size = word(12)? as usize;
        let string_size = word(16)? as usize;
        if fields == 0 || record_size != fields * 4 {
            return None;
        }
        let body = 20;
        let strings_at = body + records * record_size;
        let cells: Vec<u32> = bytes
            .get(body..strings_at)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        let strings = bytes.get(strings_at..strings_at + string_size)?.to_vec();
        let by_id = (0..records).map(|r| (cells[r * fields], r)).collect();
        Some(Self {
            fields,
            cells,
            strings,
            by_id,
        })
    }

    pub fn field_count(&self) -> usize {
        self.fields
    }

    /// The record with id `id`.
    pub fn row(&self, id: u32) -> Option<Row<'_>> {
        let r = *self.by_id.get(&id)?;
        Some(Row {
            dbc: self,
            cells: &self.cells[r * self.fields..(r + 1) * self.fields],
        })
    }

    /// Every record, in file order.
    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.cells
            .chunks_exact(self.fields)
            .map(|cells| Row { dbc: self, cells })
    }

    fn string(&self, offset: u32) -> Option<&str> {
        let tail = self.strings.get(offset as usize..)?;
        let end = tail.iter().position(|b| *b == 0)?;
        std::str::from_utf8(&tail[..end]).ok()
    }
}

/// One record.
#[derive(Clone, Copy)]
pub struct Row<'a> {
    dbc: &'a Dbc,
    cells: &'a [u32],
}

impl Row<'_> {
    pub fn u32(&self, col: usize) -> u32 {
        self.cells.get(col).copied().unwrap_or(0)
    }

    pub fn i32(&self, col: usize) -> i32 {
        self.u32(col) as i32
    }

    pub fn f32(&self, col: usize) -> f32 {
        f32::from_bits(self.u32(col))
    }

    /// A string cell; empty for offset 0 or a bad offset.
    pub fn str(&self, col: usize) -> &str {
        self.dbc.string(self.u32(col)).unwrap_or("")
    }
}

/// The databases nampower reads, each `None` when the chain lacks it.
#[derive(Default)]
pub struct Databases {
    pub spell: Option<Dbc>,
    pub spell_range: Option<Dbc>,
    pub spell_duration: Option<Dbc>,
    pub spell_cast_times: Option<Dbc>,
    pub spell_icon: Option<Dbc>,
    pub item_display_info: Option<Dbc>,
    pub talent: Option<Dbc>,
    pub talent_tab: Option<Dbc>,
}

impl Databases {
    /// Read every table from the install's patch chain; a missing install loads nothing.
    pub fn load() -> Self {
        let Some(data) = benilla_formats::wow_data() else {
            return Self::default();
        };
        let Ok(chain) = benilla_formats::Chain::open(&data) else {
            return Self::default();
        };
        let read = |name: &str| {
            chain
                .read(&format!("DBFilesClient\\{name}.dbc"))
                .ok()
                .and_then(|b| Dbc::parse(&b))
        };
        Self {
            spell: read("Spell"),
            spell_range: read("SpellRange"),
            spell_duration: read("SpellDuration"),
            spell_cast_times: read("SpellCastTimes"),
            spell_icon: read("SpellIcon"),
            item_display_info: read("ItemDisplayInfo"),
            talent: read("Talent"),
            talent_tab: read("TalentTab"),
        }
    }

    pub fn spell(&self, id: u32) -> Option<SpellRec<'_>> {
        self.spell.as_ref()?.row(id).map(SpellRec)
    }

    /// `SpellDuration.dbc`'s base duration in ms; 0 for none, -1 for infinite.
    pub fn duration_ms(&self, index: u32) -> Option<i32> {
        Some(self.spell_duration.as_ref()?.row(index)?.i32(1))
    }

    /// `SpellCastTimes.dbc`'s base cast time in ms.
    pub fn cast_time_ms(&self, index: u32) -> Option<i32> {
        Some(self.spell_cast_times.as_ref()?.row(index)?.i32(1))
    }

    /// `SpellRange.dbc`: min, max, flags and the enUS name.
    pub fn range(&self, index: u32) -> Option<(f32, f32, u32, String)> {
        let row = self.spell_range.as_ref()?.row(index)?;
        Some((row.f32(1), row.f32(2), row.u32(3), row.str(4).to_string()))
    }

    /// `SpellIcon.dbc`'s texture path.
    pub fn spell_icon(&self, id: u32) -> Option<String> {
        let path = self.spell_icon.as_ref()?.row(id)?.str(1).to_string();
        (!path.is_empty()).then_some(path)
    }

    /// `ItemDisplayInfo.dbc`'s inventory icon, as `Interface\Icons\<name>`.
    pub fn item_icon(&self, display_id: u32) -> Option<String> {
        let row = self.item_display_info.as_ref()?.row(display_id)?;
        let name = row.str(5);
        (!name.is_empty()).then(|| format!("Interface\\Icons\\{name}"))
    }
}

/// The `SPELL_ATTR_*` and `SPELL_EFFECT_*` values nampower tests.
pub mod consts {
    pub const ATTR_RANGED: u32 = 0x2;
    pub const ATTR_ON_NEXT_SWING: u32 = 0x4;
    pub const ATTR_TRADESPELL: u32 = 0x20;
    pub const ATTR_HIDDEN_CLIENTSIDE: u32 = 0x80;
    pub const ATTR_DISABLED_WHILE_ACTIVE: u32 = 0x0200_0000;
    pub const ATTR_EX_CHANNELED: u32 = 0x4;
    pub const ATTR_EX_SELF_CHANNELED: u32 = 0x40;
    pub const ATTR_EX_NO_AURA_ICON: u32 = 0x1000_0000;
    pub const ATTR_EX2_AUTO_REPEAT: u32 = 0x20;
    pub const TARGET_FLAG_SOURCE_LOCATION: u32 = 0x20;
    pub const TARGET_FLAG_DEST_LOCATION: u32 = 0x40;
    pub const EFFECT_APPLY_AURA: u32 = 6;
    pub const EFFECT_CREATE_ITEM: u32 = 24;
    pub const EFFECT_OPEN_LOCK: u32 = 33;
    pub const EFFECT_APPLY_AREA_AURA_PARTY: u32 = 35;
    pub const EFFECT_SUMMON_GUARDIAN: u32 = 42;
    pub const EFFECT_TRADE_SKILL: u32 = 47;
    pub const EFFECT_TRANS_DOOR: u32 = 50;
    pub const EFFECT_ENCHANT_ITEM: u32 = 53;
    pub const EFFECT_ENCHANT_ITEM_TEMPORARY: u32 = 54;
    pub const EFFECT_OPEN_LOCK_ITEM: u32 = 59;
    pub const EFFECT_APPLY_AREA_AURA_RAID: u32 = 65;
    pub const EFFECT_ATTACK: u32 = 78;
    pub const EFFECT_APPLY_AREA_AURA_PET: u32 = 119;
    pub const EFFECT_APPLY_AREA_AURA_FRIEND: u32 = 128;
    pub const EFFECT_APPLY_AREA_AURA_ENEMY: u32 = 129;
    pub const AURA_MOUNTED: u32 = 78;
    pub const AURA_TRACK_CREATURES: u32 = 44;
    pub const AURA_TRACK_RESOURCES: u32 = 45;
    pub const AURA_TRACK_STEALTHED: u32 = 151;
    /// The global cooldown's `StartRecoveryCategory`.
    pub const GCD_CATEGORY: u32 = 133;
    /// Auto Shot.
    pub const AUTO_SHOT: u32 = 75;
    /// Power Overwhelming: its GCD was removed server-side, the client data still carries it.
    pub const POWER_OVERWHELMING: u32 = 51714;
}

/// One `Spell.dbc` row in nampower's `SpellRec` layout: one 4-byte cell per field, the eight
/// locale strings and their flags word inline, `SpellFamilyFlags` two cells.
#[derive(Clone, Copy)]
pub struct SpellRec<'a>(pub Row<'a>);

/// How a `SpellRec` column reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    U32,
    I32,
    F32,
    /// A localized string: eight cells, enUS first.
    Locale,
    /// Two cells, low dword first.
    U64,
}

/// The `SpellRec` columns: name as `GetSpellRec` keys it, first cell, count and kind.
pub const SPELL_COLUMNS: &[(&str, usize, usize, Kind)] = &[
    ("id", 0, 1, Kind::U32),
    ("school", 1, 1, Kind::U32),
    ("category", 2, 1, Kind::U32),
    ("castUI", 3, 1, Kind::U32),
    ("dispel", 4, 1, Kind::U32),
    ("mechanic", 5, 1, Kind::U32),
    ("attributes", 6, 1, Kind::U32),
    ("attributesEx", 7, 1, Kind::U32),
    ("attributesEx2", 8, 1, Kind::U32),
    ("attributesEx3", 9, 1, Kind::U32),
    ("attributesEx4", 10, 1, Kind::U32),
    ("stances", 11, 1, Kind::U32),
    ("stancesNot", 12, 1, Kind::U32),
    ("targets", 13, 1, Kind::U32),
    ("targetCreatureType", 14, 1, Kind::U32),
    ("requiresSpellFocus", 15, 1, Kind::U32),
    ("casterAuraState", 16, 1, Kind::U32),
    ("targetAuraState", 17, 1, Kind::U32),
    ("castingTimeIndex", 18, 1, Kind::U32),
    ("recoveryTime", 19, 1, Kind::U32),
    ("categoryRecoveryTime", 20, 1, Kind::U32),
    ("interruptFlags", 21, 1, Kind::U32),
    ("auraInterruptFlags", 22, 1, Kind::U32),
    ("channelInterruptFlags", 23, 1, Kind::U32),
    ("procFlags", 24, 1, Kind::U32),
    ("procChance", 25, 1, Kind::U32),
    ("procCharges", 26, 1, Kind::U32),
    ("maxLevel", 27, 1, Kind::U32),
    ("baseLevel", 28, 1, Kind::U32),
    ("spellLevel", 29, 1, Kind::U32),
    ("durationIndex", 30, 1, Kind::U32),
    ("powerType", 31, 1, Kind::U32),
    ("manaCost", 32, 1, Kind::U32),
    ("manaCostPerlevel", 33, 1, Kind::U32),
    ("manaPerSecond", 34, 1, Kind::U32),
    ("manaPerSecondPerLevel", 35, 1, Kind::U32),
    ("rangeIndex", 36, 1, Kind::U32),
    ("speed", 37, 1, Kind::F32),
    ("modalNextSpell", 38, 1, Kind::U32),
    ("stackAmount", 39, 1, Kind::U32),
    ("totem", 40, 2, Kind::U32),
    ("reagent", 42, 8, Kind::I32),
    ("reagentCount", 50, 8, Kind::U32),
    ("equippedItemClass", 58, 1, Kind::I32),
    ("equippedItemSubClassMask", 59, 1, Kind::I32),
    ("equippedItemInventoryTypeMask", 60, 1, Kind::I32),
    ("effect", 61, 3, Kind::U32),
    ("effectDieSides", 64, 3, Kind::I32),
    ("effectBaseDice", 67, 3, Kind::U32),
    ("effectDicePerLevel", 70, 3, Kind::F32),
    ("effectRealPointsPerLevel", 73, 3, Kind::F32),
    ("effectBasePoints", 76, 3, Kind::I32),
    ("effectMechanic", 79, 3, Kind::U32),
    ("effectImplicitTargetA", 82, 3, Kind::U32),
    ("effectImplicitTargetB", 85, 3, Kind::U32),
    ("effectRadiusIndex", 88, 3, Kind::U32),
    ("effectApplyAuraName", 91, 3, Kind::U32),
    ("effectAmplitude", 94, 3, Kind::U32),
    ("effectMultipleValue", 97, 3, Kind::F32),
    ("effectChainTarget", 100, 3, Kind::U32),
    ("effectItemType", 103, 3, Kind::U32),
    ("effectMiscValue", 106, 3, Kind::I32),
    ("effectTriggerSpell", 109, 3, Kind::U32),
    ("effectPointsPerComboPoint", 112, 3, Kind::F32),
    ("spellVisual", 115, 1, Kind::U32),
    ("spellVisual2", 116, 1, Kind::U32),
    ("spellIconID", 117, 1, Kind::U32),
    ("activeIconID", 118, 1, Kind::U32),
    ("spellPriority", 119, 1, Kind::U32),
    ("name", 120, 8, Kind::Locale),
    ("rank", 129, 8, Kind::Locale),
    ("description", 138, 8, Kind::Locale),
    ("tooltip", 147, 8, Kind::Locale),
    ("manaCostPercentage", 156, 1, Kind::U32),
    ("startRecoveryCategory", 157, 1, Kind::U32),
    ("startRecoveryTime", 158, 1, Kind::U32),
    ("maxTargetLevel", 159, 1, Kind::U32),
    ("spellFamilyName", 160, 1, Kind::U32),
    ("spellFamilyFlags", 161, 2, Kind::U64),
    ("maxAffectedTargets", 163, 1, Kind::U32),
    ("dmgClass", 164, 1, Kind::U32),
    ("preventionType", 165, 1, Kind::U32),
    ("stanceBarOrder", 166, 1, Kind::U32),
    ("dmgMultiplier", 167, 3, Kind::F32),
    ("minFactionId", 170, 1, Kind::U32),
    ("minReputation", 171, 1, Kind::U32),
    ("requiredAuraVision", 172, 1, Kind::U32),
];

/// A column by its `GetSpellRec` name, case-insensitively.
pub fn spell_column(name: &str) -> Option<(usize, usize, Kind)> {
    SPELL_COLUMNS
        .iter()
        .find(|(n, ..)| n.eq_ignore_ascii_case(name))
        .map(|&(_, at, count, kind)| (at, count, kind))
}

impl<'a> SpellRec<'a> {
    pub fn id(&self) -> u32 {
        self.0.u32(0)
    }
    pub fn category(&self) -> u32 {
        self.0.u32(2)
    }
    pub fn attributes(&self) -> u32 {
        self.0.u32(6)
    }
    pub fn attributes_ex(&self) -> u32 {
        self.0.u32(7)
    }
    pub fn attributes_ex2(&self) -> u32 {
        self.0.u32(8)
    }
    pub fn targets(&self) -> u32 {
        self.0.u32(13)
    }
    pub fn casting_time_index(&self) -> u32 {
        self.0.u32(18)
    }
    pub fn recovery_time(&self) -> u32 {
        self.0.u32(19)
    }
    pub fn category_recovery_time(&self) -> u32 {
        self.0.u32(20)
    }
    pub fn duration_index(&self) -> u32 {
        self.0.u32(30)
    }
    pub fn range_index(&self) -> u32 {
        self.0.u32(36)
    }
    pub fn effect(&self, i: usize) -> u32 {
        self.0.u32(61 + i)
    }
    pub fn implicit_target_a(&self, i: usize) -> u32 {
        self.0.u32(82 + i)
    }
    pub fn implicit_target_b(&self, i: usize) -> u32 {
        self.0.u32(85 + i)
    }
    pub fn apply_aura_name(&self, i: usize) -> u32 {
        self.0.u32(91 + i)
    }
    pub fn amplitude(&self, i: usize) -> u32 {
        self.0.u32(94 + i)
    }
    pub fn misc_value(&self, i: usize) -> i32 {
        self.0.i32(106 + i)
    }
    pub fn icon_id(&self) -> u32 {
        self.0.u32(117)
    }
    pub fn name(&self) -> &'a str {
        let row: Row<'a> = self.0;
        row.dbc.string(row.u32(120)).unwrap_or("")
    }
    pub fn rank(&self) -> &'a str {
        let row: Row<'a> = self.0;
        row.dbc.string(row.u32(129)).unwrap_or("")
    }
    pub fn start_recovery_category(&self) -> u32 {
        self.0.u32(157)
    }
    pub fn start_recovery_time(&self) -> u32 {
        self.0.u32(158)
    }

    /// `SpellIsOnGcd`: the GCD category, except Power Overwhelming, whose GCD the server dropped.
    pub fn on_gcd(&self) -> bool {
        self.id() != consts::POWER_OVERWHELMING
            && self.start_recovery_category() == consts::GCD_CATEGORY
    }

    pub fn channeled(&self) -> bool {
        self.attributes_ex() & (consts::ATTR_EX_CHANNELED | consts::ATTR_EX_SELF_CHANNELED) != 0
    }

    /// A ground-targeted spell: its cast raises the terrain cursor.
    pub fn targeting(&self) -> bool {
        self.targets() & (consts::TARGET_FLAG_SOURCE_LOCATION | consts::TARGET_FLAG_DEST_LOCATION)
            != 0
    }

    pub fn on_swing(&self) -> bool {
        self.attributes() & consts::ATTR_ON_NEXT_SWING != 0
    }

    pub fn auto_repeat(&self) -> bool {
        self.attributes_ex2() & consts::ATTR_EX2_AUTO_REPEAT != 0
    }

    /// `SpellIsAttackTradeskillOrEnchant`: never queued, never spam-guarded.
    pub fn special(&self) -> bool {
        use consts::*;
        self.id() == AUTO_SHOT
            || self.attributes() & ATTR_TRADESPELL != 0
            || matches!(
                self.effect(0),
                EFFECT_ATTACK
                    | EFFECT_TRADE_SKILL
                    | EFFECT_TRANS_DOOR
                    | EFFECT_ENCHANT_ITEM
                    | EFFECT_ENCHANT_ITEM_TEMPORARY
                    | EFFECT_CREATE_ITEM
                    | EFFECT_OPEN_LOCK
                    | EFFECT_OPEN_LOCK_ITEM
            )
    }

    /// `SpellIsMounting`: an `APPLY_AURA` effect of `SPELL_AURA_MOUNTED`.
    pub fn mounting(&self) -> bool {
        (0..3).any(|i| {
            self.effect(i) == consts::EFFECT_APPLY_AURA
                && self.apply_aura_name(i) == consts::AURA_MOUNTED
        })
    }

    /// Whether any effect applies an aura.
    pub fn applies_aura(&self) -> bool {
        use consts::*;
        (0..3).any(|i| {
            matches!(
                self.effect(i),
                EFFECT_APPLY_AURA
                    | EFFECT_APPLY_AREA_AURA_PARTY
                    | EFFECT_APPLY_AREA_AURA_RAID
                    | EFFECT_APPLY_AREA_AURA_FRIEND
                    | EFFECT_APPLY_AREA_AURA_ENEMY
                    | EFFECT_APPLY_AREA_AURA_PET
            )
        })
    }

    /// `IsAuraHiddenForLua`: hidden client-side, no aura icon, or a tracking aura.
    pub fn aura_hidden(&self) -> bool {
        use consts::*;
        self.attributes() & ATTR_HIDDEN_CLIENTSIDE != 0
            || self.attributes_ex() & ATTR_EX_NO_AURA_ICON != 0
            || (0..3).any(|i| {
                matches!(
                    self.apply_aura_name(i),
                    AURA_TRACK_CREATURES | AURA_TRACK_RESOURCES | AURA_TRACK_STEALTHED
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(records: &[[u32; 3]], strings: &[u8]) -> Vec<u8> {
        let mut out = b"WDBC".to_vec();
        for w in [records.len() as u32, 3, 12, strings.len() as u32] {
            out.extend(w.to_le_bytes());
        }
        for r in records {
            for c in r {
                out.extend(c.to_le_bytes());
            }
        }
        out.extend(strings);
        out
    }

    #[test]
    fn rows_resolve_by_id_and_strings_by_offset() {
        let dbc = Dbc::parse(&file(&[[7, 1, 0], [9, 2, 1]], b"\0Fireball\0")).unwrap();
        let row = dbc.row(9).unwrap();
        assert_eq!(row.u32(1), 2);
        assert_eq!(row.str(2), "Fireball");
        assert_eq!(dbc.row(7).unwrap().str(2), "");
        assert!(dbc.row(8).is_none());
    }

    #[test]
    fn a_file_that_is_not_wdbc_or_is_short_is_refused() {
        assert!(Dbc::parse(b"WDB2").is_none());
        let mut short = file(&[[1, 2, 3]], b"\0");
        short.truncate(30);
        assert!(Dbc::parse(&short).is_none());
    }

    #[test]
    fn the_column_table_tiles_the_173_cell_record() {
        let mut next = 0;
        for &(name, at, count, kind) in SPELL_COLUMNS {
            assert_eq!(at, next, "{name} starts where the last ended");
            // A locale string carries its flags word after its eight cells.
            next = at + count + usize::from(kind == Kind::Locale);
        }
        assert_eq!(next, 173);
    }
}
