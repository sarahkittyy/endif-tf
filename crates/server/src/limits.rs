//! Rate limiting: leaky buckets keyed by client address (a few by target), so one machine cannot
//! flood the API, burn the mail quota or brute-force a login.
//!
//! Every bucket holds `level` units of water that drain at `per_sec`; a request pours one unit in
//! and is refused when that would overflow `capacity`. So a burst of `capacity` requests passes at
//! once from an idle bucket, after which the sustained rate is `per_sec`. Refusals answer
//! `429 Too Many Requests` with a `Retry-After` header and the usual `{"error": ...}` JSON, worded
//! for the player (the client shows it as is) plus `retry_after` in seconds.
//!
//! Tiers, outermost first (a request has to fit every bucket on its way in):
//!
//! | group | routes | per | burst | sustained |
//! | --- | --- | --- | --- | --- |
//! | general | everything under `/api` | address | 60 | 10 / s |
//! | auth | login, verify, reset, password / username change, register (argon2 costs ~100 ms CPU) | address | 10 | 10 / min |
//! | mail | register, forgot, resend (each sends an e-mail) | address | 3 | 1 / 2 min |
//! | mail | register, forgot, resend | whole server | 20 | `ENDIF_MAIL_PER_DAY` / day |
//! | login target | login, keyed by the username or e-mail typed | account | 10 | 2 / min |
//! | ws | signaling websocket handshakes | address | 15 | 15 / min |
//!
//! Addresses are the peer's, or `X-Forwarded-For` / `X-Real-IP` when `ENDIF_TRUST_PROXY=true`
//! (only behind a reverse proxy that sets them, or clients could pick their own bucket). IPv6 is
//! keyed by /64 so a whole SLAAC prefix shares one bucket. `ENDIF_RATE_LIMITS=false` turns all of
//! this off for local testing.

use crate::api::AppState;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::warn;

/// Shape of one bucket.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Policy {
    /// Requests that pass at once from an idle bucket.
    pub capacity: f64,
    /// Sustained rate: units that leak out per second.
    pub per_sec: f64,
}

impl Policy {
    /// A burst of `capacity`, then `n` requests every `secs` seconds.
    pub const fn per(capacity: f64, n: f64, secs: f64) -> Policy {
        Policy { capacity, per_sec: n / secs }
    }
}

const MINUTE: f64 = 60.0;
const DAY: f64 = 86_400.0;

pub const GENERAL: Policy = Policy::per(60.0, 10.0, 1.0);
pub const AUTH: Policy = Policy::per(10.0, 10.0, MINUTE);
pub const MAIL_PER_ADDRESS: Policy = Policy::per(3.0, 1.0, 2.0 * MINUTE);
/// The server-wide mail bucket: a burst of this many, then the configured daily budget.
pub const MAIL_GLOBAL_BURST: f64 = 20.0;
pub const LOGIN_TARGET: Policy = Policy::per(10.0, 2.0, MINUTE);
/// Every create, join and retry is one handshake (the menu's server check is plain HTTP, and a
/// refused room is not retried), so this is a budget of lobby actions per minute for a household.
pub const WS: Policy = Policy::per(15.0, 15.0, MINUTE);

/// Refusals are logged once a minute per bucket, not once per refused request.
const WARN_EVERY: Duration = Duration::from_secs(60);
/// Idle buckets are dropped when this many exist (and every sweep); ~100 bytes each.
const MAX_BUCKETS: usize = 100_000;

/// Whose bucket a rule fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// One bucket per client address.
    Address,
    /// One bucket for the whole server.
    Global,
}

/// One limit: the bucket shape, who it applies to and what a refused player reads. `{wait}` in the
/// message becomes the time until the next request fits.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    pub name: &'static str,
    pub scope: Scope,
    pub policy: Policy,
    pub message: &'static str,
}

/// Which set of rules a route group is behind (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    General,
    Auth,
    Mail,
    /// Both `Auth` and `Mail`.
    Register,
}

#[derive(Clone, Debug)]
pub struct LimitConfig {
    pub enabled: bool,
    pub trust_proxy: bool,
    /// Codes the whole server may mail per day (the Resend free plan allows 100).
    pub mail_per_day: u32,
}

struct Bucket {
    level: f64,
    per_sec: f64,
    at: Instant,
    warned: Option<Instant>,
}

impl Bucket {
    /// Water left after leaking until `now`.
    fn level_at(&self, now: Instant) -> f64 {
        (self.level - now.duration_since(self.at).as_secs_f64() * self.per_sec).max(0.0)
    }
}

