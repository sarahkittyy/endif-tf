//! The Fruit Ninja gallery, client side: its scene (a glass platform on a pole over a foggy void,
//! a wooden wall in front sized to the soldiers' bounds), the flying soldiers' models (grey
//! ragdolls once hit), the floating options frame, the score strip with the local records, the
//! round result and the reset countdown. The rules and the state live in `endif_sim::fruit`; the options reach the
//! simulation in the idle player's input (see `game::read_local_inputs`).

use crate::assets::{GameAssets, repeat_sampler};
use crate::audio::{Sfx, play};
use crate::game::{PendingFx, RenderStates};
use crate::menu::{no_wrap, slider_controls};
use crate::net::{LocalHandle, MatchKind};
use crate::player_model::{self, TargetPose, TargetPoses};
use crate::render::{SKY, TICK_SECS, UNIT, interp_alpha, to_bevy};
use crate::settings::{Settings, Slider, storage};
use crate::theme::{self, Theme};
use crate::{AppState, GameEntity};
use bevy::ecs::relationship::RelatedSpawnerCommands;
use bevy::light::NotShadowCaster;
use bevy::math::Affine2;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::window::PrimaryWindow;
use endif_sim::SimEvent;
use endif_sim::fruit::{self as rules, Difficulty, MAX_TARGETS, Preset, ROUND_SIZE};

/// Ticks between falling off the platform and standing on it again.
pub const FALL_RESPAWN_TICKS: u32 = 40;
/// Height of a standing soldier's hull centre above its feet: the target models tumble about it.
const BODY_CENTRE: f32 = 41.0;
/// Where the void turns to sky, in metres from the camera: the wall at its farthest preset
/// (1100 units, 28 m) stays clear, the pole is gone well before its end.
const FOG_START: f32 = 40.0;
const FOG_END: f32 = 130.0;
/// How far the pole reaches down (units); its end is deep in the fog.
const POLE_LENGTH: f32 = 6000.0;
/// The wall slab's depth and its border beams' size and how far they stand proud of the face.
const WALL_SLAB: f32 = 64.0;
const BEAM: f32 = 48.0;
const BEAM_DEPTH: f32 = 24.0;
const FRAME_W: f32 = 300.0;
const SLIDER_W: f32 = 150.0;
/// Where the frame starts: near the top right corner.
const FRAME_ANCHOR: Vec2 = Vec2::new(0.97, 0.1);
/// The countdown: a ding at each of these seconds after it starts, "Begin!" at `COUNTDOWN_SECS`.
const COUNTDOWN_STEPS: [f64; 3] = [0.0, 0.34, 0.67];
const COUNTDOWN_SECS: f64 = 1.0;
/// How long a round's result stays up before the countdown into the next one, and how long
/// a new record's stays up: the flourish (`Sfx::record`, 8 s) plays out under it.
const RESULT_SECS: f64 = 3.5;
const RECORD_RESULT_SECS: f64 = 8.5;
/// How fast a hit soldier tumbles, radians per second.
const TUMBLE_RATE: f32 = 7.0;
/// Where the records live in the local storage (see `settings::storage`).
const RECORDS_FILE: &str = "fruit_records.ini";

pub struct FruitPlugin;

impl Plugin for FruitPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Countdown>()
            .init_resource::<GreyCache>()
            .insert_resource(Records::load())
            .add_systems(OnEnter(AppState::InGame), setup.run_if(in_gallery))
            .add_systems(OnExit(AppState::InGame), leave)
            .add_systems(
                Update,
                (sync_wall, sync_targets, update_score, round_result, (drag_frame, place_frame).chain(), collapse_frame, option_buttons, frame_sections, reset_button, run_countdown)
                    .run_if(in_state(AppState::InGame).and_then(in_gallery)),
            );
    }
}

pub fn in_gallery(kind: Option<Res<MatchKind>>) -> bool {
    matches!(kind.as_deref(), Some(MatchKind::FruitNinja))
}

/// The reset countdown: three dings over a second, then "Begin!". While it is armed (from
/// `started` on, which may be a moment in the future while a round's result shows) the idle
/// player's input carries the reset flag, which keeps the gallery empty and its stats at zero.
#[derive(Resource, Default)]
pub struct Countdown {
    started: Option<f64>,
    dings: u8,
}

impl Countdown {
    pub fn active(&self) -> bool {
        self.started.is_some()
    }

    fn start(&mut self, at: f64) {
        self.started = Some(at);
        self.dings = 0;
    }
}

/// The best round on each difficulty: soldiers hit, accuracy and the longest chain, kept locally.
#[derive(Resource, Default)]
struct Records([Record; 3]);

#[derive(Clone, Copy, Default)]
struct Record {
    hits: u32,
    /// Percent.
    acc: u32,
    chain: u32,
}

impl Records {
    fn get(&self, d: Difficulty) -> Record {
        self.0[d as usize]
    }

    /// Takes a round's result in; true when it beat any of the records.
    fn submit(&mut self, d: Difficulty, hits: u32, acc: u32, chain: u32) -> bool {
        let r = &mut self.0[d as usize];
        let better = hits > r.hits || acc > r.acc || chain > r.chain;
        r.hits = r.hits.max(hits);
        r.acc = r.acc.max(acc);
        r.chain = r.chain.max(chain);
        better
    }

