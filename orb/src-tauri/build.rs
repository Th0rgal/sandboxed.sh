fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../../shared");
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rustc-env=SOFTWARE_BUILD_ID={}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    tauri_build::build()
}
