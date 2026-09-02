//! Room occupancy and the signaling topology.
//!
//! matchbox's own full-mesh topology introduces every peer in a room to every other peer but never
//! limits a room's size, never notices a peer whose connection silently died, and refuses nothing.
//! [`EndifMesh`] is that topology with three additions:
//!
//! * a room takes at most `max` peers: the handshake hook (`on_connection_request` in `main.rs`)
//!   reserves a slot before the websocket upgrade so two simultaneous joins cannot both slip in,
//!   and the state machine confirms it once the socket is really open;
//! * a slot reserved for a socket that never opens (the client vanished between the handshake and
//!   the upgrade) is released again by [`Rooms::sweep`] after a grace period, so a dropped
//!   handshake cannot hold a room "full" until the server restarts;
//! * a peer that sends nothing for [`IDLE_TIMEOUT`] is dropped. Clients send a keep-alive every
//!   10 s, so only a connection whose other end is gone (laptop lid, lost Wi-Fi, no FIN ever sent)
//!   trips this, instead of holding its slot until the kernel gives up on the TCP connection hours
//!   later.
//!
//! `GET /api/room/<code>` (`api.rs`) reports a room's occupancy so a browser client, which cannot
//! see why a websocket handshake was refused, can tell "the room is full" from "you were rate
//! limited" or "the server is gone".

use async_trait::async_trait;
use axum::extract::ws::{CloseFrame, Message};
use futures::StreamExt;
use matchbox_protocol::{JsonPeerEvent, PeerRequest};
use matchbox_signaling::common_logic::{parse_request, try_send};
use matchbox_signaling::topologies::full_mesh::FullMeshState;
use matchbox_signaling::{ClientRequestError, NoCallbacks, SignalingState, SignalingTopology, WsStateMeta};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// How long a reserved slot may wait for its socket to open before it is given back.
pub const ADMISSION_GRACE: Duration = Duration::from_secs(60);
/// Silence after which a peer is dropped (clients keep-alive every 10 s).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Websocket close code sent to a peer that lost the race for the last slot.
const CLOSE_ROOM_FULL: u16 = 4001;

struct Peer {
    room: String,
    /// The websocket is open and the peer is in matchbox's mesh.
    connected: bool,
    since: Instant,
}

/// Who is in which room.
pub struct Rooms {
    max: usize,
    /// Handshakes accepted but not yet assigned a peer id.
    pending: HashMap<SocketAddr, (String, Instant)>,
    /// Every peer holding a slot, connected or still upgrading.
    by_peer: HashMap<String, Peer>,
    /// Slots taken per room.
    counts: HashMap<String, usize>,
}

pub type SharedRooms = Arc<Mutex<Rooms>>;

impl Rooms {
    pub fn new(max: usize) -> Rooms {
        Rooms { max: max.max(1), pending: HashMap::new(), by_peer: HashMap::new(), counts: HashMap::new() }
    }

    pub fn max(&self) -> usize {
        self.max
    }

    /// Slots taken in `room` (path form, e.g. `endif-ABCDEF`).
    pub fn occupancy(&self, room: &str) -> usize {
        self.counts.get(room).copied().unwrap_or(0)
    }

    /// Players in a game: connected peers in rooms holding at least two of them. A peer alone in
    /// its room is waiting for an opponent, not playing.
    pub fn playing(&self) -> usize {
        let mut per_room: HashMap<&str, usize> = HashMap::new();
        for p in self.by_peer.values().filter(|p| p.connected) {
            *per_room.entry(p.room.as_str()).or_default() += 1;
        }
        per_room.values().filter(|&&n| n >= 2).sum()
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

    /// Moves the reservation of `origin` over to its freshly assigned peer id.
    pub fn assign(&mut self, origin: SocketAddr, peer: &str) -> Option<String> {
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

    /// Releases reservations whose socket never opened within [`ADMISSION_GRACE`]. Returns how
    /// many were released.
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

/// Full mesh with room limits and an idle timeout (see the module docs).
#[derive(Debug, Default)]
pub struct EndifMesh;

#[async_trait]
impl SignalingTopology<NoCallbacks, EndifState> for EndifMesh {
    /// Runs once per websocket, from the moment it is open until it closes.
    async fn state_machine(upgrade: WsStateMeta<NoCallbacks, EndifState>) {
        let WsStateMeta { room, peer_id, sender, mut receiver, mut state, .. } = upgrade;
        let id = peer_id.to_string();

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

        loop {
            let request = match tokio::time::timeout(IDLE_TIMEOUT, receiver.next()).await {
                Err(_) => {
                    warn!(%peer_id, %room, "nothing heard for {}s; dropping the connection", IDLE_TIMEOUT.as_secs());
                    let _ = try_send(&sender, Message::Close(None));
                    break;
                }
                Ok(None) => break,
                Ok(Some(request)) => request,
            };
            match parse_request(request) {
                Ok(PeerRequest::Signal { receiver, data }) => {
                    let event = Message::Text(JsonPeerEvent::Signal { sender: peer_id, data }.to_string().into());
                    if let Err(e) = state.mesh.try_send_to_peer(receiver, event, room.clone()) {
                        error!(%peer_id, %room, "error forwarding a signal: {e:?}");
                    }
                }
                // Keep-alives only exist to reset the idle timeout above (and to keep reverse
                // proxies from cutting a quiet socket).
                Ok(PeerRequest::KeepAlive) => {}
                Err(ClientRequestError::Json(_) | ClientRequestError::UnsupportedType(_)) => {
                    error!(%peer_id, "ignoring a malformed request");
                }
                Err(ClientRequestError::Close) => {
                    info!(%peer_id, %room, "connection closed by the peer");
                    break;
                }
                Err(e) => {
                    warn!(%peer_id, %room, "connection lost: {e:?}");
                    break;
                }
            }
        }

        state.mesh.remove_peer(&peer_id);
        let left = state.rooms.lock().unwrap().disconnect(&id);
        info!(%peer_id, %room, released = left.is_some(), "peer disconnected");
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
    fn playing_counts_full_rooms_only() {
        let mut r = Rooms::new(2);
        r.admit(addr(1), "endif-A").unwrap();
        r.admit(addr(2), "endif-A").unwrap();
        r.admit(addr(3), "endif-B").unwrap();
        r.assign(addr(1), "p1");
        r.assign(addr(2), "p2");
        r.assign(addr(3), "p3");
        r.connect("p1");
        r.connect("p3");
        assert_eq!(r.playing(), 0, "one connected peer per room: nobody is playing yet");
        r.connect("p2");
        assert_eq!(r.playing(), 2);
        r.disconnect("p1");
        assert_eq!(r.playing(), 0);
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
