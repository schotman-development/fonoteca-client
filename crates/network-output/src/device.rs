//! What a device on the network is, whatever protocol it is played to over.

use std::net::{IpAddr, Ipv4Addr};

use crate::cast;
use crate::output::NetworkOutput;

/// The protocols a device is played to over, each announced under its own
/// mDNS service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Google's, of a Chromecast or a speaker with Chromecast built in.
    Cast,
}

impl Protocol {
    pub const ALL: [Protocol; 1] = [Protocol::Cast];

    /// The mDNS services a device of the protocol announces itself under.
    pub fn service_types(self) -> &'static [&'static str] {
        match self {
            Protocol::Cast => &["_googlecast._tcp.local."],
        }
    }

    /// What the protocol is called, in the id of a device and in words.
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Cast => "Cast",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub protocol: Protocol,
    /// The device's own, which stays the same across restarts and networks.
    pub id: String,
    pub name: String,
    pub model: Option<String>,
    pub address: Ipv4Addr,
    pub port: u16,
}

impl Device {
    /// An output to play on, connecting to it in the background.
    pub fn connect(&self) -> Box<dyn NetworkOutput> {
        match self.protocol {
            Protocol::Cast => Box::new(cast::CastSession::connect(self)),
        }
    }
}

/// What the text record of an announcement says, as `key`s of it.
pub(crate) struct Announcement<'a> {
    pub fullname: &'a str,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
    pub text: &'a dyn Fn(&str) -> Option<String>,
}

impl Announcement<'_> {
    /// None for an announcement a connection can't be made from.
    pub(crate) fn device(&self, protocol: Protocol) -> Option<Device> {
        match protocol {
            Protocol::Cast => cast::device(self),
        }
    }

    /// The first IPv4 address: a link-local IPv6 one needs the interface it
    /// came in on.
    pub(crate) fn ipv4(&self) -> Option<Ipv4Addr> {
        self.addresses.iter().find_map(|address| match address {
            IpAddr::V4(address) => Some(*address),
            IpAddr::V6(_) => None,
        })
    }

    /// The instance's own name, the first label of its full name.
    pub(crate) fn instance(&self) -> String {
        self.fullname
            .split('.')
            .next()
            .unwrap_or_default()
            .to_owned()
    }
}
