//! `Talent::SpellSet` and the `SkillLineAbility.dbc` walks the spell and talent modules share.

use std::collections::{HashMap, HashSet};

use crate::dbc::Databases;
use crate::mirror::Mirror;
use crate::spells::{self, col};

/// `Talent.dbc` columns (`OFF_TALENT_*` / 4).
pub mod talent_col {
    pub const TAB: usize = 1; // +0x04
    pub const TIER: usize = 2;
    pub const COLUMN: usize = 3;
    /// Nine rank spell ids (`OFF_TALENT_SPELL_RANK` 0x10).
    pub const SPELL_RANK: usize = 0x10 / 4;
    pub const MAX_RANKS: usize = 9;
}

/// `SkillLineAbility.dbc` columns (`OFF_SLA_*` / 4).
pub mod sla_col {
    pub const SKILL: usize = 1; // +0x04
    pub const SPELL: usize = 0x08 / 4;
    pub const RACE_MASK: usize = 0x0C / 4;
    pub const CLASS_MASK: usize = 0x10 / 4;
    pub const EXCLUDE_RACE: usize = 0x14 / 4;
    pub const EXCLUDE_CLASS: usize = 0x18 / 4;
    pub const SUPERCEDED_BY: usize = 0x20 / 4;
    pub const TRIVIAL_HIGH: usize = 0x28 / 4;
    pub const TRIVIAL_LOW: usize = 0x2C / 4;
}

/// `SkillLine.dbc`'s localized name (`OFF_SKILL_LINE_NAME` 0x0C / 4).
pub const SKILL_LINE_NAME: usize = 0x0C / 4;

/// Every talent rank spell (`RankSet`).
struct RankSet(HashSet<u32>);
/// The ranks closed over `SkillLineAbility`'s superseded-by links (`LineSet`).
struct LineSet(HashSet<u32>);
/// The localized names of every talent rank spell (`NameSet`).
struct NameSet(HashSet<String>);

fn rank_set(db: &Databases) -> Option<std::sync::Arc<RankSet>> {
    db.derived(|db| {
        let talents = db.get("Talent")?;
        let set: HashSet<u32> = talents
            .rows()
            .flat_map(|r| {
                (0..talent_col::MAX_RANKS).map(move |j| r.u32(talent_col::SPELL_RANK + j))
            })
            .filter(|id| *id != 0)
            .collect();
        (!set.is_empty()).then_some(RankSet(set))
    })
}

/// `Talent::SpellSet::IsTalentRank`.
pub fn is_talent_rank(db: &Databases, spell_id: u32) -> bool {
    rank_set(db).is_some_and(|s| s.0.contains(&spell_id))
}

/// `Talent::SpellSet::IsTalentLine`: a rank, a higher rank its line links to, or a spell named
/// like a rank.
pub fn is_talent_line(db: &Databases, spell_id: u32) -> bool {
    let Some(ranks) = rank_set(db) else {
        return false;
    };
    let lines = db.derived(|db| {
        let mut next = HashMap::new();
        if let Some(sla) = db.get("SkillLineAbility") {
            for r in sla.rows() {
                let (spell, sup) = (r.u32(sla_col::SPELL), r.u32(sla_col::SUPERCEDED_BY));
                if spell > 0 && sup > 0 {
                    next.insert(spell, sup);
                }
            }
        }
        let mut set = HashSet::new();
        for &id in &ranks.0 {
            let mut cur = id;
            let mut hops = 0;
            while cur > 0 && hops < 32 && set.insert(cur) {
                cur = next.get(&cur).copied().unwrap_or(0);
                hops += 1;
            }
        }
        Some(LineSet(set))
    });
    if lines.is_some_and(|l| l.0.contains(&spell_id)) {
        return true;
    }
    let names = db.derived(|db| {
        let table = spells::table(db)?;
        Some(NameSet(
            ranks
                .0
                .iter()
                .filter_map(|id| table.row(*id))
                .map(|r| r.loc(col::NAME).to_string())
                .filter(|n| !n.is_empty())
                .collect(),
        ))
    });
    let Some(table) = spells::table(db) else {
        return false;
    };
    let name = table.row(spell_id).map(|r| r.loc(col::NAME).to_string());
    matches!((names, name), (Some(set), Some(n)) if !n.is_empty() && set.0.contains(&n))
}

/// The player's class and race bits, `1 << (byte - 1)`; zeros when unresolved.
pub fn player_bits(mirror: &Mirror) -> (u32, u32) {
    let Some(me) = mirror.me() else {
        return (0, 0);
    };
    let bit = |b: u8| if b == 0 { 0 } else { 1u32 << (b - 1) };
    (bit(me.class()), bit(me.race()))
}

/// `FindSkillIDForSpell`: the skill line of a spell's `SkillLineAbility` row, preferring the row
/// whose masks match the player, else the first.
pub fn skill_for_spell(db: &Databases, mirror: &Mirror, spell_id: u32) -> u32 {
    if spell_id == 0 {
        return 0;
    }
    let Some(sla) = db.get("SkillLineAbility") else {
        return 0;
    };
    let (class_bit, race_bit) = player_bits(mirror);
    let mut fallback = 0;
    for r in sla.rows() {
        if r.u32(sla_col::SPELL) != spell_id {
            continue;
        }
        let skill = r.u32(sla_col::SKILL);
        if skill == 0 {
            continue;
        }
        if fallback == 0 {
            fallback = skill;
        }
        if class_bit == 0 {
            return skill;
        }
        if r.u32(sla_col::EXCLUDE_CLASS) & class_bit != 0
            || r.u32(sla_col::EXCLUDE_RACE) & race_bit != 0
        {
            continue;
        }
        let class_mask = r.u32(sla_col::CLASS_MASK);
        let race_mask = r.u32(sla_col::RACE_MASK);
        if (class_mask == 0 || class_mask & class_bit != 0)
            && (race_mask == 0 || race_mask & race_bit != 0)
        {
            return skill;
        }
    }
    fallback
}

/// One learned skill line on the player, `PLAYER_SKILL_INFO`: `(cur, max, bonus)`.
pub fn skill_rank(mirror: &Mirror, skill_line: u32) -> Option<(u16, u16, u16)> {
    use crate::mirror::field::PLAYER_SKILL_INFO_1_1 as BASE;
    let me = mirror.me()?;
    (0..128).find_map(|i| {
        let at = BASE + i * 3;
        (skill_line != 0 && u32::from(me.half(at, false)) == skill_line).then(|| {
            (
                me.half(at + 1, false),
                me.half(at + 1, true),
                me.half(at + 2, true),
            )
        })
    })
}
