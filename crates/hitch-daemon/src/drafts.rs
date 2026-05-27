use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hitch_proto::{
    CommitDraft, DraftGenerationSettings, DraftProvider, ErrorCode, ProtocolError, PullRequestDraft,
};
use serde_json::Value;

const DEFAULT_TIMEOUT_SECS: u64 = 90;
const MAX_DIFF_CHARS: usize = 48_000;
const MAX_STDERR_CHARS: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DraftProviderKind {
    Stub,
    Claude,
    Codex,
}

impl DraftProviderKind {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stub" | "deterministic" => Ok(Self::Stub),
            "claude" | "claude-code" | "claude_code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => Err(format!(
                "unknown draft provider `{other}` (expected stub, claude, or codex)"
            )),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Stub => "stub",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DraftProviderConfig {
    pub(crate) kind: DraftProviderKind,
    pub(crate) claude: PathBuf,
    pub(crate) codex: PathBuf,
    pub(crate) timeout: Duration,
    pub(crate) model: Option<String>,
}

impl DraftProviderConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let kind = match std::env::var("HITCH_DRAFT_PROVIDER") {
            Ok(value) if !value.trim().is_empty() => DraftProviderKind::parse(&value)?,
            _ => DraftProviderKind::Stub,
        };
        let claude = std::env::var_os("HITCH_CLAUDE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("claude"));
        let codex = std::env::var_os("HITCH_CODEX_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("codex"));
        let model = std::env::var("HITCH_DRAFT_MODEL")
            .ok()
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        let timeout = match std::env::var("HITCH_DRAFT_TIMEOUT_SECS") {
            Ok(value) if !value.trim().is_empty() => {
                let secs = value
                    .parse::<u64>()
                    .map_err(|_| "HITCH_DRAFT_TIMEOUT_SECS must be an integer".to_string())?;
                Duration::from_secs(secs.max(1))
            }
            _ => Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        };
        Ok(Self {
            kind,
            claude,
            codex,
            timeout,
            model,
        })
    }

    pub(crate) fn set_kind(&mut self, value: &str) -> Result<(), String> {
        self.kind = DraftProviderKind::parse(value)?;
        Ok(())
    }

    pub(crate) fn set_timeout_secs(&mut self, value: &str) -> Result<(), String> {
        let secs = value
            .parse::<u64>()
            .map_err(|_| "--draft-timeout-secs requires an integer".to_string())?;
        self.timeout = Duration::from_secs(secs.max(1));
        Ok(())
    }

    pub(crate) fn with_settings(mut self, settings: Option<DraftGenerationSettings>) -> Self {
        if let Some(settings) = settings {
            self.kind = match settings.provider {
                DraftProvider::Stub => DraftProviderKind::Stub,
                DraftProvider::Claude => DraftProviderKind::Claude,
                DraftProvider::Codex => DraftProviderKind::Codex,
            };
            self.model = settings
                .model
                .map(|model| model.trim().to_string())
                .filter(|model| !model.is_empty());
        }
        self
    }
}

pub(crate) struct CommitDraftInput {
    pub(crate) worktree_path: PathBuf,
    pub(crate) staged_paths: Vec<PathBuf>,
    pub(crate) staged_patch: String,
}

pub(crate) struct PullRequestDraftInput {
    pub(crate) worktree_path: PathBuf,
    pub(crate) branch: String,
    pub(crate) base: String,
    pub(crate) commits: Vec<String>,
    pub(crate) changed_paths: Vec<PathBuf>,
    pub(crate) diff: String,
}

