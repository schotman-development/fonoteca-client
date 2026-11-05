//! What the system does to the output in use without saying so to the
//! stream. It switches the sound over by itself: headphones plugged into or
//! pulled out of the card played on, on Linux, where the card turns its
//! speaker on or off without the output going; another output becoming the
//! default, on macOS. Each switch pauses what plays as a press of pause does,
//! and playing on is on the output in use. And PipeWire leaves a stream
//! whose output went unlinked, asking for no more sound and told of nothing,
//! which is taken as the output gone.

use std::sync::Arc;
use std::time::Instant;

use cpal::ErrorKind;

use crate::output::Shared;

/// How long one look for a switch lasts, before the watch checks that the
/// output it looks at is still the one in use.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const LOOK: std::time::Duration = std::time::Duration::from_millis(500);

/// Watches `device`, found for `generation`, on a thread of its own until
/// another is found or the player quits.
pub(crate) fn watch(shared: Arc<Shared>, generation: u64, device: &cpal::Device) {
    let Some(look) = switches(device) else {
        return;
    };
    std::thread::Builder::new()
        .name("output switches".into())
        .spawn(move || watch_until_let_go(&shared, generation, look))
        .expect("a thread to watch the output");
}

/// Pauses on each switch `look` sees, and has the output gone once its
/// stream went quiet, until the output is no longer the one in use.
fn watch_until_let_go(shared: &Shared, generation: u64, mut look: impl FnMut() -> bool) {
    loop {
        let switched = look();
        let mut ring = shared.ring.lock().unwrap();
        if ring.quit || ring.generation != generation {
            return;
        }
        if ring.went_quiet(Instant::now()) {
            ring.stream_failed(&ErrorKind::DeviceNotAvailable.into());
        } else if switched {
            ring.paused = true;
        } else {
            continue;
        }
        drop(ring);
        shared.changed.notify_all();
    }
}

#[cfg(target_os = "linux")]
fn switches(device: &cpal::Device) -> Option<impl FnMut() -> bool + Send + use<>> {
    use cpal::traits::DeviceTrait;

    let id = device.id().ok()?.to_string();
    let description = device.description().ok()?;
    let mut jacks = card(&id, &description).and_then(|card| Jacks::open(&card));
    Some(move || {
        if let Some(switched) = jacks.as_ref().and_then(Jacks::look) {
            return switched;
        }
        // No card to watch, or it went with the output: the stream going
        // quiet is still looked for.
        jacks = None;
        std::thread::sleep(LOOK);
        false
    })
}

#[cfg(target_os = "macos")]
fn switches(_device: &cpal::Device) -> Option<impl FnMut() -> bool + Send + use<>> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let default = || {
        cpal::default_host()
            .default_output_device()
            .and_then(|device| device.id().ok())
    };
    let mut was = default();
    Some(move || {
        std::thread::sleep(LOOK);
        let now = default();
        std::mem::replace(&mut was, now.clone()) != now
    })
}

/// Android says when it is about to move the sound, and its streams say
/// when their output went.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn switches(_device: &cpal::Device) -> Option<fn() -> bool> {
    None
}

/// The control device of the card an output plays on, if it is one card's.
/// ALSA's pcm names it, as in `hw:CARD=PCH,DEV=0`; PipeWire's outputs on a
/// card carry such a pcm, as in `front:0` or `hdmi:0,3`.
#[cfg(target_os = "linux")]
fn card(id: &str, description: &cpal::DeviceDescription) -> Option<String> {
    let pcm = match id.split_once(':')? {
        ("alsa", pcm) => pcm,
        ("pipewire", _)
            if description
                .driver()
                .is_some_and(|driver| driver.starts_with("api.alsa.")) =>
        {
            description.address()?
        }
        _ => return None,
    };
    let (_, card_and_device) = pcm.rsplit_once(':')?;
    let card = card_and_device.split(',').next()?;
    let card = card.strip_prefix("CARD=").unwrap_or(card);
    (!card.is_empty()).then(|| format!("hw:{card}"))
}

/// A card's controls, among them its jacks, told of each change.
#[cfg(target_os = "linux")]
struct Jacks(alsa::Ctl);

#[cfg(target_os = "linux")]
impl Jacks {
    fn open(card: &str) -> Option<Jacks> {
        let controls = alsa::Ctl::new(card, true).ok()?;
        controls.subscribe_events(true).ok()?;
        Some(Jacks(controls))
    }

