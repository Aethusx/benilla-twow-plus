//! UnitXP's measures, ported from `distanceBetween.cpp` and `inSight.cpp`: the five distance
//! meters, the behind test, the camera cone and the line-of-sight heights. Pure functions over
//! Bevy space (+Y up, 1 unit 1 yard); a unit faces `rotation * -Z`.

use bevy::math::{Quat, Vec2, Vec3};

/// `distanceMeters`: which reach a distance subtracts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Meter {
    /// The raw distance between the two positions.
    Gaussian,
    /// Targeted spells: minus both combat reaches. Lua's default.
    Ranged,
    /// Melee auto attack, vmangos `Unit::CanReachWithMeleeAutoAttack`: flat, and only within 6 yd
    /// of height.
    MeleeAutoAttack,
    /// Area spells: minus one creature's reach, none for a player.
    Aoe,
    /// Chain jumps: minus both bounding radii.
    Chains,
}

impl Meter {
    /// The Lua name of a meter; anything else is [`Meter::Ranged`].
    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("meleeAutoAttack") => Meter::MeleeAutoAttack,
            Some("AoE") => Meter::Aoe,
            Some("chains") => Meter::Chains,
            Some("Gaussian") => Meter::Gaussian,
            _ => Meter::Ranged,
        }
    }
}

/// What the measures read of one unit.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Body {
    pub pos: Vec3,
    pub rotation: Quat,
    pub combat_reach: f32,
    pub bounding_radius: f32,
    pub is_player: bool,
    /// The eye height the sight lines start from.
    pub height: f32,
}

/// `UnitXP_distanceBetween`, never below 0.
pub fn distance(a: &Body, b: &Body, meter: Meter) -> f32 {
    let raw = a.pos.distance(b.pos);
    let reach_a = a.combat_reach.max(0.0);
    let reach_b = b.combat_reach.max(0.0);
    match meter {
        Meter::MeleeAutoAttack if (a.pos.y - b.pos.y).abs() < 6.0 => {
            let total = (reach_a.max(1.5) + reach_b.max(1.5) + 4.0 / 3.0).max(5.0);
            let flat = Vec2::new(a.pos.x - b.pos.x, a.pos.z - b.pos.z).length();
            (flat - total).max(0.0)
        }
        Meter::Aoe => {
            // Only a creature's reach counts; the second one wins, as the reference's order.
            let mut reach = 0.0;
            if !a.is_player {
                reach = reach_a;
            }
            if !b.is_player {
                reach = reach_b;
            }
            (raw - reach).max(0.0)
        }
        Meter::Chains => (raw - a.bounding_radius.max(0.0) - b.bounding_radius.max(0.0)).max(0.0),
        Meter::Ranged => (raw - reach_a - reach_b).max(0.0),
        // The melee meter falls back to the raw distance past 6 yd of height.
        Meter::Gaussian | Meter::MeleeAutoAttack => raw,
    }
}

/// The angle between two vectors, in radians.
fn angle(a: Vec3, b: Vec3) -> f32 {
    let (la, lb) = (a.length(), b.length());
    if la < f32::EPSILON || lb < f32::EPSILON {
        return 0.0;
    }
    (a.dot(b) / (la * lb)).clamp(-1.0, 1.0).acos()
}

/// `UnitXP_behind`: `me` stands behind `mob` when the flat angle between the mob's forward and
/// the mob-to-me line is at least `threshold`. `facing` overrides the mob's own forward: a mob in
/// melee faces its target, whatever its stale facing says.
pub fn behind(me: &Body, mob: &Body, facing: Option<Vec3>, threshold: f32) -> bool {
    let forward = facing.unwrap_or(mob.rotation * Vec3::NEG_Z);
    let forward = Vec3::new(forward.x, 0.0, forward.z);
    let to_me = Vec3::new(me.pos.x - mob.pos.x, 0.0, me.pos.z - mob.pos.z);
    angle(forward, to_me) >= threshold
}

