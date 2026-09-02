//! Numeric constants taken verbatim from the TF2 SDK (`source-sdk-2013`, `src/game/shared/...`)
//! and from the MGEMod plugin (`mge.sp`). Comments give the source of each value.

use crate::math::Vec3;

/// TF2 runs at 66.67 ticks per second: `sv.m_flTickInterval = 0.015`.
pub const TICK_INTERVAL: f32 = 0.015;
/// Ticks per second used for real-time pacing (66.67 rounded).
pub const TICKS_PER_SECOND: f32 = 1.0 / TICK_INTERVAL;

// ---- movevars_shared.cpp -------------------------------------------------------------------
pub const SV_GRAVITY: f32 = 800.0;
pub const SV_STOPSPEED: f32 = 100.0;
pub const SV_ACCELERATE: f32 = 10.0;
pub const SV_AIRACCELERATE: f32 = 10.0;
pub const SV_FRICTION: f32 = 4.0;
pub const SV_BOUNCE: f32 = 0.0;
pub const SV_MAXVELOCITY: f32 = 3500.0;
pub const SV_STEPSIZE: f32 = 18.0;

// ---- gamemovement.h / gamemovement.cpp -----------------------------------------------------
/// `CGameMovement::GetAirSpeedCap()` returns 30.
pub const AIR_SPEED_CAP: f32 = 30.0;
/// `MAX_CLIP_PLANES` in `TryPlayerMove`.
pub const MAX_CLIP_PLANES: usize = 5;
/// `GAMEMOVEMENT_DUCK_TIME` (ms).
pub const GAMEMOVEMENT_DUCK_TIME: f32 = 1000.0;
/// `TIME_TO_DUCK` / `TIME_TO_UNDUCK` (seconds) from shareddefs.h (non-CS branch).
pub const TIME_TO_DUCK: f32 = 0.2;
pub const TIME_TO_UNDUCK: f32 = 0.2;
/// `PLAYER_FALL_PUNCH_THRESHOLD` (shareddefs.h, non-CS branch).
pub const PLAYER_FALL_PUNCH_THRESHOLD: f32 = 350.0;
/// `DIST_EPSILON` from coordsize.h.
pub const DIST_EPSILON: f32 = 0.03125;
/// `COORD_RESOLUTION` = 1/32.
pub const COORD_RESOLUTION: f32 = 1.0 / 32.0;

// ---- tf_gamemovement.cpp -------------------------------------------------------------------
/// `TF_MAX_SPEED (400 * 1.3)`.
pub const TF_MAX_SPEED: f32 = 400.0 * 1.3;
/// Soldier run speed (class data).
pub const SOLDIER_MAX_SPEED: f32 = 240.0;
/// `flMul = ( 289.0f * flJumpMod ) * flGroundFactor` in `CTFGameMovement::CheckJumpButton`.
pub const TF_JUMP_VELOCITY: f32 = 289.0;
/// `BUNNYJUMP_MAX_SPEED_FACTOR` (see `PreventBunnyJumping`).
pub const BUNNYJUMP_MAX_SPEED_FACTOR: f32 = 1.2;
/// `tf_clamp_back_speed` / `tf_clamp_back_speed_min`.
pub const TF_CLAMP_BACK_SPEED: f32 = 0.9;
pub const TF_CLAMP_BACK_SPEED_MIN: f32 = 100.0;
/// `TF_TIME_TO_DUCK`: minimum time between ducks (seconds).
pub const TF_TIME_TO_DUCK: f32 = 0.3;
/// `TF_AIRDUCKED_COUNT`: number of ducks allowed per air event.
pub const TF_AIRDUCKED_COUNT: i32 = 2;
/// `TracePlayerBBox` uses the ground normal threshold 0.7 everywhere.
pub const GROUND_NORMAL_Z: f32 = 0.7;

// ---- tf_gamerules.cpp: g_TFViewVectors / g_TFClassViewVectors -----------------------------
pub const VEC_HULL_MIN: Vec3 = Vec3::new(-24.0, -24.0, 0.0);
pub const VEC_HULL_MAX: Vec3 = Vec3::new(24.0, 24.0, 82.0);
pub const VEC_DUCK_HULL_MIN: Vec3 = Vec3::new(-24.0, -24.0, 0.0);
pub const VEC_DUCK_HULL_MAX: Vec3 = Vec3::new(24.0, 24.0, 62.0);
pub const VEC_DUCK_VIEW: Vec3 = Vec3::new(0.0, 0.0, 45.0);
/// Soldier eye height: `g_TFClassViewVectors[TF_CLASS_SOLDIER] = (0, 0, 68)`.
pub const SOLDIER_VIEW: Vec3 = Vec3::new(0.0, 0.0, 68.0);

// ---- client usercmd generation (cl_forwardspeed / cl_sidespeed) ---------------------------
pub const CL_FORWARDSPEED: f32 = 450.0;
pub const CL_SIDESPEED: f32 = 450.0;
/// `cl_pitchup` / `cl_pitchdown`.
pub const MAX_PITCH: f32 = 89.0;

