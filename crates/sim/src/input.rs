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
/// Not an `in_buttons.h` bit: the player's preferred rocket launcher rides along with the buttons
/// (set: The Original, clear: the stock launcher). Source carries the loadout out of band; here
/// the input stream is the only thing peers exchange, so the choice has to travel in it. It is
/// read on the tick a player spawns (see `Player::weapon`) and ignored otherwise.
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
