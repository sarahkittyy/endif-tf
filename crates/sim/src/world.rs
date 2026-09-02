//! The complete rollback-able game state and the fixed-tick stepper.

use crate::arena::Arena;
use crate::consts::*;
use crate::input::*;
use crate::math::*;
use crate::movement::run_player_move;
use crate::player::*;
use crate::rng::Rng;
use crate::trace::*;
use crate::weapons::*;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

pub const NUM_PLAYERS: usize = 2;

/// Things that happened during a tick, for the presentation layer (sounds, particles, HUD).
/// Events are regenerated every tick and are not needed for determinism.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SimEvent {
    RocketFired { shooter: u8, rocket_id: u32, origin: Vec3, velocity: Vec3 },
    Explosion { rocket_id: u32, origin: Vec3, normal: Vec3, hit_player: Option<u8> },
    /// `height` is the victim's height above the ground below; `distance` is how far the rocket
    /// flew from the muzzle to the explosion.
    PlayerHit { victim: u8, attacker: u8, damage: f32, direct: bool, airshot_kill: bool, height: f32, distance: f32 },
    Killed { victim: u8, attacker: u8 },
    Respawn { player: u8, origin: Vec3 },
    RoundWon { winner: u8, score: [i32; 2] },
    Landed { player: u8, fall_velocity: f32 },
    Jumped { player: u8 },
}

/// Phase of the arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Phase {
    /// Waiting for both players (first spawn happens on the first tick).
    Warmup,
    Fighting,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimState {
    pub tick: u32,
    pub players: [Player; NUM_PLAYERS],
    pub rockets: Vec<Rocket>,
    pub next_rocket_id: u32,
    pub rng: Rng,
    pub rules: Rules,
    pub phase: Phase,
    /// Number of completed rounds (frag limit reached).
    pub rounds_played: u32,
    #[serde(skip)]
    pub events: Vec<SimEvent>,
}

impl Hash for SimState {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.tick.hash(state);
        for p in &self.players {
            p.hash(state);
        }
        self.rockets.hash(state);
        self.next_rocket_id.hash(state);
        self.rng.hash(state);
        self.rules.hash(state);
        self.phase.hash(state);
        self.rounds_played.hash(state);
    }
}

/// `UTIL_DropToFloor`-style placement: trace the player's hull down from 16 units above the
/// requested origin and return where it comes to rest.
pub fn drop_to_floor(brushes: &[Aabb], player: &Player) -> Vec3 {
    let env = TraceEnv::world_only(brushes);
    let start = player.origin + Vec3::new(0.0, 0.0, 16.0);
    let end = player.origin - Vec3::new(0.0, 0.0, 256.0);
    let tr = trace_hull(&env, start, end, player.hull_mins(), player.hull_maxs());
    if tr.startsolid || tr.allsolid { player.origin } else { tr.endpos }
}

impl SimState {
    pub fn new(seed: u64, rules: Rules) -> Self {
        SimState {
            tick: 0,
            players: [Player::default(), Player::default()],
            rockets: Vec::new(),
            next_rocket_id: 1,
            rng: Rng::new(seed),
            rules,
            phase: Phase::Warmup,
            rounds_played: 0,
            events: Vec::new(),
        }
    }

    /// `gpGlobals->curtime` for the tick being simulated.
    pub fn curtime(&self) -> f32 {
        self.tick as f32 * TICK_INTERVAL
    }

    /// A stable 64-bit checksum of the deterministic state (for desync detection). Identical on
    /// every platform for the same state: see [`crate::checksum::DetHasher`].
    pub fn checksum(&self) -> u64 {
        let mut h = crate::checksum::DetHasher::new();
        self.hash(&mut h);
        h.finish()
    }

    fn other(idx: usize) -> usize {
        1 - idx
    }

    fn solid_players_for(&self, idx: usize) -> Vec<(u8, Aabb)> {
        let o = Self::other(idx);
        if self.players[o].alive { vec![(o as u8, self.players[o].world_aabb())] } else { Vec::new() }
    }

    /// Picks a spawn point at random, preferring ones at least `MGE_MIN_SPAWN_DIST` from the opponent.
    fn choose_spawn(&mut self, arena: &Arena, idx: usize) -> usize {
        let n = arena.spawns.len();
        let o = Self::other(idx);
        let opponent = if self.players[o].alive { Some(self.players[o].origin) } else { None };
        let mut choice = self.rng.random_int(0, n as i32 - 1) as usize;
        for _ in 0..8 {
            match opponent {
                Some(op) if arena.spawns[choice].origin.dist_to(op) < MGE_MIN_SPAWN_DIST => {
                    choice = self.rng.random_int(0, n as i32 - 1) as usize;
                }
                _ => break,
            }
        }
        choice
    }

