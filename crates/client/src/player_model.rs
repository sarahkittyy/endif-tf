//! Third-person soldier: the TF2 model with its animation set, driven from the simulation state
//! the way `CTFPlayerAnimState` / `CMultiPlayerAnimState` do it: a 9-way run blend on
//! `move_x`/`move_y`, crouch / crouch-walk, jump float, additive attack / land gestures, and the
//! aim matrix on `body_pitch`.
//!
//! The aim matrix is not part of the Bevy animation graph. Bevy composes additive layers as
//! `delta * base` where Source's `SlerpBones` does `base * delta`; that only agrees for bones near
//! the bind pose, and the arms are nowhere near it, so the launcher and the hands drifted apart at
//! steep pitches. Instead the aim poses (single frames, exported by `tools/tf2/build_assets.py` to
//! `soldier_aim.json` and compiled in) are applied in `PostUpdate` after the graph has been
//! evaluated, exactly like Source, followed by the `ikrule lhand touch weapon_bone` rule those
//! sequences carry: a two-bone IK that keeps the left hand on the launcher's grip whatever the
//! rest of the pose does.

use crate::assets::GameAssets;
use crate::game::{PendingFx, RenderStates};
use crate::render::PlayerVis;
use crate::{AppState, GameEntity};
use bevy::animation::RepeatAnimation;
use bevy::animation::graph::AnimationNodeIndex;
use bevy::app::AnimationSystems;
use bevy::gltf::{Gltf, GltfMaterialName};
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy::world_serialization::WorldInstanceReady;
use endif_sim::{NUM_PLAYERS, SOLDIER_MAX_SPEED, SimEvent};
use serde::Deserialize;
use std::collections::HashMap;

/// Crouch-walk speed: `TF_PLAYER_CROUCH_SPEED_MULT` (1/3) of the class speed.
const CROUCH_SPEED: f32 = SOLDIER_MAX_SPEED / 3.0;
/// Grid order of `run_PRIMARY` / `crouch_walk_PRIMARY` (rows: move_x -1..1, columns: move_y -1..1).
const GRID: [&str; 9] = ["SW", "S", "SE", "W", "Center", "E", "NW", "N", "NE"];
/// `body_pitch` of the aim-matrix rows in `soldier_aim.json` (straight_up, up, mid, down; the
/// centre column, since the body always faces the aim yaw).
const AIM_ROW_PITCH: [f32; 4] = [90.0, 45.0, 0.0, -45.0];
/// Aim sets in `soldier_aim.json`: `PRIMARY_aimmatrix_idle`, `_run`, `_crouch_idle`.
const AIM_SET_IDLE: usize = 0;
const AIM_SET_RUN: usize = 1;
const AIM_SET_CROUCH: usize = 2;

/// Aim-matrix poses and the left-hand IK rule from `soldier_animations.mdl`.
const AIM_JSON: &str = include_str!("../assets/models/soldier_aim.json");

#[derive(Deserialize)]
struct AimFile {
    /// Upper arm, lower arm, hand.
    ik_chain: [String; 3],
    /// The bone the hand is locked to (`weapon_bone`) and the hand's pose in that bone's space.
    ik_target: String,
    ik_pos: [f32; 3],
    ik_rot: [f32; 4],
    /// `sets[set][row]`: bone name → position delta (xyz) and rotation delta (xyzw), in the
    /// bone's local space, the sequence weightlist already applied.
    sets: Vec<Vec<HashMap<String, [f32; 7]>>>,
}

#[derive(Resource)]
struct AimData(AimFile);

impl Default for AimData {
    fn default() -> Self {
        AimData(serde_json::from_str(AIM_JSON).expect("soldier_aim.json"))
    }
}

#[derive(Component)]
pub struct PlayerModel(pub u8);

#[derive(Clone)]
struct SoldierNodes {
    stand: AnimationNodeIndex,
    crouch: AnimationNodeIndex,
    float: AnimationNodeIndex,
    run: [AnimationNodeIndex; 9],
    run_dur: [f32; 9],
    walk: [AnimationNodeIndex; 9],
    walk_dur: [f32; 9],
    attack_stand: AnimationNodeIndex,
    attack_crouch: AnimationNodeIndex,
    land: AnimationNodeIndex,
}

