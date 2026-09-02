//! Shader / GPU / JIT warm-up. Bevy specialises and compiles render pipelines the first time
//! something needs them (skinned meshes, the additive and alpha-blended particle materials, the
//! multiply decal, shadow passes...), which on the web means the first rocket or explosion freezes
//! the game for a moment. Before the loading screen goes away, every material and mesh kind the
//! match uses is drawn for a few frames by a camera that renders into a small off-screen texture,
//! with the same view settings as the real cameras so the same pipeline variants get built. The
//! models are animated meanwhile and the simulation is stepped through a few hundred ticks, so
//! the animation, skinning and physics code has run (and, in a browser, been tiered up by the
//! wasm JIT) before the first match.
//!
//! The scene is built one step per frame rather than all at once. On WebGL2 a pipeline is a
//! synchronous shader link that can take seconds per variant on some drivers (Firefox on Apple's
//! OpenGL stack was measured at 34 s for the whole set), and spawning everything in one frame froze
//! the page for that long with the loading bar stuck. One step per frame means one or two links per
//! frame, with the loading screen repainted and progress reported in between: `WarmupProgress`
//! feeds the steps into the loading screen's count (`loading.rs`).

use crate::assets::{GameAssets, SPRITE_ADDITIVE, SPRITE_COUNT};
use crate::loading::{elapsed_secs, settled};
use crate::render::{UNIT, tf2_look};
use crate::viewmodel;
use bevy::animation::RepeatAnimation;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{ImageRenderTarget, RenderTarget};
use bevy::gltf::Gltf;
use bevy::image::Image;
use bevy::light::{DirectionalLight, NotShadowCaster, PointLight};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::world_serialization::WorldInstanceReady;
use endif_sim::{Arena, IN_ATTACK, IN_DUCK, IN_FORWARD, IN_JUMP, PlayerInput, Rules, SimState};

/// Render layer nothing else uses.
const LAYER: usize = 7;
/// Frames to keep the warm-up scene alive once everything has spawned.
const FRAMES: u32 = 12;
/// The glTF scenes: soldier, viewmodel, rocket.
const SCENES: usize = 3;
const SCENE_NAMES: [&str; SCENES] = ["soldier", "viewmodel", "rocket"];
const SCENE_POS: [Vec3; SCENES] = [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 2.0), Vec3::new(0.0, 1.0, 1.0)];
/// Arena surfaces: floor, wall.
const SURFACES: usize = 2;
/// Spawn steps, one per frame: cameras, lights, the scenes, the surfaces, the emissive line, the
/// scorch decal, the particle sprites.
const STEPS: u32 = (2 + SCENES + SURFACES + 2 + SPRITE_COUNT) as u32;

/// Present once the warm-up scene has been rendered.
#[derive(Resource)]
pub struct WarmupDone;

/// Steps done / total (spawn steps plus rendered frames); the loading screen counts these.
#[derive(Resource)]
pub struct WarmupProgress {
    pub done: u32,
    pub total: u32,
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Cameras,
    Lights,
    Scene(usize),
    Surface(usize),
    Emissive,
    Scorch,
    Sprite(usize),
}

fn step(mut i: usize) -> Step {
    if i == 0 {
        return Step::Cameras;
    }
    i -= 1;
    if i == 0 {
        return Step::Lights;
    }
    i -= 1;
    if i < SCENES {
        return Step::Scene(i);
    }
    i -= SCENES;
    if i < SURFACES {
        return Step::Surface(i);
    }
    i -= SURFACES;
    if i == 0 {
        return Step::Emissive;
    }
    i -= 1;
    if i == 0 {
        return Step::Scorch;
    }
    Step::Sprite(i - 1)
}

#[derive(Component)]
struct WarmupEntity;

/// Handles several steps share.
#[derive(Clone)]
struct Shared {
    target: Handle<Image>,
    quad: Handle<Mesh>,
    slab: Handle<Mesh>,
}

#[derive(Resource, Default)]
struct WarmupState {
    /// Next step to run.
    next: u32,
    /// A spawned glTF scene has not instantiated yet.
    scene_pending: bool,
    scenes_ready: u32,
    frames: u32,
    shared: Option<Shared>,
}

pub struct WarmupPlugin;

impl Plugin for WarmupPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(WarmupProgress { done: 0, total: STEPS + FRAMES })
            .init_resource::<WarmupState>()
            .add_systems(Update, warmup.run_if(not(resource_exists::<WarmupDone>)));
    }
}

fn quad(size: f32) -> Mesh {
    let h = size * 0.5;
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[-h, -h, 0.0], [h, -h, 0.0], [h, h, 0.0], [-h, h, 0.0]]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0, 1.0, 1.0, 0.8]; 4]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

fn surface(assets: &GameAssets, i: usize) -> &Handle<Image> {
    [&assets.floor, &assets.wall][i]
}