    fn load() -> Self {
        let mut records = Records::default();
        for line in storage::read(RECORDS_FILE).unwrap_or_default().lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (Some((name, what)), Ok(n)) = (k.trim().split_once('_'), v.trim().parse::<u32>()) else { continue };
            let Some(d) = Difficulty::from_ini(name) else { continue };
            match what {
                "hits" => records.0[d as usize].hits = n,
                "acc" => records.0[d as usize].acc = n,
                "chain" => records.0[d as usize].chain = n,
                _ => {}
            }
        }
        records
    }

    fn save(&self) {
        let mut out = String::from("; endif.tf Fruit Ninja records\n");
        for d in Difficulty::ALL {
            let r = self.get(d);
            let name = d.ini_name();
            out.push_str(&format!("{name}_hits = {}\n{name}_acc = {}\n{name}_chain = {}\n", r.hits, r.acc, r.chain));
        }
        if let Err(e) = storage::write(RECORDS_FILE, &out) {
            warn!("could not save the Fruit Ninja records: {e}");
        }
    }
}

/// The gallery's surfaces, fetched the first time it is entered (not at startup: they are of no
/// use to a match and would only slow the web loading screen down) and kept for the next time.
#[derive(Resource, Clone)]
pub struct FruitTextures {
    glass: Handle<Image>,
    wood: Handle<Image>,
    wood_beam: Handle<Image>,
    metal_pole: Handle<Image>,
    metal_dark: Handle<Image>,
}

impl FruitTextures {
    fn load(server: &AssetServer) -> Self {
        let tex = |name: &str| server.load_builder().with_settings(repeat_sampler).load(format!("textures/{name}.png"));
        FruitTextures { glass: tex("glass"), wood: tex("wood"), wood_beam: tex("wood_beam"), metal_pole: tex("metal_pole"), metal_dark: tex("metal_dark") }
    }
}

/// The wall's materials, whose texture repeats follow the wall's size.
#[derive(Resource)]
struct WallMaterials {
    wood: Handle<StandardMaterial>,
    along: Handle<StandardMaterial>,
    upright: Handle<StandardMaterial>,
}

/// Grey versions of the soldier's materials and textures for the ragdolls, made once each.
#[derive(Resource, Default)]
struct GreyCache {
    materials: HashMap<AssetId<StandardMaterial>, Handle<StandardMaterial>>,
    images: HashMap<AssetId<Image>, Handle<Image>>,
}

/// The wooden wall's root: translated to the face's distance, never rotated or scaled, so the
/// scorch marks parented to it keep their world layout. `sync_wall` places it and its parts.
#[derive(Component)]
pub struct WallVis;

#[derive(Component, Clone, Copy)]
enum WallPart {
    Slab,
    Top,
    Bottom,
    Left,
    Right,
}

/// One slot of the soldier pool: the model at the hull centre, its child scene hanging
/// `BODY_CENTRE` below. `id` is the simulation target it shows, `None` while free.
#[derive(Component)]
struct TargetVis {
    slot: u8,
    id: Option<u32>,
    spin_axis: Vec3,
    spin: f32,
    /// Set when the soldier has been hit: it stops facing the player and goes grey and limp.
    ragdoll: Option<Ragdoll>,
    /// The materials the ragdoll's grey ones replaced, to put back when the slot is reused.
    originals: Vec<(Entity, Handle<StandardMaterial>)>,
}

#[derive(Clone, Copy)]
struct Ragdoll {
    /// The yaw it was facing when hit.
    yaw: f32,
    /// Seconds since.
    since: f32,
}

#[derive(Component)]
struct HitsText;
#[derive(Component)]
struct AccuracyText;
#[derive(Component)]
struct ChainText;
/// The round's progress, or the endless mark (the TF2 fonts have no infinity glyph; it comes
/// from `Theme::symbol`).
#[derive(Component)]
struct ProgressText;
#[derive(Component)]
struct EndlessIcon;
#[derive(Component)]
struct ProgressLabel;
/// The records on the current difficulty (rounds only).
#[derive(Component)]
struct RecordHitsText;
#[derive(Component)]
struct RecordAccText;
#[derive(Component)]
struct RecordChainText;
/// A score strip element for one mode only: shown in rounds (true) or in endless play (false).
#[derive(Component)]
struct ModeOnly(bool);
/// The frame's two option sections; the one for the mode not being played is folded away.
#[derive(Component)]
struct EndlessSection;
#[derive(Component)]
struct RoundsSection;
#[derive(Component)]
struct ResetButton;
#[derive(Component)]
struct CountdownText;
#[derive(Component)]
struct ResultText;
/// The floating options frame; its title bar is the `DragHandle`, `FrameBody` folds away. It
/// sits at `anchor` (0..1 on each axis) of the room the window has for it, so it keeps its
/// place in the window through resizes; dragging moves the anchor.
#[derive(Component)]
struct FruitFrame {
    anchor: Vec2,
}
#[derive(Component)]
struct DragHandle;
#[derive(Component)]
struct FrameBody;
#[derive(Component)]
struct CollapseButton;
#[derive(Component)]
struct CollapseGlyph;
/// The frame's choices: a difficulty preset, or rounds (true) against endless play.
#[derive(Component, Clone, Copy, PartialEq)]
enum OptionButton {
    Difficulty(Difficulty),
    Rounds(bool),
}

/// Fog for the gallery's camera: the sky's haze colour, so the pole and anything that falls
/// fade out.
pub fn fog() -> DistanceFog {
    DistanceFog {
        color: SKY,
        directional_light_color: Color::NONE,
        directional_light_exponent: 8.0,
        falloff: FogFalloff::Linear { start: FOG_START, end: FOG_END },
    }
}

