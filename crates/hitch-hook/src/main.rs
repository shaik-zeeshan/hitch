//! `hitch-hook` — tiny helper invoked by known-agent hooks to report state to
//! the Hitch daemon socket (ADR 0002).

use std::fmt;
use std::io::{self, Read};
use std::path::PathBuf;

use hitch_core::{AgentState, SessionId};
use hitch_proto::transport::UnixSocketClient;
use hitch_proto::{ControlMessage, KnownAgent, Request};

fn main() {
    if let Err(err) = real_main(std::env::args().skip(1), &mut io::stdin()) {
        eprintln!("hitch-hook: {err}");
        std::process::exit(1);
    }
}

fn real_main<I, R>(args: I, stdin: &mut R) -> Result<(), HookError>
where
    I: IntoIterator,
    I::Item: Into<String>,
    R: Read,
{
    run(args, stdin, hooks_suppressed())
}

/// True when Hitch is suppressing agent-state reports for this invocation.
/// Draft generation (`claude -p` / `codex exec`) runs inside a worktree that may
/// have Hitch's hooks installed and exports [`SUPPRESS_AGENT_HOOKS_ENV`] so this
/// helper stays silent instead of moving an unrelated session's state.
fn hooks_suppressed() -> bool {
    std::env::var_os(hitch_proto::SUPPRESS_AGENT_HOOKS_ENV).is_some()
}

