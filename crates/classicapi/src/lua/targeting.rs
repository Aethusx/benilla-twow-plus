//! `target/Nearest.cpp`: `TargetNearest`, `TargetNearestEnemyPlayer`,
//! `TargetNearestFriendPlayer`, `TargetDirectionEnemy` and `TargetDirectionFriend`.
//!
//! Built, as the DLL is, on the nearest-unit scan's own per-candidate filter (`0x493e40`, through
//! benilla's `ExtWorld::tab_valid`): enemy is mode 1, friend mode 2, `TargetNearest` either, the
//! `*Player` forms only players. The cycles take every streamed unit passing the filter, nearest
//! first; presses within a second that find the target still where the last one put it step
//! through that snapshot (backwards with a true argument), a later one starts over at the nearest.
//! The direction forms take the nearest unit within `cone` radians (default π/2) of `facing`.
//!
//! The natives queue the ask; the frame resolves it against the world and selects through
//! benilla's selection, so the target changes the frame after the call, as a crate's casts do.

use std::time::{Duration, Instant};

use mlua::Value;

use crate::lua::{is_number, to_number, truthy, Api};
use crate::mirror::Mirror;
use crate::Ca;

const CYCLE_WINDOW: Duration = Duration::from_secs(1);
const DEFAULT_CONE: f32 = std::f32::consts::FRAC_PI_2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Any,
    EnemyPlayer,
    FriendPlayer,
}

#[derive(Clone, Copy, Debug)]
pub enum Ask {
    Cycle {
        filter: Filter,
        reverse: bool,
    },
    Direction {
        hostile: bool,
        facing: f32,
        cone: f32,
    },
}

#[derive(Clone, Debug, Default)]
struct Cycle {
    snapshot: Vec<u64>,
    index: usize,
    last_press: Option<Instant>,
    last_set: u64,
}

#[derive(Default)]
pub struct Targeting {
    pub(crate) asks: Vec<Ask>,
    cycles: [Cycle; 3],
}

