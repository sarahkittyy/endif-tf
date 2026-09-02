//! Rocket launcher, rockets, explosions, damage and knockback. Ports of `CTFWeaponBaseGun::FireRocket`,
//! `CTFWeaponBase::GetProjectileFireSetup`, `CTFBaseRocket`, `CTFGameRules::RadiusDamage`,
//! `CTFRadiusDamageInfo::ApplyToEntity`, `CTFGameRules::ApplyOnDamageModifyRules` (distance mod),
//! `CTFPlayer::OnTakeDamage(_Alive)` and `CTFPlayer::ApplyPushFromDamage`, plus the MGEMod endif rules.

use crate::consts::*;
use crate::math::*;
use crate::player::*;
use crate::rng::Rng;
use crate::trace::*;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

/// A live rocket (`CTFProjectile_Rocket`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rocket {
    pub id: u32,
    pub owner: u8,
    pub origin: Vec3,
    pub velocity: Vec3,
    pub angles: QAngle,
    pub spawn_tick: u32,
    /// Where the rocket was fired from (for the "airshot from N units" readout).
    pub start: Vec3,
}

impl Hash for Rocket {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.owner.hash(state);
        self.origin.hash(state);
        self.velocity.hash(state);
        self.angles.hash(state);
        self.spawn_tick.hash(state);
        self.start.hash(state);
    }
}

/// Rules knobs that are normally server convars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rules {
    /// `tf_damage_disablespread`. Competitive/MGE configs set this to 1, which also makes
    /// knockback deterministic. When false the spread uses the simulation RNG.
    pub disable_damage_spread: bool,
    /// MGE `fraglimit`.
    pub frag_limit: i32,
    /// Ticks between death and respawn. Zero means the player is back on the very next tick.
    pub respawn_delay_ticks: u32,
    /// Ticks both players stay dead after a round is won, before the next round's spawn.
    pub round_reset_delay_ticks: u32,
    /// Whether the MGE endif `BoostVectors` multiplier is applied.
    pub endif_boost: bool,
    /// endif.tf house rule (not in MGEMod): a player who was killed respawns this many units above
    /// the spawn point, still in the air, so the opponent can chain airshots.
    pub respawn_height: u32,
    /// endif.tf house rule: a high respawn is flung sideways so the fall is not a vertical line.
    /// The angle, in degrees off vertical, between the spawn point and where an untouched fall
    /// would land; drawn uniformly from this inclusive range with the simulation RNG.
    pub respawn_fling_deg: (u32, u32),
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            disable_damage_spread: true,
            frag_limit: MGE_FRAG_LIMIT,
            respawn_delay_ticks: 0,
            round_reset_delay_ticks: 67,
            endif_boost: true,
            respawn_height: 768,
            respawn_fling_deg: (25, 35),
        }
    }
}

/// `DamageForce( const Vector &size, float damage, float scale )` from tf_player.cpp.
/// The C++ evaluates `damage * ((48 * 48 * 82.0) / (size.x*size.y*size.z)) * scale` in double.
pub fn damage_force(size: Vec3, damage: f32, scale: f32) -> f32 {
    let vol = (size.x * size.y * size.z) as f64;
    let force = damage as f64 * ((48.0 * 48.0 * 82.0) / vol) * scale as f64;
    let mut force = force as f32;
    if force > DAMAGE_FORCE_MAX {
        force = DAMAGE_FORCE_MAX;
    }
    force
}

/// Everything `fire_rocket` needs to know about the world.
pub struct FireContext<'a> {
    pub env_all: TraceEnv<'a>,
    pub env_world: TraceEnv<'a>,
}

