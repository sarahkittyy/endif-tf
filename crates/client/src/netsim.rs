//! Dev-only connection impairment, behind the `netsim` cargo feature so it can never ship:
//! `--netsim delay=80,jitter=20,loss=2,spike=300,every=5` (desktop) or `?netsim=delay%3D80%2C...`
//! (web; `=` may be written `:` and `,` may be `;` to keep the address readable).
//!
//! It sits above the WebRTC channel in `net::GgrsChannel`, so it behaves the same in a browser
//! and on desktop and hits exactly the GGRS traffic. Each direction of this client is impaired
//! independently: `delay` is added one way, so the round trip grows by twice that when one client
//! runs it (and by four times when both do). Chrome's network throttling does not touch WebRTC,
//! which is why this exists.
//!
//! - `delay`: ms added to every packet.
//! - `jitter`: ms; each packet gets a uniform offset in `-jitter..=jitter` on top of `delay`
//!   (packets may overtake one another, as on a real unordered link).
//! - `loss`: percent of packets dropped.
//! - `spike` and `every`: every `every` seconds, everything sent in the next `spike` ms is held
//!   until that window ends: the Wi-Fi channel-scan pattern that freezes a game every few seconds.
//! - `seed`: for the random parts, so a run can be repeated.

use matchbox_socket::PeerId;
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::sync::Mutex;
use web_time::Instant;

/// The impairment in force, for the `--netstats` overlay.
pub static ACTIVE: Mutex<Option<NetSim>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetSim {
    pub delay_ms: f32,
    pub jitter_ms: f32,
    /// 0..=1.
    pub loss: f32,
    pub spike_ms: f32,
    pub every_s: f32,
    pub seed: u64,
}

impl NetSim {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.replace("%3D", "=").replace("%3A", ":").replace("%2C", ",").replace("%3B", ";");
        let mut sim = NetSim { delay_ms: 0.0, jitter_ms: 0.0, loss: 0.0, spike_ms: 0.0, every_s: 0.0, seed: 1 };
        let mut any = false;
        for kv in text.split([',', ';', ' ']).filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once(['=', ':'])?;
            let v: f32 = v.trim().parse().ok()?;
            match k.trim() {
                "delay" => sim.delay_ms = v.max(0.0),
                "jitter" => sim.jitter_ms = v.max(0.0),
                "loss" => sim.loss = (v / 100.0).clamp(0.0, 1.0),
                "spike" => sim.spike_ms = v.max(0.0),
                "every" => sim.every_s = v.max(0.0),
                "seed" => sim.seed = v as u64,
                other => {
                    log::warn!("netsim: unknown key {other:?}");
                    return None;
                }
            }
            any = true;
        }
        any.then_some(sim)
    }

    pub fn describe(&self) -> String {
        let mut s = format!("delay {:.0} ms, jitter {:.0} ms, loss {:.1}%", self.delay_ms, self.jitter_ms, self.loss * 100.0);
        if self.spike_ms > 0.0 && self.every_s > 0.0 {
            s.push_str(&format!(", {:.0} ms spike every {:.1} s", self.spike_ms, self.every_s));
        }
        s
    }
}

/// One direction of impaired traffic: packets go in, and come out later (or not at all).
pub struct Pipe {
    sim: NetSim,
    rng: StdRng,
    start: Instant,
    /// `(due, peer, packet)`, unordered.
    held: Vec<(f32, PeerId, Box<[u8]>)>,
}

impl Pipe {
    fn new(sim: NetSim, seed: u64, start: Instant) -> Self {
        Pipe { sim, rng: StdRng::seed_from_u64(seed), start, held: Vec::new() }
    }

    fn now(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    pub fn push(&mut self, peer: PeerId, packet: Box<[u8]>) {
        if self.sim.loss > 0.0 && self.rng.gen_range(0.0..1.0) < self.sim.loss {
            return;
        }
        let now = self.now();
        let jitter = if self.sim.jitter_ms > 0.0 { self.rng.gen_range(-self.sim.jitter_ms..=self.sim.jitter_ms) } else { 0.0 };
        let mut due = now + (self.sim.delay_ms + jitter).max(0.0) / 1000.0;
        if self.sim.spike_ms > 0.0 && self.sim.every_s > 0.0 {
            let phase = now % self.sim.every_s;
            let spike = self.sim.spike_ms / 1000.0;
            if phase < spike {
                due = due.max(now - phase + spike);
            }
        }
        self.held.push((due, peer, packet));
    }

    /// Everything whose time has come.
    pub fn drain_due(&mut self) -> Vec<(PeerId, Box<[u8]>)> {
        let now = self.now();
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].0 <= now {
                let (_, peer, packet) = self.held.swap_remove(i);
                out.push((peer, packet));
            } else {
                i += 1;
            }
        }
        out
    }
}

/// Both directions of one client's GGRS traffic.
pub struct Impaired {
    pub out: Pipe,
    pub inp: Pipe,
}

impl Impaired {
    pub fn new(sim: NetSim) -> Self {
        log::warn!("NETSIM ACTIVE: {}", sim.describe());
        *ACTIVE.lock().unwrap() = Some(sim);
        let start = Instant::now();
        Impaired { out: Pipe::new(sim, sim.seed, start), inp: Pipe::new(sim, sim.seed.wrapping_add(1), start) }
    }
}
