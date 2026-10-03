//! `Spell.dbc` and its companion tables as the DLL reads them: each column is the DLL's record
//! byte offset (`Offsets.h`) over 4.

use std::sync::Arc;

use crate::dbc::{Databases, Dbc, Row};

/// `Spell.dbc` columns, `OFF_SPELL_RECORD_*` / 4.
pub mod col {
    pub const SCHOOL: usize = 1; // +0x04
    pub const CATEGORY: usize = 0x08 / 4;
    pub const DISPEL: usize = 0x10 / 4;
    pub const MECHANIC: usize = 0x14 / 4;
    pub const ATTRIBUTES: usize = 0x18 / 4;
    pub const ATTRIBUTES_EX: usize = 0x1C / 4;
    pub const ATTRIBUTES_EX2: usize = 0x20 / 4;
    pub const ATTRIBUTES_EX3: usize = 0x24 / 4;
    pub const ATTRIBUTES_EX4: usize = 0x28 / 4;
    pub const SHAPESHIFT_MASK: usize = 0x2C / 4;
    pub const TARGETS: usize = 0x34 / 4;
    pub const TARGET_CREATURE_TYPE: usize = 0x38 / 4;
    pub const REQUIRES_SPELL_FOCUS: usize = 0x3C / 4;
    pub const CASTER_AURA_STATE: usize = 0x40 / 4;
    pub const TARGET_AURA_STATE: usize = 0x44 / 4;
    pub const CASTING_TIME_INDEX: usize = 0x48 / 4;
    pub const RECOVERY_TIME: usize = 0x4C / 4;
    pub const CATEGORY_RECOVERY_TIME: usize = 0x50 / 4;
    pub const INTERRUPT_FLAGS: usize = 0x54 / 4;
    pub const AURA_INTERRUPT_FLAGS: usize = 0x58 / 4;
    pub const CHANNEL_INTERRUPT_FLAGS: usize = 0x5C / 4;
    pub const PROC_FLAGS: usize = 0x60 / 4;
    pub const PROC_CHANCE: usize = 0x64 / 4;
    pub const PROC_CHARGES: usize = 0x68 / 4;
    pub const MAX_LEVEL: usize = 0x6C / 4;
    pub const BASE_LEVEL: usize = 0x70 / 4;
    pub const SPELL_LEVEL: usize = 0x74 / 4;
    pub const DURATION_INDEX: usize = 0x78 / 4;
    pub const POWER_TYPE: usize = 0x7C / 4;
    pub const MANA_COST: usize = 0x80 / 4;
    pub const MANA_COST_PER_LEVEL: usize = 0x84 / 4;
    pub const MANA_PER_SECOND: usize = 0x88 / 4;
    pub const MANA_PER_SECOND_PER_LEVEL: usize = 0x8C / 4;
    pub const RANGE_INDEX: usize = 0x90 / 4;
    pub const SPEED: usize = 0x94 / 4;
    pub const MODAL_NEXT_SPELL: usize = 0x98 / 4;
    pub const STACK_AMOUNT: usize = 0x9C / 4;
    pub const TOTEM: usize = 0xA0 / 4;
    pub const REAGENT: usize = 0xA8 / 4;
    pub const REAGENT_COUNT: usize = 0xC8 / 4;
    pub const EQUIPPED_ITEM_CLASS: usize = 0xE8 / 4;
    pub const EQUIPPED_ITEM_SUBCLASS_MASK: usize = 0xEC / 4;
    pub const EQUIPPED_ITEM_INVTYPE_MASK: usize = 0xF0 / 4;
    pub const EFFECT: usize = 0xF4 / 4;
    pub const EFFECT_DIE_SIDES: usize = 0x100 / 4;
    pub const EFFECT_BASE_DICE: usize = 0x10C / 4;
    pub const EFFECT_DICE_PER_LEVEL: usize = 0x118 / 4;
    pub const EFFECT_REAL_POINTS_PER_LEVEL: usize = 0x124 / 4;
    pub const EFFECT_BASE_POINTS: usize = 0x130 / 4;
    pub const EFFECT_MECHANIC: usize = 0x13C / 4;
    pub const EFFECT_IMPLICIT_TARGET_A: usize = 0x148 / 4;
    pub const EFFECT_IMPLICIT_TARGET_B: usize = 0x154 / 4;
    pub const EFFECT_RADIUS_INDEX: usize = 0x160 / 4;
    pub const EFFECT_APPLY_AURA_NAME: usize = 0x16C / 4;
    pub const EFFECT_AMPLITUDE: usize = 0x178 / 4;
    pub const EFFECT_MULTIPLE_VALUE: usize = 0x184 / 4;
    pub const EFFECT_CHAIN_TARGET: usize = 0x190 / 4;
    pub const EFFECT_ITEM_TYPE: usize = 0x19C / 4;
    pub const EFFECT_MISC_VALUE: usize = 0x1A8 / 4;
    pub const EFFECT_TRIGGER_SPELL: usize = 0x1B4 / 4;
    pub const EFFECT_POINTS_PER_COMBO_POINT: usize = 0x1C0 / 4;
    pub const SPELL_VISUAL: usize = 0x1CC / 4;
    pub const ICON_ID: usize = 0x1D4 / 4;
    pub const ACTIVE_ICON_ID: usize = 0x1D8 / 4;
    pub const SPELL_PRIORITY: usize = 0x1DC / 4;
    pub const NAME: usize = 0x1E0 / 4;
    pub const RANK: usize = 0x204 / 4;
    pub const DESCRIPTION: usize = 0x228 / 4;
    pub const TOOLTIP: usize = 0x24C / 4;
    pub const MANA_COST_PERCENT: usize = 0x270 / 4;
    pub const START_RECOVERY_CATEGORY: usize = 0x274 / 4;
    pub const START_RECOVERY_TIME: usize = 0x278 / 4;
    pub const MAX_TARGET_LEVEL: usize = 0x27C / 4;
    pub const FAMILY_NAME: usize = 0x280 / 4;
    pub const FAMILY_FLAGS: usize = 0x284 / 4;
    pub const MAX_AFFECTED_TARGETS: usize = 0x28C / 4;
    pub const DAMAGE_CLASS: usize = 0x290 / 4;
    pub const PREVENTION_TYPE: usize = 0x294 / 4;
    pub const STANCE_BAR_ORDER: usize = 0x298 / 4;
    pub const DAMAGE_MULTIPLIER: usize = 0x29C / 4;
    pub const MIN_FACTION: usize = 0x2A8 / 4;
    pub const MIN_REPUTATION: usize = 0x2AC / 4;
    pub const REQUIRED_AURA_VISION: usize = 0x2B0 / 4;
}

