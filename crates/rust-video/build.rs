use std::path::PathBuf;

fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    for (feature, os, cfg) in [
        ("VAAPI", "linux", "vaapi_available"),
        ("MEDIACODEC", "android", "mediacodec_available"),
        ("D3D11", "windows", "d3d11_available"),
        ("D3D12", "windows", "d3d12_available"),
    ] {
        println!("cargo::rustc-check-cfg=cfg({cfg})");
        if target == os && std::env::var(format!("CARGO_FEATURE_{feature}")).is_ok() {
            println!("cargo::rustc-cfg={cfg}");
        }
    }
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
