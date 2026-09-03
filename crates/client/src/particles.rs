//! CPU billboard particles with the TF2 sprite sheets: one dynamic mesh per sprite kind (so one
//! draw call each), vertex colours for tint/fade. Rough ports of `rockettrail` and
//! `ExplosionCore_wall` from the TF2 particle files.

use crate::assets::{GameAssets, SPRITE_ADDITIVE, SPRITE_COUNT, SPRITE_FRAMES, Sprite};
use crate::render::{MainCamera, UNIT};
use crate::{AppState, GameEntity};
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

/// Source gravity (800 u/s²) in metres.
const GRAVITY: f32 = 800.0 * UNIT;

#[derive(Clone, Copy)]
pub struct Particle {
    pub sprite: Sprite,
    pub frame: u32,
    pub pos: Vec3,
    pub vel: Vec3,
    pub age: f32,
    pub life: f32,
    /// Diameter at birth / death (metres).
    pub size0: f32,
    pub size1: f32,
    pub color: Vec3,
    pub alpha: f32,
    /// Colour to fade towards, and the fraction of the lifetime the fade takes (`Color Fade`).
    pub fade_to: Option<(Vec3, f32)>,
    /// Fractions of the lifetime spent fading in / out.
    pub fade_in: f32,
    pub fade_out: f32,
    pub rot: f32,
    pub spin: f32,
    pub gravity: f32,
    pub drag: f32,
}

impl Particle {
    fn t(&self) -> f32 {
        (self.age / self.life).clamp(0.0, 1.0)
    }
    fn alpha_now(&self) -> f32 {
        let t = self.t();
        let fi = if self.fade_in > 0.0 { (t / self.fade_in).min(1.0) } else { 1.0 };
        let fo = if self.fade_out > 0.0 { ((1.0 - t) / self.fade_out).min(1.0) } else { 1.0 };
        self.alpha * fi * fo
    }
    fn size_now(&self) -> f32 {
        let t = self.t();
        self.size0 + (self.size1 - self.size0) * t
    }
    fn color_now(&self) -> Vec3 {
        match self.fade_to {
            Some((to, over)) if over > 0.0 => self.color.lerp(to, (self.t() / over).min(1.0)),
            _ => self.color,
        }
    }
}

/// `rockettrail` colours: puffs are born ember-orange and fade to this pale lilac grey.
const TRAIL_SMOKE: Vec3 = Vec3::new(195.0 / 255.0, 190.0 / 255.0, 202.0 / 255.0);
const TRAIL_BORN_A: Vec3 = Vec3::new(247.0 / 255.0, 194.0 / 255.0, 117.0 / 255.0);
const TRAIL_BORN_B: Vec3 = Vec3::new(251.0 / 255.0, 142.0 / 255.0, 0.0);
/// `rockettrail_fire`: yellow-orange glow fading to dark brown (which, additively, is just dim).
const FIRE_A: Vec3 = Vec3::new(1.0, 168.0 / 255.0, 0.0);
const FIRE_B: Vec3 = Vec3::new(1.0, 234.0 / 255.0, 0.0);
const FIRE_END: Vec3 = Vec3::new(72.0 / 255.0, 37.0 / 255.0, 0.0);

#[derive(Resource, Default)]
pub struct Particles {
    pub list: Vec<Particle>,
    rng: u64,
}

impl Particles {
    /// xorshift; presentation only, never touches the simulation.
    pub fn rand(&mut self) -> f32 {
        if self.rng == 0 {
            self.rng = 0x9E37_79B9_7F4A_7C15;
        }
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        ((x >> 40) as f32) / ((1u64 << 24) as f32)
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.rand()
    }
    pub fn sphere(&mut self) -> Vec3 {
        loop {
            let v = Vec3::new(self.range(-1.0, 1.0), self.range(-1.0, 1.0), self.range(-1.0, 1.0));
            if v.length_squared() <= 1.0 {
                return v;
            }
        }
    }
    fn frame_for(&mut self, sprite: Sprite) -> u32 {
        let n = SPRITE_FRAMES[sprite as usize];
        (self.rand() * n as f32) as u32 % n
    }

