//! `hitch-agent` — known-agent integration for Hitch (ADR 0002).
//!
//! This crate owns Hitch's built-in agent registry and the filesystem changes
//! required for hook-based state reporting. It deliberately has no daemon or
//! socket code: hook commands invoke the `hitch-hook` helper, and the daemon is
//! responsible for calling this crate once worktree paths are known.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hitch_core::AgentState;
use serde_json::{json, Map, Value};

/// A known CLI agent Hitch knows how to launch and install hooks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    /// Anthropic Claude Code CLI (`claude`).
    ClaudeCode,
    /// OpenAI Codex CLI (`codex`).
    Codex,
}

impl AgentKind {
    /// Stable CLI/protocol spelling used by `hitch-hook --agent`.
    pub const fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }

    /// Human-facing agent name.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

/// Built-in registry metadata for launching and hook installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentDescriptor {
    pub kind: AgentKind,
    pub display_name: &'static str,
    pub executable: &'static str,
    pub default_args: &'static [&'static str],
    pub local_config_path: &'static str,
}

const CLAUDE_ARGS: &[&str] = &[];
const CODEX_ARGS: &[&str] = &[];
const REGISTRY: &[AgentDescriptor] = &[
    AgentDescriptor {
        kind: AgentKind::ClaudeCode,
        display_name: "Claude Code",
        executable: "claude",
        default_args: CLAUDE_ARGS,
        local_config_path: ".claude/settings.local.json",
    },
    AgentDescriptor {
        kind: AgentKind::Codex,
        display_name: "Codex",
        executable: "codex",
        default_args: CODEX_ARGS,
        local_config_path: ".codex/hooks.json",
    },
];

/// Return the built-in agent registry.
pub fn registry() -> &'static [AgentDescriptor] {
    REGISTRY
}

/// Options for installing agent hooks into a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInstallOptions {
    /// Absolute path to the `hitch-hook` helper binary written into agent config.
    pub helper_path: PathBuf,
    /// Path to the `git` binary used to detect whether a legacy `.gitignore` is
    /// tracked before cleaning it up. Defaults to `git` on `PATH`; the daemon
    /// overrides it with its configured `--git` path via [`Self::with_git`].
    pub git_path: PathBuf,
}

impl HookInstallOptions {
    pub fn new(helper_path: impl Into<PathBuf>) -> Self {
        Self {
            helper_path: helper_path.into(),
            git_path: PathBuf::from("git"),
        }
    }

    /// Override the `git` binary used for tracked-file detection.
    #[must_use]
    pub fn with_git(mut self, git_path: impl Into<PathBuf>) -> Self {
        self.git_path = git_path.into();
        self
    }
}

/// Summary of a hook installation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInstallSummary {
    pub installed_configs: Vec<InstalledHookConfig>,
    pub local_exclude_updated: bool,
    /// Whether a legacy Hitch-owned root `.gitignore` (written by pre-exclude
    /// installs) was cleaned up during this run.
    pub legacy_gitignore_removed: bool,
}

/// One local agent config touched by hook installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledHookConfig {
    pub agent: AgentKind,
    pub path: PathBuf,
}

/// Install/merge every known agent hook into `worktree_path`.
///
/// Existing user config is preserved. Object keys are merged recursively; array
/// values receive Hitch's hook entries only if those exact entries are absent,
/// making repeated installation idempotent.
pub fn install_hooks(
    worktree_path: impl AsRef<Path>,
    options: &HookInstallOptions,
) -> Result<HookInstallSummary, AgentHookError> {
    let worktree_path = worktree_path.as_ref();
    let helper_path = normalize_helper_path(&options.helper_path)?;

    let mut installed_configs = Vec::new();
    // `.codex/config.local.json` is the obsolete pre-`hooks.json` Codex config.
    // It's still excluded so worktrees migrated from a pre-exclude install (which
    // ignored it via the now-removed root `.gitignore`) don't expose that orphan
    // file as dirty once `remove_legacy_gitignore_entries` strips the old line.
    let local_config_entries = [
        ".claude/settings.local.json",
        ".codex/hooks.json",
        ".codex/config.local.json",
    ];

    install_claude_hooks(worktree_path, &helper_path, &mut installed_configs)?;

    install_codex_hooks(worktree_path, &helper_path, &mut installed_configs)?;

    let local_exclude_updated = ensure_locally_excluded(worktree_path, &local_config_entries)?;

    // Pre-exclude installs ignored these configs via a root `.gitignore` instead
    // of `.git/info/exclude`. Now that we exclude locally, strip those legacy
    // Hitch-owned entries so migrated worktrees don't keep (or depend on) the
    // deprecated tracked file.
    let legacy_gitignore_removed =
        remove_legacy_gitignore_entries(worktree_path, &options.git_path)?;

    Ok(HookInstallSummary {
        installed_configs,
        local_exclude_updated,
        legacy_gitignore_removed,
    })
}

/// Entries that pre-exclude installs appended to a root `.gitignore`.
const LEGACY_GITIGNORE_ENTRIES: &[&str] =
    &[".claude/settings.local.json", ".codex/config.local.json"];

/// Remove Hitch's legacy `.gitignore` lines from `worktree_path`. Lines the user
/// added are preserved; if stripping ours empties the file, the file is deleted
/// (matching what a fresh exclude-based install leaves behind). Returns whether
/// anything changed.
///
/// Only Hitch's own untracked droppings are touched: a `.gitignore` the user has
/// committed is left entirely alone, since deleting or rewriting a tracked file
/// would dirty the repo and discard ignore rules the user owns.
fn remove_legacy_gitignore_entries(
    worktree_path: &Path,
    git_path: &Path,
) -> Result<bool, AgentHookError> {
    let path = worktree_path.join(".gitignore");
    let existing = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    };

    if gitignore_is_tracked(worktree_path, git_path) {
        return Ok(false);
    }

    let retained = existing
        .lines()
        .filter(|line| !LEGACY_GITIGNORE_ENTRIES.contains(&line.trim()))
        .collect::<Vec<_>>();
    if retained.len() == existing.lines().count() {
        return Ok(false);
    }

    if retained.iter().all(|line| line.trim().is_empty()) {
        fs::remove_file(&path)?;
    } else {
        let mut updated = retained.join("\n");
        updated.push('\n');
        fs::write(&path, updated)?;
    }
    Ok(true)
}

