//! `endif-sim`: a deterministic, platform-independent port of the TF2 soldier movement, rocket and
//! damage code plus the MGEMod "endif" rules. This crate has no engine dependencies so it can be
//! stepped identically by every peer in a rollback session and by the server for verification.

/// Hash of this crate's sources (see `build.rs`): identical for every build of the same
/// simulation, different as soon as the simulation changes.
pub const SIM_HASH: &str = env!("ENDIF_SIM_HASH");

/// The commit this was built from (see `build.rs`; `dev` outside git). Changes with every push,
/// so a client can tell that a newer build exists even when the simulation, and therefore
/// [`protocol_id`], has not changed. The server reports it on `GET /build`.
pub const BUILD_ID: &str = env!("ENDIF_BUILD_ID");

/// Bump when the netcode changes in a way the simulation hash cannot see: the GGRS settings
/// (frame rate, input delay, prediction window), the input serialisation, the room protocol.
pub const NET_PROTOCOL: u32 = 3;

/// The identity two peers must share to play together; checked by the signaling server.
pub fn protocol_id() -> String {
    format!("{SIM_HASH}-{NET_PROTOCOL}")
}

pub mod arena;
pub mod checksum;
pub mod consts;
pub mod input;
pub mod math;
pub mod movement;
pub mod player;
pub mod rng;
pub mod trace;
pub mod weapons;
pub mod world;

pub use arena::{Arena, Spawn};
pub use checksum::DetHasher;
pub use consts::*;
pub use input::*;
pub use math::{QAngle, Vec3};
pub use player::{FL_DUCKING, FL_ONGROUND, Player, Weapon};
pub use trace::{Aabb, HitEnt, Trace, TraceEnv};
pub use weapons::{Rocket, Rules};
pub use world::{NUM_PLAYERS, Phase, SimEvent, SimState};
