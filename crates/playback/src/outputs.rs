//! Which of the outputs a host lists are worth choosing between, and what to
//! call them. Android lists every route its audio system has, so one speaker
//! shows up several times under names made for programs; ALSA's routes to one
//! card differ in what they do to the sound, so those stay as they are.

use cpal::{DeviceDescription, DeviceType, InterfaceType};

/// An output to choose from: its `name` and a short `detail` to tell it from
/// others of the name, and `title`, which says all of that in one line.
/// `same_as` holds the ids of the routes that were folded into it, which
/// still play on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub title: String,
    pub same_as: Vec<String>,
}

/// What the host says of one output.
pub(crate) struct Found {
    pub id: String,
    /// The name the host gives it, as ALSA's is shown.
    pub name: String,
    pub description: DeviceDescription,
    /// cpal turns Android's safe speaker into a plain speaker, and only its
    /// debug output tells them apart.
    pub safe_speaker: bool,
}

struct Output {
    id: String,
    name: String,
    kind: DeviceType,
    interface: InterfaceType,
    address: Option<String>,
    safe_speaker: bool,
    same_as: Vec<String>,
}

const ANDROID_HOST_PREFIX: &str = "aaudio:";
const ALSA_HOST_PREFIX: &str = "alsa:";

/// The outputs to choose from, once each, in the host's order.
pub(crate) fn list(found: Vec<Found>) -> Vec<OutputDevice> {
    let outputs = found.into_iter().map(Output::from).collect();
    distinct(outputs).into_iter().map(describe).collect()
}

impl From<Found> for Output {
    fn from(found: Found) -> Self {
        let android = found.id.starts_with(ANDROID_HOST_PREFIX);
        Output {
            name: if android {
                found.description.name().to_owned()
            } else {
                found.name
            },
            id: found.id,
            kind: found.description.device_type(),
            interface: found.description.interface_type(),
            address: found.description.address().map(str::to_owned),
            safe_speaker: found.safe_speaker,
            same_as: Vec::new(),
        }
    }
}

/// What is left of `outputs` once those not for music and those that are
/// another route to an output already there are gone.
fn distinct(outputs: Vec<Output>) -> Vec<Output> {
    let mut kept: Vec<Output> = Vec::new();
    for output in outputs {
        if !output.id.starts_with(ANDROID_HOST_PREFIX) {
            kept.push(output);
            continue;
        }
        if !for_music(&output) {
            continue;
        }
        let Some(position) = kept.iter().position(|other| same_output(other, &output)) else {
            kept.push(output);
            continue;
        };
        let other = &mut kept[position];
        if rank(&output) < rank(other) {
            let mut folded = std::mem::replace(other, output);
            other.same_as.append(&mut folded.same_as);
            other.same_as.push(folded.id);
        } else {
            other.same_as.push(output.id);
        }
    }
    kept
}

/// Android's docs: the earpiece is the in-call speaker, the telephony and
/// Bluetooth SCO routes carry calls, the bus is a car's own audio system,
/// and the tuner and remote submix reroute audio inside the system. cpal
/// turns telephony and the bus into an unknown type on an unknown interface.
fn for_music(output: &Output) -> bool {
    let unknown = output.kind == DeviceType::Unknown && output.interface == InterfaceType::Unknown;
    !unknown
        && !matches!(
            output.kind,
            DeviceType::Earpiece | DeviceType::Handset | DeviceType::Tuner | DeviceType::Virtual
        )
}

/// Android's docs: the safe speaker "may be the same physical audio device"
/// as the speaker, driven differently; a Bluetooth headset is listed once
/// per profile, and both carry the address of the headset.
fn same_output(a: &Output, b: &Output) -> bool {
    match (&a.address, &b.address) {
        (Some(left), Some(right)) => a.interface == b.interface && left == right,
        _ => a.kind == b.kind && a.interface == b.interface && a.name == b.name,
    }
}

/// Of the routes to one output, the lowest rank is the one to show: the
/// speaker rather than the safe speaker, which is tuned for alarms, and a
/// Bluetooth headset's music route rather than its call route, which cpal
/// types as a headset while the music route has no type of its own.
fn rank(output: &Output) -> u8 {
    u8::from(output.safe_speaker || output.kind == DeviceType::Headset)
}

