//! User settings: mouse sensitivity, invert Y, key bindings. Persisted as `settings.ini` next to
//! the executable on desktop (falling back to the per-user config directory when that is not
//! writable) and to `localStorage` on the web.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// TF2 `m_yaw` / `m_pitch`: degrees per mouse count at sensitivity 1.
pub const DEGREES_PER_COUNT: f32 = 0.022;
pub const SENS_MIN: f32 = 0.1;
pub const SENS_MAX: f32 = 10.0;

/// Game actions that can be bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Crouch,
    Fire,
}

impl Action {
    pub const ALL: [Action; 7] =
        [Action::Forward, Action::Back, Action::Left, Action::Right, Action::Jump, Action::Crouch, Action::Fire];

    pub fn label(self) -> &'static str {
        match self {
            Action::Forward => "Move forward",
            Action::Back => "Move back",
            Action::Left => "Strafe left",
            Action::Right => "Strafe right",
            Action::Jump => "Jump",
            Action::Crouch => "Crouch",
            Action::Fire => "Fire",
        }
    }

    fn ini_key(self) -> &'static str {
        match self {
            Action::Forward => "forward",
            Action::Back => "back",
            Action::Left => "left",
            Action::Right => "right",
            Action::Jump => "jump",
            Action::Crouch => "crouch",
            Action::Fire => "fire",
        }
    }
}

/// A physical key or a mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Binding {
    Key(KeyCode),
    Mouse(MouseButton),
}

impl Binding {
    pub fn pressed(self, keys: &ButtonInput<KeyCode>, mouse: &ButtonInput<MouseButton>) -> bool {
        match self {
            Binding::Key(k) => keys.pressed(k),
            Binding::Mouse(b) => mouse.pressed(b),
        }
    }

    /// Short human-readable name.
    pub fn label(self) -> String {
        match self {
            Binding::Key(k) => {
                let s = format!("{k:?}");
                let s = s
                    .strip_prefix("Key")
                    .or_else(|| s.strip_prefix("Digit"))
                    .map(str::to_string)
                    .unwrap_or(s);
                match s.as_str() {
                    "ControlLeft" => "L Ctrl".into(),
                    "ControlRight" => "R Ctrl".into(),
                    "ShiftLeft" => "L Shift".into(),
                    "ShiftRight" => "R Shift".into(),
                    "AltLeft" => "L Alt".into(),
                    "AltRight" => "R Alt".into(),
                    "ArrowUp" => "Up".into(),
                    "ArrowDown" => "Down".into(),
                    "ArrowLeft" => "Left".into(),
                    "ArrowRight" => "Right".into(),
                    _ => s,
                }
            }
            Binding::Mouse(b) => match b {
                MouseButton::Left => "Mouse 1".into(),
                MouseButton::Right => "Mouse 2".into(),
                MouseButton::Middle => "Mouse 3".into(),
                MouseButton::Back => "Mouse 4".into(),
                MouseButton::Forward => "Mouse 5".into(),
                MouseButton::Other(n) => format!("Mouse {}", n + 1),
            },
        }
    }

    /// `key:KeyW` / `mouse:left` / `mouse:other:7` in the ini file.
    fn to_ini(self) -> String {
        match self {
            Binding::Key(k) => format!("key:{k:?}"),
            Binding::Mouse(MouseButton::Left) => "mouse:left".into(),
            Binding::Mouse(MouseButton::Right) => "mouse:right".into(),
            Binding::Mouse(MouseButton::Middle) => "mouse:middle".into(),
            Binding::Mouse(MouseButton::Back) => "mouse:back".into(),
            Binding::Mouse(MouseButton::Forward) => "mouse:forward".into(),
            Binding::Mouse(MouseButton::Other(n)) => format!("mouse:other:{n}"),
        }
    }

