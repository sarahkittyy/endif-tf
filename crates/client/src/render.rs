//! Presentation: arena geometry, rocket visuals, explosions, scorch decals and the first-person
//! camera. Everything here reads `RenderStates` and never touches the simulation. The player
//! models live in `player_model.rs`, the viewmodel in `viewmodel.rs`, particles in `particles.rs`.

use crate::assets::GameAssets;
use crate::game::{ArenaRes, LookAngles, PendingFx, RenderStates};
use crate::net::LocalHandle;
use crate::particles::Particles;
use crate::viewmodel::{self, VIEWMODEL_LAYER};
use crate::{AppState, GameEntity};
use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::render::view::{ColorGrading, ColorGradingGlobal, ColorGradingSection};
use bevy::light::{DirectionalLight, GlobalAmbientLight, NotShadowCaster, PointLight};
use bevy::math::Affine2;
use bevy::prelude::*;
use endif_sim::Vec3 as SVec3;
use endif_sim::math::{QAngle, angle_vectors};
use endif_sim::{SimEvent, SimState};

/// One Source unit is one inch.
pub const UNIT: f32 = 0.0254;

/// `w_rocket` is modelled along -Y in Source (nose at -Y, the `trail` attachment at +Y), which is
/// +Z after the glTF root transform.
pub const ROCKET_FORWARD: Vec3 = Vec3::Z;

/// Source (x, y, z-up) → Bevy (x, y-up, z).
pub fn to_bevy(v: SVec3) -> Vec3 {
    Vec3::new(v.x * UNIT, v.z * UNIT, -v.y * UNIT)
}

pub fn to_bevy_dir(v: SVec3) -> Vec3 {
    Vec3::new(v.x, v.z, -v.y)
}

/// Bevy camera rotation for Source view angles.
pub fn view_rotation(pitch: f32, yaw: f32) -> Quat {
    let (f, _, _) = angle_vectors(QAngle::new(pitch, yaw, 0.0));
    let dir = to_bevy_dir(f);
    Transform::default().looking_to(dir, Vec3::Y).rotation
}

#[derive(Component)]
pub struct PlayerVis(pub u8);

#[derive(Component)]
pub struct RocketVis(pub u32);

#[derive(Component)]
pub struct Explosion {
    pub t0: f64,
}

#[derive(Component)]
pub struct MainCamera;

/// TF2 renders straight to sRGB with no filmic curve, so the punchy, saturated look needs a
/// contrasty tonemapper plus a little saturation on top of Bevy's defaults. Shared by the world
/// and viewmodel cameras so the two passes match.
pub fn tf2_look() -> (Tonemapping, ColorGrading) {
    let section = ColorGradingSection { contrast: 1.1, ..default() };
    (
        Tonemapping::AcesFitted,
        ColorGrading {
            global: ColorGradingGlobal { post_saturation: 1.25, ..default() },
            shadows: section,
            midtones: section,
            highlights: section,
        },
    )
}

#[derive(Resource)]
pub struct Assets3d {
    burn_mesh: Handle<Mesh>,
    burn_mat: Handle<StandardMaterial>,
}

