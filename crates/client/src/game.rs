//! The rollback game loop: local input sampling, stepping the simulation inside `GgrsSchedule`,
//! and keeping the previous/current states around for interpolated rendering.

use crate::net::{Config, LocalHandle, MatchSeed};
use crate::{AppState, GameEntity};
use bevy::input::InputSystems;
use bevy::input::mouse::MouseMotion;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow, WindowFocused};
use bevy_ggrs::prelude::*;
use bevy_ggrs::{LocalInputs, LocalPlayers, RollbackFrameCount, RunGgrsSystems};
use endif_sim::math::{normalize_yaw, QAngle};
use crate::menu::UiScreen;
use crate::settings::{Action, Settings};
use endif_sim::{
    Arena, IN_ATTACK, IN_BACK, IN_DUCK, IN_FORWARD, IN_JUMP, IN_MOVELEFT, IN_MOVERIGHT, IN_WEAPON_ORIGINAL, IN_WEAPON_STOCK, MAX_PITCH, NUM_PLAYERS,
    PlayerInput, Rules, SimEvent, SimState, Weapon,
};

/// The simulation, registered for rollback + checksums.
#[derive(Resource, Clone, Hash)]
pub struct SimRes(pub SimState);

/// The static arena (not rolled back).
#[derive(Resource, Clone)]
pub struct ArenaRes(pub Arena);

/// Previous and current simulation states for interpolation, plus the wall-clock time the current
/// state was produced.
#[derive(Resource, Clone)]
pub struct RenderStates {
    pub prev: SimState,
    pub cur: SimState,
    pub last_advance: f64,
}

/// Live (un-delayed) look angles of the local player, in Source convention.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct LookAngles {
    pub pitch: f32,
    pub yaw: f32,
}

/// Simulation events for the presentation layer this frame.
///
/// A rollback re-steps frames that were already simulated once. Events from those frames were
/// mostly surfaced the first time around, but anything that only exists because of a late remote
/// input (their rocket firing, flying and exploding before the input arrived) has never been seen.
/// `played` remembers, per frame, which events were already handed out so that a re-simulated frame
/// surfaces only what is new for it. Events that were played in prediction and then didn't happen
/// cannot be taken back; that is the usual rollback trade-off.
#[derive(Resource, Default)]
pub struct PendingFx {
    pub events: Vec<SimEvent>,
    /// Sorted by frame. Bounded to the frames a rollback can still reach.
    played: std::collections::VecDeque<(i32, Vec<FxKey>)>,
}

/// Identity of an event for de-duplication across re-simulations. Rocket ids and positions are
/// deliberately left out: a late remote shot inserted before a local one shifts the local rocket's
/// id, and positions differ slightly between the predicted and the real frame, so matching on them
/// would replay the local player's own shot. A player fires at most once per tick, so kind + player
/// is a stable identity.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FxKey {
    RocketFired(u8),
    Explosion(Option<u8>),
    PlayerHit { victim: u8, attacker: u8 },
    Killed { victim: u8, attacker: u8 },
    Respawn(u8),
    RoundWon(u8),
    Landed(u8),
    Jumped(u8),
}

impl FxKey {
    fn of(ev: &SimEvent) -> Self {
        match *ev {
            SimEvent::RocketFired { shooter, .. } => FxKey::RocketFired(shooter),
            SimEvent::Explosion { hit_player, .. } => FxKey::Explosion(hit_player),
            SimEvent::PlayerHit { victim, attacker, .. } => FxKey::PlayerHit { victim, attacker },
            SimEvent::Killed { victim, attacker } => FxKey::Killed { victim, attacker },
            SimEvent::Respawn { player, .. } => FxKey::Respawn(player),
            SimEvent::RoundWon { winner, .. } => FxKey::RoundWon(winner),
            SimEvent::Landed { player, .. } => FxKey::Landed(player),
            SimEvent::Jumped { player } => FxKey::Jumped(player),
        }
    }
}

