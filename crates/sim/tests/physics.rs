//! Scenario tests that check the port against well-known TF2 numbers.

use endif_sim::*;

fn fresh() -> (Arena, SimState) {
    let arena = Arena::classic_square();
    let mut sim = SimState::new(1234, Rules::default());
    // First tick spawns both players.
    sim.step(&arena, [PlayerInput::default(); 2]);
    (arena, sim)
}

fn idle(yaw: f32) -> PlayerInput {
    PlayerInput { buttons: 0, pitch: 0.0, yaw }
}

fn with(buttons: u32, pitch: f32, yaw: f32) -> PlayerInput {
    PlayerInput { buttons, pitch, yaw }
}

/// Teleport helper for scenarios.
fn place(sim: &mut SimState, idx: usize, origin: Vec3, yaw: f32) {
    let p = &mut sim.players[idx];
    // Rest slightly above the floor; CategorizePosition snaps the player down on the next move.
    p.origin = origin + Vec3::new(0.0, 0.0, 1.0);
    p.velocity = Vec3::ZERO;
    p.view_angles = QAngle::new(0.0, yaw, 0.0);
    p.game_code_moved = true;
}

fn settle(sim: &mut SimState, arena: &Arena, ticks: u32) {
    for _ in 0..ticks {
        sim.step(arena, [idle(0.0), idle(0.0)]);
    }
}

#[test]
fn spawns_land_on_floor() {
    let (arena, mut sim) = fresh();
    settle(&mut sim, &arena, 5);
    for p in &sim.players {
        assert!(p.alive);
        assert!(p.on_ground(), "player should be standing on the floor");
        assert!(p.origin.z >= 0.0 && p.origin.z < 0.1, "z = {}", p.origin.z);
    }
}

#[test]
fn spawns_look_at_the_arena_centre() {
    let (arena, sim) = fresh();
    for p in &sim.players {
        let (forward, _, _) = endif_sim::math::angle_vectors(p.spawn_angles);
        let to_centre = (arena.centre() - p.eye_position()).normalized();
        assert!(forward.dot(to_centre) > 0.999, "spawn at {:?} looks {:?}, centre is {:?}", p.origin, forward, to_centre);
        assert!(p.spawn_angles.pitch > 0.0 && p.spawn_angles.pitch < MAX_PITCH, "pitch = {}", p.spawn_angles.pitch);
    }
}

#[test]
fn ground_speed_caps_at_240() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(-300.0, 0.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::new(300.0, 300.0, 0.0), 0.0);
    settle(&mut sim, &arena, 3);
    let mut max_speed = 0.0f32;
    for _ in 0..66 {
        sim.step(&arena, [with(IN_FORWARD, 0.0, 0.0), idle(0.0)]);
        max_speed = max_speed.max(sim.players[0].velocity.length_2d());
    }
    assert!((max_speed - SOLDIER_MAX_SPEED).abs() < 0.01, "max speed {max_speed}");
    // Diagonal input is normalised, not faster.
    let mut diag = 0.0f32;
    for _ in 0..66 {
        sim.step(&arena, [with(IN_FORWARD | IN_MOVERIGHT, 0.0, 0.0), idle(0.0)]);
        diag = diag.max(sim.players[0].velocity.length_2d());
    }
    assert!((diag - SOLDIER_MAX_SPEED).abs() < 0.01, "diag speed {diag}");
}

#[test]
fn backwards_speed_is_clamped_to_90_percent() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(0.0, 0.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::new(300.0, 300.0, 0.0), 0.0);
    settle(&mut sim, &arena, 3);
    let mut max_speed = 0.0f32;
    for _ in 0..66 {
        sim.step(&arena, [with(IN_BACK, 0.0, 0.0), idle(0.0)]);
        max_speed = max_speed.max(sim.players[0].velocity.length_2d());
    }
    assert!((max_speed - SOLDIER_MAX_SPEED * 0.9).abs() < 0.5, "back speed {max_speed}");
}

