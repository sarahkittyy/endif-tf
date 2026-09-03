//! Fruit Ninja: an offline shooting gallery. The player stands on a small glass platform over
//! a void, facing a wooden wall, and soldiers are lobbed across the view on rocket-jump-like arcs
//! to be airshot. Rockets that hit a soldier directly count; near misses only fling them.
//!
//! Rounds are played on a difficulty: each is a `Preset` that fixes the wall distance, how fast
//! and how high the soldiers fly, how soon the next one follows and how many are up at once, so
//! a score on a difficulty means the same thing for everyone. Endless play has no score to
//! share and dials the same numbers directly: soldiers at a time, sideways speed, wall distance.
//! Gravity is always TF2's.
//!
//! Everything here is part of the rollback state (the practice session is a GGRS sync test, which
//! re-simulates every frame and compares checksums), so the live options cannot be plain
//! resources: they travel in the idle second player's input every tick, like the launcher
//! preference does for real players (that player is never alive, so its `buttons` are free). An
//! input without `IN_FRUIT_SETTINGS` (the zeroed padding GGRS delivers on the first frames)
//! leaves the settings as they are.

use crate::consts::*;
use crate::input::PlayerInput;
use crate::math::*;
use crate::player::{Player, Weapon};
use crate::rng::Rng;
use crate::trace::{Aabb, TraceEnv};
use crate::weapons::{apply_explosion_to_player, Rules};
use crate::world::SimEvent;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Difficulty {
    Easy,
    #[default]
    Medium,
    Hard,
}

/// What a difficulty plays like. The arcs are real rocket-jump arcs under `SV_GRAVITY`: a
/// soldier crossing sideways at `speed` rises `apex` above the arc's base line and comes back
/// down to it, so the distance covers itself; nothing flies below the base line but the
/// entry and exit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    /// Units from the player to the wall's face.
    pub wall_distance: f32,
    /// Sideways speed of a crossing, units per second (min, max).
    pub speed: (f32, f32),
    /// Height of an arc above `ARC_BASE` (min, max).
    pub apex: (f32, f32),
    /// Ticks after a soldier leaves or is hit before the next is thrown.
    pub gap_ticks: u32,
    /// Soldiers in the air at once (ragdolls do not count).
    pub concurrent: usize,
    /// Whether some soldiers pop straight up from below instead of crossing.
    pub pop_ups: bool,
}

impl Difficulty {
    pub const ALL: [Difficulty; 3] = [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard];

    pub fn preset(self) -> Preset {
        match self {
            // Close, slow, high: the soldier all but hangs at the apex.
            Difficulty::Easy => Preset { wall_distance: 500.0, speed: (220.0, 280.0), apex: (400.0, 600.0), gap_ticks: 67, concurrent: 1, pop_ups: false },
            Difficulty::Medium => Preset { wall_distance: 700.0, speed: (300.0, 380.0), apex: (300.0, 600.0), gap_ticks: 50, concurrent: 1, pop_ups: false },
            // Farther, faster and flatter, two at a time, and some come from below.
            Difficulty::Hard => Preset { wall_distance: 900.0, speed: (450.0, 550.0), apex: (250.0, 550.0), gap_ticks: 20, concurrent: 2, pop_ups: true },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Difficulty::Easy => "easy",
            Difficulty::Medium => "medium",
            Difficulty::Hard => "hard",
        }
    }

    pub fn ini_name(self) -> &'static str {
        self.label()
    }

    pub fn from_ini(s: &str) -> Option<Difficulty> {
        Difficulty::ALL.iter().copied().find(|d| d.label().eq_ignore_ascii_case(s.trim()))
    }

    fn from_bits(bits: u32) -> Difficulty {
        match bits & 0b11 {
            0 => Difficulty::Easy,
            2 => Difficulty::Hard,
            _ => Difficulty::Medium,
        }
    }

    fn bits(self) -> u32 {
        match self {
            Difficulty::Easy => 0,
            Difficulty::Medium => 1,
            Difficulty::Hard => 2,
        }
    }
}

/// Ranges of endless play's own numbers.
pub const SOLDIERS_MIN: u8 = 1;
pub const SOLDIERS_MAX: u8 = 4;
pub const SPEED_MIN: f32 = 150.0;
pub const SPEED_MAX: f32 = 800.0;
pub const WALL_DISTANCE_MIN: f32 = 300.0;
pub const WALL_DISTANCE_MAX: f32 = 1500.0;