#[derive(Resource, Default)]
struct SoldierGraph(Option<(Handle<AnimationGraph>, SoldierNodes)>);

#[derive(Component)]
struct SoldierAnim {
    player: u8,
    nodes: SoldierNodes,
    /// Smoothed locomotion weights: [stand, crouch, float, run×9, walk×9].
    weights: [f32; 21],
    phase: f32,
    /// Aim matrix: set, lower row, and the blend toward the row below it.
    aim_set: usize,
    aim_row: usize,
    aim_t: f32,
}

/// A rigid transform (bones carry no scale), composed like Source's bone matrices.
#[derive(Clone, Copy)]
struct Frame {
    rot: Quat,
    pos: Vec3,
}

impl Frame {
    const IDENTITY: Frame = Frame { rot: Quat::IDENTITY, pos: Vec3::ZERO };

    fn of(t: &Transform) -> Frame {
        Frame { rot: t.rotation, pos: t.translation }
    }

    /// `self * child`: the child's frame expressed in the parent's parent space.
    fn then(self, child: Frame) -> Frame {
        Frame { rot: self.rot * child.rot, pos: self.pos + self.rot * child.pos }
    }
}

/// One aim set resolved to bone entities: `rows[r][i]` is the delta of `bones[i]` in row `r`
/// (identity where the pose has no key for that bone).
struct AimSet {
    bones: Vec<Entity>,
    rows: [Vec<(Vec3, Quat)>; 4],
}

struct IkChain {
    /// The skeleton root; chain and target frames are computed relative to it.
    root: Entity,
    upper: Entity,
    lower: Entity,
    hand: Entity,
    target: Entity,
    /// The hand in the target bone's space.
    offset: Frame,
}

/// Bone entities of one spawned soldier for `apply_soldier_pose`.
#[derive(Component)]
struct SoldierPose {
    sets: Vec<AimSet>,
    ik: Option<IkChain>,
}

pub struct PlayerModelPlugin;

impl Plugin for PlayerModelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoldierGraph>()
            .init_resource::<AimData>()
            .add_systems(OnEnter(AppState::InGame), spawn_player_models)
            .add_systems(Update, drive_soldier_anims.run_if(in_state(AppState::InGame)))
            .add_systems(
                PostUpdate,
                apply_soldier_pose.after(AnimationSystems).before(TransformSystems::Propagate).run_if(in_state(AppState::InGame)),
            );
    }
}

fn spawn_player_models(mut commands: Commands, assets: Res<GameAssets>) {
    for i in 0..NUM_PLAYERS {
        commands
            .spawn((
                GameEntity,
                PlayerVis(i as u8),
                PlayerModel(i as u8),
                WorldAssetRoot(assets.soldier_scene()),
                Transform::default(),
                Visibility::Hidden,
            ))
            .observe(on_soldier_ready);
    }
}

