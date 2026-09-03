//! Connection statistics for a match, and what is done with them: the signal bars the HUD
//! always shows, the adaptive input delay, the `--netstats` / `?netstats=true` overlay and a
//! summary line in the log when the match ends.
//!
//! GGRS reports the ping, the frame advantage and its send queue; the frame loop shows the rest:
//! rollbacks and how deep they went (the simulation frame counter going backwards) and stalls
//! (the peer's inputs ran more than the prediction window behind, so no frame could be simulated
//! for a while).

use crate::config::ClientConfig;
use crate::hud::NetStatus;
use crate::net::{Config, LocalHandle, MatchKind, MAX_PREDICTION, ROLLBACK_FPS};
use crate::settings::{INPUT_DELAY_DEFAULT, Settings, frames_to_ms};
use crate::theme::{self, Theme};
use crate::{AppState, GameEntity};
use bevy::prelude::*;
use bevy_ggrs::prelude::*;
use bevy_ggrs::{RollbackFrameCount, ggrs::NetworkStats};
use std::collections::VecDeque;

const TICK: f64 = 1.0 / ROLLBACK_FPS as f64;
const TICK_MS: f64 = 1000.0 / ROLLBACK_FPS as f64;
/// Samples and records older than this are forgotten; the signal bars look this far back.
const WINDOW: f64 = 10.0;
/// The window the overlay's "recent" figures use.
const RECENT: f64 = 3.0;
/// No new simulation frame for this long while the peer is connected counts as a stall.
const STALL_AFTER: f64 = 2.0 * TICK;
/// A rendered frame longer than this is our own hitch (a hidden tab, a shader compile): the
/// ticks it owes are caught up in the same frame, and the peer's packets may just not be in yet.
const HITCH: f64 = 0.25;
/// A rollback at least this deep is a visible correction (the opponent jumps).
const ROLLBACK_BAD: i32 = 6;

/// Bounds of the adaptive input delay, in frames.
const ADAPTIVE_MIN: u8 = 1;
const ADAPTIVE_MAX: u8 = 6;
/// Frames of rollback the adaptive delay is content to leave on the opponent's side. Delay beyond
/// what covers the rest of the one-way latency only costs responsiveness.
const ADAPTIVE_TOLERANCE: f64 = 3.0;
/// A new target must hold this long before the delay follows it: quickly up, slowly down.
const ADAPTIVE_RAISE_AFTER: f64 = 1.0;
const ADAPTIVE_LOWER_AFTER: f64 = 6.0;

#[derive(Resource, Default)]
pub struct NetStats {
    /// GGRS finished its handshake with the peer.
    pub synced: bool,
    pub ping_ms: u32,
    pub frames_ahead: i32,
    pub local_behind: i32,
    pub remote_behind: i32,
    pub send_queue: usize,
    /// Input delay applied to the session, in frames.
    pub input_delay: u8,
    pub rollbacks_total: u32,
    pub stalls_total: u32,
    pub stalled_ms: f64,
    /// `(time, ping)` samples, newest last.
    pings: VecDeque<(f64, u32)>,
    /// `(time, depth)` of every rollback, newest last.
    rollbacks: VecDeque<(f64, i32)>,
    /// When each stall began, newest last.
    stalls: VecDeque<f64>,
    last_frame: i32,
    max_frame: i32,
    last_advance_at: f64,
    stall_started: Option<f64>,
    started_at: f64,
    /// The adaptive controller's current target and since when it has held.
    target: (u8, f64),
}

impl NetStats {
    /// Forgets the previous match. The input delay is kept: `poll_room` sets it just before.
    fn reset(&mut self, now: f64) {
        let delay = self.input_delay;
        *self = NetStats { input_delay: delay, last_frame: -1, max_frame: -1, last_advance_at: now, started_at: now, target: (delay, now), ..default() };
    }

