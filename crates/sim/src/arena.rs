//! Static arena geometry. Phase 1: the classic large square endif platform boxed in by four walls
//! and a ceiling, four spawn points on a cross around the centre (mirroring the mge_training_v8
//! layout).
//!
//! The arena has two wall sets. Players collide with the inner walls at `half_size`; rockets fly
//! through those and explode on the outer walls at `outer_half_size`, which are the walls that
//! are drawn. Keeping the explosions `WALL_ROCKET_GAP` units behind the player walls stops the
//! "wall ride" (hugging a wall and rocketing it at your feet to climb to the ceiling).

use crate::consts::*;
use crate::math::{QAngle, Vec3};
use crate::trace::Aabb;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spawn {
    pub origin: Vec3,
    pub angles: QAngle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Arena {
    pub name: String,
    /// Half extent of the square the players are confined to (inner, invisible walls).
    pub half_size: f32,
    /// Half extent of the visible floor and walls; rockets explode on these.
    pub outer_half_size: f32,
    /// Height of the ceiling above the floor (units).
    pub ceiling: f32,
    /// Solid brushes for players: floor slab, four inner walls and the ceiling slab.
    pub brushes: Vec<Aabb>,
    /// Solid brushes for rockets: floor slab, four outer walls and the ceiling slab.
    pub rocket_brushes: Vec<Aabb>,
    pub spawns: Vec<Spawn>,
    /// Height of the painted "airshot line" above the floor (visual + scoring reference).
    pub airshot_line_height: f32,
}

/// Height of the ceiling above the floor. Rockets fired upwards explode on it instead of living
/// out their lifetime, and the walls end there.
pub const CEILING_HEIGHT: f32 = 2000.0;
/// Top of the walls.
pub const WALL_TOP: f32 = CEILING_HEIGHT;
/// Thickness of the wall/floor/ceiling brushes.
pub const BRUSH_THICKNESS: f32 = 256.0;
/// How far behind the player-collision walls the visible walls (where rockets explode) sit.
pub const WALL_ROCKET_GAP: f32 = 64.0;

/// Floor, four walls and ceiling for a square of half extent `h`.
fn box_brushes(h: f32) -> Vec<Aabb> {
    let t = BRUSH_THICKNESS;
    vec![
        // floor
        Aabb::new(Vec3::new(-h - t, -h - t, -t), Vec3::new(h + t, h + t, 0.0)),
        // +x wall
        Aabb::new(Vec3::new(h, -h - t, 0.0), Vec3::new(h + t, h + t, WALL_TOP)),
        // -x wall
        Aabb::new(Vec3::new(-h - t, -h - t, 0.0), Vec3::new(-h, h + t, WALL_TOP)),
        // +y wall
        Aabb::new(Vec3::new(-h - t, h, 0.0), Vec3::new(h + t, h + t, WALL_TOP)),
        // -y wall
        Aabb::new(Vec3::new(-h - t, -h - t, 0.0), Vec3::new(h + t, -h, WALL_TOP)),
        // ceiling
        Aabb::new(Vec3::new(-h - t, -h - t, CEILING_HEIGHT), Vec3::new(h + t, h + t, CEILING_HEIGHT + t)),
    ]
}

impl Arena {
    /// The classic square endif arena.
    pub fn classic_square() -> Arena {
        let h = 416.0f32;
        let outer = h + WALL_ROCKET_GAP;
        let brushes = box_brushes(h);
        let rocket_brushes = box_brushes(outer);
        let d = 300.0;
        let spawns = vec![
            Spawn { origin: Vec3::new(d, 0.0, 0.0), angles: QAngle::new(0.0, 180.0, 0.0) },
            Spawn { origin: Vec3::new(0.0, d, 0.0), angles: QAngle::new(0.0, -90.0, 0.0) },
            Spawn { origin: Vec3::new(0.0, -d, 0.0), angles: QAngle::new(0.0, 90.0, 0.0) },
            Spawn { origin: Vec3::new(-d, 0.0, 0.0), angles: QAngle::new(0.0, 0.0, 0.0) },
        ];
        Arena {
            name: "Endif".to_string(),
            half_size: h,
            outer_half_size: outer,
            ceiling: CEILING_HEIGHT,
            brushes,
            rocket_brushes,
            spawns,
            airshot_line_height: MGE_ENDIF_AIRSHOT_HEIGHT,
        }
    }

    pub fn floor_z(&self) -> f32 {
        0.0
    }

    /// The point on the floor in the middle of the arena; fresh spawns look at it.
    pub fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 0.0, self.floor_z())
    }
}
