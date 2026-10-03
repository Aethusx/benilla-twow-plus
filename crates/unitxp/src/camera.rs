//! The camera options (`editCamera.cpp`): a vertical and a sideways displacement of the eye, a
//! pitch added to the view, and the view following the target. Applied after benilla seats its
//! camera each frame, so benilla's own boom, collision and zoom stay untouched underneath.

use benilla_world::collision::WorldCollision;
use benilla_world::view::WorldCamera;
use bevy::prelude::*;

use crate::Ux;

/// `cameraHeight`/`cameraVerticalDisplacement`, `cameraHorizontalDisplacement`, `cameraPitch`
/// and `cameraFollowTarget`; `cameraOrganicSmooth` and `cameraPinHeight` are kept for their
/// getters only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Offsets {
    pub vertical: f32,
    pub horizontal: f32,
    pub pitch: f32,
    pub follow_target: bool,
    pub organic_smooth: bool,
    pub pin_height: bool,
}

impl Default for Offsets {
    fn default() -> Self {
        Self {
            vertical: 0.0,
            horizontal: 0.0,
            pitch: 0.0,
            follow_target: false,
            organic_smooth: true,
            pin_height: false,
        }
    }
}

impl Offsets {
    fn active(&self) -> bool {
        self.vertical != 0.0 || self.horizontal != 0.0 || self.pitch != 0.0 || self.follow_target
    }
}

/// The eye `base` moved by the offsets, short of any wall the move would cross, and its view.
/// `None` in first person, where the DLL leaves the camera alone.
pub fn offset_pose(
    base: Transform,
    offsets: &Offsets,
    player: Vec3,
    look_at: Option<Vec3>,
    blocked: impl Fn(Vec3, Vec3) -> Option<f32>,
) -> Option<Transform> {
    let flat = Vec3::new(
        player.x - base.translation.x,
        0.0,
        player.z - base.translation.z,
    );
    if flat.length() < 0.5 {
        return None;
    }
    let right = Vec3::new(-flat.z, 0.0, flat.x).normalize();
    let wanted = base.translation + Vec3::Y * offsets.vertical + right * offsets.horizontal;
    // Keep 0.2 yd off the wall, as `keepDistanceFromWall`.
    let translation = match blocked(base.translation, wanted) {
        Some(frac) => {
            let span = wanted - base.translation;
            let keep = (span.length() * frac - 0.2).max(0.0);
            base.translation + span.normalize_or_zero() * keep
        }
        None => wanted,
    };
    let mut forward = match look_at {
        Some(target) if offsets.follow_target => (target - translation).normalize_or_zero(),
        _ => *base.forward(),
    };
    forward.y += offsets.pitch;
    let forward = forward.normalize_or_zero();
    if forward == Vec3::ZERO {
        return None;
    }
    Some(Transform::from_translation(translation).looking_to(forward, Vec3::Y))
}

/// Re-apply the offsets on top of the pose benilla seated this frame. A frame where benilla left
/// the transform as this system wrote it reuses the remembered base, so offsets never compound.
pub fn apply_offsets(
    ux: Res<Ux>,
    mut cameras: Query<&mut Transform, With<WorldCamera>>,
    collision: WorldCollision,
    mut last: Local<Option<(Transform, Transform)>>,
) {
    let Ok(mut cam) = cameras.single_mut() else {
        *last = None;
        return;
    };
    let base = match *last {
        Some((base, applied)) if applied == *cam => base,
        _ => *cam,
    };
    let (offsets, player, look_at) = {
        let st = ux.lock();
        let player = st.units.get(&st.player).map(|u| u.body.pos);
        let look_at = st
            .units
            .get(&st.selection)
            .map(|u| u.body.pos + Vec3::Y * u.body.height * 0.5);
        (st.settings.camera, player, look_at)
    };
    let posed = match (offsets.active(), player) {
        (true, Some(player)) => offset_pose(base, &offsets, player, look_at, |a, b| {
            collision.sight(a, b)
        }),
        _ => None,
    };
    match posed {
        Some(pose) => {
            *cam = pose;
            *last = Some((base, pose));
        }
        None => {
            if last.is_some() && *cam != base {
                *cam = base;
            }
            *last = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Transform {
        // Ten yards behind a player at the origin, looking at them along -Z.
        Transform::from_xyz(0.0, 2.0, 10.0).looking_at(Vec3::new(0.0, 2.0, 0.0), Vec3::Y)
    }

    #[test]
    fn displacements_move_the_eye_up_and_right() {
        let o = Offsets {
            vertical: 1.0,
            horizontal: 2.0,
            ..Default::default()
        };
        let pose = offset_pose(base(), &o, Vec3::ZERO, None, |_, _| None).unwrap();
        assert!((pose.translation - Vec3::new(2.0, 3.0, 10.0)).length() < 1e-4);
        assert!(
            (*pose.forward() - Vec3::NEG_Z).length() < 1e-4,
            "the view is kept"
        );
    }

    #[test]
    fn a_wall_stops_the_move_short() {
        let o = Offsets {
            vertical: 2.0,
            ..Default::default()
        };
        let pose = offset_pose(base(), &o, Vec3::ZERO, None, |_, _| Some(0.5)).unwrap();
        assert!((pose.translation.y - 2.8).abs() < 1e-4);
    }

    #[test]
    fn first_person_is_left_alone() {
        let o = Offsets {
            vertical: 1.0,
            ..Default::default()
        };
        let eye = Transform::from_xyz(0.1, 2.0, 0.0);
        assert!(offset_pose(eye, &o, Vec3::ZERO, None, |_, _| None).is_none());
    }
}