    /// Waits for a change to the controls, and says whether a jack changed;
    /// none once the card can't be watched.
    fn look(&self) -> Option<bool> {
        use alsa::PollDescriptors;
        use alsa::poll::Flags;

        let mut descriptors = self.0.get().ok()?;
        alsa::poll::poll(&mut descriptors, LOOK.as_millis() as i32).ok()?;
        if self
            .0
            .revents(&descriptors)
            .ok()?
            .intersects(Flags::ERR | Flags::HUP)
        {
            return None;
        }
        let mut switched = false;
        while let Ok(Some(event)) = self.0.read() {
            switched |= event.get_mask().value() && event.get_id().get_name().is_ok_and(is_jack);
        }
        Some(switched)
    }
}

/// The kernel names a card's jacks for what plugs in, ending in "Jack": a
/// microphone's among them, which pauses too rather than miss a headset. A
/// screen's comes and goes as the screen sleeps, and never moves the sound.
#[cfg(target_os = "linux")]
fn is_jack(control: &str) -> bool {
    control.ends_with(" Jack") && !control.starts_with("HDMI")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::output::Device;

    #[test]
    fn a_switch_pauses_until_another_output_is_in_use() {
        let shared = Shared::default();
        let mut looks = 0;
        watch_until_let_go(&shared, 0, || {
            looks += 1;
            let mut ring = shared.ring.lock().unwrap();
            match looks {
                1 => false,
                2 => {
                    assert!(!ring.paused, "nothing switched");
                    true
                }
                3 => {
                    assert!(ring.paused, "the switch paused it");
                    assert!(!ring.device_lost, "the output is still there");
                    ring.paused = false;
                    ring.generation += 1;
                    true
                }
                _ => unreachable!("it stops once another output is in use"),
            }
        });
        assert!(
            !shared.ring.lock().unwrap().paused,
            "a switch on an output let go of is no pause"
        );
    }

    #[test]
    fn a_stream_gone_quiet_is_its_output_gone() {
        let shared = Shared::default();
        {
            let mut ring = shared.ring.lock().unwrap();
            ring.device = Some(Device {
                name: "WH-1000XM4".into(),
                ..Default::default()
            });
            ring.asked_at = Some(Instant::now() - Duration::from_secs(5));
        }
        let mut looks = 0;
        watch_until_let_go(&shared, 0, || {
            looks += 1;
            let mut ring = shared.ring.lock().unwrap();
            if looks == 2 {
                assert!(ring.paused && ring.device_lost, "lost on the first look");
                assert_eq!(ring.problem.as_deref(), Some("WH-1000XM4 is unavailable."));
                ring.quit = true;
            }
            false
        });
        assert_eq!(looks, 2, "it ends as the player quits");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_card_is_found_in_alsas_pcm_and_in_pipewires_path_on_a_card() {
        use cpal::DeviceDescriptionBuilder;

        let plain = DeviceDescriptionBuilder::new("Card").build();
        let on_card = |path: &str| {
            DeviceDescriptionBuilder::new("Built-in Audio")
                .driver("api.alsa.pcm.sink")
                .address(path)
                .build()
        };
        let bluetooth = DeviceDescriptionBuilder::new("WH-1000XM4")
            .driver("api.bluez5.a2dp.sink")
            .address("AA:BB:CC:DD:EE:FF")
            .build();
        let cases = [
            ("alsa:hw:CARD=PCH,DEV=0", &plain, Some("hw:PCH")),
            ("alsa:sysdefault:CARD=DAC", &plain, Some("hw:DAC")),
            ("alsa:default", &plain, None),
            ("alsa:pipewire", &plain, None),
            (
                "pipewire:alsa_output.analog",
                &on_card("front:0"),
                Some("hw:0"),
            ),
            (
                "pipewire:alsa_output.hdmi",
                &on_card("hdmi:1,3"),
                Some("hw:1"),
            ),
            (
                "pipewire:alsa_output.ucm",
                &on_card("_ucm0001.hw:sofhdadsp,0"),
                Some("hw:sofhdadsp"),
            ),
            ("pipewire:bluez_output.AA_BB", &bluetooth, None),
            ("coreaudio:BuiltInSpeakerDevice", &plain, None),
        ];
        for (id, description, expected) in cases {
            assert_eq!(card(id, description).as_deref(), expected, "{id}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_jack_is_a_control_named_for_one() {
        for jack in [
            "Headphone Jack",
            "Front Headphone Jack",
            "Line Out Jack",
            "Mic Jack",
        ] {
            assert!(is_jack(jack), "{jack}");
        }
        for control in [
            "Master Playback Volume",
            "Auto-Mute Mode",
            "Jack",
            "HDMI/DP,pcm=3 Jack",
        ] {
            assert!(!is_jack(control), "{control}");
        }
    }
}