fn build_graph(gltf: &Gltf, clips: &Assets<AnimationClip>, graphs: &mut Assets<AnimationGraph>) -> Option<(Handle<AnimationGraph>, SoldierNodes)> {
    let clip = |name: &str| gltf.named_animations.get(name).cloned();
    let dur = |h: &Handle<AnimationClip>| clips.get(h).map(|c| c.duration()).unwrap_or(0.7);
    // Bevy's additive node treats its first child as the base pose and adds the rest on top, so
    // the locomotion blend must be the first child of a single additive node; the gesture clips
    // are its later children.
    let mut g = AnimationGraph::new();
    let layers = g.add_additive_blend(1.0, g.root);
    let loco = g.add_blend(1.0, layers);
    let gesture = layers;
    let add = |g: &mut AnimationGraph, name: &str, parent: AnimationNodeIndex| -> Option<(AnimationNodeIndex, f32)> {
        let c = clip(name)?;
        let d = dur(&c);
        Some((g.add_clip(c, 1.0, parent), d))
    };
    let stand = add(&mut g, "stand_PRIMARY", loco)?.0;
    let crouch = add(&mut g, "crouch_PRIMARY", loco)?.0;
    let float = add(&mut g, "jump_float_PRIMARY", loco)?.0;
    let mut run = [stand; 9];
    let mut run_dur = [0.7; 9];
    let mut walk = [stand; 9];
    let mut walk_dur = [0.7; 9];
    for (i, dir) in GRID.iter().enumerate() {
        let (n, d) = add(&mut g, &format!("a_run{dir}_PRIMARY"), loco)?;
        run[i] = n;
        run_dur[i] = d;
        let (n, d) = add(&mut g, &format!("a_crouch_walk{dir}_PRIMARY"), loco)?;
        walk[i] = n;
        walk_dur[i] = d;
    }
    let attack_stand = add(&mut g, "AttackStand_PRIMARY", gesture)?.0;
    let attack_crouch = add(&mut g, "AttackCrouch_PRIMARY", gesture)?.0;
    let land = add(&mut g, "jumpland_PRIMARY", gesture)?.0;
    let nodes = SoldierNodes { stand, crouch, float, run, run_dur, walk, walk_dur, attack_stand, attack_crouch, land };
    Some((graphs.add(g), nodes))
}

/// Resolves the aim poses and the IK chain against a spawned skeleton's bone entities (glTF
/// nodes are named after the Source bones).
fn resolve_pose(aim: &AimFile, by_name: &HashMap<String, Entity>, fallback_root: Entity) -> SoldierPose {
    let sets = aim
        .sets
        .iter()
        .map(|rows| {
            let mut bones: Vec<(String, Entity)> =
                rows.iter().flat_map(|r| r.keys()).filter_map(|n| by_name.get(n).map(|e| (n.clone(), *e))).collect();
            bones.sort();
            bones.dedup();
            let rows: [Vec<(Vec3, Quat)>; 4] = std::array::from_fn(|r| {
                bones
                    .iter()
                    .map(|(n, _)| {
                        rows.get(r)
                            .and_then(|m| m.get(n))
                            .map(|v| (Vec3::new(v[0], v[1], v[2]), Quat::from_xyzw(v[3], v[4], v[5], v[6]).normalize()))
                            .unwrap_or((Vec3::ZERO, Quat::IDENTITY))
                    })
                    .collect()
            });
            AimSet { bones: bones.into_iter().map(|(_, e)| e).collect(), rows }
        })
        .collect();
    let bone = |name: &str| by_name.get(name).copied();
    let ik = (|| {
        Some(IkChain {
            root: bone("root").unwrap_or(fallback_root),
            upper: bone(&aim.ik_chain[0])?,
            lower: bone(&aim.ik_chain[1])?,
            hand: bone(&aim.ik_chain[2])?,
            target: bone(&aim.ik_target)?,
            offset: Frame {
                rot: Quat::from_xyzw(aim.ik_rot[0], aim.ik_rot[1], aim.ik_rot[2], aim.ik_rot[3]).normalize(),
                pos: Vec3::from(aim.ik_pos),
            },
        })
    })();
    if ik.is_none() {
        warn!("soldier.glb: IK chain bones {:?} / {} not found, hand will not follow the launcher", aim.ik_chain, aim.ik_target);
    }
    SoldierPose { sets, ik }
}

