//! What a receiver's announcements say, and whether this can play to it.
//! AirPlay announces two services: `_raop._tcp`, which this plays over and
//! which has the receiver's port, and `_airplay._tcp`, which is known by the
//! same device id and says more of what the receiver is.

use std::net::Ipv4Addr;

use crate::device::{Announcement, Device, Protocol};

const RAOP: &str = "._raop._tcp.";
const AIRPLAY: &str = "._airplay._tcp.";
/// The feature bit of receivers that make a sender pair with them first,
/// which a HomePod or an Apple TV does and this does not.
const HOMEKIT_PAIRING: u32 = 46;

/// One announcement of a receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Record {
    /// A receiver this can play to, from `_raop`.
    Raop {
        id: String,
        name: String,
        model: Option<String>,
        address: Ipv4Addr,
        port: u16,
    },
    /// What `_airplay` says of the receiver of the `id`.
    AirPlay {
        id: String,
        model: Option<String>,
        /// It wants a password or a pairing, which this can't give.
        guarded: bool,
    },
}

/// None for an announcement of another service, of a receiver this can't
/// play to, or one without what a connection needs.
pub(crate) fn record(announcement: &Announcement) -> Option<Record> {
    let text = |key: &str| (announcement.text)(key).filter(|value| !value.is_empty());
    if announcement.fullname.contains(RAOP) {
        raop(announcement, &text)
    } else if announcement.fullname.contains(AIRPLAY) {
        airplay(&text)
    } else {
        None
    }
}

/// The `_raop` instance is `<device id>@<name>`: the id is the MAC address.
fn raop(announcement: &Announcement, text: &dyn Fn(&str) -> Option<String>) -> Option<Record> {
    let instance = announcement.instance();
    let (id, name) = instance.split_once('@')?;
    let listed = |key: &str, wanted: &str| {
        text(key).is_none_or(|values| values.split(',').any(|value| value.trim() == wanted))
    };
    let supported = listed("et", "0")
        && text("cn").is_some_and(|codecs| codecs.split(',').any(|codec| codec.trim() == "1"))
        && text("pw").is_none_or(|password| password != "true")
        && listed("tp", "UDP")
        && text("sr").is_none_or(|rate| rate == "44100")
        && text("ss").is_none_or(|bits| bits == "16")
        && text("ch").is_none_or(|channels| channels == "2");
    supported.then_some(Record::Raop {
        id: device_id(id)?,
        name: name.to_owned(),
        model: text("am"),
        address: announcement.ipv4()?,
        port: announcement.port,
    })
}

fn airplay(text: &dyn Fn(&str) -> Option<String>) -> Option<Record> {
    let guarded = text("pw").is_some_and(|password| password == "true")
        || text("features").is_some_and(|features| has_feature(&features, HOMEKIT_PAIRING));
    Some(Record::AirPlay {
        id: device_id(&text("deviceid")?)?,
        model: text("model"),
        guarded,
    })
}

/// `features` is one hex number, or two joined by a comma with the high bits
/// in the second.
fn has_feature(features: &str, bit: u32) -> bool {
    let mut halves = features
        .split(',')
        .map(|half| u32::from_str_radix(half.trim().trim_start_matches("0x"), 16));
    let (low, high) = (halves.next(), halves.next());
    match (bit, low, high) {
        (0..32, Some(Ok(low)), _) => low >> bit & 1 == 1,
        (32.., _, Some(Ok(high))) => high >> (bit - 32) & 1 == 1,
        _ => false,
    }
}

/// A MAC address without its colons, in capitals; the same device whichever
/// service names it.
fn device_id(raw: &str) -> Option<String> {
    let id: String = raw.chars().filter(|c| *c != ':').collect();
    (id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| id.to_ascii_uppercase())
}