pub(crate) fn list_models(
    config: &DraftProviderConfig,
    provider: DraftProvider,
) -> Result<Vec<String>, ProtocolError> {
    match provider {
        DraftProvider::Stub => Ok(vec!["stub".into()]),
        DraftProvider::Claude => Ok(vec![
            "default".into(),
            "best".into(),
            "sonnet".into(),
            "opus".into(),
            "haiku".into(),
            "sonnet[1m]".into(),
            "opus[1m]".into(),
            "opusplan".into(),
            "claude-opus-4-6".into(),
            "claude-sonnet-4-6".into(),
            "claude-haiku-4-5-20251001".into(),
        ]),
        DraftProvider::Codex => {
            let mut codex_config = config.clone();
            codex_config.kind = DraftProviderKind::Codex;
            // Model discovery is best-effort UI chrome; never let it block the
            // daemon as long as generation can.
            codex_config.timeout = codex_config.timeout.min(Duration::from_secs(5));
            let mut command = Command::new(&codex_config.codex);
            command.arg("debug").arg("models");
            let output = run_provider_command(&mut command, Path::new("."), &codex_config)?;
            let models = parse_codex_models(&output);
            if models.is_empty() {
                Ok(vec![
                    "gpt-5-codex".into(),
                    "gpt-5".into(),
                    "gpt-5-mini".into(),
                ])
            } else {
                Ok(models)
            }
        }
    }
}

pub(crate) fn generate_commit_draft(
    config: &DraftProviderConfig,
    input: CommitDraftInput,
) -> Result<CommitDraft, ProtocolError> {
    match config.kind {
        DraftProviderKind::Stub => Ok(stub_commit_draft(&input.staged_paths, &input.staged_patch)),
        DraftProviderKind::Claude | DraftProviderKind::Codex => {
            let prompt = commit_prompt(&input.staged_paths, &input.staged_patch);
            let output = run_headless_provider(config, &input.worktree_path, prompt)?;
            let mut draft = parse_commit_draft_output(&output)?;
            if draft.body.trim().is_empty() {
                draft.body = commit_body_from_context(&input.staged_paths, &input.staged_patch);
            }
            Ok(draft)
        }
    }
}

pub(crate) fn generate_pull_request_draft(
    config: &DraftProviderConfig,
    input: PullRequestDraftInput,
) -> Result<PullRequestDraft, ProtocolError> {
    match config.kind {
        DraftProviderKind::Stub => Ok(stub_pull_request_draft(
            &input.branch,
            &input.commits,
            &input.changed_paths,
            &input.diff,
        )),
        DraftProviderKind::Claude | DraftProviderKind::Codex => {
            let prompt = pull_request_prompt(
                &input.branch,
                &input.base,
                &input.commits,
                &input.changed_paths,
                &input.diff,
            );
            let output = run_headless_provider(config, &input.worktree_path, prompt)?;
            parse_pull_request_draft_output(&output)
        }
    }
}

fn stub_commit_draft(staged_paths: &[PathBuf], staged_patch: &str) -> CommitDraft {
    let primary = primary_area(staged_paths);
    let subject = format!("chore: update {primary}");
    CommitDraft {
        subject,
        body: commit_body_from_context(staged_paths, staged_patch),
    }
}

fn commit_body_from_context(staged_paths: &[PathBuf], staged_patch: &str) -> String {
    let mut body_lines = staged_paths
        .iter()
        .map(|path| format!("- Update {}", path.display()))
        .collect::<Vec<_>>();
    let changed_lines = changed_line_count(staged_patch);
    if changed_lines > 0 {
        body_lines.push(format!("- Review {changed_lines} staged changed lines"));
    }
    body_lines.join("\n")
}

fn stub_pull_request_draft(
    branch: &str,
    commits: &[String],
    changed_paths: &[PathBuf],
    diff: &str,
) -> PullRequestDraft {
    let title = title_from_branch(branch, commits.first().map(String::as_str));
    let mut body = String::from("## Summary\n\n");
    if changed_paths.is_empty() {
        body.push_str("- No committed file changes detected.\n");
    } else {
        for path in changed_paths {
            body.push_str(&format!("- Update {}\n", path.display()));
        }
    }
    if !commits.is_empty() {
        body.push_str("\n## Commits\n\n");
        for commit in commits {
            body.push_str(&format!("- {commit}\n"));
        }
    }
    let changed_lines = changed_line_count(diff);
    if changed_lines > 0 {
        body.push_str(&format!("\nChanged lines: {changed_lines}\n"));
    }
    body.push_str("\n## Testing\n\n- [ ] Not run");
    PullRequestDraft { title, body }
}

