//! Port of `CGameMovement` / `CTFGameMovement` (Source SDK 2013, TF2 branch) restricted to the
//! walk movetype with no water and no ladders. Function names follow the C++ originals so the
//! code can be diffed against `gamemovement.cpp` and `tf_gamemovement.cpp`.

use crate::consts::*;
use crate::input::*;
use crate::math::*;
use crate::player::*;
use crate::trace::*;

/// `CMoveData`: the per-command scratch data.
#[derive(Clone, Debug)]
pub struct MoveData {
    pub origin: Vec3,
    pub velocity: Vec3,
    pub view_angles: QAngle,
    pub angles: QAngle,
    pub forward_move: f32,
    pub side_move: f32,
    pub up_move: f32,
    pub buttons: u32,
    pub old_buttons: u32,
    pub max_speed: f32,
    pub client_max_speed: f32,
    pub out_wish_vel: Vec3,
    pub out_jump_vel: Vec3,
    pub out_step_height: f32,
}

impl MoveData {
    /// `CPlayerMove::SetupMove`: build the move data from the player and the user command.
    pub fn setup(player: &Player, input: &PlayerInput) -> MoveData {
        let view = QAngle::new(input.pitch, input.yaw, 0.0);
        let mut forward_move = 0.0;
        let mut side_move = 0.0;
        if input.pressed(IN_FORWARD) {
            forward_move += CL_FORWARDSPEED;
        }
        if input.pressed(IN_BACK) {
            forward_move -= CL_FORWARDSPEED;
        }
        if input.pressed(IN_MOVERIGHT) {
            side_move += CL_SIDESPEED;
        }
        if input.pressed(IN_MOVELEFT) {
            side_move -= CL_SIDESPEED;
        }
        MoveData {
            origin: player.origin,
            velocity: player.velocity,
            view_angles: view,
            angles: view,
            forward_move,
            side_move,
            up_move: 0.0,
            buttons: input.buttons,
            old_buttons: player.old_buttons,
            max_speed: player.max_speed,
            client_max_speed: player.max_speed,
            out_wish_vel: Vec3::ZERO,
            out_jump_vel: Vec3::ZERO,
            out_step_height: 0.0,
        }
    }

    /// `CPlayerMove::FinishMove`: copy results back to the player.
    pub fn finish(&self, player: &mut Player) {
        player.origin = self.origin;
        player.velocity = self.velocity;
        player.view_angles = self.view_angles;
        player.old_buttons = self.old_buttons;
    }
}

const SPEED_CROPPED_DUCK: u32 = 1;

/// Runs one movement command for one player.
pub struct GameMovement<'a> {
    pub player: &'a mut Player,
    pub mv: &'a mut MoveData,
    pub env: TraceEnv<'a>,
    pub curtime: f32,
    pub frametime: f32,
    speed_cropped: u32,
}

impl<'a> GameMovement<'a> {
    pub fn new(player: &'a mut Player, mv: &'a mut MoveData, env: TraceEnv<'a>, curtime: f32) -> Self {
        GameMovement { player, mv, env, curtime, frametime: TICK_INTERVAL, speed_cropped: 0 }
    }

    // ------------------------------------------------------------------ helpers / tracing

    fn player_mins(&self) -> Vec3 {
        self.player.hull_mins()
    }

    fn player_maxs(&self) -> Vec3 {
        self.player.hull_maxs()
    }

    fn player_view_offset(&self, ducked: bool) -> Vec3 {
        if ducked { VEC_DUCK_VIEW } else { SOLDIER_VIEW }
    }

    /// `CTFGameMovement::TracePlayerBBox`.
    fn trace_player_bbox(&self, start: Vec3, end: Vec3) -> Trace {
        trace_hull(&self.env, start, end, self.player_mins(), self.player_maxs())
    }

    /// `TestPlayerPosition`: returns the entity we are stuck in, if any.
    fn test_player_position(&self, pos: Vec3) -> Option<HitEnt> {
        let pm = trace_hull(&self.env, pos, pos, self.player_mins(), self.player_maxs());
        if pm.startsolid || pm.fraction < 1.0 { pm.ent.or(Some(HitEnt::World)) } else { None }
    }

    // ------------------------------------------------------------------ entry point

    /// `CTFGameMovement::ProcessMovement`.
    pub fn process_movement(&mut self) {
        self.speed_cropped = 0;
        // The max speed is currently set to the scout - if this changes we need to change this!
        self.mv.max_speed = TF_MAX_SPEED;
        self.player_move();
        self.finish_move();
    }

    /// `CGameMovement::FinishMove`.
    fn finish_move(&mut self) {
        self.mv.old_buttons = self.mv.buttons;
    }

    /// `CGameMovement::PlayerMove` + `CTFGameMovement::PlayerMove` (walk movetype only).
    fn player_move(&mut self) {
        self.check_parameters();

        self.mv.out_wish_vel = Vec3::ZERO;
        self.mv.out_jump_vel = Vec3::ZERO;

        self.reduce_timers();

        // CheckStuck() is skipped: the arena is convex and spawns never overlap.

        // Now that we are "unstuck", see where we are (player->GetGroundEntity()).
        if self.player.game_code_moved {
            self.categorize_position();
            self.player.game_code_moved = false;
        } else if self.mv.velocity.z > 250.0 {
            self.set_ground_entity(None);
        }

        // If we are not on ground, store off how fast we are moving down
        if self.player.ground.is_none() {
            self.player.fall_velocity = -self.mv.velocity.z;
        }

        self.update_duck_jump_eye_offset();
        self.duck();

        self.full_walk_move();
    }