    fn prune(&mut self, now: f64) {
        while self.pings.front().is_some_and(|(t, _)| now - t > WINDOW) {
            self.pings.pop_front();
        }
        while self.rollbacks.front().is_some_and(|(t, _)| now - t > WINDOW) {
            self.rollbacks.pop_front();
        }
        while self.stalls.front().is_some_and(|t| now - t > WINDOW) {
            self.stalls.pop_front();
        }
    }

    /// Takes what GGRS reports. The ping only changes with each quality report (every 200 ms),
    /// so a sample is kept when the value moved or enough time passed.
    pub fn record_stats(&mut self, stats: &NetworkStats, frames_ahead: i32, now: f64) {
        self.ping_ms = stats.ping as u32;
        self.frames_ahead = frames_ahead;
        self.local_behind = stats.local_frames_behind;
        self.remote_behind = stats.remote_frames_behind;
        self.send_queue = stats.send_queue_len;
        let keep = match self.pings.back() {
            Some((t, p)) => *p != self.ping_ms || now - t >= 0.2,
            None => true,
        };
        if keep {
            self.pings.push_back((now, self.ping_ms));
        }
        self.prune(now);
    }

    /// Called for every simulated frame (first time and re-simulations alike): the frame counter
    /// going back means a rollback, going past its previous maximum means progress.
    pub fn record_step(&mut self, frame: i32, now: f64) {
        if frame <= self.last_frame {
            let depth = self.last_frame - frame + 1;
            self.rollbacks.push_back((now, depth));
            self.rollbacks_total += 1;
        }
        if frame > self.max_frame {
            self.max_frame = frame;
            self.last_advance_at = now;
            if let Some(t0) = self.stall_started.take() {
                self.stalled_ms += (now - t0) * 1000.0;
            }
        }
        self.last_frame = frame;
    }

    /// Once per rendered frame: notices a stall. GGRS catches up every tick a frame owes, so a
    /// frame that ends without the simulation having moved for two ticks was waiting on the peer.
    pub fn tick(&mut self, now: f64, delta: f64) {
        if self.synced && self.stall_started.is_none() && delta < HITCH && now - self.last_advance_at > STALL_AFTER {
            let t0 = self.last_advance_at + TICK;
            self.stall_started = Some(t0);
            self.stalls.push_back(t0);
            self.stalls_total += 1;
        }
        self.prune(now);
    }

    /// Mean change between successive ping samples in the window (RFC 3550 style), in ms.
    pub fn jitter_ms(&self) -> f64 {
        if self.pings.len() < 2 {
            return 0.0;
        }
        let sum: f64 = self.pings.iter().zip(self.pings.iter().skip(1)).map(|((_, a), (_, b))| (*a as f64 - *b as f64).abs()).sum();
        sum / (self.pings.len() - 1) as f64
    }

    fn ping_range(&self) -> (u32, u32) {
        let lo = self.pings.iter().map(|(_, p)| *p).min().unwrap_or(self.ping_ms);
        let hi = self.pings.iter().map(|(_, p)| *p).max().unwrap_or(self.ping_ms);
        (lo, hi)
    }

    fn rollbacks_in(&self, now: f64, secs: f64) -> usize {
        self.rollbacks.iter().filter(|(t, _)| now - t <= secs).count()
    }

    fn deepest_in(&self, now: f64, secs: f64) -> i32 {
        self.rollbacks.iter().filter(|(t, _)| now - t <= secs).map(|(_, d)| *d).max().unwrap_or(0)
    }

    fn stalls_in(&self, now: f64, secs: f64) -> usize {
        self.stalls.iter().filter(|t| now - *t <= secs).count()
    }

    /// Signal bars, 0 (no connection) to 4 (nothing to complain about).
    pub fn quality(&self, now: f64) -> u8 {
        if !self.synced {
            return 0;
        }
        let mut q: i32 = 4;
        if self.ping_ms >= 80 {
            q -= 1;
        }
        if self.ping_ms >= 160 {
            q -= 1;
        }
        if self.jitter_ms() >= 15.0 {
            q -= 1;
        }
        if self.stalls_in(now, WINDOW) > 0 {
            q -= 1;
        }
        if self.deepest_in(now, WINDOW) >= ROLLBACK_BAD {
            q -= 1;
        }
        q.clamp(1, 4) as u8
    }

