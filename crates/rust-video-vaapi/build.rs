//! Build flags: the corpus gate.

use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(have_corpus)");
    println!("cargo::rerun-if-env-changed=RUST_VIDEO_CORPUS");
    let env_root = std::env::var_os("RUST_VIDEO_CORPUS");
    let root = env_root.as_ref().map_or_else(
        || {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
                .join("../../corpus")
        },
        PathBuf::from,
    );
    if root.is_dir() {
        println!("cargo::rustc-cfg=have_corpus");
    } else if env_root.is_some() {
        println!("cargo::warning=corpus absent, e2e tests compiled out; run `just vectors`");
    }
}