/// Bevy's cuboid lays UVs on its ±X faces with u along the height, so a texture meant to cover
/// `width` by `height` units on a face that looks along x needs its axes swapped.
fn x_face_uv(width: f32, height: f32) -> Affine2 {
    Affine2::from_mat2(Mat2::from_cols(Vec2::new(0.0, height / 256.0), Vec2::new(width / 256.0, 0.0)))
}

/// The scene: glass platform with a metal rim and plate, the hub and pole under it, and the
/// wall's root with its slab and beams (unit cubes that `sync_wall` scales to the bounds).
pub fn spawn_arena(commands: &mut Commands, meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>, server: &AssetServer, textures: Option<&FruitTextures>) {
    let tex = textures.cloned().unwrap_or_else(|| FruitTextures::load(server));
    commands.insert_resource(tex.clone());
    let u = UNIT;
    let half = rules::PLATFORM_HALF * u;
    let rail = 12.0 * u;

    // The glass: wire mesh, a little see-through so the plate, the pole and the void show
    // beneath. It fills the inside of the rim, which stands a touch above it, and the plate hangs
    // a little under it: no two faces share a plane.
    let glass = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.72),
        base_color_texture: Some(tex.glass.clone()),
        uv_transform: Affine2::from_scale(Vec2::splat(rules::PLATFORM_HALF * 2.0 / 128.0)),
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 0.25,
        reflectance: 0.6,
        ..default()
    });
    let inner = (half - rail) * 2.0;
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Cuboid::new(inner, 8.0 * u, inner))),
        MeshMaterial3d(glass),
        NotShadowCaster,
        Transform::from_xyz(0.0, -4.0 * u, 0.0),
    ));
    let metal_dark = materials.add(StandardMaterial {
        base_color_texture: Some(tex.metal_dark.clone()),
        uv_transform: Affine2::from_scale(Vec2::splat(1.5)),
        perceptual_roughness: 0.7,
        metallic: 0.3,
        ..default()
    });
    let plate = 160.0 * u;
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Cuboid::new(plate, 8.0 * u, plate))),
        MeshMaterial3d(metal_dark.clone()),
        Transform::from_xyz(0.0, -13.0 * u, 0.0),
    ));
    let rail_h = 9.5 * u;
    let rail_y = -8.0 * u + rail_h / 2.0;
    let rails: [(Vec3, Vec3); 4] = [
        (Vec3::new(half - rail / 2.0, rail_y, 0.0), Vec3::new(rail, rail_h, half * 2.0)),
        (Vec3::new(-half + rail / 2.0, rail_y, 0.0), Vec3::new(rail, rail_h, half * 2.0)),
        (Vec3::new(0.0, rail_y, half - rail / 2.0), Vec3::new(inner, rail_h, rail)),
        (Vec3::new(0.0, rail_y, -half + rail / 2.0), Vec3::new(inner, rail_h, rail)),
    ];
    for (pos, size) in rails {
        commands.spawn((
            GameEntity,
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(metal_dark.clone()),
            Transform::from_translation(pos),
        ));
    }

    // The hub the platform sits on and the pole into the fog. The pipe texture goes once around
    // the pole and repeats down its length.
    let metal_pole = materials.add(StandardMaterial {
        base_color_texture: Some(tex.metal_pole.clone()),
        uv_transform: Affine2::from_scale(Vec2::new(1.0, POLE_LENGTH / 113.0)),
        perceptual_roughness: 0.6,
        metallic: 0.4,
        ..default()
    });
    let hub = materials.add(StandardMaterial {
        base_color_texture: Some(tex.metal_pole.clone()),
        uv_transform: Affine2::from_scale(Vec2::new(2.0, 0.2)),
        perceptual_roughness: 0.6,
        metallic: 0.4,
        ..default()
    });
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Cylinder::new(36.0 * u, 20.0 * u))),
        MeshMaterial3d(hub),
        Transform::from_xyz(0.0, -27.0 * u, 0.0),
    ));
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Cylinder::new(18.0 * u, POLE_LENGTH * u))),
        MeshMaterial3d(metal_pole),
        NotShadowCaster,
        Transform::from_xyz(0.0, (-37.0 - POLE_LENGTH / 2.0) * u, 0.0),
    ));

    // The wall: planks on a slab whose face is at the preset's distance, beams along its four
    // edges standing proud of the face. Sizes and texture repeats are set by `sync_wall`.
    let wood = materials.add(StandardMaterial { base_color_texture: Some(tex.wood.clone()), perceptual_roughness: 0.9, ..default() });
    let beam = |materials: &mut Assets<StandardMaterial>| {
        materials.add(StandardMaterial { base_color_texture: Some(tex.wood_beam.clone()), perceptual_roughness: 0.85, ..default() })
    };
    let along = beam(materials);
    let upright = beam(materials);
    let unit = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let parts = [
        (WallPart::Slab, wood.clone()),
        (WallPart::Top, along.clone()),
        (WallPart::Bottom, along.clone()),
        (WallPart::Left, upright.clone()),
        (WallPart::Right, upright.clone()),
    ];
    commands.spawn((GameEntity, WallVis, Transform::default(), Visibility::default())).with_children(|w| {
        for (part, mat) in parts {
            w.spawn((part, Mesh3d(unit.clone()), MeshMaterial3d(mat), Transform::default()));
        }
    });
    commands.insert_resource(WallMaterials { wood, along, upright });
}

