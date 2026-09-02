//! First-person viewmodel: `c_soldier_arms` with the bone-merged `c_rocketlauncher`, rendered by a
//! second camera (TF2 `viewmodel_fov` 54, depth cleared so it never clips into walls) on render
//! layer 1, playing the `dh_*` sequences.

use crate::assets::GameAssets;
use crate::game::PendingFx;
use crate::net::LocalHandle;
use crate::{AppState, GameEntity};
use bevy::animation::RepeatAnimation;
use bevy::animation::graph::AnimationNodeIndex;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera3dDepthLoadOp, ClearColorConfig};
use bevy::gltf::{Gltf, GltfMaterialName};
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::world_serialization::WorldInstanceReady;
use endif_sim::SimEvent;

/// Render layer of the viewmodel camera and meshes.
pub const VIEWMODEL_LAYER: usize = 1;
/// TF2 `viewmodel_fov` default, defined at 4:3 like `fov_desired`.
pub const VIEWMODEL_FOV: f32 = 54.0;

#[derive(Component)]
pub struct ViewmodelCamera;

#[derive(Component)]
pub struct ViewmodelRoot;

#[derive(Component)]
struct ViewmodelAnim {
    idle: AnimationNodeIndex,
    fire: AnimationNodeIndex,
    /// The one-shot currently playing (draw / fire), if any.
    one_shot: Option<AnimationNodeIndex>,
}

pub struct ViewmodelPlugin;

impl Plugin for ViewmodelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, drive_viewmodel.run_if(in_state(AppState::InGame)));
    }
}

/// Vertical FOV for a TF2-style horizontal FOV defined at 4:3.
pub fn vertical_fov(fov_4x3_deg: f32) -> f32 {
    2.0 * ((fov_4x3_deg * 0.5).to_radians().tan() * 0.75).atan()
}

/// Spawns the viewmodel camera and model as children of the main camera.
pub fn spawn(commands: &mut Commands, camera: Entity, assets: &GameAssets, team_blue: bool) {
    commands.entity(camera).with_children(|p| {
        p.spawn((
            GameEntity,
            ViewmodelCamera,
            Camera3d { depth_load_op: Camera3dDepthLoadOp::Clear(0.0), ..default() },
            Camera { order: 1, clear_color: ClearColorConfig::None, ..default() },
            crate::render::tf2_look(),
            Projection::Perspective(PerspectiveProjection { fov: vertical_fov(VIEWMODEL_FOV), near: 0.01, ..default() }),
            RenderLayers::layer(VIEWMODEL_LAYER),
            Transform::IDENTITY,
        ));
        // Key light riding with the camera, above and to the right: TF2 viewmodels always catch a
        // highlight from the map's sun/ambient cube, so give the launcher one regardless of where
        // the world sun happens to be. Viewmodel layer only.
        p.spawn((
            GameEntity,
            PointLight { color: Color::srgb(1.0, 0.97, 0.9), intensity: 40_000.0, range: 4.0, shadow_maps_enabled: false, ..default() },
            Transform::from_xyz(0.45, 0.7, 0.35),
            RenderLayers::layer(VIEWMODEL_LAYER),
        ));
        // The glTF root already maps Source (x fwd, y left, z up) to glTF (x, y up, -z); a further
        // +90° about Y turns glTF +X into the camera's -Z forward.
        p.spawn((
            GameEntity,
            ViewmodelRoot,
            Team(team_blue),
            WorldAssetRoot(assets.viewmodel_scene()),
            Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
            RenderLayers::layer(VIEWMODEL_LAYER),
        ))
        .observe(on_viewmodel_ready);
    });
}

#[derive(Component)]
struct Team(bool);

fn on_viewmodel_ready(
    trigger: On<WorldInstanceReady>,
    mut commands: Commands,
    assets: Res<GameAssets>,
    gltfs: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    teams: Query<&Team>,
    children: Query<&Children>,
    meshes: Query<(), With<Mesh3d>>,
    mats: Query<(&GltfMaterialName, &MeshMaterial3d<StandardMaterial>)>,
    mut players: Query<&mut AnimationPlayer>,
) {
    let root = trigger.event().entity;
    let blue = teams.get(root).map(|t| t.0).unwrap_or(false);
    let graph = gltfs.get(&assets.viewmodel).and_then(|gltf| {
        let mut g = AnimationGraph::new();
        let mut add = |name: &str| gltf.named_animations.get(name).map(|c| g.add_clip(c.clone(), 1.0, g.root));
        let idle = add("dh_idle")?;
        let fire = add("dh_fire")?;
        let draw = add("dh_draw")?;
        Some((graphs.add(g), idle, fire, draw))
    });
    for e in children.iter_descendants(root) {
        if meshes.contains(e) {
            commands.entity(e).insert((RenderLayers::layer(VIEWMODEL_LAYER), NotShadowCaster));
        }
        if let Ok(mut player) = players.get_mut(e)
            && let Some((graph, idle, fire, draw)) = graph.clone()
        {
            player.start(draw).set_repeat(RepeatAnimation::Never);
            commands.entity(e).insert((AnimationGraphHandle(graph), ViewmodelAnim { idle, fire, one_shot: Some(draw) }));
        }
        if blue && let Ok((name, mat)) = mats.get(e) {
            if name.0 == "soldier_sleeves_red"
                && let Some(src) = materials.get(&mat.0)
            {
                let mut m = src.clone();
                m.base_color_texture = Some(assets.sleeves_blue.clone());
                let handle = materials.add(m);
                commands.entity(e).insert(MeshMaterial3d(handle));
            }
        }
    }
    if graph.is_none() {
        warn!("viewmodel.glb: animation clips missing, viewmodel will not animate");
    }
}

fn drive_viewmodel(fx: Res<PendingFx>, local: Res<LocalHandle>, mut q: Query<(&mut ViewmodelAnim, &mut AnimationPlayer)>) {
    for (mut anim, mut player) in &mut q {
        let fired = fx.events.iter().any(|e| matches!(e, SimEvent::RocketFired { shooter, .. } if *shooter as usize == local.0));
        if fired {
            if let Some(n) = anim.one_shot {
                player.stop(n);
            }
            player.stop(anim.idle);
            player.start(anim.fire).set_repeat(RepeatAnimation::Never);
            anim.one_shot = Some(anim.fire);
        }
        if let Some(n) = anim.one_shot
            && player.animation(n).is_none_or(|a| a.is_finished())
        {
            player.stop(n);
            anim.one_shot = None;
            player.play(anim.idle).set_repeat(RepeatAnimation::Forever);
        }
        if anim.one_shot.is_none() && !player.is_playing_animation(anim.idle) {
            player.play(anim.idle).set_repeat(RepeatAnimation::Forever);
        }
    }
}