fn commit_prompt(staged_paths: &[PathBuf], staged_patch: &str) -> String {
    let paths = path_list(staged_paths);
    let patch = truncate_context(staged_patch, MAX_DIFF_CHARS);
    format!(
        "You are Hitch's headless draft generator. Draft a concise git commit message from ONLY the staged diff below.\n\nReturn ONLY valid JSON with this exact shape, no markdown fences and no commentary:\n{{\"subject\":\"type: imperative summary under 72 chars\",\"body\":\"markdown bullet body with at least one bullet\"}}\n\nRules:\n- Do not mention unstaged or untracked files.\n- Use an imperative, review-ready subject.\n- Body must be non-empty and include at least one bullet summarizing changed files, rationale, or testing notes supported by the diff.\n- Put rationale, notable files, and testing notes in body only if supported by the diff.\n\nStaged files:\n{paths}\n\nStaged diff:\n```diff\n{patch}\n```"
    )
}

fn pull_request_prompt(
    branch: &str,
    base: &str,
    commits: &[String],
    changed_paths: &[PathBuf],
    diff: &str,
) -> String {
    let commits = if commits.is_empty() {
        "- No commits found".to_string()
    } else {
        commits
            .iter()
            .map(|commit| format!("- {commit}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let paths = path_list(changed_paths);
    let diff = truncate_context(diff, MAX_DIFF_CHARS);
    format!(
        "You are Hitch's headless draft generator. Draft a GitHub pull request title and body from the branch context below.\n\nReturn ONLY valid JSON with this exact shape, no markdown fences and no commentary:\n{{\"title\":\"concise PR title\",\"body\":\"markdown PR description\"}}\n\nRules:\n- The body must include a ## Summary section and a ## Testing section.\n- If testing cannot be inferred, include '- [ ] Not run' under ## Testing.\n- Use only the commits, changed files, and diff below.\n\nCurrent branch: {branch}\nBase branch: {base}\n\nCommits on branch:\n{commits}\n\nChanged files:\n{paths}\n\nDiff from base:\n```diff\n{diff}\n```"
    )
}

fn run_headless_provider(
    config: &DraftProviderConfig,
    cwd: &Path,
    prompt: String,
) -> Result<String, ProtocolError> {
    let mut command = match config.kind {
        DraftProviderKind::Stub => unreachable!("stub provider does not spawn a CLI"),
        DraftProviderKind::Claude => {
            let mut command = Command::new(&config.claude);
            if let Some(model) = config.model.as_deref() {
                command.arg("--model").arg(model);
            }
            command.arg("-p").arg(prompt);
            command
        }
        DraftProviderKind::Codex => {
            let mut command = Command::new(&config.codex);
            command.arg("exec");
            if let Some(model) = config.model.as_deref() {
                command.arg("--model").arg(model);
            }
            command.arg(prompt);
            command
        }
    };
    run_provider_command(&mut command, cwd, config)
}

fn run_provider_command(
    command: &mut Command,
    cwd: &Path,
    config: &DraftProviderConfig,
) -> Result<String, ProtocolError> {
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|err| {
        provider_error(format!(
            "failed to start {} draft provider: {err}",
            config.kind.label()
        ))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        provider_error(format!(
            "{} draft provider stdout was not captured",
            config.kind.label()
        ))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        provider_error(format!(
            "{} draft provider stderr was not captured",
            config.kind.label()
        ))
    })?;
    let stdout_reader = read_pipe(stdout);
    let stderr_reader = read_pipe(stderr);

    let deadline = Instant::now() + config.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(provider_error(format!(
                    "{} draft provider timed out after {}s",
                    config.kind.label(),
                    config.timeout.as_secs()
                ))
                .retryable(true));
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(err) => return Err(provider_error(format!("draft provider failed: {err}"))),
        }
    };

    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    if !status.success() {
        return Err(nonzero_provider_error(config.kind.label(), status, &stderr).retryable(true));
    }
    Ok(stdout)
}

