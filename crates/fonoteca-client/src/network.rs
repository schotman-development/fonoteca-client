//! What the app asks of Android's network, through MainActivity: which
//! network its traffic goes over, and letting multicast packets through.

use jni::objects::JObject;
use jni::{Env, JavaVM, jni_sig, jni_str};

use crate::media::ACTIVITY;

/// Whether the phone is on its mobile network now. Where Android can't say,
/// the phone counts as on wifi, which never costs the listener data.
pub fn on_mobile_data() -> bool {
    let activity = ACTIVITY.read().unwrap();
    let (Some(activity), Ok(vm)) = (activity.as_ref(), JavaVM::singleton()) else {
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

/// Wifi multicast packets get through while this is held. Android filters
/// them out to save power, and with them the announcements that devices on
/// the network make of themselves.
pub struct MulticastHold;

impl MulticastHold {
    pub fn acquire() -> Self {
        call_activity(|env, activity| {
            env.call_method(activity, jni_str!("holdMulticast"), jni_sig!("()V"), &[])?;
            Ok(())
        });
        MulticastHold
    }
}

impl Drop for MulticastHold {
    fn drop(&mut self) {
        call_activity(|env, activity| {
            env.call_method(activity, jni_str!("releaseMulticast"), jni_sig!("()V"), &[])?;
            Ok(())
        });
    }
}

fn call_activity(f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<()>) {
    let activity = ACTIVITY.read().unwrap();
    let (Some(activity), Ok(vm)) = (activity.as_ref(), JavaVM::singleton()) else {
        return;
    };
    if let Err(problem) = vm.attach_current_thread(|env| f(env, activity)) {
        eprintln!("Android's multicast lock: {problem}");
    }
}