/// Whether the assets a step draws have arrived (or failed, which must not hold the screen up).
fn ready(step: Step, server: &AssetServer, assets: &GameAssets) -> bool {
    match step {
        Step::Cameras | Step::Lights | Step::Emissive => true,
        // Resolved from the loaded file, or the file failed (then the step is skipped).
        Step::Scene(i) => assets.scene(i).is_some() || settled(server, assets.gltf(i).id()),
        Step::Surface(i) => settled(server, surface(assets, i).id()),
        Step::Scorch => settled(server, assets.scorch.id()),
        Step::Sprite(i) => settled(server, assets.sprites[i].id()),
    }
}

#[allow(clippy::too_many_arguments)]
fn warmup(
    mut commands: Commands,
    mut state: ResMut<WarmupState>,
    mut progress: ResMut<WarmupProgress>,
    server: Res<AssetServer>,
    assets: Res<GameAssets>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    entities: Query<Entity, With<WarmupEntity>>,
) {
    if state.next < STEPS {
        // A glTF scene instantiates a frame or two after it is spawned; its materials get their
        // own frame before the next step adds more.
        if state.scene_pending {
            return;
        }
        let step = step(state.next as usize);
        if !ready(step, &server, &assets) {
            return;
        }
        if state.next == 0 {
            warm_sim();
            info!("[startup {:.2}s] simulation warm-up done", elapsed_secs());
        }
        let shared = state
            .shared
            .get_or_insert_with(|| Shared {
                target: images.add(Image::new_target_texture(64, 64, TextureFormat::Rgba8UnormSrgb, None)),
                quad: meshes.add(quad(1.0)),
                slab: meshes.add(Cuboid::new(6.0, 0.2, 6.0)),
            })
            .clone();
        if let Step::Scene(i) = step
            && assets.scene(i).is_none()
        {
            warn!("[startup {:.2}s] warm-up: no {} scene to draw (did the model fail to load?)", elapsed_secs(), SCENE_NAMES[i]);
            state.scenes_ready += 1;
        } else {
            spawn_step(step, &mut commands, &assets, &shared, &mut meshes, &mut materials);
            state.scene_pending = matches!(step, Step::Scene(_));
        }
        state.next += 1;
        progress.done = state.next;
        info!("[startup {:.2}s] warm-up step {}/{STEPS}: {step:?}", elapsed_secs(), state.next);
        return;
    }
    // Everything spawned and instantiated: render a few frames.
    if state.scenes_ready < SCENES as u32 {
        return;
    }
    state.frames += 1;
    progress.done = STEPS + state.frames;
    if state.frames >= FRAMES {
        for e in &entities {
            commands.entity(e).despawn();
        }
        commands.insert_resource(WarmupDone);
        info!("[startup {:.2}s] render warm-up done ({FRAMES} frames rendered)", elapsed_secs());
    }
}

fn spawn_step(
    step: Step,
    commands: &mut Commands,
    assets: &GameAssets,
    shared: &Shared,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let layer = RenderLayers::layer(LAYER);
    match step {
        // World camera and the viewmodel-style overlay camera, same settings as the match cameras.
        Step::Cameras => {
            commands.spawn((
                WarmupEntity,
                Camera3d::default(),
                Camera { order: -10, ..default() },
                RenderTarget::Image(ImageRenderTarget { handle: shared.target.clone(), scale_factor: 1.0 }),
                Projection::Perspective(PerspectiveProjection { fov: viewmodel::vertical_fov(90.0), near: 0.03, ..default() }),
                tf2_look(),
                layer.clone(),
                Transform::from_xyz(0.0, 1.5, 6.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
            ));
            commands.spawn((
                WarmupEntity,
                Camera3d { depth_load_op: bevy::camera::Camera3dDepthLoadOp::Clear(0.0), ..default() },
                Camera { order: -9, clear_color: bevy::camera::ClearColorConfig::None, ..default() },
                RenderTarget::Image(ImageRenderTarget { handle: shared.target.clone(), scale_factor: 1.0 }),
                Projection::Perspective(PerspectiveProjection { fov: viewmodel::vertical_fov(viewmodel::VIEWMODEL_FOV), near: 0.01, ..default() }),
                tf2_look(),
                layer,
                Transform::from_xyz(0.0, 1.5, 6.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
            ));
        }
        // A shadow-casting sun (shadow pass pipelines) and a point light.
        Step::Lights => {
            commands.spawn((
                WarmupEntity,
                DirectionalLight { illuminance: 10_000.0, shadow_maps_enabled: true, ..default() },
                Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -1.0, 0.6, 0.0)),
                layer.clone(),
            ));
            commands.spawn((WarmupEntity, PointLight { intensity: 20_000.0, range: 6.0, ..default() }, Transform::from_xyz(0.5, 2.0, 1.0), layer));
        }
        // A glTF scene (skinned or static meshes, their textured materials).
        Step::Scene(i) => {
            commands
                .spawn((WarmupEntity, WorldAssetRoot(assets.scene(i).cloned().unwrap_or_default()), Transform::from_translation(SCENE_POS[i]), layer))
                .observe(on_scene_ready);
        }
        // Arena surfaces.
        Step::Surface(i) => {
            commands.spawn((
                WarmupEntity,
                Mesh3d(shared.slab.clone()),
                MeshMaterial3d(materials.add(StandardMaterial { base_color_texture: Some(surface(assets, i).clone()), perceptual_roughness: 0.95, ..default() })),
                Transform::from_xyz(0.0, -0.1, 0.0),
                layer,
            ));
        }
        // Emissive unlit (the red line).
        Step::Emissive => {
            commands.spawn((
                WarmupEntity,
                Mesh3d(meshes.add(Cuboid::new(4.0, 0.08, 0.02))),
                MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(1.0, 0.1, 0.1), emissive: LinearRgba::new(2.0, 0.1, 0.1, 1.0), unlit: true, ..default() })),
                Transform::from_xyz(0.0, 2.0, -1.0),
                layer,
            ));
        }
        // Scorch decal (multiply blend).
        Step::Scorch => {
            commands.spawn((
                WarmupEntity,
                Mesh3d(shared.quad.clone()),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color_texture: Some(assets.scorch.clone()),
                    alpha_mode: AlphaMode::Multiply,
                    unlit: true,
                    double_sided: true,
                    cull_mode: None,
                    depth_bias: -1.0,
                    ..default()
                })),
                NotShadowCaster,
                Transform::from_xyz(0.0, 0.5, 2.0),
                layer,
            ));
        }
        // Particle materials (unlit, vertex colours, blend / add).
        Step::Sprite(i) => {
            commands.spawn((
                WarmupEntity,
                Mesh3d(shared.quad.clone()),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color_texture: Some(assets.sprites[i].clone()),
                    alpha_mode: if SPRITE_ADDITIVE[i] { AlphaMode::Add } else { AlphaMode::Blend },
                    unlit: true,
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                })),
                NoFrustumCulling,
                NotShadowCaster,
                Transform::from_xyz(-1.5 + i as f32 * 0.5, 1.5, 2.0 + i as f32 * 0.1),
                layer,
            ));
        }
    }
    let _ = UNIT;
}