#[test]
fn standing_jump_apex_matches_tf2() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(0.0, 0.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::new(300.0, 300.0, 0.0), 0.0);
    settle(&mut sim, &arena, 3);
    let mut apex = 0.0f32;
    let mut first_vz = 0.0f32;
    for i in 0..80 {
        // Tap jump on the first tick only (holding it is fine too: no auto-jump).
        let b = if i == 0 { IN_JUMP } else { 0 };
        sim.step(&arena, [with(b, 0.0, 0.0), idle(0.0)]);
        if i == 0 {
            first_vz = sim.players[0].velocity.z;
        }
        apex = apex.max(sim.players[0].origin.z);
    }
    // 289 minus StartGravity, FinishGravity inside CheckJumpButton and the FinishGravity at the
    // end of FullWalkMove (6 each): the classic 271 u/s post-jump velocity.
    assert!((first_vz - 271.0).abs() < 0.01, "first vz {first_vz}");
    // Discrete integration of 277, 265, ... gives ~50 units (72 with a crouch, per the TF2 wiki).
    assert!(apex > 49.0 && apex < 52.0, "apex {apex}");
    assert!(sim.players[0].on_ground(), "should have landed");
}

#[test]
fn crouch_jump_reaches_72_units() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(0.0, 0.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::new(300.0, 300.0, 0.0), 0.0);
    settle(&mut sim, &arena, 3);
    let mut apex = 0.0f32;
    for i in 0..80 {
        // Jump, then hold crouch while airborne (origin is raised by the hull delta of 20).
        let b = if i == 0 { IN_JUMP } else if i >= 2 { IN_DUCK } else { 0 };
        sim.step(&arena, [with(b, 0.0, 0.0), idle(0.0)]);
        apex = apex.max(sim.players[0].origin.z);
    }
    assert!(apex > 70.0 && apex < 73.0, "crouch-jump apex {apex}");
}

#[test]
fn cannot_jump_while_fully_crouched() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(0.0, 0.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::new(300.0, 300.0, 0.0), 0.0);
    settle(&mut sim, &arena, 3);
    for _ in 0..30 {
        sim.step(&arena, [with(IN_DUCK, 0.0, 0.0), idle(0.0)]);
    }
    assert!(sim.players[0].flags & FL_DUCKING != 0);
    let z0 = sim.players[0].origin.z;
    for _ in 0..10 {
        sim.step(&arena, [with(IN_DUCK | IN_JUMP, 0.0, 0.0), idle(0.0)]);
    }
    assert!((sim.players[0].origin.z - z0).abs() < 0.01, "crouched player must not jump");
}

#[test]
fn air_strafing_gains_speed() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(-300.0, 200.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::new(300.0, 300.0, 0.0), 0.0);
    settle(&mut sim, &arena, 3);
    // Run forward then jump.
    for _ in 0..40 {
        sim.step(&arena, [with(IN_FORWARD, 0.0, 0.0), idle(0.0)]);
    }
    sim.step(&arena, [with(IN_FORWARD | IN_JUMP, 0.0, 0.0), idle(0.0)]);
    let speed_at_jump = sim.players[0].velocity.length_2d();
    // Strafe: hold right and keep the view pointed along the velocity so the wish direction stays
    // perpendicular to it (the optimal strafe for a 30 u/s air cap).
    let mut best = speed_at_jump;
    for _ in 0..30 {
        let v = sim.players[0].velocity;
        let yaw = v.y.atan2(v.x).to_degrees();
        sim.step(&arena, [with(IN_MOVERIGHT, 0.0, yaw), idle(0.0)]);
        best = best.max(sim.players[0].velocity.length_2d());
        if sim.players[0].on_ground() {
            break;
        }
    }
    // Source only grants full air acceleration while falling (surface friction drops to 0.25 while
    // rising), so a single jump gains roughly 20 u/s with perfect strafing.
    assert!(best > speed_at_jump + 15.0, "air strafing should gain speed: {speed_at_jump} -> {best}");
}

#[test]
fn bunny_hop_speed_is_capped() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(-300.0, 0.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::new(300.0, 300.0, 0.0), 0.0);
    settle(&mut sim, &arena, 3);
    // Give an artificial 500 u/s ground speed and jump.
    sim.players[0].velocity = Vec3::new(500.0, 0.0, 0.0);
    sim.step(&arena, [with(IN_JUMP, 0.0, 0.0), idle(0.0)]);
    let s = sim.players[0].velocity.length_2d();
    assert!((s - 240.0 * 1.2).abs() < 1.0, "bhop capped to 288, got {s}");
}