    /// `CGameMovement::CheckParameters` (no punch angle, no roll).
    fn check_parameters(&mut self) {
        let spd = self.mv.forward_move * self.mv.forward_move
            + self.mv.side_move * self.mv.side_move
            + self.mv.up_move * self.mv.up_move;

        let maxspeed = self.mv.client_max_speed;
        if maxspeed != 0.0 {
            self.mv.max_speed = fmin(maxspeed, self.mv.max_speed);
        }

        // g_bMovementOptimizations: same thing but only do the sqrt if we have to.
        if spd != 0.0 && spd > self.mv.max_speed * self.mv.max_speed {
            let ratio = self.mv.max_speed / sqrtf(spd);
            self.mv.forward_move *= ratio;
            self.mv.side_move *= ratio;
            self.mv.up_move *= ratio;
        }

        // Take angles from command.
        let v_angle = self.mv.angles;
        self.mv.angles = QAngle::new(v_angle.pitch, v_angle.yaw, 0.0);

        // Adjust client view angles to match values used on server.
        if self.mv.angles.yaw > 180.0 {
            self.mv.angles.yaw -= 360.0;
        }
    }

    /// `CGameMovement::ReduceTimers`.
    fn reduce_timers(&mut self) {
        let frame_msec = 1000.0 * self.frametime;
        let p = &mut self.player;
        if p.duck_time > 0.0 {
            p.duck_time -= frame_msec;
            if p.duck_time < 0.0 {
                p.duck_time = 0.0;
            }
        }
        if p.duck_jump_time > 0.0 {
            p.duck_jump_time -= frame_msec;
            if p.duck_jump_time < 0.0 {
                p.duck_jump_time = 0.0;
            }
        }
        if p.jump_time > 0.0 {
            p.jump_time -= frame_msec;
            if p.jump_time < 0.0 {
                p.jump_time = 0.0;
            }
        }
    }

    // ------------------------------------------------------------------ gravity / velocity

    /// `CGameMovement::StartGravity`.
    fn start_gravity(&mut self) {
        let ent_gravity = 1.0f32;
        // Add gravity so they'll be in the correct position during movement
        // yes, this 0.5 looks wrong, but it's not.
        // C++: float * float * double * float → evaluated in double, then stored to float.
        let g = (ent_gravity * SV_GRAVITY) as f64 * 0.5 * (self.frametime as f64);
        self.mv.velocity.z = (self.mv.velocity.z as f64 - g) as f32;
        self.mv.velocity.z += self.player.base_velocity.z * self.frametime;

        self.player.base_velocity.z = 0.0;

        self.check_velocity();
    }

    /// `CGameMovement::FinishGravity`.
    fn finish_gravity(&mut self) {
        let ent_gravity = 1.0f32;
        // Get the correct velocity for the end of the dt
        // C++: (float * float * float) * 0.5 (double).
        let g = (ent_gravity * SV_GRAVITY * self.frametime) as f64 * 0.5;
        self.mv.velocity.z = (self.mv.velocity.z as f64 - g) as f32;

        self.check_velocity();
    }

    /// `CGameMovement::CheckVelocity`.
    fn check_velocity(&mut self) {
        for i in 0..3 {
            let v = self.mv.velocity.get(i);
            if v.is_nan() {
                self.mv.velocity.set(i, 0.0);
            }
            if self.mv.origin.get(i).is_nan() {
                self.mv.origin.set(i, 0.0);
            }
            let v = self.mv.velocity.get(i);
            if v > SV_MAXVELOCITY {
                self.mv.velocity.set(i, SV_MAXVELOCITY);
            } else if v < -SV_MAXVELOCITY {
                self.mv.velocity.set(i, -SV_MAXVELOCITY);
            }
        }
    }

    // ------------------------------------------------------------------ friction / accel

    /// `CGameMovement::Friction`.
    fn friction(&mut self) {
        let speed = self.mv.velocity.length();
        if speed < 0.1 {
            return;
        }

        let mut drop = 0.0f32;

        if self.player.ground.is_some() {
            let friction = SV_FRICTION * self.player.surface_friction;
            let control = if speed < SV_STOPSPEED { SV_STOPSPEED } else { speed };
            drop += control * friction * self.frametime;
        }

        let mut newspeed = speed - drop;
        if newspeed < 0.0 {
            newspeed = 0.0;
        }

        if newspeed != speed {
            newspeed /= speed;
            self.mv.velocity *= newspeed;
        }

        self.mv.out_wish_vel -= (1.0 - newspeed) * self.mv.velocity;
    }

    /// `CGameMovement::Accelerate`.
    fn accelerate(&mut self, wishdir: Vec3, wishspeed: f32, accel: f32) {
        // CanAccelerate(): active state and not water jumping.
        let currentspeed = self.mv.velocity.dot(wishdir);
        let addspeed = wishspeed - currentspeed;
        if addspeed <= 0.0 {
            return;
        }
        let mut accelspeed = accel * self.frametime * wishspeed * self.player.surface_friction;
        if accelspeed > addspeed {
            accelspeed = addspeed;
        }
        for i in 0..3 {
            let v = self.mv.velocity.get(i) + accelspeed * wishdir.get(i);
            self.mv.velocity.set(i, v);
        }
    }

    /// `CGameMovement::AirAccelerate`.
    fn air_accelerate(&mut self, wishdir: Vec3, wishspeed: f32, accel: f32) {
        let mut wishspd = wishspeed;
        // Cap speed
        if wishspd > self.get_air_speed_cap() {
            wishspd = self.get_air_speed_cap();
        }
        let currentspeed = self.mv.velocity.dot(wishdir);
        let addspeed = wishspd - currentspeed;
        if addspeed <= 0.0 {
            return;
        }
        let mut accelspeed = accel * wishspeed * self.frametime * self.player.surface_friction;
        if accelspeed > addspeed {
            accelspeed = addspeed;
        }
        for i in 0..3 {
            let v = self.mv.velocity.get(i) + accelspeed * wishdir.get(i);
            self.mv.velocity.set(i, v);
            let w = self.mv.out_wish_vel.get(i) + accelspeed * wishdir.get(i);
            self.mv.out_wish_vel.set(i, w);
        }
    }