/// Places the wall for the difficulty the simulation is on: the root at the preset's face
/// distance, the slab and beams sized to the soldiers' region, the textures repeating in units.
/// Only when it changes.
fn sync_wall(
    states: Option<Res<RenderStates>>,
    mut root: Query<&mut Transform, (With<WallVis>, Without<WallPart>)>,
    mut parts: Query<(&WallPart, &mut Transform), Without<WallVis>>,
    wall: Option<Res<WallMaterials>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut last: Local<Option<Preset>>,
) {
    let (Some(states), Ok(mut root), Some(wall)) = (states, root.single_mut(), wall) else { return };
    let preset = states.cur.fruit.settings.preset();
    if *last == Some(preset) {
        return;
    }
    *last = Some(preset);
    let u = UNIT;
    let r = rules::region(&preset);
    let w = r.y_half * 2.0;
    let h = r.z_max - r.z_min;
    let zc = (r.z_max + r.z_min) / 2.0;
    root.translation = Vec3::new(preset.wall_distance * u, 0.0, 0.0);
    let x = -BEAM_DEPTH / 2.0;
    for (part, mut tf) in &mut parts {
        let (pos, size) = match part {
            WallPart::Slab => (Vec3::new(WALL_SLAB / 2.0, zc, 0.0), Vec3::new(WALL_SLAB, h, w)),
            WallPart::Top => (Vec3::new(x, r.z_max - BEAM / 2.0, 0.0), Vec3::new(BEAM_DEPTH, BEAM, w)),
            WallPart::Bottom => (Vec3::new(x, r.z_min + BEAM / 2.0, 0.0), Vec3::new(BEAM_DEPTH, BEAM, w)),
            WallPart::Left => (Vec3::new(x, zc, r.y_half - BEAM / 2.0), Vec3::new(BEAM_DEPTH, h, BEAM)),
            WallPart::Right => (Vec3::new(x, zc, -r.y_half + BEAM / 2.0), Vec3::new(BEAM_DEPTH, h, BEAM)),
        };
        tf.translation = pos * u;
        tf.scale = size * u;
    }
    for (handle, uv) in [(&wall.wood, x_face_uv(w, h)), (&wall.along, x_face_uv(w, BEAM)), (&wall.upright, x_face_uv(BEAM, h))] {
        if let Some(mut m) = materials.get_mut(handle) {
            m.uv_transform = uv;
        }
    }
}

fn setup(
    mut commands: Commands,
    theme: Res<Theme>,
    settings: Res<Settings>,
    assets: Res<GameAssets>,
    window: Query<&Window, With<PrimaryWindow>>,
    time: Res<Time<Real>>,
    mut poses: ResMut<TargetPoses>,
    mut countdown: ResMut<Countdown>,
) {
    // The gallery opens on the countdown (in place of the "Begin!" line a match starts on, see
    // `audio::match_start`): the soldiers come in with it.
    countdown.start(time.elapsed_secs_f64());
    poses.0 = vec![None; MAX_TARGETS];
    // The soldier pool: one model per slot, parented at the hull centre so a hit one can tumble.
    for slot in 0..MAX_TARGETS as u8 {
        let model = player_model::target_model(&mut commands, &assets, slot, Vec3::new(0.0, -BODY_CENTRE * UNIT, 0.0));
        commands
            .spawn((
                GameEntity,
                TargetVis { slot, id: None, spin_axis: Vec3::X, spin: 0.0, ragdoll: None, originals: Vec::new() },
                Transform::default(),
                Visibility::Hidden,
            ))
            .add_child(model);
    }
    let width = window.single().map(|w| w.width()).unwrap_or(1420.0);
    spawn_score(&mut commands, &theme, settings.fruit_rounds);
    spawn_frame(&mut commands, &theme, &settings, width);
    // The round's result and, below it, the countdown numbers, above the middle of the view.
    commands
        .spawn((
            GameEntity,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                top: Val::Percent(22.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(10.0),
                ..default()
            },
        ))
        .with_children(|p| {
            p.spawn((ResultText, theme.heading("", 44.0, theme::YELLOW), TextLayout { justify: Justify::Center, ..default() }));
            p.spawn((CountdownText, theme.heading("", 96.0, theme::YELLOW)));
        });
}

fn leave(mut poses: ResMut<TargetPoses>, mut countdown: ResMut<Countdown>) {
    poses.0.clear();
    *countdown = Countdown::default();
}

/// Top centre: soldiers hit, hit percentage, the chain, the round's progress or the endless mark,
/// the records (rounds only), and the reset button.
fn spawn_score(commands: &mut Commands, theme: &Theme, rounds: bool) {
    commands
        .spawn((
            GameEntity,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                top: Val::Px(12.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::Center,
                ..default()
            },
        ))
        .with_children(|p| {
            p.spawn(theme::panel(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(16.0),
                padding: UiRect::new(Val::Px(16.0), Val::Px(10.0), Val::Px(4.0), Val::Px(6.0)),
                ..default()
            }))
            .with_children(|r| {
                stat(r, theme, HitsText, "0", "HITS", theme::YELLOW, ());
                stat(r, theme, AccuracyText, "0%", "ACCURACY", theme::TAN_LIGHT, ());
                stat(r, theme, ChainText, "0", "CHAIN", theme::TAN_LIGHT, ());
                // The round's progress, or the endless mark, over a label that says which.
                let (value, label) = progress_text(rounds, 0);
                r.spawn(Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, min_width: Val::Px(72.0), ..default() })
                    .with_children(|c| {
                        c.spawn((ProgressText, ModeOnly(true), theme.heading(value, 30.0, theme::OFF_WHITE), no_wrap(), Node { display: shown(rounds), ..default() }));
                        c.spawn((EndlessIcon, ModeOnly(false), theme.symbol("∞", 34.0, theme::OFF_WHITE), no_wrap(), Node { display: shown(!rounds), ..default() }));
                        c.spawn((ProgressLabel, theme.label(label, 11.0, theme::OFF_WHITE)));
                    });
                // The records on the difficulty being played.
                stat(r, theme, RecordHitsText, "0", "BEST HITS", theme::ORANGE, ());
                stat(r, theme, RecordAccText, "0%", "BEST ACC", theme::ORANGE, ());
                stat(r, theme, RecordChainText, "0", "BEST CHAIN", theme::ORANGE, ());
                r.spawn(theme.button("reset", ResetButton, 92.0, 34.0, 14.0));
            });
        });
}

