//! Which network Android sends the app's traffic over, asked through
//! MainActivity.

use jni::{JavaVM, jni_sig, jni_str};

use crate::media::ACTIVITY;

/// Whether the phone is on its mobile network now. Where Android can't say,
/// the phone counts as on wifi, which never costs the listener data.
pub fn on_mobile_data() -> bool {
    let (Some(activity), Ok(vm)) = (ACTIVITY.get(), JavaVM::singleton()) else {
        return false;
    };
    let asked = vm.attach_current_thread(|env| {
        env.call_method(activity, jni_str!("isOnMobileData"), jni_sig!("()Z"), &[])?
            .z()
    });
    asked.unwrap_or_else(|problem| {
        eprintln!("Android's network: {problem}");
        false
    })
}
