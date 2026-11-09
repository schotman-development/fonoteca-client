//! Shows each shared component on its own, with sample data.

include!(concat!(env!("OUT_DIR"), "/workbench.rs"));

fn main() -> Result<(), slint::PlatformError> {
    let workbench = Workbench::new()?;
    workbench
        .global::<Waveform>()
        .on_path(fonoteca_client::waveform_path);
    workbench.run()
}
