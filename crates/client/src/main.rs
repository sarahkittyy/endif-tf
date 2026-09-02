//! endif client: desktop + web build of the TF2 soldier airshot 1v1.

mod account;
mod assets;
mod audio;
mod config;
mod copylink;
mod game;
mod hud;
mod loading;
mod menu;
mod net;
mod particles;
mod player_model;
mod render;
mod settings;
mod textfield;
mod theme;
mod viewmodel;
mod warmup;
mod webclip;
#[cfg(not(target_arch = "wasm32"))]
mod icon;

use bevy::asset::{AssetMetaCheck, AssetPlugin};
use bevy::prelude::*;
use bevy::window::{PresentMode, WindowResolution};

/// Top-level application state.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AppState {
    #[default]
    Menu,
    /// Waiting for the other peer to join the room.
    Connecting,
    InGame,
}

/// Marker for entities that belong to a match and are despawned when leaving it.
#[derive(Component)]
pub struct GameEntity;

/// Directory the assets are fetched from. `build-web.sh` names it after a hash of the asset
/// contents (`ENDIF_ASSET_DIR=assets-<hash>`) so the web server can mark everything in it as
/// cacheable forever; desktop builds use `assets` next to the crate.
const ASSET_DIR: &str = match option_env!("ENDIF_ASSET_DIR") {
    Some(dir) => dir,
    None => "assets",
};

fn main() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "endif.tf".to_string(),
                    resolution: WindowResolution::new(1280, 720),
                    present_mode: PresentMode::AutoNoVsync,
                    #[cfg(target_arch = "wasm32")]
                    canvas: Some("#endif-canvas".to_string()),
                    #[cfg(target_arch = "wasm32")]
                    fit_canvas_to_parent: true,
                    #[cfg(target_arch = "wasm32")]
                    prevent_default_event_handling: true,
                    ..default()
                }),
                ..default()
            })
            // There are no `.meta` files: without this every asset costs an extra request (a 404 on
            // the web, a failed stat on desktop) before the file itself is fetched.
            .set(AssetPlugin { meta_check: AssetMetaCheck::Never, file_path: ASSET_DIR.to_string(), ..default() })
            .set(bevy::log::LogPlugin {
                // matchbox only reports the WebRTC handshake (offers, ICE candidates, connection
                // states) at debug level; without it a failing peer connection is invisible.
                filter: "info,wgpu=warn,naga=warn,endif_client=debug,ggrs=info,matchbox_socket=debug".into(),
                ..default()
            }),
    )
    .init_state::<AppState>()
    .insert_resource(settings::Settings::load());
    let cfg = config::ClientConfig::load();
    app.insert_resource(account::Account::load(cfg.player_name.clone())).insert_resource(cfg)
    .add_plugins((
        #[cfg(not(target_arch = "wasm32"))]
        icon::IconPlugin,
        assets::GameAssetsPlugin,
        theme::ThemePlugin,
        textfield::TextFieldPlugin,
        account::AccountPlugin,
        audio::AudioFxPlugin,
        menu::MenuPlugin,
        net::NetPlugin,
        game::GamePlugin,
        render::RenderPlugin,
        player_model::PlayerModelPlugin,
        viewmodel::ViewmodelPlugin,
        particles::ParticlesPlugin,
        copylink::CopyLinkPlugin,
    ))
    .add_plugins((warmup::WarmupPlugin, hud::HudPlugin, loading::LoadingPlugin));
    app.run();
}
