//! Who the player is: the anonymous name used while logged out, the logged-in account (login
//! token + profile), the calls to the server's `/api`, the matchmaking queue and the reporting of
//! ranked results.
//!
//! The token and the anonymous name are saved next to the settings (`account.json`, or
//! `localStorage` on the web) so nobody has to log in on every start. Requests run through
//! `ehttp` (a thread on desktop, `fetch` in the browser) and are polled every frame.

use crate::config::ClientConfig;
use crate::game::{PendingFx, RenderStates};
use crate::menu::{UiRefresh, UiScreen};
use crate::net::{LocalHandle, MatchExit, MatchKind, NetCommand, PlayerNames, SignalState, SignalingStatus};
use crate::settings::storage;
use crate::textfield::{Field, Form};
use crate::AppState;
use bevy::prelude::*;
use endif_sim::SimEvent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Longest display name (anonymous or account).
pub const NAME_MAX: usize = 20;
const STORE: &str = "account.json";
/// How often the queue is polled.
const QUEUE_POLL_SECS: f64 = 1.5;
/// How often the main menu asks how many players are searching.
const SEARCHING_POLL_SECS: f64 = 5.0;
/// How long the result banner stays up before a finished ranked match returns to the menu.
const RANKED_EXIT_SECS: f64 = 4.0;
/// How often the result popup asks the server whether the match has been settled.
const RESULT_POLL_SECS: f64 = 2.0;
/// How long the popup keeps asking before it gives up on showing the rating change. A forfeit
/// settles once the server's grace period for the missing report (15 s) has passed.
const RESULT_WAIT_SECS: f64 = 60.0;
/// How long "resend code" stays greyed out after a code was sent (the server has its own, longer
/// cooldown; this just stops a double click from asking twice).
const RESEND_WAIT_SECS: f64 = 10.0;

#[derive(Clone, Debug, Deserialize, Serialize, Default, PartialEq)]
pub struct UserInfo {
    pub id: u64,
    pub username: String,
    #[serde(default)]
    pub email: Option<String>,
    pub elo: i32,
    pub wins: i32,
    pub losses: i32,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub struct HistoryEntry {
    pub id: u64,
    /// False for a private-room round: no ratings, and it counted for nothing.
    #[serde(default = "yes")]
    pub ranked: bool,
    pub opponent: String,
    pub my_score: i32,
    pub their_score: i32,
    pub won: bool,
    /// Ratings at match time and the change; absent on casual rounds.
    #[serde(default)]
    pub my_elo: Option<i32>,
    #[serde(default)]
    pub their_elo: Option<i32>,
    #[serde(default)]
    pub delta: Option<i32>,
    /// Unix seconds.
    pub played_at: i64,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
pub struct Profile {
    pub user: UserInfo,
    #[serde(default)]
    pub matches: Vec<HistoryEntry>,
}

/// A ranked pairing from the queue.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MatchInfo {
    pub match_id: u64,
    pub room: String,
    /// 0 = player_a, 1 = player_b in the server's record.
    pub slot: u8,
    pub opponent: String,
    pub opponent_elo: i32,
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    anon_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Req {
    Me,
    Login,
    Register,
    Verify,
    /// A fresh verification code for the pending registration.
    Resend,
    Forgot,
    Reset,
    ChangeUsername,
    ChangePassword,
    Profile,
    QueueJoin,
    QueuePoll,
    QueueLeave,
    /// How many players are searching, for the main menu.
    QueueCount,
    Report,
    /// A private-room round for the history.
    Casual,
    /// Whether a finished match has been settled yet (the result popup).
    MatchStatus,
}

impl Req {
    /// Requests the forms wait on (buttons show "working...").
    fn blocks(self) -> bool {
        !matches!(self, Req::Me | Req::QueuePoll | Req::QueueLeave | Req::QueueCount | Req::Report | Req::Casual | Req::MatchStatus)
    }
}

type Slot = Arc<Mutex<Option<Result<(u16, Vec<u8>), String>>>>;

struct Pending {
    kind: Req,
    slot: Slot,
}

pub struct QueueState {
    pub ticket: Option<String>,
    pub next_poll: f64,
    pub position: usize,
    /// Players in the queue, us included (0 until the server has answered).
    pub waiting: usize,
    pub since: f64,
}

/// How a ranked match ended, from this player's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Won,
    Lost,
    /// The opponent dropped or left: a win by forfeit.
    OpponentLeft,
    /// We left through the pause menu: a loss by forfeit.
    WeLeft,
}

/// The rating side of a finished match, as the server settles it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rating {
    /// Still waiting on the server (the other report, or its grace period).
    Pending,
    Settled(i32),
    /// The two reports named different winners; nobody's rating moved.
    Void,
    /// Gave up waiting; the profile will show it once the server gets there.
    Unconfirmed,
}

