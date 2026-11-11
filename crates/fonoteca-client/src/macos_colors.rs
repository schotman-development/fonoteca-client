//! Tags the window sRGB. Left untagged, macOS reads its pixels in the display's
//! own profile, which on a wide-gamut screen (Display P3) makes every colour in
//! the app, covers included, look more saturated than on other systems.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSColorSpace};

/// Tags every window the app has open; call once they exist, from the main thread.
pub fn tag_windows_srgb() {
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let srgb = NSColorSpace::sRGBColorSpace();
    for window in NSApplication::sharedApplication(main_thread).windows() {
        window.setColorSpace(Some(&srgb));
    }
}