/// Bits of the idle player's `buttons`, clear of every `in_buttons.h` value the simulation reads
/// and of the launcher bits: endless play's soldiers-at-a-time less one in bits 22-23,
/// `IN_FRUIT_ROUNDS` for rounds instead of endless play, the difficulty in bits 26-27,
/// `IN_FRUIT_SETTINGS` marking an input that carries settings at all, and `IN_FRUIT_RESET`,
/// held for as long as the reset countdown runs, which clears the arena and the stats while it
/// is. Endless play's sideways speed and wall distance ride in `pitch` and `yaw`.
const FRUIT_SOLDIERS_SHIFT: u32 = 22;
pub const IN_FRUIT_ROUNDS: u32 = 1 << 25;
const FRUIT_DIFFICULTY_SHIFT: u32 = 26;
pub const IN_FRUIT_SETTINGS: u32 = 1 << 28;
pub const IN_FRUIT_RESET: u32 = 1 << 31;

/// Soldiers in a round.
pub const ROUND_SIZE: u32 = 20;

/// The live options of the gallery.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FruitSettings {
    /// The preset rounds are played on.
    pub difficulty: Difficulty,
    /// Rounds of `ROUND_SIZE` soldiers with a result at the end, instead of endless play.
    pub rounds: bool,
    /// Endless play: soldiers in the air at once, their sideways speed and the wall distance.
    pub soldiers: u8,
    pub speed: f32,
    pub wall_distance: f32,
    /// The reset countdown is running: no soldiers, stats zeroed.
    pub reset: bool,
}

impl Default for FruitSettings {
    fn default() -> Self {
        FruitSettings { difficulty: Difficulty::Medium, rounds: false, soldiers: 1, speed: 350.0, wall_distance: 600.0, reset: false }
    }
}

impl Hash for FruitSettings {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.difficulty.hash(state);
        self.rounds.hash(state);
        self.soldiers.hash(state);
        self.speed.to_bits().hash(state);
        self.wall_distance.to_bits().hash(state);
        self.reset.hash(state);
    }
}

impl FruitSettings {
    /// The input the idle player sends every tick.
    pub fn to_input(&self) -> PlayerInput {
        let soldiers = self.soldiers.clamp(SOLDIERS_MIN, SOLDIERS_MAX) as u32 - 1;
        let mut buttons = IN_FRUIT_SETTINGS | (self.difficulty.bits() << FRUIT_DIFFICULTY_SHIFT) | (soldiers << FRUIT_SOLDIERS_SHIFT);
        if self.rounds {
            buttons |= IN_FRUIT_ROUNDS;
        }
        if self.reset {
            buttons |= IN_FRUIT_RESET;
        }
        PlayerInput { buttons, pitch: self.speed, yaw: self.wall_distance }
    }

    /// The settings an input carries, clamped to their ranges; `None` for an input without any.
    pub fn from_input(input: &PlayerInput) -> Option<FruitSettings> {
        if input.buttons & IN_FRUIT_SETTINGS == 0 {
            return None;
        }
        let defaults = FruitSettings::default();
        Some(FruitSettings {
            difficulty: Difficulty::from_bits(input.buttons >> FRUIT_DIFFICULTY_SHIFT),
            rounds: input.buttons & IN_FRUIT_ROUNDS != 0,
            soldiers: ((input.buttons >> FRUIT_SOLDIERS_SHIFT) & 0b11) as u8 + 1,
            speed: if input.pitch.is_finite() { clamp(input.pitch, SPEED_MIN, SPEED_MAX) } else { defaults.speed },
            wall_distance: if input.yaw.is_finite() { clamp(input.yaw, WALL_DISTANCE_MIN, WALL_DISTANCE_MAX) } else { defaults.wall_distance },
            reset: input.buttons & IN_FRUIT_RESET != 0,
        })
    }

    /// What is being played: the difficulty's preset in rounds, the dialled numbers in endless
    /// play (arcs of middling height, the next soldier half a second after one goes).
    pub fn preset(&self) -> Preset {
        if self.rounds {
            self.difficulty.preset()
        } else {
            Preset {
                wall_distance: self.wall_distance,
                speed: (self.speed * 0.9, self.speed * 1.1),
                apex: (250.0, 600.0),
                gap_ticks: 33,
                concurrent: self.soldiers as usize,
                pop_ups: false,
            }
        }
    }
}