/// Whether `.gitignore` is tracked by git in `worktree_path`. Returns `true` when
/// git reports the file as tracked, and — conservatively — also when git cannot
/// be run at all (a missing or misconfigured binary), so a tracked `.gitignore`
/// is never mutated just because we failed to ask. A successful `git` run that
/// reports the file as untracked (including a non-repository directory, where the
/// command exits non-zero) yields `false`, letting the legacy cleanup proceed in
/// the common untracked case.
fn gitignore_is_tracked(worktree_path: &Path, git_path: &Path) -> bool {
    Command::new(git_path)
        .current_dir(worktree_path)
        .args(["ls-files", "--error-unmatch", "--", ".gitignore"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        // Could not even spawn git: assume tracked so we don't risk rewriting a
        // file the user owns.
        .map_or(true, |status| status.success())
}

fn install_claude_hooks(
    worktree_path: &Path,
    helper_path: &Path,
    installed_configs: &mut Vec<InstalledHookConfig>,
) -> Result<(), AgentHookError> {
    let path = worktree_path.join(".claude/settings.local.json");
    let overlay = json!({
        "hooks": {
            // Identity-only announce (ADR 0011 amendment 2026-06-05): carries WHICH
            // agent with no Agent State, so the Session mark renders before the
            // first prompt. It must NOT pass `--state` (that would mean "clear").
            "SessionStart": [claude_announce_hook_entry(helper_path, "session-start")],
            "UserPromptSubmit": [claude_hook_entry(helper_path, "user-prompt-submit", AgentState::Running)],
            "PermissionRequest": [claude_hook_entry(helper_path, "permission-request", AgentState::NeedsApproval)],
            // After a deny the agent consumes the denial and finishes its turn, so
            // PermissionDenied -> running heals the sticky `needs-approval` signal
            // (symmetric with PostToolUse).
            "PermissionDenied": [claude_hook_entry(helper_path, "permission-denied", AgentState::Running)],
            // Two distinct Notification matcher groups, both Hitch-owned:
            // `permission_prompt` -> needs-approval, `idle_prompt` -> waiting (the
            // agent's own "done and idle" signal that self-heals stale state).
            "Notification": [
                claude_hook_entry_with_matcher(helper_path, "permission_prompt", "notification", AgentState::NeedsApproval),
                claude_hook_entry_with_matcher(helper_path, "idle_prompt", "notification", AgentState::Waiting)
            ],
            "PostToolUse": [claude_hook_entry(helper_path, "post-tool-use", AgentState::Running)],
            "Stop": [claude_hook_entry(helper_path, "stop", AgentState::Waiting)],
            // Per-matcher StopFailure entries each carry an explicit human-readable
            // `--detail` reason; we never parse the agent's payload to discover why
            // the turn failed (ADR 0011: no text inference).
            "StopFailure": [
                claude_stop_failure_hook_entry(helper_path, "rate_limit", "rate limited"),
                claude_stop_failure_hook_entry(helper_path, "billing_error", "billing issue"),
                claude_stop_failure_hook_entry(helper_path, "server_error", "server error")
            ],
            "SessionEnd": [claude_clear_hook_entry(helper_path, "session-end")]
        }
    });
    merge_json_file(
        &path,
        overlay,
        AgentKind::ClaudeCode,
        &[
            "UserPromptSubmit",
            "PermissionRequest",
            "PermissionDenied",
            "Notification",
            "PostToolUse",
            "Stop",
            "StopFailure",
            "SessionStart",
            "SessionEnd",
        ],
    )?;
    installed_configs.push(InstalledHookConfig {
        agent: AgentKind::ClaudeCode,
        path,
    });
    Ok(())
}

fn install_codex_hooks(
    worktree_path: &Path,
    helper_path: &Path,
    installed_configs: &mut Vec<InstalledHookConfig>,
) -> Result<(), AgentHookError> {
    let path = worktree_path.join(".codex/hooks.json");
    let overlay = json!({
        "hooks": {
            // Identity-only announce, same shape as Claude's. Codex's `SessionStart`
            // is a documented, valid event (ADR 0011 amendment 2026-06-05).
            "SessionStart": [codex_announce_hook_entry(helper_path, "session-start")],
            "UserPromptSubmit": [codex_hook_entry(helper_path, "user-prompt-submit", AgentState::Running)],
            "PermissionRequest": [codex_hook_entry(helper_path, "permission-request", AgentState::NeedsApproval)],
            "PostToolUse": [codex_hook_entry(helper_path, "post-tool-use", AgentState::Running)],
            "Stop": [codex_hook_entry(helper_path, "stop", AgentState::Waiting)]
            // No `SessionEnd`: the event does not exist in Codex (the old install's
            // entry was silently ignored). It is still pruned below so re-installing
            // over an old config migrates it away.
        }
    });
    merge_json_file(
        &path,
        overlay,
        AgentKind::Codex,
        &[
            "UserPromptSubmit",
            "PermissionRequest",
            "PostToolUse",
            "Stop",
            "SessionStart",
            // Pruned so old installs migrate: Codex never had a real SessionEnd.
            "SessionEnd",
        ],
    )?;
    installed_configs.push(InstalledHookConfig {
        agent: AgentKind::Codex,
        path,
    });
    Ok(())
}

fn codex_hook_entry(helper_path: &Path, event: &str, state: AgentState) -> Value {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": hook_command(helper_path, AgentKind::Codex, event, Some(state))
        }]
    })
}

fn codex_announce_hook_entry(helper_path: &Path, event: &str) -> Value {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": announce_command(helper_path, AgentKind::Codex, event)
        }]
    })
}

fn claude_announce_hook_entry(helper_path: &Path, event: &str) -> Value {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": announce_command(helper_path, AgentKind::ClaudeCode, event)
        }]
    })
}

