use std::path::PathBuf;

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let native = root.join("src/BetterTrafficMonitorAiUsage");
    for path in [
        native.join("UsageCoreAdapter.cpp"),
        native.join("UsageCoreAdapter.h"),
        root.join("include/usage_core.h"),
        manifest.join("native_bridge.cpp"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .include(root.join("include"))
        .include(&native)
        .file(native.join("UsageCoreAdapter.cpp"))
        .file(manifest.join("native_bridge.cpp"))
        .warnings(true)
        .extra_warnings(true)
        .warnings_into_errors(true)
        .flag_if_supported("/EHsc")
        .compile("usage_native_contract");
}