/// A ranked match that just ended: shown as a popup on the main menu until dismissed.
#[derive(Clone, Debug)]
pub struct RankedResult {
    pub match_id: u64,
    pub ending: Ending,
    pub rating: Rating,
    /// Menu time the popup started waiting, and when it next asks the server.
    since: Option<f64>,
    next_poll: f64,
}

#[derive(Resource)]
pub struct Account {
    pub token: Option<String>,
    pub user: Option<UserInfo>,
    pub anon_name: String,
    /// Last fetched profile (own or not), for the profile screen.
    pub profile: Option<Profile>,
    /// Address the register / forgot flows are waiting on a code for.
    pub pending_email: String,
    /// Menu time (`Time<Real>`) from which "resend code" may be pressed again.
    pub resend_at: f64,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub queue: Option<QueueState>,
    /// Players searching for a match right now, as last reported; `None` until the server has
    /// answered (or after it stopped answering).
    pub searching: Option<usize>,
    /// Menu time of the next `/queue` count request.
    next_searching_poll: f64,
    /// The ranked match that just ended, until its popup is dismissed.
    pub result: Option<RankedResult>,
    requests: Vec<Pending>,
}

/// Per-match bookkeeping for ranked games.
#[derive(Resource, Default)]
pub struct RankedState {
    pub reported: bool,
    /// When the deciding round ended; the match leaves for the menu a few seconds later.
    pub finished_at: Option<f64>,
}

impl Account {
    pub fn load(name_override: Option<String>) -> Account {
        let saved: Saved = storage::read(STORE).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let anon_name = name_override
            .or(saved.anon_name)
            .map(|n| clean_name(&n))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(random_anon_name);
        Account {
            token: saved.token,
            user: None,
            anon_name,
            profile: None,
            pending_email: String::new(),
            resend_at: 0.0,
            error: None,
            notice: None,
            queue: None,
            searching: None,
            next_searching_poll: 0.0,
            result: None,
            requests: Vec::new(),
        }
    }

    fn save(&self) {
        let saved = Saved { token: self.token.clone(), anon_name: Some(self.anon_name.clone()) };
        if let Err(e) = storage::write(STORE, &serde_json::to_string(&saved).unwrap_or_default()) {
            warn!("could not save the account state: {e}");
        }
    }

    pub fn logged_in(&self) -> bool {
        self.user.is_some()
    }

    /// The name shown to the other player: the account name, or the anonymous one.
    pub fn display_name(&self) -> String {
        self.user.as_ref().map(|u| u.username.clone()).unwrap_or_else(|| self.anon_name.clone())
    }

    pub fn set_anon_name(&mut self, name: &str) {
        let name = clean_name(name);
        if !name.is_empty() && name != self.anon_name {
            self.anon_name = name;
            self.save();
        }
    }

    /// A form request is in flight.
    pub fn busy(&self) -> bool {
        self.requests.iter().any(|r| r.kind.blocks())
    }

    fn in_flight(&self, kind: Req) -> bool {
        self.requests.iter().any(|r| r.kind == kind)
    }