/// A Claude `StopFailure` entry scoped to a single failure `matcher`, carrying an
/// explicit human-readable `--detail` reason. Hitch never parses the agent's
/// payload to discover why the turn failed (ADR 0011: no text inference); the
/// matcher selects the entry and the installer hard-codes the reason string.
fn claude_stop_failure_hook_entry(helper_path: &Path, matcher: &str, detail: &str) -> Value {
    let mut command = hook_command(
        helper_path,
        AgentKind::ClaudeCode,
        "stop-failure",
        Some(AgentState::Error),
    );
    let style = command_arg_style_for_path(helper_path);
    command.push_str(" --detail ");
    command.push_str(&platform_command_arg(detail, style));
    json!({
        "matcher": matcher,
        "hooks": [{
            "type": "command",
            "command": command
        }]
    })
}

fn claude_clear_hook_entry(helper_path: &Path, event: &str) -> Value {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": hook_command(helper_path, AgentKind::ClaudeCode, event, None)
        }]
    })
}
fn claude_hook_entry(helper_path: &Path, event: &str, state: AgentState) -> Value {
    claude_hook_entry_with_matcher(helper_path, "", event, state)
}

fn claude_hook_entry_with_matcher(
    helper_path: &Path,
    matcher: &str,
    event: &str,
    state: AgentState,
) -> Value {
    json!({
        "matcher": matcher,
        "hooks": [{
            "type": "command",
            "command": hook_command(helper_path, AgentKind::ClaudeCode, event, Some(state))
        }]
    })
}

/// Build a helper invocation for an **identity announce**: it names which agent
/// (via `--agent`) and the `session-start` event, plus an explicit `--announce`
/// flag, and carries NO `--state`. The announce is identity, not Agent State, so
/// it must never pass `--state` — that would be read as "clear" (ADR 0011
/// amendment 2026-06-05). The explicit flag mirrors how state entries carry an
/// explicit `--state`, so the installed entry does not depend on the helper's
/// event mapping.
fn announce_command(helper_path: &Path, agent: AgentKind, event: &str) -> String {
    let mut command = hook_command_prefix(helper_path, agent, event);
    command.push_str(" --announce");
    command
}

fn hook_command(
    helper_path: &Path,
    agent: AgentKind,
    event: &str,
    state: Option<AgentState>,
) -> String {
    let mut command = hook_command_prefix(helper_path, agent, event);
    if let Some(state) = state {
        command.push_str(" --state ");
        command.push_str(state_arg(state));
    }
    command
}

/// The shared `<helper> --agent <id> --event <event>` prefix, written in the
/// shell each agent evaluates hook commands with (see the per-agent notes below).
fn hook_command_prefix(helper_path: &Path, agent: AgentKind, event: &str) -> String {
    let style = command_arg_style_for_path(helper_path);
    // Each agent evaluates hook commands with a different shell, so the command
    // must be written in that shell's language:
    // - Claude Code runs hooks through Git Bash on Windows and `sh` elsewhere —
    //   a POSIX shell either way, so a single-quoted path invokes directly and
    //   stays free of `$`/backtick expansion (see `windows_command_arg`).
    // - Codex runs hooks through the platform default shell — PowerShell on
    //   Windows (`powershell -Command <string>`). There a quoted path is a string
    //   EXPRESSION, not an invocation: `"C:\...\hitch-hook.exe" --agent codex`
    //   is a parse error, the helper never spawns, and Codex reports the hook as
    //   failed. PowerShell needs the call operator and a single-quoted path:
    //   `& 'C:\...\hitch-hook.exe' --agent codex ...`. Single quotes also keep
    //   the string free of `"` characters, which would otherwise be mangled by
    //   the Rust-side argv quoting Codex uses to pass the command to PowerShell.
    if agent == AgentKind::Codex && style == CommandArgStyle::Windows {
        format!(
            "& {} --agent {} --event {}",
            hitch_core::powershell_single_quoted(&helper_path.to_string_lossy()),
            agent.id(),
            event
        )
    } else {
        format!(
            "{} --agent {} --event {}",
            platform_command_arg(&helper_path.to_string_lossy(), style),
            agent.id(),
            platform_command_arg(event, style)
        )
    }
}

fn state_arg(state: AgentState) -> &'static str {
    match state {
        AgentState::Running => "running",
        AgentState::NeedsApproval => "needs-approval",
        AgentState::Waiting => "waiting",
        AgentState::Error => "error",
    }
}

fn merge_json_file(
    path: &Path,
    overlay: Value,
    agent: AgentKind,
    prune_events: &[&str],
) -> Result<(), AgentHookError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut base = match fs::read_to_string(path) {
        Ok(contents) if contents.trim().is_empty() => Value::Object(Map::new()),
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|source| AgentHookError::InvalidJson {
                path: path.into(),
                source,
            })?
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(err) => return Err(err.into()),
    };

    if !base.is_object() {
        return Err(AgentHookError::InvalidConfigShape {
            path: path.into(),
            message: "top-level config must be a JSON object".into(),
        });
    }

    prune_hitch_hooks(&mut base, agent, prune_events);
    merge_preserving_existing(&mut base, overlay);
    let rendered = serde_json::to_string_pretty(&base)?;
    fs::write(path, format!("{rendered}\n"))?;
    Ok(())
}

fn prune_hitch_hooks(base: &mut Value, agent: AgentKind, events: &[&str]) {
    let Some(hooks) = base.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };

    for event in events {
        let remove_event = {
            let Some(groups) = hooks.get_mut(*event).and_then(Value::as_array_mut) else {
                continue;
            };

            groups.retain_mut(|group| {
                let Some(commands) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                    return true;
                };
                let had_commands = !commands.is_empty();
                commands.retain(|hook| !is_hitch_hook_command(hook, agent));
                !had_commands || !commands.is_empty()
            });
            groups.is_empty()
        };
        if remove_event {
            hooks.remove(*event);
        }
    }
}

fn is_hitch_hook_command(hook: &Value, agent: AgentKind) -> bool {
    let agent_flag = match agent {
        AgentKind::ClaudeCode => "--agent claude-code",
        AgentKind::Codex => "--agent codex",
    };
    hook.get("type").and_then(Value::as_str) == Some("command")
        && hook
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains("hitch-hook") && command.contains(agent_flag))
}

