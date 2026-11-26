//! Exclusive mode on a Mac: the process takes the sound device for itself
//! (Core Audio's "hog mode") and sets it to the rate of what plays, so
//! that the system's mixer neither shares the device nor converts the rate.
//! What Core Audio does is behind `Hardware`; the decisions (when to take,
//! what to say, what to give back and in which order) are here, and run on
//! every platform.

use cpal::InterfaceType;

/// Core Audio's `OSStatus`.
pub(crate) type Status = i32;

/// The hog mode owner of a device nobody holds.
const NOBODY: i32 = -1;

/// Whether the output menu offers exclusive mode for a device.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ExclusiveOffer {
    /// Not on this platform, or not for this sort of device.
    #[default]
    NotOffered,
    Offered,
    /// Offered, but another app holds the device now.
    InUseByAnotherApp,
}

/// Only a Mac has it. A device made of others or the Mac's own speakers is
/// not one to take for oneself: the first has no hardware of its own to
/// hold, and the speakers are shared with the system's sounds.
pub(crate) fn offered(interface: InterfaceType) -> ExclusiveOffer {
    if cfg!(target_os = "macos") && suits(interface) {
        ExclusiveOffer::Offered
    } else {
        ExclusiveOffer::NotOffered
    }
}

fn suits(interface: InterfaceType) -> bool {
    !matches!(interface, InterfaceType::Aggregate | InterfaceType::BuiltIn)
}

/// Whether the process `owner` of a device is another than `own`.
pub(crate) fn held_by_another(owner: i32, own: i32) -> bool {
    owner != NOBODY && owner != own
}

/// The id Core Audio's hog mode knows this process by.
pub(crate) fn own_process() -> i32 {
    std::process::id() as i32
}

/// What Core Audio says of one device, and does to it.
pub(crate) trait Hardware {
    /// The process that holds the device, or -1.
    fn hog_owner(&self) -> Result<i32, Status>;
    fn set_hog_owner(&self, owner: i32) -> Result<(), Status>;
    fn rate(&self) -> Result<u32, Status>;
    /// Returns once the device runs at `rate`.
    fn set_rate(&self, rate: u32) -> Result<(), Status>;
}

/// Whether another app holds the device now.
pub(crate) fn in_use_by_another(hardware: &impl Hardware) -> bool {
    hardware
        .hog_owner()
        .is_ok_and(|holder| held_by_another(holder, own_process()))
}

/// What stands where there is no Core Audio, which has no device to take.
#[cfg(not(target_os = "macos"))]
pub(crate) enum Nowhere {}

#[cfg(not(target_os = "macos"))]
impl Hardware for Nowhere {
    fn hog_owner(&self) -> Result<i32, Status> {
        match *self {}
    }
    fn set_hog_owner(&self, _: i32) -> Result<(), Status> {
        match *self {}
    }
    fn rate(&self) -> Result<u32, Status> {
        match *self {}
    }
    fn set_rate(&self, _: u32) -> Result<(), Status> {
        match *self {}
    }
}

/// Why the device could not be taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    InUse,
    Failed(Status),
    /// Core Audio's id for the device could not be found.
    Unreachable,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::InUse => f.write_str("in use by another app"),
            Refusal::Failed(status) => f.write_str(&status_text(*status)),
            Refusal::Unreachable => f.write_str("Core Audio does not know the device"),
        }
    }
}