    fn call(&mut self, cfg: &ClientConfig, kind: Req, path: &str, body: Option<Value>) {
        if kind.blocks() {
            self.error = None;
            self.notice = None;
        }
        let url = format!("{}/api{}", cfg.api_url(), path);
        let mut req = match body {
            Some(b) => {
                let mut req = ehttp::Request::post(&url, serde_json::to_vec(&b).unwrap_or_default());
                // `post` presets `text/plain` and `Headers::insert` appends rather than replaces, so a
                // second Content-Type would sit behind it and axum's `Json` extractor would answer 415.
                req.headers = ehttp::Headers::new(&[("Accept", "*/*"), ("Content-Type", "application/json")]);
                req
            }
            None => ehttp::Request::get(&url),
        };
        if let Some(t) = &self.token {
            req.headers.insert("Authorization", format!("Bearer {t}"));
        }
        let slot: Slot = Arc::new(Mutex::new(None));
        let out = slot.clone();
        debug!("{kind:?}: {url}");
        ehttp::fetch(req, move |res| {
            *out.lock().unwrap() = Some(res.map(|r| (r.status, r.bytes)).map_err(|e| e.to_string()));
        });
        self.requests.push(Pending { kind, slot });
    }

    pub fn fetch_me(&mut self, cfg: &ClientConfig) {
        if self.token.is_some() && !self.in_flight(Req::Me) {
            self.call(cfg, Req::Me, "/me", None);
        }
    }

    pub fn login(&mut self, cfg: &ClientConfig, username: &str, password: &str) {
        self.call(cfg, Req::Login, "/login", Some(json!({ "username": username, "password": password })));
    }

    pub fn register(&mut self, cfg: &ClientConfig, email: &str, username: &str, password: &str) {
        self.pending_email = email.to_string();
        self.call(cfg, Req::Register, "/register", Some(json!({ "email": email, "username": username, "password": password })));
    }

    pub fn verify(&mut self, cfg: &ClientConfig, code: &str) {
        let email = self.pending_email.clone();
        self.call(cfg, Req::Verify, "/verify", Some(json!({ "email": email, "code": code })));
    }

    /// Asks for another verification code for the registration waiting on `pending_email`.
    pub fn resend(&mut self, cfg: &ClientConfig) {
        let email = self.pending_email.clone();
        self.call(cfg, Req::Resend, "/resend", Some(json!({ "email": email })));
    }

    /// "resend code" is pressable: the short wait after the last code is over and nothing is in flight.
    pub fn can_resend(&self, now: f64) -> bool {
        now >= self.resend_at && !self.busy()
    }

    pub fn forgot(&mut self, cfg: &ClientConfig, email: &str) {
        self.pending_email = email.to_string();
        self.call(cfg, Req::Forgot, "/forgot", Some(json!({ "email": email })));
    }

    pub fn reset(&mut self, cfg: &ClientConfig, code: &str, password: &str) {
        let email = self.pending_email.clone();
        self.call(cfg, Req::Reset, "/reset", Some(json!({ "email": email, "code": code, "password": password })));
    }

    pub fn change_username(&mut self, cfg: &ClientConfig, username: &str) {
        self.call(cfg, Req::ChangeUsername, "/account/username", Some(json!({ "username": username })));
    }

    pub fn change_password(&mut self, cfg: &ClientConfig, current: &str, password: &str) {
        self.call(cfg, Req::ChangePassword, "/account/password", Some(json!({ "current": current, "password": password })));
    }

    pub fn fetch_profile(&mut self, cfg: &ClientConfig, username: &str) {
        self.call(cfg, Req::Profile, &format!("/profile/{username}"), None);
    }

    pub fn logout(&mut self) {
        self.token = None;
        self.user = None;
        self.profile = None;
        self.save();
    }

    pub fn join_queue(&mut self, cfg: &ClientConfig, now: f64) {
        self.queue = Some(QueueState { ticket: None, next_poll: now, position: 0, waiting: 0, since: now });
        self.call(cfg, Req::QueueJoin, "/queue/join", Some(json!({})));
    }

    pub fn leave_queue(&mut self, cfg: &ClientConfig) {
        if let Some(q) = self.queue.take()
            && let Some(ticket) = q.ticket
        {
            self.call(cfg, Req::QueueLeave, "/queue/leave", Some(json!({ "ticket": ticket })));
        }
    }

    fn poll_queue(&mut self, cfg: &ClientConfig, ticket: &str) {
        self.call(cfg, Req::QueuePoll, "/queue/poll", Some(json!({ "ticket": ticket })));
    }

    /// Asks how many players are searching (the main menu's "(N searching)").
    fn fetch_searching(&mut self, cfg: &ClientConfig) {
        if !self.in_flight(Req::QueueCount) {
            self.call(cfg, Req::QueueCount, "/queue", None);
        }
    }