/// A name for the listener and a short kind to tell outputs apart by.
fn describe(output: Output) -> OutputDevice {
    let (name, detail, title) = if output.id.starts_with(ANDROID_HOST_PREFIX) {
        let (name, detail) = android_words(&output);
        (name.clone(), detail, name)
    } else {
        // Routes to one card are told apart by their pcm id alone.
        let pcm = output
            .id
            .strip_prefix(ALSA_HOST_PREFIX)
            .unwrap_or(&output.id);
        let title = format!("{} ({pcm})", output.name);
        (output.name, pcm.to_owned(), title)
    };
    OutputDevice {
        id: output.id,
        name,
        detail,
        title,
        same_as: output.same_as,
    }
}

/// What the output is, by what Android says it is: its own name is the
/// phone's model for the speaker and the earpiece alike.
fn android_words(output: &Output) -> (String, String) {
    let product = product_name(output);
    match (output.kind, output.interface) {
        (DeviceType::Speaker, InterfaceType::BuiltIn) => {
            ("Phone speaker".into(), "Built-in".into())
        }
        (_, InterfaceType::Bluetooth) => (format!("Bluetooth: {product}"), "Bluetooth".into()),
        (_, InterfaceType::Usb) => (format!("USB: {product}"), "USB".into()),
        (_, InterfaceType::Hdmi) => ("HDMI".into(), "HDMI".into()),
        (DeviceType::Headphones, _) => ("Headphones".into(), "Wired".into()),
        (DeviceType::Headset, _) => ("Headset".into(), "Wired".into()),
        (DeviceType::HearingAid, _) => ("Hearing aid".into(), String::new()),
        (DeviceType::Dock, _) => ("Dock".into(), String::new()),
        _ => (product, String::new()),
    }
}