/// Wires the freshly spawned model: animation graph and the aim/IK bone tables on the node the
/// glTF loader gave an `AnimationPlayer`, and the blue team's textures for player 1.
#[allow(clippy::too_many_arguments)]
fn on_soldier_ready(
    trigger: On<WorldInstanceReady>,
    mut commands: Commands,
    assets: Res<GameAssets>,
    aim: Res<AimData>,
    gltfs: Res<Assets<Gltf>>,
    clips: Res<Assets<AnimationClip>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut cache: ResMut<SoldierGraph>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    models: Query<&PlayerModel>,
    children: Query<&Children>,
    names: Query<&Name>,
    players: Query<(), With<AnimationPlayer>>,
    mats: Query<(&GltfMaterialName, &MeshMaterial3d<StandardMaterial>)>,
) {
    let root = trigger.event().entity;
    let Ok(model) = models.get(root) else { return };
    if cache.0.is_none() {
        if let Some(gltf) = gltfs.get(&assets.soldier) {
            cache.0 = build_graph(gltf, &clips, &mut graphs);
        }
        if cache.0.is_none() {
            warn!("soldier.glb: animation clips missing, model will not animate");
        }
    }
    let blue = model.0 == 1;
    let mut by_name: HashMap<String, Entity> = HashMap::new();
    for e in children.iter_descendants(root) {
        if let Ok(name) = names.get(e) {
            by_name.entry(name.as_str().to_string()).or_insert(e);
        }
        if players.contains(e)
            && let Some((graph, nodes)) = cache.0.clone()
        {
            let mut weights = [0.0; 21];
            weights[0] = 1.0;
            commands.entity(e).insert((
                AnimationGraphHandle(graph),
                SoldierAnim { player: model.0, nodes, weights, phase: 0.0, aim_set: AIM_SET_IDLE, aim_row: 2, aim_t: 0.0 },
            ));
        }
        if blue && let Ok((name, mat)) = mats.get(e) {
            let swap = match name.0.as_str() {
                "soldier_red" => Some(assets.soldier_blue.clone()),
                _ => None,
            };
            if let Some(tex) = swap
                && let Some(src) = materials.get(&mat.0)
            {
                let mut m = src.clone();
                m.base_color_texture = Some(tex);
                let handle = materials.add(m);
                commands.entity(e).insert(MeshMaterial3d(handle));
            }
        }
    }
    for e in children.iter_descendants(root) {
        if players.contains(e) {
            commands.entity(e).insert(resolve_pose(&aim.0, &by_name, e));
        }
    }
}

/// Bilinear weights of a 3×3 pose-parameter grid at (x, y) ∈ [-1, 1]².
fn grid_weights(x: f32, y: f32) -> [f32; 9] {
    let fx = (x.clamp(-1.0, 1.0) + 1.0).min(1.999);
    let fy = (y.clamp(-1.0, 1.0) + 1.0).min(1.999);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let mut w = [0.0; 9];
    w[y0 * 3 + x0] += (1.0 - tx) * (1.0 - ty);
    w[y0 * 3 + x0 + 1] += tx * (1.0 - ty);
    w[(y0 + 1) * 3 + x0] += (1.0 - tx) * ty;
    w[(y0 + 1) * 3 + x0 + 1] += tx * ty;
    w
}