    /// TF2's `rockettrail` (smoke) with its `rockettrail_fire` child, behind a rocket travelling
    /// from `from` to `to` (metres) this frame. (The `rockettrail_burst` sparks are left out: they
    /// fill the shooter's view at the muzzle.) Continuous emitters are spread along the path
    /// covered this frame so the trail has no gaps at low frame rates.
    pub fn rocket_trail(&mut self, from: Vec3, to: Vec3, dir: Vec3, dt: f32) {
        let tail = -dir * 18.0 * UNIT;
        let deg = std::f32::consts::PI / 180.0;
        // Departure from vanilla: smoke drifts to the rocket's right and up a little, so it clears
        // the shooter's line of sight sooner. "Right" is horizontal relative to the flight
        // direction, which is the shooter's right at the moment they fired.
        let right = dir.cross(Vec3::Y).try_normalize().unwrap_or_else(|| dir.any_orthonormal_vector());
        let drift = (right * 24.0 + Vec3::Y * 10.0) * UNIT;

        // Smoke: 150/s, born within 1.2 u of the tail with next to no velocity (1 u/s, plus a
        // 6 u/s² upward "gravity"), so a puff sits where the rocket left it and only grows and
        // thins: radius 10 u scaled 0.25 → 2 over 1.5 s (linear), alpha 96–128/255 fading in over
        // the first 5% of life and out from 10% of life to death. Born ember-orange, lilac grey
        // by 10% of life.
        let n = (150.0 * dt).ceil().max(1.0) as usize;
        for k in 0..n {
            let s = (k as f32 + self.rand()) / n as f32;
            let p = from.lerp(to, s) + tail + self.sphere() * 1.2 * UNIT;
            let frame = self.frame_for(Sprite::Smokelit);
            let life = self.range(0.8, 1.2);
            let vel = self.sphere().normalize_or_zero() * 1.0 * UNIT + drift;
            let color = TRAIL_BORN_A.lerp(TRAIL_BORN_B, self.rand());
            let alpha = self.range(96.0, 128.0) / 255.0;
            let rot = self.range(-45.0, 0.0) * deg;
            let particle = Particle {
                sprite: Sprite::Smokelit,
                frame,
                pos: p,
                vel,
                age: (1.0 - s) * dt,
                life,
                size0: 20.0 * 0.25 * UNIT,
                size1: 20.0 * (0.25 + 1.75 * life / 1.5) * UNIT,
                color,
                alpha,
                fade_to: Some((TRAIL_SMOKE, 0.1)),
                fade_in: 0.05,
                fade_out: 0.9,
                rot,
                spin: 0.0,
                gravity: 6.0 * UNIT,
                drag: 0.0,
            };
            self.list.push(particle);
        }

        // Fire: 128/s additive glows 1–2 u off the tail, 0.2 s life, radius 5 u scaled 3 → 0,
        // alpha 64/255, yellow-orange fading to dark brown over the whole life.
        let n = (128.0 * dt).ceil().max(1.0) as usize;
        for k in 0..n {
            let s = (k as f32 + self.rand()) / n as f32;
            let p = from.lerp(to, s) + tail + self.sphere().normalize_or_zero() * self.range(1.0, 2.0) * UNIT;
            let color = FIRE_A.lerp(FIRE_B, self.rand());
            let rot = self.range(0.0, 360.0) * deg;
            let particle = Particle {
                sprite: Sprite::Glow,
                frame: 0,
                pos: p,
                vel: Vec3::ZERO,
                age: (1.0 - s) * dt,
                life: 0.2,
                size0: 10.0 * 3.0 * UNIT,
                size1: 0.0,
                color,
                alpha: 64.0 / 255.0,
                fade_to: Some((FIRE_END, 1.0)),
                fade_in: 0.1,
                fade_out: 0.9,
                rot,
                spin: 0.0,
                gravity: 6.0 * UNIT,
                drag: 0.0,
            };
            self.list.push(particle);
        }
    }

