fn main() {
    // The layout tests locate elements by their `.slint` id, which only works
    // when the compiler emits debug info. Kept off for normal builds.
    let debug_info = std::env::var_os("CARGO_FEATURE_TESTING").is_some();
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug_info);
    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile Slint UI");
}