pub struct RenderPlugin;

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::srgb(0.53, 0.68, 0.85)))
            .insert_resource(GlobalAmbientLight { color: Color::srgb(0.9, 0.93, 1.0), brightness: 900.0, ..default() })
            .add_systems(OnEnter(AppState::InGame), setup_scene)
            .add_systems(
                Update,
                (sync_players, sync_rockets, spawn_fx, animate_explosions, update_camera)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    arena: Res<ArenaRes>,
    assets: Res<GameAssets>,
    local: Res<LocalHandle>,
) {
    let arena = &arena.0;
    // The visible walls are the outer (rocket) walls; players stop at the invisible inner walls.
    let h = arena.outer_half_size * UNIT;
    let span = arena.outer_half_size * 2.0;
    // The walls reach the ceiling, like the collision brushes.
    let wall_h = arena.ceiling * UNIT;

    // TF2 world textures default to 0.25 units per texel: floor.png (1024²) covers 256 units,
    // wall.png (512×1024, from concrete/wall010) covers 256 × 512 units.
    let floor_mat = materials.add(StandardMaterial {
        base_color_texture: Some(assets.floor.clone()),
        uv_transform: Affine2::from_scale(Vec2::splat(span / 256.0)),
        perceptual_roughness: 0.95,
        ..default()
    });
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Cuboid::new(h * 2.0, 0.2, h * 2.0))),
        MeshMaterial3d(floor_mat.clone()),
        Transform::from_xyz(0.0, -0.1, 0.0),
    ));
    // Ceiling slab: same concrete as the floor. It must not cast shadows or it would put the whole
    // arena in the shade of the sun.
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Cuboid::new(h * 2.0 + 0.6, 0.3, h * 2.0 + 0.6))),
        MeshMaterial3d(floor_mat),
        NotShadowCaster,
        Transform::from_xyz(0.0, wall_h + 0.15, 0.0),
    ));

    let wall_mat = materials.add(StandardMaterial {
        base_color_texture: Some(assets.wall.clone()),
        uv_transform: Affine2::from_scale(Vec2::new(span / 256.0, (wall_h / UNIT) / 512.0)),
        perceptual_roughness: 0.95,
        ..default()
    });
    let line_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.1, 0.1),
        emissive: LinearRgba::new(2.0, 0.1, 0.1, 1.0),
        unlit: true,
        ..default()
    });
    let line_y = arena.airshot_line_height * UNIT;
    let thickness = 0.3;
    let line_thickness = 0.08;
    let wall_specs: [(Vec3, Vec3); 4] = [
        (Vec3::new(h + thickness / 2.0, wall_h / 2.0, 0.0), Vec3::new(thickness, wall_h, h * 2.0)),
        (Vec3::new(-h - thickness / 2.0, wall_h / 2.0, 0.0), Vec3::new(thickness, wall_h, h * 2.0)),
        (Vec3::new(0.0, wall_h / 2.0, h + thickness / 2.0), Vec3::new(h * 2.0, wall_h, thickness)),
        (Vec3::new(0.0, wall_h / 2.0, -h - thickness / 2.0), Vec3::new(h * 2.0, wall_h, thickness)),
    ];
    for (pos, size) in wall_specs {
        commands.spawn((
            GameEntity,
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(wall_mat.clone()),
            Transform::from_translation(pos),
        ));
        // The airshot line, slightly proud of the wall surface so it never z-fights.
        let inward = -pos.with_y(0.0).normalize_or_zero() * (thickness / 2.0 + 0.01);
        let line_size = if size.x > size.z {
            Vec3::new(size.x, line_thickness, 0.02)
        } else {
            Vec3::new(0.02, line_thickness, size.z)
        };
        commands.spawn((
            GameEntity,
            Mesh3d(meshes.add(Cuboid::new(line_size.x, line_size.y, line_size.z))),
            MeshMaterial3d(line_mat.clone()),
            Transform::from_translation(Vec3::new(pos.x, line_y, pos.z) + inward),
        ));
    }

    // Sun + a soft sky fill from the opposite side (TF2 lights models with the ambient cube plus
    // the sun; a single directional light leaves the back of everything flat and dull). Both light
    // the world and the viewmodel layer.
    commands.spawn((
        GameEntity,
        DirectionalLight { illuminance: 15_000.0, shadow_maps_enabled: true, ..default() },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -1.0, 0.6, 0.0)),
        RenderLayers::from_layers(&[0, VIEWMODEL_LAYER]),
    ));
    commands.spawn((
        GameEntity,
        DirectionalLight { illuminance: 2_000.0, color: Color::srgb(0.85, 0.92, 1.0), shadow_maps_enabled: false, ..default() },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.7, 0.6 + std::f32::consts::PI, 0.0)),
        RenderLayers::from_layers(&[0, VIEWMODEL_LAYER]),
    ));
    // Floor bounce, shining straight up: the bottom face of Source's ambient cube. It is what
    // lights the underside of the ceiling (nothing else reaches it) and the undersides of models.
    commands.spawn((
        GameEntity,
        DirectionalLight { illuminance: 3_500.0, color: Color::srgb(0.95, 0.92, 0.88), shadow_maps_enabled: false, ..default() },
        Transform::default().looking_to(Vec3::Y, Vec3::X),
        RenderLayers::from_layers(&[0, VIEWMODEL_LAYER]),
    ));

    // Scorch decal: `decals/scorch1` is a DecalModulate material (dst × src × 2, baked into the
    // PNG), drawn with a multiply blend just above the surface.
    let burn_mesh = meshes.add(Rectangle::new(48.0 * UNIT, 48.0 * UNIT));
    let burn_mat = materials.add(StandardMaterial {
        base_color_texture: Some(assets.scorch.clone()),
        alpha_mode: AlphaMode::Multiply,
        unlit: true,
        double_sided: true,
        cull_mode: None,
        depth_bias: -1.0,
        ..default()
    });
    commands.insert_resource(Assets3d { burn_mesh, burn_mat });

    // Camera + viewmodel.
    let camera = commands
        .spawn((
            GameEntity,
            MainCamera,
            Camera3d::default(),
            tf2_look(),
            Projection::Perspective(PerspectiveProjection {
                // TF2 fov_desired 90 is defined at 4:3; keep the same vertical FOV on any aspect ratio.
                fov: viewmodel::vertical_fov(90.0),
                near: 0.03,
                ..default()
            }),
            Transform::from_xyz(0.0, 1.7, 0.0),
        ))
        .id();
    viewmodel::spawn(&mut commands, camera, &assets, local.0 == 1);
}