/// Fires player 1's rocket launcher at player 0 and returns the tick the rocket exploded.
fn shoot(sim: &mut SimState, arena: &Arena, shooter_pitch: f32, shooter_yaw: f32, victim_input: PlayerInput) -> u32 {
    // Wait out the deploy delay.
    while sim.curtime() < sim.players[1].next_primary_attack {
        sim.step(arena, [victim_input, with(0, shooter_pitch, shooter_yaw)]);
    }
    sim.step(arena, [victim_input, with(IN_ATTACK, shooter_pitch, shooter_yaw)]);
    assert!(sim.events.iter().any(|e| matches!(e, SimEvent::RocketFired { shooter: 1, .. })), "rocket should fire");
    for _ in 0..200 {
        sim.step(arena, [victim_input, with(0, shooter_pitch, shooter_yaw)]);
        if sim.events.iter().any(|e| matches!(e, SimEvent::Explosion { .. })) {
            return sim.tick;
        }
    }
    panic!("rocket never exploded");
}

#[test]
fn rocket_at_feet_launches_grounded_victim() {
    let (arena, mut sim) = fresh();
    // Victim at origin, shooter 200 units away looking at the victim's feet.
    place(&mut sim, 0, Vec3::new(0.0, 0.0, 0.0), 180.0);
    place(&mut sim, 1, Vec3::new(200.0, 0.0, 0.0), 180.0);
    settle(&mut sim, &arena, 3);
    // Aim at the floor just in front of the victim: pitch down so the ray from eye (68 up) hits z=0 at x≈10.
    let dx = 190.0f32;
    let pitch = (68.0f32 / dx).atan().to_degrees();
    shoot(&mut sim, &arena, pitch, 180.0, idle(180.0));
    let hit = sim.events.iter().find_map(|e| match e {
        SimEvent::PlayerHit { victim: 0, damage, .. } => Some(*damage),
        _ => None,
    });
    let dmg = hit.expect("victim should be hit by splash");
    assert!(dmg > 40.0 && dmg < 140.0, "splash damage {dmg}");
    let vz = sim.players[0].velocity.z;
    assert!(vz > 250.0, "victim should be launched (vz {vz})");
    // Boost fires 7 ticks later and multiplies upward velocity by 2.15.
    let boost_tick = sim.tick + MGE_ENDIF_BOOST_DELAY_TICKS - 1;
    let mut before = 0.0;
    while sim.tick <= boost_tick {
        before = sim.players[0].velocity.z;
        sim.step(&arena, [idle(180.0), idle(180.0)]);
    }
    let after = sim.players[0].velocity.z;
    assert!(after > before * 1.8, "endif boost should multiply vz: {before} -> {after}");
}

#[test]
fn direct_hit_high_in_air_is_an_airshot_kill() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(0.0, 0.0, 0.0), 180.0);
    place(&mut sim, 1, Vec3::new(400.0, 0.0, 0.0), 180.0);
    settle(&mut sim, &arena, 3);
    // Put the victim high in the air, hovering by re-teleporting each tick is cheating; instead
    // give the victim a big upward velocity so it is above 250 for a while.
    sim.players[0].velocity = Vec3::new(0.0, 0.0, 900.0);
    for _ in 0..30 {
        sim.step(&arena, [idle(180.0), idle(180.0)]);
    }
    let z = sim.players[0].origin.z;
    assert!(z > 250.0, "victim should be high: {z}");
    // Shooter aims at the victim's centre.
    let center = sim.players[0].world_space_center();
    let eye = sim.players[1].eye_position();
    let d = center - eye;
    let _ = endif_sim::math::vector_angles(d);
    // Fire immediately (deploy already elapsed after settle + 30 ticks? make sure).
    while sim.curtime() < sim.players[1].next_primary_attack {
        sim.step(&arena, [idle(180.0), idle(180.0)]);
    }
    // Re-aim right before firing, since the victim keeps moving.
    let center = sim.players[0].world_space_center();
    let eye = sim.players[1].eye_position();
    // Lead the target: rocket takes ~ (400/1100) s; victim moves by vz * t.
    let t = (center - eye).length() / ROCKET_SPEED;
    let lead = Vec3::new(0.0, 0.0, sim.players[0].velocity.z * t - 0.5 * 800.0 * t * t);
    let a = endif_sim::math::vector_angles(center + lead - eye);
    let pitch2 = if a.pitch > 180.0 { a.pitch - 360.0 } else { a.pitch };
    let score_before = sim.players[1].score;
    sim.step(&arena, [idle(180.0), with(IN_ATTACK, pitch2, a.yaw)]);
    let mut killed = false;
    for _ in 0..60 {
        sim.step(&arena, [idle(180.0), idle(180.0)]);
        if sim.events.iter().any(|e| matches!(e, SimEvent::Killed { victim: 0, attacker: 1 })) {
            killed = true;
            break;
        }
    }
    assert!(killed, "direct hit above 250 units should be an airshot kill");
    assert_eq!(sim.players[1].score, score_before + 1);
    assert!(!sim.players[0].alive);
    // Victim respawns after the delay.
    for _ in 0..(sim.rules.respawn_delay_ticks + 2) {
        sim.step(&arena, [idle(180.0), idle(180.0)]);
    }
    assert!(sim.players[0].alive, "victim should respawn");
    // House rule: a killed player respawns high in the air (chain airshots), not on the floor.
    let h = sim.players[0].origin.z - arena.floor_z();
    assert!(h > sim.rules.respawn_height as f32 - 50.0, "respawn should be {} units up, was {h}", sim.rules.respawn_height);
    assert!(sim.players[0].ground.is_none(), "respawned player should be airborne");
}