/// `CTFWeaponBaseGun::FireRocket` + `CTFWeaponBase::GetProjectileFireSetup` + `CTFBaseRocket::Create`.
pub fn fire_rocket(shooter: &Player, shooter_idx: u8, id: u32, tick: u32, ctx: &FireContext) -> Rocket {
    let mut offset = ROCKET_FIRE_OFFSET;
    if shooter.flags & FL_DUCKING != 0 {
        offset.z = ROCKET_FIRE_OFFSET_Z_DUCKED;
    }

    // GetProjectileFireSetup
    let ang_spread = QAngle::new(shooter.view_angles.pitch, shooter.view_angles.yaw, 0.0);
    let (forward, right, up) = angle_vectors(ang_spread);

    let shoot_pos = shooter.eye_position(); // Weapon_ShootPosition() == EyePosition()

    // Estimate end point
    let end_pos = shoot_pos + forward * PROJECTILE_AIM_DIST;

    // Trace forward and find what's in front of us, and aim at that (ignoring teammates → only enemies + world).
    let tr = trace_line(&ctx.env_all, shoot_pos, end_pos);

    // Offset actual start point
    let src = shoot_pos + (forward * offset.x) + (right * offset.y) + (up * offset.z);

    // Find angles that will get us to our desired end point
    // Only use the trace end if it wasn't too close, which results in visually bizarre forward angles
    let ang_forward = if tr.fraction > 0.1 {
        vector_angles(tr.endpos - src)
    } else {
        vector_angles(end_pos - src)
    };

    // FireRocket: keep the spawn point out of walls (MASK_SOLID_BRUSHONLY trace from the eye).
    let tr2 = trace_line(&ctx.env_world, shoot_pos, src);
    let origin = tr2.endpos;

    // CTFBaseRocket::Create
    let (fwd2, _, _) = angle_vectors(ang_forward);
    let velocity = fwd2 * ROCKET_SPEED;
    let angles = vector_angles(velocity);

    Rocket { id, owner: shooter_idx, origin, velocity, angles, spawn_tick: tick, start: origin }
}

/// Outcome of one explosion, for the world to turn into events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitResult {
    pub victim: u8,
    pub attacker: u8,
    pub damage: f32,
    pub direct: bool,
    pub force: Vec3,
    /// MGE endif: this hit counted as an airshot kill.
    pub airshot_kill: bool,
    /// Height of the victim's origin above the surface below at the time of the hit.
    pub height_above_ground: f32,
}