fn drive_soldier_anims(
    states: Option<Res<RenderStates>>,
    fx: Res<PendingFx>,
    time: Res<Time<Real>>,
    mut q: Query<(&mut SoldierAnim, &mut AnimationPlayer)>,
) {
    let Some(states) = states else { return };
    let dt = time.delta_secs().min(0.1);
    let smoothing = 1.0 - (-dt * 12.0).exp();
    for (mut anim, mut player) in &mut q {
        let i = anim.player as usize;
        let p = &states.cur.players[i];
        let vel = p.velocity;
        let speed = (vel.x * vel.x + vel.y * vel.y).sqrt();
        let ground = p.on_ground();
        let ducked = p.ducked;
        let moving = ground && speed > 15.0;

        // Pose parameters (`ComputePoseParam_MoveYaw`): velocity in the body's frame, normalised
        // by the class speed, with `move_y` positive to the right.
        let (sy, cy) = p.view_angles.yaw.to_radians().sin_cos();
        let fwd = vel.x * cy + vel.y * sy;
        let left = -vel.x * sy + vel.y * cy;
        let max_speed = if ducked { CROUCH_SPEED } else { SOLDIER_MAX_SPEED };
        let norm = if speed > 0.0 { (speed / max_speed).min(1.0) / speed } else { 0.0 };
        let (move_x, move_y) = (fwd * norm, -left * norm);

        // Locomotion targets: [stand, crouch, float, run×9, walk×9].
        let mut target = [0.0f32; 21];
        if !ground {
            target[2] = 1.0;
        } else if moving {
            let w = grid_weights(move_y, move_x);
            let base = if ducked { 12 } else { 3 };
            target[base..base + 9].copy_from_slice(&w);
        } else if ducked {
            target[1] = 1.0;
        } else {
            target[0] = 1.0;
        }
        for k in 0..21 {
            anim.weights[k] += (target[k] - anim.weights[k]) * smoothing;
        }
        // Run cycles advance with the ground speed (`SetPlaybackRate`).
        let rate = if moving { (speed / max_speed).clamp(0.4, 1.6) } else { 1.0 };
        anim.phase += dt * rate;

        // Aim matrix: body_pitch = -eye pitch, rows at 90/45/0/-45; applied in `apply_soldier_pose`.
        let body_pitch = (-p.view_angles.pitch).clamp(AIM_ROW_PITCH[3], AIM_ROW_PITCH[0]);
        let f = ((AIM_ROW_PITCH[0] - body_pitch) / 45.0).clamp(0.0, 2.999);
        anim.aim_row = f.floor() as usize;
        anim.aim_t = f - anim.aim_row as f32;
        anim.aim_set = if ducked && ground {
            AIM_SET_CROUCH
        } else if moving {
            AIM_SET_RUN
        } else {
            AIM_SET_IDLE
        };

        let nodes = anim.nodes.clone();
        let weights = anim.weights;
        let phase = anim.phase;
        let mut apply = |node: AnimationNodeIndex, w: f32, seek: Option<f32>| {
            if w < 0.003 {
                player.stop(node);
                return;
            }
            let a = player.play(node);
            a.set_weight(w).set_repeat(RepeatAnimation::Forever);
            if let Some(t) = seek {
                a.set_seek_time(t);
            }
        };
        apply(nodes.stand, weights[0], None);
        apply(nodes.crouch, weights[1], None);
        apply(nodes.float, weights[2], None);
        for k in 0..9 {
            apply(nodes.run[k], weights[3 + k], Some(phase % nodes.run_dur[k].max(0.01)));
            apply(nodes.walk[k], weights[12 + k], Some(phase % nodes.walk_dur[k].max(0.01)));
        }

        // Gestures.
        for ev in &fx.events {
            match *ev {
                SimEvent::RocketFired { shooter, .. } if shooter as usize == i => {
                    let node = if ducked { nodes.attack_crouch } else { nodes.attack_stand };
                    player.stop(if ducked { nodes.attack_stand } else { nodes.attack_crouch });
                    player.start(node).set_weight(1.0).set_repeat(RepeatAnimation::Never);
                }
                SimEvent::Landed { player: who, .. } if who as usize == i => {
                    player.start(nodes.land).set_weight(1.0).set_repeat(RepeatAnimation::Never);
                }
                _ => {}
            }
        }
        for node in [nodes.attack_stand, nodes.attack_crouch, nodes.land] {
            if player.animation(node).is_some_and(|a| a.is_finished()) {
                player.stop(node);
            }
        }
    }
}

/// The pose of `entity` relative to `root`: the product of the local transforms from the root's
/// children down to it. `None` if `root` is not an ancestor.
fn frame_in_root(mut entity: Entity, root: Entity, parents: &Query<&ChildOf>, transforms: &Query<&mut Transform>) -> Option<Frame> {
    let mut chain = Vec::with_capacity(8);
    while entity != root {
        chain.push(entity);
        entity = parents.get(entity).ok()?.parent();
    }
    let mut frame = Frame::IDENTITY;
    for &e in chain.iter().rev() {
        frame = frame.then(Frame::of(transforms.get(e).ok()?));
    }
    Some(frame)
}

