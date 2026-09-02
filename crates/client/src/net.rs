//! Room-based peer-to-peer connection: a matchbox WebRTC socket for signaling/transport and a GGRS
//! session on top of its unreliable data channel. A second, reliable channel carries the one-off
//! hello with each player's display name. A separate socket to the server's `/presence` path,
//! held open for as long as the app runs, is how the server counts who is online (`keep_presence`).

use crate::account::{MatchInfo, QuickMatch};
use crate::config::{ClientConfig, seed_from_room};
use crate::menu::UiRefresh;
use crate::{AppState, GameEntity};
use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use bevy_ggrs::prelude::*;
use bevy_ggrs::{LocalPlayers, ggrs::DesyncDetection};
use endif_sim::PlayerInput;
use matchbox_socket::{ChannelConfig, MessageLoopFuture, PeerId, PeerState, WebRtcChannel, WebRtcSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// GGRS configuration: our input type, peers addressed by matchbox `PeerId`.
pub type Config = GgrsConfig<PlayerInput, PeerId>;

/// Frame rate GGRS advances the simulation at. TF2 ticks at 66.67 Hz; GGRS needs an integer, so
/// 67 (0.5% fast) is used rather than 66 (1% slow).
pub const ROLLBACK_FPS: usize = 67;
pub const INPUT_DELAY: usize = 2;
pub const MAX_PREDICTION: usize = 12;
/// Silence from the peer GGRS tolerates before it drops the match. A browser client can stall for
/// several seconds on its first match (see `warmup.rs`); GGRS's 2 s default turned that into a
/// disconnect for both players. A peer that really went away is noticed sooner through matchbox's
/// peer state (`watch_session`), so this only decides how long a stalled peer is waited for.
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Silence after which the HUD reports "connection interrupted".
pub const DISCONNECT_NOTIFY_START: Duration = Duration::from_millis(1000);

/// A lobby nobody joins gives its room back after this long, so a forgotten tab cannot hold a
/// room for the rest of the day. (Only counted while the engine runs, i.e. the tab is visible.)
pub const LOBBY_TIMEOUT_MINUTES: u64 = 30;
const LOBBY_TIMEOUT: f64 = LOBBY_TIMEOUT_MINUTES as f64 * 60.0;

/// Socket channels: GGRS traffic, and the reliable side channel for the hello.
const GAME_CHANNEL: usize = 0;
const CHAT_CHANNEL: usize = 1;
/// First byte of a hello message on the reliable channel; the rest is the UTF-8 display name.
const MSG_HELLO: u8 = 1;

/// How the current match was started.
#[derive(Resource, Clone, Debug)]
pub enum MatchKind {
    /// Local sync-test session, both players controlled locally (player 1 idle).
    Practice,
    /// Peer-to-peer private room. No ratings; finished rounds go into the history as casual.
    Room { code: String },
    /// A quick play pairing: like a private room (unrated, rounds go on until someone leaves),
    /// but the queue picked the opponent.
    Quick(QuickMatch),
    /// A matchmade game between two accounts; the result is reported and rated.
    Ranked(MatchInfo),
}

/// Display names of both players, indexed by GGRS handle.
#[derive(Resource, Clone, Debug, Default)]
pub struct PlayerNames(pub [String; 2]);

impl PlayerNames {
    /// Name as the HUD shows it (upper case, cut to fit a score box).
    pub fn short(&self, handle: usize) -> String {
        let name = self.0.get(handle).map(String::as_str).unwrap_or_default();
        let name = if name.is_empty() { "???" } else { name };
        name.chars().take(12).collect::<String>().to_uppercase()
    }
}

/// Why a match is ending, for the forfeit logic of ranked games.
#[derive(Resource, Clone, Debug, Default)]
pub struct MatchExit {
    pub opponent_left: bool,
}

/// Outcome of a socket's signaling loop once it has ended (`None` while it is still running).
pub type LoopResult = Arc<Mutex<Option<Result<(), String>>>>;

/// Result of a background HTTP request (`ehttp`: a thread on desktop, `fetch` in the browser):
/// status and body, or a transport error. `None` while in flight.
pub type HttpResult = Result<(u16, Vec<u8>), String>;
pub type HttpSlot = Arc<Mutex<Option<HttpResult>>>;

fn http_get(url: &str) -> HttpSlot {
    let slot: HttpSlot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    ehttp::fetch(ehttp::Request::get(url), move |res| {
        *out.lock().unwrap() = Some(res.map(|r| (r.status, r.bytes)).map_err(|e| e.to_string()));
    });
    slot
}

/// Why a lobby ended without a match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomFailure {
    /// Both slots of the room are taken.
    Full,
    /// The server is up and the room has space, but it refused or closed our connection (a rate
    /// limit after many handshakes from one address, or the socket died).
    Refused,
    /// The matchmaking server cannot be reached.
    Unreachable,
    /// The server runs a different protocol: this build is out of date.
    Outdated,
    /// Nobody joined within [`LOBBY_TIMEOUT_MINUTES`].
    Timeout,
    /// The opponent was found but the WebRTC connection to them failed (ICE state `failed`: no
    /// candidate pair worked, typically a firewall, VPN or a network that blocks UDP). Web only:
    /// the page reports the browser's ICE state, matchbox itself never gives up.
    PeerUnreachable,
    /// Web only: the browser hides why a websocket was refused, so `/api/room/<code>` is being
    /// asked whether the room is full. Resolves to one of the above.
    Checking,
}