fn read_pipe<T>(mut pipe: T) -> thread::JoinHandle<io::Result<String>>
where
    T: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = String::new();
        pipe.read_to_string(&mut output)?;
        Ok(output)
    })
}

fn join_reader(handle: thread::JoinHandle<io::Result<String>>) -> Result<String, ProtocolError> {
    handle
        .join()
        .map_err(|_| provider_error("draft provider output reader panicked"))?
        .map_err(|err| provider_error(format!("failed reading draft provider output: {err}")))
}

fn parse_codex_models(output: &str) -> Vec<String> {
    let Some(value) = parse_json_output(output) else {
        return Vec::new();
    };
    let Some(models) = value.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut slugs = models
        .iter()
        .filter(|model| model.get("visibility").and_then(Value::as_str) != Some("hidden"))
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    slugs.sort();
    slugs.dedup();
    slugs
}

fn parse_commit_draft_output(output: &str) -> Result<CommitDraft, ProtocolError> {
    let value = parse_json_output(output).ok_or_else(|| {
        provider_error("draft provider returned no JSON commit draft".to_string())
    })?;
    commit_draft_from_value(&value, 0).ok_or_else(|| {
        provider_error("draft provider JSON did not contain a subject string".to_string())
    })
}

fn parse_pull_request_draft_output(output: &str) -> Result<PullRequestDraft, ProtocolError> {
    let value = parse_json_output(output).ok_or_else(|| {
        provider_error("draft provider returned no JSON pull-request draft".to_string())
    })?;
    pull_request_draft_from_value(&value, 0).ok_or_else(|| {
        provider_error("draft provider JSON did not contain title/body strings".to_string())
    })
}

fn parse_json_output(output: &str) -> Option<Value> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end]).ok()
}

fn commit_draft_from_value(value: &Value, depth: usize) -> Option<CommitDraft> {
    if depth > 5 {
        return None;
    }
    if let Value::Object(object) = value {
        if let Some(subject) = object.get("subject").and_then(Value::as_str) {
            let subject = subject.trim().to_string();
            if !subject.is_empty() {
                let body = object
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                return Some(CommitDraft { subject, body });
            }
        }
    }
    nested_commit_draft(value, depth)
}

fn pull_request_draft_from_value(value: &Value, depth: usize) -> Option<PullRequestDraft> {
    if depth > 5 {
        return None;
    }
    if let Value::Object(object) = value {
        if let (Some(title), Some(body)) = (
            object.get("title").and_then(Value::as_str),
            object.get("body").and_then(Value::as_str),
        ) {
            let title = title.trim().to_string();
            if !title.is_empty() {
                return Some(PullRequestDraft {
                    title,
                    body: body.trim().to_string(),
                });
            }
        }
    }
    nested_pull_request_draft(value, depth)
}

fn nested_commit_draft(value: &Value, depth: usize) -> Option<CommitDraft> {
    match value {
        Value::String(text) => {
            parse_json_output(text).and_then(|value| commit_draft_from_value(&value, depth + 1))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| commit_draft_from_value(item, depth + 1)),
        Value::Object(object) => {
            for key in ["result", "message", "content", "text", "output"] {
                if let Some(draft) = object
                    .get(key)
                    .and_then(|value| commit_draft_from_value(value, depth + 1))
                {
                    return Some(draft);
                }
            }
            object.iter().find_map(|(key, value)| {
                if matches!(
                    key.as_str(),
                    "result" | "message" | "content" | "text" | "output"
                ) {
                    None
                } else {
                    commit_draft_from_value(value, depth + 1)
                }
            })
        }
        _ => None,
    }
}

