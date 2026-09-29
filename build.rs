fn main() {
    // The layout tests locate elements by their `.slint` id, which only works
    // when the compiler emits debug info. Kept off for normal builds.
    let debug_info = std::env::var_os("CARGO_FEATURE_TESTING").is_some();
    let mut config = slint_build::CompilerConfiguration::new().with_debug_info(debug_info);
    // Slint's default Fluent style looks like Windows, so macOS gets its own
    // look, the dark variant as the app is dark whatever the system's scheme.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        config = config.with_style("cupertino-dark".into());
    }
    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile Slint UI");
}
