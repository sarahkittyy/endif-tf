//! Sounds: TF2 menu music, UI clicks, and the in-game sounds driven by simulation events
//! (rocket launcher, explosions, crit/airshot stings, announcer, soldier voice lines).
//!
//! All files come from the public TF2 sound archive (see `assets/sounds/` and the README).

use crate::game::{PendingFx, RenderStates};
use crate::loading::StartupDone;
use crate::menu::UiScreen;
use crate::net::{LocalHandle, RoomConnection};
use crate::settings::Settings;
use crate::AppState;
use bevy::audio::{AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, GlobalVolume, PlaybackSettings, Volume};
use bevy::prelude::*;
use endif_sim::SimEvent;
use endif_sim::Vec3 as SVec3;

type Clip = Handle<AudioSource>;

/// Menu music. gamestartup1 = TF2 theme, 3 = Rocket Jump Waltz, 4 = The Art of War,
/// 5 = Faster Than a Speeding Bullet, 6 = Right Behind You.
const MUSIC: [&str; 5] =
    ["music/gamestartup1", "music/gamestartup3", "music/gamestartup4", "music/gamestartup5", "music/gamestartup6"];

/// Every sound handle, loaded once at startup (except the music, see `menu_music`).
#[derive(Resource)]
pub struct Sfx {
    pub rocket_shoot: Clip,
    pub rocket_reload: Clip,
    pub explode: Vec<Clip>,
    pub crit_hit: Vec<Clip>,
    pub crit_received: Vec<Clip>,
    pub hitsound: Clip,
    pub killsound: Clip,
    pub freeze_cam: Clip,
    pub button_click: Clip,
    pub button_rollover: Clip,
    pub panel_open: Clip,
    pub panel_close: Clip,
    pub join: Clip,
    pub notification: Clip,
    pub announcer_victory: Clip,
    pub announcer_you_failed: Clip,
    /// "Begin!" - the one round-start line that does not mention carts or points.
    pub announcer_begin: Clip,
    pub announcer_flawless_victory: Vec<Clip>,
    pub announcer_flawless_defeat: Vec<Clip>,
    pub soldier_cheers: Vec<Clip>,
    pub soldier_jeers: Vec<Clip>,
    pub soldier_death: Vec<Clip>,
    pub soldier_pain: Vec<Clip>,
    pub soldier_battlecry: Vec<Clip>,
    /// One slot per `MUSIC` track, filled the first time the track is picked. The tracks are the
    /// biggest files in the game (1-2 MB each), so they are not fetched up front: on the web that
    /// made them compete with the models for the browser's few connections per host, and the
    /// loading screen sat on the last few assets while the music came down.
    pub music: Vec<Option<Clip>>,
}

impl Sfx {
    /// Every clip except the menu music, for the web loading screen to wait on (the music is
    /// fetched after the screen is gone).
    pub fn startup_clips(&self) -> Vec<Clip> {
        let mut clips = vec![
            self.rocket_shoot.clone(),
            self.rocket_reload.clone(),
            self.hitsound.clone(),
            self.killsound.clone(),
            self.freeze_cam.clone(),
            self.button_click.clone(),
            self.button_rollover.clone(),
            self.panel_open.clone(),
            self.panel_close.clone(),
            self.join.clone(),
            self.notification.clone(),
            self.announcer_victory.clone(),
            self.announcer_you_failed.clone(),
            self.announcer_begin.clone(),
        ];
        for set in [
            &self.explode,
            &self.crit_hit,
            &self.crit_received,
            &self.announcer_flawless_victory,
            &self.announcer_flawless_defeat,
            &self.soldier_cheers,
            &self.soldier_jeers,
            &self.soldier_death,
            &self.soldier_pain,
            &self.soldier_battlecry,
        ] {
            clips.extend(set.iter().cloned());
        }
        clips
    }
}