    fn from_ini(s: &str) -> Option<Binding> {
        if let Some(k) = s.strip_prefix("key:") {
            // KeyCode's serde form is its Debug name as a JSON string.
            return serde_json::from_str::<KeyCode>(&format!("\"{k}\"")).ok().map(Binding::Key);
        }
        let m = s.strip_prefix("mouse:")?;
        Some(Binding::Mouse(match m {
            "left" => MouseButton::Left,
            "right" => MouseButton::Right,
            "middle" => MouseButton::Middle,
            "back" => MouseButton::Back,
            "forward" => MouseButton::Forward,
            other => MouseButton::Other(other.strip_prefix("other:")?.parse().ok()?),
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bindings {
    pub forward: Binding,
    pub back: Binding,
    pub left: Binding,
    pub right: Binding,
    pub jump: Binding,
    pub crouch: Binding,
    pub fire: Binding,
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings {
            forward: Binding::Key(KeyCode::KeyW),
            back: Binding::Key(KeyCode::KeyS),
            left: Binding::Key(KeyCode::KeyA),
            right: Binding::Key(KeyCode::KeyD),
            jump: Binding::Key(KeyCode::Space),
            // TF2's Ctrl on desktop. In a browser Ctrl+W (crouch while running forward) closes the
            // tab and a page cannot cancel it, so the web build starts on Shift; Ctrl stays
            // bindable, with a warning in the settings.
            crouch: Binding::Key(if cfg!(target_arch = "wasm32") { KeyCode::ShiftLeft } else { KeyCode::ControlLeft }),
            fire: Binding::Mouse(MouseButton::Left),
        }
    }
}

impl Bindings {
    pub fn get(&self, a: Action) -> Binding {
        match a {
            Action::Forward => self.forward,
            Action::Back => self.back,
            Action::Left => self.left,
            Action::Right => self.right,
            Action::Jump => self.jump,
            Action::Crouch => self.crouch,
            Action::Fire => self.fire,
        }
    }

    pub fn set(&mut self, a: Action, b: Binding) {
        match a {
            Action::Forward => self.forward = b,
            Action::Back => self.back = b,
            Action::Left => self.left = b,
            Action::Right => self.right = b,
            Action::Jump => self.jump = b,
            Action::Crouch => self.crouch = b,
            Action::Fire => self.fire = b,
        }
    }

    pub fn pressed(&self, a: Action, keys: &ButtonInput<KeyCode>, mouse: &ButtonInput<MouseButton>) -> bool {
        self.get(a).pressed(keys, mouse)
    }
}

/// Which mouse axis a sensitivity control edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
}

/// A value edited with a slider + value box in the menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slider {
    Sens(Axis),
    /// Master volume, 0..1 (shown as a percentage).
    Volume,
}

impl Slider {
    pub fn range(self) -> (f32, f32) {
        match self {
            Slider::Sens(_) => (SENS_MIN, SENS_MAX),
            Slider::Volume => (0.0, 1.0),
        }
    }

    pub fn value(self, s: &Settings) -> f32 {
        match self {
            Slider::Sens(axis) => s.sensitivity(axis),
            Slider::Volume => s.volume,
        }
    }

    pub fn set(self, s: &mut Settings, v: f32) {
        match self {
            Slider::Sens(axis) => s.set_sensitivity(axis, v),
            Slider::Volume => s.set_volume(v),
        }
    }

    /// Position of the value along the slider, 0..1.
    pub fn fraction(self, s: &Settings) -> f32 {
        let (lo, hi) = self.range();
        ((self.value(s) - lo) / (hi - lo)).clamp(0.0, 1.0)
    }

    /// Text shown in the value box.
    pub fn display(self, s: &Settings) -> String {
        match self {
            Slider::Sens(_) => format!("{:.2}", self.value(s)),
            Slider::Volume => format!("{:.0}%", self.value(s) * 100.0),
        }
    }

    /// Initial contents of the value box when it is opened for typing.
    pub fn edit_text(self, s: &Settings) -> String {
        match self {
            Slider::Sens(_) => format!("{:.2}", self.value(s)),
            Slider::Volume => format!("{:.0}", self.value(s) * 100.0),
        }
    }

    /// Parses typed text back into a value (volume is typed as a percentage).
    pub fn parse(self, text: &str) -> Option<f32> {
        let v: f32 = text.trim().trim_end_matches('%').trim().parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        Some(match self {
            Slider::Sens(_) => v,
            Slider::Volume => v / 100.0,
        })
    }
}

#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// TF2-style sensitivity (2.5 → 0.055°/count).
    pub sensitivity_x: f32,
    pub sensitivity_y: f32,
    /// When false (the default) the two axes are locked together and edited as one value.
    pub separate_sensitivity: bool,
    pub invert_y: bool,
    /// Master volume, 0..1.
    pub volume: f32,
    /// Web only: take the page fullscreen on the click that starts play. Off by default; in
    /// Chromium it arms the keyboard lock, which lets Ctrl be bound without Ctrl+W closing the tab.
    pub fullscreen: bool,
    pub bindings: Bindings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sensitivity_x: 2.5,
            sensitivity_y: 2.5,
            separate_sensitivity: false,
            invert_y: false,
            volume: 0.75,
            fullscreen: false,
            bindings: Bindings::default(),
        }
    }
}

