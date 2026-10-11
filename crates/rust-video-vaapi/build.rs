use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(have_corpus)");
    println!("cargo::rerun-if-env-changed=RUST_VIDEO_CORPUS");
    let root = std::env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
                .join("../../corpus")
        },
        PathBuf::from,
    );
    if root.is_dir() {
        println!("cargo::rustc-cfg=have_corpus");
    } else {
        println!("cargo::warning=corpus absent, e2e tests compiled out; run `just vectors`");
    }
}
