//! Core Audio's side of exclusive mode: the properties of one device that
//! hog mode and its rate are made of.

use std::ptr::{NonNull, null};
use std::time::{Duration, Instant};

use objc2_core_audio::{
    AudioDeviceID, AudioObjectGetPropertyData, AudioObjectPropertyAddress,
    AudioObjectPropertySelector, AudioObjectSetPropertyData, kAudioDevicePropertyHogMode,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwareUnspecifiedError,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
};

use crate::exclusive::{Hardware, Status};
use crate::transport::audio_device_id_in;

/// How long a device is given to switch to a rate, and how often it is
/// looked at meanwhile. A USB DAC can take several seconds.
const RATE_SWITCH_TIMEOUT: Duration = Duration::from_secs(5);
const RATE_SWITCH_LOOK: Duration = Duration::from_millis(20);

pub(crate) struct CoreAudioDevice(AudioDeviceID);

impl CoreAudioDevice {
    /// The device cpal found, as Core Audio knows it.
    pub fn of(device: &cpal::Device) -> Option<Self> {
        audio_device_id_in(&format!("{device:?}")).map(Self)
    }

    fn read<T: Default>(&self, selector: AudioObjectPropertySelector) -> Result<T, Status> {
        let address = global(selector);
        let mut value = T::default();
        let mut size = size_of::<T>() as u32;
        // SAFETY: `T` is the one plain number the property is, `size` says
        // so, and the pointers outlive the call; `value` is used only on
        // success.
        let status = unsafe {
            AudioObjectGetPropertyData(
                self.0,
                NonNull::from(&address),
                0,
                null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast(),
            )
        };
        if status == 0 { Ok(value) } else { Err(status) }
    }

    fn write<T>(&self, selector: AudioObjectPropertySelector, value: T) -> Result<(), Status> {
        let address = global(selector);
        // SAFETY: `T` is the one plain number the property is, and the
        // pointers outlive the call.
        let status = unsafe {
            AudioObjectSetPropertyData(
                self.0,
                NonNull::from(&address),
                0,
                null(),
                size_of::<T>() as u32,
                NonNull::from(&value).cast(),
            )
        };
        if status == 0 { Ok(()) } else { Err(status) }
    }
}

impl Hardware for CoreAudioDevice {
    fn hog_owner(&self) -> Result<i32, Status> {
        self.read::<i32>(kAudioDevicePropertyHogMode)
    }

    fn set_hog_owner(&self, owner: i32) -> Result<(), Status> {
        self.write(kAudioDevicePropertyHogMode, owner)
    }

    fn rate(&self) -> Result<u32, Status> {
        self.read::<f64>(kAudioDevicePropertyNominalSampleRate)
            .map(|rate| rate.round() as u32)
    }

    fn set_rate(&self, rate: u32) -> Result<(), Status> {
        self.write(kAudioDevicePropertyNominalSampleRate, f64::from(rate))?;
        // The device changes its rate after the call has returned.
        let give_up = Instant::now() + RATE_SWITCH_TIMEOUT;
        while Instant::now() < give_up {
            if self.rate()? == rate {
                return Ok(());
            }
            std::thread::sleep(RATE_SWITCH_LOOK);
        }
        Err(kAudioHardwareUnspecifiedError)
    }
}

fn global(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}