    fn respawn(&mut self, arena: &Arena, idx: usize) {
        let s = self.choose_spawn(arena, idx);
        let spawn = arena.spawns[s];
        let tick = self.tick;
        let curtime = self.curtime();
        // `Player::spawn` resets the player, so read the death flag first.
        let high = std::mem::take(&mut self.players[idx].respawn_high);
        self.players[idx].spawn(spawn.origin, spawn.angles, tick, curtime);
        // UTIL_DropToFloor: spawn points sit on the floor plane, but a hull touching a surface
        // counts as inside it, so trace down from slightly above and rest DIST_EPSILON over it.
        let mut origin = drop_to_floor(&arena.brushes, &self.players[idx]);
        if high {
            // House rule: after a death you come back high above the spawn, airborne.
            origin.z += self.rules.respawn_height as f32;
        }
        self.players[idx].origin = origin;
        self.events.push(SimEvent::Respawn { player: idx as u8, origin });
    }

    fn kill(&mut self, victim: usize, attacker: usize, arena: &Arena) {
        if !self.players[victim].alive {
            return;
        }
        self.players[victim].alive = false;
        self.players[victim].respawn_tick = self.tick + self.rules.respawn_delay_ticks;
        self.players[victim].respawn_high = true;
        self.players[victim].pending_boosts.clear();
        self.events.push(SimEvent::Killed { victim: victim as u8, attacker: attacker as u8 });

        if attacker != victim {
            self.players[attacker].score += 1;
            // RegenKiller: the killer is regenerated after each kill.
            self.players[attacker].health = self.players[attacker].max_health;
            self.players[attacker].clip = ROCKET_CLIP_SIZE;

            if self.players[attacker].score >= self.rules.frag_limit {
                let score = [self.players[0].score, self.players[1].score];
                self.events.push(SimEvent::RoundWon { winner: attacker as u8, score });
                self.rounds_played += 1;
                for p in &mut self.players {
                    p.score = 0;
                }
                // Both players are reset for the next round, on the floor.
                self.players[attacker].alive = false;
                self.players[attacker].respawn_tick = self.tick + self.rules.respawn_delay_ticks;
                self.players[attacker].respawn_high = false;
                self.players[victim].respawn_high = false;
                let _ = arena;
            }
        }
    }

    /// Advance the simulation by one tick using the given inputs.
    pub fn step(&mut self, arena: &Arena, inputs: [PlayerInput; NUM_PLAYERS]) {
        self.events.clear();
        let tick = self.tick;
        let curtime = self.curtime();

        // First tick: spawn everyone.
        if self.phase == Phase::Warmup {
            self.phase = Phase::Fighting;
            for i in 0..NUM_PLAYERS {
                self.respawn(arena, i);
            }
        }

        // Respawns.
        for i in 0..NUM_PLAYERS {
            if !self.players[i].alive && self.players[i].respawn_tick <= tick {
                self.respawn(arena, i);
            }
        }

        // MGE endif BoostVectors timers (SourceMod timers run at the start of the frame).
        for i in 0..NUM_PLAYERS {
            let p = &mut self.players[i];
            if p.pending_boosts.iter().any(|&t| t <= tick) {
                let fire_count = p.pending_boosts.iter().filter(|&&t| t <= tick).count();
                p.pending_boosts.retain(|&t| t > tick);
                if p.alive && self.rules.endif_boost {
                    for _ in 0..fire_count {
                        p.velocity = boost_vectors(p.velocity);
                    }
                }
            }
        }

        // Player commands: movement then weapon, in slot order.
        for i in 0..NUM_PLAYERS {
            if !self.players[i].alive {
                continue;
            }
            let input = inputs[i];
            let solid = self.solid_players_for(i);
            let env = TraceEnv { world: &arena.brushes, players: &solid };

            let was_on_ground = self.players[i].ground.is_some();
            let was_jumping = self.players[i].jumping;
            let fall_velocity = self.players[i].fall_velocity;
            run_player_move(&mut self.players[i], &input, env, curtime);
            let p = &self.players[i];
            if !was_on_ground && p.ground.is_some() {
                self.events.push(SimEvent::Landed { player: i as u8, fall_velocity });
            }
            if !was_jumping && p.jumping {
                self.events.push(SimEvent::Jumped { player: i as u8 });
            }

            self.weapon_think(i, &input, arena);
        }

        // Rockets (including the ones fired this tick).
        self.move_rockets(arena);

        self.tick += 1;
    }

    /// `CTFWeaponBaseGun::ItemPostFrame` / `PrimaryAttack` with the MGE infinite-ammo crutch.
    fn weapon_think(&mut self, idx: usize, input: &PlayerInput, arena: &Arena) {
        let curtime = self.curtime();
        let tick = self.tick;

        // Infinite ammo crutch: clip refilled 0.4 s after pressing attack.
        if self.players[idx].clip_refill_time >= 0.0 && curtime >= self.players[idx].clip_refill_time {
            self.players[idx].clip = ROCKET_CLIP_SIZE;
            self.players[idx].clip_refill_time = -1.0;
        }

        if !input.pressed(IN_ATTACK) {
            return;
        }
        if self.players[idx].clip_refill_time < 0.0 {
            self.players[idx].clip_refill_time = curtime + MGE_INFAMMO_REFILL_DELAY;
        }
        if self.players[idx].next_primary_attack > curtime || self.players[idx].clip <= 0 {
            return;
        }

        // Rockets live in the rocket world (outer walls), so aim and spawn against that.
        let solid = self.solid_players_for(idx);
        let ctx = FireContext {
            env_all: TraceEnv { world: &arena.rocket_brushes, players: &solid },
            env_world: TraceEnv::world_only(&arena.rocket_brushes),
        };
        let id = self.next_rocket_id;
        self.next_rocket_id += 1;
        let rocket = fire_rocket(&self.players[idx], idx as u8, id, tick, &ctx);
        self.events.push(SimEvent::RocketFired {
            shooter: idx as u8,
            rocket_id: id,
            origin: rocket.origin,
            velocity: rocket.velocity,
        });
        self.rockets.push(rocket);

        let p = &mut self.players[idx];
        p.clip -= 1;
        p.next_primary_attack = curtime + ROCKET_FIRE_DELAY;
    }

