//! Self-install of the `hitch` CLI on PATH (ADR 0014 amendment).
//!
//! A Hitch-installed machine should be reachable as an SSH remote host without
//! the user manually running `ln -s`. The Hitch client learns the daemon's
//! absolute path from the Hello handshake and invokes it directly, and the first
//! connect targets the known self-install location (`~/.local/bin/hitch`). A
//! drag-installed `.app`/installer has no post-install hook, so Hitch
//! self-installs idempotently on first launch and exposes a visible status/repair
//! control in the Remote Hosts settings section.
//!
//! What "install" does on Unix is PURE SYMLINK with ZERO dotfile edits — it never
//! touches a user's `~/.zshenv`/`~/.bashrc`/`~/.profile`:
//!   - Resolve the bundled daemon binary (the same one `daemon_binary_path()`
//!     finds beside the app exe) and the bundled `hitch-hook` BESIDE it.
//!   - Create TWO symlinks in `~/.local/bin` (created if missing — a
//!     user-writable dir that needs NO sudo):
//!       - `~/.local/bin/hitch`      → bundled `hitch-daemon`
//!       - `~/.local/bin/hitch-hook` → bundled `hitch-hook`
//!     The second link is REQUIRED: on macOS `std::env::current_exe()` returns
//!     the SYMLINK path (`~/.local/bin/hitch`), and the daemon resolves
//!     `hitch-hook` BESIDE its own exe — so the hook must live next to the link.
//!   - Conflict is worst-case across the two links, all-or-nothing: if EITHER
//!     path holds a foreign (non-ours) file/symlink we report `conflict` and
//!     install NOTHING, so a user's own `~/.local/bin/hitch` is never clobbered.
//!
//! A separate ONE-TIME legacy strip ([`strip_legacy_managed_blocks`]) sweeps the
//! old `# >>> hitch cli >>>` PATH block out of the shell rc files left by the
//! previous (dotfile-writing) version. It is idempotent and a no-op on clean
//! machines (a block-less file is left byte-identical), gated by a migration
//! marker in lib.rs so it runs at most once.
//!
//! Trust boundary (unchanged from ADR 0014): this is LOCAL-only. Hitch never
//! uploads binaries over the network to a remote. Self-install just makes THIS
//! machine reachable when someone SSHes into it.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// The user-facing state of the local `hitch` CLI install (mirrored by the
/// `CliInstallStatus` TS type). `state` drives the settings UI; the other fields
/// give it the concrete paths to render and the manual-fallback command.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallStatus {
    /// `installed` — both our symlinks are present and point at the bundled binaries.
    /// `not-installed` — a link is absent/stale (and none foreign); we can install.
    /// `conflict` — a foreign file/symlink occupies EITHER link path; we won't clobber.
    /// `unavailable` — the bundled daemon/hook can't be resolved (e.g. a dev build),
    ///                 or the platform needs a manual install (Windows).
    pub state: &'static str,
    /// Absolute path of the primary link (`~/.local/bin/hitch`), when known.
    pub link_path: Option<String>,
    /// Absolute path of the bundled daemon the primary link targets, when resolvable.
    pub target: Option<String>,
    /// A short human detail for the conflict/unavailable cases (the foreign path,
    /// or why it's unavailable). `None` for the clean states.
    pub detail: Option<String>,
}

impl CliInstallStatus {
    fn unavailable(detail: impl Into<String>, link_path: Option<String>) -> Self {
        Self {
            state: "unavailable",
            link_path,
            target: None,
            detail: Some(detail.into()),
        }
    }
}

// ---- legacy managed-block strip (pure, tested) ----------------------------
//
// The previous version of self-install wrote a managed PATH block into the
// user's shell rc files. We no longer write it, but we must be able to sweep it
// back out of machines that ran the old version. Only `remove_managed_block`
// (and the markers + splitter it needs) is retained — the write side is gone.

/// Markers bounding the legacy block the old version wrote. Removal keys off
/// these EXACT lines, so we only ever touch a block we wrote.
const BLOCK_START: &str = "# >>> hitch cli >>>";
const BLOCK_END: &str = "# <<< hitch cli <<<";

