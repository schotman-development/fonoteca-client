//! Devices on the network that play what the app sends them: finding them,
//! telling one what to play, and the small server they fetch the file from.
//! The protocols each live in a module of their own (so far Google's Cast in
//! `cast`) behind [`NetworkOutput`], and the rest does not know which a
//! device speaks.
#![cfg(not(target_family = "wasm"))]

mod cast;
mod device;
mod discovery;
mod file_server;
mod output;

pub use device::{Device, Protocol};
pub use discovery::Discovery;
pub use file_server::{FileServer, Hosted};
pub use output::{Media, NetworkOutput, OutputState, OutputStatus, Unavailable};