/// An `OSStatus`, as the four characters Core Audio's errors are made of
/// where they are, and always as the number.
fn status_text(status: Status) -> String {
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|byte| (0x20..0x7f).contains(byte)) {
        format!("'{}' ({status})", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}

/// The device, held. Letting go of it, by `release` or by dropping, puts the
/// rate back as it was before and then gives the device up, so that no
/// other app finds it at a rate this one chose.
pub(crate) struct Hold<H: Hardware> {
    hardware: H,
    rate_before: u32,
    released: bool,
}

impl<H: Hardware> Hold<H> {
    /// Takes the device for `owner` and sets it to `rate`; if either fails,
    /// the device is as it was.
    fn take(hardware: H, owner: i32, rate: u32) -> Result<Self, Refusal> {
        let holder = hardware.hog_owner().map_err(Refusal::Failed)?;
        if held_by_another(holder, owner) {
            return Err(Refusal::InUse);
        }
        let rate_before = hardware.rate().map_err(Refusal::Failed)?;
        hardware.set_hog_owner(owner).map_err(Refusal::Failed)?;
        let mut hold = Hold {
            hardware,
            rate_before,
            released: false,
        };
        // Another app may have taken it between the look and the taking.
        match hold.hardware.hog_owner() {
            Ok(holder) if holder == owner => {}
            Ok(_) => {
                hold.released = true;
                return Err(Refusal::InUse);
            }
            Err(status) => return Err(Refusal::Failed(status)),
        }
        hold.set_rate(rate).map_err(Refusal::Failed)?;
        Ok(hold)
    }

    fn set_rate(&self, rate: u32) -> Result<(), Status> {
        if self.hardware.rate()? == rate {
            return Ok(());
        }
        self.hardware.set_rate(rate)
    }

    /// Lets go of the device, and says what went wrong where something did.
    pub fn release(mut self) -> Result<(), Status> {
        self.let_go()
    }

    fn let_go(&mut self) -> Result<(), Status> {
        if std::mem::replace(&mut self.released, true) {
            return Ok(());
        }
        let restored = self.set_rate(self.rate_before);
        let released = self.hardware.set_hog_owner(NOBODY);
        released.and(restored)
    }
}

impl<H: Hardware> Drop for Hold<H> {
    fn drop(&mut self) {
        if let Err(status) = self.let_go() {
            eprintln!(
                "Letting go of the sound device failed: {}",
                status_text(status)
            );
        }
    }
}

/// How the device stands for the stream about to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Exclusive mode was not asked for.
    Shared,
    /// The device is held and runs at the rate of the stream.
    Exclusive,
    /// It was asked for and could not be had: the stream plays shared.
    Declined(Refusal),
}

impl Standing {
    pub fn holds(&self) -> bool {
        *self == Standing::Exclusive
    }

    /// What the signal path says of a refusal.
    pub fn note(&self) -> Option<String> {
        match self {
            Standing::Declined(refusal) => {
                Some(format!("Exclusive mode wasn't available: {refusal}"))
            }
            Standing::Shared | Standing::Exclusive => None,
        }
    }
}

/// The output thread's claim on its device: kept from one stream to the next
/// on the device, so that a change of rate between tracks doesn't let another
/// app in, and given up when nothing plays, the device changes or the
/// thread ends.
pub(crate) struct Exclusive<H: Hardware> {
    owner: i32,
    hold: Option<Hold<H>>,
}

impl<H: Hardware> Exclusive<H> {
    pub fn new(owner: i32) -> Self {
        Exclusive { owner, hold: None }
    }

    /// Brings the device to what a stream in `rate` needs, `asked` being
    /// whether exclusive mode is on for the device. With no stream to open
    /// the device is let go. `hardware` finds the device in Core Audio when
    /// it is not yet held.
    pub fn engage(
        &mut self,
        asked: bool,
        rate: Option<u32>,
        hardware: impl FnOnce() -> Option<H>,
    ) -> Standing {
        let (true, Some(rate)) = (asked, rate) else {
            self.let_go();
            return Standing::Shared;
        };
        if let Some(hold) = &self.hold {
            return match hold.set_rate(rate) {
                Ok(()) => Standing::Exclusive,
                Err(status) => {
                    self.let_go();
                    Standing::Declined(Refusal::Failed(status))
                }
            };
        }
        let Some(hardware) = hardware() else {
            return Standing::Declined(Refusal::Unreachable);
        };
        match Hold::take(hardware, self.owner, rate) {
            Ok(hold) => {
                self.hold = Some(hold);
                Standing::Exclusive
            }
            Err(refusal) => Standing::Declined(refusal),
        }
    }