    /// The input delay the adaptive mode wants right now. Input delay is per player and costs
    /// only its owner responsiveness; what it buys is on the opponent's screen: their rollbacks
    /// are as deep as the one-way latency of our inputs minus our delay. So the target covers
    /// that latency (with a jitter margin) down to a tolerated remainder, plus a frame while
    /// stalls are seen, which means the link is worse than the ping says. Both peers run the
    /// same rule, so a bad link raises both delays.
    fn adaptive_target(&mut self, now: f64) -> u8 {
        let one_way_ms = self.ping_ms as f64 / 2.0 + 2.0 * self.jitter_ms();
        let mut want = (one_way_ms / TICK_MS).ceil() - ADAPTIVE_TOLERANCE;
        if self.stalls_in(now, RECENT) > 0 {
            want += 1.0;
        }
        let want = want.clamp(ADAPTIVE_MIN as f64, ADAPTIVE_MAX as f64) as u8;
        if want != self.target.0 {
            self.target = (want, now);
        }
        let held = now - self.target.1;
        let current = self.input_delay;
        if want > current && held >= ADAPTIVE_RAISE_AFTER {
            current + 1
        } else if want < current && held >= ADAPTIVE_LOWER_AFTER {
            current - 1
        } else {
            current
        }
    }

    fn overlay_text(&self, now: f64, adaptive: bool) -> String {
        let (lo, hi) = self.ping_range();
        let secs = (now - self.started_at).max(1.0);
        let mut s = format!(
            "ping {} ms   jitter {:.0} ms   range {lo}-{hi}\nahead {:+}   behind me {} / them {}   queue {}\nrollbacks {:.1}/s   deepest {} ({RECENT:.0} s) / {} ({WINDOW:.0} s)   total {}\nstalls {} ({WINDOW:.0} s) / {} total   stalled {:.0} ms\ndelay {} frames ({} ms, {})   predict {MAX_PREDICTION}   signal {}/4   {:.0} s",
            self.ping_ms,
            self.jitter_ms(),
            self.frames_ahead,
            self.local_behind,
            self.remote_behind,
            self.send_queue,
            self.rollbacks_in(now, WINDOW) as f64 / WINDOW.min(secs),
            self.deepest_in(now, RECENT),
            self.deepest_in(now, WINDOW),
            self.rollbacks_total,
            self.stalls_in(now, WINDOW),
            self.stalls_total,
            self.stalled_ms,
            self.input_delay,
            frames_to_ms(self.input_delay),
            if adaptive { "adaptive" } else { "manual" },
            self.quality(now),
            secs,
        );
        if let Some(sim) = netsim_line() {
            s.push('\n');
            s.push_str(&sim);
        }
        s
    }

    /// One line for the log at the end of a match.
    pub fn summary(&self, now: f64) -> String {
        let (lo, hi) = self.ping_range();
        format!(
            "{:.0} s, ping {} ms (last window {lo}-{hi}, jitter {:.0} ms), {} rollbacks (deepest {} in the last {WINDOW:.0} s), {} stalls totalling {:.0} ms, input delay {} frames, prediction {MAX_PREDICTION}",
            now - self.started_at,
            self.ping_ms,
            self.jitter_ms(),
            self.rollbacks_total,
            self.deepest_in(now, WINDOW),
            self.stalls_total,
            self.stalled_ms,
            self.input_delay,
        )
    }
}

/// The impairment in force, for the overlay (dev builds only).
fn netsim_line() -> Option<String> {
    #[cfg(feature = "netsim")]
    {
        crate::netsim::ACTIVE.lock().ok()?.map(|sim| format!("NETSIM {}", sim.describe()))
    }
    #[cfg(not(feature = "netsim"))]
    None
}

