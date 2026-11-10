//! How a Mac's output is connected. cpal types every Core Audio device as
//! unknown unless it is an aggregate, so the transport type Core Audio
//! keeps for each device is read here and told to the rest as the interface
//! type Android's outputs come with.

use cpal::InterfaceType;

// Core Audio's four-character codes, as in AudioHardwareBase.h. They are
// spelled out here so the mapping below is tested on every platform.
const BUILT_IN: u32 = u32::from_be_bytes(*b"bltn");
const USB: u32 = u32::from_be_bytes(*b"usb ");
const BLUETOOTH: u32 = u32::from_be_bytes(*b"blue");
const BLUETOOTH_LE: u32 = u32::from_be_bytes(*b"blea");
const HDMI: u32 = u32::from_be_bytes(*b"hdmi");
const DISPLAY_PORT: u32 = u32::from_be_bytes(*b"dprt");
const PCI: u32 = u32::from_be_bytes(*b"pci ");
const FIRE_WIRE: u32 = u32::from_be_bytes(*b"1394");
const THUNDERBOLT: u32 = u32::from_be_bytes(*b"thun");
const AIR_PLAY: u32 = u32::from_be_bytes(*b"airp");
const AVB: u32 = u32::from_be_bytes(*b"eavb");
const VIRTUAL: u32 = u32::from_be_bytes(*b"virt");
const AGGREGATE: u32 = u32::from_be_bytes(*b"grup");

fn interface_of(transport: u32) -> InterfaceType {
    match transport {
        BUILT_IN => InterfaceType::BuiltIn,
        USB => InterfaceType::Usb,
        BLUETOOTH | BLUETOOTH_LE => InterfaceType::Bluetooth,
        HDMI => InterfaceType::Hdmi,
        DISPLAY_PORT => InterfaceType::DisplayPort,
        PCI => InterfaceType::Pci,
        FIRE_WIRE => InterfaceType::FireWire,
        THUNDERBOLT => InterfaceType::Thunderbolt,
        AIR_PLAY | AVB => InterfaceType::Network,
        VIRTUAL => InterfaceType::Virtual,
        AGGREGATE => InterfaceType::Aggregate,
        _ => InterfaceType::Unknown,
    }
}

/// cpal keeps a device's Core Audio id out of reach, but prints it in the
/// device's debug output, as `Device { audio_device_id: 73, name: .. }`.
fn audio_device_id_in(debug: &str) -> Option<u32> {
    let (_, after) = debug.split_once("audio_device_id: ")?;
    let digits = after.split(|c: char| !c.is_ascii_digit()).next()?;
    digits.parse().ok()
}

#[cfg(target_os = "macos")]
pub(crate) use macos::with_interface;

#[cfg(target_os = "macos")]
mod macos {
    use std::ptr::{NonNull, null};

    use cpal::{DeviceDescription, DeviceDescriptionBuilder, InterfaceType};
    use objc2_core_audio::{
        AudioDeviceID, AudioObjectGetPropertyData, AudioObjectPropertyAddress,
        kAudioDevicePropertyTransportType, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal,
    };

    use super::{audio_device_id_in, interface_of};

    /// `description` with how the device is connected, where cpal gave none.
    /// cpal's description on macOS holds nothing else to carry over than
    /// the name and what the device is for.
    pub(crate) fn with_interface(
        device: &cpal::Device,
        description: DeviceDescription,
    ) -> DeviceDescription {
        if description.interface_type() != InterfaceType::Unknown {
            return description;
        }
        let transport = audio_device_id_in(&format!("{device:?}")).and_then(transport_type);
        let Some(transport) = transport else {
            return description;
        };
        DeviceDescriptionBuilder::new(description.name())
            .direction(description.direction())
            .device_type(description.device_type())
            .interface_type(interface_of(transport))
            .build()
    }

    fn transport_type(device: AudioDeviceID) -> Option<u32> {
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyTransportType,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut transport: u32 = 0;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: the property is a single UInt32 and `size` says so; the
        // pointers outlive the call, and the value is used only on success.
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&address),
                0,
                null(),
                NonNull::from(&mut size),
                NonNull::from(&mut transport).cast(),
            )
        };
        (status == 0).then_some(transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_are_the_ones_core_audio_defines() {
        assert_eq!(BLUETOOTH, 0x626c7565);
        assert_eq!(BLUETOOTH_LE, 0x626c6561);
        assert_eq!(USB, 0x75736220);
        assert_eq!(HDMI, 0x68646d69);
        assert_eq!(BUILT_IN, 0x626c746e);
        assert_eq!(AGGREGATE, 0x67727570);
    }

    #[test]
    fn a_transport_becomes_the_interface_it_is() {
        assert_eq!(interface_of(BLUETOOTH_LE), InterfaceType::Bluetooth);
        assert_eq!(interface_of(USB), InterfaceType::Usb);
        assert_eq!(interface_of(HDMI), InterfaceType::Hdmi);
        assert_eq!(interface_of(DISPLAY_PORT), InterfaceType::DisplayPort);
        assert_eq!(interface_of(BUILT_IN), InterfaceType::BuiltIn);
        assert_eq!(interface_of(AIR_PLAY), InterfaceType::Network);
        assert_eq!(interface_of(0), InterfaceType::Unknown);
    }

    #[test]
    fn the_id_is_read_from_the_debug_output() {
        let debug = r#"Device { audio_device_id: 73, name: Ok("Speakers") }"#;
        assert_eq!(audio_device_id_in(debug), Some(73));
        assert_eq!(audio_device_id_in("Device"), None);
    }
}
