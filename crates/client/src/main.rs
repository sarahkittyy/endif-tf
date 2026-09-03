//! endif client: desktop + web build of the TF2 soldier airshot 1v1.

// Release builds on Windows are a plain window with no console behind it. Logs still reach a
// terminal the game was started from: see `attach_parent_console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account;
mod assets;
mod audio;
mod config;
mod copylink;
#[cfg(not(target_arch = "wasm32"))]
mod fullscreen;
mod fruit;
mod game;
mod hud;
#[cfg(not(target_arch = "wasm32"))]
mod icon;
mod loading;
mod menu;
mod net;
#[cfg(feature = "netsim")]
mod netsim;
mod netstats;
mod particles;
mod player_model;
mod render;
mod settings;
mod textfield;
mod theme;
#[cfg(not(target_arch = "wasm32"))]
mod update;
mod viewmodel;
mod warmup;
mod webclip;

use bevy::asset::{AssetMetaCheck, AssetPlugin};
use bevy::prelude::*;
use bevy::render::error_handler::{ErrorType, RenderErrorHandler, RenderErrorPolicy};
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

/// A `windows_subsystem = "windows"` binary starts without a console, so nothing it prints goes
/// anywhere. When it was started from a terminal, attach to that terminal so the logs (and the
/// `--build-id` answer) land there. Standard handles that are already set are left alone: the
/// updater pipes `--build-id`, and a debug build has its own console. Started from Explorer there
/// is no parent console, the call fails and the game runs silently, which is the point.
#[cfg(windows)]
fn attach_parent_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(std_handle: u32) -> isize;
        fn AttachConsole(process_id: u32) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const INVALID_HANDLE_VALUE: isize = -1;
    // SAFETY: plain Win32 calls that take and return integers only.
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if stdout == 0 || stdout == INVALID_HANDLE_VALUE {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

fn main() {
    #[cfg(windows)]
    attach_parent_console();
    // `endif --build-id` prints the build identity (the commit) and exits: the updater asks a
    // freshly downloaded build this before installing it, and `deploy/package-desktop.sh` writes
    // it next to the package. `--protocol` (the simulation identity) is what updaters shipped
    // before the build id existed ask; keep answering it.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--build-id") {
            println!("{}", endif_sim::BUILD_ID);
            return;
        }
        if args.iter().any(|a| a == "--protocol") {
            println!("{}", endif_sim::protocol_id());
            return;
        }
    }
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();
    #[cfg(target_os = "linux")]
    prefer_reachable_display();

    let settings = settings::Settings::load();
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "endif.tf".to_string(),
                    resolution: WindowResolution::new(1420, 800),
                    present_mode: PresentMode::AutoNoVsync,
                    #[cfg(not(target_arch = "wasm32"))]
                    mode: fullscreen::mode_for(settings.fullscreen),
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
            .set(AssetPlugin {
                meta_check: AssetMetaCheck::Never,
                file_path: ASSET_DIR.to_string(),
                ..default()
            })
            .set(bevy::log::LogPlugin {
                // matchbox only reports the WebRTC handshake (offers, ICE candidates, connection
                // states) at debug level; without it a failing peer connection is invisible.
                filter:
                    "info,wgpu=warn,naga=warn,endif_client=debug,ggrs=info,matchbox_socket=debug"
                        .into(),
                ..default()
            }),
    )
    .init_state::<AppState>()
    // Bevy's default handler quits on any wgpu error. On the web the browser expires the canvas
    // swap chain texture when the tab is hidden (or resized) mid-frame, which shows up as a
    // validation error on a destroyed 'swap chain texture'. The next frame acquires a fresh one,
    // so that case is skipped; everything else keeps the default quit-on-error behaviour.
    .insert_resource(RenderErrorHandler(|error, main_world, _| {
        if error.ty == ErrorType::Validation && error.description.contains("swap chain texture") {
            return RenderErrorPolicy::Ignore;
        }
        error!("Quitting the application due to {:?} RenderError", error.ty);
        main_world.write_message(AppExit::error());
        RenderErrorPolicy::StopRendering
    }))
    .insert_resource(settings);
    let cfg = config::ClientConfig::load();
    app.insert_resource(account::Account::load(cfg.player_name.clone()))
        .insert_resource(cfg)
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
            netstats::NetStatsPlugin,
            game::GamePlugin,
            render::RenderPlugin,
            player_model::PlayerModelPlugin,
            viewmodel::ViewmodelPlugin,
            particles::ParticlesPlugin,
            copylink::CopyLinkPlugin,
        ))
        .add_plugins((
            warmup::WarmupPlugin,
            hud::HudPlugin,
            fruit::FruitPlugin,
            loading::LoadingPlugin,
            #[cfg(not(target_arch = "wasm32"))]
            fullscreen::FullscreenPlugin,
        ));
    app.run();
}

/// winit picks Wayland whenever `WAYLAND_DISPLAY` is set and never falls back to X11, so on a box
/// where the variable is set but no compositor is listening (WSL with a third-party X server, a
/// stale login environment) the window fails to open although `DISPLAY` would work. When the
/// Wayland socket is missing and an X display is named, drop the variable so winit uses X11.
/// `WINIT_UNIX_BACKEND=x11|wayland` still overrides everything.
#[cfg(target_os = "linux")]
fn prefer_reachable_display() {
    use std::{env, path::Path};
    if env::var_os("WINIT_UNIX_BACKEND").is_some() {
        return;
    }
    let Some(wayland) = env::var("WAYLAND_DISPLAY").ok().filter(|v| !v.is_empty()) else {
        return;
    };
    if env::var("DISPLAY").map(|v| v.is_empty()).unwrap_or(true) {
        return;
    }
    let socket = if wayland.starts_with('/') {
        Path::new(&wayland).to_path_buf()
    } else {
        match env::var("XDG_RUNTIME_DIR") {
            Ok(dir) if !dir.is_empty() => Path::new(&dir).join(&wayland),
            _ => return,
        }
    };
    if !socket.exists() {
        eprintln!(
            "WAYLAND_DISPLAY={wayland} but {} does not exist; using X11 (DISPLAY)",
            socket.display()
        );
        // The process is still single-threaded here (before Bevy starts its task pools).
        unsafe { env::remove_var("WAYLAND_DISPLAY") };
    }
}