    /// `CTFGameMovement::GetAirSpeedCap` for a plain soldier.
    fn get_air_speed_cap(&self) -> f32 {
        AIR_SPEED_CAP
    }

    // ------------------------------------------------------------------ walk / air move

    /// `CTFGameMovement::WalkMove`.
    fn walk_move(&mut self) {
        let (mut forward, mut right, _up) = angle_vectors(self.mv.view_angles);
        forward.z = 0.0;
        right.z = 0.0;
        forward.normalize_in_place();
        right.normalize_in_place();

        let fmove = self.mv.forward_move;
        let smove = self.mv.side_move;

        let mut wishdir = Vec3::new(
            forward.x * fmove + right.x * smove,
            forward.y * fmove + right.y * smove,
            0.0,
        );

        let mut wishspeed = wishdir.normalize_in_place();
        wishspeed = clamp(wishspeed, 0.0, self.mv.max_speed);

        // Accelerate in the x,y plane.
        self.mv.velocity.z = 0.0;

        let mut accelerate = SV_ACCELERATE;
        let friction = SV_FRICTION * self.player.surface_friction;
        let wishspeed_threshold = 100.0 * friction / SV_ACCELERATE;

        // if our wish speed is too low (attributes), we must increase acceleration or we'll never overcome friction
        if wishspeed > 0.0 && wishspeed < wishspeed_threshold {
            let speed = self.mv.velocity.length();
            let control = if speed < SV_STOPSPEED { SV_STOPSPEED } else { speed };
            accelerate = (control * friction) / wishspeed + 1.0;
        }

        self.accelerate(wishdir, wishspeed, accelerate);

        let adjusted_max_speed = self.mv.max_speed;

        // Clamp the players speed in x,y.
        let mut newspeed = self.mv.velocity.length();
        if newspeed > adjusted_max_speed {
            let scale = adjusted_max_speed / newspeed;
            self.mv.velocity.x *= scale;
            self.mv.velocity.y *= scale;
        }

        // Now reduce their backwards speed to some percent of max, if they are traveling backwards
        // unless they are under some minimum, to not penalize deployed snipers or heavies
        if TF_CLAMP_BACK_SPEED < 1.0 && self.mv.velocity.length() > TF_CLAMP_BACK_SPEED_MIN {
            let dot = forward.dot(self.mv.velocity);
            if dot < 0.0 {
                let mut back_move = forward * dot;
                let right_move = right * right.dot(self.mv.velocity);

                let back_speed = back_move.length();
                let max_back_speed = adjusted_max_speed * TF_CLAMP_BACK_SPEED;
                if back_speed > max_back_speed {
                    back_move *= max_back_speed / back_speed;
                }

                self.mv.velocity = back_move + right_move;

                newspeed = self.mv.velocity.length();
                if newspeed > adjusted_max_speed {
                    let scale = adjusted_max_speed / newspeed;
                    self.mv.velocity.x *= scale;
                    self.mv.velocity.y *= scale;
                }
            }
        }

        // Add base velocity to the player's current velocity - base velocity = velocity from conveyors, etc.
        self.mv.velocity += self.player.base_velocity;

        // Calculate the current speed and return if we are not really moving.
        let speed = self.mv.velocity.length();
        if speed < 1.0 {
            self.mv.velocity = Vec3::ZERO;
            return;
        }

        // Calculate the destination.
        let dest = Vec3::new(
            self.mv.origin.x + self.mv.velocity.x * self.frametime,
            self.mv.origin.y + self.mv.velocity.y * self.frametime,
            self.mv.origin.z,
        );

        // Try moving to the destination.
        let trace = self.trace_player_bbox(self.mv.origin, dest);
        if trace.fraction == 1.0 {
            self.mv.origin = trace.endpos;
            self.mv.velocity -= self.player.base_velocity;
            self.mv.out_wish_vel += wishdir * wishspeed;
            return;
        }

        // Now try and do a step move.
        self.step_move(dest, trace);

        self.mv.velocity -= self.player.base_velocity;
        self.mv.out_wish_vel += wishdir * wishspeed;
    }

    /// `CTFGameMovement::AirMove`.
    fn air_move(&mut self) {
        let (mut forward, mut right, _up) = angle_vectors(self.mv.view_angles);
        let fmove = self.mv.forward_move;
        let smove = self.mv.side_move;

        forward.z = 0.0;
        right.z = 0.0;
        forward.normalize_in_place();
        right.normalize_in_place();

        let mut wishvel = Vec3::new(
            forward.x * fmove + right.x * smove,
            forward.y * fmove + right.y * smove,
            0.0,
        );

        let mut wishdir = wishvel;
        let mut wishspeed = wishdir.normalize_in_place();

        // clamp to server defined max speed
        if wishspeed != 0.0 && wishspeed > self.mv.max_speed {
            wishvel *= self.mv.max_speed / wishspeed;
            wishspeed = self.mv.max_speed;
        }
        let _ = wishvel;

        self.air_accelerate(wishdir, wishspeed, SV_AIRACCELERATE);

        // Add in any base velocity to the current velocity.
        self.mv.velocity += self.player.base_velocity;

        self.try_player_move(None, 0.0);

        // Now pull the base velocity back out.
        self.mv.velocity -= self.player.base_velocity;
    }

    /// `CTFGameMovement::FullWalkMove` (not in water).
    fn full_walk_move(&mut self) {
        self.start_gravity();

        // Was jump button pressed?
        if self.mv.buttons & IN_JUMP != 0 {
            self.check_jump_button();
        } else {
            self.mv.old_buttons &= !IN_JUMP;
        }

        // Make sure velocity is valid.
        self.check_velocity();

        if self.player.ground.is_some() {
            self.mv.velocity.z = 0.0;
            self.friction();
            self.walk_move();
        } else {
            self.air_move();
        }

        // Set final flags.
        self.categorize_position();

        // Add any remaining gravitational component if we are not in water.
        self.finish_gravity();

        // If we are on ground, no downward velocity.
        if self.player.ground.is_some() {
            self.mv.velocity.z = 0.0;
        }

        // Handling falling.
        self.check_falling();

        // Make sure velocity is valid.
        self.check_velocity();
    }