fn run<I, R>(args: I, stdin: &mut R, suppressed: bool) -> Result<(), HookError>
where
    I: IntoIterator,
    I::Item: Into<String>,
    R: Read,
{
    // A draft-generation run loads the worktree's installed hooks; without this
    // guard those reports would resolve by cwd to whatever live shell session
    // shares the worktree and flip it to running/completed. Bail before touching
    // the socket so draft generation never disturbs unrelated sessions.
    if suppressed {
        return Ok(());
    }
    let args = HookArgs::parse(args)?;
    let mut payload = String::new();
    stdin.read_to_string(&mut payload)?;

    let state = args
        .state
        .unwrap_or_else(|| infer_state(args.agent, args.event.as_deref(), &payload));
    let cwd = match args.cwd {
        Some(cwd) => Some(cwd),
        None => std::env::current_dir().ok(),
    };
    let detail = args.detail.or_else(|| detail_from_payload(&payload));

    send_report(HookReport {
        socket_path: args.socket_path,
        agent: args.agent,
        event: args.event,
        state,
        session_id: args.session_id,
        cwd,
        detail,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HookArgs {
    socket_path: PathBuf,
    agent: KnownAgent,
    event: Option<String>,
    state: Option<AgentState>,
    session_id: Option<SessionId>,
    cwd: Option<PathBuf>,
    detail: Option<String>,
}

impl HookArgs {
    fn parse<I>(args: I) -> Result<Self, HookError>
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let mut socket_path = std::env::var_os("HITCH_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(hitch_proto::transport::default_socket_path);
        let mut agent = None;
        let mut event = None;
        let mut state = None;
        let mut session_id = std::env::var("HITCH_SESSION_ID")
            .ok()
            .map(|value| parse_session_id(&value))
            .transpose()?;
        let mut cwd = None;
        let mut detail = None;

        let mut iter = args.into_iter().map(Into::into);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--socket" => socket_path = PathBuf::from(required_value(&mut iter, "--socket")?),
                "--agent" => agent = Some(parse_agent(&required_value(&mut iter, "--agent")?)?),
                "--event" => event = Some(required_value(&mut iter, "--event")?),
                "--state" => state = Some(parse_state(&required_value(&mut iter, "--state")?)?),
                "--session-id" => {
                    session_id = Some(parse_session_id(&required_value(
                        &mut iter,
                        "--session-id",
                    )?)?)
                }
                "--cwd" => cwd = Some(PathBuf::from(required_value(&mut iter, "--cwd")?)),
                "--detail" => detail = Some(required_value(&mut iter, "--detail")?),
                "--help" | "-h" => return Err(HookError::Usage(usage())),
                other => {
                    return Err(HookError::Usage(format!(
                        "unknown argument: {other}\n{}",
                        usage()
                    )))
                }
            }
        }

        Ok(Self {
            socket_path,
            agent: agent
                .ok_or_else(|| HookError::Usage(format!("--agent is required\n{}", usage())))?,
            event,
            state,
            session_id,
            cwd,
            detail,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HookReport {
    socket_path: PathBuf,
    agent: KnownAgent,
    event: Option<String>,
    state: AgentState,
    session_id: Option<SessionId>,
    cwd: Option<PathBuf>,
    detail: Option<String>,
}

fn send_report(report: HookReport) -> Result<(), HookError> {
    let mut client = UnixSocketClient::connect(&report.socket_path)?;
    let detail = report
        .detail
        .or(report.event.map(|event| format!("event: {event}")));
    client
        .connection_mut()
        .send_control(&ControlMessage::request(
            1,
            Request::ReportAgentState {
                agent: report.agent,
                state: report.state,
                session_id: report.session_id,
                cwd: report.cwd,
                detail,
            },
        ))
        .map_err(|err| HookError::Transport(err.to_string()))?;
    Ok(())
}

fn infer_state(agent: KnownAgent, event: Option<&str>, payload: &str) -> AgentState {
    let event = event.unwrap_or_default().to_ascii_lowercase();
    match agent {
        KnownAgent::ClaudeCode => match event.as_str() {
            "notification" => AgentState::NeedsApproval,
            "stop" | "subagent-stop" | "session-stop" => AgentState::Completed,
            "error" => AgentState::Error,
            "user-prompt-submit" | "pre-tool-use" | "post-tool-use" => AgentState::Running,
            _ => infer_state_from_text(&format!("{event}\n{payload}")),
        },
        KnownAgent::Codex => infer_state_from_text(&format!("{event}\n{payload}")),
    }
}

fn infer_state_from_text(text: &str) -> AgentState {
    let lower = text.to_ascii_lowercase();
    if contains_any(
        &lower,
        &[
            "approval",
            "permission",
            "confirm",
            "authorize",
            "waiting for user",
        ],
    ) {
        AgentState::NeedsApproval
    } else if contains_any(&lower, &["error", "failed", "failure", "panic"]) {
        AgentState::Error
    } else if contains_any(
        &lower,
        &["completed", "complete", "finished", "done", "success"],
    ) {
        AgentState::Completed
    } else {
        AgentState::Running
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn detail_from_payload(payload: &str) -> Option<String> {
    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
        for key in ["message", "detail", "error", "reason", "title"] {
            if let Some(text) = value.get(key).and_then(|value| value.as_str()) {
                return Some(truncate(text, 240));
            }
        }
    }

    Some(truncate(payload, 240))
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn parse_agent(value: &str) -> Result<KnownAgent, HookError> {
    match value {
        "claude-code" | "claude" | "claude_code" => Ok(KnownAgent::ClaudeCode),
        "codex" => Ok(KnownAgent::Codex),
        _ => Err(HookError::Usage(format!("unknown agent: {value}"))),
    }
}

fn parse_state(value: &str) -> Result<AgentState, HookError> {
    match value {
        "running" => Ok(AgentState::Running),
        "needs-approval" | "needs_approval" | "approval" => Ok(AgentState::NeedsApproval),
        "completed" | "complete" | "done" => Ok(AgentState::Completed),
        "error" | "failed" => Ok(AgentState::Error),
        _ => Err(HookError::Usage(format!("unknown state: {value}"))),
    }
}

fn parse_session_id(value: &str) -> Result<SessionId, HookError> {
    serde_json::from_str(&format!("\"{value}\""))
        .map_err(|_| HookError::Usage(format!("invalid session id: {value}")))
}

fn required_value<I>(iter: &mut I, flag: &str) -> Result<String, HookError>
where
    I: Iterator<Item = String>,
{
    iter.next()
        .ok_or_else(|| HookError::Usage(format!("{flag} requires a value")))
}

fn usage() -> String {
    "usage: hitch-hook --agent <claude-code|codex> [--event NAME] [--state STATE] [--socket PATH] [--session-id UUID] [--cwd PATH] [--detail TEXT]".into()
}

#[derive(Debug)]
enum HookError {
    Io(io::Error),
    Transport(String),
    Usage(String),
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Transport(err) => write!(f, "{err}"),
            Self::Usage(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for HookError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Transport(_) | Self::Usage(_) => None,
        }
    }
}

impl From<io::Error> for HookError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hitch_proto::transport::UnixSocketListener;
    use hitch_proto::{ControlMessage, Request};
    use std::fs;
    use std::io::{BufRead, BufReader, Cursor};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn explicit_state_args_parse() {
        let args = HookArgs::parse([
            "--agent",
            "claude-code",
            "--event",
            "notification",
            "--state",
            "needs-approval",
            "--socket",
            "/tmp/hitch.sock",
        ])
        .unwrap();
        assert_eq!(args.agent, KnownAgent::ClaudeCode);
        assert_eq!(args.event.as_deref(), Some("notification"));
        assert_eq!(args.state, Some(AgentState::NeedsApproval));
        assert_eq!(args.socket_path, PathBuf::from("/tmp/hitch.sock"));
    }

    #[test]
    fn infers_common_hook_events() {
        assert_eq!(
            infer_state(KnownAgent::ClaudeCode, Some("notification"), "{}"),
            AgentState::NeedsApproval
        );
        assert_eq!(
            infer_state(KnownAgent::ClaudeCode, Some("stop"), "{}"),
            AgentState::Completed
        );
        assert_eq!(
            infer_state(
                KnownAgent::Codex,
                Some("notify"),
                r#"{"message":"permission requested"}"#
            ),
            AgentState::NeedsApproval
        );
        assert_eq!(
            infer_state(
                KnownAgent::Codex,
                Some("notify"),
                r#"{"message":"run completed"}"#
            ),
            AgentState::Completed
        );
    }

    #[test]
    fn reports_agent_state_to_socket() {
        let socket = test_socket_path();
        let listener = UnixSocketListener::bind(&socket).unwrap();
        let socket_for_client = socket.clone();

        let server = thread::spawn(move || {
            let stream = listener.accept().unwrap().into_inner();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str::<ControlMessage>(line.trim()).unwrap()
        });

        let mut stdin = Cursor::new(r#"{"message":"permission requested"}"#.as_bytes());
        real_main(
            [
                "--agent".to_string(),
                "codex".to_string(),
                "--event".to_string(),
                "notify".to_string(),
                "--socket".to_string(),
                socket_for_client.display().to_string(),
                "--cwd".to_string(),
                "/repo/worktree".to_string(),
            ],
            &mut stdin,
        )
        .unwrap();

        let message = server.join().unwrap();
        let ControlMessage::Request { request, .. } = message else {
            panic!("expected request");
        };
        let Request::ReportAgentState {
            agent,
            state,
            cwd,
            detail,
            ..
        } = request
        else {
            panic!("expected report-agent-state");
        };
        assert_eq!(agent, KnownAgent::Codex);
        assert_eq!(state, AgentState::NeedsApproval);
        assert_eq!(cwd, Some(PathBuf::from("/repo/worktree")));
        assert_eq!(detail.as_deref(), Some("permission requested"));

        let _ = fs::remove_file(socket);
    }

    #[test]
    fn suppressed_run_does_not_touch_socket() {
        // With suppression on, the helper must return Ok without attempting to
        // connect — proven here by handing it a socket path that cannot exist:
        // if the bail-out regressed, `run` would try to connect and Err.
        let mut stdin = Cursor::new(b"{}" as &[u8]);
        run(
            [
                "--agent".to_string(),
                "claude-code".to_string(),
                "--event".to_string(),
                "stop".to_string(),
                "--socket".to_string(),
                "/nonexistent/hitch-suppressed.sock".to_string(),
            ],
            &mut stdin,
            true,
        )
        .unwrap();
    }

    fn test_socket_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("hitch-hook-test-{nonce}.sock"))
    }
}
