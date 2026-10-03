fn main() {
    let script = format!("{}/../linker.ld", env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rustc-link-arg=-T{script}");
    println!("cargo:rerun-if-changed={script}");
}