fn nested_pull_request_draft(value: &Value, depth: usize) -> Option<PullRequestDraft> {
    match value {
        Value::String(text) => parse_json_output(text)
            .and_then(|value| pull_request_draft_from_value(&value, depth + 1)),
        Value::Array(items) => items
            .iter()
            .find_map(|item| pull_request_draft_from_value(item, depth + 1)),
        Value::Object(object) => {
            for key in ["result", "message", "content", "text", "output"] {
                if let Some(draft) = object
                    .get(key)
                    .and_then(|value| pull_request_draft_from_value(value, depth + 1))
                {
                    return Some(draft);
                }
            }
            object.iter().find_map(|(key, value)| {
                if matches!(
                    key.as_str(),
                    "result" | "message" | "content" | "text" | "output"
                ) {
                    None
                } else {
                    pull_request_draft_from_value(value, depth + 1)
                }
            })
        }
        _ => None,
    }
}

fn path_list(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "- None".to_string();
    }
    paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_context(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut output = input.chars().take(max_chars).collect::<String>();
    output.push_str("\n...[truncated by Hitch]...\n");
    output
}

fn changed_line_count(diff: &str) -> usize {
    diff.lines()
        .filter(|line| {
            (line.starts_with('+') || line.starts_with('-'))
                && !line.starts_with("+++")
                && !line.starts_with("---")
        })
        .count()
}

fn primary_area(paths: &[PathBuf]) -> String {
    paths
        .first()
        .and_then(|path| path.components().next())
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "files".into())
}

fn title_from_branch(branch: &str, first_commit: Option<&str>) -> String {
    if let Some(summary) = first_commit.filter(|summary| !summary.trim().is_empty()) {
        return summary.to_string();
    }
    branch
        .rsplit('/')
        .next()
        .unwrap_or(branch)
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn provider_error(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::Unavailable, message.into())
}

