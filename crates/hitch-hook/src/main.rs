//! `hitch-hook` — tiny helper invoked by known-agent hooks to report state to
//! the Hitch daemon socket (ADR 0002).

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use hitch_core::{AgentState, SessionId, SESSION_ID_ENV};
use hitch_proto::transport::{DaemonClient, DaemonStream};
use hitch_proto::{ControlMessage, KnownAgent, Request};

fn main() {
    if let Err(err) = real_main(std::env::args().skip(1), &mut io::stdin()) {
        // Reporting agent state is strictly best-effort. A non-zero exit makes the
        // agent (Claude Code / Codex) treat its own hook as failed — surfacing an
        // error to the user and, depending on the event, interrupting the turn.
        // Nothing this helper can hit (a malformed invocation, an extra argument
        // the agent appended, an absent or busy daemon) is worth breaking the
        // agent over, so every failure degrades to a logged no-op with exit 0.
        eprintln!("hitch-hook: {err}");
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

/// Env var that opts a hook invocation into the temp-dir debug log. Off by
/// default: the log records session IDs and socket paths, so writing it
/// unconditionally both leaks that metadata and litters the temp dir on every
/// agent event. Set `HITCH_HOOK_DEBUG=1` (any value) to turn it back on.
const HOOK_DEBUG_ENV: &str = "HITCH_HOOK_DEBUG";

/// Diagnostic: append a line to a debug log so we can see what the hook receives
/// when an agent runs it. Gated behind [`HOOK_DEBUG_ENV`] (default off) because
/// the lines carry session/socket metadata. Best-effort; ignores all I/O errors.
fn debug_log(message: &str) {
    use std::io::Write as _;
    if std::env::var_os(HOOK_DEBUG_ENV).is_none() {
        return;
    }
    let path = std::env::temp_dir().join("hitch-hook-debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "[pid {}] {message}", std::process::id());
    }
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
        debug_log("suppressed=true; skipping");
        return Ok(());
    }
    let args = HookArgs::parse(args)?;
    debug_log(&format!(
        "parsed agent={:?} event={:?} state_arg={:?} session_id_env={} HITCH_SOCKET={:?} socket={}",
        args.agent,
        args.event,
        args.state,
        std::env::var("HITCH_SESSION_ID")
            .map(|v| format!("present({v})"))
            .unwrap_or_else(|_| "ABSENT".into()),
        std::env::var("HITCH_SOCKET").ok(),
        args.socket_path.display(),
    ));
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
        let session_id = std::env::var(SESSION_ID_ENV)
            .ok()
            .map(|value| parse_session_id(&value))
            .transpose()?;
        let mut cwd = None;
        let mut detail = None;
        let mut payload = None;

        let mut iter = args.into_iter().map(Into::into).peekable();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--socket" => socket_path = PathBuf::from(required_value(&mut iter, "--socket")?),
                "--agent" => agent = Some(parse_agent(&required_value(&mut iter, "--agent")?)?),
                "--event" => event = Some(required_value(&mut iter, "--event")?),
                "--state" => state = Some(parse_state(&required_value(&mut iter, "--state")?)?),
                "--session-id" => {
                    let _ = required_value(&mut iter, "--session-id")?;
                    return Err(HookError::Usage(format!(
                        "--session-id is not accepted; set {SESSION_ID_ENV} in the hook environment\n{}",
                        usage()
                    )));
                }
                "--cwd" => cwd = Some(PathBuf::from(required_value(&mut iter, "--cwd")?)),
                "--detail" => detail = Some(required_value(&mut iter, "--detail")?),
                "--help" | "-h" => return Err(HookError::Usage(usage())),
                other if !other.starts_with('-') && payload.is_none() => {
                    payload = Some(other.to_owned());
                }
                // Unrecognized FLAGS are ignored rather than rejected. An agent
                // may append its own context (an event name, a JSON blob, extra
                // flags) to the configured hook command; failing on those would
                // break the agent's hook (see `main`). Skipping them keeps the
                // known flags — which carry the state report — fully effective.
                //
                // Treat an unknown flag as value-taking: every flag this tool
                // defines consumes one value via `next()`, so the likely shape of
                // an unknown flag is `--something value`. If we skipped only the
                // flag, its value (e.g. `cli` after `--session-source cli`) would
                // fall through to the positional-payload arm above and be captured
                // as the human-facing detail string. So swallow one following
                // non-dash token along with the flag. (An unknown *boolean* flag
                // immediately before a legit positional payload would then swallow
                // the payload — but the positional is only a stdin-empty fallback,
                // and a stray value masquerading as detail is the worse outcome.)
                unknown if unknown.starts_with('-') => {
                    if iter.peek().is_some_and(|next| !next.starts_with('-')) {
                        iter.next();
                    }
                }
                // A non-dash token when a payload was already captured: ignore it
                // (extra appended context) rather than reject.
                _ => {}
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
    let Some(mut client) = connect_to_daemon(&report.socket_path)? else {
        debug_log("connect: no daemon (unavailable); report not sent");
        return Ok(());
    };
    debug_log(&format!(
        "connect OK; sending agent={:?} state={:?} session_id={:?}",
        report.agent, report.state, report.session_id
    ));
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

    debug_log("sent; waiting for ack");
    wait_for_ack(client.into_connection());
    debug_log("done");
    Ok(())
}

/// Connect to the daemon, returning `Ok(None)` when there is simply nothing to
/// report to. Connecting is best-effort by design (see `main`): a missing daemon
/// is the common case for a bare terminal session, and a momentarily busy one is
/// transient. Only a connect error that is neither — an unexpected transport
/// fault — propagates, and even that is downgraded to a no-op by `main`.
fn connect_to_daemon(socket_path: &Path) -> Result<Option<DaemonClient>, HookError> {
    use std::time::{Duration, Instant};

    // On Windows a named-pipe server serves a bounded set of instances; while the
    // daemon is between accept polls every instance can be momentarily occupied,
    // and a connecting client gets `ERROR_PIPE_BUSY` instead of connecting. That
    // is transient — the daemon frees an instance as it loops — so retry briefly
    // rather than dropping the report. Capped tight so a hook never stalls long.
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match DaemonClient::connect(socket_path) {
            Ok(client) => return Ok(Some(client)),
            // Nothing is listening at all: no session to update, give up quietly.
            Err(err) if daemon_unavailable(&err) => return Ok(None),
            // Up at the endpoint but not accepting yet (busy pipe / not-yet-armed
            // instance): wait for the next poll and try again until the deadline.
            Err(err) if daemon_transiently_unavailable(&err) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            // Out of retries for a transient absence: no session can currently
            // accept the report, so give up quietly.
            Err(err) if daemon_transiently_unavailable(&err) => return Ok(None),
            // Real transport faults (permission denied, malformed endpoint, etc.)
            // are not "no daemon"; surface them to the caller's best-effort
            // downgrade path instead of silently dropping the report.
            Err(err) => return Err(err.into()),
        }
    }
}