/// Remove the legacy Hitch-managed PATH block from `content`, returning the new
/// content. Idempotent: a no-op (returns an equivalent string) when no block is
/// present. Removes ONLY the marked block and the single blank line the old
/// version inserted before it, leaving every other line untouched — so a
/// block-less file comes back byte-identical.
pub fn remove_managed_block(content: &str) -> String {
    let Some((before, after)) = split_around_block(content) else {
        return content.to_string();
    };
    // `before` ends at the start marker, `after` begins right after the end
    // marker (it usually starts with a trailing "\n"). Trim one separating blank
    // line we own from the boundary so removal doesn't leave a widening gap.
    let before = before.trim_end_matches('\n');
    let after = after.trim_start_matches('\n');
    let joined = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (true, false) => after.to_string(),
        (false, true) => format!("{before}\n"),
        (false, false) => format!("{before}\n{after}"),
    };
    joined
}

/// Split `content` into the text strictly before the start marker and the text
/// strictly after the end marker line, or `None` if the block isn't present (or
/// is malformed — only a start, or end-before-start). The markers themselves and
/// the block body are dropped from the returned halves.
fn split_around_block(content: &str) -> Option<(String, String)> {
    let start = content.find(BLOCK_START)?;
    let end_marker = content.get(start..)?.find(BLOCK_END)? + start;
    if end_marker < start {
        return None;
    }
    // Extend `end` past the end-marker line (up to and including its newline).
    let after_end_marker = end_marker + BLOCK_END.len();
    let rest = &content[after_end_marker..];
    let after = rest.strip_prefix('\n').map(|s| s).unwrap_or(rest);
    let before = &content[..start];
    Some((before.to_string(), after.to_string()))
}

/// The shell rc files the old version wrote a managed block into, which the
/// one-time strip sweeps clean: `~/.zshenv`, `~/.bashrc`, `~/.profile`.
fn legacy_rc_files() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".zshenv"),
        home.join(".bashrc"),
        home.join(".profile"),
    ]
}

/// One-time sweep of the legacy managed PATH block out of the shell rc files the
/// old (dotfile-writing) version touched. Idempotent and best-effort per file: an
/// absent file is skipped; a block-less file is left byte-identical (never
/// rewritten); a write failure on one file doesn't abort the others. Safe to call
/// on a clean machine — it's a no-op there. Gated by a migration marker in lib.rs
/// so it runs at most once.
pub fn strip_legacy_managed_blocks() {
    for file in legacy_rc_files() {
        let Ok(current) = std::fs::read_to_string(&file) else {
            continue;
        };
        let next = remove_managed_block(&current);
        if next != current {
            let _ = std::fs::write(&file, next);
        }
    }
}

// ---- symlink-conflict decision (pure, tested) -----------------------------

/// What occupies a link path, deciding whether install is a no-op, can proceed,
/// or must back off. Pure over the inspected facts so it is unit-tested without
/// touching the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// Nothing at the link path — safe to create the symlink.
    Absent,
    /// Our symlink already points at the wanted target — already installed.
    OursCorrect,
    /// Our symlink exists but points elsewhere (e.g. an old app location) —
    /// safe to repoint (it's still our managed link).
    OursStale,
    /// A non-symlink file, or a symlink to an unrelated target — DO NOT clobber.
    Foreign,
}

/// Decide the link state from: whether anything exists at the path, whether it is
/// a symlink, the symlink's resolved target (if any), and the wanted target. The
/// "ours" test is link-shape + target equality: a plain file (even named `hitch`)
/// is always Foreign so a user's own script is never destroyed.
pub fn classify_link(
    exists: bool,
    is_symlink: bool,
    current_target: Option<&Path>,
    wanted_target: &Path,
) -> LinkState {
    if !exists && !is_symlink {
        // `!exists` alone can be a dangling symlink; check both.
        return LinkState::Absent;
    }
    if !is_symlink {
        return LinkState::Foreign;
    }
    match current_target {
        Some(t) if paths_equiv(t, wanted_target) => LinkState::OursCorrect,
        Some(_) => {
            // It IS a symlink. The caller passes only symlinks it found at OUR
            // link path, so any symlink there is ours to repoint.
            LinkState::OursStale
        }
        None => LinkState::OursStale,
    }
}