#[test]
fn self_rocket_jump_is_plausible() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(0.0, 0.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::new(300.0, 300.0, 0.0), 0.0);
    settle(&mut sim, &arena, 3);
    while sim.curtime() < sim.players[0].next_primary_attack {
        sim.step(&arena, [idle(0.0), idle(0.0)]);
    }
    // Jump, crouch, and shoot straight down at the same time (a classic vertical rocket jump).
    sim.step(&arena, [with(IN_JUMP, 89.0, 0.0), idle(0.0)]);
    sim.step(&arena, [with(IN_DUCK | IN_ATTACK, 89.0, 0.0), idle(0.0)]);
    let mut apex = 0.0f32;
    let mut max_vz = 0.0f32;
    for _ in 0..200 {
        sim.step(&arena, [with(IN_DUCK, 89.0, 0.0), idle(0.0)]);
        apex = apex.max(sim.players[0].origin.z);
        max_vz = max_vz.max(sim.players[0].velocity.z);
        if sim.players[0].on_ground() && sim.tick > 40 {
            break;
        }
    }
    // Airborne self damage is scaled by 0.6 before the push force is computed, so a crouched
    // jump-and-shoot gets roughly 0.6 * ~80 dmg * 10 * (82/55) ≈ 700 u/s on top of the ~230 u/s
    // left from the jump, well under the 1000 u/s DamageForce cap.
    assert!(max_vz > 820.0 && max_vz < 1000.0, "rocket jump max vz {max_vz}");
    assert!(apex > 450.0 && apex < 650.0, "rocket jump apex {apex}");
    // Self damage healed by endif rules, still alive.
    assert!(sim.players[0].alive);
}

#[test]
fn simulation_is_deterministic_and_rollback_safe() {
    let arena = Arena::classic_square();
    let mut a = SimState::new(99, Rules::default());
    let mut b = SimState::new(99, Rules::default());
    let mut rng = 0x1234_5678u32;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    let mut inputs = Vec::new();
    for _ in 0..1500 {
        let r0 = next();
        let r1 = next();
        let mk = |r: u32| PlayerInput {
            buttons: r & (IN_ATTACK | IN_JUMP | IN_DUCK | IN_FORWARD | IN_BACK | IN_MOVELEFT | IN_MOVERIGHT),
            pitch: ((r >> 8) % 178) as f32 - 89.0,
            yaw: ((r >> 16) % 360) as f32 - 180.0,
        };
        inputs.push([mk(r0), mk(r1)]);
    }
    let mut snapshots = Vec::new();
    for (i, inp) in inputs.iter().enumerate() {
        a.step(&arena, *inp);
        if i % 7 == 0 {
            snapshots.push((i, a.clone()));
        }
    }
    for inp in &inputs {
        b.step(&arena, *inp);
    }
    assert_eq!(a.checksum(), b.checksum(), "two identical runs must match");

    // Rollback: restore a snapshot, replay, and compare with the straight run.
    let (i, snap) = snapshots[snapshots.len() / 2].clone();
    let mut c = snap;
    for inp in &inputs[i + 1..] {
        c.step(&arena, *inp);
    }
    assert_eq!(a.checksum(), c.checksum(), "resimulation from a snapshot must match");
    assert!(a.players.iter().all(|p| p.origin.is_finite() && p.velocity.is_finite()));
}