/// `inViewingFrustum`: within the 90° field of view narrowed by `cone` (2 is the field itself).
pub fn in_cone(camera: Vec3, forward: Vec3, pos: Vec3, cone: f32) -> bool {
    // The DLL's literal 1.5708, a quarter turn.
    const FOV: f32 = std::f32::consts::FRAC_PI_2;
    angle(pos - camera, forward) <= FOV / cone
}

/// `vanilla1121_unitInLineOfSight`'s two segments: both eyes at the shorter unit's height, and if
/// that is blocked, eye to eye. `blocked` traces one segment.
pub fn in_sight(a: &Body, b: &Body, mut blocked: impl FnMut(Vec3, Vec3) -> bool) -> bool {
    let (low, high) = if a.height > b.height { (b, a) } else { (a, b) };
    let lift = Vec3::Y * low.height;
    if !blocked(low.pos + lift, high.pos + lift) {
        return true;
    }
    if low.height == high.height {
        return false;
    }
    !blocked(low.pos + lift, high.pos + Vec3::Y * high.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32, z: f32) -> Body {
        Body {
            pos: Vec3::new(x, y, z),
            combat_reach: 1.5,
            bounding_radius: 0.5,
            height: 2.0,
            ..Default::default()
        }
    }

    #[test]
    fn the_meters_subtract_what_the_reference_does() {
        let a = Body {
            is_player: true,
            ..at(0.0, 0.0, 0.0)
        };
        let b = at(10.0, 0.0, 0.0);
        assert_eq!(distance(&a, &b, Meter::Gaussian), 10.0);
        assert_eq!(distance(&a, &b, Meter::Ranged), 7.0);
        assert_eq!(
            distance(&a, &b, Meter::Aoe),
            8.5,
            "only the creature's reach"
        );
        assert_eq!(distance(&a, &b, Meter::Chains), 9.0);
        // 1.5 + 1.5 + 4/3 is under the 5 yd floor.
        assert_eq!(distance(&a, &b, Meter::MeleeAutoAttack), 5.0);
        let high = at(10.0, 7.0, 0.0);
        assert!((distance(&a, &high, Meter::MeleeAutoAttack) - 149f32.sqrt()).abs() < 1e-4);
    }

    #[test]
    fn behind_reads_the_mobs_forward() {
        // Facing 0 is `-Z` in Bevy.
        let mob = at(0.0, 0.0, 0.0);
        let front = at(0.0, 0.0, -3.0);
        let back = at(0.0, 0.0, 3.0);
        let half = std::f32::consts::FRAC_PI_2;
        assert!(!behind(&front, &mob, None, half));
        assert!(behind(&back, &mob, None, half));
        // Facing its target at `+Z` turns it around.
        assert!(behind(&front, &mob, Some(Vec3::Z), half));
    }

    #[test]
    fn the_cone_narrows_with_its_factor() {
        let (cam, fwd) = (Vec3::ZERO, Vec3::NEG_Z);
        let off = Vec3::new(1.0, 0.0, -1.0); // 45° off-axis
        assert!(in_cone(cam, fwd, off, 2.0));
        assert!(!in_cone(cam, fwd, off, 2.2));
        assert!(!in_cone(cam, fwd, Vec3::Z, 2.0));
    }

    #[test]
    fn sight_tries_the_low_line_then_eye_to_eye() {
        let short = at(0.0, 0.0, 0.0);
        let tall = Body {
            height: 5.0,
            ..at(10.0, 0.0, 0.0)
        };
        // A wall up to 3 yd blocks the 2 yd line but not the line rising to 5 yd.
        let wall = |from: Vec3, to: Vec3| from.y.max(to.y) < 3.0;
        assert!(in_sight(&short, &tall, wall));
        let mut calls = 0;
        assert!(!in_sight(&short, &at(10.0, 0.0, 0.0), |_, _| {
            calls += 1;
            true
        }));
        assert_eq!(calls, 1, "equal heights take one line");
    }
}