/// Ticks past the newest simulated state the view keeps moving at the last velocity when no new
/// state arrives (the peer's inputs are late and GGRS is waiting). Short stalls then read as
/// motion instead of a freeze, like `cl_extrapolate` in Source; after that the view holds, since
/// guessing further only makes the correction bigger when the frames do come.
const EXTRAPOLATE_TICKS: f64 = 2.0;
const TICK_SECS: f32 = 1.0 / crate::net::ROLLBACK_FPS as f32;

/// Position and view offset of player `i` at `alpha` ticks after the previous state: between the
/// two states up to 1, extrapolated from the current one beyond that.
fn lerp_state(prev: &SimState, cur: &SimState, alpha: f32, i: usize) -> (SVec3, SVec3) {
    let a = &prev.players[i];
    let b = &cur.players[i];
    // Don't interpolate across a respawn/teleport.
    if a.spawn_tick != b.spawn_tick || !a.alive || !b.alive {
        return (b.origin, b.view_offset);
    }
    if alpha > 1.0 {
        return (b.origin + b.velocity * ((alpha - 1.0) * TICK_SECS), b.view_offset);
    }
    let origin = a.origin + (b.origin - a.origin) * alpha;
    let view = a.view_offset + (b.view_offset - a.view_offset) * alpha;
    (origin, view)
}

/// Ticks since the previous state: 0..1 while a new state arrives every tick, up to
/// `1 + EXTRAPOLATE_TICKS` while waiting for one.
fn interp_alpha(states: &RenderStates, now: f64) -> f32 {
    let dt = 1.0 / crate::net::ROLLBACK_FPS as f64;
    ((now - states.last_advance) / dt).clamp(0.0, 1.0 + EXTRAPOLATE_TICKS) as f32
}

pub fn sync_players(
    states: Option<Res<RenderStates>>,
    local: Res<LocalHandle>,
    time: Res<Time<Real>>,
    mut q: Query<(&PlayerVis, &mut Transform, &mut Visibility)>,
) {
    let Some(states) = states else { return };
    let alpha = interp_alpha(&states, time.elapsed_secs_f64());
    for (vis, mut tf, mut visibility) in &mut q {
        let i = vis.0 as usize;
        let p = &states.cur.players[i];
        let (origin, _) = lerp_state(&states.prev, &states.cur, alpha, i);
        tf.translation = to_bevy(origin);
        // The model faces +X at yaw 0, like the Source model; the animations handle crouching.
        tf.rotation = Quat::from_rotation_y(p.view_angles.yaw.to_radians());
        *visibility = if i == local.0 || !p.alive { Visibility::Hidden } else { Visibility::Visible };
    }
}