// ---- arena ------------------------------------------------------------------------------------

/// Half extent of the square glass platform the player stands on.
pub const PLATFORM_HALF: f32 = 96.0;
/// Thickness of the platform's collision slab (the glass and the metal plate under it).
pub const PLATFORM_THICKNESS: f32 = 16.0;
/// A player whose origin falls below this has fallen off and dies.
pub const FALL_DEATH_Z: f32 = -700.0;
/// Depth of the wooden wall's brush behind its face.
pub const WALL_THICKNESS: f32 = 256.0;
/// Height above the platform the arcs rise from and come back to: comfortably above the eye
/// line, where an airshot target belongs.
pub const ARC_BASE: f32 = 200.0;

/// Where the soldiers fly: the vertical plane a little in front of the wall and the box around
/// it that the wall covers and that a soldier leaves the gallery at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    /// Distance of the plane from the spawn.
    pub x: f32,
    /// Sideways reach of the widest arc, plus margin.
    pub y_half: f32,
    /// Bottom of the view at that distance: soldiers enter just under it and are gone below it.
    pub z_lo: f32,
    pub z_min: f32,
    /// Top of the highest arc, plus margin.
    pub z_max: f32,
}

/// How far below the view's bottom a soldier starts, so it rises into view.
const ENTRY_MARGIN: f32 = 60.0;
/// Margin around the arcs the wall covers and a soldier gets before it is removed.
const EXIT_MARGIN: f32 = 300.0;

/// Seconds an arc of height `h` spends above its base line.
fn hang_time(h: f32) -> f32 {
    2.0 * sqrtf(2.0 * h / SV_GRAVITY)
}

pub fn region(p: &Preset) -> Region {
    let gap = clamp(0.3 * p.wall_distance, 30.0, 300.0);
    let x = p.wall_distance - gap;
    // The widest crossing, shifted off centre by up to a quarter of itself, plus the run-in
    // from below the view.
    let widest = p.speed.1 * hang_time(p.apex.1);
    let z_lo = SOLDIER_VIEW.z - 0.75 * x;
    Region { x, y_half: 0.75 * widest + p.speed.1 * 0.8 + EXIT_MARGIN, z_lo, z_min: z_lo - EXIT_MARGIN, z_max: ARC_BASE + p.apex.1 + EXIT_MARGIN }
}

/// The wall's brush for a preset: its face is at the preset's distance and it covers the
/// soldiers' region.
pub fn wall_brush(p: &Preset) -> Aabb {
    let d = p.wall_distance;
    let r = region(p);
    Aabb::new(Vec3::new(d, -r.y_half, r.z_min), Vec3::new(d + WALL_THICKNESS, r.y_half, r.z_max))
}

/// The world the players and rockets collide with this tick: the arena's static brushes plus the
/// wall for what is being played.
pub fn world_with_wall(brushes: &[Aabb], p: &Preset) -> Vec<Aabb> {
    let mut world = Vec::with_capacity(brushes.len() + 1);
    world.extend_from_slice(brushes);
    world.push(wall_brush(p));
    world
}

// ---- targets ----------------------------------------------------------------------------------

/// Most soldiers in the air at once, ragdolls included; a spawn waits while the gallery is full.
pub const MAX_TARGETS: usize = 24;
/// Soldiers still around after this long are removed whatever they are doing (a splash can send
/// one on a very long flight).
const TARGET_MAX_AGE: f32 = 12.0;
/// Ticks between the end of a reset and the first new soldier.
const RESET_SPAWN_DELAY_TICKS: u32 = 20;
/// Ticks until the first soldier of a fresh match.
const FIRST_SPAWN_TICK: u32 = 67;
/// Two soldiers are never thrown closer together than this, gap or no gap.
const MIN_SPAWN_GAP_TICKS: u32 = 27;
/// The `victim` index handed to the damage code for a target: only compared against the
/// attacker's index, never used as a player slot.
const TARGET_VICTIM_INDEX: u8 = 200;
/// Every this many soldiers hit in a row, the chain is called out (`SimEvent::Chain`).
pub const CHAIN_STEP: u32 = 5;
/// The most a ragdoll may drift toward the player, units per second.
const RAGDOLL_DRIFT: f32 = 40.0;

