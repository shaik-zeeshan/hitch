use std::path::PathBuf;

fn main() {
    // Copy the pre-built hitch-daemon into src-tauri/binaries/ with the
    // target-triple suffix that Tauri's externalBin expects. Tauri validates the
    // sidecar for every build (dev included), so we run this for both profiles
    // and source the daemon from the matching target dir. The daemon must be
    // built first: `cargo build -p hitch-daemon [--release]` (the dev flow's
    // beforeDevCommand does this).
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

    let daemon_src = workspace_root.join(format!("target/{profile}/hitch-daemon"));
    if daemon_src.exists() {
        let binaries_dir = manifest_dir.join("binaries");
        std::fs::create_dir_all(&binaries_dir).unwrap();
        let daemon_dst = binaries_dir.join(format!("hitch-daemon-{target}"));
        std::fs::copy(&daemon_src, &daemon_dst).expect("failed to copy hitch-daemon");
    } else {
        let build_flag = if profile == "release" {
            " --release"
        } else {
            ""
        };
        println!(
            "cargo:warning=hitch-daemon not found at {}; run `cargo build -p hitch-daemon{build_flag}` first",
            daemon_src.display()
        );
    }

    tauri_build::build()
}