/// `Spell.dbc`'s three effects.
pub const EFFECTS: usize = 3;

/// `SPELL_ATTR_PASSIVE`, `Attributes` bit 6.
pub const ATTR_PASSIVE: u32 = 0x40;
/// `SPELL_ATTR_EX2_HEALTH_FUNNEL`, `AttributesEx2` bit 11.
pub const ATTR_EX2_HEALTH_FUNNEL: u32 = 0x800;
/// `SPELL_EFFECT_DISENCHANT`.
pub const EFFECT_DISENCHANT: u32 = 99;

/// `Spell.dbc`, opened once.
pub fn table(db: &Databases) -> Option<Arc<Dbc>> {
    db.get("Spell")
}

/// A `SpellIcon.dbc` path (column 1) by icon id; `None` for 0, a missing row or an empty path.
pub fn icon_path(db: &Databases, icon_id: u32) -> Option<String> {
    let icons = db.get("SpellIcon")?;
    let path = icons.row(icon_id)?.str(1);
    (!path.is_empty()).then(|| path.to_string())
}

/// The spell's icon path, its active icon when `active` (`Spell::Lookup::IconPath`).
pub fn spell_icon(db: &Databases, rec: &Row, active: bool) -> Option<String> {
    icon_path(
        db,
        rec.u32(if active {
            col::ACTIVE_ICON_ID
        } else {
            col::ICON_ID
        }),
    )
}