fn shown(on: bool) -> Display {
    if on { Display::Flex } else { Display::None }
}

/// The fourth stat's value and label: "7/20 SOLDIER" in rounds, the infinity and "ENDLESS" otherwise.
fn progress_text(rounds: bool, thrown: u32) -> (String, &'static str) {
    if rounds { (format!("{}/{ROUND_SIZE}", thrown.min(ROUND_SIZE)), "SOLDIER") } else { (String::new(), "ENDLESS") }
}

fn stat(r: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, marker: impl Component, value: &str, label: &str, color: Color, label_marker: impl Bundle) {
    // The records only mean something in rounds.
    let rounds_only = label.starts_with("BEST");
    let mut e = r.spawn(Node { flex_direction: FlexDirection::Column, align_items: AlignItems::Center, min_width: Val::Px(72.0), ..default() });
    if rounds_only {
        e.insert(ModeOnly(true));
    }
    e.with_children(|c| {
            c.spawn((marker, theme.heading(value, 30.0, color), no_wrap()));
            c.spawn((label_marker, theme.label(label, 11.0, theme::OFF_WHITE)));
        });
}

/// The floating options frame, top right to begin with: a title bar to drag it by with a fold
/// button, the endless / rounds choice, then the difficulty presets (rounds) or the sliders
/// for soldiers at a time, sideways speed and wall distance (endless play).
fn spawn_frame(commands: &mut Commands, theme: &Theme, settings: &Settings, window_width: f32) {
    commands
        .spawn((
            GameEntity,
            FruitFrame { anchor: FRAME_ANCHOR },
            theme::panel(Node {
                position_type: PositionType::Absolute,
                left: Val::Px((window_width - FRAME_W - 16.0).max(0.0)),
                top: Val::Px(70.0),
                width: Val::Px(FRAME_W),
                flex_direction: FlexDirection::Column,
                ..default()
            }),
        ))
        .with_children(|f| {
            f.spawn((
                DragHandle,
                Interaction::None,
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.06)),
            ))
            .with_children(|t| {
                t.spawn((theme.heading_flat("SETTINGS (TAB TO FOCUS)", 14.0, theme::TAN_LIGHT), no_wrap()));
                t.spawn((
                    Button,
                    CollapseButton,
                    theme::inset(Node { width: Val::Px(24.0), height: Val::Px(24.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() }),
                ))
                .with_children(|b| {
                    b.spawn((CollapseGlyph, theme.heading_flat("-", 16.0, theme::YELLOW)));
                });
            });
            f.spawn((
                FrameBody,
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(Val::Px(12.0)),
                    row_gap: Val::Px(2.0),
                    ..default()
                },
            ))
            .with_children(|b| {
                option_label(b, theme, "mode");
                option_row(b, |r| {
                    r.spawn(theme.button("endless", OptionButton::Rounds(false), 128.0, 30.0, 12.0));
                    r.spawn(theme.button("rounds of 20", OptionButton::Rounds(true), 128.0, 30.0, 12.0));
                });
                b.spawn((RoundsSection, section_node(settings.fruit_rounds))).with_children(|s| {
                    option_label(s, theme, "difficulty");
                    option_row(s, |r| {
                        for d in Difficulty::ALL {
                            r.spawn(theme.button(d.label(), OptionButton::Difficulty(d), 82.0, 30.0, 12.0));
                        }
                    });
                });
                b.spawn((EndlessSection, section_node(!settings.fruit_rounds))).with_children(|s| {
                    for (label, slider) in [("soldiers at a time", Slider::Soldiers), ("sideways speed", Slider::Speed), ("wall distance", Slider::WallDistance)] {
                        option_label(s, theme, label);
                        s.spawn(Node { flex_direction: FlexDirection::Row, align_items: AlignItems::Center, column_gap: Val::Px(12.0), ..default() })
                            .with_children(|r| slider_controls(r, theme, settings, slider, SLIDER_W));
                    }
                });
            });
        });
}

/// A section of the frame's body, shown or folded away.
fn section_node(on: bool) -> Node {
    Node { display: shown(on), width: Val::Percent(100.0), flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }
}

/// Shows the section for the mode being played and hides the other.
#[allow(clippy::type_complexity)]
fn frame_sections(settings: Res<Settings>, mut sections: Query<(&mut Node, Has<RoundsSection>), Or<(With<RoundsSection>, With<EndlessSection>)>>) {
    if !settings.is_changed() {
        return;
    }
    for (mut node, rounds) in &mut sections {
        let want = shown(rounds == settings.fruit_rounds);
        if node.display != want {
            node.display = want;
        }
    }
}

fn option_label(b: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, text: &str) {
    b.spawn((theme.heading_flat(text.to_uppercase(), 12.0, theme::OFF_WHITE), Node { margin: UiRect::top(Val::Px(6.0)), ..default() }));
}

