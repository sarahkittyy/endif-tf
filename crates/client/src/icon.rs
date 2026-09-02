//! Desktop window icon (the rocket launcher), handed to winit once the window exists.

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
use bevy::winit::WINIT_WINDOWS;
use winit::window::Icon;

const ICON_PNG: &[u8] = include_bytes!("../assets/ui/rocket_launcher.png");

pub struct IconPlugin;

impl Plugin for IconPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, set_window_icon);
    }
}

fn decode_icon() -> Option<Icon> {
    let img = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Winit windows are main-thread only, hence the `NonSendMarker`.
fn set_window_icon(_main_thread: NonSendMarker, mut icon: Local<Option<Icon>>, mut done: Local<bool>) {
    if *done {
        return;
    }
    if icon.is_none() {
        match decode_icon() {
            Some(i) => *icon = Some(i),
            None => {
                warn!("could not decode the window icon");
                *done = true;
                return;
            }
        }
    }
    WINIT_WINDOWS.with_borrow(|windows| {
        if windows.windows.is_empty() {
            return;
        }
        for window in windows.windows.values() {
            window.set_window_icon(icon.clone());
        }
        *done = true;
    });
}
