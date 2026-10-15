use std::path::PathBuf;

fn main() {
    // The flow test finds elements through i-slint-backend-testing, which needs
    // this; release builds leave it out.
    let debug = std::env::var("PROFILE").is_ok_and(|profile| profile == "debug");
    let config = || slint_build::CompilerConfiguration::new().with_debug_info(debug);
    slint_build::compile_with_config("ui/app.slint", config()).expect("Slint build failed");

    // The workbench binary includes its own output, so the app doesn't carry it.
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("workbench.rs");
    let inputs =
        slint_build::compile_with_output_path(manifest.join("ui/workbench.slint"), out, config())
            .expect("Slint build failed");
    for input in inputs {
        println!("cargo:rerun-if-changed={}", input.display());
    }
}