    // ------------------------------------------------------------------ jumping

    /// `CTFGameMovement::PreventBunnyJumping`.
    fn prevent_bunny_jumping(&mut self) {
        let maxscaledspeed = BUNNYJUMP_MAX_SPEED_FACTOR * self.player.max_speed;
        if maxscaledspeed <= 0.0 {
            return;
        }
        let spd = self.mv.velocity.length();
        if spd <= maxscaledspeed {
            return;
        }
        let fraction = maxscaledspeed / spd;
        self.mv.velocity *= fraction;
    }

    /// `CTFGameMovement::CheckJumpButton` for a soldier.
    fn check_jump_button(&mut self) -> bool {
        let on_ground = self.player.ground.is_some();

        // Cannot jump while ducked.
        if self.player.flags & FL_DUCKING != 0 {
            return false;
        }

        // Cannot jump while in the unduck transition.
        if (self.player.ducking && self.player.flags & FL_DUCKING != 0) || self.player.duck_jump_time > 0.0 {
            return false;
        }

        // Cannot jump again until the jump button has been released.
        if self.mv.old_buttons & IN_JUMP != 0 {
            return false;
        }

        // In air, so ignore jumps (unless you are a scout or ghost or parachute)
        if !on_ground {
            self.mv.old_buttons |= IN_JUMP;
            return false;
        }

        self.prevent_bunny_jumping();

        self.player.jumping = true;

        // Set the player as in the air.
        self.set_ground_entity(None);

        let ground_factor = 1.0f32;
        let jump_mod = 1.0f32;
        let mul = (TF_JUMP_VELOCITY * jump_mod) * ground_factor;

        // Save the current z velocity.
        let start_z = self.mv.velocity.z;

        // Acclerate upward
        if self.player.ducking || self.player.flags & FL_DUCKING != 0 {
            self.mv.velocity.z = mul;
        } else {
            self.mv.velocity.z += mul;
        }

        // Apply gravity.
        self.finish_gravity();

        self.mv.out_jump_vel.z += self.mv.velocity.z - start_z;
        self.mv.out_step_height += 0.15;

        // Flag that we jumped and don't jump again until it is released.
        self.mv.old_buttons |= IN_JUMP;
        true
    }

    // ------------------------------------------------------------------ collision movement

    /// `CGameMovement::ClipVelocity`.
    fn clip_velocity(input: Vec3, normal: Vec3, overbounce: f32, redirect_coeff: f32) -> (Vec3, i32) {
        let angle = normal.z;
        let mut blocked = 0;
        if angle > 0.0 {
            blocked |= 0x01;
        }
        if angle == 0.0 {
            blocked |= 0x02;
        }

        // Determine how far along plane to slide based on incoming direction.
        let fl_blocked = input.dot(normal);
        let backoff = fl_blocked * overbounce;

        let mut out = Vec3::ZERO;
        for i in 0..3 {
            let change = normal.get(i) * backoff;
            out.set(i, input.get(i) - change);
        }

        // iterate once to make sure we aren't still moving through the plane
        let adjust = out.dot(normal);
        if adjust < 0.0 {
            out -= normal * adjust;
        }

        if redirect_coeff > 0.0 {
            let len = out.length();
            out *= (-1.0 * fl_blocked * redirect_coeff + len) / len;
        }

        (out, blocked)
    }