/// Steps a throwaway simulation through rocket jumps and shots so the movement, trace and
/// explosion code has all run once (milliseconds of work, seconds of hitching saved on the web
/// where the JIT compiles wasm functions properly only once they are hot).
fn warm_sim() {
    let arena = Arena::classic_square();
    let mut sim = SimState::new(1, Rules::default());
    for i in 0..400u32 {
        let jumping = i % 60 < 3;
        let a = PlayerInput {
            buttons: if jumping { IN_ATTACK | IN_JUMP | IN_DUCK } else { IN_FORWARD },
            pitch: if jumping { 89.0 } else { 0.0 },
            yaw: 180.0,
        };
        let b = PlayerInput { buttons: if i % 40 == 0 { IN_ATTACK } else { 0 }, pitch: -15.0, yaw: 0.0 };
        sim.step(&arena, [a, b]);
    }
    debug!("simulation warm-up done (checksum {:#x})", std::hint::black_box(sim.checksum()));
}

/// Puts every mesh of a spawned warm-up scene on the warm-up layer, and starts the animations
/// the match will play so the skinning / additive blending paths run.
fn on_scene_ready(
    trigger: On<WorldInstanceReady>,
    mut commands: Commands,
    mut state: ResMut<WarmupState>,
    assets: Res<GameAssets>,
    gltfs: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    roots: Query<&WorldAssetRoot>,
    children: Query<&Children>,
    meshes: Query<(), With<Mesh3d>>,
    mut players: Query<&mut AnimationPlayer>,
) {
    state.scene_pending = false;
    state.scenes_ready += 1;
    let root = trigger.event().entity;
    let id = roots.get(root).map(|r| r.0.id()).ok();
    let which = (0..SCENES).find(|&i| id.is_some() && id == assets.scene(i).map(|h| h.id()));
    let (gltf, clips): (Option<&Gltf>, &[&str]) = match which {
        Some(0) => (gltfs.get(&assets.soldier), &["stand_PRIMARY", "a_runN_PRIMARY", "AttackStand_PRIMARY"]),
        Some(1) => (gltfs.get(&assets.viewmodel), &["dh_idle", "dh_fire"]),
        _ => (None, &[]),
    };
    info!(
        "[startup {:.2}s] warm-up scene {} instantiated ({}/{SCENES})",
        elapsed_secs(),
        which.map_or("?", |i| SCENE_NAMES[i]),
        state.scenes_ready
    );
    let graph = gltf.map(|gltf| {
        let mut g = AnimationGraph::new();
        let layers = g.add_additive_blend(1.0, g.root);
        let nodes: Vec<_> = clips.iter().filter_map(|n| gltf.named_animations.get(*n)).map(|c| g.add_clip(c.clone(), 1.0, layers)).collect();
        (graphs.add(g), nodes)
    });
    for e in children.iter_descendants(root) {
        if meshes.contains(e) {
            commands.entity(e).insert(RenderLayers::layer(LAYER));
        }
        if let Ok(mut player) = players.get_mut(e)
            && let Some((graph, nodes)) = graph.clone()
        {
            for n in nodes {
                player.play(n).set_repeat(RepeatAnimation::Forever);
            }
            commands.entity(e).insert(AnimationGraphHandle(graph));
        }
    }
}