fn merge_preserving_existing(base: &mut Value, overlay: Value) {
    if base.is_null() {
        *base = overlay;
        return;
    }

    match overlay {
        Value::Object(overlay) => {
            if let Value::Object(base) = base {
                for (key, value) in overlay {
                    match base.get_mut(&key) {
                        Some(existing) => merge_preserving_existing(existing, value),
                        None => {
                            base.insert(key, value);
                        }
                    }
                }
            }
        }
        Value::Array(overlay) => {
            if let Value::Array(base) = base {
                for value in overlay {
                    if !base.contains(&value) {
                        base.push(value);
                    }
                }
            }
        }
        // Preserve user-owned scalar/mismatched values instead of clobbering.
        _ => {}
    }
}

fn ensure_locally_excluded(worktree_path: &Path, entries: &[&str]) -> Result<bool, AgentHookError> {
    // Git reads `info/exclude` from the *common* git dir shared by every linked
    // worktree, not from a linked worktree's per-worktree gitdir, so excludes
    // must be written there or they are silently ignored.
    let Some(git_dir) = resolve_common_git_dir(worktree_path)? else {
        return Ok(false);
    };
    let info_dir = git_dir.join("info");
    fs::create_dir_all(&info_dir)?;
    let path = info_dir.join("exclude");
    let existing = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };

    let existing_lines = existing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    let missing = entries
        .iter()
        .copied()
        .filter(|entry| !existing_lines.contains(entry))
        .collect::<Vec<_>>();

    if missing.is_empty() {
        return Ok(false);
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    for entry in missing {
        updated.push_str(entry);
        updated.push('\n');
    }
    fs::write(path, updated)?;
    Ok(true)
}

/// Resolve the *common* git directory for `worktree_path`.
///
/// Git applies `$GIT_DIR/info/exclude` from the common git dir, which is shared
/// by the main worktree and every linked worktree — not from a linked worktree's
/// per-worktree gitdir (`<repo>/.git/worktrees/<name>`). A linked worktree's
/// gitdir contains a `commondir` file pointing at that shared dir; for the main
/// worktree the resolved gitdir already is the common dir.
fn resolve_common_git_dir(worktree_path: &Path) -> Result<Option<PathBuf>, AgentHookError> {
    let Some(git_dir) = resolve_git_dir(worktree_path)? else {
        return Ok(None);
    };
    let common = match fs::read_to_string(git_dir.join("commondir")) {
        Ok(contents) => {
            let trimmed = contents.trim();
            let candidate = Path::new(trimmed);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                // A relative `commondir` is resolved against the gitdir. The
                // embedded `..` segments are left for the OS to resolve at I/O
                // time; we only ever join `info/exclude` onto this path.
                git_dir.join(trimmed)
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => git_dir,
        Err(err) => return Err(err.into()),
    };
    Ok(Some(common))
}

fn resolve_git_dir(worktree_path: &Path) -> Result<Option<PathBuf>, AgentHookError> {
    let dot_git = worktree_path.join(".git");
    match fs::metadata(&dot_git) {
        Ok(metadata) if metadata.is_dir() => Ok(Some(dot_git)),
        Ok(_) => resolve_git_dir_file(worktree_path, &dot_git).map(Some),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn resolve_git_dir_file(worktree_path: &Path, dot_git: &Path) -> Result<PathBuf, AgentHookError> {
    let contents = fs::read_to_string(dot_git)?;
    let raw_path = contents
        .trim()
        .strip_prefix("gitdir:")
        .ok_or_else(|| AgentHookError::InvalidGitDirFile {
            path: dot_git.into(),
            message: "missing gitdir prefix".into(),
        })?
        .trim();
    if raw_path.is_empty() {
        return Err(AgentHookError::InvalidGitDirFile {
            path: dot_git.into(),
            message: "empty gitdir path".into(),
        });
    }
    let git_dir = PathBuf::from(raw_path);
    if git_dir.is_absolute() {
        Ok(git_dir)
    } else {
        Ok(worktree_path.join(git_dir))
    }
}

fn normalize_helper_path(path: &Path) -> Result<PathBuf, AgentHookError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn command_arg_style_for_path(path: &Path) -> CommandArgStyle {
    if is_windows_absolute_path(&path.to_string_lossy()) {
        CommandArgStyle::Windows
    } else {
        command_arg_style()
    }
}

fn is_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.starts_with(br"\\")
        || (bytes.len() >= 3
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/')
            && bytes[0].is_ascii_alphabetic())
}

#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandArgStyle {
    Posix,
    Windows,
}

#[cfg(windows)]
fn command_arg_style() -> CommandArgStyle {
    CommandArgStyle::Windows
}

#[cfg(not(windows))]
fn command_arg_style() -> CommandArgStyle {
    CommandArgStyle::Posix
}

fn platform_command_arg(value: &str, style: CommandArgStyle) -> String {
    match style {
        CommandArgStyle::Posix => posix_command_arg(value),
        CommandArgStyle::Windows => windows_command_arg(value),
    }
}

fn posix_command_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn windows_command_arg(value: &str) -> String {
    // This style is only ever reached for Claude Code on Windows, which evaluates
    // hook commands through Git Bash (a POSIX `sh`) — Codex's PowerShell path is
    // handled separately in `hook_command`. So quote for bash, not cmd.
    //
    // Single quotes, not double. Inside bash double quotes `$` and backtick still
    // expand and `\` still escapes them, so a helper path containing `$`/backtick
    // (e.g. `C:\Users\$env\hitch-hook.exe`) would be mangled by variable/command
    // substitution. Single quotes are fully literal in POSIX shells: every
    // character — including the Windows path backslashes — is preserved verbatim,
    // with no expansion. (An embedded `'` ends the quote and re-enters via the
    // `'\''` idiom; Windows paths can't contain `'` but the helper handles it for
    // safety.) Bare identifiers like event names have nothing to escape and stay
    // unquoted so the common command reads cleanly.
    if value.is_empty()
        || value.bytes().any(|byte| {
            matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b'\\' | b'$' | b'`')
        })
    {
        posix_command_arg(value)
    } else {
        value.to_owned()
    }
}

