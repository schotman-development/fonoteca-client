//! Keeps downloads going with the app in the background, told to Android: a
//! user-initiated data transfer job (Android 14 and later), shown in its Task
//! Manager with a notification. The downloads themselves are src/downloads.rs's;
//! elsewhere, and before Android 14, the app downloads while it runs and picks
//! up again at its next start.

#[cfg(target_os = "android")]
mod android {
    use std::sync::OnceLock;

    use jni::objects::{JClass, JObject, JValue};
    use jni::{Env, JavaVM, NativeMethod, jni_sig, jni_str, native_method};
    use slint::ComponentHandle;

    use crate::AppWindow;
    use crate::media::ACTIVITY;

    /// The window the job's stop acts on.
    static WINDOW: OnceLock<slint::Weak<AppWindow>> = OnceLock::new();

    // jni 0.22's macro calls an atomic method that newer Rust renamed.
    #[allow(deprecated)]
    const STOPPED: NativeMethod = native_method! {
        static fn downloads_stopped(),
    };

    /// Hands MainActivity.downloadsStopped its Rust side. Runs once, as the
    /// app starts.
    pub fn init(env: &mut Env, activity: &JObject) -> jni::errors::Result<()> {
        let class = env.get_object_class(activity)?;
        // Safety: STOPPED's macro checked its function against its signature.
        unsafe { env.register_native_methods(&class, &[STOPPED])? };
        Ok(())
    }

    pub fn wire(ui: &AppWindow) {
        let _ = WINDOW.set(ui.as_weak());
    }

    /// Starts the job for about `bytes` of downloading, over an unmetered
    /// network only if `unmetered`.
    pub fn start(bytes: u64, unmetered: bool) {
        call(|env, activity| {
            env.call_method(
                activity,
                jni_str!("startDownloads"),
                jni_sig!("(JZ)V"),
                &[
                    JValue::Long(i64::try_from(bytes).unwrap_or(i64::MAX)),
                    JValue::Bool(unmetered),
                ],
            )?;
            Ok(())
        });
    }

    /// Says `text` under the notification's title, with `done` of `total`
    /// songs on its bar.
    pub fn progress(text: &str, done: usize, total: usize) {
        call(|env, activity| {
            let text = env.new_string(text)?;
            env.call_method(
                activity,
                jni_str!("progressDownloads"),
                jni_sig!("(Ljava/lang/String;II)V"),
                &[
                    JValue::Object(&text),
                    JValue::Int(i32::try_from(done).unwrap_or(i32::MAX)),
                    JValue::Int(i32::try_from(total).unwrap_or(i32::MAX)),
                ],
            )?;
            Ok(())
        });
    }

    /// Ends the job and takes its notification down.
    pub fn finish() {
        call(|env, activity| {
            env.call_method(activity, jni_str!("finishDownloads"), jni_sig!("()V"), &[])?;
            Ok(())
        });
    }

    fn call(f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<()>) {
        let (Some(activity), Ok(vm)) = (ACTIVITY.get(), JavaVM::singleton()) else {
            return;
        };
        // The job is a convenience: downloading goes on without it.
        if let Err(problem) = vm.attach_current_thread(|env| f(env, activity)) {
            eprintln!("Android's download job: {problem}");
        }
    }

    /// The user stopped the job, on the Java side's thread.
    fn downloads_stopped<'local>(
        _env: &mut Env<'local>,
        _class: JClass<'local>,
    ) -> jni::errors::Result<()> {
        if let Some(window) = WINDOW.get() {
            let _ = window.upgrade_in_event_loop(|ui| ui.invoke_stop_downloads());
        }
        Ok(())
    }
}

#[cfg(target_os = "android")]
pub use android::*;

/// What the tests were told of the job, since elsewhere there is none.
#[cfg(all(test, not(target_os = "android")))]
pub mod told {
    use std::sync::atomic::AtomicUsize;

    pub static STARTS: AtomicUsize = AtomicUsize::new(0);
    pub static FINISHES: AtomicUsize = AtomicUsize::new(0);
}

#[cfg(not(target_os = "android"))]
pub fn start(_bytes: u64, _unmetered: bool) {
    #[cfg(test)]
    told::STARTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(not(target_os = "android"))]
pub fn progress(_text: &str, _done: usize, _total: usize) {}

#[cfg(not(target_os = "android"))]
pub fn finish() {
    #[cfg(test)]
    told::FINISHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