/// Live room state while connecting / playing.
#[derive(Resource)]
pub struct RoomConnection {
    pub code: String,
    /// `None` once the lobby gave its slot back (timeout).
    pub socket: Option<WebRtcSocket>,
    pub loop_result: LoopResult,
    /// The signaling server accepted us (we have a peer id) and is waiting for the other player.
    pub connected: bool,
    pub started: bool,
    /// Why the lobby ended, once it has.
    pub failure: Option<RoomFailure>,
    /// Menu time (`Time<Real>`) the server accepted us, for the lobby timeout.
    accepted_at: Option<f64>,
    /// The occupancy lookup behind `RoomFailure::Checking`, with the time it was started.
    check: Option<(HttpSlot, f64)>,
}

impl RoomConnection {
    /// Web: the browser's ICE connection state with the opponent ("new", "checking",
    /// "connected"...) once the handshake with them has started; `None` before that and on desktop.
    pub fn peer_status(&self) -> Option<String> {
        web_rtc::state()
    }
}

/// The browser's view of the WebRTC handshake, recorded by `web/index.html` (matchbox reports
/// nothing between "opponent found" and "data channel open"). Desktop: nothing.
mod web_rtc {
    pub fn state() -> Option<String> {
        #[cfg(target_arch = "wasm32")]
        {
            let window = web_sys::window()?;
            let v = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("endifRtcState")).ok()?;
            v.as_string().filter(|s| !s.is_empty())
        }
        #[cfg(not(target_arch = "wasm32"))]
        None
    }
}

/// Whether the matchmaking (signaling) server can be reached right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignalState {
    #[default]
    Unknown,
    Up,
    Down,
    /// The server is up but runs a different protocol: this build is stale (or too new).
    Outdated,
}

/// Desktop: what `/download/<platform>.version` said about the package on the site.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub enum PackageBuild {
    /// Not asked yet, or the answer is still on its way.
    #[default]
    Unknown,
    /// The site does not publish one (a bare server without nginx in front): the updater has to
    /// find out by downloading.
    Unavailable,
    Known(String),
}

/// The menu's view of the matchmaking server, kept current by polling `GET /version` and
/// `GET /build`: the UI greys out online play while it is unreachable or this build's protocol is
/// stale, and offers an update as soon as the server runs a newer build at all.
#[derive(Resource, Default)]
pub struct SignalingStatus {
    pub state: SignalState,
    /// The protocol id the server last reported.
    pub server_version: Option<String>,
    /// The build id the server last reported (the version an update must reach); `None` from a
    /// server that predates `/build`.
    pub server_build: Option<String>,
    /// Desktop: the build of the package on the site, asked for while an update is pending.
    pub package_build: PackageBuild,
    /// The `/version` and `/build` requests in flight and when they were sent.
    probe: Option<(HttpSlot, HttpSlot, f64)>,
    #[cfg(not(target_arch = "wasm32"))]
    package_probe: Option<(HttpSlot, f64)>,
    next_probe_at: f64,
}