/// A row of buttons spread across the frame.
fn option_row(b: &mut RelatedSpawnerCommands<ChildOf>, f: impl FnOnce(&mut RelatedSpawnerCommands<ChildOf>)) {
    b.spawn(Node { width: Val::Percent(100.0), flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween, ..default() }).with_children(f);
}

/// The room the window leaves for the frame: its size less the frame's (as last laid out;
/// logical pixels, like the cursor). Zero on an axis the frame does not fit on.
fn frame_room(window: &Window, frame: &ComputedNode) -> Option<Vec2> {
    let size = frame.size * frame.inverse_scale_factor;
    if size.x <= 0.0 || size.y <= 0.0 {
        return None;
    }
    Some((Vec2::new(window.width(), window.height()) - size).max(Vec2::ZERO))
}

/// Moves the frame by its title bar: the drag sets the anchor, `place_frame` puts it there.
fn drag_frame(
    mouse: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    handle: Query<&Interaction, With<DragHandle>>,
    mut frame: Query<(&mut FruitFrame, &Node, &ComputedNode)>,
    mut grab: Local<Option<Vec2>>,
) {
    let (Ok(window), Ok((mut frame, node, computed))) = (window.single(), frame.single_mut()) else { return };
    let Some(cursor) = window.cursor_position() else {
        *grab = None;
        return;
    };
    if mouse.just_pressed(MouseButton::Left) && handle.iter().any(|i| *i == Interaction::Pressed) {
        let (Val::Px(left), Val::Px(top)) = (node.left, node.top) else { return };
        *grab = Some(cursor - Vec2::new(left, top));
    }
    if !mouse.pressed(MouseButton::Left) {
        *grab = None;
        return;
    }
    let (Some(offset), Some(room)) = (*grab, frame_room(window, computed)) else { return };
    let pos = (cursor - offset).clamp(Vec2::ZERO, room);
    frame.anchor = Vec2::new(if room.x > 0.0 { pos.x / room.x } else { 0.0 }, if room.y > 0.0 { pos.y / room.y } else { 0.0 });
}

/// Keeps the frame at its anchor as the window (or the frame, folding) changes size.
fn place_frame(window: Query<&Window, With<PrimaryWindow>>, mut frame: Query<(&FruitFrame, &mut Node, &ComputedNode)>) {
    let (Ok(window), Ok((frame, mut node, computed))) = (window.single(), frame.single_mut()) else { return };
    let Some(room) = frame_room(window, computed) else { return };
    let pos = frame.anchor * room;
    let (left, top) = (Val::Px(pos.x.round()), Val::Px(pos.y.round()));
    // Only write on change: a mutated `Node` re-runs layout.
    if node.left != left || node.top != top {
        node.left = left;
        node.top = top;
    }
}

/// The fold button hides and shows the frame's body.
fn collapse_frame(
    buttons: Query<&Interaction, (Changed<Interaction>, With<CollapseButton>)>,
    mut body: Query<&mut Node, With<FrameBody>>,
    mut glyph: Query<&mut Text, With<CollapseGlyph>>,
) {
    if !buttons.iter().any(|i| *i == Interaction::Pressed) {
        return;
    }
    let Ok(mut node) = body.single_mut() else { return };
    let open = node.display == Display::None;
    node.display = if open { Display::Flex } else { Display::None };
    if let Ok(mut t) = glyph.single_mut() {
        t.0 = (if open { "-" } else { "+" }).to_string();
    }
}

/// Econ-button colours for a button that is not a menu action: lit orange while it is the
/// current choice, otherwise by hover state.
fn button_colour(interaction: Interaction, active: bool) -> Color {
    if active {
        theme::ORANGE
    } else {
        match interaction {
            Interaction::Pressed => theme::BTN_ACTIVE,
            Interaction::Hovered => theme::BTN_HOVER,
            Interaction::None => theme::BTN,
        }
    }
}

/// The difficulty and mode buttons: the chosen ones stay lit; a click picks, saves, and starts
/// over with a countdown (the simulation restarts on either change too).
fn option_buttons(
    mut settings: ResMut<Settings>,
    time: Res<Time<Real>>,
    mut countdown: ResMut<Countdown>,
    mut q: Query<(Ref<Interaction>, &OptionButton, &mut BackgroundColor)>,
    mut label: Query<&mut Text, With<ProgressLabel>>,
) {
    for (interaction, button, mut bg) in &mut q {
        let active = match *button {
            OptionButton::Difficulty(d) => settings.fruit_difficulty == d,
            OptionButton::Rounds(r) => settings.fruit_rounds == r,
        };
        if interaction.is_changed() && *interaction == Interaction::Pressed && !active {
            match *button {
                OptionButton::Difficulty(d) => settings.fruit_difficulty = d,
                OptionButton::Rounds(r) => {
                    settings.fruit_rounds = r;
                    if let Ok(mut t) = label.single_mut() {
                        t.0 = progress_text(r, 0).1.to_string();
                    }
                }
            }
            settings.save();
            // The simulation starts over on these too (see `FruitState::apply_input`).
            countdown.start(time.elapsed_secs_f64());
        }
        let want = button_colour(*interaction, active);
        if bg.0 != want {
            bg.0 = want;
        }
    }
}