    /// `ExplosionCore_wall`: flash, fireball, smoke cloud, dust along the surface, embers, debris.
    pub fn explosion(&mut self, pos: Vec3, normal: Vec3) {
        let n = if normal.length_squared() > 0.0 { normal.normalize() } else { Vec3::Y };
        let c = pos + n * 6.0 * UNIT;
        // Wall fragments: a spray of tiny chips knocked out of the surface, fast and short-lived,
        // thrown mostly along the normal and dropping under full gravity.
        for _ in 0..28 {
            let frame = self.frame_for(Sprite::Debris);
            let dir = (n * self.range(0.6, 2.0) + self.sphere()).normalize_or_zero();
            let size = self.range(1.0, 2.5) * UNIT;
            let shade = self.range(0.5, 0.68);
            let particle = Particle {
                sprite: Sprite::Debris,
                frame,
                pos: pos + n * 2.0 * UNIT,
                vel: dir * self.range(250.0, 650.0) * UNIT,
                age: 0.0,
                life: self.range(0.4, 0.8),
                size0: size,
                size1: size,
                color: Vec3::new(shade, shade * 0.97, shade * 0.92),
                alpha: 1.0,
                fade_to: None,
                fade_in: 0.0,
                fade_out: 0.3,
                rot: self.range(0.0, std::f32::consts::TAU),
                spin: self.range(-20.0, 20.0),
                gravity: -GRAVITY,
                drag: 0.2,
            };
            self.list.push(particle);
        }
        // Core flash + fireball (additive glows).
        let particle = Particle {
            sprite: Sprite::Softglow,
            frame: 0,
            pos: c,
            vel: Vec3::ZERO,
            age: 0.0,
            life: 0.22,
            size0: 90.0 * UNIT,
            size1: 150.0 * UNIT,
            color: Vec3::new(1.0, 0.8, 0.5),
            alpha: 1.0,
            fade_to: None,
            fade_in: 0.08,
            fade_out: 0.6,
            rot: self.range(0.0, std::f32::consts::TAU),
            spin: 0.0,
            gravity: 0.0,
            drag: 0.0,
        };
        self.list.push(particle);
        for _ in 0..3 {
            let jitter = self.sphere() * 8.0 * UNIT;
            let particle = Particle {
                sprite: Sprite::Glow,
                frame: 0,
                pos: c + jitter,
                vel: n * 30.0 * UNIT,
                age: 0.0,
                life: self.range(0.25, 0.4),
                size0: self.range(50.0, 70.0) * UNIT,
                size1: self.range(90.0, 120.0) * UNIT,
                color: Vec3::new(1.0, 0.5, 0.15),
                alpha: 0.9,
                fade_to: None,
                fade_in: 0.05,
                fade_out: 0.7,
                rot: self.range(0.0, std::f32::consts::TAU),
                spin: self.range(-2.0, 2.0),
                gravity: 0.0,
                drag: 0.0,
            };
            self.list.push(particle);
        }
        // Smoke cloud.
        for _ in 0..10 {
            let frame = self.frame_for(Sprite::Smoke1);
            let dir = (n * self.range(0.6, 1.6) + self.sphere()).normalize_or_zero();
            let particle = Particle {
                sprite: Sprite::Smoke1,
                frame,
                pos: c + self.sphere() * 12.0 * UNIT,
                vel: dir * self.range(80.0, 200.0) * UNIT,
                age: 0.0,
                life: self.range(1.0, 1.7),
                size0: self.range(22.0, 32.0) * UNIT,
                size1: self.range(60.0, 90.0) * UNIT,
                color: Vec3::new(0.66, 0.63, 0.59),
                alpha: 0.38,
                fade_to: None,
                fade_in: 0.05,
                fade_out: 0.6,
                rot: self.range(0.0, std::f32::consts::TAU),
                spin: self.range(-1.0, 1.0),
                gravity: 25.0 * UNIT,
                drag: 2.5,
            };
            self.list.push(particle);
        }
        // Dust rolling out along the surface.
        let (t1, t2) = n.any_orthonormal_pair();
        for _ in 0..8 {
            let a = self.range(0.0, std::f32::consts::TAU);
            let dir = t1 * a.cos() + t2 * a.sin();
            let frame = self.frame_for(Sprite::Smoke1);
            let particle = Particle {
                sprite: Sprite::Smoke1,
                frame,
                pos: c + dir * 10.0 * UNIT,
                vel: (dir * self.range(200.0, 320.0) + n * 20.0) * UNIT,
                age: 0.0,
                life: self.range(0.7, 1.1),
                size0: 18.0 * UNIT,
                size1: self.range(45.0, 65.0) * UNIT,
                color: Vec3::new(0.7, 0.66, 0.6),
                alpha: 0.3,
                fade_to: None,
                fade_in: 0.05,
                fade_out: 0.5,
                rot: self.range(0.0, std::f32::consts::TAU),
                spin: self.range(-2.0, 2.0),
                gravity: 0.0,
                drag: 3.5,
            };
            self.list.push(particle);
        }
        // Embers.
        for _ in 0..16 {
            let dir = (n * self.range(0.5, 1.5) + self.sphere() * 1.2).normalize_or_zero();
            let particle = Particle {
                sprite: Sprite::Ember,
                frame: 0,
                pos: c,
                vel: dir * self.range(150.0, 450.0) * UNIT,
                age: 0.0,
                life: self.range(0.4, 0.9),
                size0: self.range(3.0, 5.0) * UNIT,
                size1: 1.0 * UNIT,
                color: Vec3::new(1.0, 0.55, 0.15),
                alpha: 1.0,
                fade_to: None,
                fade_in: 0.0,
                fade_out: 0.5,
                rot: 0.0,
                spin: 0.0,
                gravity: -GRAVITY,
                drag: 0.5,
            };
            self.list.push(particle);
        }
        // Debris chunks.
        for _ in 0..6 {
            let frame = self.frame_for(Sprite::Debris);
            let dir = (n * self.range(0.8, 1.8) + self.sphere()).normalize_or_zero();
            let particle = Particle {
                sprite: Sprite::Debris,
                frame,
                pos: c,
                vel: dir * self.range(200.0, 420.0) * UNIT,
                age: 0.0,
                life: self.range(0.9, 1.4),
                size0: self.range(5.0, 9.0) * UNIT,
                size1: self.range(5.0, 9.0) * UNIT,
                color: Vec3::new(0.55, 0.5, 0.45),
                alpha: 1.0,
                fade_to: None,
                fade_in: 0.0,
                fade_out: 0.25,
                rot: self.range(0.0, std::f32::consts::TAU),
                spin: self.range(-12.0, 12.0),
                gravity: -GRAVITY,
                drag: 0.3,
            };
            self.list.push(particle);
        }
    }
}

