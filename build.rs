use std::{env, fs, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=shared");
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rustc-env=SOFTWARE_BUILD_ID={}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    println!("cargo:rerun-if-changed=catalog");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut files: Vec<_> = fs::read_dir(root.join("catalog/snapshots"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    let mut generated = String::from("const BUNDLED: &[&str] = &[\n");
    for file in files {
        generated.push_str(&format!("include_str!({:?}),\n", file));
    }
    generated.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("model_snapshots.rs"),
        generated,
    )
    .unwrap();
}