/// The receivers to list: one device for each id with a `_raop` record, left
/// out when its `_airplay` record says it is guarded.
pub(crate) fn merge<'a>(records: impl Iterator<Item = &'a Record> + Clone) -> Vec<Device> {
    let guarded = |wanted: &str| {
        records.clone().any(
            |record| matches!(record, Record::AirPlay { id, guarded: true, .. } if id == wanted),
        )
    };
    let airplay_model = |wanted: &str| {
        records.clone().find_map(|record| match record {
            Record::AirPlay { id, model, .. } if id == wanted => model.clone(),
            _ => None,
        })
    };
    let mut devices: Vec<Device> = Vec::new();
    for record in records.clone() {
        let Record::Raop {
            id,
            name,
            model,
            address,
            port,
        } = record
        else {
            continue;
        };
        if guarded(id) || devices.iter().any(|device| device.id == *id) {
            continue;
        }
        devices.push(Device {
            protocol: Protocol::AirPlay,
            id: id.clone(),
            name: name.clone(),
            model: model.clone().or_else(|| airplay_model(id)),
            address: *address,
            port: *port,
        });
    }
    devices
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::IpAddr;

    use super::*;

    fn heard(fullname: &str, port: u16, text: &[(&str, &str)]) -> Option<Record> {
        let text: HashMap<String, String> = text
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let lookup = |key: &str| text.get(key).cloned();
        record(&Announcement {
            fullname,
            port,
            addresses: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30))],
            text: &lookup,
        })
    }

    const SHAIRPORT: &str = "A1B2C3D4E5F6@Kitchen._raop._tcp.local.";

    fn shairport() -> Vec<(&'static str, &'static str)> {
        vec![
            ("txtvers", "1"),
            ("ch", "2"),
            ("cn", "0,1"),
            ("et", "0,1"),
            ("md", "0,1,2"),
            ("pw", "false"),
            ("sr", "44100"),
            ("ss", "16"),
            ("tp", "UDP"),
            ("vn", "65537"),
            ("am", "ShairportSync"),
        ]
    }

    #[test]
    fn a_shairport_sync_receiver_is_known_by_the_mac_in_its_instance_and_named_after_it() {
        assert_eq!(
            heard(SHAIRPORT, 5000, &shairport()),
            Some(Record::Raop {
                id: "A1B2C3D4E5F6".into(),
                name: "Kitchen".into(),
                model: Some("ShairportSync".into()),
                address: Ipv4Addr::new(192, 168, 1, 30),
                port: 5000,
            })
        );
    }

    #[test]
    fn an_airport_express_that_wants_an_rsa_key_is_not_one_this_plays_to() {
        let airport = [
            ("cn", "0,1"),
            ("et", "1"),
            ("pw", "false"),
            ("sr", "44100"),
            ("ss", "16"),
            ("ch", "2"),
            ("tp", "TCP,UDP"),
            ("am", "AirPort10,115"),
        ];
        assert_eq!(
            heard("001122334455@Office._raop._tcp.local.", 5000, &airport),
            None
        );
    }

    #[test]
    fn a_receiver_with_a_password_or_without_apple_lossless_is_not_listed() {
        let without = |key: &str, value: &str| {
            let mut text: Vec<_> = shairport().into_iter().filter(|(k, _)| *k != key).collect();
            text.push((key, value));
            heard(SHAIRPORT, 5000, &text)
        };
        assert_eq!(without("pw", "true"), None);
        assert_eq!(without("cn", "0,2"), None);
    }

    #[test]
    fn an_encryption_list_without_none_a_rate_or_a_transport_this_does_not_play_is_not_listed() {
        let without = |key: &str, value: &str| {
            let mut text: Vec<_> = shairport().into_iter().filter(|(k, _)| *k != key).collect();
            text.push((key, value));
            heard(SHAIRPORT, 5000, &text)
        };
        assert_eq!(without("et", "3,5"), None);
        assert_eq!(without("sr", "48000"), None);
        assert_eq!(without("tp", "TCP"), None);
        assert_eq!(without("ch", "6"), None);
    }

    #[test]
    fn a_homepod_style_receiver_passes_its_raop_record_and_is_guarded_by_its_airplay_one() {
        let homepod = [
            ("cn", "0,1,2,3"),
            ("et", "0,4"),
            ("pw", "false"),
            ("tp", "UDP"),
            ("am", "AudioAccessory5,1"),
        ];
        assert!(heard("112233445566@Living._raop._tcp.local.", 7000, &homepod).is_some());
        let airplay = heard(
            "Living._airplay._tcp.local.",
            7000,
            &[
                ("deviceid", "11:22:33:44:55:66"),
                ("features", "0x4A7FCFD5,0x3C155FDE"),
                ("model", "AudioAccessory5,1"),
            ],
        );
        assert_eq!(
            airplay,
            Some(Record::AirPlay {
                id: "112233445566".into(),
                model: Some("AudioAccessory5,1".into()),
                guarded: true,
            })
        );
    }

    #[test]
    fn features_are_read_from_one_or_two_hex_numbers() {
        assert!(has_feature("0x4A7FCFD5,0x3C155FDE", 46));
        assert!(has_feature("0x4A7FCFD5", 0));
        assert!(!has_feature("0x4A7FCFD5", 46));
        assert!(!has_feature("nonsense", 0));
        assert!(!has_feature("0x1,0x1", 33));
    }

    #[test]
    fn an_instance_that_is_not_a_mac_at_a_name_is_no_device() {
        assert_eq!(heard("Kitchen._raop._tcp.local.", 5000, &shairport()), None);
        assert_eq!(
            heard("nothex@Kitchen._raop._tcp.local.", 5000, &shairport()),
            None
        );
        assert_eq!(
            heard(
                "A1B2C3D4E5F6@Kitchen._other._tcp.local.",
                5000,
                &shairport()
            ),
            None
        );
    }

    fn listed(records: &[Record]) -> Vec<Device> {
        merge(records.iter())
    }

    fn raop_record(id: &str, model: Option<&str>) -> Record {
        Record::Raop {
            id: id.into(),
            name: "Kitchen".into(),
            model: model.map(str::to_owned),
            address: Ipv4Addr::new(192, 168, 1, 30),
            port: 5000,
        }
    }

    fn airplay_record(id: &str, model: Option<&str>, guarded: bool) -> Record {
        Record::AirPlay {
            id: id.into(),
            model: model.map(str::to_owned),
            guarded,
        }
    }

    #[test]
    fn the_two_services_of_one_receiver_are_one_device_with_the_model_either_gave() {
        let devices = listed(&[
            airplay_record("AA", Some("Model X"), false),
            raop_record("AA", None),
        ]);
        assert_eq!(devices.len(), 1);
        assert_eq!(
            (
                devices[0].protocol,
                devices[0].model.as_deref(),
                devices[0].port
            ),
            (Protocol::AirPlay, Some("Model X"), 5000)
        );
        assert_eq!(
            listed(&[raop_record("AA", Some("Raop's"))])[0]
                .model
                .as_deref(),
            Some("Raop's")
        );
    }

    #[test]
    fn a_receiver_that_is_guarded_or_has_only_an_airplay_record_is_not_listed() {
        assert!(listed(&[raop_record("AA", None), airplay_record("AA", None, true)]).is_empty());
        assert!(listed(&[airplay_record("AA", None, false)]).is_empty());
        let two = listed(&[
            raop_record("AA", None),
            raop_record("AA", None),
            raop_record("BB", None),
        ]);
        assert_eq!(two.len(), 2);
    }
}