/// cpal names an Android output "<product name> (<type>)" unless it can't
/// type it.
fn product_name(output: &Output) -> String {
    let suffix = format!(" ({})", output.kind);
    output
        .name
        .strip_suffix(&suffix)
        .unwrap_or(&output.name)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use cpal::DeviceDescriptionBuilder;

    use super::*;

    fn found(
        id: &str,
        name: &str,
        kind: DeviceType,
        interface: InterfaceType,
        address: &str,
    ) -> Found {
        let mut description = DeviceDescriptionBuilder::new(name)
            .device_type(kind)
            .interface_type(interface);
        if !address.is_empty() {
            description = description.address(address);
        }
        Found {
            id: id.to_owned(),
            name: name.to_owned(),
            description: description.build(),
            safe_speaker: false,
        }
    }

    fn safe(mut found: Found) -> Found {
        found.safe_speaker = true;
        found
    }

    fn ids(found: Vec<Found>) -> Vec<String> {
        list(found).into_iter().map(|output| output.id).collect()
    }

    fn words(found: Vec<Found>) -> Vec<(String, String)> {
        list(found)
            .into_iter()
            .map(|output| (output.name, output.detail))
            .collect()
    }

    fn speaker(id: &str) -> Found {
        found(
            id,
            "Pixel 7 (Speaker)",
            DeviceType::Speaker,
            InterfaceType::BuiltIn,
            "",
        )
    }

    /// What Android 14 lists on a phone with nothing plugged in, as cpal
    /// describes it.
    fn phone() -> Vec<Found> {
        vec![
            speaker("aaudio:2"),
            safe(speaker("aaudio:3")),
            found(
                "aaudio:4",
                "Pixel 7 (Earpiece)",
                DeviceType::Earpiece,
                InterfaceType::BuiltIn,
                "",
            ),
            found(
                "aaudio:5",
                "Pixel 7",
                DeviceType::Unknown,
                InterfaceType::Unknown,
                "",
            ),
        ]
    }

    fn buds_call_route() -> Found {
        found(
            "aaudio:7",
            "Buds Pro (Headset)",
            DeviceType::Headset,
            InterfaceType::Bluetooth,
            "AA:BB",
        )
    }

    fn buds_music_route() -> Found {
        found(
            "aaudio:8",
            "Buds Pro",
            DeviceType::Unknown,
            InterfaceType::Bluetooth,
            "AA:BB",
        )
    }

    #[test]
    fn the_phones_speaker_is_listed_once_without_the_earpiece_or_telephony() {
        assert_eq!(ids(phone()), ["aaudio:2"]);
    }

    #[test]
    fn the_speaker_wins_over_the_safe_speaker_in_either_order() {
        assert_eq!(
            ids(vec![safe(speaker("aaudio:3")), speaker("aaudio:2")]),
            ["aaudio:2"]
        );
        assert_eq!(
            ids(vec![speaker("aaudio:2"), safe(speaker("aaudio:3"))]),
            ["aaudio:2"]
        );
    }

    #[test]
    fn the_routes_folded_in_are_remembered_whichever_comes_first() {
        let listed = list(vec![safe(speaker("aaudio:3")), speaker("aaudio:2")]);
        assert_eq!(listed[0].same_as, ["aaudio:3"]);
        let listed = list(vec![speaker("aaudio:2"), safe(speaker("aaudio:3"))]);
        assert_eq!(listed[0].same_as, ["aaudio:3"]);
    }

    #[test]
    fn a_product_name_loses_the_type_cpal_added_to_it() {
        let words = words(vec![
            found(
                "aaudio:10",
                "Pixel Buds (Headset)",
                DeviceType::Headset,
                InterfaceType::Bluetooth,
                "AA:BB",
            ),
            found(
                "aaudio:11",
                "Dongle (USB)",
                DeviceType::Speaker,
                InterfaceType::Usb,
                "card=1",
            ),
        ]);
        assert_eq!(words[0].0, "Bluetooth: Pixel Buds");
        assert_eq!(
            words[1].0, "USB: Dongle (USB)",
            "only the type's own words go"
        );
    }

    #[test]
    fn a_bluetooth_headset_is_listed_once_by_its_music_route() {
        let mut found = phone();
        found.push(buds_call_route());
        found.push(buds_music_route());
        assert_eq!(ids(found), ["aaudio:2", "aaudio:8"]);
    }

    #[test]
    fn a_headset_on_its_call_route_alone_stays() {
        assert_eq!(ids(vec![buds_call_route()]), ["aaudio:7"]);
    }

    #[test]
    fn outputs_that_differ_stay_in_the_hosts_order() {
        let mut outputs = phone();
        outputs.push(found(
            "aaudio:9",
            "Pixel 7 (Headphones)",
            DeviceType::Headphones,
            InterfaceType::Unknown,
            "",
        ));
        outputs.push(found(
            "aaudio:10",
            "USB DAC",
            DeviceType::Unknown,
            InterfaceType::Usb,
            "card=1;device=0",
        ));
        outputs.push(found(
            "aaudio:11",
            "Buds",
            DeviceType::Unknown,
            InterfaceType::Bluetooth,
            "AA:BB",
        ));
        outputs.push(found(
            "aaudio:12",
            "Car",
            DeviceType::Unknown,
            InterfaceType::Bluetooth,
            "CC:DD",
        ));
        assert_eq!(
            ids(outputs),
            [
                "aaudio:2",
                "aaudio:9",
                "aaudio:10",
                "aaudio:11",
                "aaudio:12"
            ]
        );
    }

    #[test]
    fn android_outputs_are_named_for_what_they_are() {
        let mut outputs = phone();
        outputs.push(found(
            "aaudio:9",
            "Pixel 7 (Headphones)",
            DeviceType::Headphones,
            InterfaceType::Unknown,
            "",
        ));
        outputs.push(found(
            "aaudio:10",
            "USB DAC",
            DeviceType::Unknown,
            InterfaceType::Usb,
            "card=1;device=0",
        ));
        outputs.push(buds_call_route());
        outputs.push(buds_music_route());
        let expected = [
            ("Phone speaker", "Built-in"),
            ("Headphones", "Wired"),
            ("USB: USB DAC", "USB"),
            ("Bluetooth: Buds Pro", "Bluetooth"),
        ];
        assert_eq!(
            words(outputs),
            expected.map(|(name, detail)| (name.to_owned(), detail.to_owned()))
        );
    }

    #[test]
    fn no_name_carries_an_internal_id_or_type_tags() {
        for (name, detail) in words(phone()) {
            for text in [name, detail] {
                assert!(!text.contains("aaudio") && !text.contains('['), "{text}");
            }
        }
    }

    #[test]
    fn alsa_routes_to_one_card_stay_apart_with_their_names_and_pcm_ids() {
        let loopback = |pcm: &str| {
            found(
                &format!("alsa:{pcm}:CARD=Loopback,DEV=0"),
                "Loopback, Loopback PCM",
                DeviceType::Unknown,
                InterfaceType::Unknown,
                "",
            )
        };
        let listed = list(vec![loopback("hw"), loopback("plughw"), loopback("dmix")]);
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[1].name, "Loopback, Loopback PCM");
        assert_eq!(listed[1].detail, "plughw:CARD=Loopback,DEV=0");
        assert_eq!(
            listed[1].title,
            "Loopback, Loopback PCM (plughw:CARD=Loopback,DEV=0)"
        );
    }
}
