//! Two GGRS peers running the real simulation in one process over an in-memory link with
//! simulated latency, jitter, loss and periodic spikes, on a virtual clock, so the netcode's
//! parameters (input delay, prediction window) can be compared under set conditions in seconds,
//! headless and repeatably.
//!
//! GGRS keeps a little wall-clock state of its own (ping estimates, keep-alives, the handshake's
//! retry timer), so the ping it reports here is meaningless and its time-sync is a touch less
//! accurate than in a real match; stalls and rollbacks come from actual packet arrival and are
//! what this measures. `frames_ahead` handling mirrors `bevy_ggrs`: a peer that is ahead runs its
//! frames 10% slower.
//!
//! One wall-clock piece matters: a peer only sends inputs when it advances a frame, and until the
//! first acknowledgement has made the round trip a receiver cannot decode (and silently drops)
//! any input packet but the first, so on a slow link both peers can reach the prediction limit
//! with nothing decodable in flight. GGRS resolves that with a 200 ms wall-clock resend. The loop
//! below sleeps that real 200 ms whenever both peers are stalled, so such a stall costs here what
//! it costs in a match.
//!
//! The quick check runs on `cargo test`; the sweep prints a table and is run by hand:
//!
//! ```text
//! cargo test -p endif-sim --release --test netsweep -- --ignored --nocapture
//! NETSWEEP_FRAMES=8000 cargo test -p endif-sim --release --test netsweep -- --ignored --nocapture
//! ```

use endif_sim::{Arena, IN_ATTACK, IN_BACK, IN_DUCK, IN_FORWARD, IN_JUMP, IN_MOVELEFT, IN_MOVERIGHT, PlayerInput, Rules, SimState};
use ggrs::{Config, GgrsError, GgrsEvent, GgrsRequest, Message, NonBlockingSocket, P2PSession, PlayerType, SessionBuilder, SessionState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FPS: usize = 67;
const TICK: f64 = 1.0 / FPS as f64;

struct Cfg;

impl Config for Cfg {
    type Input = PlayerInput;
    type State = SimState;
    type Address = usize;
    type InputPredictor = ggrs::PredictRepeatLast;
}

/// xorshift64*, enough for scripted inputs and packet loss.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.unit() * n as f64) as u32
    }
}

#[derive(Clone, Copy)]
struct Cond {
    name: &'static str,
    /// One-way delay, ms.
    delay_ms: f64,
    /// Uniform +-, ms.
    jitter_ms: f64,
    /// 0..=1 per packet.
    loss: f64,
    /// Every `every_s`, packets sent in the next `spike_ms` are held until it ends.
    spike_ms: f64,
    every_s: f64,
}

const CONDITIONS: &[Cond] = &[
    Cond { name: "lan", delay_ms: 3.0, jitter_ms: 1.0, loss: 0.0, spike_ms: 0.0, every_s: 0.0 },
    Cond { name: "good", delay_ms: 25.0, jitter_ms: 3.0, loss: 0.0, spike_ms: 0.0, every_s: 0.0 },
    Cond { name: "ok", delay_ms: 50.0, jitter_ms: 8.0, loss: 0.5, spike_ms: 0.0, every_s: 0.0 },
    Cond { name: "far", delay_ms: 100.0, jitter_ms: 10.0, loss: 0.5, spike_ms: 0.0, every_s: 0.0 },
    Cond { name: "wifi", delay_ms: 30.0, jitter_ms: 40.0, loss: 2.0, spike_ms: 250.0, every_s: 4.0 },
    Cond { name: "lossy", delay_ms: 50.0, jitter_ms: 10.0, loss: 5.0, spike_ms: 0.0, every_s: 0.0 },
    Cond { name: "awful", delay_ms: 150.0, jitter_ms: 60.0, loss: 3.0, spike_ms: 400.0, every_s: 5.0 },
];

/// The link between the two peers: a virtual clock and one queue of `(due, message)` per peer.
struct Net {
    now: f64,
    cond: Cond,
    /// Loss and spikes are off during the handshake: its retries use a wall clock.
    impair: bool,
    rng: Rng,
    queues: [Vec<(f64, Message)>; 2],
}