impl FromWorld for Sfx {
    fn from_world(world: &mut World) -> Self {
        let a = world.resource::<AssetServer>();
        let load = |p: &str| a.load::<AudioSource>(format!("sounds/{p}.ogg"));
        let set = |ps: &[&str]| ps.iter().map(|p| load(p)).collect::<Vec<_>>();
        Sfx {
            rocket_shoot: load("weapons/rocket_shoot"),
            rocket_reload: load("weapons/rocket_reload"),
            explode: set(&["weapons/explode1", "weapons/explode2", "weapons/explode3"]),
            crit_hit: set(&["player/crit_hit", "player/crit_hit2", "player/crit_hit3"]),
            crit_received: set(&["player/crit_received1", "player/crit_received2", "player/crit_received3"]),
            hitsound: load("ui/hitsound"),
            killsound: load("ui/killsound"),
            freeze_cam: load("misc/freeze_cam"),
            button_click: load("ui/buttonclick"),
            button_rollover: load("ui/buttonrollover"),
            panel_open: load("ui/panel_open"),
            panel_close: load("ui/panel_close"),
            join: load("ui/mm_join"),
            notification: load("ui/notification_alert"),
            announcer_victory: load("vo/announcer_victory"),
            announcer_you_failed: load("vo/announcer_you_failed"),
            announcer_begin: load("vo/announcer_am_roundstart03"),
            announcer_flawless_victory: set(&["vo/announcer_am_flawlessvictory01", "vo/announcer_am_flawlessvictory02"]),
            announcer_flawless_defeat: set(&["vo/announcer_am_flawlessdefeat01", "vo/announcer_am_flawlessdefeat02"]),
            soldier_cheers: set(&[
                "vo/soldier_cheers01",
                "vo/soldier_cheers02",
                "vo/soldier_cheers03",
                "vo/soldier_laughlong01",
                "vo/soldier_laughlong02",
            ]),
            soldier_jeers: set(&["vo/soldier_jeers01", "vo/soldier_jeers02", "vo/soldier_jeers03"]),
            soldier_death: set(&[
                "vo/soldier_paincrticialdeath01",
                "vo/soldier_paincrticialdeath02",
                "vo/soldier_paincrticialdeath03",
            ]),
            soldier_pain: set(&["vo/soldier_painsevere01", "vo/soldier_painsevere02", "vo/soldier_painsevere03"]),
            soldier_battlecry: set(&["vo/soldier_battlecry01", "vo/soldier_battlecry02", "vo/soldier_battlecry03"]),
            music: vec![None; MUSIC.len()],
        }
    }
}

/// Sounds scheduled for a later wall-clock time (announcer lines after a round, ...).
#[derive(Resource, Default)]
struct Delayed(Vec<(f64, Clip, f32)>);

#[derive(Component)]
struct Music;

pub struct AudioFxPlugin;

impl Plugin for AudioFxPlugin {
    fn build(&self, app: &mut App) {
        // Nothing plays until the web loading screen is gone (`StartupDone`, present from the start
        // on desktop). A room link or `?practice` in the URL joins or starts a match while the
        // page is still loading, and any sound started then (the join sting, the "Begin!" line,
        // rockets) crackles under the loading screen, because the main thread is busy compiling
        // shaders for the render warm-up and the browser's audio callback starves.
        let ready = resource_exists::<StartupDone>;
        app.init_resource::<Sfx>()
            .init_resource::<Delayed>()
            .add_systems(PreUpdate, apply_volume)
            .add_systems(OnEnter(AppState::InGame), (stop_music, match_start.run_if(ready)))
            .add_systems(OnEnter(AppState::Connecting), (|mut c: Commands, sfx: Res<Sfx>| play(&mut c, &sfx.join, 0.45)).run_if(ready))
            .add_systems(
                Update,
                (
                    menu_music.run_if(not(in_state(AppState::InGame))),
                    ui_sounds,
                    screen_sounds,
                    room_error_sound.run_if(in_state(AppState::Connecting)),
                    (game_sounds, reload_sound).run_if(in_state(AppState::InGame)),
                    flush_delayed,
                )
                    .run_if(ready),
            );
    }
}

