//! Which of the outputs a host lists are worth choosing between, and what to
//! call them. Android lists every route its audio system has, so one speaker
//! shows up several times under names made for programs; ALSA's routes to one
//! card differ in what they do to the sound, so those stay as they are.
//! PipeWire names each output it mixes for, so beside it only ALSA's routes
//! straight to the hardware, which go around it, are worth listing.

use cpal::{DeviceDescription, DeviceDirection, DeviceType, InterfaceType};

/// An output to choose from: its `name` and a short `detail` to tell it from
/// others of the name, and `title`, which says all of that in one line;
/// its `kind`, shown beside the name. `key` is what it goes by from one
/// connection to the next, for what is kept about it. `same_as` holds the
/// ids of the routes that were folded into it, which still play on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: String,
    pub key: String,
    pub name: String,
    pub detail: String,
    pub title: String,
    pub kind: OutputKind,
    pub same_as: Vec<String>,
}

/// What sort of output it is, as far as the system says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputKind {
    #[default]
    Unknown,
    Speaker,
    Headphones,
    Headset,
    Bluetooth,
    Usb,
    Hdmi,
}

impl OutputKind {
    /// What the sort is called, or nothing for one not known.
    pub fn name(self) -> &'static str {
        match self {
            OutputKind::Unknown => "",
            OutputKind::Speaker => "Speaker",
            OutputKind::Headphones => "Headphones",
            OutputKind::Headset => "Headset",
            OutputKind::Bluetooth => "Bluetooth",
            OutputKind::Usb => "USB",
            OutputKind::Hdmi => "HDMI",
        }
    }
}

