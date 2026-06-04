use std::path::PathBuf;

fn main() {
    // Copy pre-built sidecars into src-tauri/binaries/ with the target-triple
    // suffix that Tauri's externalBin expects. Tauri validates sidecars for every
    // build (dev included), so we run this for both profiles. Both binaries must
    // be built first: `cargo build -p hitch-daemon -p hitch-hook [--release]`.
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let target = std::env::var("TARGET").expect("TARGET not set");
    let host = std::env::var("HOST").expect("HOST not set");
    let cross_compiling = target != host;
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

    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                target_dir_from_out_dir(&target, &profile).unwrap_or_else(|| workspace_root.join(path))
            }
        })
        .unwrap_or_else(|| workspace_root.join("target"));
    let exe_suffix = std::env::var("CARGO_CFG_TARGET_OS")
        .map(|os| if os == "windows" { ".exe" } else { "" })
        .unwrap_or("");
    let binaries_dir = manifest_dir.join("binaries");
    for binary in ["hitch-daemon", "hitch-hook"] {
        let file_name = format!("{binary}{exe_suffix}");
        let host_artifact = target_dir.join(&profile).join(&file_name);
        let target_artifact = target_dir.join(&target).join(&profile).join(&file_name);
        // When cross-compiling, only the target-triple artifact has the right
        // architecture. `target/<profile>/` holds a host-arch binary, so it must
        // never be a candidate — mtime selection there could copy a fresher host
        // build into a target installer.
        //
        // When host == target, Cargo writes host builds to `target/<profile>/`,
        // but a one-off `--target <triple>` build also leaves an artifact in
        // `target/<triple>/<profile>/`. A fixed preference order would resurrect
        // that stale binary and shadow every fresh host build from then on — and
        // Tauri copies the chosen sidecar back over `target/<profile>/`, so the
        // stale binary ends up bundled. Picking the most recently built artifact
        // avoids that.
        let candidates: Vec<PathBuf> = if cross_compiling {
            vec![target_artifact]
        } else {
            vec![target_artifact, host_artifact]
        };
        if let Some(src) = candidates
            .iter()
            .filter(|path| path.exists())
            .max_by_key(|path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok())
        {
            std::fs::create_dir_all(&binaries_dir).unwrap();
            let dst = binaries_dir.join(format!("{binary}-{target}{exe_suffix}"));
            std::fs::copy(src, &dst).unwrap_or_else(|err| {
                panic!(
                    "failed to copy {binary} from {} to {}: {err}",
                    src.display(),
                    dst.display()
                )
            });
        } else {
            let build_flag = if profile == "release" {
                " --release"
            } else {
                ""
            };
            let searched = candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(" or ");
            let target_flag = if cross_compiling {
                format!(" --target {target}")
            } else {
                String::new()
            };
            println!(
                "cargo:warning={binary} not found at {searched}; run `cargo build -p hitch-daemon -p hitch-hook{build_flag}{target_flag}` first"
            );
        }
    }

    // Embed a long-path-aware application manifest on Windows. tauri-build only
    // applies `app_manifest` to the Windows resource; on other targets the
    // attribute is ignored. The manifest starts from Tauri's default
    // (Common-Controls v6) and adds `<longPathAware>true</longPathAware>` so deep
    // managed-worktree paths under %LOCALAPPDATA%\Hitch aren't capped at MAX_PATH
    // (ADR 0012). Falls back to the default build if the manifest can't be read.
    let windows = tauri_build::WindowsAttributes::new()
        .app_manifest(include_str!("windows-app-manifest.xml"));
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}

fn target_dir_from_out_dir(target: &str, profile: &str) -> Option<PathBuf> {
    let mut path = PathBuf::from(std::env::var_os("OUT_DIR")?);
    path.pop();
    path.pop();
    path.pop();
    if path.file_name()? != profile {
        return None;
    }

    path.pop();
    if path.file_name()? == std::ffi::OsStr::new(target) {
        path.pop();
    }
    Some(path)
}
