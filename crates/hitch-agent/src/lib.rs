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
        local_config_path: ".codex/config.local.json",
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
}

impl HookInstallOptions {
    pub fn new(helper_path: impl Into<PathBuf>) -> Self {
        Self {
            helper_path: helper_path.into(),
        }
    }
}

/// Summary of a hook installation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInstallSummary {
    pub installed_configs: Vec<InstalledHookConfig>,
    pub gitignore_updated: bool,
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
    let mut gitignore_entries = Vec::new();

    install_claude_hooks(worktree_path, &helper_path, &mut installed_configs)?;
    gitignore_entries.push(".claude/settings.local.json");

    install_codex_hooks(worktree_path, &helper_path, &mut installed_configs)?;
    gitignore_entries.push(".codex/config.local.json");

    let gitignore_updated = ensure_gitignored(worktree_path, &gitignore_entries)?;

    Ok(HookInstallSummary {
        installed_configs,
        gitignore_updated,
    })
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
            "Notification": [claude_hook_entry(helper_path, "notification", AgentState::NeedsApproval)],
            "Stop": [claude_hook_entry(helper_path, "stop", AgentState::Completed)]
        }
    });
    merge_json_file(&path, overlay)?;
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
    let path = worktree_path.join(".codex/config.local.json");
    let overlay = json!({
        "notify": [hook_command(helper_path, AgentKind::Codex, "notify", None)]
    });
    merge_json_file(&path, overlay)?;
    installed_configs.push(InstalledHookConfig {
        agent: AgentKind::Codex,
        path,
    });
    Ok(())
}

fn claude_hook_entry(helper_path: &Path, event: &str, state: AgentState) -> Value {
    json!({
        "matcher": "",
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

fn merge_json_file(path: &Path, overlay: Value) -> Result<(), AgentHookError> {
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
    let rendered = serde_json::to_string_pretty(&base)?;
    fs::write(path, format!("{rendered}\n"))?;
    Ok(())
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

fn ensure_gitignored(worktree_path: &Path, entries: &[&str]) -> Result<bool, AgentHookError> {
    let path = worktree_path.join(".gitignore");
    let existing = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };

    let existing_lines = existing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
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
        }
    }
}

impl std::error::Error for AgentHookError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Serde(err) => Some(err),
            Self::InvalidJson { source, .. } => Some(source),
            Self::InvalidConfigShape { .. } => None,
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
  "hooks": {"Notification": [{"matcher":"user", "hooks": []}]}
}"#,
        )
        .unwrap();

        let summary =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(summary.gitignore_updated);
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

        let gitignore = fs::read_to_string(worktree.join(".gitignore")).unwrap();
        assert!(gitignore.contains(".claude/settings.local.json"));
        assert!(gitignore.contains(".codex/config.local.json"));

        fs::remove_dir_all(worktree).unwrap();
    }

    #[test]
    fn install_is_idempotent() {
        let worktree = temp_dir("idempotent");
        install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();
        let first_claude =
            fs::read_to_string(worktree.join(".claude/settings.local.json")).unwrap();
        let first_codex = fs::read_to_string(worktree.join(".codex/config.local.json")).unwrap();
        let first_gitignore = fs::read_to_string(worktree.join(".gitignore")).unwrap();

        let second =
            install_hooks(&worktree, &HookInstallOptions::new("/opt/hitch/hitch-hook")).unwrap();

        assert!(!second.gitignore_updated);
        assert_eq!(
            fs::read_to_string(worktree.join(".claude/settings.local.json")).unwrap(),
            first_claude
        );
        assert_eq!(
            fs::read_to_string(worktree.join(".codex/config.local.json")).unwrap(),
            first_codex
        );
        assert_eq!(
            fs::read_to_string(worktree.join(".gitignore")).unwrap(),
            first_gitignore
        );

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
