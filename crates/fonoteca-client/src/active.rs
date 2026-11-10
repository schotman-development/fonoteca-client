//! The player behind the bar: this device's own sound output, or a device on
//! the network that plays the queue in its place. Both take the same
//! commands and say what they play in the same [`Status`], so the rest of
//! the app does not tell them apart.

#[cfg(not(target_family = "wasm"))]
use playback::Dsp;
use playback::{Leveling, Player, Repeat, Status, Track};

#[cfg(not(target_family = "wasm"))]
use crate::network_player::NetworkPlayer;

pub enum Active {
    Local(Player),
    #[cfg(not(target_family = "wasm"))]
    Network(NetworkPlayer),
}

impl Active {
    pub fn play(&self, queue: Vec<Track>, start: usize, seconds: f64) {
        match self {
            Active::Local(player) => player.play(queue, start, seconds),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.play(queue, start, seconds),
        }
    }

    pub fn requeue(&self, queue: Vec<Track>, moved: Vec<Option<usize>>) {
        match self {
            Active::Local(player) => player.requeue(queue, moved),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.requeue(queue, moved),
        }
    }

    pub fn jump(&self, index: usize) {
        match self {
            Active::Local(player) => player.jump(index),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.jump(index),
        }
    }

    pub fn next(&self) {
        match self {
            Active::Local(player) => player.next(),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.next(),
        }
    }

    pub fn previous(&self) {
        match self {
            Active::Local(player) => player.previous(),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.previous(),
        }
    }

    pub fn seek(&self, seconds: f64) {
        match self {
            Active::Local(player) => player.seek(seconds),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.seek(seconds),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        match self {
            Active::Local(player) => player.set_paused(paused),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.set_paused(paused),
        }
    }

    pub fn set_repeat(&self, repeat: Repeat) {
        match self {
            Active::Local(player) => player.set_repeat(repeat),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.set_repeat(repeat),
        }
    }

    /// Volume leveling is done by the audio engine, which a device on the
    /// network does not go through.
    pub fn set_leveling(&self, leveling: Leveling) {
        match self {
            Active::Local(player) => player.set_leveling(leveling),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(_) => {}
        }
    }

    /// The sound processing is the audio engine's, like the leveling.
    #[cfg(not(target_family = "wasm"))]
    pub fn set_dsp(&self, dsp: Dsp) {
        match self {
            Active::Local(player) => player.set_dsp(dsp),
            Active::Network(_) => {}
        }
    }

    /// Moves what plays to another sound device of this one. A device on the
    /// network is not moved this way: a different output is a different
    /// player.
    #[cfg(not(target_family = "wasm"))]
    pub fn set_device(&self, device: Option<String>) {
        match self {
            Active::Local(player) => player.set_device(device),
            Active::Network(_) => {}
        }
    }

    /// Ends what plays, for the output to be used by the next.
    #[cfg(not(target_family = "wasm"))]
    pub fn stop(&self) {
        match self {
            Active::Local(player) => player.stop(),
            Active::Network(player) => player.stop(),
        }
    }

    /// Lets the player go for the app to end: a device on the network is
    /// told to pause before it returns, which dropping does not wait for.
    pub fn close(self) {
        match self {
            Active::Local(_) => {}
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.close(),
        }
    }

    pub fn status(&self) -> Status {
        match self {
            Active::Local(player) => player.status(),
            #[cfg(not(target_family = "wasm"))]
            Active::Network(player) => player.status(),
        }
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn is_network(&self) -> bool {
        matches!(self, Active::Network(_))
    }
}
