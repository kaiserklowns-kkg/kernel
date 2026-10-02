fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").expect("cargo sets CARGO_CFG_TARGET_ARCH");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "none" {
        // Host builds (e.g. `cargo check` on the workspace) do not link the kernel.
        return;
    }
    let script = format!("{}/linker-{arch}.ld", env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rustc-link-arg=-T{script}");
    println!("cargo:rerun-if-changed={script}");
}