#[derive(Component)]
struct OverlayText;

pub struct NetStatsPlugin;

impl Plugin for NetStatsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetStats>()
            .add_systems(GgrsSchedule, record_step)
            .add_systems(OnEnter(AppState::InGame), (reset, spawn_overlay))
            .add_systems(Update, (watch_frames, apply_input_delay, update_overlay).chain().run_if(in_state(AppState::InGame)))
            .add_systems(OnExit(AppState::InGame), log_summary);
    }
}

fn reset(mut stats: ResMut<NetStats>, time: Res<Time<Real>>) {
    stats.reset(time.elapsed_secs_f64());
}

fn record_step(frame: Res<RollbackFrameCount>, time: Res<Time<Real>>, mut stats: ResMut<NetStats>) {
    stats.record_step(i32::from(*frame), time.elapsed_secs_f64());
}

fn watch_frames(time: Res<Time<Real>>, mut stats: ResMut<NetStats>) {
    stats.tick(time.elapsed_secs_f64(), time.delta_secs_f64());
}

/// Keeps the session's input delay at the setting (manual) or the controller's choice
/// (adaptive). GGRS fills the gap when the delay grows and drops the frames in it when it
/// shrinks, either way the peer sees consecutive frames.
fn apply_input_delay(
    settings: Res<Settings>,
    local: Res<LocalHandle>,
    time: Res<Time<Real>>,
    mut stats: ResMut<NetStats>,
    session: Option<ResMut<Session<Config>>>,
) {
    let Some(mut session) = session else { return };
    let Session::P2P(s) = session.as_mut() else { return };
    if !stats.synced {
        return;
    }
    let now = time.elapsed_secs_f64();
    let want = if settings.adaptive_delay { stats.adaptive_target(now) } else { settings.input_delay };
    if want == stats.input_delay {
        return;
    }
    match s.set_input_delay(local.0, want as usize) {
        Ok(()) => {
            info!("input delay {} -> {want} frames ({})", stats.input_delay, if settings.adaptive_delay { "adaptive" } else { "manual" });
            stats.input_delay = want;
            stats.target = (want, now);
        }
        Err(e) => warn!("could not set input delay: {e}"),
    }
}

/// The overlay, under the status panel at the top left, for online matches when asked for.
fn spawn_overlay(mut commands: Commands, theme: Res<Theme>, cfg: Res<ClientConfig>, kind: Option<Res<MatchKind>>) {
    if !cfg.netstats || matches!(kind.as_deref(), None | Some(MatchKind::Practice | MatchKind::FruitNinja)) {
        return;
    }
    commands
        .spawn((
            GameEntity,
            theme::panel(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(74.0),
                left: Val::Px(12.0),
                padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                ..default()
            }),
        ))
        .with_children(|p| {
            p.spawn((OverlayText, theme.label("", 12.0, theme::OFF_WHITE)));
        });
}

fn update_overlay(stats: Res<NetStats>, settings: Res<Settings>, time: Res<Time<Real>>, mut q: Query<&mut Text, With<OverlayText>>) {
    let Ok(mut text) = q.single_mut() else { return };
    text.0 = stats.overlay_text(time.elapsed_secs_f64(), settings.adaptive_delay);
}

fn log_summary(stats: Res<NetStats>, time: Res<Time<Real>>) {
    if stats.synced {
        info!("match network summary: {}", stats.summary(time.elapsed_secs_f64()));
    }
}

/// The delay a new session starts with.
pub fn initial_input_delay(settings: &Settings) -> u8 {
    if settings.adaptive_delay { INPUT_DELAY_DEFAULT } else { settings.input_delay }
}

/// Signal bars for the HUD: 0 while GGRS is still synchronizing or the peer has gone quiet.
pub fn signal_level(stats: &NetStats, status: &NetStatus, now: f64) -> u8 {
    if status.text == "connection interrupted" {
        return 0;
    }
    stats.quality(now)
}