fn nonzero_provider_error(label: &str, status: ExitStatus, stderr: &str) -> ProtocolError {
    let stderr = truncate_context(stderr.trim(), MAX_STDERR_CHARS);
    let detail = if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    };
    provider_error(format!(
        "{label} draft provider exited with status {status}{detail}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn stub_commit_draft_uses_staged_paths_only() {
        let draft = stub_commit_draft(
            &[PathBuf::from("src/main.rs")],
            "diff --git a/src/main.rs b/src/main.rs\n+new\n-old\n",
        );
        assert_eq!(draft.subject, "chore: update src");
        assert!(draft.body.contains("src/main.rs"));
        assert!(draft.body.contains("2 staged changed lines"));
    }

    #[test]
    fn parses_plain_and_wrapped_provider_json() {
        let direct = parse_commit_draft_output(
            r#"{"subject":"feat: add drafts","body":"- Adds provider support"}"#,
        )
        .unwrap();
        assert_eq!(direct.subject, "feat: add drafts");

        let wrapped = parse_pull_request_draft_output(
            r####"{"type":"result","result":"{\"title\":\"Add drafts\",\"body\":\"## Summary\\n\\n- Done\"}"}"####,
        )
        .unwrap();
        assert_eq!(wrapped.title, "Add drafts");

        let subject_only = parse_commit_draft_output(r#"{"subject":"fix: keep body"}"#).unwrap();
        assert_eq!(subject_only.subject, "fix: keep body");
        assert_eq!(subject_only.body, "");
    }

    #[test]
    fn provider_commit_draft_falls_back_to_context_body_when_body_is_empty() {
        let script = temp_file("empty-commit-body-provider", "sh");
        fs::write(
            &script,
            "#!/bin/sh\n[ \"$1\" = \"-p\" ] || exit 12\nprintf '%s\n' '{\"subject\":\"fix: preserve generated body\",\"body\":\"\"}'\n",
        )
        .unwrap();
        make_executable(&script);
        let cwd = temp_dir("empty-commit-body-provider-cwd");
        fs::create_dir_all(&cwd).unwrap();
        let config = DraftProviderConfig {
            kind: DraftProviderKind::Claude,
            claude: script.clone(),
            codex: PathBuf::from("codex"),
            timeout: Duration::from_secs(2),
            model: None,
        };
        let draft = generate_commit_draft(
            &config,
            CommitDraftInput {
                worktree_path: cwd.clone(),
                staged_paths: vec![PathBuf::from("src/lib.rs")],
                staged_patch: "diff --git a/src/lib.rs b/src/lib.rs\n+new\n-old\n".into(),
            },
        )
        .unwrap();
        assert_eq!(draft.subject, "fix: preserve generated body");
        assert!(draft.body.contains("src/lib.rs"));
        assert!(draft.body.contains("2 staged changed lines"));
        let _ = fs::remove_file(script);
        let _ = fs::remove_dir_all(cwd);
    }

    #[test]
    fn parses_codex_debug_models() {
        let models = parse_codex_models(
            r#"{"models":[{"slug":"gpt-5.5","visibility":"list"},{"slug":"gpt-5-codex","visibility":"list"},{"slug":"internal","visibility":"hidden"}]}"#,
        );
        assert_eq!(models, vec!["gpt-5-codex", "gpt-5.5"]);
    }

    #[test]
    fn claude_provider_runs_headless_print_mode() {
        let script = temp_file("claude-provider", "sh");
        fs::write(
            &script,
            "#!/bin/sh\n[ \"$1\" = \"--model\" ] || exit 12\n[ \"$2\" = \"sonnet\" ] || exit 13\n[ \"$3\" = \"-p\" ] || exit 14\nprintf '%s\n' '{\"subject\":\"feat: generated\",\"body\":\"Generated body\"}'\n",
        )
        .unwrap();
        make_executable(&script);
        let cwd = temp_dir("claude-provider-cwd");
        fs::create_dir_all(&cwd).unwrap();
        let config = DraftProviderConfig {
            kind: DraftProviderKind::Claude,
            claude: script.clone(),
            codex: PathBuf::from("codex"),
            timeout: Duration::from_secs(2),
            model: Some("sonnet".into()),
        };
        let draft = generate_commit_draft(
            &config,
            CommitDraftInput {
                worktree_path: cwd.clone(),
                staged_paths: vec![PathBuf::from("tracked.txt")],
                staged_patch: "+change".into(),
            },
        )
        .unwrap();
        assert_eq!(draft.subject, "feat: generated");
        assert_eq!(draft.body, "Generated body");
        let _ = fs::remove_file(script);
        let _ = fs::remove_dir_all(cwd);
    }

    #[test]
    fn codex_provider_runs_exec_mode() {
        let script = temp_file("codex-provider", "sh");
        fs::write(
            &script,
            "#!/bin/sh\n[ \"$1\" = \"exec\" ] || exit 13\n[ \"$2\" = \"--model\" ] || exit 14\n[ \"$3\" = \"gpt-5-codex\" ] || exit 15\nprintf '%s\n' '{\"title\":\"Generated PR\",\"body\":\"## Summary\\n\\n- Done\\n\\n## Testing\\n\\n- [ ] Not run\"}'\n",
        )
        .unwrap();
        make_executable(&script);
        let cwd = temp_dir("codex-provider-cwd");
        fs::create_dir_all(&cwd).unwrap();
        let config = DraftProviderConfig {
            kind: DraftProviderKind::Codex,
            claude: PathBuf::from("claude"),
            codex: script.clone(),
            timeout: Duration::from_secs(2),
            model: Some("gpt-5-codex".into()),
        };
        let draft = generate_pull_request_draft(
            &config,
            PullRequestDraftInput {
                worktree_path: cwd.clone(),
                branch: "feature/drafts".into(),
                base: "main".into(),
                commits: vec!["add drafts".into()],
                changed_paths: vec![PathBuf::from("tracked.txt")],
                diff: "+change".into(),
            },
        )
        .unwrap();
        assert_eq!(draft.title, "Generated PR");
        assert!(draft.body.contains("## Testing"));
        let _ = fs::remove_file(script);
        let _ = fs::remove_dir_all(cwd);
    }

    fn temp_file(name: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("hitch-{name}-{nonce}.{extension}"))
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("hitch-{name}-{nonce}"))
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).unwrap();
        }
    }
}
