# Windows packaging

Hitch publishes Windows desktop artifacts for `x86_64-pc-windows-msvc` from `windows-latest` CI runners. The release artifact is a Tauri Windows installer for 64-bit Windows.

## Supported runtime

- OS: Windows 10 or Windows 11, x64.
- WebView: Microsoft Edge WebView2 Runtime. The installer uses Tauri's `downloadBootstrapper` WebView2 install mode, so it can download and install/update the Evergreen Runtime when needed. Machines without WebView2 need network access during installation, or WebView2 must be preinstalled by the user/administrator.
- Git: Git must be available on `PATH` for git-backed projects and worktrees.

The packaged app includes the Hitch daemon and hook as Tauri sidecars using target-triple suffixed binaries for the Windows target.