    pub fn let_go(&mut self) {
        let Some(hold) = self.hold.take() else { return };
        if let Err(status) = hold.release() {
            eprintln!(
                "Letting go of the sound device failed: {}",
                status_text(status)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    const ME: i32 = 4242;
    const OTHER: i32 = 77;

    /// Records what is done to a device that starts held by `owner` at
    /// `rate`, and fails the calls named in `fails`.
    #[derive(Default)]
    struct Fake {
        owner: i32,
        rate: u32,
        calls: Vec<String>,
        fails: Vec<&'static str>,
        /// The hog call that another app wins the race for.
        stolen: bool,
    }

    #[derive(Clone)]
    struct Device(Rc<RefCell<Fake>>);

    fn device(owner: i32, rate: u32) -> Device {
        Device(Rc::new(RefCell::new(Fake {
            owner,
            rate,
            ..Fake::default()
        })))
    }

    impl Device {
        fn fail(&self, call: &'static str) {
            self.0.borrow_mut().fails.push(call);
        }
        fn calls(&self) -> Vec<String> {
            self.0.borrow().calls.clone()
        }
        fn owner(&self) -> i32 {
            self.0.borrow().owner
        }
        fn rate_now(&self) -> u32 {
            self.0.borrow().rate
        }
    }

    const BROKEN: Status = 0x7768_6174;

    impl Hardware for Device {
        fn hog_owner(&self) -> Result<i32, Status> {
            let fake = self.0.borrow();
            if fake.fails.contains(&"hog_owner") {
                return Err(BROKEN);
            }
            Ok(if fake.stolen { OTHER } else { fake.owner })
        }
        fn set_hog_owner(&self, owner: i32) -> Result<(), Status> {
            let mut fake = self.0.borrow_mut();
            fake.calls.push(format!("hog {owner}"));
            if fake.fails.contains(&"set_hog_owner") {
                return Err(BROKEN);
            }
            fake.owner = owner;
            Ok(())
        }
        fn rate(&self) -> Result<u32, Status> {
            Ok(self.0.borrow().rate)
        }
        fn set_rate(&self, rate: u32) -> Result<(), Status> {
            let mut fake = self.0.borrow_mut();
            fake.calls.push(format!("rate {rate}"));
            if fake.fails.contains(&"set_rate") {
                return Err(BROKEN);
            }
            fake.rate = rate;
            Ok(())
        }
    }

    fn exclusive() -> Exclusive<Device> {
        Exclusive::new(ME)
    }

    fn engage(exclusive: &mut Exclusive<Device>, device: &Device, rate: u32) -> Standing {
        exclusive.engage(true, Some(rate), || Some(device.clone()))
    }

    #[test]
    fn the_device_is_taken_before_its_rate_is_set() {
        let device = device(NOBODY, 44_100);
        let mut exclusive = exclusive();
        assert_eq!(engage(&mut exclusive, &device, 96_000), Standing::Exclusive);
        assert_eq!(device.calls(), ["hog 4242", "rate 96000"]);
        assert_eq!(device.owner(), ME);
    }

    #[test]
    fn a_rate_already_right_is_not_set_again() {
        let device = device(NOBODY, 96_000);
        let mut exclusive = exclusive();
        engage(&mut exclusive, &device, 96_000);
        assert_eq!(device.calls(), ["hog 4242"]);
    }

    #[test]
    fn letting_go_puts_the_rate_back_and_then_gives_the_device_up() {
        let device = device(NOBODY, 44_100);
        let mut exclusive = exclusive();
        engage(&mut exclusive, &device, 96_000);
        exclusive.let_go();
        assert_eq!(
            device.calls(),
            ["hog 4242", "rate 96000", "rate 44100", "hog -1"]
        );
        assert_eq!((device.owner(), device.rate_now()), (NOBODY, 44_100));
    }

    #[test]
    fn a_new_rate_keeps_the_device_held() {
        let device = device(NOBODY, 44_100);
        let mut exclusive = exclusive();
        engage(&mut exclusive, &device, 96_000);
        assert_eq!(engage(&mut exclusive, &device, 48_000), Standing::Exclusive);
        assert_eq!(device.calls(), ["hog 4242", "rate 96000", "rate 48000"]);
    }

    #[test]
    fn nothing_to_play_lets_go() {
        let device = device(NOBODY, 44_100);
        let mut exclusive = exclusive();
        engage(&mut exclusive, &device, 96_000);
        let standing = exclusive.engage(true, None, || Some(device.clone()));
        assert_eq!(standing, Standing::Shared);
        assert_eq!(device.owner(), NOBODY);
    }

    #[test]
    fn turning_it_off_lets_go() {
        let device = device(NOBODY, 44_100);
        let mut exclusive = exclusive();
        engage(&mut exclusive, &device, 96_000);
        let standing = exclusive.engage(false, Some(96_000), || Some(device.clone()));
        assert_eq!(standing, Standing::Shared);
        assert_eq!(device.owner(), NOBODY);
    }

    #[test]
    fn dropping_it_lets_go() {
        let device = device(NOBODY, 44_100);
        {
            let mut exclusive = exclusive();
            engage(&mut exclusive, &device, 96_000);
        }
        assert_eq!((device.owner(), device.rate_now()), (NOBODY, 44_100));
    }

    #[test]
    fn a_device_another_app_holds_is_left_alone() {
        let device = device(OTHER, 44_100);
        let mut exclusive = exclusive();
        let standing = engage(&mut exclusive, &device, 96_000);
        assert_eq!(standing, Standing::Declined(Refusal::InUse));
        assert_eq!(
            standing.note().unwrap(),
            "Exclusive mode wasn't available: in use by another app"
        );
        assert!(device.calls().is_empty());
        drop(exclusive);
        assert_eq!(device.owner(), OTHER);
    }

    #[test]
    fn a_device_held_by_this_process_already_is_taken_again() {
        let device = device(ME, 44_100);
        let mut exclusive = exclusive();
        assert_eq!(engage(&mut exclusive, &device, 96_000), Standing::Exclusive);
    }

    #[test]
    fn losing_the_race_for_the_device_leaves_it_alone() {
        let device = device(NOBODY, 44_100);
        device.0.borrow_mut().stolen = true;
        let mut exclusive = exclusive();
        let standing = engage(&mut exclusive, &device, 96_000);
        assert_eq!(standing, Standing::Declined(Refusal::InUse));
        drop(exclusive);
        assert!(!device.calls().contains(&"hog -1".to_owned()));
    }

    #[test]
    fn a_rate_the_device_refuses_gives_the_device_back() {
        let device = device(NOBODY, 44_100);
        device.fail("set_rate");
        let mut exclusive = exclusive();
        let standing = engage(&mut exclusive, &device, 96_000);
        assert_eq!(standing, Standing::Declined(Refusal::Failed(BROKEN)));
        assert_eq!(device.owner(), NOBODY);
        assert!(!standing.holds());
    }

    #[test]
    fn a_hog_call_that_fails_changes_nothing() {
        let device = device(NOBODY, 44_100);
        device.fail("set_hog_owner");
        let mut exclusive = exclusive();
        let standing = engage(&mut exclusive, &device, 96_000);
        assert_eq!(standing, Standing::Declined(Refusal::Failed(BROKEN)));
        assert_eq!(device.rate_now(), 44_100);
        assert_eq!(
            standing.note().unwrap(),
            "Exclusive mode wasn't available: 'what' (2003329396)"
        );
    }

    #[test]
    fn a_rate_refused_while_held_gives_the_device_back() {
        let device = device(NOBODY, 44_100);
        let mut exclusive = exclusive();
        engage(&mut exclusive, &device, 96_000);
        device.fail("set_rate");
        let standing = engage(&mut exclusive, &device, 48_000);
        assert_eq!(standing, Standing::Declined(Refusal::Failed(BROKEN)));
        assert_eq!(device.owner(), NOBODY);
    }

    #[test]
    fn a_device_without_a_core_audio_id_is_declined() {
        let mut exclusive = exclusive();
        let standing = exclusive.engage(true, Some(96_000), || None::<Device>);
        assert_eq!(standing, Standing::Declined(Refusal::Unreachable));
    }

    #[test]
    fn only_a_held_device_is_exclusive() {
        assert!(Standing::Exclusive.holds());
        assert!(!Standing::Shared.holds());
        assert!(!Standing::Declined(Refusal::InUse).holds());
        assert_eq!(Standing::Shared.note(), None);
    }

    #[test]
    fn an_aggregate_device_and_the_built_in_speakers_are_not_offered_it() {
        assert!(suits(InterfaceType::Usb));
        assert!(suits(InterfaceType::Unknown));
        assert!(!suits(InterfaceType::Aggregate));
        assert!(!suits(InterfaceType::BuiltIn));
    }

    #[test]
    fn only_another_process_is_another_app() {
        assert!(held_by_another(OTHER, ME));
        assert!(!held_by_another(ME, ME));
        assert!(!held_by_another(NOBODY, ME));
    }
}