impl Net {
    fn delay(&mut self) -> f64 {
        let c = self.cond;
        let jitter = if c.jitter_ms > 0.0 { (self.rng.unit() * 2.0 - 1.0) * c.jitter_ms } else { 0.0 };
        let mut due = self.now + (c.delay_ms + jitter).max(0.0) / 1000.0;
        if self.impair && c.spike_ms > 0.0 && c.every_s > 0.0 {
            let phase = self.now % c.every_s;
            let spike = c.spike_ms / 1000.0;
            if phase < spike {
                due = due.max(self.now - phase + spike);
            }
        }
        due
    }
}

struct Sock {
    id: usize,
    net: Arc<Mutex<Net>>,
}

impl NonBlockingSocket<usize> for Sock {
    fn send_to(&mut self, msg: &Message, addr: &usize) {
        let mut net = self.net.lock().unwrap();
        if net.impair && net.cond.loss > 0.0 && net.rng.unit() < net.cond.loss / 100.0 {
            return;
        }
        let due = net.delay();
        net.queues[*addr].push((due, msg.clone()));
    }

    fn receive_all_messages(&mut self) -> Vec<(usize, Message)> {
        let mut net = self.net.lock().unwrap();
        let now = net.now;
        let from = 1 - self.id;
        let queue = &mut net.queues[self.id];
        let mut out = Vec::new();
        let mut i = 0;
        while i < queue.len() {
            if queue[i].0 <= now {
                out.push((from, queue.swap_remove(i).1));
            } else {
                i += 1;
            }
        }
        out
    }
}

/// Scripted player: holds a random movement for a while, drifts the view, fires now and then.
struct Script {
    rng: Rng,
    input: PlayerInput,
    hold: u32,
}

impl Script {
    fn next(&mut self) -> PlayerInput {
        if self.hold == 0 {
            self.hold = 5 + self.rng.below(25);
            let mut b = 0;
            match self.rng.below(4) {
                0 => b |= IN_FORWARD,
                1 => b |= IN_BACK,
                _ => {}
            }
            match self.rng.below(3) {
                0 => b |= IN_MOVELEFT,
                1 => b |= IN_MOVERIGHT,
                _ => {}
            }
            if self.rng.below(10) < 3 {
                b |= IN_JUMP;
            }
            if self.rng.below(10) < 1 {
                b |= IN_DUCK;
            }
            self.input.buttons = b;
        }
        self.hold -= 1;
        if self.rng.below(20) == 0 {
            self.input.buttons ^= IN_ATTACK;
        }
        self.input.yaw += (self.rng.unit() as f32 - 0.5) * 6.0;
        if self.input.yaw > 180.0 {
            self.input.yaw -= 360.0;
        } else if self.input.yaw <= -180.0 {
            self.input.yaw += 360.0;
        }
        self.input.pitch = (self.input.pitch + (self.rng.unit() as f32 - 0.5) * 4.0).clamp(-80.0, 80.0);
        self.input
    }
}

/// GGRS's `RUNNING_RETRY_INTERVAL`, after which a peer resends its pending inputs.
const GGRS_RESEND: f64 = 0.2;

struct Peer {
    sess: P2PSession<Cfg>,
    sim: SimState,
    /// Frame of `sim`.
    cur: i32,
    /// Virtual time of the next frame.
    next_at: f64,
    script: Script,
    frames: u32,
    stalls: u32,
    rollbacks: u32,
    depth_sum: u64,
    depth_max: i32,
    /// Checksum of the state at the check frame, last write wins (a re-simulation overwrites).
    check: Option<u64>,
    /// GGRS events that must not happen: disconnects, desyncs.
    disconnects: u32,
    desyncs: u32,
    interrupted: u32,
    /// The last attempt could not advance.
    stalled: bool,
}

#[derive(Default, Clone, Copy)]
struct Outcome {
    stalls: u32,
    rollbacks: u32,
    depth_mean: f64,
    depth_max: i32,
    consistent: bool,
    disconnects: u32,
    desyncs: u32,
    interrupted: u32,
    /// Lowest simulation frame either peer got to (below the check frame: it stalled for good).
    reached: i32,
}