impl Settings {
    pub fn yaw_per_count(&self) -> f32 {
        DEGREES_PER_COUNT * self.sensitivity_x
    }

    pub fn pitch_per_count(&self) -> f32 {
        DEGREES_PER_COUNT * self.sensitivity_y * if self.invert_y { -1.0 } else { 1.0 }
    }

    pub fn sensitivity(&self, axis: Axis) -> f32 {
        match axis {
            Axis::X => self.sensitivity_x,
            Axis::Y => self.sensitivity_y,
        }
    }

    /// Sets a sensitivity, clamped to the slider range and rounded to two decimals. While the
    /// axes are locked together, either axis sets both.
    pub fn set_sensitivity(&mut self, axis: Axis, value: f32) {
        let v = (value.clamp(SENS_MIN, SENS_MAX) * 100.0).round() / 100.0;
        if !self.separate_sensitivity {
            self.sensitivity_x = v;
            self.sensitivity_y = v;
            return;
        }
        match axis {
            Axis::X => self.sensitivity_x = v,
            Axis::Y => self.sensitivity_y = v,
        }
    }

    /// Sets the master volume, clamped to 0..1 and rounded to whole percents.
    pub fn set_volume(&mut self, value: f32) {
        self.volume = (value.clamp(0.0, 1.0) * 100.0).round() / 100.0;
    }

    /// Unlocks or relocks the axes; relocking copies X into Y.
    pub fn set_separate_sensitivity(&mut self, separate: bool) {
        self.separate_sensitivity = separate;
        if !separate {
            self.sensitivity_y = self.sensitivity_x;
        }
    }

    pub fn load() -> Self {
        match storage::read(FILE) {
            Some(text) => Settings::from_ini(&text),
            None => Settings::default(),
        }
    }

    pub fn save(&self) {
        if let Err(e) = storage::write(FILE, &self.to_ini()) {
            warn!("could not save settings: {e}");
        }
    }

    pub fn to_ini(&self) -> String {
        let mut out = String::from("; endif.tf settings\n[mouse]\n");
        out.push_str(&format!("sensitivity_x = {:.2}\n", self.sensitivity_x));
        out.push_str(&format!("sensitivity_y = {:.2}\n", self.sensitivity_y));
        out.push_str(&format!("separate_sensitivity = {}\n", self.separate_sensitivity));
        out.push_str(&format!("invert_y = {}\n\n[audio]\n", self.invert_y));
        out.push_str(&format!("volume = {:.2}\n\n[video]\n", self.volume));
        out.push_str(&format!("fullscreen = {}\n\n[keys]\n", self.fullscreen));
        for a in Action::ALL {
            out.push_str(&format!("{} = {}\n", a.ini_key(), self.bindings.get(a).to_ini()));
        }
        out
    }

    /// Lenient parser: unknown or malformed lines keep the default for that setting.
    pub fn from_ini(text: &str) -> Self {
        let mut s = Settings::default();
        // Axes are read as separate values first; the lock is applied once the file is parsed.
        s.separate_sensitivity = true;
        let mut separate = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "sensitivity_x" => {
                    if let Ok(x) = v.parse() {
                        s.set_sensitivity(Axis::X, x);
                    }
                }
                "sensitivity_y" => {
                    if let Ok(y) = v.parse() {
                        s.set_sensitivity(Axis::Y, y);
                    }
                }
                "separate_sensitivity" => separate = matches!(v, "true" | "1" | "yes"),
                "invert_y" => s.invert_y = matches!(v, "true" | "1" | "yes"),
                "fullscreen" => s.fullscreen = matches!(v, "true" | "1" | "yes"),
                "volume" => {
                    if let Ok(x) = v.parse() {
                        s.set_volume(x);
                    }
                }
                _ => {
                    if let Some(a) = Action::ALL.iter().find(|a| a.ini_key() == k)
                        && let Some(b) = Binding::from_ini(v)
                    {
                        s.bindings.set(*a, b);
                    }
                }
            }
        }
        s.set_separate_sensitivity(separate);
        s
    }
}

