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

/// The desktop client abandons a request after this many seconds (see
/// `apps/desktop/src-tauri/src/lib.rs`). The daemon's draft timeout is clamped
/// below it so the daemon always sends its response — success or timeout error
/// — before the client gives up and drops the late reply.
const CLIENT_RESPONSE_TIMEOUT_SECS: u64 = 120;
/// Margin reserved for the daemon to format and write its response (and for the
/// kill/wait/join path on a timeout) before the client's deadline elapses.
const CLIENT_RESPONSE_MARGIN_SECS: u64 = 10;
/// Hard upper bound on a draft provider timeout. Stays safely below the client
/// response timeout so a successful draft is never dropped.
const MAX_TIMEOUT_SECS: u64 = CLIENT_RESPONSE_TIMEOUT_SECS - CLIENT_RESPONSE_MARGIN_SECS;

/// Clamp a configured draft timeout into the supported range: at least one
/// second, and never above [`MAX_TIMEOUT_SECS`] so the daemon responds before
/// the client's fixed response timeout.
fn clamp_timeout_secs(secs: u64) -> Duration {
    Duration::from_secs(secs.clamp(1, MAX_TIMEOUT_SECS))
}

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
                clamp_timeout_secs(secs)
            }
            _ => clamp_timeout_secs(DEFAULT_TIMEOUT_SECS),
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
        self.timeout = clamp_timeout_secs(secs);
        Ok(())
    }

    pub(crate) fn with_settings(mut self, settings: Option<DraftGenerationSettings>) -> Self {
        if let Some(settings) = settings {
            self.kind = match settings.provider {
                DraftProvider::Stub => DraftProviderKind::Stub,
                DraftProvider::Claude => DraftProviderKind::Claude,
                DraftProvider::Codex => DraftProviderKind::Codex,
            };
            // Only override the configured model when the request actually
            // carries one. A None/empty request model must not clobber an
            // operator-configured --draft-model / HITCH_DRAFT_MODEL.
            if let Some(model) = settings
                .model
                .map(|model| model.trim().to_string())
                .filter(|model| !model.is_empty())
            {
                self.model = Some(model);
            }
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
            // Draft generation is read-only: the prompt already carries the full
            // git context, so the provider never needs filesystem or shell
            // access. `--tools ""` disables every built-in tool so pressing
            // Generate can't edit, run commands in, or otherwise mutate the
            // worktree, regardless of the user's permission settings.
            command.arg("--tools").arg("");
            // Request the structured print-mode envelope
            // (`{"type":"result","result":"<text>",...}`); the default `-p`
            // emits free-form text the parser can't reliably unwrap. The
            // nested-unwrap on the "result" key handles this shape.
            command.arg("--output-format").arg("json");
            command.arg("-p").arg(prompt);
            command
        }
        DraftProviderKind::Codex => {
            let mut command = Command::new(&config.codex);
            command.arg("exec");
            // Same read-only guarantee as Claude: pin the sandbox to read-only
            // so an operator's `workspace-write`/`danger-full-access` Codex
            // config can't let draft generation modify the worktree. Without
            // this, `codex exec` inherits the user's configured sandbox mode.
            command.arg("--sandbox").arg("read-only");
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

    // Run the provider as its own process-group leader so a timeout can signal
    // the whole tree. Otherwise killing only the direct child leaves any
    // grandchild it spawned (a wrapper shell, an MCP/helper subprocess) alive
    // holding the inherited stdout/stderr pipes, and the reader-thread joins
    // below would block long past the configured timeout.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

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
                kill_process_tree(&mut child);
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

/// Kill the provider and every descendant it spawned. On Unix the provider is
/// its own process-group leader (see `process_group(0)` at spawn), so signalling
/// the negated pid reaches grandchildren that inherited the stdout/stderr
/// pipes; without that, the reader-thread joins would block until those
/// grandchildren exit, defeating the timeout for wrapper-script providers.
#[cfg(unix)]
fn kill_process_tree(child: &mut std::process::Child) {
    let pid = child.id() as libc::pid_t;
    // SAFETY: `kill(2)` with a negative pid signals the process group whose id
    // is `pid`. It has no memory-safety preconditions; an already-gone group
    // simply returns ESRCH, which we ignore.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    // Belt-and-braces: also signal the leader directly in case the group send
    // missed it (e.g. the child re-set its own process group).
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
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
    // Fast path: the provider returned clean JSON with no surrounding prose.
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    // Fallback: the provider wrapped JSON in prose, log lines, or printed
    // several objects (possibly alongside stray/unbalanced braces in the
    // surrounding text). Attempt to parse one complete JSON object starting at
    // every `{`; a stray `{not json` simply fails and we advance to the next
    // candidate. CLIs print the final result last, so prefer the last object
    // that parses into a usable value.
    last_embedded_json_object(trimmed)
}

/// Try to parse a complete JSON object beginning at each top-level `{` in
/// `input`, using a streaming deserializer that ignores trailing data. Returns
/// the last object that parses successfully (the final printed result for most
/// CLIs). `{` characters that fall inside an already-parsed object's span are
/// skipped so braces embedded in string values don't shadow the real object.
fn last_embedded_json_object(input: &str) -> Option<Value> {
    let mut found = None;
    let mut consumed_until = 0;
    for (idx, _) in input.match_indices('{') {
        if idx < consumed_until {
            continue;
        }
        let mut stream = serde_json::Deserializer::from_str(&input[idx..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(value @ Value::Object(_))) => {
                // byte_offset() is relative to the slice we started at.
                consumed_until = idx + stream.byte_offset();
                found = Some(value);
            }
            _ => continue,
        }
    }
    found
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
    fn parses_prose_wrapped_and_log_prefixed_provider_json() {
        // JSON surrounded by conversational prose with stray braces in the text.
        let prose = "Sure! Here is the {commit} you asked for:\n{\"subject\":\"feat: ship it\",\"body\":\"- Done\"}\nLet me know if you need changes.";
        let draft = parse_commit_draft_output(prose).unwrap();
        assert_eq!(draft.subject, "feat: ship it");
        assert_eq!(draft.body, "- Done");

        // Log lines printed before the final JSON result; the last complete
        // top-level object wins.
        let logs = "[info] starting draft\n[warn] partial {not json\n{\"subject\":\"fix: real\",\"body\":\"- Body\"}\n";
        let draft = parse_commit_draft_output(logs).unwrap();
        assert_eq!(draft.subject, "fix: real");
        assert_eq!(draft.body, "- Body");

        // A brace inside a JSON string value must not break depth tracking.
        let braces_in_string =
            "noise {\"subject\":\"chore: braces { } in text\",\"body\":\"- b\"} trailing";
        let draft = parse_commit_draft_output(braces_in_string).unwrap();
        assert_eq!(draft.subject, "chore: braces { } in text");
    }

    #[test]
    fn provider_commit_draft_falls_back_to_context_body_when_body_is_empty() {
        let script = temp_file("empty-commit-body-provider", "sh");
        fs::write(
            &script,
            "#!/bin/sh\n[ \"$1\" = \"--tools\" ] || exit 11\n[ \"$2\" = \"\" ] || exit 12\n[ \"$3\" = \"--output-format\" ] || exit 13\n[ \"$4\" = \"json\" ] || exit 14\n[ \"$5\" = \"-p\" ] || exit 15\nprintf '%s\n' '{\"subject\":\"fix: preserve generated body\",\"body\":\"\"}'\n",
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

    fn config_with_model(model: Option<&str>) -> DraftProviderConfig {
        DraftProviderConfig {
            kind: DraftProviderKind::Claude,
            claude: PathBuf::from("claude"),
            codex: PathBuf::from("codex"),
            timeout: Duration::from_secs(2),
            model: model.map(str::to_string),
        }
    }

    #[test]
    fn with_settings_keeps_configured_model_when_request_model_is_absent_or_empty() {
        // Request without a model must not clobber the operator-configured model.
        let kept = config_with_model(Some("opus")).with_settings(Some(DraftGenerationSettings {
            provider: DraftProvider::Claude,
            model: None,
        }));
        assert_eq!(kept.model.as_deref(), Some("opus"));

        // An empty/whitespace request model is also ignored.
        let kept = config_with_model(Some("opus")).with_settings(Some(DraftGenerationSettings {
            provider: DraftProvider::Claude,
            model: Some("   ".into()),
        }));
        assert_eq!(kept.model.as_deref(), Some("opus"));

        // A real request model still overrides.
        let overridden =
            config_with_model(Some("opus")).with_settings(Some(DraftGenerationSettings {
                provider: DraftProvider::Codex,
                model: Some("gpt-5-codex".into()),
            }));
        assert_eq!(overridden.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(overridden.kind, DraftProviderKind::Codex);
    }

    #[test]
    fn timeout_is_clamped_below_client_response_deadline() {
        let mut config = config_with_model(None);
        // A huge configured timeout is capped so the daemon responds before the
        // client's fixed response timeout.
        config.set_timeout_secs("100000").unwrap();
        assert_eq!(config.timeout.as_secs(), MAX_TIMEOUT_SECS);
        assert!(MAX_TIMEOUT_SECS < CLIENT_RESPONSE_TIMEOUT_SECS);

        // A zero/sub-second timeout floors at one second.
        config.set_timeout_secs("0").unwrap();
        assert_eq!(config.timeout.as_secs(), 1);
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
            "#!/bin/sh\n[ \"$1\" = \"--model\" ] || exit 12\n[ \"$2\" = \"sonnet\" ] || exit 13\n[ \"$3\" = \"--tools\" ] || exit 14\n[ \"$4\" = \"\" ] || exit 15\n[ \"$5\" = \"--output-format\" ] || exit 16\n[ \"$6\" = \"json\" ] || exit 17\n[ \"$7\" = \"-p\" ] || exit 18\nprintf '%s\n' '{\"type\":\"result\",\"result\":\"{\\\"subject\\\":\\\"feat: generated\\\",\\\"body\\\":\\\"Generated body\\\"}\"}'\n",
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
            "#!/bin/sh\n[ \"$1\" = \"exec\" ] || exit 13\n[ \"$2\" = \"--sandbox\" ] || exit 14\n[ \"$3\" = \"read-only\" ] || exit 15\n[ \"$4\" = \"--model\" ] || exit 16\n[ \"$5\" = \"gpt-5-codex\" ] || exit 17\nprintf '%s\n' '{\"title\":\"Generated PR\",\"body\":\"## Summary\\n\\n- Done\\n\\n## Testing\\n\\n- [ ] Not run\"}'\n",
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

    #[cfg(unix)]
    #[test]
    fn provider_timeout_does_not_block_on_grandchild_holding_the_pipe() {
        // Regression: a provider that backgrounds a child inheriting stdout must
        // not keep the daemon blocked past the timeout. Before the
        // process-group kill, signalling only the direct child left the
        // grandchild holding the stdout pipe, so the reader-thread join blocked
        // until that grandchild exited (~30s here) instead of ~1s.
        let script = temp_file("blocking-grandchild-provider", "sh");
        fs::write(
            &script,
            "#!/bin/sh\n# Grandchild inherits stdout and holds the pipe open well past the timeout.\nsleep 30 &\n# Parent stays alive so the timeout path (not a normal exit) runs.\nsleep 30\n",
        )
        .unwrap();
        make_executable(&script);
        let cwd = temp_dir("blocking-grandchild-cwd");
        fs::create_dir_all(&cwd).unwrap();
        let config = DraftProviderConfig {
            kind: DraftProviderKind::Codex,
            claude: PathBuf::from("claude"),
            codex: script.clone(),
            timeout: Duration::from_secs(1),
            model: None,
        };
        let mut command = Command::new(&config.codex);
        let started = Instant::now();
        let result = run_provider_command(&mut command, &cwd, &config);
        let elapsed = started.elapsed();
        assert!(result.is_err(), "expected a timeout error");
        assert!(
            elapsed < Duration::from_secs(10),
            "timeout path blocked for {elapsed:?}; the process-group kill should free the inherited pipe promptly"
        );
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
