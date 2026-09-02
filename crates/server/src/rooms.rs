//! Room occupancy, presence and the signaling topology.
//!
//! matchbox's own full-mesh topology introduces every peer in a room to every other peer but never
//! limits a room's size, never notices a peer whose connection silently died, and refuses nothing.
//! [`EndifMesh`] is that topology with these additions:
//!
//! * a room takes at most `max` peers: the handshake hook (`on_connection_request` in `main.rs`)
//!   reserves a slot before the websocket upgrade so two simultaneous joins cannot both slip in,
//!   and the state machine confirms it once the socket is really open;
//! * a slot reserved for a socket that never opens (the client vanished between the handshake and
//!   the upgrade) is released again by [`Rooms::sweep`] after a grace period, so a dropped
//!   handshake cannot hold a room "full" until the server restarts;
//! * a peer that sends nothing for [`IDLE_TIMEOUT`] is dropped. The server pings every socket every
//!   [`PING_INTERVAL`]; a browser answers those from its networking stack even for a tab in the
//!   background (whose own keep-alives get throttled), so only a connection whose other end is
//!   really gone (laptop lid, lost Wi-Fi, no FIN ever sent) trips this, instead of holding its
//!   slot until the kernel gives up on the TCP connection hours later;
//! * the path [`PRESENCE_PATH`] is not a room: every client keeps one socket to it open for as long
//!   as it runs, and the number of those sockets is the "players online" count. Nobody is
//!   introduced to anybody there, and the sockets take no room slots;
//! * rooms the matchmaker created are tagged with their kind ([`Rooms::tag`]), so the activity
//!   counts can tell a competitive game from a quick play game from a private room.
//!
//! `GET /api/room/<code>` (`api.rs`) reports a room's occupancy so a browser client, which cannot
//! see why a websocket handshake was refused, can tell "the room is full" from "you were rate
//! limited" or "the server is gone".

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures::StreamExt;
use futures::stream::SplitStream;
use matchbox_protocol::{JsonPeerEvent, JsonPeerRequest, PeerRequest};
use matchbox_signaling::common_logic::{SignalingChannel, parse_request, try_send};
use matchbox_signaling::topologies::full_mesh::FullMeshState;
use matchbox_signaling::{ClientRequestError, NoCallbacks, SignalingState, SignalingTopology, WsStateMeta};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// How long a reserved slot may wait for its socket to open before it is given back.
pub const ADMISSION_GRACE: Duration = Duration::from_secs(60);
/// Silence after which a peer is dropped (the server pings every [`PING_INTERVAL`]).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How often every socket is pinged.
pub const PING_INTERVAL: Duration = Duration::from_secs(20);
/// A room tag that nobody ever occupied is forgotten after this long.
const TAG_TTL: Duration = Duration::from_secs(60 * 60);
/// Websocket close code sent to a peer that lost the race for the last slot.
const CLOSE_ROOM_FULL: u16 = 4001;
/// The websocket path that counts as "online" rather than joining a room.
pub const PRESENCE_PATH: &str = "presence";

/// How a room came to be, for the activity counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomKind {
    /// Created by a player with "create private room" (or any code the server never issued).
    Private,
    /// Issued by the competitive queue.
    Ranked,
    /// Issued by the quick play queue.
    Quick,
}

/// Players in a game, by the kind of room they are in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Playing {
    pub ranked: usize,
    pub quick: usize,
    pub private: usize,
}

struct Peer {
    room: String,
    /// The websocket is open and the peer is in matchbox's mesh.
    connected: bool,
    since: Instant,
}

/// Who is in which room, and who is online at all.
pub struct Rooms {
    max: usize,
    /// Handshakes accepted but not yet assigned a peer id.
    pending: HashMap<SocketAddr, (String, Instant)>,
    /// Every peer holding a slot, connected or still upgrading.
    by_peer: HashMap<String, Peer>,
    /// Slots taken per room.
    counts: HashMap<String, usize>,
    /// Rooms the matchmaker issued (path form), with the kind and when they were issued.
    kinds: HashMap<String, (RoomKind, Instant)>,
    /// Presence handshakes accepted but not yet assigned a peer id.
    presence_pending: HashMap<SocketAddr, Instant>,
    /// Presence sockets that are open: one per running client.
    presence: HashSet<String>,
}

