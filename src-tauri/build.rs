fn main() {
    // With the optional `gpu` feature, link the Vulkan loader as a DELAY-load
    // import so the app still LAUNCHES on machines that lack vulkan-1.dll (no/old
    // GPU driver, some VMs) instead of failing at load time. The runtime guard in
    // `whisper_local::inference` forces CPU when the loader is absent, so the
    // delayed import is never actually triggered — the app falls back cleanly.
    let windows_target = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let gpu_feature = std::env::var_os("CARGO_FEATURE_GPU").is_some();
    if windows_target && gpu_feature {
        println!("cargo:rustc-link-arg=/DELAYLOAD:vulkan-1.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }

    tauri_build::build()
}
