# hitch — FROZEN (2026-07-22)

This repo is frozen. The Rust daemon workspace (`crates/*`), `CONTEXT.md`, and
`docs/adr` moved to **`~/Code/hitch-native`** (plain copy; history stays here), where
the native Swift successor app is being built. See `hitch-native/FEATURES.md` for the
architecture decision record.

The Tauri app here still builds and runs against its own copy of the crates, but
receives no further changes. Do not edit the daemon here — the living copy is in
hitch-native.