fn run(cond: Cond, input_delay: usize, prediction: usize, frames: u32, seed: u64) -> Outcome {
    let trace = std::env::var_os("NETSWEEP_TRACE").is_some();
    let arena = Arena::classic_square();
    let net = Arc::new(Mutex::new(Net { now: 0.0, cond, impair: false, rng: Rng(seed | 1), queues: [Vec::new(), Vec::new()] }));
    let mut peers: Vec<Peer> = (0..2)
        .map(|id| {
            let mut b = SessionBuilder::<Cfg>::new()
                .with_num_players(2)
                .unwrap()
                .with_fps(FPS)
                .unwrap()
                .with_input_delay(input_delay)
                .with_max_prediction_window(prediction)
                .with_disconnect_timeout(Duration::from_secs(3600))
                .with_disconnect_notify_delay(Duration::from_secs(3600));
            for h in 0..2 {
                b = b.add_player(if h == id { PlayerType::Local } else { PlayerType::Remote(h) }, h).unwrap();
            }
            let sess = b.start_p2p_session(Sock { id, net: net.clone() }).unwrap();
            let mut sim = SimState::new(0xE11D1F, Rules::default());
            sim.step(&arena, [PlayerInput::default(); 2]);
            Peer {
                sess,
                sim,
                cur: 0,
                next_at: id as f64 * TICK * 0.5,
                script: Script { rng: Rng((seed + 7 * (id as u64 + 1)) | 1), input: PlayerInput::default(), hold: 0 },
                frames: 0,
                stalls: 0,
                rollbacks: 0,
                depth_sum: 0,
                depth_max: 0,
                check: None,
                disconnects: 0,
                desyncs: 0,
                interrupted: 0,
                stalled: false,
            }
        })
        .collect();

    // Handshake on a clean link.
    let mut guard = 0;
    while peers.iter().any(|p| p.sess.current_state() != SessionState::Running) {
        net.lock().unwrap().now += 0.001;
        for p in &mut peers {
            p.sess.poll_remote_clients();
        }
        guard += 1;
        assert!(guard < 200_000, "handshake never finished");
    }
    {
        let mut n = net.lock().unwrap();
        n.impair = true;
        let now = n.now;
        for p in &mut peers {
            p.next_at += now;
        }
    }

    let check_frame = frames as i32;
    // A tail on a clean link so both peers get to, and confirm, `check_frame`.
    let tail = check_frame + 60;
    let mut last_sleep = f64::MIN;
    let mut attempts = 0u32;
    while peers.iter().any(|p| p.frames < frames || p.cur < tail) {
        attempts += 1;
        assert!(attempts < frames * 4, "peers never reached frame {tail}");
        let i = if peers[0].next_at <= peers[1].next_at { 0 } else { 1 };
        {
            let mut n = net.lock().unwrap();
            n.now = n.now.max(peers[i].next_at);
            if peers.iter().all(|p| p.frames >= frames) && n.impair {
                n.impair = false;
                n.cond = CONDITIONS[0];
            }
            if peers.iter().all(|p| p.stalled) && n.now - last_sleep >= GGRS_RESEND {
                std::thread::sleep(Duration::from_secs_f64(GGRS_RESEND + 0.01));
                last_sleep = n.now;
            }
        }
        let p = &mut peers[i];
        for ev in p.sess.events() {
            match ev {
                GgrsEvent::Disconnected { .. } => p.disconnects += 1,
                GgrsEvent::DesyncDetected { .. } => p.desyncs += 1,
                GgrsEvent::NetworkInterrupted { .. } => p.interrupted += 1,
                _ => {}
            }
        }
        let input = p.script.next();
        p.sess.add_local_input(i, input).unwrap();
        match p.sess.advance_frame() {
            Ok(reqs) => {
                // At the prediction limit GGRS 0.13 answers without an `AdvanceFrame` request
                // (it no longer returns `PredictionThreshold`): that is a stall.
                p.stalled = !reqs.iter().any(|r| matches!(r, GgrsRequest::AdvanceFrame { .. }));
                if p.stalled {
                    p.stalls += 1;
                }
                for r in reqs {
                    match r {
                        GgrsRequest::SaveGameState { cell, frame } => cell.save(frame, Some(p.sim.clone()), Some(p.sim.checksum() as u128)),
                        GgrsRequest::LoadGameState { cell, frame } => {
                            let depth = p.cur - frame;
                            p.rollbacks += 1;
                            p.depth_sum += depth as u64;
                            p.depth_max = p.depth_max.max(depth);
                            p.sim = cell.load().expect("saved state");
                            p.cur = frame;
                        }
                        GgrsRequest::AdvanceFrame { inputs } => {
                            p.sim.step(&arena, [inputs[0].0, inputs[1].0]);
                            p.cur += 1;
                            if p.cur == check_frame {
                                p.check = Some(p.sim.checksum());
                            }
                        }
                    }
                }
            }
            Err(GgrsError::PredictionThreshold) => p.stalls += 1,
            Err(e) => panic!("ggrs: {e}"),
        }
        p.frames += 1;
        let period = if p.sess.frames_ahead() > 0 { TICK * 1.1 } else { TICK };
        p.next_at += period;
        if trace && p.frames % 500 == 0 {
            let now = net.lock().unwrap().now;
            println!(
                "  t={now:7.2}s peer {i}: attempts {} frame {} confirmed {} stalls {} rollbacks {} ahead {}",
                p.frames,
                p.cur,
                p.sess.confirmed_frame(),
                p.stalls,
                p.rollbacks,
                p.sess.frames_ahead()
            );
        }
    }

    let consistent = peers[0].check.is_some() && peers[0].check == peers[1].check;
    let reached = peers.iter().map(|p| p.cur).min().unwrap_or(0);
    let rollbacks: u32 = peers.iter().map(|p| p.rollbacks).sum();
    let depth_sum: u64 = peers.iter().map(|p| p.depth_sum).sum();
    Outcome {
        stalls: peers.iter().map(|p| p.stalls).sum(),
        rollbacks,
        depth_mean: if rollbacks > 0 { depth_sum as f64 / rollbacks as f64 } else { 0.0 },
        depth_max: peers.iter().map(|p| p.depth_max).max().unwrap_or(0),
        consistent,
        disconnects: peers.iter().map(|p| p.disconnects).sum(),
        desyncs: peers.iter().map(|p| p.desyncs).sum(),
        interrupted: peers.iter().map(|p| p.interrupted).sum(),
        reached,
    }
}