/// File name of the settings in the shared storage.
const FILE: &str = "settings.ini";

/// Small named text files: next to the executable on desktop (falling back to the per-user config
/// directory when that is not writable) and `localStorage` entries on the web. Shared by the
/// settings and the account state.
#[cfg(not(target_arch = "wasm32"))]
pub mod storage {
    use std::path::PathBuf;

    /// `<name>` next to the executable.
    fn exe_path(name: &str) -> Option<PathBuf> {
        Some(std::env::current_exe().ok()?.parent()?.join(name))
    }

    /// Fallback for installs where the executable's directory is read-only.
    fn config_path(name: &str) -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("endif.tf").join(name))
    }

    pub fn read(name: &str) -> Option<String> {
        [exe_path(name), config_path(name)].into_iter().flatten().find_map(|p| std::fs::read_to_string(p).ok())
    }

    pub fn write(name: &str, text: &str) -> Result<(), String> {
        if let Some(p) = exe_path(name)
            && std::fs::write(&p, text).is_ok()
        {
            return Ok(());
        }
        let p = config_path(name).ok_or("no config directory")?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&p, text).map_err(|e| e.to_string())
    }
}

#[cfg(target_arch = "wasm32")]
pub mod storage {
    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    fn key(name: &str) -> String {
        format!("endif.tf/{name}")
    }

    pub fn read(name: &str) -> Option<String> {
        storage()?.get_item(&key(name)).ok().flatten()
    }

    pub fn write(name: &str, text: &str) -> Result<(), String> {
        storage().ok_or("localStorage unavailable")?.set_item(&key(name), text).map_err(|_| "localStorage write failed".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_axes_move_together_and_persist() {
        let mut s = Settings::default();
        assert!(!s.separate_sensitivity);
        s.set_sensitivity(Axis::Y, 3.1);
        assert_eq!((s.sensitivity_x, s.sensitivity_y), (3.1, 3.1));
        let back = Settings::from_ini(&s.to_ini());
        assert_eq!(back, s);

        s.set_separate_sensitivity(true);
        s.set_sensitivity(Axis::Y, 1.5);
        assert_eq!((s.sensitivity_x, s.sensitivity_y), (3.1, 1.5));
        let back = Settings::from_ini(&s.to_ini());
        assert_eq!(back, s);

        // Relocking snaps Y back to X, also when an old file had no lock key.
        s.set_separate_sensitivity(false);
        assert_eq!(s.sensitivity_y, 3.1);
        let legacy = Settings::from_ini("[mouse]\nsensitivity_x = 2.00\nsensitivity_y = 4.00\n");
        assert!(!legacy.separate_sensitivity);
        assert_eq!((legacy.sensitivity_x, legacy.sensitivity_y), (2.0, 2.0));
        assert_eq!(legacy.volume, 0.75);
    }

    #[test]
    fn volume_slider_round_trips() {
        let mut s = Settings::default();
        assert_eq!(s.volume, 0.75);
        assert_eq!(Slider::Volume.display(&s), "75%");
        Slider::Volume.set(&mut s, Slider::Volume.parse("40").unwrap());
        assert_eq!(s.volume, 0.4);
        assert_eq!(Slider::Volume.parse("120%"), Some(1.2));
        Slider::Volume.set(&mut s, 1.2);
        assert_eq!(s.volume, 1.0);
        s.set_volume(0.333);
        assert_eq!(Settings::from_ini(&s.to_ini()), s);
        assert_eq!(s.volume, 0.33);
    }

    #[test]
    fn fullscreen_round_trips_and_defaults_off() {
        let mut s = Settings::default();
        assert!(!s.fullscreen);
        s.fullscreen = true;
        assert!(Settings::from_ini(&s.to_ini()).fullscreen);
        assert!(!Settings::from_ini("[audio]\nvolume = 0.50\n").fullscreen);
    }
}
