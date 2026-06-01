//! `hitch-hook` — tiny helper invoked by known-agent hooks to report state to
//! the Hitch daemon socket (ADR 0002).

use std::fmt;
use std::io::{self, Read};
use std::path::PathBuf;

use hitch_core::{AgentState, SessionId, SESSION_ID_ENV};
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
    // guard those reports would resolve to whatever live shell session shares
    // the worktree. Bail before touching the socket so draft generation never
    // disturbs unrelated sessions.
    if suppressed {
        return Ok(());
    }
    let args = HookArgs::parse(args)?;
    let mut payload = String::new();
    stdin.read_to_string(&mut payload)?;
    if payload.is_empty() {
        if let Some(arg_payload) = args.payload {
            payload = arg_payload;
        }
    }

    let state = match args.state {
        Some(state) => state,
        None => match state_from_event(args.agent, args.event.as_deref()) {
            Some(state) => state,
            None => return Ok(()),
        },
    };
    let cwd = match args.cwd {
        Some(cwd) => Some(cwd),
        None => cwd_from_payload(&payload).or_else(|| std::env::current_dir().ok()),
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
    state: Option<Option<AgentState>>,
    session_id: Option<SessionId>,
    cwd: Option<PathBuf>,
    detail: Option<String>,
    payload: Option<String>,
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
        let mut session_id = std::env::var(SESSION_ID_ENV)
            .ok()
            .map(|value| parse_session_id(&value))
            .transpose()?;
        let mut cwd = None;
        let mut detail = None;
        let mut payload = None;

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
                other if !other.starts_with('-') && payload.is_none() => {
                    payload = Some(other.to_owned());
                }
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
            payload,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HookReport {
    socket_path: PathBuf,
    agent: KnownAgent,
    event: Option<String>,
    state: Option<AgentState>,
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

fn state_from_event(agent: KnownAgent, event: Option<&str>) -> Option<Option<AgentState>> {
    let event = event?;
    if event_matches(event, b"userpromptsubmit") || event_matches(event, b"posttooluse") {
        Some(Some(AgentState::Running))
    } else if event_matches(event, b"permissionrequest") {
        Some(Some(AgentState::NeedsApproval))
    } else if agent == KnownAgent::ClaudeCode && event_matches(event, b"notification") {
        Some(Some(AgentState::NeedsApproval))
    } else if event_matches(event, b"stop") {
        Some(Some(AgentState::Waiting))
    } else if agent == KnownAgent::ClaudeCode && event_matches(event, b"stopfailure") {
        Some(Some(AgentState::Error))
    } else if event_matches(event, b"sessionend") {
        Some(None)
    } else {
        None
    }
}

fn event_matches(event: &str, expected: &[u8]) -> bool {
    let mut index = 0;
    for byte in event.bytes() {
        if byte == b'-' || byte == b'_' {
            continue;
        }
        if index == expected.len() || byte.to_ascii_lowercase() != expected[index] {
            return false;
        }
        index += 1;
    }
    index == expected.len()
}

fn cwd_from_payload(payload: &str) -> Option<PathBuf> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("cwd")?
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}

fn detail_from_payload(payload: &str) -> Option<String> {
    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
        for key in [
            "message",
            "detail",
            "error",
            "reason",
            "title",
            "last_assistant_message",
        ] {
            if let Some(text) = value.get(key).and_then(|value| value.as_str()) {
                return Some(truncate(text, 240));
            }
        }
        if let Some(text) = value
            .get("tool_input")
            .and_then(|value| value.get("description"))
            .and_then(|value| value.as_str())
        {
            return Some(truncate(text, 240));
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

fn parse_state(value: &str) -> Result<Option<AgentState>, HookError> {
    match value {
        "running" => Ok(Some(AgentState::Running)),
        "needs-approval" | "needs_approval" | "approval" => Ok(Some(AgentState::NeedsApproval)),
        "waiting" => Ok(Some(AgentState::Waiting)),
        "error" | "failed" => Ok(Some(AgentState::Error)),
        "none" | "clear" | "null" => Ok(None),
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
    "usage: hitch-hook --agent <claude-code|codex> [--event NAME] [--state running|needs-approval|waiting|error|none] [--socket PATH] [--session-id UUID] [--cwd PATH] [--detail TEXT]".into()
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static SOCKET_NONCE: AtomicU64 = AtomicU64::new(0);
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
        assert_eq!(args.state, Some(Some(AgentState::NeedsApproval)));
        assert_eq!(args.socket_path, PathBuf::from("/tmp/hitch.sock"));
    }

    #[test]
    fn explicit_clear_state_args_parse() {
        for value in ["none", "clear", "null"] {
            let args = HookArgs::parse(["--agent", "claude-code", "--state", value]).unwrap();
            assert_eq!(args.state, Some(None));
        }
    }

    #[test]
    fn maps_known_hook_events() {
        assert_eq!(
            state_from_event(KnownAgent::ClaudeCode, Some("notification")),
            Some(Some(AgentState::NeedsApproval))
        );
        assert_eq!(
            state_from_event(KnownAgent::ClaudeCode, Some("stop")),
            Some(Some(AgentState::Waiting))
        );
        assert_eq!(
            state_from_event(KnownAgent::Codex, Some("post-tool-use")),
            Some(Some(AgentState::Running))
        );
        assert_eq!(
            state_from_event(KnownAgent::ClaudeCode, Some("session-end")),
            Some(None)
        );
        assert_eq!(
            state_from_event(KnownAgent::Codex, Some("notification")),
            None
        );
    }

    #[test]
    fn hook_payload_can_arrive_as_trailing_argument() {
        let args = HookArgs::parse([
            "--agent",
            "codex",
            "--event",
            "permission-request",
            r#"{"message":"permission requested"}"#,
        ])
        .unwrap();
        assert_eq!(
            args.payload.as_deref(),
            Some(r#"{"message":"permission requested"}"#)
        );
        assert_eq!(
            state_from_event(KnownAgent::Codex, args.event.as_deref()),
            Some(Some(AgentState::NeedsApproval))
        );
    }

    #[test]
    fn uses_hook_payload_cwd_when_no_cwd_arg_is_given() {
        let payload = r#"{"cwd":"/repo/from-payload","hook_event_name":"UserPromptSubmit"}"#;
        assert_eq!(
            cwd_from_payload(payload),
            Some(PathBuf::from("/repo/from-payload"))
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
                "permission-request".to_string(),
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
        assert_eq!(state, Some(AgentState::NeedsApproval));
        assert_eq!(cwd, Some(PathBuf::from("/repo/worktree")));
        assert_eq!(detail.as_deref(), Some("permission requested"));

        let _ = fs::remove_file(socket);
    }

    #[test]
    fn unknown_event_without_state_does_not_touch_socket() {
        let mut stdin = Cursor::new(r#"{"message":"permission requested"}"#.as_bytes());
        run(
            [
                "--agent".to_string(),
                "codex".to_string(),
                "--event".to_string(),
                "mystery-event".to_string(),
                "--socket".to_string(),
                "/nonexistent/hitch-unknown-event.sock".to_string(),
            ],
            &mut stdin,
            false,
        )
        .unwrap();
    }

    #[test]
    fn session_end_reports_clear_state_to_socket() {
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

        let mut stdin = Cursor::new(b"{}" as &[u8]);
        real_main(
            [
                "--agent".to_string(),
                "claude-code".to_string(),
                "--event".to_string(),
                "session-end".to_string(),
                "--socket".to_string(),
                socket_for_client.display().to_string(),
            ],
            &mut stdin,
        )
        .unwrap();

        let message = server.join().unwrap();
        let ControlMessage::Request { request, .. } = message else {
            panic!("expected request");
        };
        let Request::ReportAgentState { state, .. } = request else {
            panic!("expected report-agent-state");
        };
        assert_eq!(state, None);

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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = SOCKET_NONCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "hitch-hook-test-{}-{now}-{seq}.sock",
            std::process::id()
        ))
    }
}