impl SignalingStatus {
    /// Online play is unavailable (server unreachable or this build is out of date).
    pub fn is_down(&self) -> bool {
        matches!(self.state, SignalState::Down | SignalState::Outdated)
    }
    /// The server runs a different protocol: it will refuse this build.
    pub fn is_outdated(&self) -> bool {
        self.state == SignalState::Outdated
    }
    /// The server runs a newer build than this one. Without a protocol change this build can
    /// still play; the menu offers the update either way.
    pub fn update_available(&self) -> bool {
        self.is_outdated() || self.server_build.as_deref().is_some_and(|b| b != endif_sim::BUILD_ID)
    }
    /// Desktop: the package on the site is the server's build, so the updater will get it (or the
    /// site does not say, and the updater checks what it downloaded). The packages go up a few
    /// minutes after the server, so this lags `update_available`.
    pub fn package_ready(&self) -> bool {
        match &self.package_build {
            PackageBuild::Known(p) => Some(p) == self.server_build.as_ref(),
            PackageBuild::Unavailable => true,
            PackageBuild::Unknown => false,
        }
    }
    /// Everything the menu draws differently; a change means a redraw.
    fn ui_key(&self) -> (SignalState, bool, bool) {
        (self.state, self.update_available(), self.package_ready())
    }
    /// Forgets the requests in flight (entering a match: the answers are not wanted any more).
    fn cancel(&mut self) {
        self.probe = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.package_probe = None;
        }
    }

    /// Both probe answers are in: records what the server reported and returns the new state.
    /// `may_reload` allows the web page to fetch a newer build straight away (only from the main
    /// menu: a reload from a lobby would lose the room).
    fn settle(&mut self, version: HttpResult, build: HttpResult, may_reload: bool) -> SignalState {
        let version = match version {
            Ok((200, body)) => String::from_utf8_lossy(&body).trim().to_string(),
            Ok((code, _)) => {
                warn!("version probe answered HTTP {code}");
                return SignalState::Down;
            }
            Err(e) => {
                warn!("version probe failed: {e}");
                return SignalState::Down;
            }
        };
        self.server_version = Some(version.clone());
        self.server_build = match build {
            Ok((200, body)) => Some(String::from_utf8_lossy(&body).trim().to_string()),
            // A server from before `/build` existed: the protocol is all there is to go on.
            Ok((code, _)) => {
                info!("build probe answered HTTP {code}");
                None
            }
            Err(e) => {
                warn!("build probe failed: {e}");
                None
            }
        };
        let outdated = version != endif_sim::protocol_id();
        let newer = self.server_build.as_deref().is_some_and(|b| b != endif_sim::BUILD_ID);
        if outdated {
            warn!("server protocol {version} != ours {}: this build is out of date", endif_sim::protocol_id());
        } else if newer {
            info!("server build {} != ours {}: a newer build is available", self.server_build.as_deref().unwrap_or_default(), endif_sim::BUILD_ID);
        }
        if outdated || (newer && may_reload) {
            // Web: the page is stale; one reload per server build fetches the current one, so a
            // page that is still stale afterwards (deploy in progress) does not loop. No-op on desktop.
            let key = self.server_build.clone().unwrap_or(version);
            if crate::webclip::reload_for_update(Some(&key)) {
                info!("reloading the page for the current build");
            }
        }
        if outdated { SignalState::Outdated } else { SignalState::Up }
    }

    /// Desktop: takes the answer to the package version request, if it is in.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_package(&mut self, now: f64) {
        let (answer, started) = match &self.package_probe {
            Some((slot, started)) => (slot.lock().unwrap().clone(), *started),
            None => return,
        };
        let build = match answer {
            Some(Ok((200, body))) => PackageBuild::Known(String::from_utf8_lossy(&body).trim().to_string()),
            Some(Ok((code, _))) => {
                info!("package version answered HTTP {code}");
                PackageBuild::Unavailable
            }
            Some(Err(e)) => {
                warn!("package version failed: {e}");
                PackageBuild::Unavailable
            }
            None if now - started > PROBE_TIMEOUT => {
                warn!("package version timed out");
                PackageBuild::Unavailable
            }
            None => return,
        };
        self.package_probe = None;
        if build != self.package_build {
            info!("desktop package on the site: {build:?}");
            self.package_build = build;
        }
    }
}

/// How long a `/version`, `/build` or `/api/room` request may take before the server counts as unreachable.
const PROBE_TIMEOUT: f64 = 8.0;
/// Pause between probes while the server answers.
const PROBE_INTERVAL: f64 = 15.0;
/// Pause between probes after a failure.
const PROBE_RETRY: f64 = 5.0;
/// Pause before the presence socket is opened again after it dropped (or was refused).
const PRESENCE_RETRY: f64 = 20.0;