pub type SharedRooms = Arc<Mutex<Rooms>>;

impl Rooms {
    pub fn new(max: usize) -> Rooms {
        Rooms {
            max: max.max(1),
            pending: HashMap::new(),
            by_peer: HashMap::new(),
            counts: HashMap::new(),
            kinds: HashMap::new(),
            presence_pending: HashMap::new(),
            presence: HashSet::new(),
        }
    }

    pub fn max(&self) -> usize {
        self.max
    }

    /// Slots taken in `room` (path form, e.g. `endif-ABCDEF`).
    pub fn occupancy(&self, room: &str) -> usize {
        self.counts.get(room).copied().unwrap_or(0)
    }

    /// Remembers that the matchmaker issued `room` (path form) for a game of `kind`. Untagged
    /// rooms are private.
    pub fn tag(&mut self, room: &str, kind: RoomKind) {
        self.kinds.insert(room.to_string(), (kind, Instant::now()));
    }

    fn kind(&self, room: &str) -> RoomKind {
        self.kinds.get(room).map(|(k, _)| *k).unwrap_or(RoomKind::Private)
    }

    /// Players in a game: connected peers in rooms holding at least two of them, by room kind. A
    /// peer alone in its room is waiting for an opponent, not playing.
    pub fn playing(&self) -> Playing {
        let mut per_room: HashMap<&str, usize> = HashMap::new();
        for p in self.by_peer.values().filter(|p| p.connected) {
            *per_room.entry(p.room.as_str()).or_default() += 1;
        }
        let mut out = Playing::default();
        for (room, n) in per_room.into_iter().filter(|&(_, n)| n >= 2) {
            match self.kind(room) {
                RoomKind::Ranked => out.ranked += n,
                RoomKind::Quick => out.quick += n,
                RoomKind::Private => out.private += n,
            }
        }
        out
    }

    /// Clients connected right now (open presence sockets), whatever they are doing.
    pub fn online(&self) -> usize {
        self.presence.len()
    }

    /// Reserves a slot for a handshake from `origin`. `Ok(taken)` with the new occupancy, or
    /// `Err(taken)` when the room is full.
    pub fn admit(&mut self, origin: SocketAddr, room: &str) -> Result<usize, usize> {
        let taken = self.occupancy(room);
        if taken >= self.max {
            return Err(taken);
        }
        *self.counts.entry(room.to_string()).or_default() += 1;
        self.pending.insert(origin, (room.to_string(), Instant::now()));
        Ok(taken + 1)
    }

    /// Accepts a presence handshake from `origin`: no slot, just the bookkeeping that lets
    /// `assign` tell it from a room handshake.
    pub fn admit_presence(&mut self, origin: SocketAddr) {
        self.presence_pending.insert(origin, Instant::now());
    }

    /// Moves the reservation of `origin` over to its freshly assigned peer id. Returns the room
    /// (or [`PRESENCE_PATH`]) the handshake was for.
    pub fn assign(&mut self, origin: SocketAddr, peer: &str) -> Option<String> {
        if self.presence_pending.remove(&origin).is_some() {
            return Some(PRESENCE_PATH.to_string());
        }
        let (room, since) = self.pending.remove(&origin)?;
        self.by_peer.insert(peer.to_string(), Peer { room: room.clone(), connected: false, since });
        Some(room)
    }

    /// The socket of `peer` is open. Returns the room's occupancy, or `None` if the reservation is
    /// gone (swept, or never made).
    pub fn connect(&mut self, peer: &str) -> Option<usize> {
        let p = self.by_peer.get_mut(peer)?;
        p.connected = true;
        let room = p.room.clone();
        Some(self.occupancy(&room))
    }