    /// `CBaseEntity::PhysicsToss` for each rocket, exploding on contact.
    fn move_rockets(&mut self, arena: &Arena) {
        let tick = self.tick;
        let mut i = 0;
        while i < self.rockets.len() {
            let r = self.rockets[i];
            let owner = r.owner as usize;

            // Lifetime cap for rockets that fly straight up forever.
            if (tick - r.spawn_tick) as f32 * TICK_INTERVAL > ROCKET_MAX_LIFETIME {
                self.rockets.swap_remove(i);
                continue;
            }

            // Rockets pass through the player-collision walls and explode on the outer walls.
            let solid = self.solid_players_for(owner);
            let env = TraceEnv { world: &arena.rocket_brushes, players: &solid };
            let end = r.origin + r.velocity * TICK_INTERVAL;
            let tr = trace_line(&env, r.origin, end);

            if tr.fraction < 1.0 || tr.startsolid {
                let hit_player = match tr.ent {
                    Some(HitEnt::Player(p)) => Some(p as usize),
                    _ => None,
                };
                self.explode(i, tr, hit_player, arena);
                // explode removed the rocket; do not advance.
                continue;
            }

            self.rockets[i].origin = end;
            i += 1;
        }
    }

    /// `CTFBaseRocket::Explode` + `CTFGameRules::RadiusDamage`.
    fn explode(&mut self, rocket_index: usize, tr: Trace, hit_player: Option<usize>, arena: &Arena) {
        let rocket = self.rockets.swap_remove(rocket_index);
        let attacker = rocket.owner as usize;

        // Pull out a bit.
        let src = if tr.fraction != 1.0 { tr.endpos + tr.normal * 1.0 } else { rocket.origin };
        let distance = (src - rocket.start).length();

        self.events.push(SimEvent::Explosion {
            rocket_id: rocket.id,
            origin: src,
            normal: tr.normal,
            hit_player: hit_player.map(|p| p as u8),
        });

        // Blast line of sight must not be blocked by the invisible player walls.
        let env_world = TraceEnv::world_only(&arena.rocket_brushes);
        let attacker_snapshot = self.players[attacker].clone();

        // Everyone except the attacker, within TF_ROCKET_RADIUS.
        let mut hits: Vec<HitResult> = Vec::new();
        for v in 0..NUM_PLAYERS {
            if v == attacker {
                continue;
            }
            let direct = hit_player == Some(v);
            let res = apply_explosion_to_player(
                &mut self.players[v],
                v as u8,
                &attacker_snapshot,
                attacker as u8,
                src,
                TF_ROCKET_RADIUS,
                ROCKET_DAMAGE,
                direct,
                &env_world,
                &self.rules,
                &mut self.rng,
            );
            if let Some(h) = res {
                hits.push(h);
            }
        }

        // The attacker, with the rocket-jump radius and base damage.
        {
            let res = apply_explosion_to_player(
                &mut self.players[attacker],
                attacker as u8,
                &attacker_snapshot,
                attacker as u8,
                src,
                TF_ROCKET_RADIUS_FOR_RJS,
                ROCKET_DAMAGE,
                false,
                &env_world,
                &self.rules,
                &mut self.rng,
            );
            if let Some(h) = res {
                hits.push(h);
            }
        }

        // MGEMod Event_PlayerHurt bookkeeping.
        let tick = self.tick;
        for h in hits {
            let v = h.victim as usize;
            self.events.push(SimEvent::PlayerHit {
                victim: h.victim,
                attacker: h.attacker,
                damage: h.damage,
                direct: h.direct,
                airshot_kill: h.airshot_kill,
                height: h.height_above_ground,
                distance,
            });

            if h.victim != h.attacker {
                // Attacker is holding a rocket launcher → BoostVectors timer.
                self.players[v].pending_boosts.push(tick + MGE_ENDIF_BOOST_DELAY_TICKS);
            }

            if h.airshot_kill {
                self.players[v].airshots += 0; // victim stat unchanged
                self.players[h.attacker as usize].airshots += 1;
                self.kill(v, h.attacker as usize, arena);
            } else if self.players[v].health <= 0 {
                // Lethal raw damage (cannot happen without crits, kept for completeness).
                self.kill(v, h.attacker as usize, arena);
            } else {
                // Every non-lethal hit heals to full in endif.
                self.players[v].health = self.players[v].max_health;
            }
        }
    }
}