/// Overall install state for a link path, folding both managed links together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverallState {
    /// Both links present and correct.
    Installed,
    /// At least one link absent/stale, none foreign — installable.
    NotInstalled,
    /// At least one link is foreign — install NOTHING.
    Conflict,
}

/// Fold the two per-link states into a single all-or-nothing outcome:
///   - EITHER foreign ⇒ `Conflict` (install nothing, never clobber).
///   - BOTH `OursCorrect` ⇒ `Installed`.
///   - otherwise (any absent/stale, none foreign) ⇒ `NotInstalled` (installable).
/// Pure so it is unit-tested directly.
pub fn fold_link_states(a: LinkState, b: LinkState) -> OverallState {
    if a == LinkState::Foreign || b == LinkState::Foreign {
        return OverallState::Conflict;
    }
    if a == LinkState::OursCorrect && b == LinkState::OursCorrect {
        return OverallState::Installed;
    }
    OverallState::NotInstalled
}

/// Compare two paths for install-equivalence. Best-effort canonicalization (a
/// symlink target may be relative or not yet canonical); falls back to a literal
/// compare when canonicalize fails (e.g. the target moved).
fn paths_equiv(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

// ---- filesystem-touching install/uninstall/status -------------------------
//
// These wrap the pure helpers above with real I/O. `daemon_path` is injected by
// the Tauri command (it calls `crate::daemon_binary_path()`), so this module has
// no dependency back into the big lib.rs and stays testable.

/// The link directory, `~/.local/bin`. `None` if the home dir can't be resolved.
fn link_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".local").join("bin"))
}

/// The primary link path, `~/.local/bin/hitch` (`hitch.exe` on Windows for
/// symmetry, though Windows takes the manual/installer path).
fn link_path() -> Option<PathBuf> {
    link_dir().map(|d| d.join(link_file_name()))
}

/// The hook link path, `~/.local/bin/hitch-hook` (+ exe suffix). Sits BESIDE the
/// primary link so the daemon (whose `current_exe()` on macOS is the symlink
/// path) resolves the hook next to itself.
fn hook_link_path() -> Option<PathBuf> {
    link_dir().map(|d| d.join(hook_link_file_name()))
}

fn link_file_name() -> String {
    format!("hitch{}", std::env::consts::EXE_SUFFIX)
}

fn hook_link_file_name() -> String {
    format!("hitch-hook{}", std::env::consts::EXE_SUFFIX)
}

/// Resolve the bundled `hitch-hook` path from the bundled daemon path: the hook
/// lives in the SAME directory as the daemon, named `hitch-hook` + the platform
/// exe suffix. `None` if the daemon path has no parent.
fn bundled_hook_path(daemon_path: &Path) -> Option<PathBuf> {
    let dir = daemon_path.parent()?;
    Some(dir.join(format!("hitch-hook{}", std::env::consts::EXE_SUFFIX)))
}

/// Windows status helper: is `dir` a segment of the user's `PATH`? Uses the
/// process `PATH` (which on Windows is the merged user+system environment, so it
/// reflects the installer's HKCU `Environment\Path` edit after a session
/// refresh). Compared case-insensitively and by canonicalized path where
/// possible, since PATH entries and the install dir can differ in case or
/// trailing separators. Defined unconditionally (the caller is a runtime
/// `cfg!(windows)` branch, not a `#[cfg]`), so it must compile on every host;
/// `std::env::split_paths` uses the platform PATH separator.
fn windows_dir_on_user_path(dir: &Path) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let wanted = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let wanted_str = dir.to_string_lossy().to_lowercase();
    for entry in std::env::split_paths(&path) {
        if entry.canonicalize().map(|c| c == wanted).unwrap_or(false) {
            return true;
        }
        // Fall back to a case-insensitive literal compare (Windows PATH is
        // case-insensitive and the dir may not exist on disk to canonicalize).
        let entry_str = entry.to_string_lossy().to_lowercase();
        if entry_str == wanted_str
            || entry_str.trim_end_matches(['\\', '/']) == wanted_str.trim_end_matches(['\\', '/'])
        {
            return true;
        }
    }
    false
}

