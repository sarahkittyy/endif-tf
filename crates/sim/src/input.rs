//! Per-tick player input. This is the only data exchanged between peers, so it must be
//! `Pod`-like and identical on every platform.

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

// `in_buttons.h` values.
pub const IN_ATTACK: u32 = 1 << 0;
pub const IN_JUMP: u32 = 1 << 1;
pub const IN_DUCK: u32 = 1 << 2;
pub const IN_FORWARD: u32 = 1 << 3;
pub const IN_BACK: u32 = 1 << 4;
pub const IN_MOVELEFT: u32 = 1 << 9;
pub const IN_MOVERIGHT: u32 = 1 << 10;
pub const IN_RELOAD: u32 = 1 << 13;
/// Not `in_buttons.h` bits: the player's preferred rocket launcher rides along with the buttons.
/// Source carries the loadout out of band; here the input stream is the only thing peers
/// exchange, so the choice has to travel in it. The client sets exactly one of the two; an input
/// with neither is one nobody wrote (GGRS pads the first `input_delay` frames and any
/// disconnected slot with zeroed inputs), so it carries no preference and a spawn keeps waiting
/// for one (see `Player::weapon_pending`). Read on the tick a player spawns and ignored otherwise.
pub const IN_WEAPON_STOCK: u32 = 1 << 29;
pub const IN_WEAPON_ORIGINAL: u32 = 1 << 30;

/// One tick of input for one player.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable, Serialize, Deserialize)]
pub struct PlayerInput {
    /// `IN_*` button bits.
    pub buttons: u32,
    /// View pitch in degrees, Source convention (positive looks down), clamped to ±89.
    pub pitch: f32,
    /// View yaw in degrees in (-180, 180].
    pub yaw: f32,
}

impl PlayerInput {
    pub fn pressed(&self, bit: u32) -> bool {
        self.buttons & bit != 0
    }
}