/// `SpellCastTimes.dbc`'s base time (column 1), ms; 0 for no row.
pub fn cast_time_ms(db: &Databases, rec: &Row) -> i32 {
    db.get("SpellCastTimes")
        .and_then(|t| t.row(rec.u32(col::CASTING_TIME_INDEX)).map(|r| r.i32(1)))
        .unwrap_or(0)
}

/// `SpellRange.dbc`'s `(min, max)` yards (columns 1 and 2); zeros for no row.
pub fn range(db: &Databases, rec: &Row) -> (f32, f32) {
    db.get("SpellRange")
        .and_then(|t| {
            t.row(rec.u32(col::RANGE_INDEX))
                .map(|r| (r.f32(1), r.f32(2)))
        })
        .unwrap_or((0.0, 0.0))
}

/// `Spell::Lookup::NthRecipeReagentItemID`: the `n`-th (1-based) reagent, stopping at the first
/// empty one.
pub fn nth_reagent(rec: &Row, n: usize) -> Option<u32> {
    if n == 0 {
        return None;
    }
    (0..8)
        .map(|i| rec.u32(col::REAGENT + i))
        .take_while(|id| *id != 0)
        .nth(n - 1)
}

/// `Spell::Lookup::IsFitToFamily`.
pub fn fits_family(rec: &Row, family: u32, mask: u64) -> bool {
    rec.u32(col::FAMILY_NAME) == family && rec.u64(col::FAMILY_FLAGS) & mask != 0
}

/// `Unit::Power::PowerTypeToken`.
pub fn power_token(power_type: i64) -> &'static str {
    match power_type {
        0 => "MANA",
        1 => "RAGE",
        2 => "FOCUS",
        3 => "ENERGY",
        4 => "HAPPINESS",
        5 => "RUNES",
        6 => "RUNIC_POWER",
        _ => "UNKNOWN",
    }
}

/// `GetPowerCost 0x6e31b0` for the local player, benilla's `power_cost_at`: `manaCost`, the
/// signed per-level term against `baseLevel`, `ManaCostPercentage` of the per-type basis (base
/// health, base mana, 1000 for rage, 100 for focus and energy), then `SPELLMOD_COST`, clamped at 0.
pub fn power_cost(
    rec: &Row,
    me: &crate::mirror::Fields,
    mods: &crate::spellmod::Tables,
    family: u32,
) -> u32 {
    use crate::mirror::field;
    let pct = i64::from(rec.i32(col::MANA_COST_PERCENT));
    let basis = if pct == 0 {
        0
    } else {
        match rec.i32(col::POWER_TYPE) {
            -2 => i64::from(me.u32(field::UNIT_BASE_HEALTH)),
            0 => i64::from(me.u32(field::UNIT_BASE_MANA)),
            1 => 1000,
            2 | 3 => 100,
            _ => 0,
        }
    };
    let level_delta = i64::from(me.level()) - i64::from(rec.u32(col::BASE_LEVEL));
    let cost = i64::from(rec.i32(col::MANA_COST))
        + level_delta * i64::from(rec.i32(col::MANA_COST_PER_LEVEL))
        + basis * pct / 100;
    let cost = cost.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    u32::try_from(crate::spellmod::apply_int(
        mods,
        family,
        rec,
        crate::spellmod::OP_COST,
        cost,
    ))
    .unwrap_or(0)
}

/// benilla-formats' parsed spell tables, for the range core and the description tokens.
pub fn catalog(db: &Databases) -> Option<Arc<benilla_formats::SpellCatalog>> {
    db.catalog(benilla_formats::load_spell_catalog)
}

pub fn range_catalog(db: &Databases) -> Option<Arc<benilla_formats::SpellRangeCatalog>> {
    db.catalog(benilla_formats::load_spell_ranges)
}