/// `CIKContext` for an `IK_SELF` rule: the hand goes to `target * offset`, the arm is re-bent to
/// reach it (`Studio_SolveIK`: reach clamped, the elbow kept in the plane the animation put it in),
/// and the hand takes the rule's orientation.
fn solve_hand_ik(ik: &IkChain, parents: &Query<&ChildOf>, transforms: &mut Query<&mut Transform>) -> Option<()> {
    let shoulder_parent = frame_in_root(parents.get(ik.upper).ok()?.parent(), ik.root, parents, transforms)?;
    let target = frame_in_root(ik.target, ik.root, parents, transforms)?.then(ik.offset);
    let upper = Frame::of(transforms.get(ik.upper).ok()?);
    let lower = Frame::of(transforms.get(ik.lower).ok()?);
    let hand = Frame::of(transforms.get(ik.hand).ok()?);
    let g_upper = shoulder_parent.then(upper);
    let g_lower = g_upper.then(lower);
    let g_hand = g_lower.then(hand);
    let (s, e, h) = (g_upper.pos, g_lower.pos, g_hand.pos);
    let a = (e - s).length();
    let b = (h - e).length();
    if a < 1e-3 || b < 1e-3 {
        return None;
    }

    // Reach limits as in `Studio_SolveIK`: no more than a hair short of straight, no closer than
    // a sharp bend allows (then along the animated direction).
    let mut d = target.pos - s;
    let mut dist = d.length();
    let max = (a + b) * 0.9998;
    let min = ((a - b).abs() * 1.15).max(a.min(b) * 0.15);
    if dist < min {
        d = (h - s).normalize_or_zero() * min;
        dist = min;
    } else if dist > max {
        d *= max / dist;
        dist = max;
    }
    let n = d / dist;
    if !n.is_normalized() {
        return None;
    }
    // Elbow: law of cosines along `n`, bent toward where the animation had it.
    let pole = e - s;
    let pole = pole - n * pole.dot(n);
    let pole = if pole.length_squared() > 1e-6 { pole.normalize() } else { n.any_orthonormal_vector() };
    let x = (a * a - b * b + dist * dist) / (2.0 * dist);
    let y = (a * a - x * x).max(0.0).sqrt();
    let new_elbow = s + n * x + pole * y;
    let new_hand = s + d;

    let from = (e - s) / a;
    let to = (new_elbow - s).normalize_or_zero();
    if !to.is_normalized() {
        return None;
    }
    let g_upper = Frame { rot: (Quat::from_rotation_arc(from, to) * g_upper.rot).normalize(), pos: s };
    let g_lower = g_upper.then(lower);
    let h2 = g_lower.then(hand).pos;
    let from = (h2 - g_lower.pos).normalize_or_zero();
    let to = (new_hand - g_lower.pos).normalize_or_zero();
    if !from.is_normalized() || !to.is_normalized() {
        return None;
    }
    let g_lower = Frame { rot: (Quat::from_rotation_arc(from, to) * g_lower.rot).normalize(), pos: g_lower.pos };

    transforms.get_mut(ik.upper).ok()?.rotation = shoulder_parent.rot.inverse() * g_upper.rot;
    transforms.get_mut(ik.lower).ok()?.rotation = g_upper.rot.inverse() * g_lower.rot;
    transforms.get_mut(ik.hand).ok()?.rotation = g_lower.rot.inverse() * target.rot;
    Some(())
}

/// After Bevy has written the graph's pose: the aim matrix (`base * delta`, rows blended by
/// `body_pitch`) and then the left-hand IK, before transforms propagate to the meshes.
fn apply_soldier_pose(q: Query<(&SoldierAnim, &SoldierPose)>, parents: Query<&ChildOf>, mut transforms: Query<&mut Transform>) {
    for (anim, pose) in &q {
        if let Some(set) = pose.sets.get(anim.aim_set) {
            let r0 = anim.aim_row.min(3);
            let r1 = (r0 + 1).min(3);
            let t = anim.aim_t;
            for (i, &bone) in set.bones.iter().enumerate() {
                let (p0, q0) = set.rows[r0][i];
                let (p1, q1) = set.rows[r1][i];
                if let Ok(mut tf) = transforms.get_mut(bone) {
                    tf.translation += p0.lerp(p1, t);
                    tf.rotation = (tf.rotation * q0.slerp(q1, t)).normalize();
                }
            }
        }
        if let Some(ik) = &pose.ik {
            solve_hand_ik(ik, &parents, &mut transforms);
        }
    }
}