/// The streamed units passing `pass`, with their squared distance and horizontal offset.
fn candidates(m: &Mirror, pass: impl Fn(u64) -> bool) -> Vec<(u64, f32, [f32; 2])> {
    let Some(me) = m.place(m.player) else {
        return Vec::new();
    };
    let mut out: Vec<(u64, f32, [f32; 2])> = m
        .objects
        .iter()
        .filter(|(g, f)| **g != m.player && f.is(crate::mirror::typemask::UNIT))
        .filter_map(|(g, _)| {
            let p = m.place(*g)?;
            let d = [
                p.pos[0] - me.pos[0],
                p.pos[1] - me.pos[1],
                p.pos[2] - me.pos[2],
            ];
            Some((*g, d[0] * d[0] + d[1] * d[1] + d[2] * d[2], [d[0], d[1]]))
        })
        .filter(|(g, _, _)| pass(*g))
        .collect();
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

impl Targeting {
    /// Resolve this frame's asks; the guid to select, if any.
    /// `valid(guid, hostile)` is the scan's filter.
    pub fn resolve(
        &mut self,
        m: &Mirror,
        valid: &dyn Fn(u64, bool) -> bool,
        now: Instant,
    ) -> Option<u64> {
        let mut pick = None;
        for ask in std::mem::take(&mut self.asks) {
            let current = pick.unwrap_or(m.target);
            pick = match ask {
                Ask::Cycle { filter, reverse } => {
                    self.cycle(m, valid, filter, reverse, current, now).or(pick)
                }
                Ask::Direction {
                    hostile,
                    facing,
                    cone,
                } => {
                    let half = (cone * 0.5).cos();
                    let dir = [facing.cos(), facing.sin()];
                    candidates(m, |g| valid(g, hostile))
                        .into_iter()
                        .find(|(_, _, h)| {
                            let len = (h[0] * h[0] + h[1] * h[1]).sqrt();
                            len > 0.01 && (dir[0] * h[0] + dir[1] * h[1]) / len >= half
                        })
                        .map(|c| c.0)
                        .or(pick)
                }
            };
        }
        pick
    }

    fn cycle(
        &mut self,
        m: &Mirror,
        valid: &dyn Fn(u64, bool) -> bool,
        filter: Filter,
        reverse: bool,
        current: u64,
        now: Instant,
    ) -> Option<u64> {
        let st = &mut self.cycles[filter as usize];
        let carry_on = !st.snapshot.is_empty()
            && st
                .last_press
                .is_some_and(|at| now.duration_since(at) <= CYCLE_WINDOW)
            && current == st.last_set;
        if carry_on {
            let n = st.snapshot.len();
            st.index = if reverse {
                (st.index + n - 1) % n
            } else {
                (st.index + 1) % n
            };
        } else {
            let player = |g: u64| crate::guid::classify(g) == crate::guid::Kind::Player;
            let list = candidates(m, |g| match filter {
                Filter::Any => valid(g, true) || valid(g, false),
                Filter::EnemyPlayer => player(g) && valid(g, true),
                Filter::FriendPlayer => player(g) && valid(g, false),
            });
            if list.is_empty() {
                st.snapshot.clear();
                return None;
            }
            st.snapshot = list.into_iter().map(|c| c.0).collect();
            st.index = 0;
        }
        let guid = st.snapshot[st.index];
        st.last_set = guid;
        st.last_press = Some(now);
        Some(guid)
    }
}

fn push(ca: &Ca, ask: Ask) {
    ca.lock().targeting.asks.push(ask);
}

pub(super) fn install(api: &Api) -> mlua::Result<()> {
    for (name, filter) in [
        ("TargetNearest", Filter::Any),
        ("TargetNearestEnemyPlayer", Filter::EnemyPlayer),
        ("TargetNearestFriendPlayer", Filter::FriendPlayer),
    ] {
        let c = api.ca.clone();
        api.global(name, move |_, reverse: Value| {
            push(
                &c,
                Ask::Cycle {
                    filter,
                    reverse: truthy(&reverse),
                },
            );
            Ok(())
        })?;
    }
    for (name, hostile) in [
        ("TargetDirectionEnemy", true),
        ("TargetDirectionFriend", false),
    ] {
        let c = api.ca.clone();
        let usage = format!("Usage: {name}(facing [, coneAngle])");
        api.global(name, move |_, (facing, cone): (Value, Value)| {
            if !is_number(&facing) {
                return Err(mlua::Error::runtime(usage.clone()));
            }
            let cone = if is_number(&cone) {
                to_number(&cone) as f32
            } else {
                DEFAULT_CONE
            };
            push(
                &c,
                Ask::Direction {
                    hostile,
                    facing: to_number(&facing) as f32,
                    cone: if cone > 0.0 { cone } else { DEFAULT_CONE },
                },
            );
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::{Fields, Place};

    const ME: u64 = 0x10;
    const NEAR: u64 = 0xF130_0000_0100_0001;
    const FAR: u64 = 0xF130_0000_0100_0002;
    const PLAYER: u64 = 0x20;

    fn mirror() -> Mirror {
        let mut m = Mirror::default();
        m.player = ME;
        for (g, x, y) in [
            (ME, 0.0, 0.0),
            (NEAR, 5.0, 0.0),
            (FAR, 0.0, 20.0),
            (PLAYER, -8.0, 0.0),
        ] {
            let mut cells = vec![0u32; 200];
            cells[2] = crate::mirror::typemask::UNIT;
            m.objects.insert(g, Fields::from_vec(cells));
            m.places.insert(
                g,
                Place {
                    pos: [x, y, 0.0],
                    facing: 0.0,
                },
            );
        }
        m
    }

    #[test]
    fn a_cycle_steps_its_snapshot_and_a_cone_picks_in_front() {
        let mut m = mirror();
        let hostile_npcs = |g: u64, hostile: bool| hostile && g != PLAYER;
        let mut t = Targeting::default();
        let t0 = Instant::now();
        let press = |t: &mut Targeting, m: &Mirror, reverse: bool, at: Instant| {
            t.asks.push(Ask::Cycle {
                filter: Filter::Any,
                reverse,
            });
            t.resolve(m, &hostile_npcs, at)
        };
        assert_eq!(press(&mut t, &m, false, t0), Some(NEAR));
        m.target = NEAR;
        assert_eq!(press(&mut t, &m, false, t0), Some(FAR));
        m.target = FAR;
        assert_eq!(press(&mut t, &m, true, t0), Some(NEAR));
        // A late press starts over at the nearest.
        m.target = NEAR;
        assert_eq!(
            press(&mut t, &m, false, t0 + Duration::from_secs(5)),
            Some(NEAR)
        );
        // Facing +y (pi/2): only FAR is in the cone.
        t.asks.push(Ask::Direction {
            hostile: true,
            facing: std::f32::consts::FRAC_PI_2,
            cone: DEFAULT_CONE,
        });
        assert_eq!(t.resolve(&m, &hostile_npcs, t0), Some(FAR));
        // Players only, friendly side: the one player.
        t.asks.push(Ask::Cycle {
            filter: Filter::FriendPlayer,
            reverse: false,
        });
        assert_eq!(t.resolve(&m, &|g, h| !h && g == PLAYER, t0), Some(PLAYER));
    }
}
