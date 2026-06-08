# Windows packaging

Hitch publishes Windows desktop artifacts for `x86_64-pc-windows-msvc` from `windows-latest` CI runners. The release artifact is a Tauri Windows installer for 64-bit Windows.

## Supported runtime

- OS: Windows 10 or Windows 11, x64.
- WebView: Microsoft Edge WebView2 Runtime. The installer uses Tauri's `downloadBootstrapper` WebView2 install mode, so it can download and install/update the Evergreen Runtime when needed. Machines without WebView2 need network access during installation, or WebView2 must be preinstalled by the user/administrator.
- Git: Git must be available on `PATH` for git-backed projects and worktrees.

The packaged app includes the Hitch daemon and hook as Tauri sidecars using target-triple suffixed binaries for the Windows target.

## CLI on `PATH` (installer reach-in)

On Unix/macOS the `hitch` CLI self-installs at runtime by symlinking from the daemon's reported `exe_path` (ADR 0014, Approach C); `apps/desktop/src-tauri/src/cli_install.rs` owns that path. On Windows the same end state — a bare `hitch` resolvable on the shell `PATH` — is instead set up **by the installer**, because editing a user's `PATH` and overwriting locked sidecars is the installer's job, not the running app's. The runtime `cli_install.rs` is therefore **status-only on Windows** (it reports whether `hitch.exe` is present and on `PATH`; it never edits the registry).

The installer reach-in lives in two fragments wired from `tauri.conf.json`:

- **`installer/cli-reach-in.nsh`** — the NSIS hooks. NSIS is the **primary** Windows installer, so this carries the full behavior.
- **`installer/cli-reach-in.wxs`** — a WiX fragment that expresses the equivalent for the MSI build (`<Environment>` PATH append + a second-install of the daemon payload as `hitch.exe`, both auto-reversed by Windows Installer).

What the NSIS hooks do:

- **CLI entrypoint.** `NSIS_HOOK_POSTINSTALL` copies `$INSTDIR\hitch-daemon.exe` → `$INSTDIR\hitch.exe`. The daemon and `hitch-hook.exe` sidecars already ship adjacent via Tauri `externalBin`, so the single `hitch` entrypoint is just a copy of the daemon next to its hook. `NSIS_HOOK_POSTUNINSTALL` deletes it.
- **Per-user `PATH` append.** The install dir is appended to `HKCU\Environment\Path` (the `currentUser` install mode means HKCU is the right hive), then `WM_SETTINGCHANGE` is broadcast so already-open shells pick it up without a logout. The append is de-duped (skipped if `;$INSTDIR;` is already present) and removed on uninstall.
- **Type-preserving registry edit.** A user's `PATH` is normally `REG_EXPAND_SZ`. The hook reads the value via a raw `RegQueryValueExW` (NSIS `ReadRegStr` returns `""` for `REG_EXPAND_SZ`) to capture the real type, then writes it back as the **same** type — rewriting a `REG_EXPAND_SZ` `PATH` as `REG_SZ` would freeze any `%VAR%` references in it.
- **Destructive-overwrite fail-safe.** The read also sets an "a non-empty `PATH` exists" guard from the value's byte length *before* any read can fail. If a populated `PATH` is present but can't be read back cleanly, the hook **refuses to write** rather than bare-overwriting (which would wipe the user's entire `PATH`); it leaves `PATH` untouched and the Remote Hosts settings panel shows "not installed" so the user can re-run/repair.
- **Stop the daemon before file ops.** `NSIS_HOOK_PREINSTALL` and `NSIS_HOOK_PREUNINSTALL` run `taskkill /F /T` on `hitch.exe` and `hitch-daemon.exe` first. The daemon sidecar runs detached and keeps its `.exe` open; without stopping it, an upgrade-over-a-running-daemon fails with NSIS `Error opening file for writing: hitch-daemon.exe`. Tauri's template only closes the GUI app, so the daemon (and the `hitch.exe` copy of it) must be stopped here.

## Long paths (MAX_PATH)

Managed worktrees live deep under `%LOCALAPPDATA%\Hitch\worktrees\…` (ADR 0012), where the legacy 260-character `MAX_PATH` limit can truncate a checkout on a long root. Hitch mitigates this in two complementary ways:

- **Long-path-aware app manifest.** `apps/desktop/src-tauri/windows-app-manifest.xml` sets `<longPathAware>true</longPathAware>` (the 2016 `windowsSettings` namespace) and is embedded into the GUI executable through `tauri-build` (`WindowsAttributes::app_manifest` in `build.rs`). It starts from Tauri's default manifest, so the required `Microsoft.Windows.Common-Controls` v6 dependency is preserved. **This setting only takes effect when the OS `LongPathsEnabled` policy is also enabled** (`HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled = 1`, Windows 10 1607+); without it, manifest-only long-path support does not apply.
- **`\\?\` prefixing in `hitch-git`.** For its own managed-worktree lifecycle filesystem access (directory creation, existence/`is_dir` probes), `hitch-git` converts the absolute path to its extended-length (`\\?\C:\…`, or `\\?\UNC\…` for UNC) form before handing it to `std::fs`. This opts the individual syscall out of `MAX_PATH` **regardless of the manifest or the `LongPathsEnabled` policy**, and so covers the daemon and hook sidecars too — they pick up long-path support from this prefixing even though the manifest is embedded only in the GUI exe. Git CLI arguments, display strings, and paths stored or sent to the GUI keep the normal (non-prefixed) form.

If a worktree path still cannot be represented in extended-length form (e.g. a non-absolute path), Hitch surfaces a clear error rather than operating on a truncated path.
