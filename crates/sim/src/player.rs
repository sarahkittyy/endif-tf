//! Player state. Everything here is part of the rollback snapshot.

use crate::consts::*;
use crate::math::{QAngle, Vec3};
use crate::trace::{Aabb, HitEnt};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

/// `FL_ONGROUND`.
pub const FL_ONGROUND: u32 = 1 << 0;
/// `FL_DUCKING`.
pub const FL_DUCKING: u32 = 1 << 1;

/// Which rocket launcher a player holds. The two are the same weapon in every number that
/// matters (speed, damage, clip, fire rate); The Original (item 513) carries the
/// `centerfire_projectile` attribute, so its rockets leave from the middle of the screen instead
/// of over the right shoulder (`CTFWeaponBase::GetProjectileFireSetup`), and it has its own
/// model, viewmodel animations and Quake sounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Weapon {
    #[default]
    Stock,
    Original,
}

impl Weapon {
    /// The launcher a player's input asks for (`IN_WEAPON_ORIGINAL` / `IN_WEAPON_STOCK`), or
    /// `None` for a blank input that states no preference.
    pub fn from_buttons(buttons: u32) -> Option<Weapon> {
        use crate::input::{IN_WEAPON_ORIGINAL, IN_WEAPON_STOCK};
        if buttons & IN_WEAPON_ORIGINAL != 0 {
            Some(Weapon::Original)
        } else if buttons & IN_WEAPON_STOCK != 0 {
            Some(Weapon::Stock)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Player {
    // ---- transform / physics (CBasePlayer + CMoveData persisted parts) ----
    pub origin: Vec3,
    pub velocity: Vec3,
    pub base_velocity: Vec3,
    /// Eye angles from the last command (pitch, yaw, roll).
    pub view_angles: QAngle,
    /// `m_vecViewOffset`: eye position relative to origin.
    pub view_offset: Vec3,
    /// Angles the player was given at the last spawn, aimed at the arena's centre floor point. The
    /// spawn tick's own input overwrites `view_angles` at once, so the client reads these (keyed
    /// on `spawn_tick`) to point its live look angles that way when it sees a respawn.
    pub spawn_angles: QAngle,
    pub ground: Option<HitEnt>,
    /// `FL_*` flags we care about.
    pub flags: u32,

    // ---- ducking (m_Local) ----
    pub ducked: bool,
    pub ducking: bool,
    pub in_duck_jump: bool,
    pub duck_time: f32,
    pub duck_jump_time: f32,
    pub jump_time: f32,
    /// `m_Shared.m_flDuckTimer`: absolute time before which grounded ducking is blocked.
    pub duck_timer: f32,
    /// `m_Shared.AirDuckedCount()`.
    pub air_ducked: i32,
    pub air_dash: i32,

    // ---- misc movement ----
    pub fall_velocity: f32,
    pub surface_friction: f32,
    pub jumping: bool,
    pub old_buttons: u32,
    pub max_speed: f32,
    /// `m_bGameCodeMovedPlayer`: set after a teleport so the next move re-categorizes.
    pub game_code_moved: bool,

    // ---- health / life ----
    pub alive: bool,
    pub health: i32,
    pub max_health: i32,
    /// Tick at which a dead player respawns.
    pub respawn_tick: u32,
    /// The next respawn is after a death: place the player `Rules::respawn_height` up in the air.
    pub respawn_high: bool,
    /// Tick of the most recent spawn (for spawn protection/animations on the client).
    pub spawn_tick: u32,

    // ---- weapon ----
    /// The launcher in hand. Picked from the player's input on the tick they spawn and kept for
    /// the whole life, like a loadout change in TF2 that only takes effect at the next spawn
    /// (every tick under `Rules::instant_weapon_switch`).
    pub weapon: Weapon,
    /// A fresh spawn that has not read a launcher preference yet (`SimState::begin` spawns before
    /// the first input exists, and the first inputs GGRS delivers are blank padding);
    /// `SimState::step` resolves it from the first input that states one.
    pub weapon_pending: bool,
    pub next_primary_attack: f32,
    pub clip: i32,
    /// Absolute time at which the infinite-ammo crutch refills the clip; negative when idle.
    pub clip_refill_time: f32,

    // ---- MGE bookkeeping ----
    pub score: i32,
    /// Ticks at which pending endif `BoostVectors` timers fire.
    pub pending_boosts: Vec<u32>,
    /// Number of confirmed airshot kills this round (stats).
    pub airshots: i32,
    /// Whether the player has touched the ground since spawning. False while falling in from a
    /// high respawn: a kill made before it becomes true extends the attacker's `chain`.
    pub landed_since_spawn: bool,
    /// Length of the current kill chain: 1 after an ordinary kill, one more for each kill on a
    /// victim that had not landed since respawning, 0 after dying. The kill feed shows "x2",
    /// "x3", ... for chains of two or more.
    pub chain: u8,
}

impl Hash for Player {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.origin.hash(state);
        self.velocity.hash(state);
        self.base_velocity.hash(state);
        self.view_angles.hash(state);
        self.view_offset.hash(state);
        self.spawn_angles.hash(state);
        self.ground.hash(state);
        self.flags.hash(state);
        self.ducked.hash(state);
        self.ducking.hash(state);
        self.in_duck_jump.hash(state);
        self.duck_time.to_bits().hash(state);
        self.duck_jump_time.to_bits().hash(state);
        self.jump_time.to_bits().hash(state);
        self.duck_timer.to_bits().hash(state);
        self.air_ducked.hash(state);
        self.air_dash.hash(state);
        self.fall_velocity.to_bits().hash(state);
        self.surface_friction.to_bits().hash(state);
        self.jumping.hash(state);
        self.old_buttons.hash(state);
        self.max_speed.to_bits().hash(state);
        self.game_code_moved.hash(state);
        self.alive.hash(state);
        self.health.hash(state);
        self.max_health.hash(state);
        self.respawn_tick.hash(state);
        self.respawn_high.hash(state);
        self.spawn_tick.hash(state);
        self.weapon.hash(state);
        self.weapon_pending.hash(state);
        self.next_primary_attack.to_bits().hash(state);
        self.clip.hash(state);
        self.clip_refill_time.to_bits().hash(state);
        self.score.hash(state);
        self.pending_boosts.hash(state);
        self.airshots.hash(state);
        self.landed_since_spawn.hash(state);
        self.chain.hash(state);
    }
}

impl Default for Player {
    fn default() -> Self {
        Player {
            origin: Vec3::ZERO,
            velocity: Vec3::ZERO,
            base_velocity: Vec3::ZERO,
            view_angles: QAngle::default(),
            view_offset: SOLDIER_VIEW,
            spawn_angles: QAngle::default(),
            ground: None,
            flags: 0,
            ducked: false,
            ducking: false,
            in_duck_jump: false,
            duck_time: 0.0,
            duck_jump_time: 0.0,
            jump_time: 0.0,
            duck_timer: 0.0,
            air_ducked: 0,
            air_dash: 0,
            fall_velocity: 0.0,
            surface_friction: 1.0,
            jumping: false,
            old_buttons: 0,
            max_speed: SOLDIER_MAX_SPEED,
            game_code_moved: true,
            alive: false,
            health: SOLDIER_MAX_HEALTH,
            max_health: SOLDIER_MAX_HEALTH,
            respawn_tick: 0,
            respawn_high: false,
            spawn_tick: 0,
            weapon: Weapon::Stock,
            weapon_pending: true,
            next_primary_attack: 0.0,
            clip: ROCKET_CLIP_SIZE,
            clip_refill_time: -1.0,
            score: 0,
            pending_boosts: Vec::new(),
            airshots: 0,
            landed_since_spawn: false,
            chain: 0,
        }
    }
}

impl Player {
    pub fn on_ground(&self) -> bool {
        self.ground.is_some()
    }

    pub fn is_ducking_flag(&self) -> bool {
        self.flags & FL_DUCKING != 0
    }

    /// `GetPlayerMins(ducked)` using the class hull (`m_bDucked` selects the crouch hull).
    pub fn hull_mins(&self) -> Vec3 {
        if self.ducked { VEC_DUCK_HULL_MIN } else { VEC_HULL_MIN }
    }

    pub fn hull_maxs(&self) -> Vec3 {
        if self.ducked { VEC_DUCK_HULL_MAX } else { VEC_HULL_MAX }
    }

    /// Absolute collision bounds (`CollisionProp()->WorldSpaceAABB`).
    pub fn world_aabb(&self) -> Aabb {
        Aabb::new(self.origin + self.hull_mins(), self.origin + self.hull_maxs())
    }

    /// `WorldSpaceCenter()`: centre of the collision bounds.
    pub fn world_space_center(&self) -> Vec3 {
        self.origin + (self.hull_mins() + self.hull_maxs()) * 0.5
    }

    /// `WorldAlignSize()`: extents of the collision bounds.
    pub fn world_align_size(&self) -> Vec3 {
        self.hull_maxs() - self.hull_mins()
    }

    /// `EyePosition()`.
    pub fn eye_position(&self) -> Vec3 {
        self.origin + self.view_offset
    }

    /// Resets movement/weapon state for a fresh spawn at `origin` facing `angles`. The launcher
    /// is left `weapon_pending` for the caller to pick from the spawn tick's input.
    pub fn spawn(&mut self, origin: Vec3, angles: QAngle, tick: u32, curtime: f32) {
        let score = self.score;
        let airshots = self.airshots;
        *self = Player::default();
        self.score = score;
        self.airshots = airshots;
        self.origin = origin;
        self.view_angles = angles;
        self.spawn_angles = angles;
        self.alive = true;
        self.spawn_tick = tick;
        self.game_code_moved = true;
        self.next_primary_attack = curtime + WEAPON_DEPLOY_TIME;
    }
}