/// Frames of `played` history to keep. A rollback never reaches further back than the prediction
/// window; the margin covers the frame the rollback lands on and any off-by-one in GGRS's counting.
const FX_HISTORY: i32 = crate::net::MAX_PREDICTION as i32 + 4;

impl PendingFx {
    /// Queues the events of `frame` that haven't been surfaced by an earlier simulation of it.
    fn push_frame(&mut self, frame: i32, events: &[SimEvent]) {
        while self.played.front().is_some_and(|(f, _)| *f < frame - FX_HISTORY) {
            self.played.pop_front();
        }
        let idx = match self.played.binary_search_by_key(&frame, |(f, _)| *f) {
            Ok(i) => i,
            Err(i) => {
                self.played.insert(i, (frame, Vec::new()));
                i
            }
        };
        let record = &mut self.played[idx].1;
        // Each already-played key absorbs one matching event; the rest are new.
        let mut unmatched = record.clone();
        for ev in events {
            let key = FxKey::of(ev);
            if let Some(pos) = unmatched.iter().position(|k| *k == key) {
                unmatched.swap_remove(pos);
            } else {
                record.push(key);
                self.events.push(ev.clone());
            }
        }
    }
}

/// Whether the mouse is captured for looking. On the web this is reconciled with the browser's
/// actual pointer-lock state every frame (see `sync_browser_lock`).
#[derive(Resource, Default)]
pub struct MouseCaptured(pub bool);

/// When the last pointer-lock request was made. Browsers grant the lock asynchronously, so a fresh
/// request gets `LOCK_GRACE_SECS` before an unlocked pointer counts as "the browser released it".
#[derive(Resource, Default)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
struct LockRequestedAt(Option<f64>);

#[cfg(target_arch = "wasm32")]
const LOCK_GRACE_SECS: f64 = 1.5;

/// Drops single mouse events that are far larger than anything recent. Browsers now and then report
/// the pointer-lock re-centering warp as movement (Chromium, mostly when frames are dropped) and some
/// drivers deliver absolute positions, either of which shows up as one huge delta among ordinary
/// ones and whips the view around. A real flick ramps up over several samples, so an event several
/// times the recent peak that is also above a floor no single sample of hand motion reaches is a
/// spike, not input.
#[derive(Default)]
struct SpikeFilter {
    /// Largest recent per-event delta, decaying with `HALF_LIFE`.
    peak: f32,
    last: f64,
}

impl SpikeFilter {
    /// Counts in one event below which nothing is ever rejected.
    const FLOOR: f32 = 400.0;
    /// How many times the recent peak an event must exceed to be rejected.
    const RATIO: f32 = 4.0;
    const HALF_LIFE: f64 = 0.15;

    fn accept(&mut self, delta: Vec2, now: f64) -> bool {
        let mag = delta.length();
        self.peak *= 0.5f64.powf((now - self.last) / Self::HALF_LIFE) as f32;
        self.last = now;
        if mag > Self::FLOOR && mag > Self::RATIO * self.peak {
            debug!("dropped mouse spike of {mag:.0} counts (recent peak {:.0})", self.peak);
            return false;
        }
        self.peak = self.peak.max(mag);
        true
    }
}

pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(GgrsPlugin::<Config>::default())
            .insert_resource(RollbackFrameRate(crate::net::ROLLBACK_FPS))
            .rollback_resource_with_clone::<SimRes>()
            // Not `checksum_resource_with_hash`: that feeds `Hash` into SeaHash, which writes
            // `usize` lengths and enum discriminants in native width, so a desktop peer and a
            // browser peer never agree on the checksum of an identical state.
            .checksum_resource::<SimRes>(|sim| sim.0.checksum())
            .init_resource::<LookAngles>()
            .init_resource::<PendingFx>()
            .init_resource::<MouseCaptured>()
            .init_resource::<LockRequestedAt>()
            .insert_resource(ArenaRes(Arena::classic_square()))
            .add_systems(ReadInputs, read_local_inputs)
            .add_systems(GgrsSchedule, step_sim)
            .add_systems(OnEnter(AppState::InGame), (setup_match, || crate::webclip::set_in_match(true)))
            .add_systems(OnExit(AppState::InGame), (release_cursor, || crate::webclip::set_in_match(false)))
            .add_systems(
                PreUpdate,
                (capture_cursor, accumulate_look)
                    .chain()
                    .after(InputSystems)
                    .before(RunGgrsSystems)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(Last, clear_fx)
            .add_systems(Update, (dev_tools, log_long_frames));
        #[cfg(target_arch = "wasm32")]
        app.add_systems(
            PreUpdate,
            sync_browser_lock
                .after(InputSystems)
                .before(capture_cursor)
                .run_if(in_state(AppState::InGame)),
        )
        .add_systems(Update, sync_page_settings);
    }
}

/// Mirrors the fullscreen setting to the page whenever it changes (and once at startup): the
/// page reads it when the pointer is locked, see `web/index.html`.
#[cfg(target_arch = "wasm32")]
fn sync_page_settings(settings: Res<Settings>) {
    if settings.is_changed() {
        crate::webclip::set_fullscreen_on_play(settings.fullscreen);
    }
}