/// Master volume from the settings. Bevy only folds `GlobalVolume` into a sink when it is created,
/// so sounds already playing (the menu music, a long announcer line) are updated here by hand.
fn apply_volume(
    settings: Res<Settings>,
    mut global: ResMut<GlobalVolume>,
    mut sinks: Query<(&PlaybackSettings, &mut AudioSink)>,
) {
    if !settings.is_changed() {
        return;
    }
    global.volume = Volume::Linear(settings.volume);
    for (playback, mut sink) in &mut sinks {
        sink.set_volume(playback.volume * global.volume);
    }
}

fn pick(clips: &[Clip]) -> &Clip {
    &clips[rand::random::<usize>() % clips.len()]
}

fn play(commands: &mut Commands, clip: &Clip, volume: f32) {
    commands.spawn((AudioPlayer::new(clip.clone()), PlaybackSettings::DESPAWN.with_volume(Volume::Linear(volume))));
}

/// Simple distance falloff for sounds made by the opponent: full volume within 200 units, then
/// linear down to a quarter at 2700 units.
fn attenuate(source: SVec3, listener: SVec3, base: f32) -> f32 {
    let d = source.dist_to(listener);
    base * (1.0 - (d - 200.0).max(0.0) / 2500.0).clamp(0.25, 1.0)
}

/// Keeps one menu track playing while in the menus; picks a different one each time it ends.
/// A track is only fetched the first time it is picked (and then kept), so at most one track is
/// downloading at a time and it starts as soon as it arrives. Like every sound this is not run
/// until the loading screen is gone, so the first track does not slow the startup assets down.
fn menu_music(
    mut commands: Commands,
    server: Res<AssetServer>,
    mut sfx: ResMut<Sfx>,
    playing: Query<(), With<Music>>,
    mut last: Local<Option<usize>>,
) {
    if !playing.is_empty() {
        return;
    }
    let mut i = rand::random::<usize>() % MUSIC.len();
    if MUSIC.len() > 1 && *last == Some(i) {
        i = (i + 1) % MUSIC.len();
    }
    *last = Some(i);
    let clip = sfx.music[i].get_or_insert_with(|| server.load::<AudioSource>(format!("sounds/{}.ogg", MUSIC[i]))).clone();
    commands.spawn((Music, AudioPlayer::new(clip), PlaybackSettings::DESPAWN.with_volume(Volume::Linear(0.35))));
}

fn stop_music(mut commands: Commands, q: Query<Entity, With<Music>>) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

fn match_start(mut commands: Commands, sfx: Res<Sfx>, time: Res<Time<Real>>, mut delayed: ResMut<Delayed>) {
    play(&mut commands, &sfx.announcer_begin, 0.85);
    delayed.0.push((time.elapsed_secs_f64() + 1.6, pick(&sfx.soldier_battlecry).clone(), 0.55));
}

fn ui_sounds(mut commands: Commands, sfx: Res<Sfx>, q: Query<&Interaction, (Changed<Interaction>, With<Button>)>) {
    for i in &q {
        match i {
            Interaction::Hovered => play(&mut commands, &sfx.button_rollover, 0.5),
            Interaction::Pressed => play(&mut commands, &sfx.button_click, 0.7),
            Interaction::None => {}
        }
    }
}

fn screen_sounds(mut commands: Commands, sfx: Res<Sfx>, screen: Res<UiScreen>, mut prev: Local<Option<UiScreen>>) {
    if !screen.is_changed() {
        return;
    }
    let before = prev.replace(*screen);
    match (before, *screen) {
        (Some(UiScreen::Hidden), UiScreen::Pause) => play(&mut commands, &sfx.panel_open, 0.6),
        (Some(UiScreen::Pause), UiScreen::Hidden) => play(&mut commands, &sfx.panel_close, 0.6),
        (Some(_), UiScreen::Settings { .. }) => play(&mut commands, &sfx.panel_open, 0.6),
        _ => {}
    }
}

