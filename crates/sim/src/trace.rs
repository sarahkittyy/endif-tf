//! Swept-AABB tracing against axis-aligned box brushes, mirroring the engine's
//! `CM_ClipBoxToBrush` (including `DIST_EPSILON` and the startsolid/allsolid semantics).

use crate::consts::DIST_EPSILON;
use crate::math::Vec3;
use serde::{Deserialize, Serialize};

/// What a trace hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HitEnt {
    World,
    Player(u8),
    /// A Fruit Ninja soldier, by id (see `fruit::Target`).
    Target(u32),
}

/// An axis-aligned box in world space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub mins: Vec3,
    pub maxs: Vec3,
}

impl Aabb {
    pub const fn new(mins: Vec3, maxs: Vec3) -> Self {
        Aabb { mins, maxs }
    }

    /// `CCollisionProperty::CalcNearestPoint`: closest point on the box to `p`.
    pub fn nearest_point(&self, p: Vec3) -> Vec3 {
        Vec3::new(
            clampf(p.x, self.mins.x, self.maxs.x),
            clampf(p.y, self.mins.y, self.maxs.y),
            clampf(p.z, self.mins.z, self.maxs.z),
        )
    }

    pub fn center(&self) -> Vec3 {
        (self.mins + self.maxs) * 0.5
    }

    pub fn size(&self) -> Vec3 {
        self.maxs - self.mins
    }

    /// Whether the boxes overlap or touch (a hull resting against another counts as in it).
    pub fn touches(&self, o: &Aabb) -> bool {
        self.mins.x <= o.maxs.x && self.maxs.x >= o.mins.x
            && self.mins.y <= o.maxs.y && self.maxs.y >= o.mins.y
            && self.mins.z <= o.maxs.z && self.maxs.z >= o.mins.z
    }
}

#[inline]
fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// Result of a trace (`trace_t`).
#[derive(Clone, Copy, Debug)]
pub struct Trace {
    pub fraction: f32,
    pub endpos: Vec3,
    /// Plane normal of the surface hit; zero when nothing was hit.
    pub normal: Vec3,
    pub startsolid: bool,
    pub allsolid: bool,
    pub ent: Option<HitEnt>,
}

impl Trace {
    fn none(end: Vec3) -> Trace {
        Trace {
            fraction: 1.0,
            endpos: end,
            normal: Vec3::ZERO,
            startsolid: false,
            allsolid: false,
            ent: None,
        }
    }

    pub fn did_hit(&self) -> bool {
        self.fraction < 1.0 || self.startsolid
    }
}

/// Solid geometry a trace can collide with.
#[derive(Clone, Copy)]
pub struct TraceEnv<'a> {
    pub world: &'a [Aabb],
    /// Other solid players as `(player index, absolute hull)`.
    pub players: &'a [(u8, Aabb)],
    /// Fruit Ninja soldiers as `(target id, absolute hull)`; only rockets see them.
    pub targets: &'a [(u32, Aabb)],
}

impl<'a> TraceEnv<'a> {
    pub fn world_only(world: &'a [Aabb]) -> Self {
        TraceEnv { world, players: &[], targets: &[] }
    }

    pub fn with_players(world: &'a [Aabb], players: &'a [(u8, Aabb)]) -> Self {
        TraceEnv { world, players, targets: &[] }
    }
}

struct Plane {
    normal: Vec3,
    dist: f32,
}

fn box_planes(b: &Aabb) -> [Plane; 6] {
    [
        Plane { normal: Vec3::new(1.0, 0.0, 0.0), dist: b.maxs.x },
        Plane { normal: Vec3::new(-1.0, 0.0, 0.0), dist: -b.mins.x },
        Plane { normal: Vec3::new(0.0, 1.0, 0.0), dist: b.maxs.y },
        Plane { normal: Vec3::new(0.0, -1.0, 0.0), dist: -b.mins.y },
        Plane { normal: Vec3::new(0.0, 0.0, 1.0), dist: b.maxs.z },
        Plane { normal: Vec3::new(0.0, 0.0, -1.0), dist: -b.mins.z },
    ]
}