fn reset_button(time: Res<Time<Real>>, mut countdown: ResMut<Countdown>, mut q: Query<(Ref<Interaction>, &mut BackgroundColor), With<ResetButton>>) {
    for (interaction, mut bg) in &mut q {
        if interaction.is_changed() && *interaction == Interaction::Pressed && !countdown.active() {
            countdown.start(time.elapsed_secs_f64());
        }
        let want = button_colour(*interaction, false);
        if bg.0 != want {
            bg.0 = want;
        }
    }
}

/// The end of a round: its result goes up for a moment (against the records on that
/// difficulty, which it may have beaten, with a flourish), then the countdown into the next
/// one (the reset it carries clears the stats, which the result has already captured).
#[allow(clippy::too_many_arguments)]
fn round_result(
    mut commands: Commands,
    fx: Res<PendingFx>,
    states: Option<Res<RenderStates>>,
    sfx: Res<Sfx>,
    time: Res<Time<Real>>,
    mut records: ResMut<Records>,
    mut countdown: ResMut<Countdown>,
    mut text: Query<&mut Text, With<ResultText>>,
) {
    let Some(states) = states else { return };
    for ev in &fx.events {
        if let SimEvent::RoundOver { hits, shots, best_chain } = *ev {
            let difficulty = states.cur.fruit.settings.difficulty;
            let accuracy = (hits * 100 + shots / 2).checked_div(shots).unwrap_or(0);
            let record = records.submit(difficulty, hits, accuracy, best_chain);
            if record {
                records.save();
                play(&mut commands, &sfx.record, 0.8);
            }
            let best = records.get(difficulty);
            if let Ok(mut t) = text.single_mut() {
                let title = if record { "NEW RECORD!\n" } else { "" };
                t.0 = format!("{title}{hits} hit (best {})\n{accuracy}% acc (best {}%)\n{best_chain} chain (best {})", best.hits, best.acc, best.chain);
            }
            if !countdown.active() {
                countdown.start(time.elapsed_secs_f64() + if record { RECORD_RESULT_SECS } else { RESULT_SECS });
            }
        }
    }
}

/// "3", "2", "1" with the hit sound, then "Begin!" and the soldiers come back (the reset flag
/// in the input drops with `Countdown::started`). A round's result clears with the first ding.
#[allow(clippy::type_complexity)]
fn run_countdown(
    mut commands: Commands,
    sfx: Res<Sfx>,
    time: Res<Time<Real>>,
    mut countdown: ResMut<Countdown>,
    mut text: Query<&mut Text, (With<CountdownText>, Without<ResultText>)>,
    mut result: Query<&mut Text, (With<ResultText>, Without<CountdownText>)>,
) {
    let Some(t0) = countdown.started else { return };
    let elapsed = time.elapsed_secs_f64() - t0;
    while (countdown.dings as usize) < COUNTDOWN_STEPS.len() && elapsed >= COUNTDOWN_STEPS[countdown.dings as usize] {
        play(&mut commands, &sfx.hitsound, 0.6);
        if let Ok(mut t) = text.single_mut() {
            t.0 = (3 - countdown.dings).to_string();
        }
        if let Ok(mut t) = result.single_mut() {
            t.0.clear();
        }
        countdown.dings += 1;
    }
    if elapsed >= COUNTDOWN_SECS {
        play(&mut commands, &sfx.announcer_begin, 0.85);
        if let Ok(mut t) = text.single_mut() {
            t.0.clear();
        }
        countdown.started = None;
    }
}

#[allow(clippy::type_complexity)]
fn update_score(
    states: Option<Res<RenderStates>>,
    records: Res<Records>,
    mut texts: ParamSet<(
        Query<&mut Text, With<HitsText>>,
        Query<&mut Text, With<AccuracyText>>,
        Query<&mut Text, With<ChainText>>,
        Query<&mut Text, With<ProgressText>>,
        Query<&mut Text, With<RecordHitsText>>,
        Query<&mut Text, With<RecordAccText>>,
        Query<&mut Text, With<RecordChainText>>,
    )>,
    mut marks: Query<(&mut Node, &ModeOnly)>,
) {
    let Some(states) = states else { return };
    let f = &states.cur.fruit;
    let rounds = f.settings.rounds;
    let best = records.get(f.settings.difficulty);
    let wanted = [f.hits.to_string(), format!("{}%", f.accuracy()), f.chain.to_string(), progress_text(rounds, f.thrown).0, best.hits.to_string(), format!("{}%", best.acc), best.chain.to_string()];
    let set = |text: Option<Mut<Text>>, want: &str| {
        if let Some(mut t) = text
            && t.0 != want
        {
            t.0 = want.to_string();
        }
    };
    set(texts.p0().single_mut().ok(), &wanted[0]);
    set(texts.p1().single_mut().ok(), &wanted[1]);
    set(texts.p2().single_mut().ok(), &wanted[2]);
    set(texts.p3().single_mut().ok(), &wanted[3]);
    set(texts.p4().single_mut().ok(), &wanted[4]);
    set(texts.p5().single_mut().ok(), &wanted[5]);
    set(texts.p6().single_mut().ok(), &wanted[6]);
    // The progress number and the records in rounds, the infinity in endless play.
    for (mut node, only) in &mut marks {
        let want = shown(only.0 == rounds);
        if node.display != want {
            node.display = want;
        }
    }
}

