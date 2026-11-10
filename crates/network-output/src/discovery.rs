//! Finding the devices of every protocol: each announces its own service
//! over mDNS, with what it is in the text record.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use mdns_sd::{ServiceDaemon, ServiceEvent};

use crate::device::{Announcement, Device, Protocol};

#[derive(Default)]
struct Found {
    /// By the announcement's full name, which is all a removal says.
    devices: BTreeMap<String, Device>,
    /// How many times `devices` changed.
    changes: u64,
}

impl Found {
    fn resolved(&mut self, fullname: &str, device: Option<Device>) {
        let changed = match device {
            Some(device) => {
                self.devices.insert(fullname.to_owned(), device.clone()) != Some(device)
            }
            None => self.devices.remove(fullname).is_some(),
        };
        self.changes += u64::from(changed);
    }

    fn removed(&mut self, fullname: &str) {
        self.changes += u64::from(self.devices.remove(fullname).is_some());
    }
}

/// Browses for devices of every protocol for as long as it lives.
pub struct Discovery {
    daemon: ServiceDaemon,
    found: Arc<Mutex<Found>>,
}

impl Discovery {
    pub fn start() -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        let found = Arc::new(Mutex::new(Found::default()));
        for protocol in Protocol::ALL {
            for service_type in protocol.service_types() {
                let events = daemon.browse(service_type)?;
                let listener = found.clone();
                std::thread::Builder::new()
                    .name("network-discovery".into())
                    .spawn(move || {
                        // Ends once the daemon is shut down and drops its sender.
                        while let Ok(event) = events.recv() {
                            apply(&mut listener.lock().unwrap(), protocol, event);
                        }
                    })
                    .expect("a thread for discovery");
            }
        }
        Ok(Discovery { daemon, found })
    }

    /// The devices found so far, by name.
    pub fn devices(&self) -> Vec<Device> {
        let mut devices: Vec<Device> = self
            .found
            .lock()
            .unwrap()
            .devices
            .values()
            .cloned()
            .collect();
        devices.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        devices
    }

    /// Changes whenever a device is found, changes or goes.
    pub fn changes(&self) -> u64 {
        self.found.lock().unwrap().changes
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        if let Err(problem) = self.daemon.shutdown() {
            eprintln!("The search for network outputs didn't shut down: {problem}");
        }
    }
}

fn apply(found: &mut Found, protocol: Protocol, event: ServiceEvent) {
    match event {
        ServiceEvent::ServiceResolved(service) => {
            let text = |key: &str| service.get_property_val_str(key).map(str::to_owned);
            let announcement = Announcement {
                fullname: &service.fullname,
                port: service.port,
                addresses: service.addresses.iter().map(|ip| ip.to_ip_addr()).collect(),
                text: &text,
            };
            found.resolved(&service.fullname, announcement.device(protocol));
        }
        ServiceEvent::ServiceRemoved(_, fullname) => found.removed(&fullname),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::{Duration, Instant};

    use mdns_sd::ServiceInfo;

    use super::*;

    fn kitchen() -> Device {
        Device {
            protocol: Protocol::Cast,
            id: "1".into(),
            name: "Kitchen".into(),
            model: None,
            address: Ipv4Addr::new(192, 168, 1, 20),
            port: 8009,
        }
    }

    #[test]
    fn a_device_that_goes_leaves_and_every_change_is_counted_once() {
        let kitchen = kitchen();
        let mut found = Found::default();
        found.resolved("a", Some(kitchen.clone()));
        found.resolved("a", Some(kitchen));
        assert_eq!((found.devices.len(), found.changes), (1, 1));
        found.removed("a");
        found.removed("a");
        assert_eq!((found.devices.len(), found.changes), (0, 2));
    }

    /// Needs multicast between two sockets of this machine, which a sandbox
    /// without a network may not allow.
    #[test]
    #[ignore = "needs multicast on this machine"]
    fn a_device_announced_on_the_network_is_found_and_leaves_when_it_says_goodbye() {
        let announcer = ServiceDaemon::new().unwrap();
        let properties = [("id", "feed"), ("fn", "Test speaker")];
        let service = ServiceInfo::new(
            Protocol::Cast.service_types()[0],
            "test-speaker",
            "test-speaker.local.",
            "127.0.0.1",
            8009,
            &properties[..],
        )
        .unwrap()
        .enable_addr_auto();
        let fullname = service.get_fullname().to_owned();
        announcer.register(service).unwrap();
        let discovery = Discovery::start().unwrap();
        let until = |done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        until(&|| !discovery.devices().is_empty());
        let names: Vec<String> = discovery.devices().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["Test speaker"]);
        announcer.unregister(&fullname).unwrap().recv().unwrap();
        until(&|| discovery.devices().is_empty());
        assert!(discovery.devices().is_empty());
    }
}
