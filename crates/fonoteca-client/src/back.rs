//! Android's back button and gesture. Back goes as a key to what has the
//! keys, such as a menu, a dialog or the page; what none of them takes steps
//! back through the app, which back never closes.

use slint::platform::{Key, WindowEvent, WindowEventDispatchResult};
use slint::{ComponentHandle, SharedString};

use crate::AppWindow;

/// Back pressed: offered as a key to what has the keys, unless Android
/// offered it already, and a step back through the app when none of them
/// takes it.
pub fn pressed(ui: &AppWindow, offered: bool) {
    if offered || !taken(ui) {
        ui.invoke_step_back();
    }
}

/// Whether what has the keys takes back, pressed and let go.
fn taken(ui: &AppWindow) -> bool {
    let key = SharedString::from(Key::Back);
    let window = ui.window();
    let pressed = window.dispatch_event_with_result(WindowEvent::KeyPressed { text: key.clone() });
    let released = window.dispatch_event_with_result(WindowEvent::KeyReleased { text: key });
    [pressed, released]
        .iter()
        .any(|result| matches!(result, Ok(WindowEventDispatchResult::Accepted)))
}

#[cfg(target_os = "android")]
mod android {
    use std::sync::Mutex;

    use jni::objects::{JClass, JObject};
    use jni::sys::jboolean;
    use jni::{Env, JavaVM, NativeMethod, jni_sig, jni_str, native_method};
    use slint::ComponentHandle;

    use crate::AppWindow;
    use crate::media::ACTIVITY;

    /// The window back acts on: that of the app's latest start.
    static WINDOW: Mutex<Option<slint::Weak<AppWindow>>> = Mutex::new(None);

    // jni 0.22's macro calls an atomic method that newer Rust renamed.
    #[allow(deprecated)]
    const BACK: NativeMethod = native_method! {
        static fn back(offered: jboolean),
    };

    /// Hands MainActivity.back its Rust side. Runs as the app starts.
    pub fn init(env: &mut Env, activity: &JObject) -> jni::errors::Result<()> {
        let class = env.get_object_class(activity)?;
        // Safety: BACK's macro checked its function against its signature.
        unsafe { env.register_native_methods(&class, &[BACK])? };
        Ok(())
    }

    /// Has back come to `ui`. Runs once Slint has started, which asks
    /// Android for back too: of the two, Android asks the later first.
    pub fn wire(ui: &AppWindow) {
        *WINDOW.lock().unwrap() = Some(ui.as_weak());
        let activity = ACTIVITY.read().unwrap();
        let (Some(activity), Ok(vm)) = (activity.as_ref(), JavaVM::singleton()) else {
            return;
        };
        let asked = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            env.call_method(activity, jni_str!("takeBack"), jni_sig!("()V"), &[])?;
            Ok(())
        });
        // Slint then has back to itself, and closes the app when nothing
        // on screen takes it.
        if let Err(problem) = asked {
            eprintln!("Android's back: {problem}");
        }
    }

    /// Back pressed, on the Java side's thread, `offered` already as a key.
    fn back<'local>(
        _env: &mut Env<'local>,
        _class: JClass<'local>,
        offered: jboolean,
    ) -> jni::errors::Result<()> {
        let Some(window) = WINDOW.lock().unwrap().clone() else {
            return Ok(());
        };
        let _ = window.upgrade_in_event_loop(move |ui| super::pressed(&ui, offered));
        Ok(())
    }
}

#[cfg(target_os = "android")]
pub use android::*;
