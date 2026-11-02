//! What plays, told to Android: a notification and the lock screen show it
//! with buttons, and the foreground service behind them keeps the app
//! playing with the screen off. Those buttons, a headset's and a watch's
//! come back as the bar's.

use std::sync::OnceLock;

use jni::objects::{Global, JClass, JObject, JValue};
use jni::sys::{jdouble, jint};
use jni::{Env, JavaVM, NativeMethod, jni_sig, jni_str, native_method};
use slint::ComponentHandle;

use crate::AppWindow;

/// The activity, whose Java side runs the service.
pub(crate) static ACTIVITY: OnceLock<Global<JObject<'static>>> = OnceLock::new();
/// The window the buttons act on.
static WINDOW: OnceLock<slint::Weak<AppWindow>> = OnceLock::new();

/// The buttons, numbered as PlaybackService.java numbers them.
const PLAY: jint = 0;
const PAUSE: jint = 1;
const NEXT: jint = 2;
const PREVIOUS: jint = 3;
const SEEK: jint = 4;

// jni 0.22's macro calls an atomic method that newer Rust renamed.
#[allow(deprecated)]
const COMMAND: NativeMethod = native_method! {
    static fn command(button: jint, seconds: jdouble),
};

/// Keeps the activity for later calls and hands MainActivity.command its
/// Rust side. Runs once, as the app starts.
pub fn init(env: &mut Env, activity: &JObject) -> jni::errors::Result<()> {
    let class = env.get_object_class(activity)?;
    // Safety: COMMAND's macro checked its function against its signature.
    unsafe { env.register_native_methods(&class, &[COMMAND])? };
    let _ = ACTIVITY.set(env.new_global_ref(activity)?);
    Ok(())
}

pub fn wire(ui: &AppWindow) {
    let _ = WINDOW.set(ui.as_weak());
}

/// Shows `title` by `artists` as playing or paused, `seconds` into
/// `duration`.
pub fn show(title: &str, artists: &str, playing: bool, duration: f64, seconds: f64) {
    call(|env, activity| {
        let (title, artists) = (env.new_string(title)?, env.new_string(artists)?);
        env.call_method(
            activity,
            jni_str!("showPlaying"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;ZJJ)V"),
            &[
                JValue::Object(&title),
                JValue::Object(&artists),
                JValue::Bool(playing),
                JValue::Long((duration * 1000.0) as i64),
                JValue::Long((seconds * 1000.0) as i64),
            ],
        )?;
        Ok(())
    });
}

/// Takes the notification down and lets the app be stopped in the
/// background again.
pub fn hide() {
    call(|env, activity| {
        env.call_method(activity, jni_str!("hidePlaying"), jni_sig!("()V"), &[])?;
        Ok(())
    });
}

fn call(f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<()>) {
    let (Some(activity), Ok(vm)) = (ACTIVITY.get(), JavaVM::singleton()) else {
        return;
    };
    // The notification is a convenience: playing goes on without it.
    if let Err(problem) = vm.attach_current_thread(|env| f(env, activity)) {
        eprintln!("Android's media controls: {problem}");
    }
}

/// A button pressed, on the Java side's thread.
fn command<'local>(
    _env: &mut Env<'local>,
    _class: JClass<'local>,
    button: jint,
    seconds: jdouble,
) -> jni::errors::Result<()> {
    let Some(window) = WINDOW.get() else {
        return Ok(());
    };
    let _ = window.upgrade_in_event_loop(move |ui| match button {
        // A headset's play while playing changes nothing.
        PLAY | PAUSE if ui.get_playing_paused() == (button == PLAY) => ui.invoke_toggle_pause(),
        NEXT => ui.invoke_next_track(),
        PREVIOUS => ui.invoke_previous_track(),
        SEEK => ui.invoke_seek(seconds as f32),
        _ => {}
    });
    Ok(())
}
