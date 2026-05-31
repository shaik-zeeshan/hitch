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

/// Whether `.gitignore` is tracked by git in `worktree_path`. Returns `true` only
/// when git positively reports the file as tracked; a missing git binary, a
/// non-repository directory, or an untracked file all yield `false` so the
/// legacy cleanup still runs in the common (untracked) case.
fn gitignore_is_tracked(worktree_path: &Path, git_path: &Path) -> bool {
    Command::new(git_path)
        .current_dir(worktree_path)
        .args(["ls-files", "--error-unmatch", "--", ".gitignore"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn install_claude_hooks(
    worktree_path: &Path,
    helper_path: &Path,
    installed_configs: &mut Vec<InstalledHookConfig>,
) -> Result<(), AgentHookError> {
    let path = worktree_path.join(".claude/settings.local.json");
    let overlay = json!({
        "hooks": {
            "UserPromptSubmit": [claude_hook_entry(helper_path, "user-prompt-submit", AgentState::Running)],
            "PermissionRequest": [claude_hook_entry(helper_path, "permission-request", AgentState::NeedsApproval)],
            "Notification": [claude_hook_entry_with_matcher(helper_path, "permission_prompt", "notification", AgentState::NeedsApproval)],
            "Stop": [claude_hook_entry(helper_path, "stop", AgentState::Completed)],
            "StopFailure": [claude_hook_entry(helper_path, "stop-failure", AgentState::Error)]
        }
    });
    merge_json_file(
        &path,
        overlay,
        AgentKind::ClaudeCode,
        &["SessionStart", "SessionEnd"],
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
            "UserPromptSubmit": [codex_hook_entry(helper_path, "user-prompt-submit", AgentState::Running)],
            "PermissionRequest": [codex_hook_entry(helper_path, "permission-request", AgentState::NeedsApproval)],
            "Stop": [codex_hook_entry(helper_path, "stop", AgentState::Completed)]
        }
    });
    merge_json_file(&path, overlay, AgentKind::Codex, &["SessionStart"])?;
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

fn hook_command(
    helper_path: &Path,
    agent: AgentKind,
    event: &str,
    state: Option<AgentState>,
) -> String {
    let mut command = format!(
        "{} --agent {} --event {}",
        shell_quote(&helper_path.to_string_lossy()),
        agent.id(),
        shell_quote(event)
    );
    if let Some(state) = state {
        command.push_str(" --state ");
        command.push_str(state_arg(state));
    }
    command
}

fn state_arg(state: AgentState) -> &'static str {
    match state {
        AgentState::Running => "running",
        AgentState::NeedsApproval => "needs-approval",
        AgentState::Completed => "completed",
        AgentState::Error => "error",
    }
}

fn merge_json_file(
    path: &Path,
    overlay: Value,
    agent: AgentKind,
    obsolete_events: &[&str],
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

    merge_preserving_existing(&mut base, overlay);
    prune_obsolete_hitch_hooks(&mut base, agent, obsolete_events);
    let rendered = serde_json::to_string_pretty(&base)?;
    fs::write(path, format!("{rendered}\n"))?;
    Ok(())
}

fn prune_obsolete_hitch_hooks(base: &mut Value, agent: AgentKind, obsolete_events: &[&str]) {
    let Some(hooks) = base.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };

    for event in obsolete_events {
        let remove_event = {
            let Some(groups) = hooks.get_mut(*event).and_then(Value::as_array_mut) else {
                continue;
            };

            for group in groups.iter_mut() {
                let Some(commands) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                    continue;
                };
                commands.retain(|hook| !is_hitch_hook_command(hook, agent));
            }
            groups.retain(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .map_or(true, |commands| !commands.is_empty())
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
    let Some(git_dir) = resolve_git_dir(worktree_path)? else {
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
    "SessionEnd": [{"matcher":"other", "hooks": [{"type":"command","command":"/opt/hitch/hitch-hook --agent claude-code --event session-end --state completed"}]}]
  }
}"#,
        )
        .unwrap();
        fs::create_dir_all(worktree.join(".codex")).unwrap();
        fs::write(
            worktree.join(".codex/hooks.json"),
            r#"{"hooks":{"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"/opt/hitch/hitch-hook --agent codex --event session-start --state running"}]}]}}"#,
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
            2,
            "keeps user hook and appends Hitch hook"
        );
        assert!(notification_hooks.iter().any(|value| {
            value.to_string().contains("--agent claude-code")
                && value.to_string().contains("--state needs-approval")
        }));
        assert!(config["hooks"]["SessionStart"].is_null());
        assert!(config["hooks"]["SessionEnd"].is_null());

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
        assert!(codex["hooks"]["SessionStart"].is_null());

        assert!(!worktree.join(".gitignore").exists());

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
