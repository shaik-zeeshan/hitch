use std::path::PathBuf;

fn main() {
    // Copy pre-built sidecars into src-tauri/binaries/ with the target-triple
    // suffix that Tauri's externalBin expects. Tauri validates sidecars for every
    // build (dev included), so we run this for both profiles. Both binaries must
    // be built first: `cargo build -p hitch-daemon -p hitch-hook [--release]`.
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let target = std::env::var("TARGET").expect("TARGET not set");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    let workspace_root = manifest_dir
        .ancestors()
        .skip(1)
        .find(|p| {
            p.join("Cargo.toml").exists()
                && std::fs::read_to_string(p.join("Cargo.toml"))
                    .unwrap_or_default()
                    .contains("[workspace]")
        })
        .expect("workspace root not found");

    let binaries_dir = manifest_dir.join("binaries");
    for binary in ["hitch-daemon", "hitch-hook"] {
        let src = workspace_root.join(format!("target/{profile}/{binary}"));
        if src.exists() {
            std::fs::create_dir_all(&binaries_dir).unwrap();
            let dst = binaries_dir.join(format!("{binary}-{target}"));
            std::fs::copy(&src, &dst).unwrap_or_else(|err| {
                panic!("failed to copy {binary} from {} to {}: {err}", src.display(), dst.display())
            });
        } else {
            let build_flag = if profile == "release" {
                " --release"
            } else {
                ""
            };
            println!(
                "cargo:warning={binary} not found at {}; run `cargo build -p hitch-daemon -p hitch-hook{build_flag}` first",
                src.display()
            );
        }
    }

    tauri_build::build()
}