/// A flying soldier. It has no collision with the world: it falls under `SV_GRAVITY` along its
/// arc, and only rockets and explosions act on it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub id: u32,
    pub origin: Vec3,
    pub velocity: Vec3,
    pub spawn_tick: u32,
    /// The launcher it carries (looks only), stock or The Original, a coin toss each.
    pub weapon: Weapon,
    /// Hit directly by a rocket: counted, no longer solid for rockets (so it cannot soak up a
    /// second one), still pushed around by explosions, and flying on as a ragdoll.
    pub hit: bool,
}

impl Hash for Target {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.origin.hash(state);
        self.velocity.hash(state);
        self.spawn_tick.hash(state);
        self.weapon.hash(state);
        self.hit.hash(state);
    }
}

impl Target {
    /// The soldier's standing hull.
    pub fn aabb(&self) -> Aabb {
        Aabb::new(self.origin + VEC_HULL_MIN, self.origin + VEC_HULL_MAX)
    }
}

/// The gallery's rollback state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FruitState {
    /// The options as last received from the idle player's input.
    pub settings: FruitSettings,
    pub targets: Vec<Target>,
    pub next_id: u32,
    pub next_spawn_tick: u32,
    /// Soldiers in the air (not ragdolls) after the last tick, to notice one leaving.
    pub live: u32,
    /// Soldiers hit directly since the last reset.
    pub hits: u32,
    /// Rockets fired since the last reset.
    pub shots: u32,
    /// Soldiers hit in a row without one getting away, and the best such run.
    pub chain: u32,
    pub best_chain: u32,
    /// Soldiers thrown this round (rounds only).
    pub thrown: u32,
    /// The round's soldiers are all thrown and gone; nothing more until a reset.
    pub round_over: bool,
    /// The reset flag was set on the previous tick too (its first tick zeroes the stats).
    pub resetting: bool,
}

impl Default for FruitState {
    fn default() -> Self {
        FruitState {
            settings: FruitSettings::default(),
            targets: Vec::new(),
            next_id: 1,
            next_spawn_tick: FIRST_SPAWN_TICK,
            live: 0,
            hits: 0,
            shots: 0,
            chain: 0,
            best_chain: 0,
            thrown: 0,
            round_over: false,
            resetting: false,
        }
    }
}

impl Hash for FruitState {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.settings.hash(state);
        self.targets.hash(state);
        self.next_id.hash(state);
        self.next_spawn_tick.hash(state);
        self.live.hash(state);
        self.hits.hash(state);
        self.shots.hash(state);
        self.chain.hash(state);
        self.best_chain.hash(state);
        self.thrown.hash(state);
        self.round_over.hash(state);
        self.resetting.hash(state);
    }
}

impl FruitState {
    /// Hit percentage, 0..100, for the score display.
    pub fn accuracy(&self) -> u32 {
        (self.hits * 100 + self.shots / 2).checked_div(self.shots).unwrap_or(0)
    }

    /// Takes the options out of the idle player's input for this tick. A switch between endless
    /// play and rounds, or a new difficulty for rounds, starts over; endless play's numbers just
    /// apply (soldiers already in the air fly on as they were).
    pub fn apply_input(&mut self, input: &PlayerInput, tick: u32) {
        if let Some(s) = FruitSettings::from_input(input) {
            if s.rounds != self.settings.rounds || (s.rounds && s.difficulty != self.settings.difficulty) {
                self.start_over(tick);
            }
            self.settings = s;
        }
    }

    /// Clears the gallery and the stats, with the first new soldier a moment away.
    fn start_over(&mut self, tick: u32) {
        self.targets.clear();
        self.hits = 0;
        self.shots = 0;
        self.chain = 0;
        self.best_chain = 0;
        self.thrown = 0;
        self.round_over = false;
        self.next_spawn_tick = tick + RESET_SPAWN_DELAY_TICKS;
    }