pub struct Limiter {
    pub enabled: bool,
    trust_proxy: bool,
    buckets: Mutex<HashMap<String, Bucket>>,
    general: Rule,
    auth: Rule,
    mail_address: Rule,
    mail_global: Rule,
}

impl Limiter {
    pub fn new(cfg: &LimitConfig) -> Limiter {
        let per_day = cfg.mail_per_day.max(1) as f64;
        Limiter {
            enabled: cfg.enabled,
            trust_proxy: cfg.trust_proxy,
            buckets: Mutex::new(HashMap::new()),
            general: Rule { name: "general", scope: Scope::Address, policy: GENERAL, message: "whoa, slow down: too many requests from your connection; try again in {wait}" },
            auth: Rule { name: "auth", scope: Scope::Address, policy: AUTH, message: "too many attempts from your connection; take a breath and try again in {wait}" },
            mail_address: Rule {
                name: "mail",
                scope: Scope::Address,
                policy: MAIL_PER_ADDRESS,
                message: "too many codes requested from your connection; check your inbox (and spam), or try again in {wait}",
            },
            mail_global: Rule {
                name: "mail-global",
                scope: Scope::Global,
                policy: Policy::per(MAIL_GLOBAL_BURST, per_day, DAY),
                message: "we have sent a lot of e-mail today, so codes are on hold for a bit; try again in {wait}",
            },
        }
    }

    fn rules(&self, group: Group) -> Vec<Rule> {
        match group {
            Group::General => vec![self.general],
            Group::Auth => vec![self.auth],
            Group::Mail => vec![self.mail_address, self.mail_global],
            Group::Register => vec![self.auth, self.mail_address, self.mail_global],
        }
    }

    /// Pours one request into `key`'s bucket. `Err` carries the seconds until it would fit.
    pub fn take(&self, key: &str, policy: Policy) -> Result<(), f64> {
        let now = Instant::now();
        let mut map = self.buckets.lock().unwrap();
        if map.len() >= MAX_BUCKETS && !map.contains_key(key) {
            Self::drop_idle(&mut map, now);
        }
        let b = map.entry(key.to_string()).or_insert(Bucket { level: 0.0, per_sec: policy.per_sec, at: now, warned: None });
        b.level = b.level_at(now);
        b.per_sec = policy.per_sec;
        b.at = now;
        if b.level + 1.0 > policy.capacity + 1e-9 {
            return Err((b.level + 1.0 - policy.capacity) / policy.per_sec);
        }
        b.level += 1.0;
        Ok(())
    }

    /// `take` for handlers and the websocket handshake: a refusal becomes the 429 response, worded
    /// with `message`. `who` is for the log line.
    pub fn check(&self, key: &str, policy: Policy, who: &str, message: &str) -> Result<(), Response> {
        if !self.enabled {
            return Ok(());
        }
        match self.take(key, policy) {
            Ok(()) => Ok(()),
            Err(wait) => {
                self.note_refusal(key, who, wait);
                Err(refused(message, wait))
            }
        }
    }

    /// Logs a refusal, at most once a minute per bucket so a flood does not flood the log too.
    fn note_refusal(&self, key: &str, who: &str, wait: f64) {
        let now = Instant::now();
        let mut map = self.buckets.lock().unwrap();
        if let Some(b) = map.get_mut(key)
            && b.warned.is_none_or(|t| now.duration_since(t) >= WARN_EVERY)
        {
            b.warned = Some(now);
            warn!(%who, bucket = key, retry_after = wait.ceil(), "rate limited");
        }
    }

    /// Drops buckets that have drained (call periodically).
    pub fn sweep(&self) {
        let now = Instant::now();
        Self::drop_idle(&mut self.buckets.lock().unwrap(), now);
    }

    fn drop_idle(map: &mut HashMap<String, Bucket>, now: Instant) {
        map.retain(|_, b| b.level_at(now) > 0.0 || b.warned.is_some_and(|t| now.duration_since(t) < WARN_EVERY));
    }

    /// How many buckets are live.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.buckets.lock().unwrap().len()
    }

    /// The bucket key for a client: its address, or the proxy-reported one when configured.
    pub fn client_key(&self, peer: Option<SocketAddr>, headers: &HeaderMap) -> String {
        let peer = peer.map(|p| p.ip());
        let ip = if self.trust_proxy { forwarded_ip(headers).or(peer) } else { peer };
        ip.map(address_key).unwrap_or_else(|| "unknown".to_string())
    }
}