/// Browsers release pointer lock on their own (Esc, tab switch, alt-tab, a refused request) and
/// neither winit nor Bevy report it: the game would keep steering the view with the free cursor's
/// deltas and a click could not re-lock because the cursor options never change. So the captured
/// flag follows `document.pointerLockElement`, and losing the lock resets the cursor options so
/// the next request is applied again.
#[cfg(target_arch = "wasm32")]
fn sync_browser_lock(
    time: Res<Time<Real>>,
    mut captured: ResMut<MouseCaptured>,
    mut requested: ResMut<LockRequestedAt>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if !captured.0 {
        return;
    }
    let locked = web_sys::window().and_then(|w| w.document()).and_then(|d| d.pointer_lock_element()).is_some();
    if locked {
        requested.0 = None;
        return;
    }
    if let Some(t) = requested.0
        && time.elapsed_secs_f64() - t < LOCK_GRACE_SECS
    {
        return;
    }
    debug!("browser released pointer lock");
    requested.0 = None;
    captured.0 = false;
    if let Ok(mut cursor) = cursor.single_mut() {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

fn setup_match(
    mut commands: Commands,
    seed: Res<MatchSeed>,
    arena: Res<ArenaRes>,
    time: Res<Time<Real>>,
    kind: Option<Res<crate::net::MatchKind>>,
    pending: Option<ResMut<crate::net::PendingSession>>,
) {
    // Nobody dies in practice, so a launcher change would never arrive: apply it at once there.
    let practice = matches!(kind.as_deref(), Some(crate::net::MatchKind::Practice));
    let mut sim = SimState::new(seed.0, Rules { instant_weapon_switch: practice, ..Rules::default() });
    // Spawn everyone so the first rendered frame has players; the launchers are picked from the
    // first inputs GGRS delivers.
    sim.begin(&arena.0);
    // Face the local player toward the arena centre; the live look angles start from the spawn angles.
    commands.insert_resource(RenderStates { prev: sim.clone(), cur: sim.clone(), last_advance: time.elapsed_secs_f64() });
    commands.insert_resource(PendingFx::default());
    commands.insert_resource(SimRes(sim));
    commands.insert_resource(LookAngles::default());
    commands.insert_resource(MouseCaptured(false));
    // Hand the session to GGRS now that the rollback resources exist (commands apply in order).
    if let Some(mut pending) = pending
        && let Some(session) = pending.0.take()
    {
        commands.insert_resource(session);
        commands.remove_resource::<crate::net::PendingSession>();
    }
}

/// Locks the cursor on click (browsers require a user gesture for pointer lock).
fn capture_cursor(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    screen: Res<UiScreen>,
    time: Res<Time<Real>>,
    mut focus: MessageReader<WindowFocused>,
    mut captured: ResMut<MouseCaptured>,
    mut requested: ResMut<LockRequestedAt>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let Ok(mut cursor) = cursor.single_mut() else { return };
    let overlay = screen.blocks_game_input();
    // Losing the window (alt-tab, another app, a browser tab switch) drops the grab on every
    // platform, so the flag and the prompt follow it and the next click grabs again.
    let lost_focus = focus.read().any(|f| !f.focused);
    // Any overlay releases the mouse; closing it (Resume / Esc) grabs it again.
    if (overlay || lost_focus) && captured.0 {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
        captured.0 = false;
        requested.0 = None;
    }
    let resume = screen.is_changed() && !overlay;
    if !overlay && !captured.0 && !lost_focus && (mouse.just_pressed(MouseButton::Left) || resume) {
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
        captured.0 = true;
        requested.0 = Some(time.elapsed_secs_f64());
    }
    if captured.0 && keys.just_pressed(KeyCode::Tab) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
        captured.0 = false;
        requested.0 = None;
    }
}

fn release_cursor(mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>) {
    if let Ok(mut cursor) = cursor.single_mut() {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

fn accumulate_look(
    mut motion: MessageReader<MouseMotion>,
    captured: Res<MouseCaptured>,
    settings: Res<Settings>,
    time: Res<Time<Real>>,
    mut look: ResMut<LookAngles>,
    states: Option<Res<RenderStates>>,
    local: Res<LocalHandle>,
    mut last_spawn: Local<Option<(u32, QAngle)>>,
    mut was_captured: Local<bool>,
    mut filter: Local<SpikeFilter>,
) {
    // Every spawn (the first one included) points the view at the arena centre. The simulation
    // cannot hold the facing itself: the spawn tick's input overwrites it at once, so the live
    // look angles are re-seeded from `spawn_angles` whenever a new spawn shows up in the current
    // state. A rollback that moves the spawn elsewhere changes the angles too and re-seeds again.
    if let Some(s) = states.as_ref() {
        let p = &s.cur.players[local.0];
        let key = (p.spawn_tick, p.spawn_angles);
        if *last_spawn != Some(key) {
            look.pitch = p.spawn_angles.pitch;
            look.yaw = p.spawn_angles.yaw;
            *last_spawn = Some(key);
        }
    }
    // Motion from before the capture (moving the free cursor over to click) must not turn the view,
    // so the events are read individually and discarded while uncaptured and on the capture frame.
    let just_captured = captured.0 && !*was_captured;
    *was_captured = captured.0;
    if !captured.0 || just_captured {
        motion.clear();
        *filter = SpikeFilter::default();
        return;
    }
    let now = time.elapsed_secs_f64();
    let mut d = Vec2::ZERO;
    for ev in motion.read() {
        if filter.accept(ev.delta, now) {
            d += ev.delta;
        }
    }
    if d == Vec2::ZERO {
        return;
    }
    look.yaw = normalize_yaw(look.yaw - d.x * settings.yaw_per_count());
    look.pitch = (look.pitch + d.y * settings.pitch_per_count()).clamp(-MAX_PITCH, MAX_PITCH);
}

fn read_local_inputs(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    look: Res<LookAngles>,
    local_players: Res<LocalPlayers>,
    local: Res<LocalHandle>,
    captured: Res<MouseCaptured>,
    settings: Res<Settings>,
    screen: Res<UiScreen>,
) {
    let mut inputs = HashMap::default();
    for handle in &local_players.0 {
        let input = if *handle == local.0 {
            let mut buttons = 0u32;
            if captured.0 && !screen.blocks_game_input() {
                let table = [
                    (Action::Forward, IN_FORWARD),
                    (Action::Back, IN_BACK),
                    (Action::Left, IN_MOVELEFT),
                    (Action::Right, IN_MOVERIGHT),
                    (Action::Jump, IN_JUMP),
                    (Action::Crouch, IN_DUCK),
                    (Action::Fire, IN_ATTACK),
                ];
                for (action, bit) in table {
                    if settings.bindings.pressed(action, &keys, &mouse) {
                        buttons |= bit;
                    }
                }
            }
            // The launcher preference is not a key: it travels with every input, menu open or
            // not, and the simulation applies it at the next spawn. Always one of the two bits, so
            // the zeroed inputs GGRS pads the first frames with are told apart from "stock".
            buttons |= match settings.weapon {
                Weapon::Original => IN_WEAPON_ORIGINAL,
                Weapon::Stock => IN_WEAPON_STOCK,
            };
            PlayerInput { buttons, pitch: look.pitch, yaw: look.yaw }
        } else {
            // Second local player in practice mode: stands still.
            PlayerInput::default()
        };
        inputs.insert(*handle, input);
    }
    commands.insert_resource(LocalInputs::<Config>(inputs));
}

fn step_sim(
    mut sim: ResMut<SimRes>,
    arena: Res<ArenaRes>,
    inputs: Res<PlayerInputs<Config>>,
    frame: Res<RollbackFrameCount>,
    time: Res<Time<Real>>,
    mut states: ResMut<RenderStates>,
    mut fx: ResMut<PendingFx>,
) {
    let mut ins = [PlayerInput::default(); NUM_PLAYERS];
    for (i, slot) in ins.iter_mut().enumerate() {
        if let Some((input, _status)) = inputs.get(i) {
            *slot = *input;
        }
    }
    sim.0.step(&arena.0, ins);

    fx.push_frame(i32::from(*frame), &sim.0.events);

    states.prev = std::mem::replace(&mut states.cur, sim.0.clone());
    states.last_advance = time.elapsed_secs_f64();
}

/// Events are consumed by every presentation system during the frame, then cleared.
fn clear_fx(mut fx: ResMut<PendingFx>) {
    fx.events.clear();
}

/// Reports frames that took far longer than a tick, with how long the current state has been
/// active, so a hitch (shader compile, JIT tier-up, asset decode...) can be tied to what was
/// happening. Pauses longer than a few seconds are a hidden tab or a debugger, not a hitch.
fn log_long_frames(time: Res<Time<Real>>, state: Res<State<AppState>>, mut entered: Local<(AppState, f64)>) {
    let now = time.elapsed_secs_f64();
    if entered.0 != *state.get() {
        *entered = (*state.get(), now);
    }
    let dt = time.delta_secs_f64();
    if (0.08..5.0).contains(&dt) {
        debug!("long frame: {:.0} ms, {:.1} s into {:?}", dt * 1000.0, now - entered.1, state.get());
    }
}

/// Dev helpers: timed screenshot and auto-quit (see `--screenshot` / `--quit-after`).
pub fn dev_tools(
    mut commands: Commands,
    cfg: Res<crate::config::ClientConfig>,
    time: Res<Time<Real>>,
    state: Res<State<AppState>>,
    mut entered_at: Local<Option<f64>>,
    mut shot_taken: Local<bool>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = time.elapsed_secs_f64();
    if let Some(q) = cfg.quit_after
        && now > q
    {
        exit.write(AppExit::Success);
    }
    if *state.get() != AppState::InGame {
        *entered_at = None;
        return;
    }
    let t0 = *entered_at.get_or_insert(now);
    if !*shot_taken
        && now - t0 > 4.0
        && let Some(path) = cfg.screenshot.clone()
    {
        *shot_taken = true;
        use bevy::render::view::screenshot::{Screenshot, save_to_disk};
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        let _ = GameEntity;
    }
}