/// Hook installation/parsing error.
#[derive(Debug)]
pub enum AgentHookError {
    Io(io::Error),
    Serde(serde_json::Error),
    InvalidJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    InvalidConfigShape {
        path: PathBuf,
        message: String,
    },
    InvalidGitDirFile {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for AgentHookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Serde(err) => write!(f, "{err}"),
            Self::InvalidJson { path, source } => {
                write!(f, "invalid JSON in {}: {source}", path.display())
            }
            Self::InvalidConfigShape { path, message } => {
                write!(f, "invalid config shape in {}: {message}", path.display())
            }
            Self::InvalidGitDirFile { path, message } => {
                write!(f, "invalid gitdir file {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for AgentHookError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Serde(err) => Some(err),
            Self::InvalidJson { source, .. } => Some(source),
            Self::InvalidConfigShape { .. } | Self::InvalidGitDirFile { .. } => None,
        }
    }
}

impl From<io::Error> for AgentHookError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for AgentHookError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serde(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn registry_contains_expected_agents() {
        let ids = registry()
            .iter()
            .map(|agent| agent.kind.id())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["claude-code", "codex"]);
        assert_eq!(registry()[0].executable, "claude");
        assert_eq!(registry()[1].executable, "codex");
    }

    #[test]
    fn claude_hook_command_quotes_windows_helper_path_and_keeps_explicit_state() {
        let helper = Path::new(r"C:\Program Files\Hitch Tools\hitch-hook.exe");

        let entry = claude_hook_entry(helper, "notification", AgentState::NeedsApproval);
        let command = entry["hooks"][0]["command"].as_str().unwrap();

        // Claude Code runs hooks via Git Bash (POSIX sh). The path is single-quoted
        // so backslashes stay literal AND `$`/backtick in a path can't expand;
        // double quotes (the old behavior) would have permitted that expansion.
        assert_eq!(
            command,
            r"'C:\Program Files\Hitch Tools\hitch-hook.exe' --agent claude-code --event notification --state needs-approval"
        );
    }

    #[test]
    fn codex_windows_hook_command_is_a_powershell_invocation() {
        // Codex evaluates hook commands with PowerShell on Windows, where a
        // double-quoted path is a string expression rather than an invocation.
        // The command must use the call operator and a single-quoted path or the
        // helper never spawns (Codex shows "hook: <event> Failed").
        let helper = Path::new(r"C:\Program Files\Hitch Tools\hitch-hook.exe");

        let entry = codex_hook_entry(helper, "user-prompt-submit", AgentState::Running);
        let command = entry["hooks"][0]["command"].as_str().unwrap();

        assert_eq!(
            command,
            r"& 'C:\Program Files\Hitch Tools\hitch-hook.exe' --agent codex --event user-prompt-submit --state running"
        );
    }

    #[test]
    fn codex_windows_hook_command_escapes_single_quotes_in_helper_path() {
        let helper = Path::new(r"C:\Users\o'brien\hitch\hitch-hook.exe");

        let entry = codex_hook_entry(helper, "stop", AgentState::Waiting);
        let command = entry["hooks"][0]["command"].as_str().unwrap();

        assert_eq!(
            command,
            r"& 'C:\Users\o''brien\hitch\hitch-hook.exe' --agent codex --event stop --state waiting"
        );
    }

    #[test]
    fn windows_command_quotes_spaceless_path_so_bash_keeps_backslashes() {
        // Regression: Claude Code runs hooks via Git Bash, which strips unquoted
        // backslashes. A spaceless `target\debug` path must still be quoted, or it
        // mangles to `C:Code...hitch-hook.exe` -> "command not found". Single
        // quotes keep every backslash literal (and block `$`/backtick expansion).
        let helper =
            Path::new(r"C:\Code\worktrees\hitch\round-thrush\hitch\target\debug\hitch-hook.exe");

        let entry = claude_hook_entry(helper, "notification", AgentState::NeedsApproval);
        let command = entry["hooks"][0]["command"].as_str().unwrap();

        assert!(
            command.starts_with(
                r"'C:\Code\worktrees\hitch\round-thrush\hitch\target\debug\hitch-hook.exe'"
            ),
            "spaceless Windows helper path must be single-quoted for Git Bash: {command}"
        );
    }

    #[test]
    fn windows_claude_hook_command_does_not_expand_dollar_or_backtick_in_path() {
        // A helper path containing `$` or a backtick must not be subject to
        // variable/command substitution under Git Bash. Double quotes (the old
        // behavior) would expand `$env` and run the backtick as a command; single
        // quotes keep the path verbatim.
        let helper = Path::new(r"C:\Users\$env`whoami`\hitch-hook.exe");

        let entry = claude_hook_entry(helper, "notification", AgentState::NeedsApproval);
        let command = entry["hooks"][0]["command"].as_str().unwrap();

        assert!(
            command.starts_with(r"'C:\Users\$env`whoami`\hitch-hook.exe'"),
            "path with $ / backtick must be single-quoted verbatim: {command}"
        );
    }

    #[test]
    fn windows_command_argument_quotes_backslash_paths_but_not_bare_identifiers() {
        // Backslash paths must be single-quoted so Git Bash keeps the separators
        // and performs no `$`/backtick expansion...
        assert_eq!(
            platform_command_arg(r"C:\a\b.exe", CommandArgStyle::Windows),
            r"'C:\a\b.exe'"
        );
        // ...while safe identifiers (event names) stay bare.
        assert_eq!(
            platform_command_arg("user-prompt-submit", CommandArgStyle::Windows),
            "user-prompt-submit"
        );
    }

    #[test]
    fn posix_command_arguments_keep_single_quote_escaping() {
        assert_eq!(
            platform_command_arg("/opt/Hitch Tools/hitch-hook", CommandArgStyle::Posix),
            "'/opt/Hitch Tools/hitch-hook'"
        );
        assert_eq!(
            platform_command_arg("can't-stop", CommandArgStyle::Posix),
            "'can'\\''t-stop'"
        );
    }