// ---- Rocket launcher (tf_weapon_rocketlauncher.txt + tf_weaponbase_rocket.h) --------------
/// `flLaunchSpeed = 1100.0f`.
pub const ROCKET_SPEED: f32 = 1100.0;
/// Weapon script `Damage 90`.
pub const ROCKET_DAMAGE: f32 = 90.0;
/// `TF_ROCKET_RADIUS (146)`: radius used when applying damage to others.
pub const TF_ROCKET_RADIUS: f32 = 146.0;
/// `TF_ROCKET_RADIUS_FOR_RJS (110.0f * 1.1f)`: radius used when applying damage to the attacker.
pub const TF_ROCKET_RADIUS_FOR_RJS: f32 = 110.0 * 1.1;
/// Weapon script `TimeFireDelay 0.8`.
pub const ROCKET_FIRE_DELAY: f32 = 0.8;
/// Clip size of the stock rocket launcher.
pub const ROCKET_CLIP_SIZE: i32 = 4;
/// Base weapon deploy time (`CTFWeaponBase::Deploy`).
pub const WEAPON_DEPLOY_TIME: f32 = 0.5;
/// `GetProjectileFireSetup` traces `flEndDist = 2000` in front of the eye to aim projectiles.
pub const PROJECTILE_AIM_DIST: f32 = 2000.0;
/// `Vector vecOffset( 23.5f, 12.0f, -3.0f )` in `CTFWeaponBaseGun::FireRocket`; z becomes 8 when ducking.
pub const ROCKET_FIRE_OFFSET: Vec3 = Vec3::new(23.5, 12.0, -3.0);
pub const ROCKET_FIRE_OFFSET_Z_DUCKED: f32 = 8.0;
/// Rockets that never hit anything are removed after this many seconds.
pub const ROCKET_MAX_LIFETIME: f32 = 12.0;

// ---- Damage (tf_player.cpp / tf_gamerules.cpp) ----------------------------------------------
/// `DMG_HALF_FALLOFF` → `CTFRadiusDamageInfo::flFalloff = 0.5`.
pub const ROCKET_FALLOFF: f32 = 0.5;
/// `tf_damage_range` (0.5): random damage range as a fraction of base damage.
pub const TF_DAMAGE_RANGE: f32 = 0.5;
/// `flRandomDamageSpread = 0.10f`.
pub const TF_DAMAGE_SPREAD: f32 = 0.10;
/// `flOptimalDistance = 512.0` in the distance mod.
pub const TF_DAMAGE_OPTIMAL_DISTANCE: f32 = 512.0;
/// `tf_damageforcescale_other`.
pub const TF_DAMAGEFORCESCALE_OTHER: f32 = 6.0;
/// `tf_damageforcescale_self_soldier_rj` (airborne rocket jump).
pub const TF_DAMAGEFORCESCALE_SELF_SOLDIER_RJ: f32 = 10.0;
/// `tf_damageforcescale_self_soldier_badrj` (grounded rocket jump).
pub const TF_DAMAGEFORCESCALE_SELF_SOLDIER_BADRJ: f32 = 5.0;
/// `tf_damagescale_self_soldier` (HP only, when airborne).
pub const TF_DAMAGESCALE_SELF_SOLDIER: f32 = 0.60;
/// Ducked self-hull z override in `ApplyPushFromDamage`: "82 standing, 62 ducking, 55 modified".
pub const SELF_PUSH_DUCKED_HULL_Z: f32 = 55.0;
/// `DamageForce` caps the force at 1000.
pub const DAMAGE_FORCE_MAX: f32 = 1000.0;
/// Soldier max health.
pub const SOLDIER_MAX_HEALTH: i32 = 200;
/// `vecDir = inflictor->WorldSpaceCenter() - Vector(0,0,10) - WorldSpaceCenter()`.
pub const DAMAGE_DIR_Z_FUDGE: f32 = 10.0;

// ---- MGEMod endif (mge.sp) ------------------------------------------------------------------
/// `mgemod_airshot_height` (80).
pub const MGE_AIRSHOT_HEIGHT: f32 = 80.0;
/// Hardcoded `dist >= 250` for endif kills.
pub const MGE_ENDIF_AIRSHOT_HEIGHT: f32 = 250.0;
/// `mgemod_endif_force_x/y/z`.
pub const MGE_ENDIF_FORCE_X: f32 = 1.1;
pub const MGE_ENDIF_FORCE_Y: f32 = 1.1;
pub const MGE_ENDIF_FORCE_Z: f32 = 2.15;
/// `CreateTimer(0.1, BoostVectors, ...)`: 0.1 s = 6.67 ticks → fires on the 7th tick.
pub const MGE_ENDIF_BOOST_DELAY_TICKS: u32 = 7;
/// Infinite ammo crutch: `CreateTimer(0.4, Timer_GiveAmmo)` after pressing attack.
pub const MGE_INFAMMO_REFILL_DELAY: f32 = 0.4;
/// Default `fraglimit` of the endif arenas.
pub const MGE_FRAG_LIMIT: i32 = 5;
/// Minimum distance between a fresh spawn and the opponent (`mindist`).
pub const MGE_MIN_SPAWN_DIST: f32 = 100.0;

/// Half-width, in degrees, of the random yaw around the spawn's facing direction that a high
/// respawn is flung towards (see `Rules::respawn_fling_deg`). The spawns sit 300 units from the
/// centre of a 416-unit half-size square, so ±45° keeps the longest fling inside the walls.
pub const RESPAWN_FLING_YAW_SPREAD: f32 = 45.0;