    /// Sends this player's view of a ranked result: `score` in match-record order (a, b).
    pub fn report(&mut self, cfg: &ClientConfig, match_id: u64, score: [i32; 2], winner: u8) {
        self.call(cfg, Req::Report, &format!("/match/{match_id}/report"), Some(json!({ "score": score, "winner": winner })));
    }

    /// Records a finished private-room round in the history (logged-in players only).
    pub fn report_casual(&mut self, cfg: &ClientConfig, room: &str, opponent: &str, score: [i32; 2], won: bool) {
        if !self.logged_in() {
            return;
        }
        self.call(cfg, Req::Casual, "/match/casual", Some(json!({ "room": room, "opponent": opponent, "score": score, "won": won })));
    }

    /// Remembers how the ranked match that just ended went, for the popup on the main menu.
    fn set_result(&mut self, match_id: u64, ending: Ending) {
        self.result = Some(RankedResult { match_id, ending, rating: Rating::Pending, since: None, next_poll: 0.0 });
    }

    /// The result popup is up: dismiss it.
    pub fn dismiss_result(&mut self) {
        self.result = None;
    }
}

/// Trims and caps a display name.
fn clean_name(name: &str) -> String {
    name.trim().chars().filter(|c| !c.is_control()).take(NAME_MAX).collect::<String>().trim().to_string()
}

/// `SoldierXYZ` with three random digits.
fn random_anon_name() -> String {
    use rand::Rng;
    format!("Soldier{:03}", rand::thread_rng().gen_range(0..1000))
}

pub struct AccountPlugin;

impl Plugin for AccountPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RankedState>()
            .add_systems(Startup, initial_me)
            .add_systems(OnEnter(AppState::Menu), refresh_me)
            .add_systems(OnEnter(AppState::InGame), |mut r: ResMut<RankedState>| *r = RankedState::default())
            .add_systems(
                Update,
                (
                    poll_requests,
                    queue_tick.run_if(in_state(AppState::Menu)),
                    searching_tick.run_if(in_state(AppState::Menu)),
                    result_poll.run_if(in_state(AppState::Menu)),
                    sync_anon_name.run_if(in_state(AppState::Menu)),
                )
                    .chain(),
            )
            .add_systems(Update, (ranked_round_end, casual_round_end).run_if(in_state(AppState::InGame)))
            .add_systems(OnExit(AppState::InGame), ranked_exit.before(crate::net::teardown_match));
    }
}

fn initial_me(mut account: ResMut<Account>, cfg: Res<ClientConfig>) {
    account.fetch_me(&cfg);
}

/// Back in the menu (after a ranked match): pick up the new rating.
fn refresh_me(mut account: ResMut<Account>, cfg: Res<ClientConfig>) {
    account.fetch_me(&cfg);
}