/// A grey copy of a soldier material: its texture drained of colour (a texture the CPU cannot
/// read is tinted instead). Made once per material.
fn grey_material(cache: &mut GreyCache, materials: &mut Assets<StandardMaterial>, images: &mut Assets<Image>, handle: &Handle<StandardMaterial>) -> Option<Handle<StandardMaterial>> {
    if let Some(h) = cache.materials.get(&handle.id()) {
        return Some(h.clone());
    }
    let mut m = materials.get(handle)?.clone();
    let drained = m.base_color_texture.as_ref().and_then(|t| grey_image(cache, images, t));
    m.base_color = if drained.is_some() { Color::srgb(0.8, 0.8, 0.8) } else { Color::srgb(0.45, 0.45, 0.45) };
    if let Some(img) = drained {
        m.base_color_texture = Some(img);
    }
    m.emissive = LinearRgba::BLACK;
    let h = materials.add(m);
    cache.materials.insert(handle.id(), h.clone());
    Some(h)
}

fn grey_image(cache: &mut GreyCache, images: &mut Assets<Image>, handle: &Handle<Image>) -> Option<Handle<Image>> {
    if let Some(h) = cache.images.get(&handle.id()) {
        return Some(h.clone());
    }
    let src = images.get(handle)?;
    if !matches!(src.texture_descriptor.format, TextureFormat::Rgba8UnormSrgb | TextureFormat::Rgba8Unorm) {
        return None;
    }
    let mut img = src.clone();
    for px in img.data.as_mut()?.as_chunks_mut::<4>().0 {
        let l = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) as u8;
        px[0] = l;
        px[1] = l;
        px[2] = l;
    }
    let h = images.add(img);
    cache.images.insert(handle.id(), h.clone());
    Some(h)
}

/// Puts a slot's own materials back and forgets the ragdoll, for a new soldier.
fn restore(commands: &mut Commands, vis: &mut TargetVis) {
    for (e, mat) in vis.originals.drain(..) {
        commands.entity(e).try_insert(MeshMaterial3d(mat));
    }
    vis.ragdoll = None;
}

/// Gives every soldier in the simulation a model from the pool and moves it: interpolated
/// between the last two states like the players, facing the player until hit, then a grey
/// ragdoll tumbling along its flight.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sync_targets(
    mut commands: Commands,
    states: Option<Res<RenderStates>>,
    local: Res<LocalHandle>,
    time: Res<Time<Real>>,
    mut poses: ResMut<TargetPoses>,
    mut cache: ResMut<GreyCache>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    children: Query<&Children>,
    mesh_materials: Query<&MeshMaterial3d<StandardMaterial>>,
    mut q: Query<(Entity, &mut TargetVis, &mut Transform, &mut Visibility)>,
) {
    let Some(states) = states else { return };
    let alpha = interp_alpha(&states, time.elapsed_secs_f64());
    let dt = time.delta_secs().min(0.1);
    let cur = &states.cur.fruit.targets;

    // Slots whose soldier is gone are freed; new soldiers take a free slot, each with its own
    // tumble axis for when it is hit.
    for (_, mut vis, _, mut visibility) in &mut q {
        if vis.id.is_some_and(|id| !cur.iter().any(|t| t.id == id)) {
            vis.id = None;
            *visibility = Visibility::Hidden;
            restore(&mut commands, &mut vis);
        }
    }
    for t in cur {
        if q.iter().any(|(_, v, ..)| v.id == Some(t.id)) {
            continue;
        }
        if let Some((_, mut vis, ..)) = q.iter_mut().find(|(_, v, ..)| v.id.is_none()) {
            vis.id = Some(t.id);
            vis.spin = 0.0;
            let a = t.id as f32 * 2.399;
            vis.spin_axis = Vec3::new(a.cos(), 0.35, a.sin()).normalize();
        }
    }

    if poses.0.len() != MAX_TARGETS {
        poses.0 = vec![None; MAX_TARGETS];
    }
    let me = states.cur.players[local.0].origin;
    for (entity, mut vis, mut tf, mut visibility) in &mut q {
        let slot = vis.slot as usize;
        let Some(t) = vis.id.and_then(|id| cur.iter().find(|t| t.id == id)) else {
            poses.0[slot] = None;
            continue;
        };
        let prev = states.prev.fruit.targets.iter().find(|p| p.id == t.id).map(|p| p.origin).unwrap_or(t.origin);
        let origin = if alpha > 1.0 { t.origin + t.velocity * ((alpha - 1.0) * TICK_SECS) } else { prev + (t.origin - prev) * alpha };
        let to_me = me - t.origin;
        let facing = to_me.y.atan2(to_me.x).to_degrees();
        if t.hit && vis.ragdoll.is_none() {
            vis.ragdoll = Some(Ragdoll { yaw: facing, since: 0.0 });
            for e in children.iter_descendants(entity) {
                if let Ok(mat) = mesh_materials.get(e)
                    && let Some(grey) = grey_material(&mut cache, &mut materials, &mut images, &mat.0)
                {
                    vis.originals.push((e, mat.0.clone()));
                    commands.entity(e).insert(MeshMaterial3d(grey));
                }
            }
        }
        let yaw = match vis.ragdoll.as_mut() {
            Some(r) => {
                r.since += dt;
                r.yaw
            }
            None => facing,
        };
        let mut rotation = Quat::from_rotation_y(yaw.to_radians());
        if vis.ragdoll.is_some() {
            vis.spin += dt * TUMBLE_RATE;
            rotation *= Quat::from_axis_angle(vis.spin_axis, vis.spin);
        }
        tf.translation = to_bevy(origin) + Vec3::Y * (BODY_CENTRE * UNIT);
        tf.rotation = rotation;
        *visibility = Visibility::Visible;
        poses.0[slot] = Some(TargetPose { velocity: t.velocity, yaw, weapon: t.weapon, ragdoll: vis.ragdoll.map(|r| r.since) });
    }
}