    #[test]
    fn installs_hooks_without_clobbering_user_config() {
        let worktree = temp_dir("preserve");
        fs::create_dir_all(worktree.join(".claude")).unwrap();
        fs::write(
            worktree.join(".claude/settings.local.json"),
            r#"{
  "permissions": {"allow": ["Bash(git status)"]},
  "hooks": {
    "Notification": [{"matcher":"user", "hooks": []}],
    "SessionStart": [{"matcher":"startup", "hooks": [{"type":"command","command":"/opt/hitch/hitch-hook --agent claude-code --event session-start --state running"}]}],
    "SessionEnd": [{"matcher":"other", "hooks": [{"type":"command","command":"/opt/hitch/hitch-hook --agent claude-code --event session-end --state none"}]}]
  }
}"#,
        )
        .unwrap();
        fs::create_dir_all(worktree.join(".codex")).unwrap();
        fs::write(
            worktree.join(".codex/hooks.json"),
            r#"{"hooks":{"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"/opt/hitch/hitch-hook --agent codex --event session-start --state running"}]}],"SessionEnd":[{"matcher":"","hooks":[{"type":"command","command":"/opt/hitch/hitch-hook --agent codex --event session-end"}]}]}}"#,
        )
        .unwrap();

        let summary =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(!summary.local_exclude_updated);
        assert_eq!(summary.installed_configs.len(), 2);

