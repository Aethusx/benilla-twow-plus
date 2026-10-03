//! `Spell::Mod`: the local player's talent and item spell modifiers. Two `i32[64][29]` tables,
//! a row per `SpellFamilyFlags` bit and a column per op, each cell the absolute total the
//! server's last `SMSG_SET_FLAT_SPELL_MODIFIER` / `SMSG_SET_PCT_SPELL_MODIFIER` set.

use crate::dbc::{Databases, Row};
use crate::mirror::Mirror;
use crate::spells::col;

/// Ops per row (`SPELLMOD_SLOT_STRIDE` 0x74 / 4).
pub const OPS: usize = 29;
/// `SpellFamilyFlags` bits (`SPELLMOD_SLOT_COUNT`).
pub const BITS: usize = 64;

pub const OP_DAMAGE: u8 = 0;
pub const OP_DURATION: u8 = 1;
pub const OP_RANGE: u8 = 5;
pub const OP_RADIUS: u8 = 6;
pub const OP_CASTING_TIME: u8 = 10;
pub const OP_COOLDOWN: u8 = 11;
pub const OP_COST: u8 = 14;
pub const OP_GLOBAL_COOLDOWN: u8 = 21;

/// `SPELL_ATTR_EX3_IGNORE_CASTER_MODIFIERS`.
const ATTR_EX3_IGNORE_CASTER_MODIFIERS: u32 = 0x2000_0000;
/// `ChrClasses.dbc`'s `SpellClassSet` (`OFF_CHRCLASSES_SPELL_FAMILY` 0x3C / 4).
const CHRCLASSES_SPELL_FAMILY: usize = 0x3C / 4;

/// The two tables.
#[derive(Clone)]
pub struct Tables {
    flat: Vec<i32>,
    pct: Vec<i32>,
}

impl Default for Tables {
    fn default() -> Self {
        Self {
            flat: vec![0; BITS * OPS],
            pct: vec![0; BITS * OPS],
        }
    }
}

impl Tables {
    /// One packet: the cell's new total. An out-of-range bit or op is dropped.
    pub fn set(&mut self, flat: bool, bit: u8, op: u8, value: i32) {
        if usize::from(bit) >= BITS || usize::from(op) >= OPS {
            return;
        }
        let i = usize::from(bit) * OPS + usize::from(op);
        if flat {
            self.flat[i] = value;
        } else {
            self.pct[i] = value;
        }
    }

    /// The `(flat, pct)` sums over `family_flags`' set bits for `op`.
    pub fn sums(&self, family_flags: u64, op: u8) -> (i32, i32) {
        let mut flat = 0i32;
        let mut pct = 0i32;
        for bit in 0..BITS {
            if (family_flags >> bit) & 1 == 0 {
                continue;
            }
            let i = bit * OPS + usize::from(op);
            flat = flat.wrapping_add(self.flat[i]);
            pct = pct.wrapping_add(self.pct[i]);
        }
        (flat, pct)
    }
}

/// The local player's class `SpellFamilyName`, 0 when unresolved.
pub fn player_family(db: &Databases, mirror: &Mirror) -> u32 {
    let Some(me) = mirror.me() else {
        return 0;
    };
    db.get("ChrClasses")
        .and_then(|t| {
            t.row(u32::from(me.class()))
                .map(|r| r.u32(CHRCLASSES_SPELL_FAMILY))
        })
        .unwrap_or(0)
}

/// The `(flat, pct)` sums of the player's modifiers on `rec` for `op`; `None` for another family,
/// a spell that ignores caster modifiers, or no matching cell.
pub fn cell(tables: &Tables, family: u32, rec: &Row, op: u8) -> Option<(i32, i32)> {
    let family_name = rec.u32(col::FAMILY_NAME);
    if family_name == 0
        || family_name != family
        || rec.u32(col::ATTRIBUTES_EX3) & ATTR_EX3_IGNORE_CASTER_MODIFIERS != 0
    {
        return None;
    }
    let flags = rec.u64(col::FAMILY_FLAGS);
    if flags == 0 {
        return None;
    }
    let (flat, pct) = tables.sums(flags, op);
    (flat != 0 || pct != 0).then_some((flat, pct))
}

/// `Spell::Mod::Apply`: `base` with the player's modifiers for `op`, `(base + flat) *
/// max(0, 100 + pct) / 100`; `base` unchanged where [`cell`] finds none.
pub fn apply(tables: &Tables, family: u32, rec: &Row, op: u8, base: f32) -> f32 {
    let Some((flat, pct)) = cell(tables, family, rec, op) else {
        return base;
    };
    let total = (pct + 100).max(0);
    (base + flat as f32) * total as f32 * 0.01
}

/// The engine's integer applier (`0x6e6af0`), the cost and cast-time form: the division
/// truncates toward zero.
pub fn apply_int(tables: &Tables, family: u32, rec: &Row, op: u8, base: i32) -> i32 {
    let Some((flat, pct)) = cell(tables, family, rec, op) else {
        return base;
    };
    let total = i64::from((pct + 100).max(0));
    ((i64::from(base) + i64::from(flat)) * total / 100)
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}
