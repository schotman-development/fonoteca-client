fn main() {
    // The flow test finds elements through i-slint-backend-testing, which needs
    // this; release builds leave it out.
    let debug = std::env::var("PROFILE").is_ok_and(|profile| profile == "debug");
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug);
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");
}