    /// Takes a slot for a peer whose reservation was swept before its socket opened.
    pub fn readmit(&mut self, peer: &str, room: &str) -> Option<usize> {
        let taken = self.occupancy(room);
        if taken >= self.max {
            return None;
        }
        *self.counts.entry(room.to_string()).or_default() += 1;
        self.by_peer.insert(peer.to_string(), Peer { room: room.to_string(), connected: true, since: Instant::now() });
        Some(taken + 1)
    }

    /// Releases the slot of `peer`; returns the room it was in.
    pub fn disconnect(&mut self, peer: &str) -> Option<String> {
        let p = self.by_peer.remove(peer)?;
        self.leave(&p.room);
        Some(p.room)
    }

    /// A presence socket opened. Returns how many are online now.
    pub fn presence_connect(&mut self, peer: &str) -> usize {
        self.presence.insert(peer.to_string());
        self.presence.len()
    }

    /// A presence socket closed. Returns how many are online now.
    pub fn presence_disconnect(&mut self, peer: &str) -> usize {
        self.presence.remove(peer);
        self.presence.len()
    }

    /// Releases reservations whose socket never opened within [`ADMISSION_GRACE`] and forgets
    /// room tags nobody used. Returns how many reservations were released.
    pub fn sweep(&mut self) -> usize {
        let now = Instant::now();
        let mut released = 0;
        let stale: Vec<SocketAddr> = self.pending.iter().filter(|(_, (_, t))| now.duration_since(*t) > ADMISSION_GRACE).map(|(a, _)| *a).collect();
        for origin in stale {
            if let Some((room, _)) = self.pending.remove(&origin) {
                warn!(%origin, %room, "handshake never became a peer; releasing its slot");
                self.leave(&room);
                released += 1;
            }
        }
        let stale: Vec<String> = self.by_peer.iter().filter(|(_, p)| !p.connected && now.duration_since(p.since) > ADMISSION_GRACE).map(|(id, _)| id.clone()).collect();
        for id in stale {
            if let Some(p) = self.by_peer.remove(&id) {
                warn!(peer = %id, room = %p.room, "socket never opened; releasing its slot");
                self.leave(&p.room);
                released += 1;
            }
        }
        self.presence_pending.retain(|_, t| now.duration_since(*t) <= ADMISSION_GRACE);
        // A tag outlives its room's last player by up to an hour, which is harmless: an empty room
        // counts for nothing. A room still in use keeps its tag however long the game runs.
        let counts = &self.counts;
        self.kinds.retain(|room, (_, t)| counts.contains_key(room) || now.duration_since(*t) <= TAG_TTL);
        released
    }

    fn leave(&mut self, room: &str) {
        if let Some(n) = self.counts.get_mut(room) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.counts.remove(room);
            }
        }
    }
}

/// State handed to every socket's state machine: matchbox's mesh plus our occupancy.
#[derive(Clone)]
pub struct EndifState {
    pub mesh: FullMeshState,
    pub rooms: SharedRooms,
}

impl SignalingState for EndifState {}

/// Full mesh with room limits, an idle timeout and the presence path (see the module docs).
#[derive(Debug, Default)]
pub struct EndifMesh;