/// What the host says of one output.
pub(crate) struct Found {
    pub id: String,
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
const PIPEWIRE_HOST_PREFIX: &str = "pipewire:";

/// PipeWire's stand-ins for whichever output is the default, which follow
/// it to another output and don't say which one they are on.
const PIPEWIRE_STAND_INS: [&str; 2] = ["pipewire:sink_default", "pipewire:output_default"];

/// The outputs to choose from, once each, in the hosts' order.
pub(crate) fn list(found: Vec<Found>) -> Vec<OutputDevice> {
    let outputs = worth_listing(found).into_iter().map(Output::from).collect();
    distinct(outputs).into_iter().map(describe).collect()
}

/// Whether the device id is a stand-in for the system's default rather
/// than an output of its own.
pub(crate) fn is_stand_in(id: &str) -> bool {
    PIPEWIRE_STAND_INS.contains(&id)
}

/// The output among those `listed` that PipeWire's stand-in plays on:
/// `default_sink`, the node PipeWire plays on by default, where it is
/// listed, or else the first.
pub(crate) fn behind_the_stand_in(
    default_sink: Option<&str>,
    listed: &[OutputDevice],
) -> Option<String> {
    let default = default_sink.map(|name| format!("{PIPEWIRE_HOST_PREFIX}{name}"));
    listed
        .iter()
        .find(|output| Some(&output.id) == default.as_ref())
        .or(listed.first())
        .map(|output| output.id.clone())
}

/// Whether a device id is the hardware itself rather than a sound server or a
/// plugin that converts.
pub(crate) fn exclusive(id: &str) -> bool {
    id.split_once(':')
        .is_some_and(|(_, pcm)| pcm.starts_with("hw:"))
}

/// With PipeWire there, its outputs and ALSA's to the hardware; PipeWire
/// lists programs' streams and its stand-ins too, which are no outputs, and
/// cpal gives its outputs as able to record what they play as well.
fn worth_listing(found: Vec<Found>) -> Vec<Found> {
    let pipewire = found
        .iter()
        .any(|found| found.id.starts_with(PIPEWIRE_HOST_PREFIX));
    found
        .into_iter()
        .filter(|found| {
            if found.id.starts_with(PIPEWIRE_HOST_PREFIX) {
                found.description.direction() == DeviceDirection::Duplex && !is_stand_in(&found.id)
            } else {
                !pipewire || exclusive(&found.id)
            }
        })
        .collect()
}

impl From<Found> for Output {
    fn from(found: Found) -> Self {
        Output {
            // Not the description as cpal prints it, which adds its kind
            // after the name, as "via Bluetooth": the kind is an icon.
            name: found.description.name().to_owned(),
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

/// A name for the listener, and what sort of output it is.
fn describe(output: Output) -> OutputDevice {
    let key = key(&output);
    let kind = kind_of(output.kind, output.interface);
    let (name, detail, title) = if output.id.starts_with(ANDROID_HOST_PREFIX) {
        let name = android_name(&output);
        (name.clone(), String::new(), name)
    } else if output.id.starts_with(PIPEWIRE_HOST_PREFIX) {
        (output.name.clone(), String::new(), output.name)
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
        key,
        name,
        detail,
        title,
        kind,
        same_as: output.same_as,
    }
}

/// What an output goes by from one connection to the next: its id, except on
/// Android, which numbers its outputs anew each time one connects. There it
/// is the address Android gives a headset or a USB card, or else its type
/// and name, as for the speaker.
fn key(output: &Output) -> String {
    if !output.id.starts_with(ANDROID_HOST_PREFIX) {
        return output.id.clone();
    }
    let interface = output.interface;
    match &output.address {
        Some(address) => format!("{ANDROID_HOST_PREFIX}{interface}/{address}"),
        None => format!(
            "{ANDROID_HOST_PREFIX}{interface}/{}/{}",
            output.kind, output.name
        ),
    }
}

/// What sort of output the system says it is: how it is connected first,
/// since a Bluetooth or USB one is often a speaker or headphones too. ALSA
/// and macOS say neither, and an output on a PCI card may be the speaker,
/// the headphone jack or a screen's HDMI, so those stay unknown. A
/// DisplayPort output is a screen too and shares HDMI's icon.
fn kind_of(kind: DeviceType, interface: InterfaceType) -> OutputKind {
    match (kind, interface) {
        (_, InterfaceType::Bluetooth) => OutputKind::Bluetooth,
        (_, InterfaceType::Usb) => OutputKind::Usb,
        (_, InterfaceType::Hdmi | InterfaceType::DisplayPort) => OutputKind::Hdmi,
        (DeviceType::Speaker, _) => OutputKind::Speaker,
        (DeviceType::Headphones, _) => OutputKind::Headphones,
        (DeviceType::Headset, _) => OutputKind::Headset,
        _ => OutputKind::Unknown,
    }
}

/// What the output is called, by what Android says it is: its own name is
/// the phone's model for the speaker and the earpiece alike, and a wired
/// one's has nothing of the headphones in it.
fn android_name(output: &Output) -> String {
    match (output.kind, output.interface) {
        (DeviceType::Speaker, InterfaceType::BuiltIn) => "Phone speaker".into(),
        (_, InterfaceType::Bluetooth | InterfaceType::Usb) => product_name(output),
        (_, InterfaceType::Hdmi) => OutputKind::Hdmi.name().into(),
        (DeviceType::Headphones, _) => OutputKind::Headphones.name().into(),
        (DeviceType::Headset, _) => OutputKind::Headset.name().into(),
        (DeviceType::HearingAid, _) => "Hearing aid".into(),
        (DeviceType::Dock, _) => "Dock".into(),
        _ => product_name(output),
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

    fn kinds(found: Vec<Found>) -> Vec<(String, OutputKind)> {
        list(found)
            .into_iter()
            .map(|output| (output.name, output.kind))
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
        assert_eq!(words[0].0, "Pixel Buds");
        assert_eq!(words[1].0, "Dongle (USB)", "only the type's own words go");
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
        outputs.push(found(
            "aaudio:13",
            "TV",
            DeviceType::Unknown,
            InterfaceType::Hdmi,
            "",
        ));
        let expected = [
            ("Phone speaker", OutputKind::Speaker),
            ("Headphones", OutputKind::Headphones),
            ("USB DAC", OutputKind::Usb),
            ("Buds Pro", OutputKind::Bluetooth),
            ("HDMI", OutputKind::Hdmi),
        ];
        let listed = list(outputs);
        let named: Vec<_> = listed
            .iter()
            .map(|output| (output.name.as_str(), output.kind))
            .collect();
        assert_eq!(named, expected);
        assert!(
            listed.iter().all(|output| output.detail.is_empty()),
            "the kind's icon says what a line under the name would"
        );
    }

    #[test]
    fn an_android_output_keeps_its_key_when_it_connects_again_under_another_id() {
        let keys = |found: Vec<Found>| -> Vec<String> {
            list(found).into_iter().map(|output| output.key).collect()
        };
        let mut buds_again = buds_music_route();
        buds_again.id = "aaudio:15".into();
        let before = keys(vec![speaker("aaudio:2"), buds_music_route()]);
        assert_eq!(before, keys(vec![speaker("aaudio:3"), buds_again]));
        assert_ne!(before[0], before[1]);
        assert_eq!(
            keys(vec![buds_call_route()]),
            keys(vec![buds_music_route()]),
            "one headset, whichever route lists it"
        );
        let other_buds = found(
            "aaudio:16",
            "Buds Pro",
            DeviceType::Unknown,
            InterfaceType::Bluetooth,
            "CC:DD",
        );
        assert_ne!(
            keys(vec![other_buds]),
            keys(vec![buds_music_route()]),
            "another of the same headset"
        );
        assert_eq!(
            keys(vec![alsa("hw:CARD=PCH,DEV=0")]),
            ["alsa:hw:CARD=PCH,DEV=0"],
            "elsewhere its id"
        );
    }

    #[test]
    fn how_an_output_is_connected_comes_before_what_it_is() {
        let cases = [
            (
                DeviceType::Headset,
                InterfaceType::Bluetooth,
                OutputKind::Bluetooth,
            ),
            (
                DeviceType::Speaker,
                InterfaceType::Bluetooth,
                OutputKind::Bluetooth,
            ),
            (DeviceType::Headset, InterfaceType::Usb, OutputKind::Usb),
            (DeviceType::Unknown, InterfaceType::Hdmi, OutputKind::Hdmi),
            (
                DeviceType::Unknown,
                InterfaceType::DisplayPort,
                OutputKind::Hdmi,
            ),
            (
                DeviceType::Speaker,
                InterfaceType::BuiltIn,
                OutputKind::Speaker,
            ),
            (
                DeviceType::Headphones,
                InterfaceType::Unknown,
                OutputKind::Headphones,
            ),
            (
                DeviceType::Headset,
                InterfaceType::Unknown,
                OutputKind::Headset,
            ),
        ];
        for (kind, interface, expected) in cases {
            assert_eq!(
                kind_of(kind, interface),
                expected,
                "{kind:?} on {interface:?}"
            );
        }
    }

    #[test]
    fn an_output_the_system_says_too_little_of_is_of_no_known_kind() {
        for (kind, interface) in [
            (DeviceType::Unknown, InterfaceType::Unknown),
            (DeviceType::Unknown, InterfaceType::Pci),
            (DeviceType::Unknown, InterfaceType::BuiltIn),
            (DeviceType::Unknown, InterfaceType::Line),
            (DeviceType::HearingAid, InterfaceType::Unknown),
            (DeviceType::Dock, InterfaceType::Unknown),
        ] {
            assert_eq!(
                kind_of(kind, interface),
                OutputKind::Unknown,
                "{kind:?} on {interface:?}"
            );
        }
    }

    #[test]
    fn no_name_carries_an_internal_id_or_type_tags() {
        for (name, detail) in words(phone()) {
            for text in [name, detail] {
                assert!(!text.contains("aaudio") && !text.contains('['), "{text}");
            }
        }
    }

    /// cpal types a PipeWire output's interface by its bus, and Bluetooth
    /// by its API.
    fn pipewire(id: &str, name: &str, direction: DeviceDirection) -> Found {
        let interface = if id.starts_with("bluez_output.") {
            InterfaceType::Bluetooth
        } else if id.starts_with("alsa_output.pci-") {
            InterfaceType::Pci
        } else {
            InterfaceType::Unknown
        };
        Found {
            id: format!("pipewire:{id}"),
            description: DeviceDescriptionBuilder::new(name)
                .direction(direction)
                .interface_type(interface)
                .build(),
            safe_speaker: false,
        }
    }

    fn alsa(pcm: &str) -> Found {
        found(
            &format!("alsa:{pcm}"),
            "HDA Intel PCH, ALC3246 Analog",
            DeviceType::Unknown,
            InterfaceType::Unknown,
            "",
        )
    }

    /// What cpal lists on a laptop with Bluetooth headphones on, while a
    /// browser plays.
    fn laptop_with_pipewire() -> Vec<Found> {
        vec![
            pipewire("sink_default", "default_sink", DeviceDirection::Duplex),
            pipewire("output_default", "default_output", DeviceDirection::Output),
            pipewire(
                "alsa_output.pci-0000_00_1f.3.analog-stereo",
                "Built-in Audio Analog Stereo",
                DeviceDirection::Duplex,
            ),
            pipewire(
                "alsa_input.pci-0000_00_1f.3.analog-stereo",
                "Built-in Audio Analog Stereo",
                DeviceDirection::Input,
            ),
            pipewire(
                "bluez_output.AA_BB_CC_DD_EE_FF.1",
                "WH-1000XM4",
                DeviceDirection::Duplex,
            ),
            pipewire("Firefox", "Firefox", DeviceDirection::Output),
            alsa("hw:CARD=PCH,DEV=0"),
            alsa("plughw:CARD=PCH,DEV=0"),
            alsa("pipewire"),
            alsa("default"),
        ]
    }

    #[test]
    fn beside_pipewires_outputs_only_alsas_to_the_hardware_are_listed() {
        assert_eq!(
            ids(laptop_with_pipewire()),
            [
                "pipewire:alsa_output.pci-0000_00_1f.3.analog-stereo",
                "pipewire:bluez_output.AA_BB_CC_DD_EE_FF.1",
                "alsa:hw:CARD=PCH,DEV=0",
            ]
        );
    }

    #[test]
    fn behind_pipewires_stand_in_is_its_default_sink_not_the_first_listed() {
        let listed = list(laptop_with_pipewire());
        let first = Some("pipewire:alsa_output.pci-0000_00_1f.3.analog-stereo");
        assert_eq!(
            behind_the_stand_in(Some("bluez_output.AA_BB_CC_DD_EE_FF.1"), &listed).as_deref(),
            Some("pipewire:bluez_output.AA_BB_CC_DD_EE_FF.1")
        );
        assert_eq!(behind_the_stand_in(None, &listed).as_deref(), first);
        assert_eq!(behind_the_stand_in(Some("gone"), &listed).as_deref(), first);
    }

    #[test]
    fn without_pipewire_alsa_lists_every_route() {
        let routes = ["hw:CARD=PCH,DEV=0", "plughw:CARD=PCH,DEV=0", "default"];
        assert_eq!(
            ids(routes.map(alsa).into()),
            routes.map(|pcm| format!("alsa:{pcm}"))
        );
    }

    #[test]
    fn pipewires_and_alsas_outputs_are_of_the_kind_cpal_says() {
        assert_eq!(
            kinds(laptop_with_pipewire()),
            [
                (
                    "Built-in Audio Analog Stereo".to_owned(),
                    OutputKind::Unknown
                ),
                ("WH-1000XM4".to_owned(), OutputKind::Bluetooth),
                (
                    "HDA Intel PCH, ALC3246 Analog".to_owned(),
                    OutputKind::Unknown
                ),
            ]
        );
    }

    #[test]
    fn a_pipewire_output_goes_by_the_name_pipewire_gives_it() {
        let listed = list(laptop_with_pipewire());
        assert_eq!(listed[1].name, "WH-1000XM4");
        assert_eq!(listed[1].detail, "");
        assert_eq!(listed[1].title, "WH-1000XM4");
        assert!(is_stand_in("pipewire:output_default"));
        assert!(!is_stand_in(&listed[1].id));
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