    /// Once per tick: the reset, the soldiers' flight, the ones that left (a miss for the
    /// chain), the round's end, and the next throw.
    pub fn think(&mut self, tick: u32, rng: &mut Rng, events: &mut Vec<SimEvent>) {
        let p = self.settings.preset();
        let reset = self.settings.reset;
        if reset {
            if !self.resetting {
                self.start_over(tick);
            }
            self.targets.clear();
            self.next_spawn_tick = tick + RESET_SPAWN_DELAY_TICKS;
        }
        self.resetting = reset;

        for t in &mut self.targets {
            t.velocity.z -= SV_GRAVITY * TICK_INTERVAL;
            t.origin += t.velocity * TICK_INTERVAL;
        }
        let r = region(&p);
        let mut escaped = false;
        self.targets.retain(|t| {
            let age = (tick - t.spawn_tick) as f32 * TICK_INTERVAL;
            let keep = t.origin.z > r.z_min && fabsf(t.origin.y) < r.y_half && t.origin.x > -150.0 && t.origin.x < p.wall_distance + 40.0 && age < TARGET_MAX_AGE;
            if !keep && !t.hit {
                escaped = true;
            }
            keep
        });
        if escaped {
            self.chain = 0;
        }
        // One fewer in the air (gone or hit): the next follows after the preset's gap.
        let live = self.targets.iter().filter(|t| !t.hit).count() as u32;
        if live < self.live {
            self.next_spawn_tick = self.next_spawn_tick.max(tick + p.gap_ticks);
        }
        self.live = live;

        let rounds = self.settings.rounds;
        if rounds && !self.round_over && self.thrown >= ROUND_SIZE && live == 0 {
            self.round_over = true;
            events.push(SimEvent::RoundOver { hits: self.hits, shots: self.shots, best_chain: self.best_chain });
        }
        let round_full = rounds && self.thrown >= ROUND_SIZE;
        let may_throw = !reset && !round_full && live < p.concurrent as u32 && self.targets.len() < MAX_TARGETS && tick >= self.next_spawn_tick;
        if may_throw {
            self.spawn(tick, &p, &r, rng);
            self.thrown += 1;
            self.next_spawn_tick = tick + p.gap_ticks.max(MIN_SPAWN_GAP_TICKS);
        }
    }

    /// Throws one soldier under TF2 gravity: a crossing from one side on an arc that rises `apex`
    /// above `ARC_BASE` and returns to it (started a little below the view so it comes up into
    /// it), or, on presets that have them, a pop-up from below to the same heights.
    fn spawn(&mut self, tick: u32, p: &Preset, r: &Region, rng: &mut Rng) {
        let g = SV_GRAVITY;
        let apex = rng.random_float(p.apex.0, p.apex.1);
        let x = r.x * rng.random_float(0.94, 1.06);
        let z0 = r.z_lo - ENTRY_MARGIN;
        // Rising from `z0` to peak `apex` above the base line.
        let vz0 = sqrtf(2.0 * g * (ARC_BASE + apex - z0));
        let pop_up = p.pop_ups && rng.random_int(0, 3) == 0;
        let (origin, velocity) = if pop_up {
            let y = rng.random_float(-0.4, 0.4) * r.y_half;
            (Vec3::new(x, y, z0), Vec3::new(0.0, rng.random_float(-120.0, 120.0), vz0))
        } else {
            let speed = rng.random_float(p.speed.0, p.speed.1);
            let dir = if rng.random_int(0, 1) == 0 { 1.0 } else { -1.0 };
            // The part of the arc above the base line spans `span`, centred off the middle by up
            // to a quarter of itself; the run-in from `z0` up to the base line comes before it.
            let span = speed * hang_time(apex);
            let centre = rng.random_float(-0.25, 0.25) * span;
            let run_in = (vz0 - sqrtf(2.0 * g * apex)) / g;
            let y0 = centre - dir * (span / 2.0 + speed * run_in);
            (Vec3::new(x, y0, z0), Vec3::new(0.0, dir * speed, vz0))
        };
        let weapon = if rng.random_int(0, 1) == 0 { Weapon::Stock } else { Weapon::Original };
        self.targets.push(Target { id: self.next_id, origin, velocity, spawn_tick: tick, weapon, hit: false });
        self.next_id += 1;
    }

    /// The hulls rockets can hit: every soldier not hit yet.
    pub fn solid_targets(&self) -> Vec<(u32, Aabb)> {
        self.targets.iter().filter(|t| !t.hit).map(|t| (t.id, t.aabb())).collect()
    }

