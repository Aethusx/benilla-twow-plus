//! `UnitXP("target", ...)`, ported from `targeting.cpp`: the candidate filter every mode shares,
//! then the pick. The candidates arrive already measured; this module only filters and chooses.

/// One unit as the targeting modes weigh it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Candidate {
    pub guid: u64,
    pub is_player: bool,
    /// A creature whose master is a player (`UNIT_FLAG_PLAYER_CONTROLLED`): a pet, never picked.
    pub player_controlled: bool,
    pub can_attack: bool,
    pub dead: bool,
    pub in_combat: bool,
    /// `CreatureType.dbc` 8, a critter.
    pub critter: bool,
    /// Rank 3, a world boss.
    pub world_boss: bool,
    /// The Gaussian distance from us.
    pub distance: f32,
    /// The ranged-meter distance from us.
    pub ranged: f32,
    /// The melee-meter distance from us.
    pub melee: f32,
    pub health: u32,
    pub in_cone: bool,
    pub in_sight: bool,
    /// Its raid mark, 1-8, 0 for none.
    pub mark: u8,
}

/// The modes' tunables.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    pub cone: f32,
    pub far_range: f32,
    pub in_combat_filter: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            cone: 2.2,
            far_range: 41.0,
            in_combat_filter: true,
        }
    }
}

/// The filter all modes share: an attackable, living, non-pet unit in the camera cone, no
/// out-of-combat critter, and in combat when we are and the filter is on.
fn eligible(c: &Candidate, tuning: &Tuning, self_in_combat: bool) -> bool {
    (c.is_player || !c.player_controlled)
        && c.can_attack
        && !c.dead
        && (c.in_combat || !c.critter)
        && c.in_cone
        && !(tuning.in_combat_filter && !c.is_player && self_in_combat && !c.in_combat)
}

/// The next by guid after `current`, wrapping to the first.
fn next_by(current: u64, mut list: Vec<Candidate>, key: impl Fn(&Candidate) -> i64) -> Option<u64> {
    list.sort_by_key(|c| key(c));
    let after = list
        .iter()
        .position(|c| c.guid == current)
        .and_then(|i| list.get(i + 1));
    after.or(list.first()).map(|c| c.guid)
}

/// What a mode picks, or `None` when nothing qualifies.
pub fn nearest(cands: &[Candidate], limit: f32, tuning: &Tuning, in_combat: bool) -> Option<u64> {
    cands
        .iter()
        .filter(|c| eligible(c, tuning, in_combat) && c.in_sight && c.distance <= limit)
        .min_by(|a, b| a.distance.total_cmp(&b.distance))
        .map(|c| c.guid)
}

pub fn most_hp(cands: &[Candidate], tuning: &Tuning, in_combat: bool) -> Option<u64> {
    cands
        .iter()
        .filter(|c| eligible(c, tuning, in_combat) && c.in_sight && c.distance <= tuning.far_range)
        .max_by_key(|c| c.health)
        .map(|c| c.guid)
}

/// `targetWorldBoss`: the nearest world boss, or the next one after the current target.
pub fn world_boss(
    cands: &[Candidate],
    current: Option<u64>,
    tuning: &Tuning,
    in_combat: bool,
) -> Option<u64> {
    let mut bosses: Vec<Candidate> = cands
        .iter()
        .filter(|c| eligible(c, tuning, in_combat) && c.world_boss)
        .copied()
        .collect();
    bosses.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    match current {
        Some(cur) if bosses.iter().any(|b| b.guid == cur) => {
            next_by(cur, bosses, |c| c.guid as i64)
        }
        _ => bosses.first().map(|c| c.guid),
    }
}

/// `targetEnemyInCycle`: every eligible unit in range and sight, stepped by guid.
pub fn in_cycle(
    cands: &[Candidate],
    current: Option<u64>,
    forward: bool,
    tuning: &Tuning,
    in_combat: bool,
) -> Option<u64> {
    let Some(current) = current else {
        return nearest(cands, tuning.far_range, tuning, in_combat);
    };
    let list: Vec<Candidate> = cands
        .iter()
        .filter(|c| eligible(c, tuning, in_combat) && c.in_sight && c.ranged <= tuning.far_range)
        .copied()
        .collect();
    next_by(current, list, |c| {
        if forward {
            c.guid as i64
        } else {
            -(c.guid as i64)
        }
    })
}

