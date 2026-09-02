//! The matchmaking queue: first come, first served. Players join with their account, poll every
//! second or two, and are paired with whoever was waiting before them. Pairing creates a match
//! record and a fresh room code that both clients then join like a private room.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// A player waiting for an opponent.
pub struct Waiting {
    pub ticket: String,
    pub account_id: u64,
    pub username: String,
    pub elo: i32,
    pub last_seen: Instant,
}

/// What a paired player is told.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Matched {
    pub match_id: u64,
    pub room: String,
    /// 0 = player_a, 1 = player_b in the match record.
    pub slot: u8,
    pub opponent: String,
    pub opponent_elo: i32,
    #[serde(skip)]
    pub account_id: u64,
    #[serde(skip)]
    pub created: Instant,
}

/// A queue entry that stops polling is dropped after this long.
const STALE: Duration = Duration::from_secs(15);
/// A pairing that was never picked up is forgotten after this long.
const MATCH_TTL: Duration = Duration::from_secs(120);

#[derive(Default)]
pub struct Queue {
    waiting: VecDeque<Waiting>,
    /// Matched tickets, until their owner polls them.
    matched: HashMap<String, Matched>,
}

pub enum PollResult {
    Waiting { position: usize },
    Matched(Matched),
    Expired,
}

impl Queue {
    fn prune(&mut self) {
        let now = Instant::now();
        self.waiting.retain(|w| now.duration_since(w.last_seen) < STALE);
        self.matched.retain(|_, m| now.duration_since(m.created) < MATCH_TTL);
    }

    /// Joins the queue. Returns the ticket to poll with, and the player who was waiting before us
    /// (removed from the queue) when there is one: the caller creates the match and calls `pair`.
    pub fn join(&mut self, account_id: u64, username: &str, elo: i32) -> (String, Option<Waiting>) {
        self.prune();
        // Already queued (a second tab, a retry): keep the existing ticket.
        if let Some(w) = self.waiting.iter_mut().find(|w| w.account_id == account_id) {
            w.last_seen = Instant::now();
            return (w.ticket.clone(), None);
        }
        // Already paired and not yet told.
        if let Some((ticket, _)) = self.matched.iter().find(|(_, m)| m.account_id == account_id) {
            return (ticket.clone(), None);
        }
        let ticket = random_ticket();
        let opponent = self.waiting.pop_front();
        if opponent.is_none() {
            self.waiting.push_back(Waiting {
                ticket: ticket.clone(),
                account_id,
                username: username.to_string(),
                elo,
                last_seen: Instant::now(),
            });
        }
        (ticket, opponent)
    }

    /// Records a pairing so both tickets learn about it on their next poll.
    pub fn pair(&mut self, ticket_a: &str, a: Matched, ticket_b: &str, b: Matched) {
        self.matched.insert(ticket_a.to_string(), a);
        self.matched.insert(ticket_b.to_string(), b);
    }

    /// Puts a player back at the front after a failed pairing.
    pub fn requeue(&mut self, w: Waiting) {
        self.waiting.push_front(w);
    }

    pub fn poll(&mut self, ticket: &str) -> PollResult {
        self.prune();
        if let Some(m) = self.matched.remove(ticket) {
            return PollResult::Matched(m);
        }
        for (i, w) in self.waiting.iter_mut().enumerate() {
            if w.ticket == ticket {
                w.last_seen = Instant::now();
                return PollResult::Waiting { position: i + 1 };
            }
        }
        PollResult::Expired
    }

    pub fn leave(&mut self, ticket: &str) {
        self.waiting.retain(|w| w.ticket != ticket);
    }

    /// Players waiting for an opponent (those already paired are not counted).
    pub fn len(&self) -> usize {
        self.waiting.len()
    }
}

fn random_ticket() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..24).map(|_| rng.gen_range(b'a'..=b'z') as char).collect()
}

/// Room codes use the client's alphabet (no I, O, 0, 1).
pub fn random_room_code() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..6).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}