    /// `CGameMovement::TryPlayerMove`.
    fn try_player_move(&mut self, first: Option<(Vec3, Trace)>, slide_multiplier: f32) -> i32 {
        let numbumps = 4;
        let mut blocked = 0;
        let mut numplanes = 0usize;
        let mut planes = [Vec3::ZERO; MAX_CLIP_PLANES];

        let mut original_velocity = self.mv.velocity;
        let primal_velocity = self.mv.velocity;

        let mut all_fraction = 0.0f32;
        let mut time_left = self.frametime;

        let mut new_velocity = Vec3::ZERO;

        for _bumpcount in 0..numbumps {
            if self.mv.velocity.length() == 0.0 {
                break;
            }

            // Assume we can move all the way from the current origin to the end point.
            let end = self.mv.origin.ma(time_left, self.mv.velocity);

            // See if we can make it from origin to end point.
            let pm = match &first {
                Some((dest, tr)) if end == *dest => *tr,
                _ => self.trace_player_bbox(self.mv.origin, end),
            };

            all_fraction += pm.fraction;

            // If we started in a solid object, or we were in solid space the whole way,
            // zero out our velocity and return that we are blocked by floor and wall.
            if pm.allsolid {
                self.mv.velocity = Vec3::ZERO;
                return 4;
            }

            // If we moved some portion of the total distance, then copy the end position
            // into the pmove.origin and zero the plane counter.
            if pm.fraction > 0.0 {
                if numbumps > 0 && pm.fraction == 1.0 {
                    // There's a precision issue with terrain tracing that can cause a swept box to successfully trace
                    // when the end position is stuck in the triangle. Re-run the test with an unswept box.
                    let stuck = self.trace_player_bbox(pm.endpos, pm.endpos);
                    if stuck.startsolid || stuck.fraction != 1.0 {
                        self.mv.velocity = Vec3::ZERO;
                        break;
                    }
                }
                self.mv.origin = pm.endpos;
                original_velocity = self.mv.velocity;
                numplanes = 0;
            }

            // If we covered the entire distance, we are done and can return.
            if pm.fraction == 1.0 {
                break;
            }

            // If the plane we hit has a high z component in the normal, then it's probably a floor
            if pm.normal.z > 0.7 {
                blocked |= 1;
            }
            // If the plane has a zero z component in the normal, then it's a step or wall
            if pm.normal.z == 0.0 {
                blocked |= 2;
            }

            // Reduce amount of m_flFrameTime left by total time left * fraction that we covered.
            time_left -= time_left * pm.fraction;

            // Did we run out of planes to clip against?
            if numplanes >= MAX_CLIP_PLANES {
                self.mv.velocity = Vec3::ZERO;
                break;
            }

            // Set up next clipping plane
            planes[numplanes] = pm.normal;
            numplanes += 1;

            // modify original_velocity so it parallels all of the clip planes
            //
            // reflect player velocity
            // Only give this a try for first impact plane because you can get yourself stuck in an acute corner by jumping in place
            //  and pressing forward and nobody was really using this bounce/reflection feature anyway...
            if numplanes == 1 && self.player.ground.is_none() {
                for plane in planes.iter().take(numplanes) {
                    if plane.z > 0.7 {
                        // floor or slope
                        let (nv, _) = Self::clip_velocity(original_velocity, *plane, 1.0, slide_multiplier);
                        new_velocity = nv;
                        original_velocity = new_velocity;
                    } else {
                        let (nv, _) = Self::clip_velocity(
                            original_velocity,
                            *plane,
                            1.0 + SV_BOUNCE * (1.0 - self.player.surface_friction),
                            slide_multiplier,
                        );
                        new_velocity = nv;
                    }
                }

                self.mv.velocity = new_velocity;
                original_velocity = new_velocity;
            } else {
                let mut i = 0;
                while i < numplanes {
                    let (nv, _) = Self::clip_velocity(original_velocity, planes[i], 1.0, slide_multiplier);
                    self.mv.velocity = nv;

                    let mut j = 0;
                    while j < numplanes {
                        if j != i {
                            // Are we now moving against this plane?
                            if self.mv.velocity.dot(planes[j]) < 0.0 {
                                break; // not ok
                            }
                        }
                        j += 1;
                    }
                    if j == numplanes {
                        // Didn't have to clip, so we're ok
                        break;
                    }
                    i += 1;
                }

                // Did we go all the way through plane set
                if i != numplanes {
                    // go along this plane; velocity is set in clipping call, no need to set again.
                } else {
                    // go along the crease
                    if numplanes != 2 {
                        self.mv.velocity = Vec3::ZERO;
                        break;
                    }
                    let mut dir = planes[0].cross(planes[1]);
                    dir.normalize_in_place();
                    let d = dir.dot(self.mv.velocity);
                    self.mv.velocity = dir * d;
                }

                // if original velocity is against the original velocity, stop dead
                // to avoid tiny occilations in sloping corners
                let d = self.mv.velocity.dot(primal_velocity);
                if d <= 0.0 {
                    self.mv.velocity = Vec3::ZERO;
                    break;
                }
            }
        }

        if all_fraction == 0.0 {
            self.mv.velocity = Vec3::ZERO;
        }

        blocked
    }

    /// `CTFGameMovement::StepMove`.
    fn step_move(&mut self, destination: Vec3, trace: Trace) {
        let save_trace = trace;

        let vec_pos = self.mv.origin;
        let vec_vel = self.mv.velocity;

        let mut low_road = false;
        let mut up_road = true;

        // First try the "high road" where we move up and over obstacles
        // (m_bAllowAutoMovement is always true for players).
        {
            // Trace up by step height
            let mut end_pos = self.mv.origin;
            end_pos.z += SV_STEPSIZE + DIST_EPSILON;
            let tr = self.trace_player_bbox(self.mv.origin, end_pos);
            if !tr.startsolid && !tr.allsolid {
                self.mv.origin = tr.endpos;
            }

            // Trace over from there
            self.try_player_move(None, 0.0);

            // Then trace back down by step height to get final position
            let mut end_pos = self.mv.origin;
            end_pos.z -= SV_STEPSIZE + DIST_EPSILON;
            let tr = self.trace_player_bbox(self.mv.origin, end_pos);
            // If the trace ended up in empty space, copy the end over to the origin.
            if !tr.startsolid && !tr.allsolid {
                self.mv.origin = tr.endpos;
            }

            // If we are not on the standable ground any more or going the "high road" didn't move us at all,
            // then we'll also want to check the "low road"
            if (tr.fraction != 1.0 && tr.normal.z < 0.7) || self.mv.origin == vec_pos {
                low_road = true;
                up_road = false;
            }
        }

        if low_road {
            // Save off upward results
            let mut vec_up_pos = Vec3::ZERO;
            let mut vec_up_vel = Vec3::ZERO;
            if up_road {
                vec_up_pos = self.mv.origin;
                vec_up_vel = self.mv.velocity;
            }

            // Take the "low" road
            self.mv.origin = vec_pos;
            self.mv.velocity = vec_vel;
            self.try_player_move(Some((destination, save_trace)), 0.0);

            // Down results.
            let vec_down_pos = self.mv.origin;
            let vec_down_vel = self.mv.velocity;

            if up_road {
                let up_dist = (vec_up_pos.x - vec_pos.x) * (vec_up_pos.x - vec_pos.x)
                    + (vec_up_pos.y - vec_pos.y) * (vec_up_pos.y - vec_pos.y);
                let down_dist = (vec_down_pos.x - vec_pos.x) * (vec_down_pos.x - vec_pos.x)
                    + (vec_down_pos.y - vec_pos.y) * (vec_down_pos.y - vec_pos.y);

                // decide which one went farther
                if up_dist >= down_dist {
                    self.mv.origin = vec_up_pos;
                    self.mv.velocity = vec_up_vel;
                    // copy z value from the Low Road move
                    self.mv.velocity.z = vec_down_vel.z;
                }
            }
        }

        let step_dist = self.mv.origin.z - vec_pos.z;
        if step_dist > 0.0 {
            self.mv.out_step_height += step_dist;
        }
    }

