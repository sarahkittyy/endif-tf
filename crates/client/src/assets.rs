//! Handles to the TF2 game assets (built by `tools/tf2/build_assets.py`), loaded once at startup so
//! entering a match doesn't wait on disk. They are requested while the app is being built, before
//! the fonts and sounds, so on the web the big models and textures get the browser's connections
//! first: they take the longest to download and to decode, and the loading screen waits on all of
//! them anyway.

use bevy::gltf::Gltf;
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;

/// Particle sprite strips: square frames side by side (`frames = width / height`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Sprite {
    Smoke1 = 0,
    Smokelit,
    Smoke2lit,
    Debris,
    Glow,
    Softglow,
    Ember,
}

pub const SPRITE_COUNT: usize = 7;
pub const SPRITE_FILES: [&str; SPRITE_COUNT] =
    ["smoke1", "smokelit", "smoke2lit", "debris_chunk", "brightglow_y", "softglow", "circle1"];
pub const SPRITE_FRAMES: [u32; SPRITE_COUNT] = [6, 5, 5, 6, 1, 1, 1];
/// Whether the sprite is drawn additively (`$additive 1` in its VMT) or alpha blended.
pub const SPRITE_ADDITIVE: [bool; SPRITE_COUNT] = [false, false, false, false, true, true, true];

/// The `Gltf` handles are kept so the clips/scenes stay loaded for the whole session.
///
/// Scene 0 of each file is taken from the loaded `Gltf` (`resolve_scenes`) rather than requested
/// as a labelled load: a labelled load (`file.glb#Scene0`) starts its own load of the file, so
/// asking for both fetched and decoded every model twice, and the second copy replaced the first
/// mid-session. The scene handles are `None` until the file has loaded.
#[derive(Resource)]
#[allow(dead_code)]
pub struct GameAssets {
    pub soldier: Handle<Gltf>,
    soldier_scene: Option<Handle<WorldAsset>>,
    pub viewmodel: Handle<Gltf>,
    viewmodel_scene: Option<Handle<WorldAsset>>,
    pub rocket: Handle<Gltf>,
    rocket_scene: Option<Handle<WorldAsset>>,
    pub soldier_blue: Handle<Image>,
    pub sleeves_blue: Handle<Image>,
    pub wall: Handle<Image>,
    pub floor: Handle<Image>,
    pub scorch: Handle<Image>,
    /// The sky, a cubemap strip built by `tools/tf2/skybox.py` (six faces stacked, wgpu order);
    /// `render::prepare_skybox` turns it into a cube texture once it has loaded.
    pub skybox: Handle<Image>,
    pub sprites: [Handle<Image>; SPRITE_COUNT],
}

/// The glTF files by index: soldier, viewmodel, rocket.
pub const GLTF_COUNT: usize = 3;

impl GameAssets {
    pub fn gltf(&self, i: usize) -> &Handle<Gltf> {
        [&self.soldier, &self.viewmodel, &self.rocket][i]
    }

    /// Scene 0 of file `i`, once it has loaded.
    pub fn scene(&self, i: usize) -> Option<&Handle<WorldAsset>> {
        [&self.soldier_scene, &self.viewmodel_scene, &self.rocket_scene][i].as_ref()
    }

    /// The soldier scene, or a placeholder handle (draws nothing) if the file has not loaded yet.
    pub fn soldier_scene(&self) -> Handle<WorldAsset> {
        self.soldier_scene.clone().unwrap_or_default()
    }

    pub fn viewmodel_scene(&self) -> Handle<WorldAsset> {
        self.viewmodel_scene.clone().unwrap_or_default()
    }

    pub fn rocket_scene(&self) -> Handle<WorldAsset> {
        self.rocket_scene.clone().unwrap_or_default()
    }

    fn scenes_resolved(&self) -> bool {
        (0..GLTF_COUNT).all(|i| self.scene(i).is_some())
    }
}

/// Fills in the scene handles as the glTF files finish loading.
fn resolve_scenes(mut assets: ResMut<GameAssets>, gltfs: Res<Assets<Gltf>>) {
    let GameAssets { soldier, soldier_scene, viewmodel, viewmodel_scene, rocket, rocket_scene, .. } = &mut *assets;
    for (file, scene) in [(&*soldier, soldier_scene), (&*viewmodel, viewmodel_scene), (&*rocket, rocket_scene)] {
        if scene.is_none()
            && let Some(gltf) = gltfs.get(file)
        {
            *scene = gltf.default_scene.clone().or_else(|| gltf.scenes.first().cloned());
        }
    }
}

pub struct GameAssetsPlugin;

impl Plugin for GameAssetsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameAssets>()
            .add_systems(PreUpdate, resolve_scenes.run_if(|assets: Res<GameAssets>| !assets.scenes_resolved()));
    }
}

pub(crate) fn repeat_sampler(settings: &mut ImageLoaderSettings) {
    settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        anisotropy_clamp: 8,
        ..ImageSamplerDescriptor::linear()
    });
}

impl FromWorld for GameAssets {
    fn from_world(world: &mut World) -> Self {
        let server = world.resource::<AssetServer>();
        let sprites = std::array::from_fn(|i| server.load(format!("particles/{}.png", SPRITE_FILES[i])));
        GameAssets {
            soldier: server.load("models/soldier.glb"),
            soldier_scene: None,
            viewmodel: server.load("models/viewmodel.glb"),
            viewmodel_scene: None,
            rocket: server.load("models/rocket.glb"),
            rocket_scene: None,
            soldier_blue: server.load("textures/soldier_blue.png"),
            sleeves_blue: server.load("textures/soldier_sleeves_blue.png"),
            wall: server.load_builder().with_settings(repeat_sampler).load("textures/wall.png"),
            floor: server.load_builder().with_settings(repeat_sampler).load("textures/floor.png"),
            scorch: server.load("textures/scorch.png"),
            skybox: server.load("textures/skybox.png"),
            sprites,
        }
    }
}