/// The socket that counts this client as online: `/presence` on the signaling server (see the
/// server's `rooms.rs`), opened when the app starts, kept for its whole life and reopened when it
/// drops. One per running client, in the menu or in a match alike, so nobody is counted twice.
#[derive(Resource, Default)]
pub struct Presence {
    socket: Option<WebRtcSocket>,
    loop_result: Option<LoopResult>,
    retry_at: f64,
}

/// A session waiting for the match resources to exist. `game::setup_match` moves it into the
/// real `Session` resource so GGRS never runs before the simulation state is in place.
#[derive(Resource)]
pub struct PendingSession(pub Option<Session<Config>>);

/// The local player's GGRS handle.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct LocalHandle(pub usize);

/// Seed for the simulation, shared by both peers.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct MatchSeed(pub u64);

/// Requests from the menu.
#[derive(Message, Debug, Clone)]
pub enum NetCommand {
    CreateRoom,
    JoinRoom(String),
    /// The competitive queue paired us: join the match's room.
    StartRanked(MatchInfo),
    /// The quick play queue paired us: join the room.
    StartQuick(QuickMatch),
    Practice,
    Leave,
}

/// Adapter that lets GGRS talk over a matchbox channel.
pub struct GgrsChannel(pub WebRtcChannel);

impl bevy_ggrs::ggrs::NonBlockingSocket<PeerId> for GgrsChannel {
    fn send_to(&mut self, msg: &bevy_ggrs::ggrs::Message, addr: &PeerId) {
        match bincode::serialize(msg) {
            Ok(bytes) => self.0.send(bytes.into_boxed_slice(), *addr),
            Err(e) => warn!("failed to serialize ggrs message: {e}"),
        }
    }

    fn receive_all_messages(&mut self) -> Vec<(PeerId, bevy_ggrs::ggrs::Message)> {
        self.0
            .receive()
            .into_iter()
            .filter_map(|(peer, packet)| match bincode::deserialize(&packet) {
                Ok(msg) => Some((peer, msg)),
                Err(e) => {
                    warn!("dropping malformed ggrs packet from {peer}: {e}");
                    None
                }
            })
            .collect()
    }
}

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NetCommand>()
            .init_resource::<LocalHandle>()
            .init_resource::<MatchSeed>()
            .init_resource::<SignalingStatus>()
            .init_resource::<PlayerNames>()
            .init_resource::<MatchExit>()
            .init_resource::<Presence>()
            .add_systems(Update, (handle_net_commands, keep_presence))
            .add_systems(Update, probe_signaling.run_if(in_state(AppState::Menu).or_else(in_state(AppState::Connecting))))
            .add_systems(OnEnter(AppState::Menu), |mut s: ResMut<SignalingStatus>| s.next_probe_at = 0.0)
            .add_systems(OnEnter(AppState::InGame), |mut s: ResMut<SignalingStatus>, mut e: ResMut<MatchExit>| {
                s.cancel();
                *e = MatchExit::default();
            })
            .add_systems(Update, poll_room.run_if(in_state(AppState::Connecting)))
            .add_systems(Update, watch_session.run_if(in_state(AppState::InGame)))
            .add_systems(OnExit(AppState::InGame), teardown_match);
    }
}

/// Opens a socket and runs its signaling loop in the background. The returned `LoopResult` is
/// filled in when that loop ends (connection refused, server gone, room refused...). A refused
/// handshake is not retried: a full room is still full three seconds later, and every attempt
/// counts against the server's per-address handshake budget.
fn open_socket(url: &str, cfg: &ClientConfig) -> (WebRtcSocket, LoopResult) {
    let ice = cfg.ice_servers();
    info!("connecting to signaling server {url} (ice: {})", ice.urls.join(" "));
    let (socket, loop_fut) = WebRtcSocket::builder(url)
        .ice_server(ice)
        .add_channel(ChannelConfig::unreliable())
        .add_channel(ChannelConfig::reliable())
        .reconnect_attempts(Some(1))
        .build();
    (socket, spawn_signaling(loop_fut))
}

/// Runs a socket's signaling loop in the background; the returned slot is filled when it ends.
fn spawn_signaling(loop_fut: MessageLoopFuture) -> LoopResult {
    let result: LoopResult = Arc::new(Mutex::new(None));
    let slot = result.clone();
    let run = async move {
        let r = loop_fut.await;
        if let Err(e) = &r {
            warn!("signaling loop ended: {e:?}");
        }
        *slot.lock().unwrap() = Some(r.map_err(|e| e.to_string()));
    };

    #[cfg(not(target_arch = "wasm32"))]
    IoTaskPool::get().spawn(async_compat::Compat::new(run)).detach();

    #[cfg(target_arch = "wasm32")]
    IoTaskPool::get().spawn_local(run).detach();

    result
}