    // ------------------------------------------------------------------ ground

    /// `CGameMovement::SetGroundEntity` + `CTFGameMovement::SetGroundEntity`.
    fn set_ground_entity(&mut self, pm: Option<&Trace>) {
        let new_ground = pm.and_then(|t| t.ent);

        // Ground velocities are always zero here (no movers), so base velocity is untouched.
        self.player.ground = new_ground;
        if new_ground.is_some() {
            self.player.flags |= FL_ONGROUND;
            self.mv.velocity.z = 0.0;
            // TF: reset air dash / air duck counters when landing.
            self.player.air_dash = 0;
            self.player.air_ducked = 0;
        } else {
            self.player.flags &= !FL_ONGROUND;
        }
    }

    /// `TracePlayerBBoxForGround`: traces the player's bounds in quadrants looking for a plane
    /// that can be stood upon. Regardless of success or failure, replace the fraction and endpos
    /// with the original ones.
    fn trace_player_bbox_for_ground(&self, start: Vec3, end: Vec3, mins_src: Vec3, maxs_src: Vec3, pm: &mut Trace) {
        let fraction = pm.fraction;
        let endpos = pm.endpos;

        let quadrants = [
            // -x, -y
            (mins_src, Vec3::new(fmin(0.0, maxs_src.x), fmin(0.0, maxs_src.y), maxs_src.z)),
            // +x, +y
            (Vec3::new(fmax(0.0, mins_src.x), fmax(0.0, mins_src.y), mins_src.z), maxs_src),
            // -x, +y
            (
                Vec3::new(mins_src.x, fmax(0.0, mins_src.y), mins_src.z),
                Vec3::new(fmin(0.0, maxs_src.x), maxs_src.y, maxs_src.z),
            ),
            // +x, -y
            (
                Vec3::new(fmax(0.0, mins_src.x), mins_src.y, mins_src.z),
                Vec3::new(maxs_src.x, fmin(0.0, maxs_src.y), maxs_src.z),
            ),
        ];

        for (mins, maxs) in quadrants {
            *pm = trace_hull(&self.env, start, end, mins, maxs);
            if pm.ent.is_some() && pm.normal.z >= 0.7 {
                pm.fraction = fraction;
                pm.endpos = endpos;
                return;
            }
        }

        pm.fraction = fraction;
        pm.endpos = endpos;
    }

    /// `CTFGameMovement::CategorizePosition`.
    fn categorize_position(&mut self) {
        // Reset this each time we-recategorize, otherwise we have bogus friction when we jump into water and plunge downward really quickly
        self.player.surface_friction = 1.0;

        // Check for a jump.
        if self.mv.velocity.z > 250.0 {
            self.set_ground_entity(None);
            return;
        }

        // Calculate the start and end position.
        let start_pos = self.mv.origin;
        let mut end_pos = Vec3::new(self.mv.origin.x, self.mv.origin.y, self.mv.origin.z - 2.0);

        // NOTE YWB 7/5/07: Since we're already doing a traceline here, we'll subsume the StayOnGround (stair debouncing) check into the main traceline we do here to see what we're standing on
        let mut move_to_end_pos = false;
        if self.player.ground.is_some() {
            // if walking and still think we're on ground, we'll extend trace down by stepsize so we don't bounce down slopes
            end_pos.z -= SV_STEPSIZE;
            move_to_end_pos = true;
        }

        let mut trace = self.trace_player_bbox(start_pos, end_pos);

        let mut in_air = false;

        // Steep plane, not on ground.
        if trace.normal.z < 0.7 {
            // Test four sub-boxes, to see if any of them would have found shallower slope we could actually stand on.
            let mins = self.player_mins();
            let maxs = self.player_maxs();
            self.trace_player_bbox_for_ground(start_pos, end_pos, mins, maxs, &mut trace);

            if trace.normal.z < 0.7 {
                // Too steep.
                in_air = true;
                if self.mv.velocity.z > 0.0 {
                    self.player.surface_friction = 0.25;
                }
            }
        } else {
            // YWB: This logic block essentially lifted from StayOnGround implementation
            if move_to_end_pos && !trace.startsolid && trace.fraction > 0.0 && trace.fraction < 1.0 {
                let delta = fabsf(self.mv.origin.z - trace.endpos.z);
                // HACK HACK: The real problem is that trace returning that strange value
                //  we can't network over based on bit precision of networking origins
                if delta > 0.5 * COORD_RESOLUTION {
                    self.mv.origin.z = trace.endpos.z;
                }
            }
        }

        if in_air {
            self.set_ground_entity(None);
        } else {
            self.set_ground_entity(Some(&trace));
        }
    }

    /// `CTFGameMovement::CheckFalling` + `CGameMovement::CheckFalling` (fall damage handled by
    /// the world, which reads `fall_velocity`/`landed`).
    fn check_falling(&mut self) {
        // if we landed on the ground
        if self.player.ground.is_some() {
            // turn off the jumping flag if we're on ground after a jump
            if self.player.jumping {
                self.player.jumping = false;
            }
        }

        // this function really deals with landing, not falling, so early out otherwise
        if self.player.ground.is_none() || self.player.fall_velocity <= 0.0 {
            return;
        }

        // Fall damage would be applied here (PlayerFallingDamage). A soldier needs a fall
        // velocity of 6000 u/s to die from it, which is above sv_maxvelocity, and MGE heals every
        // non-lethal hit, so it is intentionally not simulated.

        // Clear the fall velocity so the impact doesn't happen again.
        self.player.fall_velocity = 0.0;
    }

    // ------------------------------------------------------------------ ducking