/// Body of a finished request: the JSON on success, the server's `error` (or a network message)
/// on failure, and whether the failure was the token being rejected.
fn outcome(result: Result<(u16, Vec<u8>), String>) -> Result<Value, (String, bool)> {
    match result {
        Err(e) => {
            warn!("request failed: {e}");
            Err(("cannot reach the server".into(), false))
        }
        Ok((status, bytes)) => {
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if (200..300).contains(&status) {
                Ok(value)
            } else {
                // The server's errors are written for the player; these cover answers without one
                // (a proxy in between, or a body that is not JSON).
                let msg = value.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| match status {
                    429 => "whoa, slow down a little: too many requests; try again in a moment".to_string(),
                    502..=504 => "the server is not answering right now; try again in a moment".to_string(),
                    _ => format!("server error ({status})"),
                });
                Err((msg, status == 401))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_requests(
    mut account: ResMut<Account>,
    mut screen: ResMut<UiScreen>,
    mut refresh: ResMut<UiRefresh>,
    mut form: ResMut<Form>,
    mut cmds: MessageWriter<NetCommand>,
    cfg: Res<ClientConfig>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed_secs_f64();
    let mut done = Vec::new();
    let mut i = 0;
    while i < account.requests.len() {
        let finished = account.requests[i].slot.lock().unwrap().take();
        match finished {
            Some(result) => {
                let p = account.requests.remove(i);
                done.push((p.kind, result));
            }
            None => i += 1,
        }
    }
    for (kind, result) in done {
        // A queue poll answers every 1.5 s while nothing on screen changes (the status line ticks
        // on its own); rebuilding the screen for it flashes the backdrop. A match or a refusal
        // switches screens, which rebuilds anyway. The result popup's poll refreshes only once
        // the answer changes what it shows.
        // The searching count is a label that updates in place.
        if !matches!(kind, Req::QueuePoll | Req::MatchStatus | Req::QueueCount) {
            refresh.0 = true;
        }
        match (kind, outcome(result)) {
            (Req::Me, Ok(v)) => {
                if let Ok(user) = serde_json::from_value::<UserInfo>(v["user"].clone()) {
                    account.user = Some(user);
                }
            }
            (Req::Me, Err((msg, rejected))) => {
                if rejected {
                    info!("saved login is no longer valid: {msg}");
                    account.logout();
                }
            }
            (Req::Login | Req::Verify | Req::Reset | Req::ChangeUsername | Req::ChangePassword, Ok(v)) => {
                account.token = v["token"].as_str().map(str::to_string);
                account.user = serde_json::from_value(v["user"].clone()).ok();
                account.save();
                form.clear_secrets();
                form.clear(Field::Code);
                form.clear(Field::NewUsername);
                *screen = match kind {
                    Req::ChangeUsername => {
                        account.notice = Some("username changed".into());
                        let name = account.display_name();
                        account.fetch_profile(&cfg, &name);
                        UiScreen::Profile
                    }
                    Req::ChangePassword => {
                        account.notice = Some("password changed".into());
                        UiScreen::Profile
                    }
                    _ => UiScreen::Main,
                };
            }
            (Req::Register, Ok(_)) => {
                account.resend_at = now + RESEND_WAIT_SECS;
                *screen = UiScreen::Verify;
            }
            (Req::Resend, Ok(_)) => {
                account.resend_at = now + RESEND_WAIT_SECS;
                account.notice = Some("a new code is on its way".into());
            }
            (Req::Forgot, Ok(_)) => {
                account.resend_at = now + RESEND_WAIT_SECS;
                // Pressed "resend code" on the reset screen: say so (the address is not confirmed
                // either way, and the server keeps its own cooldown, so the older code still works).
                if *screen == UiScreen::Reset {
                    account.notice = Some("if that address has an account, a new code is on its way".into());
                }
                *screen = UiScreen::Reset;
            }
            (Req::Profile, Ok(v)) => match serde_json::from_value::<Profile>(v) {
                Ok(p) => account.profile = Some(p),
                Err(e) => account.error = Some(format!("bad profile data: {e}")),
            },
            (Req::QueueJoin, Ok(v)) => {
                if let Some(q) = account.queue.as_mut() {
                    q.ticket = v["ticket"].as_str().map(str::to_string);
                    q.waiting = v["waiting"].as_u64().unwrap_or(0) as usize;
                }
            }
            (Req::QueueJoin, Err((msg, _))) => {
                account.queue = None;
                account.error = Some(msg);
                *screen = UiScreen::Main;
            }
            (Req::QueuePoll, Ok(v)) => match v["status"].as_str().unwrap_or_default() {
                "matched" => match serde_json::from_value::<MatchInfo>(v["match"].clone()) {
                    Ok(info) => {
                        info!("matched against {} ({} elo), room {}", info.opponent, info.opponent_elo, info.room);
                        account.queue = None;
                        cmds.write(NetCommand::StartRanked(info));
                    }
                    Err(e) => {
                        account.queue = None;
                        account.error = Some(format!("bad match data: {e}"));
                        *screen = UiScreen::Main;
                    }
                },
                "waiting" => {
                    if let Some(q) = account.queue.as_mut() {
                        q.position = v["position"].as_u64().unwrap_or(0) as usize;
                        q.waiting = v["waiting"].as_u64().unwrap_or(0) as usize;
                    }
                }
                _ => {
                    // Expired (the server restarted, or we stopped polling): join again.
                    if let Some(q) = account.queue.as_mut() {
                        q.ticket = None;
                    }
                    if *screen == UiScreen::Queue {
                        account.call(&cfg, Req::QueueJoin, "/queue/join", Some(json!({})));
                    }
                }
            },
            (Req::QueuePoll, Err((msg, rejected))) => {
                if rejected {
                    account.queue = None;
                    account.error = Some(msg);
                    *screen = UiScreen::Main;
                }
            }
            (Req::QueueCount, Ok(v)) => account.searching = v["waiting"].as_u64().map(|n| n as usize),
            (Req::QueueCount, Err(_)) => account.searching = None,
            (Req::QueueLeave | Req::Report | Req::Casual, Ok(_)) => {}
            (Req::Report, Err((msg, _))) => warn!("result report refused: {msg}"),
            (Req::Casual, Err((msg, _))) => warn!("casual round not recorded: {msg}"),
            (Req::MatchStatus, Ok(v)) => {
                let Some(r) = account.result.as_mut() else { continue };
                let rating = match v["status"].as_str().unwrap_or_default() {
                    "finished" => Rating::Settled(v["delta"].as_i64().unwrap_or(0) as i32),
                    "void" => Rating::Void,
                    _ => Rating::Pending,
                };
                if rating != Rating::Pending {
                    info!("ranked match {} settled: {rating:?}", r.match_id);
                    r.rating = rating;
                    refresh.0 = true;
                    // The identity corner shows the rating: pick up the new one.
                    account.fetch_me(&cfg);
                }
            }
            (Req::MatchStatus, Err((msg, _))) => warn!("match status: {msg}"),
            (_, Err((msg, rejected))) => {
                if rejected && matches!(kind, Req::ChangeUsername | Req::ChangePassword | Req::Profile) && account.token.is_some() && msg.contains("session") {
                    account.logout();
                    *screen = UiScreen::Main;
                }
                account.error = Some(msg);
            }
        }
    }
}

/// Polls the queue while the queue screen is up; leaves it when the screen goes away.
fn queue_tick(mut account: ResMut<Account>, screen: Res<UiScreen>, cfg: Res<ClientConfig>, time: Res<Time<Real>>) {
    if *screen != UiScreen::Queue {
        if account.queue.is_some() {
            account.leave_queue(&cfg);
        }
        return;
    }
    let now = time.elapsed_secs_f64();
    let Some(q) = account.queue.as_mut() else { return };
    if now < q.next_poll {
        return;
    }
    q.next_poll = now + QUEUE_POLL_SECS;
    let Some(ticket) = q.ticket.clone() else { return };
    if !account.in_flight(Req::QueuePoll) && !account.in_flight(Req::QueueJoin) {
        account.poll_queue(&cfg, &ticket);
    }
}

/// Keeps the main menu's count of searching players current while it is up and the server answers.
fn searching_tick(mut account: ResMut<Account>, screen: Res<UiScreen>, status: Res<SignalingStatus>, cfg: Res<ClientConfig>, time: Res<Time<Real>>) {
    if *screen != UiScreen::Main || status.state != SignalState::Up {
        return;
    }
    let now = time.elapsed_secs_f64();
    if now < account.next_searching_poll {
        return;
    }
    account.next_searching_poll = now + SEARCHING_POLL_SECS;
    account.fetch_searching(&cfg);
}

/// The anonymous name box saves as it is typed.
fn sync_anon_name(form: Res<Form>, mut account: ResMut<Account>) {
    if !form.is_changed() {
        return;
    }
    let typed = form.get(Field::AnonName);
    if !typed.is_empty() && clean_name(typed) != account.anon_name {
        account.set_anon_name(typed);
    }
}

/// Maps a sim result (indexed by GGRS handle) to the server's a/b order.
fn report_for(info: &MatchInfo, local: usize, score: [i32; 2], winner_handle: usize) -> ([i32; 2], u8) {
    let (mine, theirs) = (score[local], score[1 - local]);
    let ordered = if info.slot == 0 { [mine, theirs] } else { [theirs, mine] };
    let winner = if winner_handle == local { info.slot } else { 1 - info.slot };
    (ordered, winner)
}

/// The deciding round of a ranked match ended: report it, then head back to the menu, where the
/// result popup waits for the server to settle the rating.
#[allow(clippy::too_many_arguments)]
fn ranked_round_end(
    fx: Res<PendingFx>,
    kind: Option<Res<MatchKind>>,
    local: Res<LocalHandle>,
    time: Res<Time<Real>>,
    cfg: Res<ClientConfig>,
    mut account: ResMut<Account>,
    mut ranked: ResMut<RankedState>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(MatchKind::Ranked(info)) = kind.as_deref() else { return };
    let now = time.elapsed_secs_f64();
    if !ranked.reported {
        for ev in &fx.events {
            if let SimEvent::RoundWon { winner, score } = ev {
                let won = *winner as usize == local.0;
                let (score, winner) = report_for(info, local.0, *score, *winner as usize);
                info!("ranked match {} over: {score:?}, winner slot {winner}", info.match_id);
                account.report(&cfg, info.match_id, score, winner);
                account.set_result(info.match_id, if won { Ending::Won } else { Ending::Lost });
                ranked.reported = true;
                ranked.finished_at = Some(now);
            }
        }
    }
    if let Some(t) = ranked.finished_at
        && now - t >= RANKED_EXIT_SECS
    {
        ranked.finished_at = None;
        next.set(AppState::Menu);
    }
}

/// A round in a private room reached the frag limit: put it in the history. The room goes on
/// (the sim resets the scores for the next round), so this fires once per round, and a round
/// abandoned part-way is not recorded. The opponent is known by display name only.
fn casual_round_end(fx: Res<PendingFx>, kind: Option<Res<MatchKind>>, local: Res<LocalHandle>, names: Res<PlayerNames>, cfg: Res<ClientConfig>, mut account: ResMut<Account>) {
    let Some(MatchKind::Room { code }) = kind.as_deref() else { return };
    for ev in &fx.events {
        if let SimEvent::RoundWon { winner, score } = ev {
            let me = local.0;
            let opponent = names.0[1 - me].trim();
            let opponent = if opponent.is_empty() { "unknown" } else { opponent };
            let won = *winner as usize == me;
            info!("casual round in {code} over: {}-{} vs {opponent}", score[me], score[1 - me]);
            account.report_casual(&cfg, code, opponent, [score[me], score[1 - me]], won);
        }
    }
}

/// Leaving a ranked match early: whoever left forfeits with the score as it stood.
fn ranked_exit(
    kind: Option<Res<MatchKind>>,
    exit: Res<MatchExit>,
    ranked: Res<RankedState>,
    states: Option<Res<RenderStates>>,
    local: Res<LocalHandle>,
    cfg: Res<ClientConfig>,
    mut account: ResMut<Account>,
) {
    let Some(MatchKind::Ranked(info)) = kind.as_deref() else { return };
    if ranked.reported {
        return;
    }
    let score = states.as_ref().map(|s| [s.cur.players[0].score, s.cur.players[1].score]).unwrap_or([0, 0]);
    let winner_handle = if exit.opponent_left { local.0 } else { 1 - local.0 };
    let (score, winner) = report_for(info, local.0, score, winner_handle);
    info!("ranked match {} abandoned ({}): {score:?}, winner slot {winner}", info.match_id, if exit.opponent_left { "opponent left" } else { "we left" });
    account.report(&cfg, info.match_id, score, winner);
    account.set_result(info.match_id, if exit.opponent_left { Ending::OpponentLeft } else { Ending::WeLeft });
}

/// While the result popup is up and the rating is pending, asks the server every few seconds
/// whether the match has been settled; gives up after a while.
fn result_poll(mut account: ResMut<Account>, cfg: Res<ClientConfig>, time: Res<Time<Real>>, mut refresh: ResMut<UiRefresh>) {
    let now = time.elapsed_secs_f64();
    let Some(r) = account.result.as_mut() else { return };
    if r.rating != Rating::Pending {
        return;
    }
    let since = *r.since.get_or_insert(now);
    if now - since >= RESULT_WAIT_SECS {
        warn!("ranked match {} still not settled after {RESULT_WAIT_SECS} s; giving up on the popup", r.match_id);
        r.rating = Rating::Unconfirmed;
        refresh.0 = true;
        return;
    }
    if now < r.next_poll {
        return;
    }
    r.next_poll = now + RESULT_POLL_SECS;
    let match_id = r.match_id;
    if !account.in_flight(Req::MatchStatus) {
        account.call(&cfg, Req::MatchStatus, &format!("/match/{match_id}"), None);
    }
}