/// IPv4 as is, IPv6 by /64 (one household or server usually holds a whole /64).
pub fn address_key(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
}

/// The client address a reverse proxy reports: the last `X-Forwarded-For` entry (the one our own
/// proxy appended), else `X-Real-IP`.
fn forwarded_ip(headers: &HeaderMap) -> Option<IpAddr> {
    let parse = |v: &HeaderValue| v.to_str().ok().and_then(|s| s.rsplit(',').next()).and_then(|s| s.trim().parse::<IpAddr>().ok());
    headers.get("x-forwarded-for").and_then(parse).or_else(|| headers.get("x-real-ip").and_then(parse))
}

/// `{wait}` as a player reads it.
pub fn human_wait(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs.div_ceil(60)),
        _ => format!("{} h", secs.div_ceil(3600)),
    }
}

/// The 429 response: JSON error the client shows, `retry_after` seconds, `Retry-After` header.
pub fn refused(message: &str, wait: f64) -> Response {
    let secs = wait.ceil().max(1.0) as u64;
    let msg = message.replace("{wait}", &human_wait(secs));
    let mut res = (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": msg, "retry_after": secs }))).into_response();
    res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(secs));
    res
}

/// The axum middleware: `route_layer(from_fn_with_state((state, group), limits::limit))`.
pub async fn limit(State((state, group)): State<(AppState, Group)>, req: Request, next: Next) -> Response {
    let limits = &state.limits;
    if !limits.enabled {
        return next.run(req).await;
    }
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    let who = limits.client_key(peer, req.headers());
    for rule in limits.rules(group) {
        let key = match rule.scope {
            Scope::Address => format!("{}:{who}", rule.name),
            Scope::Global => rule.name.to_string(),
        };
        if let Err(wait) = limits.take(&key, rule.policy) {
            limits.note_refusal(&key, &format!("{who} {} {}", req.method(), req.uri().path()), wait);
            return refused(rule.message, wait);
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter() -> Limiter {
        Limiter::new(&LimitConfig { enabled: true, trust_proxy: false, mail_per_day: 90 })
    }

    #[test]
    fn burst_then_refuse() {
        let l = limiter();
        let p = Policy::per(3.0, 1.0, 10.0);
        assert!(l.take("k", p).is_ok());
        assert!(l.take("k", p).is_ok());
        assert!(l.take("k", p).is_ok());
        let wait = l.take("k", p).unwrap_err();
        assert!((wait - 10.0).abs() < 0.01, "{wait}");
        // Another key has its own bucket.
        assert!(l.take("other", p).is_ok());
    }

    #[test]
    fn leaks_over_time() {
        let l = limiter();
        let p = Policy::per(1.0, 1.0, 0.01);
        assert!(l.take("k", p).is_ok());
        assert!(l.take("k", p).is_err());
        std::thread::sleep(Duration::from_millis(15));
        assert!(l.take("k", p).is_ok());
    }

    #[test]
    fn sweep_drops_drained_buckets() {
        let l = limiter();
        let p = Policy::per(1.0, 1.0, 0.01);
        assert!(l.take("k", p).is_ok());
        assert_eq!(l.len(), 1);
        std::thread::sleep(Duration::from_millis(15));
        l.sweep();
        assert_eq!(l.len(), 0);
    }

    #[test]
    fn address_keys() {
        assert_eq!(address_key("203.0.113.9".parse().unwrap()), "203.0.113.9");
        assert_eq!(address_key("::ffff:203.0.113.9".parse().unwrap()), "203.0.113.9");
        assert_eq!(address_key("2001:db8:1:2:3:4:5:6".parse().unwrap()), "2001:db8:1:2::/64");
    }

    #[test]
    fn proxy_headers_only_when_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.7, 203.0.113.9"));
        let peer: SocketAddr = "10.0.0.1:1234".parse().unwrap();
        let l = limiter();
        assert_eq!(l.client_key(Some(peer), &headers), "10.0.0.1");
        let l = Limiter::new(&LimitConfig { enabled: true, trust_proxy: true, mail_per_day: 90 });
        assert_eq!(l.client_key(Some(peer), &headers), "203.0.113.9");
        assert_eq!(l.client_key(Some(peer), &HeaderMap::new()), "10.0.0.1");
    }

    #[test]
    fn waits_read_well() {
        assert_eq!(human_wait(1), "1 s");
        assert_eq!(human_wait(59), "59 s");
        assert_eq!(human_wait(61), "2 min");
        assert_eq!(human_wait(7200), "2 h");
    }
}
