//! Desktop fullscreen: F11 or Alt+Enter toggles borderless fullscreen on the monitor the window
//! is on. The choice is kept in the `[video] fullscreen` setting so the next launch starts the
//! same way. The browser owns those keys on the web, so this module is desktop only.

use crate::settings::{Settings, alt_held};
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PrimaryWindow, WindowMode};

pub struct FullscreenPlugin;

impl Plugin for FullscreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, toggle_fullscreen);
    }
}

/// The window mode the setting asks for.
pub fn mode_for(fullscreen: bool) -> WindowMode {
    if fullscreen {
        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
    } else {
        WindowMode::Windowed
    }
}

fn toggle_fullscreen(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<Settings>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
) {
    let toggled = keys.just_pressed(KeyCode::F11) || (alt_held(&keys) && keys.just_pressed(KeyCode::Enter));
    if !toggled {
        return;
    }
    let Ok(mut window) = window.single_mut() else { return };
    let fullscreen = matches!(window.mode, WindowMode::Windowed);
    window.mode = mode_for(fullscreen);
    settings.fullscreen = fullscreen;
    settings.save();
}