#[derive(Component)]
struct ParticleLayer(usize);

#[derive(Resource)]
struct ParticleMeshes([Handle<Mesh>; SPRITE_COUNT]);

pub struct ParticlesPlugin;

impl Plugin for ParticlesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Particles>()
            .add_systems(OnEnter(AppState::InGame), setup_layers)
            .add_systems(OnExit(AppState::InGame), clear)
            .add_systems(Update, update_particles.run_if(in_state(AppState::InGame)).after(crate::render::spawn_fx));
    }
}

/// A mesh with one degenerate (invisible) triangle: zero-sized vertex buffers upset the render
/// world's slab allocator, so an "empty" layer keeps this placeholder instead.
fn empty_mesh() -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 3]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
    mesh
}

fn setup_layers(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    assets: Res<GameAssets>,
    mut particles: ResMut<Particles>,
) {
    particles.list.clear();
    let handles: [Handle<Mesh>; SPRITE_COUNT] = std::array::from_fn(|_| meshes.add(empty_mesh()));
    for (i, handle) in handles.iter().enumerate() {
        let material = materials.add(StandardMaterial {
            base_color_texture: Some(assets.sprites[i].clone()),
            alpha_mode: if SPRITE_ADDITIVE[i] { AlphaMode::Add } else { AlphaMode::Blend },
            unlit: true,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        commands.spawn((
            GameEntity,
            ParticleLayer(i),
            Mesh3d(handle.clone()),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            NoFrustumCulling,
            NotShadowCaster,
            NotShadowReceiver,
        ));
    }
    commands.insert_resource(ParticleMeshes(handles));
}

fn clear(mut particles: ResMut<Particles>) {
    particles.list.clear();
}

fn update_particles(
    time: Res<Time<Real>>,
    mut particles: ResMut<Particles>,
    handles: Option<Res<ParticleMeshes>>,
    mut meshes: ResMut<Assets<Mesh>>,
    cam: Query<&GlobalTransform, With<MainCamera>>,
    mut layers: Query<(&ParticleLayer, &mut Visibility)>,
) {
    let Some(handles) = handles else { return };
    let dt = time.delta_secs().min(0.1);
    let rot = cam.single().map(|c| c.rotation()).unwrap_or(Quat::IDENTITY);
    let cam_right = rot * Vec3::X;
    let cam_up = rot * Vec3::Y;

    // Integrate.
    for p in &mut particles.list {
        p.age += dt;
        p.vel.y += p.gravity * dt;
        p.vel *= (1.0 - p.drag * dt).max(0.0);
        p.pos += p.vel * dt;
        p.rot += p.spin * dt;
    }
    particles.list.retain(|p| p.age < p.life);

    // Rebuild one mesh per sprite kind.
    let mut positions: Vec<Vec<[f32; 3]>> = vec![Vec::new(); SPRITE_COUNT];
    let mut normals: Vec<Vec<[f32; 3]>> = vec![Vec::new(); SPRITE_COUNT];
    let mut uvs: Vec<Vec<[f32; 2]>> = vec![Vec::new(); SPRITE_COUNT];
    let mut colors: Vec<Vec<[f32; 4]>> = vec![Vec::new(); SPRITE_COUNT];
    let mut indices: Vec<Vec<u32>> = vec![Vec::new(); SPRITE_COUNT];
    let normal = (rot * Vec3::Z).to_array();
    for p in &particles.list {
        let k = p.sprite as usize;
        let half = p.size_now() * 0.5;
        let (s, c) = p.rot.sin_cos();
        let r = (cam_right * c + cam_up * s) * half;
        let u = (cam_up * c - cam_right * s) * half;
        let frames = SPRITE_FRAMES[k] as f32;
        let u0 = p.frame as f32 / frames;
        let u1 = (p.frame + 1) as f32 / frames;
        let a = p.alpha_now();
        let color = p.color_now();
        let col = [color.x, color.y, color.z, a];
        let base = positions[k].len() as u32;
        positions[k].extend_from_slice(&[
            (p.pos - r - u).to_array(),
            (p.pos + r - u).to_array(),
            (p.pos + r + u).to_array(),
            (p.pos - r + u).to_array(),
        ]);
        normals[k].extend_from_slice(&[normal; 4]);
        uvs[k].extend_from_slice(&[[u0, 1.0], [u1, 1.0], [u1, 0.0], [u0, 0.0]]);
        colors[k].extend_from_slice(&[col; 4]);
        indices[k].extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    for (layer, mut vis) in &mut layers {
        let k = layer.0;
        let empty = positions[k].is_empty();
        let was_empty = *vis == Visibility::Hidden;
        *vis = if empty { Visibility::Hidden } else { Visibility::Visible };
        if empty {
            if !was_empty && let Some(mut mesh) = meshes.get_mut(&handles.0[k]) {
                *mesh = empty_mesh();
            }
            continue;
        }
        if let Some(mut mesh) = meshes.get_mut(&handles.0[k]) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, std::mem::take(&mut positions[k]));
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, std::mem::take(&mut normals[k]));
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, std::mem::take(&mut uvs[k]));
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, std::mem::take(&mut colors[k]));
            mesh.insert_indices(Indices::U32(std::mem::take(&mut indices[k])));
        }
    }
}