/// Aims player 1 at player 0 with lead for the victim's current velocity (and gravity), fires,
/// and returns the `chain` of the resulting airshot kill, or `None` if the rocket missed.
fn fire_at_victim(sim: &mut SimState, arena: &Arena) -> Option<u8> {
    while sim.curtime() < sim.players[1].next_primary_attack {
        sim.step(arena, [idle(180.0), idle(180.0)]);
    }
    let eye = sim.players[1].eye_position();
    let center = sim.players[0].world_space_center();
    let vel = sim.players[0].velocity;
    // Iterate the intercept: where the victim will be by the time the rocket gets there.
    let mut t = (center - eye).length() / ROCKET_SPEED;
    let mut aim = center;
    for _ in 0..4 {
        aim = center + vel * t + Vec3::new(0.0, 0.0, -0.5 * SV_GRAVITY * t * t);
        t = (aim - eye).length() / ROCKET_SPEED;
    }
    let a = endif_sim::math::vector_angles(aim - eye);
    let pitch = if a.pitch > 180.0 { a.pitch - 360.0 } else { a.pitch };
    sim.step(arena, [idle(180.0), with(IN_ATTACK, pitch, a.yaw)]);
    assert!(sim.events.iter().any(|e| matches!(e, SimEvent::RocketFired { shooter: 1, .. })), "rocket should fire");
    for _ in 0..120 {
        sim.step(arena, [idle(180.0), idle(180.0)]);
        let kill = sim.events.iter().find_map(|e| match e {
            SimEvent::PlayerHit { victim: 0, attacker: 1, airshot_kill: true, chain, .. } => Some(*chain),
            _ => None,
        });
        if kill.is_some() {
            return kill;
        }
        if sim.events.iter().any(|e| matches!(e, SimEvent::Explosion { .. })) {
            return None;
        }
    }
    None
}

/// Runs the sim until the dead victim is back, then stands the shooter at the arena centre (300
/// units from every spawn, muzzle clear of the walls) with the launcher ready, so the next rocket
/// arrives long before the victim falls below the airshot line.
fn respawn_victim(sim: &mut SimState, arena: &Arena) {
    for _ in 0..(sim.rules.respawn_delay_ticks + 2) {
        if sim.players[0].alive {
            break;
        }
        sim.step(arena, [idle(180.0), idle(180.0)]);
    }
    assert!(sim.players[0].alive, "victim should respawn");
    place(sim, 1, Vec3::ZERO, 180.0);
    sim.players[1].next_primary_attack = sim.curtime();
}

#[test]
fn kills_before_the_victim_lands_from_respawn_chain() {
    let (arena, mut sim) = fresh();
    place(&mut sim, 0, Vec3::new(0.0, 0.0, 0.0), 180.0);
    place(&mut sim, 1, Vec3::new(400.0, 0.0, 0.0), 180.0);
    settle(&mut sim, &arena, 3);
    assert!(sim.players[0].landed_since_spawn, "a floor spawn counts as landed");

    // First kill: the victim is thrown up and shot. An ordinary kill, chain 1.
    sim.players[0].velocity = Vec3::new(0.0, 0.0, 900.0);
    for _ in 0..30 {
        sim.step(&arena, [idle(180.0), idle(180.0)]);
    }
    assert_eq!(fire_at_victim(&mut sim, &arena), Some(1), "first kill");
    assert_eq!(sim.players[1].chain, 1);
    assert_eq!(sim.players[0].chain, 0);

    // The victim comes back high up; killing them before they land chains: x2, then x3.
    for expected in [2u8, 3] {
        respawn_victim(&mut sim, &arena);
        assert!(!sim.players[0].landed_since_spawn, "respawned high, not landed yet");
        assert_eq!(fire_at_victim(&mut sim, &arena), Some(expected), "chain x{expected}");
        assert_eq!(sim.players[1].chain, expected);
    }

    // Letting the victim land breaks the chain: the next kill is an ordinary one again.
    respawn_victim(&mut sim, &arena);
    for _ in 0..300 {
        if sim.players[0].on_ground() {
            break;
        }
        sim.step(&arena, [idle(180.0), idle(180.0)]);
    }
    assert!(sim.players[0].on_ground() && sim.players[0].landed_since_spawn, "victim should have landed");
    sim.players[0].velocity = Vec3::new(0.0, 0.0, 900.0);
    for _ in 0..30 {
        sim.step(&arena, [idle(180.0), idle(180.0)]);
    }
    assert_eq!(fire_at_victim(&mut sim, &arena), Some(1), "kill after landing");
    assert_eq!(sim.players[1].chain, 1);
}