    /// `CGameMovement::UpdateDuckJumpEyeOffset`.
    fn update_duck_jump_eye_offset(&mut self) {
        if self.player.duck_jump_time != 0.0 {
            let duck_ms = fmax(0.0, GAMEMOVEMENT_DUCK_TIME - self.player.duck_jump_time);
            let duck_seconds = duck_ms / GAMEMOVEMENT_DUCK_TIME;
            if duck_seconds > TIME_TO_UNDUCK {
                self.player.duck_jump_time = 0.0;
                self.set_ducked_eye_offset(0.0);
            } else {
                let frac = simple_spline(1.0 - (duck_seconds / TIME_TO_UNDUCK));
                self.set_ducked_eye_offset(frac);
            }
        }
    }

    /// `CGameMovement::SetDuckedEyeOffset`.
    fn set_ducked_eye_offset(&mut self, duck_fraction: f32) {
        let duck_hull_min = VEC_DUCK_HULL_MIN;
        let stand_hull_min = VEC_HULL_MIN;
        let more = duck_hull_min.z - stand_hull_min.z;

        let duck_view = self.player_view_offset(true);
        let stand_view = self.player_view_offset(false);
        self.player.view_offset.z =
            ((duck_view.z - more) * duck_fraction) + (stand_view.z * (1.0 - duck_fraction));
    }

    /// `CGameMovement::HandleDuckingSpeedCrop`.
    fn handle_ducking_speed_crop(&mut self) {
        if self.speed_cropped & SPEED_CROPPED_DUCK == 0
            && self.player.flags & FL_DUCKING != 0
            && self.player.ground.is_some()
        {
            let frac = 0.333_333_33f32;
            self.mv.forward_move *= frac;
            self.mv.side_move *= frac;
            self.mv.up_move *= frac;
            self.speed_cropped |= SPEED_CROPPED_DUCK;
        }
    }

    /// `CGameMovement::CanUnduck`.
    fn can_unduck(&mut self) -> bool {
        let mut new_origin = self.mv.origin;

        if self.player.ground.is_some() {
            for i in 0..3 {
                let v = new_origin.get(i) + (VEC_DUCK_HULL_MIN.get(i) - VEC_HULL_MIN.get(i));
                new_origin.set(i, v);
            }
        } else {
            // If in air an letting go of crouch, make sure we can offset origin to make up for uncrouching
            let hull_size_normal = VEC_HULL_MAX - VEC_HULL_MIN;
            let hull_size_crouch = VEC_DUCK_HULL_MAX - VEC_DUCK_HULL_MIN;
            let view_delta = -(hull_size_normal - hull_size_crouch);
            new_origin += view_delta;
        }

        let save_ducked = self.player.ducked;
        self.player.ducked = false;
        let trace = self.trace_player_bbox(self.mv.origin, new_origin);
        self.player.ducked = save_ducked;

        !(trace.startsolid || trace.fraction != 1.0)
    }

    /// `CGameMovement::FinishUnDuck`.
    fn finish_unduck(&mut self) {
        let mut new_origin = self.mv.origin;

        if self.player.ground.is_some() {
            for i in 0..3 {
                let v = new_origin.get(i) + (VEC_DUCK_HULL_MIN.get(i) - VEC_HULL_MIN.get(i));
                new_origin.set(i, v);
            }
        } else {
            let hull_size_normal = VEC_HULL_MAX - VEC_HULL_MIN;
            let hull_size_crouch = VEC_DUCK_HULL_MAX - VEC_DUCK_HULL_MIN;
            let view_delta = -(hull_size_normal - hull_size_crouch);
            new_origin += view_delta;
        }

        self.player.ducked = false;
        self.player.flags &= !FL_DUCKING;
        self.player.ducking = false;
        self.player.in_duck_jump = false;
        self.player.view_offset = self.player_view_offset(false);
        self.player.duck_time = 0.0;

        self.mv.origin = new_origin;

        // Recategorize position since ducking can change origin
        self.categorize_position();
    }

    /// `CGameMovement::FinishDuck`.
    fn finish_duck(&mut self) {
        if self.player.flags & FL_DUCKING != 0 {
            return;
        }

        self.player.flags |= FL_DUCKING;
        self.player.ducked = true;
        self.player.ducking = false;

        self.player.view_offset = self.player_view_offset(true);

        // HACKHACK - Fudge for collision bug - no time to fix this properly
        if self.player.ground.is_some() {
            for i in 0..3 {
                let v = self.mv.origin.get(i) - (VEC_DUCK_HULL_MIN.get(i) - VEC_HULL_MIN.get(i));
                self.mv.origin.set(i, v);
            }
        } else {
            let hull_size_normal = VEC_HULL_MAX - VEC_HULL_MIN;
            let hull_size_crouch = VEC_DUCK_HULL_MAX - VEC_DUCK_HULL_MIN;
            let view_delta = hull_size_normal - hull_size_crouch;
            self.mv.origin += view_delta;
        }

        // See if we are stuck?
        self.fix_player_crouch_stuck(true);

        // Recategorize position since ducking can change origin
        self.categorize_position();
    }

    /// `CGameMovement::FixPlayerCrouchStuck`.
    fn fix_player_crouch_stuck(&mut self, upward: bool) {
        let direction = if upward { 1.0 } else { 0.0 };

        if self.test_player_position(self.mv.origin).is_none() {
            return;
        }

        let test = self.mv.origin;
        for _ in 0..36 {
            self.mv.origin.z += direction;
            if self.test_player_position(self.mv.origin).is_none() {
                return;
            }
        }

        self.mv.origin = test; // Failed
    }

    /// `CTFGameMovement::DuckOverrides`.
    fn duck_overrides(&mut self) {
        let on_ground = self.player.ground.is_some();

        // tf_clamp_airducks is 1.
        // Check the duck timer and disable the duck button.
        if self.curtime < self.player.duck_timer && on_ground {
            self.mv.buttons &= !IN_DUCK;
        }

        // If we're trying to stand up, don't let the player try to re-duck ("Quantum Crouch").
        if self.player.ducked && self.player.ducking {
            self.mv.buttons &= !IN_DUCK;
        }

        // Only allow one duck per air event.
        if !on_ground && self.player.air_ducked >= TF_AIRDUCKED_COUNT {
            self.mv.buttons &= !IN_DUCK;
        }
    }

