//! Devices on the network as outputs of the app, whatever protocol they are
//! played to over: found while the output menu is in use or one is played
//! to, listed beside the sound devices, and played to through a
//! [`NetworkPlayer`].

use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use network_output::{Device, Discovery, FileServer, OpenPcm, Protocol};
use playback::{Fetch, NetworkProtocol, OutputDevice, OutputKind};

#[cfg(target_os = "android")]
use crate::network::MulticastHold;
use crate::network_player::{Connect, NetworkPlayer};

/// Starts the id of a device on the network among the ids of sound devices;
/// the protocol and the device's own id follow.
const ID_PREFIX: &str = "network:";
/// What the menu says does not apply to a device on the network, which gets
/// the file as it is.
const NOT_APPLIED: &str = "No sound processing, leveling or bit-perfect";
/// An AirPlay receiver is sent the track decoded, in the one format it takes.
const AIRPLAY_FORMAT: &str = "sent as 44.1 kHz 16-bit";
/// How long the search goes on after the menu was last opened, when no
/// device is played to.
const SEARCH_FOR: Duration = Duration::from_secs(60);

pub fn is_network(output_id: &str) -> bool {
    output_id.starts_with(ID_PREFIX)
}

pub fn output_id(device: &Device) -> String {
    format!(
        "{ID_PREFIX}{}:{}",
        device.protocol.name().to_lowercase(),
        device.id
    )
}

/// The device as the output menu lists it.
pub fn output_device(device: &Device) -> OutputDevice {
    let id = output_id(device);
    OutputDevice {
        key: id.clone(),
        id,
        name: device.name.clone(),
        detail: match device.protocol {
            Protocol::Cast => NOT_APPLIED.to_owned(),
            Protocol::AirPlay => format!("{NOT_APPLIED}, {AIRPLAY_FORMAT}"),
        },
        title: device.name.clone(),
        kind: OutputKind::Network(match device.protocol {
            Protocol::Cast => NetworkProtocol::Cast,
            Protocol::AirPlay => NetworkProtocol::AirPlay,
        }),
        same_as: Vec::new(),
    }
}

/// Nothing holds multicast packets back outside Android, so nothing is held.
#[cfg(not(target_os = "android"))]
struct MulticastHold;

#[cfg(not(target_os = "android"))]
impl MulticastHold {
    fn acquire() -> Self {
        MulticastHold
    }
}

/// What a change of the devices found calls for.
#[derive(Debug, PartialEq, Eq)]
pub enum Search {
    Quiet,
    /// Devices were found, changed or went since the last look.
    Changed,
    /// Nobody looked at the menu for a while: the search ended.
    Ended,
}

/// The search for devices on the network, which runs only while it is of use.
#[derive(Default)]
pub struct Finder {
    discovery: Option<Discovery>,
    /// Held for as long as the search runs.
    multicast: Option<MulticastHold>,
    seen_changes: u64,
    until: Option<Instant>,
}

impl Finder {
    /// Searches, or goes on searching, for [`SEARCH_FOR`] from `now`.
    pub fn keep_searching(&mut self, now: Instant) {
        self.until = Some(now + SEARCH_FOR);
        if self.discovery.is_some() {
            return;
        }
        let multicast = MulticastHold::acquire();
        match Discovery::start() {
            Ok(discovery) => {
                self.discovery = Some(discovery);
                self.multicast = Some(multicast);
            }
            Err(problem) => eprintln!("Devices on the network can't be searched for: {problem}"),
        }
    }

    pub fn devices(&self) -> Vec<Device> {
        self.discovery.iter().flat_map(Discovery::devices).collect()
    }

    /// Looks at what the search found since the last look. The search ends
    /// once it has gone unused for long enough, unless `playing` to a
    /// device, which is watched for as long as it plays.
    pub fn look(&mut self, now: Instant, playing: bool) -> Search {
        let Some(discovery) = &self.discovery else {
            return Search::Ended;
        };
        if playing {
            self.until = Some(now + SEARCH_FOR);
        } else if self.until.is_none_or(|until| now >= until) {
            self.discovery = None;
            self.multicast = None;
            return Search::Ended;
        }
        let changes = discovery.changes();
        if changes == std::mem::replace(&mut self.seen_changes, changes) {
            return Search::Quiet;
        }
        Search::Changed
    }
}

/// A player to `device`, that gets its tracks from `fetch` and keeps them
/// in `folder` for the device to fetch from this one.
pub fn player(device: &Device, fetch: Fetch, folder: PathBuf) -> io::Result<NetworkPlayer> {
    let files = FileServer::start(address_towards(device.address)?)?;
    let target = device.clone();
    let connect: Connect = Box::new(move || target.connect(decode_to_pcm()));
    Ok(NetworkPlayer::new(
        device.name.clone(),
        connect,
        files,
        fetch,
        folder,
    ))
}

/// How a receiver that is sent its audio gets it: decoded by the player's own
/// decoder.
fn decode_to_pcm() -> OpenPcm {
    Arc::new(|path: &Path, start| {
        let pcm = playback::open_pcm(path, start)?;
        Ok(Box::new(pcm) as Box<dyn Read + Send>)
    })
}

/// This device's address on the network that reaches `device`, which is
/// where the device can reach this one. Connecting a datagram socket sends
/// nothing, and has the system pick the route.
fn address_towards(device: Ipv4Addr) -> io::Result<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect((device, 9))?;
    Ok(socket.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kitchen() -> Device {
        Device {
            protocol: Protocol::Cast,
            id: "9f8e".into(),
            name: "Kitchen".into(),
            model: None,
            address: Ipv4Addr::LOCALHOST,
            port: 8009,
        }
    }

    #[test]
    fn a_device_on_the_network_is_listed_by_its_name_with_its_own_kind_and_says_what_does_not_apply()
     {
        let listed = output_device(&kitchen());
        assert_eq!(
            (listed.id.as_str(), listed.name.as_str(), listed.kind),
            (
                "network:cast:9f8e",
                "Kitchen",
                OutputKind::Network(NetworkProtocol::Cast)
            )
        );
        assert!(listed.detail.contains("bit-perfect"));
        assert!(is_network(&listed.id));
        assert!(!is_network("alsa:hw:CARD=PCH,DEV=0"));
    }

    #[test]
    fn an_airplay_receiver_is_listed_with_its_own_kind_and_says_what_it_is_sent() {
        let airplay = Device {
            protocol: Protocol::AirPlay,
            id: "A1B2C3D4E5F6".into(),
            ..kitchen()
        };
        let listed = output_device(&airplay);
        assert_eq!(
            (listed.id.as_str(), listed.kind),
            (
                "network:airplay:A1B2C3D4E5F6",
                OutputKind::Network(NetworkProtocol::AirPlay)
            )
        );
        assert!(listed.detail.contains("bit-perfect") && listed.detail.contains("16-bit"));
    }

    #[test]
    fn the_address_a_device_reaches_this_one_on_is_the_interface_that_routes_to_it() {
        assert_eq!(
            address_towards(Ipv4Addr::LOCALHOST).unwrap(),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
    }

    #[test]
    fn a_finder_that_was_never_started_has_nothing_to_look_at() {
        let mut finder = Finder::default();
        assert!(finder.devices().is_empty());
        assert_eq!(finder.look(Instant::now(), false), Search::Ended);
    }
}