/// Home directory, via `$HOME` (Unix) / `$USERPROFILE` (Windows). Avoids pulling
/// a new crate just for this.
fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Inspect the live install for `cli_install_status`. Cheap: two symlink stats.
/// Never mutates anything.
pub fn status(daemon_path: Option<PathBuf>) -> CliInstallStatus {
    let link = link_path().map(|p| p.display().to_string());

    // Windows: STATUS-ONLY. Reach-in (copy hitch.exe + per-user PATH edit) is
    // owned by the installer (NSIS hook / WiX fragment); the runtime never
    // installs or uninstalls. We just report whether the installer's work is in
    // place: the install dir (the dir holding the bundled daemon) is on the user
    // PATH AND `hitch.exe` exists beside it. `install`/`uninstall` are no-ops
    // that return this status.
    if cfg!(windows) {
        // The install dir is the parent of the bundled daemon. With no daemon
        // (a dev build) there is nothing to report against -> `unavailable`.
        let Some(daemon) = daemon_path.clone() else {
            return CliInstallStatus::unavailable(
                "The bundled hitch daemon couldn't be found (this is expected in a dev build).",
                link,
            );
        };
        let Some(install_dir) = daemon.parent().map(Path::to_path_buf) else {
            return CliInstallStatus::unavailable(
                "Couldn't resolve the Hitch install directory.",
                link,
            );
        };
        let hitch_exe = install_dir.join(format!("hitch{}", std::env::consts::EXE_SUFFIX));
        let dir_str = install_dir.display().to_string();

        let on_user_path = windows_dir_on_user_path(&install_dir);
        let exe_present = hitch_exe.is_file();

        if on_user_path && exe_present {
            return CliInstallStatus {
                state: "installed",
                link_path: Some(hitch_exe.display().to_string()),
                target: Some(daemon.display().to_string()),
                detail: Some(format!(
                    "Managed by the Hitch installer ({dir_str} is on your PATH)."
                )),
            };
        }

        return CliInstallStatus {
            state: "not-installed",
            link_path: Some(hitch_exe.display().to_string()),
            target: Some(daemon.display().to_string()),
            detail: Some(
                "CLI reach-in is managed by the Hitch installer — reinstall to repair."
                    .into(),
            ),
        };
    }

    let Some(target) = daemon_path else {
        return CliInstallStatus::unavailable(
            "The bundled hitch daemon couldn't be found (this is expected in a dev build).",
            link,
        );
    };
    let Some(link_path) = link_path() else {
        return CliInstallStatus::unavailable("Couldn't resolve your home directory.", None);
    };
    let Some(hook_target) = bundled_hook_path(&target) else {
        return CliInstallStatus::unavailable(
            "Couldn't resolve the bundled hitch-hook path.",
            Some(link_path.display().to_string()),
        );
    };
    let Some(hook_link) = hook_link_path() else {
        return CliInstallStatus::unavailable(
            "Couldn't resolve your home directory.",
            Some(link_path.display().to_string()),
        );
    };
    if !hook_target.is_file() {
        // We can't complete hook adjacency without the bundled hook binary.
        return CliInstallStatus::unavailable(
            format!(
                "The bundled hitch-hook couldn't be found at {} (this is expected in a dev build).",
                hook_target.display()
            ),
            Some(link_path.display().to_string()),
        );
    }

    let daemon_state = inspect_link(&link_path, &target);
    let hook_state = inspect_link(&hook_link, &hook_target);

    match fold_link_states(daemon_state, hook_state) {
        OverallState::Installed => CliInstallStatus {
            state: "installed",
            link_path: Some(link_path.display().to_string()),
            target: Some(target.display().to_string()),
            detail: None,
        },
        OverallState::NotInstalled => CliInstallStatus {
            // Any absent/stale link reads as not-installed so Install/Repair
            // (re)creates/repoints both; the install path is idempotent.
            state: "not-installed",
            link_path: Some(link_path.display().to_string()),
            target: Some(target.display().to_string()),
            detail: None,
        },
        OverallState::Conflict => CliInstallStatus {
            state: "conflict",
            link_path: Some(link_path.display().to_string()),
            target: Some(target.display().to_string()),
            detail: Some(conflict_detail(daemon_state, &link_path, hook_state, &hook_link)),
        },
    }
}

