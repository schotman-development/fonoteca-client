//! Android 14's bit-perfect USB output, asked for through MainActivity (see
//! BitPerfectOutput.java): the player hands it the format of each track and
//! the system's mixer then leaves the audio alone instead of resampling it.

use std::sync::Arc;

use jni::objects::{JIntArray, JObject, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};
use playback::{Format, SampleFormat, SystemMixer};

use crate::media::ACTIVITY;

/// The bit depth BitPerfectOutput.java numbers float samples with.
const FLOAT_BITS: i32 = 32;
const INT_BITS: i32 = 16;

pub fn system() -> Arc<dyn SystemMixer> {
    Arc::new(Android)
}

struct Android;

impl SystemMixer for Android {
    fn bit_perfect_formats(&self) -> Vec<Format> {
        call("bitPerfectFormats", |env, activity| {
            let triples = env
                .call_method(
                    activity,
                    jni_str!("bitPerfectFormats"),
                    jni_sig!("()[I"),
                    &[],
                )?
                .l()?;
            let triples = env.cast_local::<JIntArray>(triples)?;
            let mut flat = vec![0; triples.len(env)?];
            triples.get_region(env, 0, &mut flat)?;
            Ok(flat
                .as_chunks::<3>()
                .0
                .iter()
                .filter_map(|&[rate, channels, bits]| format_of(rate, channels, bits))
                .collect())
        })
        .unwrap_or_default()
    }

    fn engage_bit_perfect(&self, format: Format) -> bool {
        let Some(bits) = bits_of(format.sample) else {
            return false;
        };
        call("engageBitPerfect", |env, activity| {
            env.call_method(
                activity,
                jni_str!("engageBitPerfect"),
                jni_sig!("(III)Z"),
                &[
                    JValue::Int(format.rate as i32),
                    JValue::Int(i32::from(format.channels)),
                    JValue::Int(bits),
                ],
            )?
            .z()
        })
        .unwrap_or(false)
    }

    fn release_bit_perfect(&self) {
        call("releaseBitPerfect", |env, activity| {
            env.call_method(
                activity,
                jni_str!("releaseBitPerfect"),
                jni_sig!("()V"),
                &[],
            )?;
            Ok(())
        });
    }
}

fn format_of(rate: i32, channels: i32, bits: i32) -> Option<Format> {
    Some(Format {
        rate: u32::try_from(rate).ok()?,
        channels: u16::try_from(channels).ok()?,
        sample: match bits {
            INT_BITS => SampleFormat::I16,
            FLOAT_BITS => SampleFormat::F32,
            _ => return None,
        },
    })
}

fn bits_of(sample: SampleFormat) -> Option<i32> {
    match sample {
        SampleFormat::I16 => Some(INT_BITS),
        SampleFormat::F32 => Some(FLOAT_BITS),
        _ => None,
    }
}

/// Runs `f` on the activity from the calling thread. A failed call is
/// reported and counts as the system saying nothing: playing goes on through
/// its mixer.
fn call<T>(what: &str, f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<T>) -> Option<T> {
    let (Some(activity), Ok(vm)) = (ACTIVITY.get(), JavaVM::singleton()) else {
        return None;
    };
    match vm.attach_current_thread(|env| f(env, activity)) {
        Ok(value) => Some(value),
        Err(problem) => {
            eprintln!("Android's bit-perfect output, {what}: {problem}");
            None
        }
    }
}