/// Keeps the presence socket open. The server pings it and the browser (or the native websocket)
/// answers on its own, so it stays up in a background tab too; it only drops when the tab or the
/// app is closed, the connection dies or the server restarts, and is then reopened after a pause.
fn keep_presence(mut presence: ResMut<Presence>, status: Res<SignalingStatus>, cfg: Res<ClientConfig>, time: Res<Time<Real>>) {
    let now = time.elapsed_secs_f64();
    if presence.socket.is_some() {
        if presence.loop_result.as_ref().is_some_and(|r| r.lock().unwrap().is_some()) {
            info!("presence connection ended; reconnecting in {PRESENCE_RETRY}s");
            presence.socket = None;
            presence.loop_result = None;
            presence.retry_at = now + PRESENCE_RETRY;
        }
        return;
    }
    // An out-of-date build would only be turned away (426) until it has updated.
    if now < presence.retry_at || status.is_outdated() {
        return;
    }
    let (socket, loop_fut) = WebRtcSocket::builder(cfg.presence_url()).add_channel(ChannelConfig::reliable()).reconnect_attempts(Some(1)).build();
    presence.loop_result = Some(spawn_signaling(loop_fut));
    presence.socket = Some(socket);
    presence.retry_at = now + PRESENCE_RETRY;
}