pub fn sync_rockets(
    mut commands: Commands,
    states: Option<Res<RenderStates>>,
    assets: Res<GameAssets>,
    time: Res<Time<Real>>,
    mut particles: ResMut<Particles>,
    mut q: Query<(Entity, &RocketVis, &mut Transform)>,
) {
    let Some(states) = states else { return };
    let alpha = interp_alpha(&states, time.elapsed_secs_f64());
    let dt = time.delta_secs().min(0.1);
    let mut seen = Vec::new();
    for (entity, vis, mut tf) in &mut q {
        match states.cur.rockets.iter().find(|r| r.id == vis.0) {
            Some(r) => {
                seen.push(r.id);
                let prev = states.prev.rockets.iter().find(|p| p.id == r.id).map(|p| p.origin).unwrap_or(r.origin);
                let origin = if alpha > 1.0 { r.origin + r.velocity * ((alpha - 1.0) * TICK_SECS) } else { prev + (r.origin - prev) * alpha };
                let dir = to_bevy_dir(r.velocity).normalize_or_zero();
                let from = tf.translation;
                tf.translation = to_bevy(origin);
                tf.rotation = Quat::from_rotation_arc(ROCKET_FORWARD, dir);
                particles.rocket_trail(from, tf.translation, dir, dt);
            }
            None => commands.entity(entity).despawn(),
        }
    }
    for r in &states.cur.rockets {
        if seen.contains(&r.id) {
            continue;
        }
        let dir = to_bevy_dir(r.velocity).normalize_or_zero();
        commands
            .spawn((
                GameEntity,
                RocketVis(r.id),
                Transform::from_translation(to_bevy(r.origin)).with_rotation(Quat::from_rotation_arc(ROCKET_FORWARD, dir)),
                Visibility::default(),
            ))
            .with_children(|p| {
                p.spawn((WorldAssetRoot(assets.rocket_scene()), Transform::IDENTITY));
                p.spawn((
                    PointLight { color: Color::srgb(1.0, 0.6, 0.2), intensity: 20_000.0, range: 6.0, ..default() },
                    Transform::from_xyz(-0.4, 0.0, 0.0),
                ));
            });
    }
}

pub fn spawn_fx(
    mut commands: Commands,
    fx: Res<PendingFx>,
    assets: Option<Res<Assets3d>>,
    arena: Res<ArenaRes>,
    time: Res<Time<Real>>,
    mut particles: ResMut<Particles>,
) {
    let Some(assets) = assets else { return };
    let now = time.elapsed_secs_f64();
    for ev in &fx.events {
        if let SimEvent::Explosion { origin, normal, hit_player, .. } = *ev {
            particles.explosion(to_bevy(origin), to_bevy_dir(normal));
            commands.spawn((
                GameEntity,
                PointLight { color: Color::srgb(1.0, 0.7, 0.3), intensity: 400_000.0, range: 12.0, ..default() },
                Explosion { t0: now },
                Transform::from_translation(to_bevy(origin) + to_bevy_dir(normal) * 0.3),
            ));
            // Scorch mark on the world surface that was hit. A direct hit on a player has the
            // player's hull as its "surface", so project the mark onto the ground below instead
            // (only when the ground is close enough for the blast to plausibly scorch it).
            let mark = if hit_player.is_some() {
                let env = endif_sim::TraceEnv::world_only(&arena.0.rocket_brushes);
                let tr = endif_sim::trace::trace_line(&env, origin, origin - SVec3::new(0.0, 0.0, 120.0));
                (tr.fraction < 1.0).then_some((tr.endpos, tr.normal))
            } else {
                Some((origin - normal * 1.0, normal))
            };
            if let Some((pos, n_src)) = mark {
                let n = to_bevy_dir(n_src);
                if n != Vec3::ZERO {
                    let spin = Quat::from_axis_angle(n, particles.range(0.0, std::f32::consts::TAU));
                    commands.spawn((
                        GameEntity,
                        Mesh3d(assets.burn_mesh.clone()),
                        MeshMaterial3d(assets.burn_mat.clone()),
                        NotShadowCaster,
                        Transform::from_translation(to_bevy(pos) + n * 0.01).with_rotation(spin * Quat::from_rotation_arc(Vec3::Z, n)),
                    ));
                }
            }
        }
    }
}

fn animate_explosions(mut commands: Commands, time: Res<Time<Real>>, mut q: Query<(Entity, &Explosion, &mut PointLight)>) {
    let now = time.elapsed_secs_f64();
    for (e, ex, mut light) in &mut q {
        let t = ((now - ex.t0) / 0.35) as f32;
        if t >= 1.0 {
            commands.entity(e).despawn();
            continue;
        }
        light.intensity = 400_000.0 * (1.0 - t);
    }
}

fn update_camera(
    states: Option<Res<RenderStates>>,
    local: Res<LocalHandle>,
    look: Res<LookAngles>,
    time: Res<Time<Real>>,
    mut cam: Query<&mut Transform, With<MainCamera>>,
) {
    let (Some(states), Ok(mut tf)) = (states, cam.single_mut()) else { return };
    let alpha = interp_alpha(&states, time.elapsed_secs_f64());
    let i = local.0;
    let p = &states.cur.players[i];
    let (origin, view) = lerp_state(&states.prev, &states.cur, alpha, i);
    let eye = if p.alive { origin + view } else { origin + SVec3::new(0.0, 0.0, 14.0) };
    tf.translation = to_bevy(eye);
    tf.rotation = view_rotation(look.pitch, look.yaw);
}
