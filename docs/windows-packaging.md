# Windows packaging

Hitch publishes Windows desktop artifacts for `x86_64-pc-windows-msvc` from `windows-latest` CI runners. The release artifact is a Tauri Windows installer for 64-bit Windows.

## Supported runtime

- OS: Windows 10 or Windows 11, x64.
- WebView: Microsoft Edge WebView2 Runtime. The installer uses Tauri's `downloadBootstrapper` WebView2 install mode, so it can download and install/update the Evergreen Runtime when needed. Machines without WebView2 need network access during installation, or WebView2 must be preinstalled by the user/administrator.
- Git: Git must be available on `PATH` for git-backed projects and worktrees.

The packaged app includes the Hitch daemon and hook as Tauri sidecars using target-triple suffixed binaries for the Windows target.

## Long paths (MAX_PATH)

Managed worktrees live deep under `%LOCALAPPDATA%\Hitch\worktrees\…` (ADR 0012), where the legacy 260-character `MAX_PATH` limit can truncate a checkout on a long root. Hitch mitigates this in two complementary ways:

- **Long-path-aware app manifest.** `apps/desktop/src-tauri/windows-app-manifest.xml` sets `<longPathAware>true</longPathAware>` (the 2016 `windowsSettings` namespace) and is embedded into the GUI executable through `tauri-build` (`WindowsAttributes::app_manifest` in `build.rs`). It starts from Tauri's default manifest, so the required `Microsoft.Windows.Common-Controls` v6 dependency is preserved. **This setting only takes effect when the OS `LongPathsEnabled` policy is also enabled** (`HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled = 1`, Windows 10 1607+); without it, manifest-only long-path support does not apply.
- **`\\?\` prefixing in `hitch-git`.** For its own managed-worktree lifecycle filesystem access (directory creation, existence/`is_dir` probes), `hitch-git` converts the absolute path to its extended-length (`\\?\C:\…`, or `\\?\UNC\…` for UNC) form before handing it to `std::fs`. This opts the individual syscall out of `MAX_PATH` **regardless of the manifest or the `LongPathsEnabled` policy**, and so covers the daemon and hook sidecars too — they pick up long-path support from this prefixing even though the manifest is embedded only in the GUI exe. Git CLI arguments, display strings, and paths stored or sent to the GUI keep the normal (non-prefixed) form.

If a worktree path still cannot be represented in extended-length form (e.g. a non-absolute path), Hitch surfaces a clear error rather than operating on a truncated path.