/// Polls `GET /version` and `GET /build` together while in the menus: the protocol identity
/// means the server is up (and tells a stale simulation from a current one); the build identity
/// tells whether a newer build exists at all. An error, another status or no answer within
/// `PROBE_TIMEOUT` means it is down, in which case the next probe comes sooner. Desktop: while an
/// update is pending, also asks the site which build its package is.
fn probe_signaling(
    mut status: ResMut<SignalingStatus>,
    cfg: Res<ClientConfig>,
    time: Res<Time<Real>>,
    state: Res<State<AppState>>,
    mut refresh: ResMut<UiRefresh>,
) {
    let now = time.elapsed_secs_f64();
    let before = status.ui_key();
    let in_flight = status.probe.as_ref().map(|(v, b, started)| (v.lock().unwrap().clone(), b.lock().unwrap().clone(), *started));
    let new_state = match in_flight {
        Some((Some(version), Some(build), _)) => Some(status.settle(version, build, *state.get() == AppState::Menu)),
        Some((_, _, started)) if now - started > PROBE_TIMEOUT => {
            warn!("version probe timed out");
            Some(SignalState::Down)
        }
        Some(_) => None,
        None => {
            if now >= status.next_probe_at {
                status.probe = Some((http_get(&cfg.version_url()), http_get(&cfg.build_url()), now));
            }
            None
        }
    };
    if let Some(s) = new_state {
        status.probe = None;
        status.next_probe_at = now + if s == SignalState::Up { PROBE_INTERVAL } else { PROBE_RETRY };
        if s != status.state {
            info!("matchmaking server: {s:?}");
            status.state = s;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if status.update_available() && !status.package_ready() && status.package_probe.is_none() {
            status.package_probe = Some((http_get(&cfg.package_version_url()), now));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    status.poll_package(now);
    if status.ui_key() != before {
        refresh.0 = true;
    }
}

fn handle_net_commands(
    mut commands: Commands,
    mut cmds: MessageReader<NetCommand>,
    cfg: Res<ClientConfig>,
    account: Res<crate::account::Account>,
    status: Res<SignalingStatus>,
    mut names: ResMut<PlayerNames>,
    mut next: ResMut<NextState<AppState>>,
) {
    for cmd in cmds.read() {
        match cmd {
            NetCommand::CreateRoom => {
                if status.is_down() {
                    warn!("matchmaking server unreachable; not creating a room");
                    continue;
                }
                let code = crate::config::generate_room_code();
                start_room(&mut commands, &cfg, code.clone(), MatchKind::Room { code }, &mut next);
            }
            NetCommand::JoinRoom(code) => {
                if code.len() != crate::config::ROOM_CODE_LEN {
                    warn!("room code must be {} characters", crate::config::ROOM_CODE_LEN);
                    continue;
                }
                if status.is_down() {
                    warn!("matchmaking server unreachable; not joining a room");
                    continue;
                }
                start_room(&mut commands, &cfg, code.clone(), MatchKind::Room { code: code.clone() }, &mut next);
            }
            NetCommand::StartRanked(info) => {
                if status.is_down() {
                    warn!("matchmaking server unreachable; not joining the ranked room");
                    continue;
                }
                start_room(&mut commands, &cfg, info.room.clone(), MatchKind::Ranked(info.clone()), &mut next);
            }
            NetCommand::StartQuick(info) => {
                if status.is_down() {
                    warn!("matchmaking server unreachable; not joining the quick play room");
                    continue;
                }
                start_room(&mut commands, &cfg, info.room.clone(), MatchKind::Quick(info.clone()), &mut next);
            }
            NetCommand::Practice => {
                let session = SessionBuilder::<Config>::new()
                    .with_num_players(2)
                    .expect("num players")
                    .with_check_distance(2)
                    .add_player(PlayerType::Local, 0)
                    .expect("player 0")
                    .add_player(PlayerType::Local, 1)
                    .expect("player 1")
                    .start_synctest_session()
                    .expect("synctest session");
                names.0 = [account.display_name(), "DUMMY".to_string()];
                commands.insert_resource(PendingSession(Some(Session::SyncTest(session))));
                commands.insert_resource(LocalHandle(0));
                commands.insert_resource(MatchSeed(0xE11D1F));
                commands.insert_resource(MatchKind::Practice);
                next.set(AppState::InGame);
            }
            NetCommand::Leave => {
                next.set(AppState::Menu);
            }
        }
    }
}

fn start_room(commands: &mut Commands, cfg: &ClientConfig, code: String, kind: MatchKind, next: &mut NextState<AppState>) {
    let (socket, loop_result) = open_socket(&cfg.room_url(&code), cfg);
    commands.insert_resource(MatchSeed(seed_from_room(&code)));
    commands.insert_resource(kind);
    commands.insert_resource(RoomConnection { code, socket: Some(socket), loop_result, connected: false, started: false, failure: None, accepted_at: None, check: None });
    next.set(AppState::Connecting);
}

/// What a signaling loop that ended before the match started means. Desktop websockets report the
/// HTTP status of a refused handshake (401 full, 426 protocol mismatch, 429 rate limited); a
/// browser reports nothing, so unless the server is known to be down the room is asked.
fn classify_failure(detail: &str, status: &SignalingStatus) -> RoomFailure {
    if status.is_outdated() || detail.contains("426") {
        RoomFailure::Outdated
    } else if detail.contains("401") {
        RoomFailure::Full
    } else if detail.contains("429") {
        RoomFailure::Refused
    } else if status.state == SignalState::Down {
        RoomFailure::Unreachable
    } else {
        RoomFailure::Checking
    }
}

/// Outcome of the `/api/room/<code>` lookup, once it has one.
fn check_result(slot: &HttpSlot, started: f64, now: f64) -> Option<RoomFailure> {
    let res = slot.lock().unwrap().clone();
    match res {
        Some(Ok((200, body))) => {
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let peers = v["peers"].as_u64().unwrap_or(0);
            let max = v["max"].as_u64().unwrap_or(2);
            info!("room has {peers}/{max} peers");
            Some(if peers >= max { RoomFailure::Full } else { RoomFailure::Refused })
        }
        Some(Ok((code, _))) => {
            warn!("room lookup answered HTTP {code}");
            Some(RoomFailure::Refused)
        }
        Some(Err(e)) => {
            warn!("room lookup failed: {e}");
            Some(RoomFailure::Unreachable)
        }
        None if now - started > PROBE_TIMEOUT => {
            warn!("room lookup timed out");
            Some(RoomFailure::Unreachable)
        }
        None => None,
    }
}

/// Waits for exactly one other peer, then starts the GGRS P2P session.
#[allow(clippy::too_many_arguments)]
fn poll_room(
    mut commands: Commands,
    room: Option<ResMut<RoomConnection>>,
    status: Res<SignalingStatus>,
    cfg: Res<ClientConfig>,
    kind: Option<Res<MatchKind>>,
    account: Res<crate::account::Account>,
    time: Res<Time<Real>>,
    mut names: ResMut<PlayerNames>,
    mut next: ResMut<NextState<AppState>>,
) {
    let Some(mut room) = room else { return };
    let room: &mut RoomConnection = &mut room;
    let now = time.elapsed_secs_f64();
    if room.started {
        return;
    }
    if let Some(failure) = &room.failure {
        if *failure == RoomFailure::Checking
            && let Some((slot, started)) = &room.check
            && let Some(resolved) = check_result(slot, *started, now)
        {
            room.failure = Some(resolved);
            room.check = None;
        }
        return;
    }
    let Some(socket) = room.socket.as_mut() else { return };

    // The signaling loop ending before the session started means the server refused us (room
    // full, rate limited) or cannot be reached / went away while we were waiting.
    let loop_ended = room.loop_result.lock().unwrap().clone();
    let updates = match (loop_ended, socket.try_update_peers()) {
        (None, Ok(u)) => u,
        (ended, res) => {
            let detail = match (&ended, &res) {
                (Some(Err(e)), _) => e.clone(),
                (_, Err(e)) => e.to_string(),
                _ => "signaling connection closed".to_string(),
            };
            let failure = classify_failure(&detail, &status);
            warn!("lobby for room {} ended ({detail}): {failure:?}", room.code);
            if failure == RoomFailure::Checking {
                room.check = Some((http_get(&format!("{}/api/room/{}", cfg.api_url(), room.code)), now));
            }
            room.failure = Some(failure);
            return;
        }
    };
    for (peer, state) in updates {
        match state {
            PeerState::Connected => info!("peer {peer} joined the room (data channel open)"),
            PeerState::Disconnected => info!("peer {peer} left the room"),
        }
    }

    let Some(our_id) = socket.id() else { return };
    if !room.connected {
        room.connected = true;
        room.accepted_at = Some(now);
        info!("signaling accepted us in room endif-{} as {our_id}; waiting for the other player", room.code);
    }
    let mut ids: Vec<PeerId> = socket.connected_peers().collect();
    if ids.is_empty() {
        // The browser gave up on every candidate pair; matchbox would wait forever.
        if room.peer_status().as_deref() == Some("failed") {
            warn!("WebRTC connection to the opponent failed (ICE failed); leaving room {}", room.code);
            room.socket = None;
            room.failure = Some(RoomFailure::PeerUnreachable);
            return;
        }
        if room.accepted_at.is_some_and(|t| now - t > LOBBY_TIMEOUT) {
            warn!("nobody joined room {} for {LOBBY_TIMEOUT_MINUTES} minutes; leaving it", room.code);
            // Dropping the socket closes the signaling connection and frees our slot right away.
            room.socket = None;
            room.failure = Some(RoomFailure::Timeout);
        }
        return;
    }
    if ids.len() > 1 {
        room.failure = Some(RoomFailure::Full);
        return;
    }
    let remote_id = ids[0];
    ids.push(our_id);
    ids.sort();

    let mut builder = SessionBuilder::<Config>::new()
        .with_num_players(2)
        .expect("num players")
        .with_fps(ROLLBACK_FPS)
        .expect("fps")
        .with_input_delay(INPUT_DELAY)
        .with_max_prediction_window(MAX_PREDICTION)
        .with_disconnect_timeout(DISCONNECT_TIMEOUT)
        .with_disconnect_notify_delay(DISCONNECT_NOTIFY_START)
        .with_desync_detection_mode(DesyncDetection::On { interval: 30 });

    let mut local_handle = 0;
    for (handle, id) in ids.iter().enumerate() {
        let ty = if *id == our_id {
            local_handle = handle;
            PlayerType::Local
        } else {
            PlayerType::Remote(*id)
        };
        builder = builder.add_player(ty, handle).expect("add player");
    }

    // Names: ours, and the opponent's from the server for matchmade games (a private room's
    // opponent introduces themselves over the reliable channel once the session runs).
    let my_name = account.display_name();
    let their_name = match kind.as_deref() {
        Some(MatchKind::Ranked(info)) => info.opponent.clone(),
        Some(MatchKind::Quick(info)) => info.opponent.clone(),
        _ => String::new(),
    };
    names.0 = if local_handle == 0 { [my_name.clone(), their_name] } else { [their_name, my_name.clone()] };
    let mut hello = vec![MSG_HELLO];
    hello.extend_from_slice(my_name.as_bytes());
    socket.channel_mut(CHAT_CHANNEL).send(hello.into_boxed_slice(), remote_id);

    let channel = match socket.take_channel(GAME_CHANNEL) {
        Ok(c) => c,
        Err(e) => {
            warn!("no data channel: {e:?}");
            room.failure = Some(RoomFailure::Refused);
            return;
        }
    };

    match builder.start_p2p_session(GgrsChannel(channel)) {
        Ok(session) => {
            info!("starting p2p session as player {local_handle}");
            commands.insert_resource(PendingSession(Some(Session::P2P(session))));
            commands.insert_resource(LocalHandle(local_handle));
            room.started = true;
            next.set(AppState::InGame);
        }
        Err(e) => {
            warn!("failed to start session: {e}");
            room.failure = Some(RoomFailure::Refused);
        }
    }
}

/// Logs GGRS events and drops back to the menu when the peer disconnects.
#[allow(clippy::too_many_arguments)]
fn watch_session(
    session: Option<ResMut<Session<Config>>>,
    room: Option<ResMut<RoomConnection>>,
    local: Res<LocalHandle>,
    mut names: ResMut<PlayerNames>,
    mut exit: ResMut<MatchExit>,
    mut next: ResMut<NextState<AppState>>,
    mut status: ResMut<crate::hud::NetStatus>,
) {
    // matchbox reports the WebRTC connection itself going away (tab closed, ICE failed) without
    // waiting out the GGRS silence timeout. Errors here mean the signaling connection is gone,
    // which does not matter once the peers talk directly.
    if let Some(mut room) = room
        && let Some(socket) = room.socket.as_mut()
    {
        if let Ok(updates) = socket.try_update_peers() {
            for (peer, state) in updates {
                if matches!(state, PeerState::Disconnected) {
                    warn!("peer {peer} left (WebRTC connection closed)");
                    status.text = "opponent left".to_string();
                    exit.opponent_left = true;
                    next.set(AppState::Menu);
                }
            }
        }
        for (peer, packet) in socket.channel_mut(CHAT_CHANNEL).receive() {
            match packet.split_first() {
                Some((&MSG_HELLO, name)) => {
                    let name: String = String::from_utf8_lossy(name).chars().filter(|c| !c.is_control()).take(crate::account::NAME_MAX).collect();
                    info!("peer {peer} is {name:?}");
                    names.0[1 - local.0] = name;
                }
                _ => debug!("ignoring unknown message from {peer} ({} bytes)", packet.len()),
            }
        }
    }
    let Some(mut session) = session else { return };
    if let Session::P2P(s) = session.as_mut() {
        let handle_remote = s.remote_player_handles().first().copied();
        if let Some(h) = handle_remote
            && let Ok(stats) = s.network_stats(h)
        {
            status.ping_ms = stats.ping as u32;
            status.frames_ahead = s.frames_ahead();
        }
        for ev in s.events() {
            match ev {
                GgrsEvent::Synchronizing { total, count, .. } => {
                    info!("ggrs synchronizing {count}/{total}");
                    status.text = format!("synchronizing {count}/{total}");
                }
                GgrsEvent::Synchronized { .. } => {
                    info!("ggrs synchronized");
                    status.text = "connected".to_string();
                }
                GgrsEvent::NetworkInterrupted { disconnect_timeout, .. } => {
                    warn!("nothing from the peer for {DISCONNECT_NOTIFY_START:?}; dropping them in {disconnect_timeout} ms");
                    status.text = "connection interrupted".to_string();
                }
                GgrsEvent::NetworkResumed { .. } => {
                    info!("peer traffic resumed");
                    status.text = "connected".to_string();
                }
                GgrsEvent::Disconnected { .. } => {
                    warn!("peer disconnected");
                    status.text = "opponent left".to_string();
                    exit.opponent_left = true;
                    next.set(AppState::Menu);
                }
                GgrsEvent::DesyncDetected { frame, local_checksum, remote_checksum, .. } => {
                    error!("DESYNC at frame {frame}: local {local_checksum:#x} remote {remote_checksum:#x}");
                    status.text = format!("desync at frame {frame}!");
                }
                GgrsEvent::WaitRecommendation { .. } => {}
            }
        }
    }
}

pub(crate) fn teardown_match(mut commands: Commands, entities: Query<Entity, With<GameEntity>>) {
    commands.remove_resource::<Session<Config>>();
    commands.remove_resource::<PendingSession>();
    commands.remove_resource::<RoomConnection>();
    commands.remove_resource::<MatchKind>();
    commands.insert_resource(LocalPlayers::default());
    // Children of match entities (rocket models, lights) carry the marker too and are already
    // gone with their parent by the time their own command runs.
    for e in &entities {
        commands.entity(e).try_despawn();
    }
}