/// Where player 1's rocket starts when they fire straight ahead at yaw 0 after spawning with the
/// given launcher preference bits, relative to the eye (Source axes: x forward, -y to the right).
fn muzzle_offset(weapon_bits: u32) -> (Vec3, Weapon) {
    let arena = Arena::classic_square();
    let mut sim = SimState::new(1234, Rules::default());
    let input = with(weapon_bits, 0.0, 0.0);
    sim.step(&arena, [idle(0.0), input]);
    place(&mut sim, 0, Vec3::new(-300.0, 0.0, 0.0), 0.0);
    place(&mut sim, 1, Vec3::ZERO, 0.0);
    while sim.curtime() < sim.players[1].next_primary_attack {
        sim.step(&arena, [idle(0.0), input]);
    }
    let eye = sim.players[1].eye_position();
    sim.step(&arena, [idle(0.0), with(weapon_bits | IN_ATTACK, 0.0, 0.0)]);
    sim.events
        .iter()
        .find_map(|e| match e {
            SimEvent::RocketFired { shooter: 1, origin, weapon, .. } => Some((*origin - eye, *weapon)),
            _ => None,
        })
        .expect("rocket should fire")
}

#[test]
fn the_original_fires_from_the_middle() {
    // `FireRocket`: 23.5 forward, 12 to the right and 3 below the eye; The Original's
    // `centerfire_projectile` drops the sideways part.
    let (stock, weapon) = muzzle_offset(0);
    assert_eq!(weapon, Weapon::Stock);
    assert!((stock - Vec3::new(23.5, -12.0, -3.0)).length() < 0.01, "stock muzzle {stock:?}");
    let (original, weapon) = muzzle_offset(IN_WEAPON_ORIGINAL);
    assert_eq!(weapon, Weapon::Original);
    assert!((original - Vec3::new(23.5, 0.0, -3.0)).length() < 0.01, "original muzzle {original:?}");
}

#[test]
fn launcher_choice_waits_for_the_next_spawn() {
    let (arena, mut sim) = fresh();
    assert_eq!(sim.players[0].weapon, Weapon::Stock);
    // Asking for The Original mid-life changes nothing...
    for _ in 0..10 {
        sim.step(&arena, [with(IN_WEAPON_ORIGINAL, 0.0, 0.0), idle(0.0)]);
    }
    assert_eq!(sim.players[0].weapon, Weapon::Stock);
    // ...until the player dies and comes back.
    sim.players[0].alive = false;
    sim.players[0].respawn_tick = sim.tick;
    sim.step(&arena, [with(IN_WEAPON_ORIGINAL, 0.0, 0.0), idle(0.0)]);
    assert!(sim.players[0].alive);
    assert_eq!(sim.players[0].weapon, Weapon::Original);
    // And going back to stock waits the same way.
    for _ in 0..10 {
        sim.step(&arena, [idle(0.0), idle(0.0)]);
    }
    assert_eq!(sim.players[0].weapon, Weapon::Original);
    // Both players were spawned by `begin` before any input existed: the first stepped tick
    // decides for them too.
    let mut sim = SimState::new(7, Rules::default());
    sim.begin(&arena);
    assert!(sim.players[1].alive && sim.players[1].weapon_pending);
    sim.step(&arena, [idle(0.0), with(IN_WEAPON_ORIGINAL, 0.0, 0.0)]);
    assert_eq!((sim.players[0].weapon, sim.players[1].weapon), (Weapon::Stock, Weapon::Original));
}

#[test]
fn practice_switches_launchers_at_once() {
    let arena = Arena::classic_square();
    let mut sim = SimState::new(1234, Rules { instant_weapon_switch: true, ..Rules::default() });
    sim.step(&arena, [idle(0.0), idle(0.0)]);
    assert_eq!(sim.players[0].weapon, Weapon::Stock);
    sim.step(&arena, [with(IN_WEAPON_ORIGINAL, 0.0, 0.0), idle(0.0)]);
    assert_eq!(sim.players[0].weapon, Weapon::Original);
    sim.step(&arena, [idle(0.0), idle(0.0)]);
    assert_eq!(sim.players[0].weapon, Weapon::Stock);
}