fn room_error_sound(mut commands: Commands, sfx: Res<Sfx>, room: Option<Res<RoomConnection>>, mut played: Local<bool>) {
    // `Checking` is still the waiting panel; the box (and its sound) comes once the room answered.
    let has_error = room.is_some_and(|r| r.failure.as_ref().is_some_and(|f| !matches!(f, crate::net::RoomFailure::Checking)));
    if has_error && !*played {
        play(&mut commands, &sfx.notification, 0.6);
    }
    *played = has_error;
}

fn game_sounds(
    mut commands: Commands,
    sfx: Res<Sfx>,
    fx: Res<PendingFx>,
    local: Res<LocalHandle>,
    states: Option<Res<RenderStates>>,
    time: Res<Time<Real>>,
    mut delayed: ResMut<Delayed>,
) {
    let Some(states) = states else { return };
    let me = local.0 as u8;
    let my_pos = states.cur.players[local.0].origin;
    let now = time.elapsed_secs_f64();
    for ev in &fx.events {
        match ev {
            SimEvent::RocketFired { shooter, origin, .. } => {
                let v = if *shooter == me { 0.55 } else { attenuate(*origin, my_pos, 0.55) };
                play(&mut commands, &sfx.rocket_shoot, v);
            }
            SimEvent::Explosion { origin, .. } => {
                play(&mut commands, pick(&sfx.explode), attenuate(*origin, my_pos, 0.8));
            }
            SimEvent::PlayerHit { victim, attacker, airshot_kill, .. } => {
                if *attacker == me && *victim != me {
                    if *airshot_kill {
                        play(&mut commands, pick(&sfx.crit_hit), 0.9);
                        play(&mut commands, &sfx.killsound, 0.6);
                    } else {
                        play(&mut commands, &sfx.hitsound, 0.5);
                    }
                }
                if *victim == me && *attacker != me {
                    if *airshot_kill {
                        play(&mut commands, pick(&sfx.crit_received), 0.9);
                        play(&mut commands, &sfx.freeze_cam, 0.5);
                    } else {
                        play(&mut commands, pick(&sfx.soldier_pain), 0.5);
                    }
                }
            }
            SimEvent::Killed { victim, .. } => {
                let v = if *victim == me { 0.8 } else { 0.55 };
                play(&mut commands, pick(&sfx.soldier_death), v);
            }
            SimEvent::RoundWon { winner, score } => {
                let won = *winner == me;
                let flawless = score.iter().any(|s| *s == 0);
                if won {
                    play(&mut commands, &sfx.announcer_victory, 0.9);
                    delayed.0.push((now + 1.3, pick(&sfx.soldier_cheers).clone(), 0.7));
                } else {
                    play(&mut commands, &sfx.announcer_you_failed, 0.9);
                    delayed.0.push((now + 1.3, pick(&sfx.soldier_jeers).clone(), 0.6));
                }
                if flawless {
                    let line = if won { &sfx.announcer_flawless_victory } else { &sfx.announcer_flawless_defeat };
                    delayed.0.push((now + 3.0, pick(line).clone(), 0.9));
                }
                delayed.0.push((now + 5.5, sfx.announcer_begin.clone(), 0.85));
            }
            _ => {}
        }
    }
}

/// The launcher reloads a rocket 0.4 s after firing; play the reload click when the clip refills.
fn reload_sound(mut commands: Commands, sfx: Res<Sfx>, local: Res<LocalHandle>, states: Option<Res<RenderStates>>) {
    let Some(states) = states else { return };
    let (a, b) = (&states.prev.players[local.0], &states.cur.players[local.0]);
    if a.alive && b.alive && a.spawn_tick == b.spawn_tick && b.clip > a.clip {
        play(&mut commands, &sfx.rocket_reload, 0.6);
    }
}

fn flush_delayed(mut commands: Commands, time: Res<Time<Real>>, mut delayed: ResMut<Delayed>) {
    let now = time.elapsed_secs_f64();
    let mut i = 0;
    while i < delayed.0.len() {
        if delayed.0[i].0 <= now {
            let (_, clip, vol) = delayed.0.swap_remove(i);
            play(&mut commands, &clip, vol);
        } else {
            i += 1;
        }
    }
}