#[async_trait]
impl SignalingTopology<NoCallbacks, EndifState> for EndifMesh {
    /// Runs once per websocket, from the moment it is open until it closes.
    async fn state_machine(upgrade: WsStateMeta<NoCallbacks, EndifState>) {
        let WsStateMeta { room, peer_id, sender, receiver, mut state, .. } = upgrade;
        let id = peer_id.to_string();

        if room == PRESENCE_PATH {
            let online = state.rooms.lock().unwrap().presence_connect(&id);
            info!(%peer_id, online, "client online");
            let why = read_loop(receiver, &sender, |_| {}).await;
            let online = state.rooms.lock().unwrap().presence_disconnect(&id);
            info!(%peer_id, online, "client offline ({why})");
            return;
        }

        // Confirm the slot reserved at the handshake; take a fresh one if the reservation was
        // swept in the meantime, or turn the peer away if that is no longer possible.
        let occupancy = {
            let mut rooms = state.rooms.lock().unwrap();
            match rooms.connect(&id) {
                Some(n) => Some(n),
                None => rooms.readmit(&id, &room),
            }
        };
        let Some(occupancy) = occupancy else {
            warn!(%peer_id, %room, "refused after the upgrade: room is full");
            let _ = try_send(&sender, Message::Close(Some(CloseFrame { code: CLOSE_ROOM_FULL, reason: "room is full".into() })));
            return;
        };
        info!(%peer_id, %room, occupancy, "peer connected");
        state.mesh.add_peer(peer_id, sender.clone(), room.clone());

        let why = read_loop(receiver, &sender, |request| match request {
            PeerRequest::Signal { receiver, data } => {
                let event = Message::Text(JsonPeerEvent::Signal { sender: peer_id, data }.to_string().into());
                if let Err(e) = state.mesh.try_send_to_peer(receiver, event, room.clone()) {
                    error!(%peer_id, %room, "error forwarding a signal: {e:?}");
                }
            }
            // Keep-alives only exist to reset the idle timeout (and to keep reverse proxies from
            // cutting a quiet socket).
            PeerRequest::KeepAlive => {}
        })
        .await;

        state.mesh.remove_peer(&peer_id);
        let left = state.rooms.lock().unwrap().disconnect(&id);
        info!(%peer_id, %room, released = left.is_some(), "peer disconnected ({why})");
    }
}

