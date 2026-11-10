//! What the player needs of a device on the network, whatever protocol it
//! speaks. Every call returns at once: the device answers when it does, and
//! [`NetworkOutput::status`] says what it last said.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Opens the file at a path as 44.1 kHz, 16-bit, stereo samples in the
/// machine's byte order, from the given second: for a device that is sent the
/// audio instead of fetching the file.
pub type OpenPcm = Arc<dyn Fn(&Path, f64) -> Result<Box<dyn Read + Send>, String> + Send + Sync>;

/// A file the device is to play: it fetches the file from `url`, or is sent
/// what is decoded from the file at `path`.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub url: String,
    pub path: PathBuf,
    pub content_type: String,
    /// Seconds into the file to begin at.
    pub start: f64,
    /// Wait for [`NetworkOutput::set_paused`] with `false` before playing.
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutputState {
    /// Connecting, or the file asked for is not playing yet.
    Loading,
    Playing,
    Paused,
    /// The file played to its end.
    Finished,
    /// The device could not play the file, or something else took it over.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputStatus {
    /// How many [`NetworkOutput::load`]s the device has answered. A state
    /// seen while this is fewer than the loads asked for belongs to the file
    /// before.
    pub loads: u64,
    pub state: OutputState,
    /// Seconds into the file.
    pub position: f64,
}

/// The device stopped answering or closed the connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Unavailable(pub String);

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

pub trait NetworkOutput: Send {
    fn load(&mut self, media: Media);
    fn set_paused(&mut self, paused: bool);
    fn seek(&mut self, seconds: f64);
    /// Ends what plays, for the device to be used by something else.
    fn stop(&mut self);
    fn status(&self) -> Result<OutputStatus, Unavailable>;
}