fn frames_from_env(default: u32) -> u32 {
    std::env::var("NETSWEEP_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Both peers end up with the same state, and a clean link needs no stalls.
#[test]
fn peers_agree_and_lan_never_stalls() {
    let out = run(CONDITIONS[1], 2, 16, 600, 42);
    assert!(out.consistent, "peers diverged");
    assert_eq!(out.stalls, 0, "stalls on a good link: {}", out.stalls);
}

/// One run with progress lines: `NETSWEEP_TRACE=1 NETSWEEP_PROBE=far,1,12 cargo test ... probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn probe() {
    let spec = std::env::var("NETSWEEP_PROBE").unwrap_or_else(|_| "far,1,12".to_string());
    let mut it = spec.split(',');
    let name = it.next().unwrap_or("far");
    let delay: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let pred: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(12);
    let cond = *CONDITIONS.iter().find(|c| c.name == name).expect("unknown condition");
    let out = run(cond, delay, pred, frames_from_env(4000), 42);
    println!("{name} delay {delay} pred {pred}: stalls {} rollbacks {} depth max {} reached {} consistent {}", out.stalls, out.rollbacks, out.depth_max, out.reached, out.consistent);
}

/// The table: every condition against a few input delays and prediction windows.
#[test]
#[ignore]
fn sweep() {
    let frames = frames_from_env(4000);
    let secs = frames as f64 * TICK;
    println!("\n{frames} frames ({secs:.0} s) per run, both peers counted; stalls are frames a peer could not simulate, depth in frames\n");
    println!("{:<7} {:>5} {:>4} | {:>7} {:>8} {:>9} {:>10} {:>9} {:>5} {:>4} {:>6} {:>6} {:>7}", "cond", "delay", "pred", "stalls", "stall/min", "rollbacks", "depth mean", "depth max", "ok", "disc", "desync", "interr", "reached");
    for cond in CONDITIONS {
        for &pred in &[12usize, 16] {
            for &delay in &[1usize, 2, 3, 4, 6] {
                let out = run(*cond, delay, pred, frames, 42);
                println!(
                    "{:<7} {:>5} {:>4} | {:>7} {:>8.1} {:>9} {:>10.1} {:>9} {:>5} {:>4} {:>6} {:>6} {:>7}",
                    cond.name,
                    delay,
                    pred,
                    out.stalls,
                    out.stalls as f64 / (secs / 60.0),
                    out.rollbacks,
                    out.depth_mean,
                    out.depth_max,
                    if out.consistent { "yes" } else { "NO" },
                    out.disconnects,
                    out.desyncs,
                    out.interrupted,
                    out.reached,
                );
            }
        }
        println!();
    }
}