/// Port of `CM_ClipBoxToBrush` for a single box brush.
fn clip_box_to_brush(
    brush: &Aabb,
    ent: HitEnt,
    mins: Vec3,
    maxs: Vec3,
    p1: Vec3,
    p2: Vec3,
    trace: &mut Trace,
) {
    let mut enterfrac = -1.0f32;
    let mut leavefrac = 1.0f32;
    let mut clip_normal: Option<Vec3> = None;
    let mut getout = false;
    let mut startout = false;

    for plane in box_planes(brush).iter() {
        // push the plane out appropriately for mins/maxs
        let ofs = Vec3::new(
            if plane.normal.x < 0.0 { maxs.x } else { mins.x },
            if plane.normal.y < 0.0 { maxs.y } else { mins.y },
            if plane.normal.z < 0.0 { maxs.z } else { mins.z },
        );
        let dist = plane.dist - ofs.dot(plane.normal);

        let d1 = p1.dot(plane.normal) - dist;
        let d2 = p2.dot(plane.normal) - dist;

        if d2 > 0.0 {
            getout = true; // endpoint is not in solid
        }
        if d1 > 0.0 {
            startout = true;
        }

        // if completely in front of face, no intersection
        if d1 > 0.0 && d2 >= d1 {
            return;
        }
        if d1 <= 0.0 && d2 <= 0.0 {
            continue;
        }

        // crosses face
        if d1 > d2 {
            // enter
            let f = (d1 - DIST_EPSILON) / (d1 - d2);
            if f > enterfrac {
                enterfrac = f;
                clip_normal = Some(plane.normal);
            }
        } else {
            // leave
            let f = (d1 + DIST_EPSILON) / (d1 - d2);
            if f < leavefrac {
                leavefrac = f;
            }
        }
    }

    if !startout {
        // original point was inside brush
        trace.startsolid = true;
        trace.ent = Some(ent);
        if !getout {
            trace.allsolid = true;
        }
        return;
    }

    if enterfrac < leavefrac && enterfrac > -1.0 && enterfrac < trace.fraction {
        if enterfrac < 0.0 {
            enterfrac = 0.0;
        }
        trace.fraction = enterfrac;
        trace.normal = clip_normal.unwrap_or(Vec3::ZERO);
        trace.ent = Some(ent);
    }
}

/// Sweep the box `[mins, maxs]` (relative to the moving point) from `start` to `end`.
pub fn trace_hull(env: &TraceEnv, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> Trace {
    let mut tr = Trace::none(end);

    for b in env.world {
        clip_box_to_brush(b, HitEnt::World, mins, maxs, start, end, &mut tr);
        if tr.allsolid {
            break;
        }
    }
    if !tr.allsolid {
        for (idx, b) in env.players {
            clip_box_to_brush(b, HitEnt::Player(*idx), mins, maxs, start, end, &mut tr);
            if tr.allsolid {
                break;
            }
        }
    }
    if !tr.allsolid {
        for (id, b) in env.targets {
            clip_box_to_brush(b, HitEnt::Target(*id), mins, maxs, start, end, &mut tr);
            if tr.allsolid {
                break;
            }
        }
    }

    if tr.allsolid {
        tr.fraction = 0.0;
        tr.endpos = start;
    } else if tr.fraction == 1.0 {
        tr.endpos = end;
    } else {
        tr.endpos = Vec3::new(
            start.x + tr.fraction * (end.x - start.x),
            start.y + tr.fraction * (end.y - start.y),
            start.z + tr.fraction * (end.z - start.z),
        );
    }
    tr
}

/// A point (ray) trace.
pub fn trace_line(env: &TraceEnv, start: Vec3, end: Vec3) -> Trace {
    trace_hull(env, start, end, Vec3::ZERO, Vec3::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor() -> Vec<Aabb> {
        vec![Aabb::new(Vec3::new(-100.0, -100.0, -64.0), Vec3::new(100.0, 100.0, 0.0))]
    }

    #[test]
    fn hull_stops_above_floor() {
        let w = floor();
        let env = TraceEnv::world_only(&w);
        let mins = Vec3::new(-24.0, -24.0, 0.0);
        let maxs = Vec3::new(24.0, 24.0, 82.0);
        let tr = trace_hull(&env, Vec3::new(0.0, 0.0, 50.0), Vec3::new(0.0, 0.0, -50.0), mins, maxs);
        assert!(tr.fraction < 1.0);
        assert_eq!(tr.normal, Vec3::new(0.0, 0.0, 1.0));
        assert!((tr.endpos.z - DIST_EPSILON).abs() < 1e-4, "{}", tr.endpos.z);
        assert!(!tr.startsolid);
    }

    #[test]
    fn point_inside_is_allsolid() {
        let w = floor();
        let env = TraceEnv::world_only(&w);
        let tr = trace_line(&env, Vec3::new(0.0, 0.0, -10.0), Vec3::new(0.0, 0.0, -20.0));
        assert!(tr.startsolid && tr.allsolid);
        assert_eq!(tr.fraction, 0.0);
    }

    #[test]
    fn miss_is_full_fraction() {
        let w = floor();
        let env = TraceEnv::world_only(&w);
        let tr = trace_line(&env, Vec3::new(0.0, 0.0, 10.0), Vec3::new(10.0, 0.0, 10.0));
        assert_eq!(tr.fraction, 1.0);
        assert_eq!(tr.normal, Vec3::ZERO);
        assert!(tr.ent.is_none());
    }
}