/// `targetEnemyConsideringDistance`: step within the nearest occupied band, melee (8 yd), charge
/// (25 yd, the 3 nearest) or far (the 5 nearest).
pub fn considering_distance(
    cands: &[Candidate],
    current: Option<u64>,
    forward: bool,
    tuning: &Tuning,
    in_combat: bool,
) -> Option<u64> {
    let Some(current) = current else {
        return nearest(cands, tuning.far_range, tuning, in_combat);
    };
    let (mut melee, mut charge, mut far) = (Vec::new(), Vec::new(), Vec::new());
    for c in cands
        .iter()
        .filter(|c| eligible(c, tuning, in_combat) && c.ranged <= tuning.far_range)
    {
        if c.ranged <= 8.0 {
            if c.in_sight || c.melee < 5.0 {
                melee.push(*c);
            }
        } else if c.ranged <= 25.0 && c.in_sight {
            charge.push(*c);
        } else if c.ranged < tuning.far_range && c.in_sight {
            far.push(*c);
        }
    }
    let key = |c: &Candidate| {
        if forward {
            c.guid as i64
        } else {
            -(c.guid as i64)
        }
    };
    let keep = |mut band: Vec<Candidate>, limit: usize| {
        band.sort_by(|a, b| a.ranged.total_cmp(&b.ranged));
        band.truncate(limit);
        band
    };
    if !melee.is_empty() {
        return next_by(current, melee, key);
    }
    if !charge.is_empty() {
        return next_by(current, keep(charge, 3), key);
    }
    if !far.is_empty() {
        return next_by(current, keep(far, 5), key);
    }
    None
}

/// The mark priority a string like `"87"` gives, skull first by default.
pub fn mark_priority(text: &str) -> Vec<u8> {
    let p: Vec<u8> = text
        .bytes()
        .filter(|b| (b'1'..=b'8').contains(b))
        .take(8)
        .map(|b| b - b'0')
        .collect();
    if p.is_empty() {
        (1..=8).rev().collect()
    } else {
        p
    }
}

/// `targetMarkedEnemyInCycle`: marked units in priority order, stepped from the current target.
pub fn marked_in_cycle(
    cands: &[Candidate],
    current: Option<u64>,
    forward: bool,
    priority: &[u8],
    tuning: &Tuning,
    in_combat: bool,
) -> Option<u64> {
    let rank = |c: &Candidate| priority.iter().position(|m| *m == c.mark);
    let list: Vec<Candidate> = cands
        .iter()
        .filter(|c| c.mark > 0 && eligible(c, tuning, in_combat) && rank(c).is_some())
        .copied()
        .collect();
    if list.is_empty() {
        return None;
    }
    let key = |c: &Candidate| {
        let r = rank(c).unwrap_or(usize::MAX) as i64;
        if forward {
            r
        } else {
            -r
        }
    };
    next_by(current.unwrap_or(0), list, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mob(guid: u64, distance: f32) -> Candidate {
        Candidate {
            guid,
            can_attack: true,
            in_cone: true,
            in_sight: true,
            distance,
            ranged: distance,
            melee: distance,
            health: 100,
            ..Default::default()
        }
    }

    #[test]
    fn nearest_skips_what_the_filter_refuses() {
        let t = Tuning::default();
        let pet = Candidate {
            player_controlled: true,
            ..mob(1, 2.0)
        };
        let critter = Candidate {
            critter: true,
            ..mob(2, 3.0)
        };
        let blind = Candidate {
            in_sight: false,
            ..mob(3, 4.0)
        };
        let cands = [pet, critter, blind, mob(4, 9.0), mob(5, 6.0)];
        assert_eq!(nearest(&cands, 41.0, &t, false), Some(5));
        // In combat, an idle mob drops out under the filter.
        assert_eq!(nearest(&cands, 41.0, &t, true), None);
    }

    #[test]
    fn the_cycle_steps_by_guid_and_wraps() {
        let t = Tuning::default();
        let cands = [mob(30, 5.0), mob(10, 5.0), mob(20, 5.0)];
        assert_eq!(in_cycle(&cands, Some(10), true, &t, false), Some(20));
        assert_eq!(in_cycle(&cands, Some(30), true, &t, false), Some(10));
        assert_eq!(in_cycle(&cands, Some(10), false, &t, false), Some(30));
        assert_eq!(
            in_cycle(&cands, None, true, &t, false),
            Some(30),
            "none: the nearest, first"
        );
    }

    #[test]
    fn distance_bands_prefer_melee() {
        let t = Tuning::default();
        let cands = [mob(1, 20.0), mob(2, 6.0), mob(3, 7.0)];
        assert_eq!(
            considering_distance(&cands, Some(2), true, &t, false),
            Some(3)
        );
        let far = [mob(1, 20.0), mob(2, 21.0)];
        assert_eq!(
            considering_distance(&far, Some(1), true, &t, false),
            Some(2)
        );
    }

    #[test]
    fn marks_follow_the_priority_string() {
        let t = Tuning::default();
        let skull = Candidate {
            mark: 8,
            ..mob(1, 5.0)
        };
        let cross = Candidate {
            mark: 7,
            ..mob(2, 5.0)
        };
        let star = Candidate {
            mark: 1,
            ..mob(3, 5.0)
        };
        let cands = [star, cross, skull];
        let p = mark_priority("");
        assert_eq!(marked_in_cycle(&cands, None, true, &p, &t, false), Some(1));
        assert_eq!(
            marked_in_cycle(&cands, Some(1), true, &p, &t, false),
            Some(2)
        );
        let only = mark_priority("71");
        assert_eq!(
            marked_in_cycle(&cands, Some(2), true, &only, &t, false),
            Some(3)
        );
    }
}