    /// `RadiusDamage` for the soldiers: the TF2 damage and knockback pipeline is run on each one
    /// within reach as if it were a standing player, and the push is kept, except that a ragdoll
    /// is never pushed into the wall: away from the player it stops, toward the player it drifts
    /// at most a little. The one the rocket hit (`direct`) is counted and extends the chain,
    /// which is announced every `CHAIN_STEP`.
    #[allow(clippy::too_many_arguments)]
    pub fn explode(&mut self, src: Vec3, direct: Option<u32>, attacker: &Player, env_world: &TraceEnv, rules: &Rules, rng: &mut Rng, events: &mut Vec<SimEvent>) {
        for t in &mut self.targets {
            let mut body = Player { origin: t.origin, velocity: t.velocity, alive: true, ..Player::default() };
            let res = apply_explosion_to_player(
                &mut body,
                TARGET_VICTIM_INDEX,
                attacker,
                0,
                src,
                TF_ROCKET_RADIUS,
                ROCKET_DAMAGE,
                direct == Some(t.id),
                env_world,
                rules,
                rng,
            );
            let Some(h) = res else { continue };
            t.velocity = body.velocity;
            if h.direct && !t.hit {
                t.hit = true;
                self.hits += 1;
                self.chain += 1;
                self.best_chain = self.best_chain.max(self.chain);
                if self.chain.is_multiple_of(CHAIN_STEP) {
                    events.push(SimEvent::Chain { chain: self.chain });
                }
            }
            if t.hit {
                t.velocity.x = clamp(t.velocity.x, -RAGDOLL_DRIFT, 0.0);
            }
            events.push(SimEvent::TargetHit { id: t.id, origin: t.origin, direct: h.direct });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_the_input() {
        let s = FruitSettings { difficulty: Difficulty::Hard, rounds: true, soldiers: 3, speed: 420.0, wall_distance: 950.0, reset: true };
        assert_eq!(FruitSettings::from_input(&s.to_input()), Some(s));
        assert_eq!(FruitSettings::from_input(&PlayerInput::default()), None);
        let wild = FruitSettings { soldiers: 9, speed: 1e9, wall_distance: -1.0, ..FruitSettings::default() };
        let got = FruitSettings::from_input(&wild.to_input()).unwrap();
        assert_eq!((got.soldiers, got.speed, got.wall_distance), (SOLDIERS_MAX, SPEED_MAX, WALL_DISTANCE_MIN));
    }

    /// Every arc stays inside the wall, above the arc base line while in the middle of the
    /// view, and is over within a few seconds; rounds stop at `ROUND_SIZE` soldiers.
    #[test]
    fn arcs_fit_the_region_and_rounds_end() {
        let mut rng = Rng::new(7);
        for d in Difficulty::ALL {
            let mut f = FruitState { settings: FruitSettings { difficulty: d, rounds: true, ..FruitSettings::default() }, ..FruitState::default() };
            let p = d.preset();
            let r = region(&p);
            let mut events = Vec::new();
            let mut longest = 0.0f32;
            for tick in 0..12000 {
                f.think(tick, &mut rng, &mut events);
                for t in &f.targets {
                    longest = longest.max((tick - t.spawn_tick) as f32 * TICK_INTERVAL);
                    assert!(t.origin.z < r.z_max && fabsf(t.origin.y) < r.y_half, "{d:?}: a soldier is outside the wall at {:?}", t.origin);
                    assert!(t.origin.x < p.wall_distance, "{d:?}: a soldier is in the wall");
                    // A crossing (pop-ups rise from the bottom of the view on purpose) is above
                    // the base line by the time it reaches the middle.
                    if fabsf(t.origin.y) < 0.1 * r.y_half && t.velocity.z > 0.0 && fabsf(t.velocity.y) > 150.0 {
                        assert!(t.origin.z > ARC_BASE - 30.0, "{d:?}: rising through the middle at {} units", t.origin.z);
                    }
                }
            }
            assert_eq!(f.thrown, ROUND_SIZE, "{d:?}");
            assert!(f.round_over);
            assert!(events.iter().any(|e| matches!(e, SimEvent::RoundOver { .. })));
            assert!(longest < 4.5, "{d:?}: a soldier stayed {longest} s");
        }
    }

    #[test]
    fn hits_run_the_chain_and_escapes_break_it() {
        let mut f = FruitState::default();
        f.hits = 3;
        f.chain = 3;
        f.best_chain = 3;
        f.targets.push(Target { id: 9, origin: Vec3::new(0.0, 1e6, 0.0), velocity: Vec3::ZERO, spawn_tick: 0, weapon: Weapon::Stock, hit: false });
        let mut rng = Rng::new(1);
        let mut events = Vec::new();
        f.think(100, &mut rng, &mut events);
        assert_eq!((f.chain, f.best_chain, f.hits), (0, 3, 3));
    }
}