/// Block until the daemon replies to the report just sent, then return.
///
/// This wait is what makes state reporting work on Windows. The daemon services
/// its socket with a *non-blocking* accept loop that polls on an interval; a hook
/// that writes its report and disconnects immediately is gone before the next
/// poll, so the connection — and the buffered report — are silently dropped and
/// no agent state ever updates. (Unix never hit this: the kernel queues the
/// connection and its bytes for `accept`, so a fire-and-forget write survives.)
/// Holding the socket open until the daemon answers keeps it present across the
/// poll, which both lets the daemon accept and read the request and confirms it
/// landed. We can't bound the read with `set_nonblocking` because the connected
/// named-pipe client rejects it (`ERROR_PIPE_BUSY`), so a watchdog thread caps
/// the wait instead: the report is already written, so a slow or wedged daemon
/// must never freeze the hook (and with it the agent that ran it).
fn wait_for_ack(mut connection: DaemonStream) {
    let (cancel_watchdog, watchdog_cancelled) = std::sync::mpsc::channel();
    let watchdog = spawn_ack_watchdog(watchdog_cancelled);
    // Returns as soon as the daemon answers (normally within one accept poll),
    // or when it closes the connection. The watchdog covers the case where it
    // does neither. The reply itself is discarded — we only needed to wait for
    // it. A read error means the connection is already gone; nothing to do.
    let _ = connection.read_control_messages();
    let _ = cancel_watchdog.send(());
    let _ = watchdog.join();
}

/// Time the helper's exit can be delayed waiting for the daemon's acknowledgement
/// (see [`wait_for_ack`]). A live daemon answers in tens of milliseconds; this is
/// only the ceiling for a daemon that accepted the connection but then stalled.
const ACK_WATCHDOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Force a clean exit if the acknowledgement read in [`wait_for_ack`] outlives
/// [`ACK_WATCHDOG_TIMEOUT`]. The report has already been written, so giving up on
/// the confirmation is harmless; exiting `0` keeps the agent's hook from blocking
/// on an unresponsive daemon.
fn spawn_ack_watchdog(cancelled: std::sync::mpsc::Receiver<()>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if cancelled.recv_timeout(ACK_WATCHDOG_TIMEOUT).is_err() {
            std::process::exit(0);
        }
    })
}