/// Reads a socket until it closes or falls silent, pinging it every [`PING_INTERVAL`]; anything
/// that arrives, a pong included, counts as a sign of life. Parsed requests go to `on_request`.
/// Returns why the loop ended, for the log.
async fn read_loop(mut receiver: SplitStream<WebSocket>, sender: &SignalingChannel, mut on_request: impl FnMut(JsonPeerRequest)) -> String {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await; // the first tick completes at once
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if heard.elapsed() > IDLE_TIMEOUT {
                    let _ = try_send(sender, Message::Close(None));
                    return format!("nothing heard for {}s", IDLE_TIMEOUT.as_secs());
                }
                if try_send(sender, Message::Ping(Bytes::new())).is_err() {
                    return "socket gone".to_string();
                }
            }
            next = receiver.next() => {
                let Some(message) = next else { return "connection closed".to_string() };
                heard = Instant::now();
                match message {
                    // A pong answers our ping; a ping is answered by axum itself.
                    Ok(Message::Pong(_) | Message::Ping(_)) => {}
                    other => match parse_request(other) {
                        Ok(request) => on_request(request),
                        Err(ClientRequestError::Json(_) | ClientRequestError::UnsupportedType(_)) => error!("ignoring a malformed request"),
                        Err(ClientRequestError::Close) => return "closed by the peer".to_string(),
                        Err(e) => return format!("connection lost: {e:?}"),
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn admission_is_capped() {
        let mut r = Rooms::new(2);
        assert_eq!(r.admit(addr(1), "endif-A"), Ok(1));
        assert_eq!(r.admit(addr(2), "endif-A"), Ok(2));
        assert_eq!(r.admit(addr(3), "endif-A"), Err(2));
        assert_eq!(r.assign(addr(1), "p1"), Some("endif-A".to_string()));
        assert_eq!(r.connect("p1"), Some(2));
        assert_eq!(r.disconnect("p1"), Some("endif-A".to_string()));
        assert_eq!(r.occupancy("endif-A"), 1);
        assert_eq!(r.admit(addr(3), "endif-A"), Ok(2));
    }

    #[test]
    fn sweep_releases_only_stale_reservations() {
        let mut r = Rooms::new(2);
        r.admit(addr(1), "endif-A").unwrap();
        r.admit(addr(2), "endif-A").unwrap();
        r.assign(addr(2), "p2");
        r.connect("p2");
        assert_eq!(r.sweep(), 0);
        // Age the pending handshake and the unassigned peer past the grace period.
        r.pending.get_mut(&addr(1)).unwrap().1 = Instant::now() - ADMISSION_GRACE * 2;
        assert_eq!(r.sweep(), 1);
        assert_eq!(r.occupancy("endif-A"), 1);
        r.by_peer.get_mut("p2").unwrap().since = Instant::now() - ADMISSION_GRACE * 2;
        assert_eq!(r.sweep(), 0, "connected peers are never swept");
    }

    #[test]
    fn playing_counts_full_rooms_only_by_kind() {
        let mut r = Rooms::new(2);
        r.tag("endif-A", RoomKind::Ranked);
        r.admit(addr(1), "endif-A").unwrap();
        r.admit(addr(2), "endif-A").unwrap();
        r.admit(addr(3), "endif-B").unwrap();
        r.assign(addr(1), "p1");
        r.assign(addr(2), "p2");
        r.assign(addr(3), "p3");
        r.connect("p1");
        r.connect("p3");
        assert_eq!(r.playing(), Playing::default(), "one connected peer per room: nobody is playing yet");
        r.connect("p2");
        assert_eq!(r.playing(), Playing { ranked: 2, quick: 0, private: 0 });
        r.admit(addr(4), "endif-B").unwrap();
        r.assign(addr(4), "p4");
        r.connect("p4");
        assert_eq!(r.playing(), Playing { ranked: 2, quick: 0, private: 2 }, "an untagged room is private");
        r.disconnect("p1");
        assert_eq!(r.playing(), Playing { ranked: 0, quick: 0, private: 2 });
    }

    #[test]
    fn tags_outlive_their_rooms_but_not_forever() {
        let mut r = Rooms::new(2);
        r.tag("endif-Q", RoomKind::Quick);
        r.tag("endif-R", RoomKind::Quick);
        r.admit(addr(1), "endif-Q").unwrap();
        r.kinds.get_mut("endif-Q").unwrap().1 = Instant::now() - TAG_TTL * 2;
        r.kinds.get_mut("endif-R").unwrap().1 = Instant::now() - TAG_TTL * 2;
        r.sweep();
        assert_eq!(r.kind("endif-Q"), RoomKind::Quick, "an occupied room keeps its tag");
        assert_eq!(r.kind("endif-R"), RoomKind::Private, "an old, empty tag is forgotten");
    }

    #[test]
    fn presence_takes_no_slot() {
        let mut r = Rooms::new(1);
        r.admit_presence(addr(9));
        assert_eq!(r.assign(addr(9), "w1"), Some(PRESENCE_PATH.to_string()));
        assert_eq!(r.presence_connect("w1"), 1);
        assert_eq!(r.presence_connect("w2"), 2);
        assert_eq!(r.online(), 2);
        assert_eq!(r.occupancy(PRESENCE_PATH), 0);
        assert_eq!(r.playing(), Playing::default());
        assert_eq!(r.presence_disconnect("w1"), 1);
        assert_eq!(r.presence_disconnect("w1"), 1, "a second disconnect changes nothing");
        r.admit_presence(addr(10));
        r.presence_pending.insert(addr(10), Instant::now() - ADMISSION_GRACE * 2);
        r.sweep();
        assert_eq!(r.assign(addr(10), "w3"), None, "a stale presence handshake is forgotten");
    }

    #[test]
    fn readmit_after_a_sweep() {
        let mut r = Rooms::new(1);
        assert_eq!(r.connect("ghost"), None);
        assert_eq!(r.readmit("ghost", "endif-B"), Some(1));
        assert_eq!(r.readmit("other", "endif-B"), None);
        assert_eq!(r.disconnect("ghost"), Some("endif-B".to_string()));
        assert_eq!(r.occupancy("endif-B"), 0);
    }
}
