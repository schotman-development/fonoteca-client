//! Shows each shared component on its own, with sample data.

include!(concat!(env!("OUT_DIR"), "/workbench.rs"));

fn main() -> Result<(), slint::PlatformError> {
    Workbench::new()?.run()
}