/// `CTFRadiusDamageInfo::ApplyToEntity` followed by the TF2 damage pipeline for one victim.
/// Returns `None` when the explosion did not affect the victim.
#[allow(clippy::too_many_arguments)]
pub fn apply_explosion_to_player(
    victim: &mut Player,
    victim_idx: u8,
    attacker_snapshot: &Player,
    attacker_idx: u8,
    src: Vec3,
    radius: f32,
    base_damage: f32,
    direct: bool,
    env_world: &TraceEnv,
    rules: &Rules,
    rng: &mut Rng,
) -> Option<HitResult> {
    if !victim.alive {
        return None;
    }

    // CEntitySphereQuery does a box test, then RadiusDamage checks the nearest point on the
    // collision bounds against the radius.
    let nearest = victim.world_aabb().nearest_point(src);
    if (src - nearest).length_sqr() > radius * radius {
        return None;
    }

    // Check that the explosion can 'see' this entity (BodyTarget == EyePosition, players ignored).
    let spot = victim.eye_position();
    let tr = trace_line(env_world, src, spot);
    if tr.fraction != 1.0 {
        return None;
    }

    // Adjust the damage - apply falloff.
    let distance_to_entity = if direct {
        // Rockets store the ent they hit as the enemy and have already dealt full damage to them by this time
        0.0
    } else {
        // Use whichever is closer, absorigin or worldspacecenter
        let to_center = (src - victim.world_space_center()).length();
        let to_origin = (src - victim.origin).length();
        fmin(to_center, to_origin)
    };

    let adjusted = remap_val_clamped(distance_to_entity, 0.0, radius, base_damage, base_damage * ROCKET_FALLOFF);
    if adjusted <= 0.0 {
        return None;
    }

    // ---- CTFPlayer::OnTakeDamage ----
    let is_self = victim_idx == attacker_idx;
    let mut damage = adjusted;

    // Soldier rocket-jump self damage scale. In `CTFPlayer::OnTakeDamage` this runs *before*
    // `ApplyOnDamageModifyRules`, whose first line captures `info.GetDamage()` as the damage used
    // for the push force (`SetDamageForForceCalc`). So an airborne rocket jump pushes with the
    // 0.6-scaled damage; a grounded ("bad") rocket jump and hits on other players do not scale.
    let self_rocket_jumping = is_self && victim.flags & FL_ONGROUND == 0;
    if self_rocket_jumping {
        damage *= TF_DAMAGESCALE_SELF_SOLDIER;
    }

    // ---- CTFGameRules::ApplyOnDamageModifyRules ----
    let damage_for_force_calc = damage; // SetDamageForForceCalc(info.GetDamage()) before the spread
    if !is_self {
        // If we're not damaging ourselves, apply randomness (DMG_USEDISTANCEMOD is set for rockets).
        let mut random_damage = damage * TF_DAMAGE_RANGE;
        let random_damage_spread = TF_DAMAGE_SPREAD;
        let mut min = 0.5 - random_damage_spread;
        let mut max = 0.5 + random_damage_spread;

        let attacker_pos = attacker_snapshot.world_space_center();
        let optimal_distance = TF_DAMAGE_OPTIMAL_DISTANCE;
        let distance = fmax(1.0, (victim.world_space_center() - attacker_pos).length());
        let center = remap_val_clamped(distance / optimal_distance, 0.0, 2.0, 1.0, 0.0);
        // bDoShortRangeDistanceIncrease is true for non-crits, so both branches apply.
        if center > 0.5 || center <= 0.5 {
            min = fmax(0.0, center - random_damage_spread);
            max = fmin(1.0, center + random_damage_spread);
        }

        let random_range_val = if rules.disable_damage_spread {
            min + random_damage_spread
        } else {
            rng.random_float(min, max)
        };

        // Rocket launcher only has half the bonus of the other weapons at short range
        if random_range_val > 0.5 {
            random_damage *= 0.5;
        }

        // Random damage variance.
        let dmg_variance = simple_spline_remap_val_clamped(random_range_val, 0.0, 1.0, -random_damage, random_damage);
        // (bDoShortRangeDistanceIncrease && variance > 0) || bDoLongRangeDistanceDecrease → always for non-crits.
        damage += dmg_variance;
    }

    // ---- CTFPlayer::OnTakeDamage_Alive ----
    // (Pretend that the inflictor is a little lower than it really is, so the body will tend to fly upward a bit).
    let mut vec_dir = src - Vec3::new(0.0, 0.0, DAMAGE_DIR_Z_FUDGE) - victim.world_space_center();
    vec_dir.normalize_in_place();

    // Do the damage.
    victim.health -= damage as i32;

    // Apply a damage force (ApplyPushFromDamage).
    let force = if is_self {
        let mut size = victim.world_align_size();
        let hull_size_crouch = VEC_DUCK_HULL_MAX - VEC_DUCK_HULL_MIN;
        if size == hull_size_crouch {
            // "Ducking actually increases blast force, this value increases it even more 82 standing, 62 ducking, 55 modified"
            size.z = SELF_PUSH_DUCKED_HULL_Z;
        }
        let scale = if victim.flags & FL_ONGROUND != 0 {
            TF_DAMAGEFORCESCALE_SELF_SOLDIER_BADRJ
        } else {
            TF_DAMAGEFORCESCALE_SELF_SOLDIER_RJ
        };
        let f = vec_dir * -damage_force(size, damage_for_force_calc, scale);
        // Reset duck in air on self rocket impulse.
        victim.air_ducked = 0;
        f
    } else {
        vec_dir * -damage_force(victim.world_align_size(), damage, TF_DAMAGEFORCESCALE_OTHER)
    };

    // ApplyAbsVelocityImpulse
    victim.velocity += force;

    // ---- MGEMod: Event_PlayerHurt ----
    let height = distance_above_ground(victim.origin, env_world);
    let mut airshot_kill = false;
    if !is_self {
        if direct {
            let victim_in_air = victim.flags & FL_ONGROUND == 0;
            if victim_in_air && height >= MGE_AIRSHOT_HEIGHT && height >= MGE_ENDIF_AIRSHOT_HEIGHT {
                airshot_kill = true;
            }
        }
    }

    Some(HitResult {
        victim: victim_idx,
        attacker: attacker_idx,
        damage,
        direct,
        force,
        airshot_kill,
        height_above_ground: height,
    })
}

/// MGEMod `DistanceAboveGround`: trace straight down from the origin, ignoring players.
pub fn distance_above_ground(origin: Vec3, env_world: &TraceEnv) -> f32 {
    let end = Vec3::new(origin.x, origin.y, origin.z - 65536.0);
    let tr = trace_line(env_world, origin, end);
    if tr.fraction < 1.0 || tr.startsolid {
        (origin - tr.endpos).length()
    } else {
        -1.0
    }
}

/// MGEMod `BoostVectors`.
pub fn boost_vectors(velocity: Vec3) -> Vec3 {
    Vec3::new(
        velocity.x * MGE_ENDIF_FORCE_X,
        velocity.y * MGE_ENDIF_FORCE_Y,
        if velocity.z > 0.0 { velocity.z * MGE_ENDIF_FORCE_Z } else { velocity.z },
    )
}