    /// `CTFGameMovement::OnDuck`.
    fn on_duck(&mut self, buttons_pressed: u32) {
        let in_air = self.player.ground.is_none();
        let in_duck = self.player.flags & FL_DUCKING != 0;

        // Have the duck button pressed, but the player currently isn't in the duck position.
        if buttons_pressed & IN_DUCK != 0 && !in_duck {
            self.player.duck_time = GAMEMOVEMENT_DUCK_TIME;
            self.player.ducking = true;
        }

        // The player is in duck transition and not duck-jumping.
        if self.player.ducking {
            let duck_ms = fmax(0.0, GAMEMOVEMENT_DUCK_TIME - self.player.duck_time);
            let duck_seconds = duck_ms * 0.001;

            // Finish in duck transition when transition time is over, in "duck", in air.
            if duck_seconds > TIME_TO_DUCK || in_duck || in_air {
                self.finish_duck();
            } else {
                // Calc parametric time
                let frac = simple_spline(duck_seconds / TIME_TO_DUCK);
                self.set_ducked_eye_offset(frac);
            }
        }
    }

    /// `CTFGameMovement::OnUnDuck`.
    fn on_unduck(&mut self, buttons_released: u32) {
        let in_air = self.player.ground.is_none();
        let in_duck = self.player.flags & FL_DUCKING != 0;

        // Once the duck button is released, start a timer. The player will not be able to engage in a duck
        // until the timer expires. In addition, set that we have ducked in air (will be allowed only once while in air).
        if buttons_released & IN_DUCK != 0 {
            self.player.duck_timer = self.curtime + TF_TIME_TO_DUCK;
            if in_air {
                self.player.air_ducked += 1;
            }
        }

        // Try to unduck unless automovement is not allowed (always allowed for players).
        {
            // We released the duck button, we aren't in "duck" and we are not in the air - start unduck transition.
            if buttons_released & IN_DUCK != 0 {
                if in_duck {
                    self.player.duck_time = GAMEMOVEMENT_DUCK_TIME;
                } else if self.player.ducking && !self.player.ducked {
                    // Invert time if release before fully ducked!!!
                    let unduck_ms = 1000.0 * TIME_TO_UNDUCK;
                    let duck_ms = 1000.0 * TIME_TO_DUCK;
                    let elapsed_ms = GAMEMOVEMENT_DUCK_TIME - self.player.duck_time;

                    let frac_ducked = elapsed_ms / duck_ms;
                    let remaining_unduck_ms = frac_ducked * unduck_ms;

                    self.player.duck_time = GAMEMOVEMENT_DUCK_TIME - unduck_ms + remaining_unduck_ms;
                }
            }

            // Check to see if we are capable of unducking.
            if self.can_unduck() {
                // or unducking
                if self.player.ducking || self.player.ducked {
                    let duck_ms = fmax(0.0, GAMEMOVEMENT_DUCK_TIME - self.player.duck_time);
                    let duck_seconds = duck_ms * 0.001;

                    // Finish ducking immediately if duck time is over or not on ground
                    if duck_seconds > TIME_TO_UNDUCK || in_air {
                        self.finish_unduck();
                    } else {
                        // Calc parametric time
                        let frac = simple_spline(1.0 - (duck_seconds / TIME_TO_UNDUCK));
                        self.set_ducked_eye_offset(frac);
                        self.player.ducking = true;
                    }
                }
            } else {
                // Still under something where we can't unduck, so make sure we reset this timer so
                //  that we'll unduck once we exit the tunnel, etc.
                if self.player.duck_time != GAMEMOVEMENT_DUCK_TIME {
                    self.set_ducked_eye_offset(1.0);
                    self.player.duck_time = GAMEMOVEMENT_DUCK_TIME;
                    self.player.ducked = true;
                    self.player.ducking = false;
                    self.player.flags |= FL_DUCKING;
                }
            }
        }
    }

    /// `CTFGameMovement::Duck`.
    fn duck(&mut self) {
        // Check duck overrides.
        self.duck_overrides();

        // Calculate the button state.
        let buttons_changed = self.mv.old_buttons ^ self.mv.buttons;
        let buttons_pressed = buttons_changed & self.mv.buttons;
        let buttons_released = buttons_changed & self.mv.old_buttons;
        if self.mv.buttons & IN_DUCK != 0 {
            self.mv.old_buttons |= IN_DUCK;
        } else {
            self.mv.old_buttons &= !IN_DUCK;
        }

        // Slow down ducked players.
        self.handle_ducking_speed_crop();

        // If the player is holding down the duck button, the player is in duck transition, ducking, or duck-jumping.
        let in_duck = self.player.flags & FL_DUCKING != 0;
        if self.mv.buttons & IN_DUCK != 0 || self.player.ducking || in_duck {
            if self.mv.buttons & IN_DUCK != 0 {
                // DUCK (CanDuck() is true for a plain soldier)
                self.on_duck(buttons_pressed);
            } else {
                // UNDUCK (or attempt to...)
                self.on_unduck(buttons_released);
            }
        } else {
            // Restore the view height if it somehow got left at the ducked height.
            let offset_delta = self.player.view_offset.z - self.player_view_offset(false).z;
            if fabsf(offset_delta) > 0.1 {
                self.set_ducked_eye_offset(0.0);
            }
        }
    }
}

/// Convenience: run a full movement command for `player` with `input`.
pub fn run_player_move(player: &mut Player, input: &PlayerInput, env: TraceEnv, curtime: f32) {
    let mut mv = MoveData::setup(player, input);
    {
        let mut gm = GameMovement::new(player, &mut mv, env, curtime);
        gm.process_movement();
    }
    mv.finish(player);
}