/// Name the conflicting path(s) for a `conflict` status detail.
fn conflict_detail(
    daemon_state: LinkState,
    link_path: &Path,
    hook_state: LinkState,
    hook_link: &Path,
) -> String {
    let mut conflicting = Vec::new();
    if daemon_state == LinkState::Foreign {
        conflicting.push(link_path.display().to_string());
    }
    if hook_state == LinkState::Foreign {
        conflicting.push(hook_link.display().to_string());
    }
    format!(
        "Something else already exists at {} — Hitch won't overwrite it.",
        conflicting.join(" and ")
    )
}

/// Perform the install: create the link dir and symlink BOTH `hitch` → daemon and
/// `hitch-hook` → bundled hook. PURE SYMLINK — never writes a shell rc file.
/// All-or-nothing on a foreign conflict: if either link path holds a foreign file
/// we install neither. Best-effort and idempotent — a missing home dir, missing
/// bundled hook, or a conflict is reported via the returned status, not an `Err`,
/// unless an unexpected I/O error occurs. Returns the post-install status.
pub fn install(daemon_path: Option<PathBuf>) -> Result<CliInstallStatus, String> {
    if cfg!(windows) {
        // Manual / installer-owned on Windows; report status without touching disk.
        return Ok(status(daemon_path));
    }
    let Some(target) = daemon_path.clone() else {
        return Ok(status(None));
    };
    let Some(link_path) = link_path() else {
        return Ok(status(daemon_path));
    };
    let Some(hook_link) = hook_link_path() else {
        return Ok(status(daemon_path));
    };
    let Some(hook_target) = bundled_hook_path(&target) else {
        return Ok(status(daemon_path));
    };
    if !hook_target.is_file() {
        // Can't complete hook adjacency; surface `unavailable` via status.
        return Ok(status(daemon_path));
    }

    let daemon_state = inspect_link(&link_path, &target);
    let hook_state = inspect_link(&hook_link, &hook_target);

    // All-or-nothing: a foreign file on EITHER path aborts the whole install so we
    // never clobber a user's own `~/.local/bin/hitch` or `hitch-hook`.
    if fold_link_states(daemon_state, hook_state) == OverallState::Conflict {
        return Ok(status(daemon_path));
    }

    let dir = link_path.parent().ok_or("link path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    ensure_link(daemon_state, &target, &link_path)?;
    ensure_link(hook_state, &hook_target, &hook_link)?;

    Ok(status(daemon_path))
}

/// Create or repoint a single managed symlink based on its current state.
/// `OursCorrect` is a no-op (idempotent); `Absent`/`OursStale` (re)creates it.
/// `Foreign` is unreachable here — callers fold to `Conflict` and abort first.
fn ensure_link(state: LinkState, target: &Path, link_path: &Path) -> Result<(), String> {
    match state {
        LinkState::OursCorrect => Ok(()),
        LinkState::Foreign => Ok(()), // defensive: never clobber (caller aborted).
        LinkState::Absent | LinkState::OursStale => {
            // Repoint a stale link by removing it first (it's ours).
            if link_path.symlink_metadata().is_ok() {
                let _ = std::fs::remove_file(link_path);
            }
            symlink(target, link_path)
                .map_err(|e| format!("symlink {}: {e}", link_path.display()))
        }
    }
}

/// Uninstall: remove BOTH of OUR symlinks (only if they are ours). Never removes
/// a foreign file. (The legacy dotfile block is swept by the one-time
/// [`strip_legacy_managed_blocks`], not here.) Returns the post-uninstall status.
pub fn uninstall(daemon_path: Option<PathBuf>) -> Result<CliInstallStatus, String> {
    if cfg!(windows) {
        return Ok(status(daemon_path));
    }
    // Remove the primary link if it's a symlink we own; a foreign file is left.
    if let Some(link_path) = link_path() {
        let wanted = daemon_path.clone().unwrap_or_else(|| PathBuf::from(""));
        let state = inspect_link(&link_path, &wanted);
        if matches!(state, LinkState::OursCorrect | LinkState::OursStale) {
            let _ = std::fs::remove_file(&link_path);
        }
    }
    // Remove the hook link likewise.
    if let Some(hook_link) = hook_link_path() {
        let wanted = daemon_path
            .as_deref()
            .and_then(bundled_hook_path)
            .unwrap_or_else(|| PathBuf::from(""));
        let state = inspect_link(&hook_link, &wanted);
        if matches!(state, LinkState::OursCorrect | LinkState::OursStale) {
            let _ = std::fs::remove_file(&hook_link);
        }
    }
    Ok(status(daemon_path))
}

/// Stat the link path and classify it against the wanted target.
fn inspect_link(link_path: &Path, wanted: &Path) -> LinkState {
    let meta = std::fs::symlink_metadata(link_path);
    let exists = meta.is_ok();
    let is_symlink = meta
        .as_ref()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let current_target = std::fs::read_link(link_path).ok();
    classify_link(exists, is_symlink, current_target.as_deref(), wanted)
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    // Unreachable on the Windows install path (status-only / installer-owned), but
    // keep the workspace building: a file symlink needs the privilege we don't
    // assume.
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ---- legacy managed-block strip (remove only; round-trip + no-op) ----

    /// Re-create a representative legacy block so the strip tests have a real
    /// payload to remove. Mirrors the body the old version wrote (markers + a
    /// PATH guard), but lives only in the tests now.
    fn legacy_block(dir: &str) -> String {
        format!(
            "{BLOCK_START}\n\
             # Added by Hitch so `ssh <host> hitch …` finds the CLI in a non-interactive shell.\n\
             case \":$PATH:\" in\n\
             \x20\x20*\":{dir}:\"*) ;;\n\
             \x20\x20*) export PATH=\"{dir}:$PATH\" ;;\n\
             esac\n\
             {BLOCK_END}"
        )
    }

    #[test]
    fn remove_strips_block_and_owned_blank_line() {
        // A file the old version produced: content, one blank line, then the block.
        let original = "export FOO=1\n";
        let added = format!("export FOO=1\n\n{}\n", legacy_block("/d/bin"));
        let removed = remove_managed_block(&added);
        assert_eq!(removed, original, "remove must restore the original file");
    }

    #[test]
    fn remove_is_idempotent_and_noop_without_block() {
        let plain = "export FOO=1\nalias g=git\n";
        // A block-less file comes back byte-identical (never rewritten).
        assert_eq!(remove_managed_block(plain), plain);
        let added = format!("{plain}\n{}\n", legacy_block("/d/bin"));
        let once = remove_managed_block(&added);
        let twice = remove_managed_block(&once);
        assert_eq!(once, twice);
        assert!(!once.contains(BLOCK_START));
    }

    #[test]
    fn remove_preserves_surrounding_content() {
        let pre = "line A\nline B\n";
        let post = "line C\nline D\n";
        let mid = legacy_block("/d/bin");
        let content = format!("{pre}{mid}\n{post}");
        let removed = remove_managed_block(&content);
        assert!(removed.contains("line A"));
        assert!(removed.contains("line D"));
        assert!(!removed.contains(BLOCK_START));
    }

    #[test]
    fn remove_roundtrips_for_blockless_middle() {
        // Block sandwiched between content on both sides round-trips cleanly.
        let original = "first\nsecond\n";
        let added = format!("first\nsecond\n\n{}\n", legacy_block("/d/bin"));
        let removed = remove_managed_block(&added);
        assert_eq!(removed, original);
    }

    /// The one-time strip leaves a block-less file BYTE-IDENTICAL and removes a
    /// present block — exercised through `remove_managed_block`, the pure core the
    /// I/O wrapper calls (the wrapper only writes back when content changes).
    #[test]
    fn legacy_strip_is_byte_identical_without_block_and_removes_present_block() {
        // No block: byte-identical, and the wrapper would write nothing.
        let clean = "# my zshenv\nexport EDITOR=vim\n";
        let stripped = remove_managed_block(clean);
        assert_eq!(stripped, clean);
        // With a block: the block (and its owned blank line) is gone.
        let dirty = format!("{clean}\n{}\n", legacy_block("/home/u/.local/bin"));
        let cleaned = remove_managed_block(&dirty);
        assert_eq!(cleaned, clean);
        assert!(!cleaned.contains(BLOCK_START));
        assert!(!cleaned.contains(BLOCK_END));
    }

    // ---- per-link symlink classification ----

    #[test]
    fn classify_absent_when_nothing_exists() {
        let wanted = Path::new("/opt/hitch/hitch-daemon");
        assert_eq!(
            classify_link(false, false, None, wanted),
            LinkState::Absent
        );
    }

    #[test]
    fn classify_foreign_for_plain_file() {
        let wanted = Path::new("/opt/hitch/hitch-daemon");
        assert_eq!(
            classify_link(true, false, None, wanted),
            LinkState::Foreign,
            "a non-symlink (even named hitch) must never be clobbered"
        );
    }

    #[test]
    fn classify_ours_correct_when_target_matches() {
        let wanted = Path::new("/opt/hitch/hitch-daemon");
        assert_eq!(
            classify_link(true, true, Some(wanted), wanted),
            LinkState::OursCorrect
        );
    }

    #[test]
    fn classify_ours_stale_when_symlink_points_elsewhere() {
        let wanted = Path::new("/opt/hitch-2/hitch-daemon");
        let current = Path::new("/opt/hitch-1/hitch-daemon");
        assert_eq!(
            classify_link(true, true, Some(current), wanted),
            LinkState::OursStale,
            "a symlink in our dir pointing at an old location is ours to repoint"
        );
    }

    #[test]
    fn classify_dangling_symlink_is_ours_stale() {
        // exists=false (broken link) but is_symlink=true, no readable target.
        let wanted = Path::new("/opt/hitch/hitch-daemon");
        assert_eq!(
            classify_link(false, true, None, wanted),
            LinkState::OursStale
        );
    }

    // ---- two-link worst-case fold (all-or-nothing) ----

    #[test]
    fn fold_both_correct_is_installed() {
        assert_eq!(
            fold_link_states(LinkState::OursCorrect, LinkState::OursCorrect),
            OverallState::Installed
        );
    }

    #[test]
    fn fold_foreign_on_either_link_is_conflict() {
        // Foreign on the daemon link.
        assert_eq!(
            fold_link_states(LinkState::Foreign, LinkState::OursCorrect),
            OverallState::Conflict
        );
        // Foreign on the hook link.
        assert_eq!(
            fold_link_states(LinkState::OursCorrect, LinkState::Foreign),
            OverallState::Conflict
        );
        // Foreign wins even over an otherwise-installable mix.
        assert_eq!(
            fold_link_states(LinkState::Absent, LinkState::Foreign),
            OverallState::Conflict
        );
    }

    #[test]
    fn fold_mixed_absent_or_stale_is_installable() {
        for (a, b) in [
            (LinkState::Absent, LinkState::Absent),
            (LinkState::Absent, LinkState::OursCorrect),
            (LinkState::OursCorrect, LinkState::OursStale),
            (LinkState::OursStale, LinkState::OursStale),
            (LinkState::OursStale, LinkState::Absent),
        ] {
            assert_eq!(
                fold_link_states(a, b),
                OverallState::NotInstalled,
                "mix of absent/ours (no foreign) must be installable: {a:?}, {b:?}"
            );
        }
    }

    #[test]
    fn fold_plain_file_on_either_link_is_never_clobbered() {
        // A plain file (even named `hitch` or `hitch-hook`) classifies Foreign and
        // folds to Conflict ⇒ install nothing.
        let wanted = Path::new("/opt/hitch/hitch-daemon");
        let plain = classify_link(true, false, None, wanted);
        assert_eq!(plain, LinkState::Foreign);
        assert_eq!(
            fold_link_states(plain, LinkState::Absent),
            OverallState::Conflict
        );
        assert_eq!(
            fold_link_states(LinkState::Absent, plain),
            OverallState::Conflict
        );
    }
}
