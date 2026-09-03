//! Startup progress for the web page's loading screen.
//!
//! `web/index.html` shows a TF2-style loading screen while the wasm downloads, then keeps it up
//! until the assets fetched at startup (models, textures, fonts, sounds) have arrived, so the first
//! thing drawn on the canvas is a complete menu rather than missing fonts over a black background.
//! This reports `loaded / total` to `window.endifProgress` whenever the count changes, and logs which
//! asset finished when (plus a "still waiting" list every few seconds) so a slow start can be read
//! off the browser console. Menu music is not waited for: it only starts downloading once the screen
//! is gone (see `audio.rs`), so it never competes with the assets the screen is waiting on.
//! `StartupDone` marks that point; on desktop it is inserted right away. Joining a room from a
//! link (`menu.rs`) and playing any sound wait for it: a match must not start, and its countdown
//! run, under the loading screen, and audio started while the render warm-up is compiling shaders
//! only crackles.

use bevy::asset::{LoadState, RecursiveDependencyLoadState, UntypedAssetId, UntypedHandle};
use bevy::prelude::*;

use crate::assets::GameAssets;
use crate::audio::Sfx;
use crate::theme::Theme;
use crate::warmup::WarmupProgress;

pub struct LoadingPlugin;

impl Plugin for LoadingPlugin {
    fn build(&self, app: &mut App) {
        if cfg!(target_arch = "wasm32") {
            app.add_systems(Startup, collect)
                .add_systems(Update, report.run_if(resource_exists::<StartupAssets>));
        } else {
            app.insert_resource(StartupDone);
        }
    }
}

/// Present once the loading screen is gone (immediately on desktop). Downloads that are not needed
/// for the first frame (the menu music) wait for this.
#[derive(Resource)]
pub struct StartupDone;

/// The handles the loading screen waits for, and the count last reported to the page.
#[derive(Resource)]
struct StartupAssets {
    handles: Vec<UntypedHandle>,
    /// Per handle: already seen finished (and logged).
    done: Vec<bool>,
    reported: Option<usize>,
    /// When the pending list was last logged.
    last_pending_log: f64,
}

/// Seconds since the page started loading (the browser's `performance.now()`), so the engine's
/// startup lines line up with the ones `web/index.html` prints. Seconds since first call on desktop.
pub fn elapsed_secs() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js::now() / 1000.0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
    }
}

fn name(handle: &UntypedHandle) -> String {
    handle.path().map(|p| p.to_string()).unwrap_or_else(|| format!("{:?}", handle.id()))
}

fn collect(mut commands: Commands, assets: Res<GameAssets>, theme: Res<Theme>, sfx: Res<Sfx>) {
    let mut handles = vec![
        assets.soldier.clone().untyped(),
        assets.viewmodel.clone().untyped(),
        assets.rocket.clone().untyped(),
        assets.soldier_blue.clone().untyped(),
        assets.sleeves_blue.clone().untyped(),
        assets.wall.clone().untyped(),
        assets.floor.clone().untyped(),
        assets.scorch.clone().untyped(),
        assets.skybox.clone().untyped(),
        theme.build.clone().untyped(),
        theme.secondary.clone().untyped(),
        theme.symbols.clone().untyped(),
        theme.soldier.clone().untyped(),
        theme.menu_bg.clone().untyped(),
    ];
    handles.extend(assets.sprites.iter().map(|h| h.clone().untyped()));
    handles.extend(sfx.startup_clips().into_iter().map(Handle::untyped));
    let now = elapsed_secs();
    info!("[startup {now:.2}s] engine running (renderer initialised); waiting for {} assets and the render warm-up", handles.len());
    commands.insert_resource(StartupAssets { done: vec![false; handles.len()], handles, reported: None, last_pending_log: now });
}

/// Loaded with all its dependencies, or failed: a missing file must not keep the screen up forever.
pub fn settled(server: &AssetServer, id: impl Into<UntypedAssetId>) -> bool {
    let id = id.into();
    server.is_loaded_with_dependencies(id)
        || matches!(server.load_state(id), LoadState::Failed(_))
        || matches!(server.recursive_dependency_load_state(id), RecursiveDependencyLoadState::Failed(_))
}

fn report(mut commands: Commands, server: Res<AssetServer>, mut startup: ResMut<StartupAssets>, warm: Res<WarmupProgress>) {
    let now = elapsed_secs();
    let StartupAssets { handles, done, .. } = &mut *startup;
    for (h, seen) in handles.iter().zip(done.iter_mut()) {
        if *seen || !settled(&server, h.id()) {
            continue;
        }
        *seen = true;
        match server.load_state(h.id()) {
            LoadState::Failed(e) => warn!("[startup {now:.2}s] FAILED {}: {e}", name(h)),
            _ => info!("[startup {now:.2}s] loaded {}", name(h)),
        }
    }
    // The render warm-up (see `warmup.rs`) counts its steps into the total, so the screen stays
    // up, with the bar still moving, until the match's shaders are compiled.
    let total = startup.handles.len() + warm.total as usize;
    let done = startup.done.iter().filter(|d| **d).count() + warm.done as usize;
    if done < total && now - startup.last_pending_log >= 5.0 {
        startup.last_pending_log = now;
        let mut pending: Vec<String> = startup
            .handles
            .iter()
            .zip(&startup.done)
            .filter(|(_, d)| !**d)
            .map(|(h, _)| format!("{} ({:?})", name(h), server.load_state(h.id())))
            .collect();
        if warm.done < warm.total {
            pending.push(format!("render warm-up ({}/{})", warm.done, warm.total));
        }
        info!("[startup {now:.2}s] still waiting ({done}/{total}): {}", pending.join(", "));
    }
    if startup.reported == Some(done) {
        return;
    }
    startup.reported = Some(done);
    debug!("startup assets loaded: {done}/{total}");
    #[cfg(target_arch = "wasm32")]
    if let Err(e) = js::progress(done as u32, total as u32) {
        warn!("window.endifProgress failed: {e:?}");
    }
    if done == total {
        info!("[startup {now:.2}s] all startup assets and the warm-up are done");
        commands.remove_resource::<StartupAssets>();
        commands.insert_resource(StartupDone);
    }
}

#[cfg(target_arch = "wasm32")]
mod js {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        /// Defined by `web/index.html`; `catch` so a page without it (or an old cached page) only
        /// yields an `Err` instead of a panic.
        #[wasm_bindgen(catch, js_namespace = window, js_name = endifProgress)]
        pub fn progress(done: u32, total: u32) -> Result<(), JsValue>;
        /// `performance.now()`: milliseconds since the page started loading.
        #[wasm_bindgen(js_namespace = performance)]
        pub fn now() -> f64;
    }
}