/// True when a connect error means "no daemon is listening" rather than a real
/// transport fault. These are the kinds the OS reports when nothing has bound
/// the endpoint: `NotFound` (Windows FILE_NOT_FOUND, Unix ENOENT on a missing
/// socket path) and `ConnectionRefused` (Unix socket file with no listener).
fn daemon_unavailable(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// True when a connect error means the daemon endpoint exists but cannot accept a
/// client right now. Windows named pipes report this while all pipe instances are
/// occupied or not yet armed; after the retry deadline it is equivalent to
/// daemon-unavailable for a best-effort hook report.
fn daemon_transiently_unavailable(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
        || hitch_proto::transport::is_endpoint_busy(err)
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
    "usage: hitch-hook --agent <claude-code|codex> [--event NAME] [--state running|needs-approval|waiting|error|none] [--socket PATH] [--cwd PATH] [--detail TEXT]".into()
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
    use hitch_proto::transport::DaemonListener;
    use hitch_proto::{ControlMessage, Request};
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static SOCKET_NONCE: AtomicU64 = AtomicU64::new(0);
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    #[test]
    fn debug_log_is_off_unless_env_var_is_set() {
        // The debug log records session IDs and socket paths, so it must stay off
        // by default and only write when HITCH_HOOK_DEBUG is set.
        let _lock = ENV_LOCK.lock().unwrap();
        let path = std::env::temp_dir().join("hitch-hook-debug.log");
        let _ = std::fs::remove_file(&path);

        let previous = std::env::var_os(HOOK_DEBUG_ENV);
        std::env::remove_var(HOOK_DEBUG_ENV);
        debug_log("must-not-be-written");
        assert!(
            !path.exists(),
            "debug log must not be written when {HOOK_DEBUG_ENV} is unset"
        );

        std::env::set_var(HOOK_DEBUG_ENV, "1");
        debug_log("written-when-enabled");
        let wrote = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            wrote.contains("written-when-enabled"),
            "debug log must be written when {HOOK_DEBUG_ENV} is set; got {wrote:?}"
        );

        match previous {
            Some(value) => std::env::set_var(HOOK_DEBUG_ENV, value),
            None => std::env::remove_var(HOOK_DEBUG_ENV),
        }
        let _ = std::fs::remove_file(&path);
    }

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
    fn unknown_arguments_are_ignored_so_the_report_still_parses() {
        // An agent may append its own context (extra flags, an event name, a JSON
        // blob) after the configured hook command. Those must not fail parsing —
        // a non-zero exit would make the agent treat its own hook as failed — and
        // the known flags carrying the state report must still take effect.
        let args = HookArgs::parse([
            "--agent",
            "codex",
            "--event",
            "user-prompt-submit",
            "--state",
            "running",
            "--hook-name",
            "UserPromptSubmit",
            "--unexpected-flag",
            r#"{"appended":"json"}"#,
        ])
        .unwrap();
        assert_eq!(args.agent, KnownAgent::Codex);
        assert_eq!(args.event.as_deref(), Some("user-prompt-submit"));
        assert_eq!(args.state, Some(Some(AgentState::Running)));
    }

    #[test]
    fn unknown_value_flag_does_not_leak_its_value_as_the_payload() {
        // An unknown flag that takes a value (the shape of every flag this tool
        // defines) must consume its value too — otherwise the value falls through
        // to the positional-payload arm and surfaces as the session's detail text.
        let args = HookArgs::parse([
            "--agent",
            "claude-code",
            "--state",
            "running",
            "--session-source",
            "cli",
        ])
        .unwrap();
        assert_eq!(args.agent, KnownAgent::ClaudeCode);
        assert_eq!(args.state, Some(Some(AgentState::Running)));
        // `cli` was the unknown flag's value, not a positional payload.
        assert_eq!(args.payload, None);
    }

    #[test]
    fn unknown_flag_alone_is_ignored() {
        let args = HookArgs::parse([
            "--agent",
            "codex",
            "--state",
            "running",
            "--standalone-flag",
        ])
        .unwrap();
        assert_eq!(args.agent, KnownAgent::Codex);
        assert_eq!(args.state, Some(Some(AgentState::Running)));
        assert_eq!(args.payload, None);
    }

    #[test]
    fn unknown_value_flag_before_a_known_flag_does_not_swallow_it() {
        // The token after the unknown flag is a dash-prefixed known flag, so it
        // must NOT be consumed as the unknown flag's value.
        let args = HookArgs::parse([
            "--agent",
            "claude-code",
            "--unknown-flag",
            "--state",
            "running",
        ])
        .unwrap();
        assert_eq!(args.agent, KnownAgent::ClaudeCode);
        assert_eq!(args.state, Some(Some(AgentState::Running)));
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
    fn session_id_cli_argument_is_rejected() {
        let _env = cleared_session_env();
        let err = HookArgs::parse([
            "--agent",
            "claude-code",
            "--state",
            "running",
            "--session-id",
            "11111111-1111-4111-8111-111111111111",
        ])
        .unwrap_err();

        assert!(err.to_string().contains("--session-id is not accepted"));
    }

    #[test]
    fn session_id_resolves_from_env() {
        let _env = session_env("22222222-2222-4222-8222-222222222222");
        let args = HookArgs::parse(["--agent", "claude-code", "--state", "running"]).unwrap();

        assert_eq!(
            args.session_id,
            Some(parse_session_id("22222222-2222-4222-8222-222222222222").unwrap())
        );
    }

    #[test]
    fn sends_env_session_id_and_windows_cwd_to_socket() {
        let _env = session_env("33333333-3333-4333-8333-333333333333");
        let socket = test_socket_path();
        let listener = DaemonListener::bind(&socket).unwrap();
        let socket_for_client = socket.clone();
        let windows_cwd = r"C:\Users\agent\repo\worktree";

        let server = thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let messages = stream.read_control_messages().unwrap();
            assert_eq!(messages.len(), 1);
            messages.into_iter().next().unwrap()
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
                windows_cwd.to_string(),
            ],
            &mut stdin,
        )
        .unwrap();

        let message = server.join().unwrap();
        let ControlMessage::Request { request, .. } = message else {
            panic!("expected request");
        };
        let Request::ReportAgentState {
            session_id, cwd, ..
        } = request
        else {
            panic!("expected report-agent-state");
        };
        assert_eq!(
            session_id,
            Some(parse_session_id("33333333-3333-4333-8333-333333333333").unwrap())
        );
        assert_eq!(cwd, Some(PathBuf::from(windows_cwd)));

        #[cfg(unix)]
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn reports_agent_state_to_socket() {
        let socket = test_socket_path();
        let listener = DaemonListener::bind(&socket).unwrap();
        let socket_for_client = socket.clone();

        let server = thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let messages = stream.read_control_messages().unwrap();
            assert_eq!(messages.len(), 1);
            messages.into_iter().next().unwrap()
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

        #[cfg(unix)]
        let _ = std::fs::remove_file(socket);
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
        let listener = DaemonListener::bind(&socket).unwrap();
        let socket_for_client = socket.clone();

        let server = thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let messages = stream.read_control_messages().unwrap();
            assert_eq!(messages.len(), 1);
            messages.into_iter().next().unwrap()
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

        #[cfg(unix)]
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn absent_daemon_is_reported_as_success() {
        // A stateful event with no daemon listening must NOT fail the agent's
        // hook: connecting returns NotFound (Windows FILE_NOT_FOUND / Unix
        // ENOENT) and the helper should exit Ok rather than propagating it. The
        // socket path points at an endpoint nothing has bound.
        let socket = test_socket_path();
        assert!(!socket.exists());
        let mut stdin = Cursor::new(b"{}" as &[u8]);
        real_main(
            [
                "--agent".to_string(),
                "claude-code".to_string(),
                "--event".to_string(),
                "session-end".to_string(),
                "--socket".to_string(),
                socket.display().to_string(),
            ],
            &mut stdin,
        )
        .unwrap();
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

    struct SessionEnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
    }

    impl Drop for SessionEnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(SESSION_ID_ENV, value),
                None => std::env::remove_var(SESSION_ID_ENV),
            }
        }
    }

    fn session_env(value: &str) -> SessionEnvGuard {
        let guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os(SESSION_ID_ENV);
        std::env::set_var(SESSION_ID_ENV, value);
        SessionEnvGuard {
            _lock: guard,
            previous,
        }
    }

    fn cleared_session_env() -> SessionEnvGuard {
        let guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os(SESSION_ID_ENV);
        std::env::remove_var(SESSION_ID_ENV);
        SessionEnvGuard {
            _lock: guard,
            previous,
        }
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