        let config: Value = serde_json::from_str(
            &fs::read_to_string(worktree.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(config["permissions"]["allow"][0], "Bash(git status)");
        let notification_hooks = config["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(
            notification_hooks.len(),
            3,
            "keeps the user's empty-hooks entry and appends BOTH Hitch matcher groups"
        );
        // The two Hitch-owned Notification groups are distinct: permission_prompt ->
        // needs-approval and idle_prompt -> waiting both survive prune+merge.
        assert!(notification_hooks.iter().any(|value| {
            value["matcher"] == "permission_prompt"
                && value.to_string().contains("--agent claude-code")
                && value.to_string().contains("--state needs-approval")
        }));
        assert!(notification_hooks.iter().any(|value| {
            value["matcher"] == "idle_prompt"
                && value.to_string().contains("--agent claude-code")
                && value.to_string().contains("--state waiting")
        }));
        // The user's empty Notification group is untouched.
        assert!(notification_hooks
            .iter()
            .any(|value| value["matcher"] == "user"));

        // SessionStart is now an identity announce: the user's stale hitch
        // session-start entry is pruned and replaced with `--announce` (no state).
        let session_start_hooks = config["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start_hooks.len(), 1);
        assert!(session_start_hooks.iter().any(|value| {
            value.to_string().contains("--agent claude-code")
                && value.to_string().contains("session-start")
                && value.to_string().contains("--announce")
                && !value.to_string().contains("--state")
        }));

        // PermissionDenied -> running heals an abandoned permission prompt.
        let permission_denied = config["hooks"]["PermissionDenied"].as_array().unwrap();
        assert!(permission_denied.iter().any(|value| {
            value.to_string().contains("--agent claude-code")
                && value.to_string().contains("permission-denied")
                && value.to_string().contains("--state running")
        }));

        // StopFailure is per-matcher with explicit, human-readable --detail.
        let stop_failure = config["hooks"]["StopFailure"].as_array().unwrap();
        assert_eq!(stop_failure.len(), 3);
        assert!(stop_failure.iter().any(|value| {
            value["matcher"] == "rate_limit"
                && value.to_string().contains("--state error")
                && value.to_string().contains("--detail")
                && value.to_string().contains("rate limited")
        }));
        assert!(stop_failure.iter().any(|value| {
            value["matcher"] == "billing_error" && value.to_string().contains("billing issue")
        }));
        assert!(stop_failure.iter().any(|value| {
            value["matcher"] == "server_error" && value.to_string().contains("server error")
        }));

        let session_end_hooks = config["hooks"]["SessionEnd"].as_array().unwrap();
        assert!(session_end_hooks.iter().any(|value| {
            value.to_string().contains("--agent claude-code")
                && value.to_string().contains("session-end")
                && !value.to_string().contains("--state")
        }));

        let codex: Value =
            serde_json::from_str(&fs::read_to_string(worktree.join(".codex/hooks.json")).unwrap())
                .unwrap();
        let prompt_hooks = codex["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert!(prompt_hooks.iter().any(|value| {
            value.to_string().contains("--agent codex")
                && value.to_string().contains("--event")
                && value.to_string().contains("user-prompt-submit")
                && value.to_string().contains("--state running")
        }));
        // Codex SessionStart is now the identity announce (replacing the stale
        // hitch session-start entry), NOT null.
        let codex_session_start = codex["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(codex_session_start.len(), 1);
        assert!(codex_session_start.iter().any(|value| {
            value.to_string().contains("--agent codex")
                && value.to_string().contains("session-start")
                && value.to_string().contains("--announce")
                && !value.to_string().contains("--state")
        }));
        // The dead Codex SessionEnd is pruned on re-install and not re-added: the
        // event does not exist in Codex.
        assert!(
            codex["hooks"]["SessionEnd"].is_null(),
            "Codex must not carry a SessionEnd entry"
        );

        assert!(!worktree.join(".gitignore").exists());

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn announce_command_carries_announce_flag_and_no_state() {
        // The exact string the helper agent must map (`--announce` -> AnnounceAgent).
        let claude = claude_announce_hook_entry(Path::new("/opt/hitch/hitch-hook"), "session-start");
        let command = claude["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(
            command,
            "'/opt/hitch/hitch-hook' --agent claude-code --event 'session-start' --announce"
        );
        assert!(!command.contains("--state"));

        let codex = codex_announce_hook_entry(Path::new("/opt/hitch/hitch-hook"), "session-start");
        let command = codex["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(
            command,
            "'/opt/hitch/hitch-hook' --agent codex --event 'session-start' --announce"
        );
        assert!(!command.contains("--state"));
    }

    #[test]
    fn stop_failure_entry_carries_explicit_state_and_detail() {
        let entry =
            claude_stop_failure_hook_entry(Path::new("/opt/hitch/hitch-hook"), "rate_limit", "rate limited");
        assert_eq!(entry["matcher"], "rate_limit");
        let command = entry["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(
            command,
            "'/opt/hitch/hitch-hook' --agent claude-code --event 'stop-failure' --state error --detail 'rate limited'"
        );
    }

    #[test]
    fn claude_notification_keeps_two_distinct_groups_and_is_idempotent() {
        let worktree = temp_dir("two-notifications");
        init_git_repo(&worktree);
        install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        let read_notification = |worktree: &Path| -> Vec<Value> {
            let config: Value = serde_json::from_str(
                &fs::read_to_string(worktree.join(".claude/settings.local.json")).unwrap(),
            )
            .unwrap();
            config["hooks"]["Notification"].as_array().unwrap().clone()
        };

        let first = read_notification(&worktree);
        assert_eq!(first.len(), 2, "two distinct Hitch Notification groups");
        assert!(first
            .iter()
            .any(|v| v["matcher"] == "permission_prompt"
                && v.to_string().contains("--state needs-approval")));
        assert!(first
            .iter()
            .any(|v| v["matcher"] == "idle_prompt" && v.to_string().contains("--state waiting")));

        // Re-install must not duplicate or collapse the two matcher groups.
        install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();
        let second = read_notification(&worktree);
        assert_eq!(second, first, "Notification groups stable across re-install");

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn reinstall_migrates_old_codex_session_end_away() {
        let worktree = temp_dir("codex-session-end-migrate");
        init_git_repo(&worktree);
        // Simulate an old install that wrote a Codex SessionEnd entry.
        fs::create_dir_all(worktree.join(".codex")).unwrap();
        fs::write(
            worktree.join(".codex/hooks.json"),
            r#"{"hooks":{"SessionEnd":[{"matcher":"","hooks":[{"type":"command","command":"/opt/hitch/hitch-hook --agent codex --event session-end"}]}]}}"#,
        )
        .unwrap();

        install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        let codex: Value =
            serde_json::from_str(&fs::read_to_string(worktree.join(".codex/hooks.json")).unwrap())
                .unwrap();
        assert!(
            codex["hooks"]["SessionEnd"].is_null(),
            "old Codex SessionEnd must be pruned on re-install"
        );
        // And the announce is now present.
        assert!(codex["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.to_string().contains("--announce")));

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn install_is_idempotent() {
        let worktree = temp_dir("idempotent");
        init_git_repo(&worktree);
        let first =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();
        assert!(first.local_exclude_updated);
        let first_claude =
            fs::read_to_string(worktree.join(".claude/settings.local.json")).unwrap();
        let first_codex = fs::read_to_string(worktree.join(".codex/hooks.json")).unwrap();
        let first_exclude = fs::read_to_string(worktree.join(".git/info/exclude")).unwrap();

        let second =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(!second.local_exclude_updated);
        assert_eq!(
            fs::read_to_string(worktree.join(".claude/settings.local.json")).unwrap(),
            first_claude
        );
        assert_eq!(
            fs::read_to_string(worktree.join(".codex/hooks.json")).unwrap(),
            first_codex
        );
        assert_eq!(
            fs::read_to_string(worktree.join(".git/info/exclude")).unwrap(),
            first_exclude
        );

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn install_replaces_stale_hitch_hook_paths() {
        let worktree = temp_dir("stale-helper");
        init_git_repo(&worktree);
        install_hooks(
            &worktree,
            &HookInstallOptions::new("/old/target/debug/hitch-hook"),
        )
        .unwrap();

        let claude_path = worktree.join(".claude/settings.local.json");
        let mut claude: Value =
            serde_json::from_str(&fs::read_to_string(&claude_path).unwrap()).unwrap();
        claude["hooks"]["Notification"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "matcher": "user-owned",
                "hooks": [{
                    "type": "command",
                    "command": "echo keep-me"
                }]
            }));
        fs::write(
            &claude_path,
            format!("{}\n", serde_json::to_string_pretty(&claude).unwrap()),
        )
        .unwrap();

        install_hooks(
            &worktree,
            &HookInstallOptions::new("/new/target/debug/hitch-hook"),
        )
        .unwrap();

        let claude = fs::read_to_string(&claude_path).unwrap();
        assert!(!claude.contains("/old/target/debug/hitch-hook"));
        // SessionStart, UserPromptSubmit, PermissionRequest, PermissionDenied,
        // Notification x2, PostToolUse, Stop, StopFailure x3, SessionEnd = 12.
        assert_eq!(claude.matches("/new/target/debug/hitch-hook").count(), 12);
        assert!(claude.contains("echo keep-me"));

        let codex = fs::read_to_string(worktree.join(".codex/hooks.json")).unwrap();
        assert!(!codex.contains("/old/target/debug/hitch-hook"));
        // SessionStart, UserPromptSubmit, PermissionRequest, PostToolUse, Stop = 5
        // (no SessionEnd).
        assert_eq!(codex.matches("/new/target/debug/hitch-hook").count(), 5);

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn install_updates_local_exclude_in_git_directory() {
        let worktree = temp_dir("exclude");
        init_git_repo(&worktree);

        let summary =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(summary.local_exclude_updated);
        assert!(!worktree.join(".gitignore").exists());
        let exclude = fs::read_to_string(worktree.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains(".claude/settings.local.json"));
        assert!(exclude.contains(".codex/hooks.json"));
        assert_git_status_clean(&worktree);

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn install_resolves_relative_linked_worktree_gitdir() {
        let root = temp_dir("linked-relative-root");
        let worktree = root.join("linked");
        let git_dir = root.join("main/.git/worktrees/linked");
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(git_dir.join("info")).unwrap();
        fs::write(
            worktree.join(".git"),
            "gitdir: ../main/.git/worktrees/linked\n",
        )
        .unwrap();

        let summary =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(summary.local_exclude_updated);
        let exclude = fs::read_to_string(git_dir.join("info/exclude")).unwrap();
        assert!(exclude.contains(".claude/settings.local.json"));
        assert!(exclude.contains(".codex/hooks.json"));
        assert!(!worktree.join(".gitignore").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_resolves_absolute_linked_worktree_gitdir() {
        let root = temp_dir("linked-absolute-root");
        let worktree = root.join("linked");
        let git_dir = root.join("main/.git/worktrees/linked");
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(git_dir.join("info")).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();

        install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        let exclude = fs::read_to_string(git_dir.join("info/exclude")).unwrap();
        assert!(exclude.contains(".claude/settings.local.json"));
        assert!(exclude.contains(".codex/hooks.json"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_excludes_are_honored_in_real_linked_worktree() {
        // The real bug: Git reads `info/exclude` from the common git dir, so an
        // exclude written into a linked worktree's per-worktree gitdir is silently
        // ignored and the worktree shows the hook configs as untracked. Drive real
        // `git` end-to-end to prove the installed excludes actually hide them.
        let root = temp_dir("real-linked");
        let main = root.join("main");
        fs::create_dir_all(&main).unwrap();
        // `init_git_repo` already configures a user and lands an initial commit,
        // so the repo is ready for `git worktree add`.
        init_git_repo(&main);

        let feature = root.join("feature");
        git(
            &main,
            ["worktree", "add", "--quiet", feature.to_str().unwrap()],
        );

        let summary =
            install_hooks(&feature, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();
        assert!(summary.local_exclude_updated);

        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&feature)
            .output()
            .expect("git status");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            !stdout.contains(".claude/settings.local.json"),
            "claude config should be excluded, got: {stdout}"
        );
        assert!(
            !stdout.contains(".codex/hooks.json"),
            "codex config should be excluded, got: {stdout}"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_removes_legacy_hitch_owned_gitignore() {
        let worktree = temp_dir("legacy-gitignore");
        init_git_repo(&worktree);
        // Simulate a pre-exclude install: Hitch's two lines are the whole file.
        fs::write(
            worktree.join(".gitignore"),
            ".claude/settings.local.json\n.codex/config.local.json\n",
        )
        .unwrap();

        let summary =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(summary.legacy_gitignore_removed);
        assert!(!worktree.join(".gitignore").exists());
        assert_git_status_clean(&worktree);

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn install_strips_only_legacy_lines_from_user_gitignore() {
        let worktree = temp_dir("legacy-gitignore-mixed");
        init_git_repo(&worktree);
        fs::write(
            worktree.join(".gitignore"),
            "node_modules/\n.claude/settings.local.json\ntarget/\n.codex/config.local.json\n",
        )
        .unwrap();

        let summary =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(summary.legacy_gitignore_removed);
        let gitignore = fs::read_to_string(worktree.join(".gitignore")).unwrap();
        assert_eq!(gitignore, "node_modules/\ntarget/\n");
        assert!(!gitignore.contains(".claude/settings.local.json"));
        assert!(!gitignore.contains(".codex/config.local.json"));

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn install_preserves_tracked_legacy_gitignore() {
        let worktree = temp_dir("tracked-legacy-gitignore");
        init_git_repo(&worktree);
        // A user-committed `.gitignore` that happens to hold only the legacy
        // lines must not be deleted: that would dirty the repo with a tracked
        // deletion and discard rules the user owns.
        fs::write(
            worktree.join(".gitignore"),
            ".claude/settings.local.json\n.codex/config.local.json\n",
        )
        .unwrap();
        git(&worktree, ["add", ".gitignore"]);
        git(&worktree, ["commit", "-m", "track gitignore"]);

        let summary =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(!summary.legacy_gitignore_removed);
        assert!(worktree.join(".gitignore").exists());
        let gitignore = fs::read_to_string(worktree.join(".gitignore")).unwrap();
        assert!(gitignore.contains(".claude/settings.local.json"));
        assert!(gitignore.contains(".codex/config.local.json"));
        // The tracked file is untouched and the agent configs are locally
        // excluded, so the worktree stays clean.
        assert_git_status_clean(&worktree);

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn migrated_worktree_keeps_legacy_codex_config_excluded() {
        let worktree = temp_dir("legacy-codex-config");
        init_git_repo(&worktree);
        // Pre-exclude state: legacy `.gitignore` plus the orphaned Codex config
        // that the old install left behind on disk.
        fs::write(
            worktree.join(".gitignore"),
            ".claude/settings.local.json\n.codex/config.local.json\n",
        )
        .unwrap();
        fs::create_dir_all(worktree.join(".codex")).unwrap();
        fs::write(worktree.join(".codex/config.local.json"), "{}\n").unwrap();

        let summary =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(summary.legacy_gitignore_removed);
        assert!(!worktree.join(".gitignore").exists());
        let exclude = fs::read_to_string(worktree.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains(".codex/config.local.json"));
        // Removing the legacy `.gitignore` line must not expose the orphaned
        // config: the local exclude keeps the repo clean.
        assert_git_status_clean(&worktree);

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn rejects_non_object_config_instead_of_overwriting_it() {
        let worktree = temp_dir("bad-shape");
        fs::create_dir_all(worktree.join(".claude")).unwrap();
        fs::write(worktree.join(".claude/settings.local.json"), "[]").unwrap();

        let err = install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook"))
            .unwrap_err();
        assert!(matches!(err, AgentHookError::InvalidConfigShape { .. }));

        fs::remove_dir_all(worktree).unwrap();
    }

    fn init_git_repo(path: &Path) {
        git(path, ["init", "--initial-branch=main"]);
        git(path, ["config", "user.name", "Hitch Test"]);
        git(path, ["config", "user.email", "hitch@example.test"]);
        fs::write(path.join("tracked.txt"), "initial\n").unwrap();
        git(path, ["add", "tracked.txt"]);
        git(path, ["commit", "-m", "initial"]);
    }

    fn assert_git_status_clean(path: &Path) {
        let output = std::process::Command::new("git")
            .current_dir(path)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "git status was dirty: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn git<const N: usize>(cwd: &Path, args: [&str; N]) {
        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hitch-agent-{name}-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
